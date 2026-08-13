#!/usr/bin/env python3
# stet - A PostScript Interpreter
# Copyright (c) 2026 Scott Bowman
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Tier-0 error sweep: run stet over the PS corpus, classify failures.

No image comparison — this only asks "did stet survive the file, and if not,
how did it die". Uses ``--device null`` so nothing is written next to the
inputs.

Three properties are load-bearing, and the absence of each one is what made
the previous ad-hoc sweep take the machine down instead of reporting a bug:

  1. **Every child runs under a hard memory cap.** stet has inputs that
     allocate without bound (a 55 KB corpus file reaches >3 GB in under a
     second). With N unbounded children the sweep OOMs the *host*, losing the
     run and the result. Each child is confined to its own cgroup via
     ``systemd-run --scope``, with swap denied so a runaway is killed rather
     than paged out. The sweep's whole-run ceiling is ``jobs * mem-max``,
     printed at startup — keep it under total RAM.

  2. **Child output is spooled to disk, never buffered in the parent.**
     ``subprocess.run(capture_output=True)`` accumulates unbounded output in
     the parent's address space, which turns a chatty child into a second
     way to exhaust memory.

  3. **stdout is examined, not just stderr.** stet writes the PLRM
     ``%%[ Error: … ]%%`` banner to *stdout* (per PLRM, the error handler
     writes to the standard output file) and exits 0 even when a job fails.
     A sweep that reads only stderr and only checks the exit status reports a
     clean run over a corpus where ~18% of files raise a PostScript error.

Usage:
    ./scripts/ps_corpus_sweep.py --jobs 4 --mem-max 3G
    ./scripts/ps_corpus_sweep.py --per-variant 60      # quick stratified pass
"""

import argparse
import collections
import concurrent.futures
import json
import os
import re
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parent.parent
STET = PROJECT_ROOT / "target" / "release" / "stet"
CORPUS = Path(os.environ.get("STET_PS_CORPUS", PROJECT_ROOT / "ps_corpus"))

PS_ERROR_RE = re.compile(r"%%\[\s*Error:\s*(\w+)[^\]]*\]%%")
PS_OFFENDING_RE = re.compile(r"OffendingCommand:\s*(\S+?)\s*\]")
PANIC_RE = re.compile(r"thread '[^']*' panicked at ([^\n]*)")

# Cap on how much of a child's output we read back for classification. The
# full stream still goes to disk; only this prefix enters the parent.
READBACK_BYTES = 64 * 1024

HAVE_SYSTEMD_RUN = shutil.which("systemd-run") is not None


def confine(argv, mem_max):
    """Wrap argv so the child runs in its own memory-capped cgroup."""
    if not HAVE_SYSTEMD_RUN:
        return argv
    return [
        "systemd-run", "--user", "--scope", "--quiet",
        "-p", f"MemoryMax={mem_max}",
        "-p", "MemorySwapMax=0",
    ] + argv


def rlimit_fallback(mem_bytes):
    """preexec_fn capping address space, for hosts without systemd-run.

    RLIMIT_AS bounds virtual address space rather than RSS, so it is given
    generous headroom: it exists to stop a runaway, not to measure one.
    """
    def _apply():
        import resource
        resource.setrlimit(resource.RLIMIT_AS, (mem_bytes, mem_bytes))
    return _apply


def parse_mem(spec):
    units = {"K": 1024, "M": 1024 ** 2, "G": 1024 ** 3}
    s = str(spec).strip().upper().rstrip("B")
    if s and s[-1] in units:
        return int(float(s[:-1]) * units[s[-1]])
    return int(s)


def peak_rss_poller(pid, box, stop):
    """Sample VmHWM so the report can rank files by real memory cost."""
    path = f"/proc/{pid}/status"
    while not stop.is_set():
        try:
            with open(path) as fh:
                for line in fh:
                    if line.startswith("VmHWM:"):
                        box[0] = max(box[0], int(line.split()[1]))
                        break
        except OSError:
            return
        time.sleep(0.05)


def classify(out_head, err_head, rc, timed_out, oom):
    """Collapse a run into a stable, groupable (kind, signature)."""
    if timed_out:
        return "timeout", "timeout"
    if oom:
        return "oom", f"exceeded memory cap (rc={rc})"
    both = out_head + "\n" + err_head
    m = PANIC_RE.search(both)
    if m:
        loc = re.sub(r":\d+:\d+", "", m.group(1)).strip()
        after = both[m.end():m.end() + 200].strip().splitlines()
        return "panic", f"{loc} :: {after[0][:110] if after else ''}"
    m = PS_ERROR_RE.search(out_head)
    if m:
        cmd = PS_OFFENDING_RE.search(out_head)
        return "pserror", f"{m.group(1)} / {cmd.group(1) if cmd else '?'}"
    if "FAILED" in err_head:
        return "jobfail", "job reported FAILED with no error banner"
    if rc != 0:
        tail = [l for l in err_head.strip().splitlines() if l.strip()]
        return "exit", (tail[-1][:110] if tail else f"exit {rc}")
    return None, None


def run_one(path, args, spool):
    tag = path.stem + "-" + str(abs(hash(str(path))) % 10 ** 8)
    op, ep = spool / f"{tag}.out", spool / f"{tag}.err"
    box, stop = [0], threading.Event()
    started = time.time()
    mem_bytes = parse_mem(args.mem_max)

    argv = confine([str(STET), "--device", "null", str(path)], args.mem_max)
    kwargs = {}
    if not HAVE_SYSTEMD_RUN:
        kwargs["preexec_fn"] = rlimit_fallback(int(mem_bytes * 1.5))

    with open(op, "wb") as ofh, open(ep, "wb") as efh:
        try:
            p = subprocess.Popen(argv, stdout=ofh, stderr=efh, **kwargs)
        except OSError as exc:
            return {"path": str(path.relative_to(CORPUS)), "kind": "exit",
                    "signature": str(exc), "rc": -1, "seconds": 0.0,
                    "peak_rss_mb": 0.0, "in_mb": 0.0, "out_bytes": 0,
                    "head": ""}
        th = threading.Thread(target=peak_rss_poller, args=(p.pid, box, stop),
                              daemon=True)
        th.start()
        try:
            rc, timed_out = p.wait(timeout=args.timeout), False
        except subprocess.TimeoutExpired:
            p.kill()
            rc, timed_out = p.wait(), True
        stop.set()

    out_bytes, err_bytes = op.stat().st_size, ep.stat().st_size
    with open(op, "rb") as fh:
        out_head = fh.read(READBACK_BYTES).decode("utf-8", "replace")
    with open(ep, "rb") as fh:
        err_head = fh.read(READBACK_BYTES).decode("utf-8", "replace")
    if not args.keep_output:
        op.unlink(missing_ok=True)
        ep.unlink(missing_ok=True)

    peak_mb = box[0] / 1024
    # systemd reports an OOM kill as 137 (128+SIGKILL); a direct SIGKILL
    # surfaces as -9. Either way, treat "killed near the cap" as OOM.
    oom = (rc in (-9, 137)) and not timed_out and peak_mb > 0.75 * (
        mem_bytes / 1048576)
    kind, sig = classify(out_head, err_head, rc, timed_out, oom)

    return {
        "path": str(path.relative_to(CORPUS)),
        "variant": path.relative_to(CORPUS / "files").parts[0],
        "in_mb": round(path.stat().st_size / 1048576, 2),
        "peak_rss_mb": round(peak_mb, 1),
        "seconds": round(time.time() - started, 2),
        "rc": rc,
        "kind": kind,
        "signature": sig,
        "out_bytes": out_bytes,
        "err_bytes": err_bytes,
        "head": out_head[:600] if kind else "",
    }


def main():
    ap = argparse.ArgumentParser(
        description="Run stet over the PS corpus and classify failures.")
    ap.add_argument("--jobs", type=int, default=4,
                    help="concurrent children (default 4)")
    ap.add_argument("--mem-max", default="3G",
                    help="hard memory cap PER CHILD (default 3G)")
    ap.add_argument("--timeout", type=int, default=90,
                    help="per-file seconds (default 90)")
    ap.add_argument("--per-variant", type=int, default=0,
                    help="stratified sample per variant (0 = whole corpus)")
    ap.add_argument("--min-mb", type=float, default=0.0,
                    help="only files at least this large")
    ap.add_argument("--keep-output", action="store_true",
                    help="retain per-file stdout/stderr spool files")
    # Defaults live beside the corpus (already outside the repo), so a sweep
    # never drops artifacts into the working tree.
    ap.add_argument("--out", default=str(CORPUS / "sweep.jsonl"))
    args = ap.parse_args()

    if not STET.exists():
        sys.exit(f"No stet binary at {STET} — run 'cargo build --release'.")
    files_root = CORPUS / "files"
    if not files_root.is_dir():
        sys.exit(f"No corpus at {files_root} — run ps_corpus_build.py first.")

    by_variant = collections.defaultdict(list)
    for f in files_root.rglob("*.ps"):
        if args.min_mb and f.stat().st_size < args.min_mb * 1048576:
            continue
        by_variant[f.relative_to(files_root).parts[0]].append(f)

    sample = []
    for v in sorted(by_variant):
        fs = sorted(by_variant[v])
        if args.per_variant:
            stride = max(1, len(fs) // args.per_variant)
            fs = fs[::stride][: args.per_variant]
        sample += fs

    ceiling = parse_mem(args.mem_max) * args.jobs / 1024 ** 3
    print(f"sweeping {len(sample)} files across {len(by_variant)} variants")
    print(f"  jobs={args.jobs}  mem-max={args.mem_max}/child  "
          f"timeout={args.timeout}s")
    print(f"  worst-case resident ceiling: {ceiling:.1f} GB"
          f"{'' if HAVE_SYSTEMD_RUN else '  (RLIMIT_AS fallback — no systemd-run)'}")
    if not HAVE_SYSTEMD_RUN:
        print("  WARNING: without systemd-run the cap is address-space based "
              "and less precise.")
    print(flush=True)

    spool = Path(args.out).parent / ".sweep_spool"
    spool.mkdir(parents=True, exist_ok=True)

    results, t0 = [], time.time()
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
        futs = [pool.submit(run_one, f, args, spool) for f in sample]
        for i, fut in enumerate(concurrent.futures.as_completed(futs), 1):
            results.append(fut.result())
            if i % 200 == 0:
                nf = sum(1 for r in results if r["kind"])
                noom = sum(1 for r in results if r["kind"] == "oom")
                print(f"  {i}/{len(sample)}  failures={nf}  oom={noom}  "
                      f"{i/(time.time()-t0):.1f}/s", flush=True)

    with open(args.out, "w") as fh:
        for r in results:
            fh.write(json.dumps(r) + "\n")

    fails = [r for r in results if r["kind"]]
    print(f"\n{'='*72}")
    print(f"{len(fails)} failures / {len(results)} files "
          f"({100*len(fails)/max(1,len(results)):.1f}%) in {time.time()-t0:.0f}s\n")

    for kind in ("oom", "panic", "timeout", "pserror", "jobfail", "exit"):
        group = [r for r in fails if r["kind"] == kind]
        if not group:
            continue
        print(f"--- {kind.upper()}  ({len(group)} occurrences) ---")
        for sig, n in collections.Counter(
                r["signature"] for r in group).most_common(15):
            variants = sorted({r["variant"] for r in group
                               if r["signature"] == sig})
            print(f"  {n:4d}x  {sig}")
            print(f"        variants: {', '.join(variants)}")
        print()

    heavy = sorted(results, key=lambda r: -r["peak_rss_mb"])[:15]
    print("--- heaviest by peak RSS (memory-cost outliers) ---")
    print(f"  {'in_MB':>7} {'peak_MB':>8} {'secs':>6}  path")
    for r in heavy:
        print(f"  {r['in_mb']:7.1f} {r['peak_rss_mb']:8.0f} "
              f"{r['seconds']:6.1f}  {r['path']}")

    print("\n--- failure rate by variant ---")
    tot = collections.Counter(r["variant"] for r in results)
    bad = collections.Counter(r["variant"] for r in fails)
    for v in sorted(tot):
        print(f"  {v:22s} {bad[v]:4d}/{tot[v]:<4d}  {100*bad[v]/tot[v]:5.1f}%")
    print(f"\nfull records: {args.out}")


if __name__ == "__main__":
    main()
