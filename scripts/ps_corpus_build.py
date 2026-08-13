#!/usr/bin/env python3
# stet - A PostScript Interpreter
# Copyright (c) 2026 Scott Bowman
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Build a bulk PostScript corpus for differential testing against Ghostscript.

The corpus is assembled from two sources:

  1. PDFs already on disk, transcoded to PostScript through two independent
     converters (poppler's ``pdftops`` and Ghostscript's ``ps2write``) across a
     matrix of language levels. Two unrelated producers means the corpus is not
     stylistically captured by either engine.
  2. PostScript / EPS files already on disk, ingested verbatim.

Everything is content-hashed on the way in, so the heavy duplication across
backup trees collapses to one copy, and every artifact gets a stable id that
survives renames and re-runs.

PostForge trees are excluded unconditionally — see EXCLUDE_PATTERNS.

Usage:
    # 1. Discover and dedup inputs (fast, no conversion)
    ./scripts/ps_corpus_build.py scan
    ./scripts/ps_corpus_build.py scan --scan-root /data --scan-root /home/scott

    # 2. Transcode (resumable; safe to interrupt and re-run)
    ./scripts/ps_corpus_build.py build --jobs 8 --max-gb 100

    # 3. Report
    ./scripts/ps_corpus_build.py status

Layout (default CORPUS_ROOT=/data/stet-ps-corpus, symlinked as ./ps_corpus):

    inputs.jsonl                     deduped input inventory
    manifest.jsonl                   one record per produced PostScript file
    files/<variant>/<xx>/<sha12>.ps  the corpus itself
"""

import argparse
import concurrent.futures
import hashlib
import json
import os
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_CORPUS_ROOT = Path("/data/stet-ps-corpus")
CORPUS_SYMLINK = PROJECT_ROOT / "ps_corpus"

# Paths containing any of these substrings (case-insensitive) are never
# ingested. PostForge is excluded by explicit instruction: it is the AGPL
# sister project and its sample tree stays out of stet's corpus entirely.
EXCLUDE_PATTERNS = [
    "postforge",
    "/proc/",
    "/sys/",
    "/dev/",
    "/run/",
    "/.git/",
    "/target/",
    "/ps_corpus/",
    "/stet-ps-corpus/",
    "/visual_tests_",
    "/.cargo/",
    "/node_modules/",
]

PDF_EXTS = {".pdf"}
PS_EXTS = {".ps", ".eps", ".epsf", ".epsi"}

# Applied to every deduped PDF input.
CORE_VARIANTS = ["pdftops-level3", "gs-ps2write"]

# Applied only to a stratified sample — these exist for dialect diversity,
# which a sample spanning the size distribution buys just as well as the full
# cross product, at a fraction of the disk.
EXTENDED_VARIANTS = [
    "pdftops-level1",
    "pdftops-level1sep",
    "pdftops-level2",
    "pdftops-level2sep",
    "pdftops-level3sep",
    "pdftops-eps",
    "gs-eps2write",
]

ALL_VARIANTS = CORE_VARIANTS + EXTENDED_VARIANTS + ["native"]


def build_command(variant, src, dst):
    """Return the argv that converts `src` (a PDF) into `dst` (PostScript).

    The -eps variants force a single page: EPS is single-page by definition and
    both converters fail outright on a multi-page input otherwise.
    """
    if variant.startswith("pdftops-"):
        level = variant[len("pdftops-"):]
        argv = ["pdftops"]
        if level == "eps":
            argv += ["-f", "1", "-l", "1", "-eps"]
        else:
            argv += [f"-{level}"]
        return argv + [str(src), str(dst)]

    if variant in ("gs-ps2write", "gs-eps2write"):
        device = "ps2write" if variant == "gs-ps2write" else "eps2write"
        argv = [
            "gs", "-q", "-dSAFER", "-dBATCH", "-dNOPAUSE",
            f"-sDEVICE={device}",
        ]
        if device == "eps2write":
            argv += ["-dFirstPage=1", "-dLastPage=1"]
        return argv + [f"-sOutputFile={dst}", str(src)]

    raise ValueError(f"unknown variant: {variant}")


def sha256_file(path, chunk=1 << 20):
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        while True:
            block = fh.read(chunk)
            if not block:
                break
            h.update(block)
    return h.hexdigest()


def is_excluded(path_str, extra_patterns):
    low = path_str.lower()
    for pat in EXCLUDE_PATTERNS:
        if pat in low:
            return True
    for pat in extra_patterns:
        if pat.lower() in low:
            return True
    return False


def human(n):
    for unit in ("B", "KB", "MB", "GB", "TB"):
        if abs(n) < 1024:
            return f"{n:.1f}{unit}"
        n /= 1024
    return f"{n:.1f}PB"


def corpus_root(args):
    return Path(args.corpus_root).resolve()


def read_jsonl(path):
    if not path.exists():
        return []
    out = []
    with open(path) as fh:
        for line in fh:
            line = line.strip()
            if line:
                out.append(json.loads(line))
    return out


# ---------------------------------------------------------------- scan


def cmd_scan(args):
    root = corpus_root(args)
    root.mkdir(parents=True, exist_ok=True)

    scan_roots = [Path(p).resolve() for p in args.scan_root] if args.scan_root else [
        PROJECT_ROOT / "pdf_samples",
        PROJECT_ROOT / "more_pdf_samples",
        PROJECT_ROOT / "ps_samples",
    ]

    print("Scanning for inputs...")
    for r in scan_roots:
        print(f"  root: {r}")
    if args.exclude:
        print(f"  extra excludes: {', '.join(args.exclude)}")

    candidates = []
    excluded = 0
    for r in scan_roots:
        if not r.exists():
            print(f"  (skip, missing: {r})")
            continue
        for dirpath, dirnames, filenames in os.walk(r, followlinks=False):
            if is_excluded(dirpath + "/", args.exclude):
                excluded += len(filenames)
                dirnames[:] = []
                continue
            for fn in filenames:
                ext = Path(fn).suffix.lower()
                if ext not in PDF_EXTS and ext not in PS_EXTS:
                    continue
                full = os.path.join(dirpath, fn)
                if is_excluded(full, args.exclude):
                    excluded += 1
                    continue
                candidates.append(full)

    print(f"  {len(candidates)} candidate files ({excluded} excluded by pattern)")
    print("Hashing and deduplicating...")

    seen = {}
    dup_bytes = 0
    errors = 0
    for i, path in enumerate(candidates):
        if i and i % 500 == 0:
            print(f"  ...{i}/{len(candidates)}")
        try:
            size = os.path.getsize(path)
            digest = sha256_file(path)
        except OSError:
            errors += 1
            continue
        if digest in seen:
            seen[digest]["duplicate_paths"].append(path)
            dup_bytes += size
            continue
        ext = Path(path).suffix.lower()
        seen[digest] = {
            "sha256": digest,
            "path": path,
            "size": size,
            "kind": "pdf" if ext in PDF_EXTS else "ps",
            "duplicate_paths": [],
        }

    records = sorted(seen.values(), key=lambda r: r["size"])
    inputs_path = root / "inputs.jsonl"
    with open(inputs_path, "w") as fh:
        for rec in records:
            fh.write(json.dumps(rec) + "\n")

    pdfs = [r for r in records if r["kind"] == "pdf"]
    pss = [r for r in records if r["kind"] == "ps"]
    total = sum(r["size"] for r in records)

    print()
    print(f"  unique inputs : {len(records)}  ({len(pdfs)} PDF, {len(pss)} PS/EPS)")
    print(f"  unique bytes  : {human(total)}")
    print(f"  dedup saved   : {human(dup_bytes)} across "
          f"{sum(len(r['duplicate_paths']) for r in records)} duplicate copies")
    if errors:
        print(f"  unreadable    : {errors}")
    print(f"  written       : {inputs_path}")
    print()
    print("Next: ./scripts/ps_corpus_build.py build")


# ---------------------------------------------------------------- build


class Budget:
    """Thread-safe running total of bytes written, with a hard ceiling."""

    def __init__(self, max_bytes):
        self.max_bytes = max_bytes
        self.used = 0
        self.lock = threading.Lock()
        self.exceeded = False

    def add(self, n):
        with self.lock:
            self.used += n
            if self.max_bytes and self.used >= self.max_bytes:
                self.exceeded = True
            return self.exceeded


def out_path_for(root, variant, digest):
    short = digest[:12]
    return root / "files" / variant / short[:2] / f"{short}.ps"


def do_one(rec, variant, root, budget, timeout, max_output_bytes):
    """Produce one corpus file. Returns a manifest record."""
    digest = rec["sha256"]
    dst = out_path_for(root, variant, digest)
    dst.parent.mkdir(parents=True, exist_ok=True)
    started = time.time()

    base = {
        "sha256": None,
        "input_sha256": digest,
        "input_path": rec["path"],
        "variant": variant,
        "path": str(dst.relative_to(root)),
    }

    if variant == "native":
        try:
            shutil.copy2(rec["path"], dst)
        except OSError as exc:
            return {**base, "status": "error", "error": str(exc),
                    "size": 0, "seconds": 0.0}
        size = dst.stat().st_size
        budget.add(size)
        return {**base, "status": "ok", "size": size,
                "sha256": sha256_file(dst),
                "seconds": round(time.time() - started, 2)}

    argv = build_command(variant, rec["path"], dst)
    try:
        proc = subprocess.run(
            argv, capture_output=True, timeout=timeout, check=False,
        )
        status = "ok" if proc.returncode == 0 else "convert-failed"
        err = proc.stderr.decode("utf-8", "replace")[:400]
    except subprocess.TimeoutExpired:
        status, err = "timeout", f"exceeded {timeout}s"
    except OSError as exc:
        status, err = "error", str(exc)

    elapsed = round(time.time() - started, 2)

    if not dst.exists() or dst.stat().st_size == 0:
        dst.unlink(missing_ok=True)
        return {**base, "status": status if status != "ok" else "empty",
                "error": err if status != "ok" else "produced no output",
                "size": 0, "seconds": elapsed}

    size = dst.stat().st_size
    if max_output_bytes and size > max_output_bytes:
        dst.unlink()
        return {**base, "status": "oversized",
                "error": f"{human(size)} exceeds cap", "size": size,
                "seconds": elapsed}

    budget.add(size)
    out = {**base, "status": "ok" if status == "ok" else "partial",
           "size": size, "sha256": sha256_file(dst), "seconds": elapsed}
    if status != "ok":
        # A nonzero exit that still produced output is worth keeping: partial
        # PostScript is exactly the malformed input the harness should see.
        out["error"] = err
    return out


def cmd_build(args):
    root = corpus_root(args)
    inputs_path = root / "inputs.jsonl"
    if not inputs_path.exists():
        sys.exit(f"No inputs.jsonl at {inputs_path} — run 'scan' first.")

    inputs = read_jsonl(inputs_path)
    manifest_path = root / "manifest.jsonl"
    done = {(r["input_sha256"], r["variant"]) for r in read_jsonl(manifest_path)}
    if done:
        print(f"Resuming: {len(done)} conversions already recorded.")

    pdfs = [r for r in inputs if r["kind"] == "pdf"]
    pss = [r for r in inputs if r["kind"] == "ps"]

    # Stratify the extended matrix across the size distribution. inputs.jsonl is
    # written size-sorted, so a fixed stride spans small to large evenly.
    sample = []
    if pdfs and args.extended_sample > 0:
        stride = max(1, len(pdfs) // args.extended_sample)
        sample = pdfs[::stride][: args.extended_sample]
    sample_shas = {r["sha256"] for r in sample}

    jobs = []
    for rec in pss:
        jobs.append((rec, "native"))
    for rec in pdfs:
        for v in CORE_VARIANTS:
            jobs.append((rec, v))
        if rec["sha256"] in sample_shas:
            for v in EXTENDED_VARIANTS:
                jobs.append((rec, v))

    jobs = [(r, v) for (r, v) in jobs if (r["sha256"], v) not in done]

    print(f"  inputs        : {len(pdfs)} PDF, {len(pss)} PS/EPS")
    print(f"  extended set  : {len(sample)} PDFs x {len(EXTENDED_VARIANTS)} variants")
    print(f"  conversions   : {len(jobs)} to run")
    print(f"  disk ceiling  : {human(args.max_gb * 1024 ** 3)}")
    print(f"  jobs          : {args.jobs}")
    if args.dry_run:
        by_variant = {}
        for _, v in jobs:
            by_variant[v] = by_variant.get(v, 0) + 1
        print("\n  dry run — planned conversions per variant:")
        for v in sorted(by_variant):
            print(f"    {v:22s} {by_variant[v]}")
        return

    budget = Budget(int(args.max_gb * 1024 ** 3))
    max_out = int(args.max_output_mb * 1024 ** 2)
    counts = {}
    written = 0
    started = time.time()

    with open(manifest_path, "a") as mf:
        lock = threading.Lock()
        with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
            futures = {
                pool.submit(do_one, rec, v, root, budget, args.timeout, max_out): (rec, v)
                for rec, v in jobs
            }
            try:
                for i, fut in enumerate(concurrent.futures.as_completed(futures), 1):
                    result = fut.result()
                    with lock:
                        mf.write(json.dumps(result) + "\n")
                        counts[result["status"]] = counts.get(result["status"], 0) + 1
                        written += result.get("size", 0) if result["status"] in ("ok", "partial") else 0
                        if i % 200 == 0:
                            mf.flush()
                            rate = i / max(1e-9, time.time() - started)
                            print(f"  {i}/{len(jobs)}  {human(budget.used)} written  "
                                  f"{rate:.1f}/s  " +
                                  " ".join(f"{k}={v}" for k, v in sorted(counts.items())))
                    if budget.exceeded:
                        print("\n  Disk ceiling reached — stopping cleanly.")
                        for f in futures:
                            f.cancel()
                        break
            except KeyboardInterrupt:
                print("\n  Interrupted — manifest is consistent, re-run to resume.")
                for f in futures:
                    f.cancel()

    elapsed = time.time() - started
    print()
    print(f"  produced      : {human(written)} in {elapsed / 60:.1f} min")
    for k, v in sorted(counts.items()):
        print(f"  {k:14s}: {v}")
    print(f"  manifest      : {manifest_path}")


# ---------------------------------------------------------------- status


def cmd_status(args):
    root = corpus_root(args)
    inputs = read_jsonl(root / "inputs.jsonl")
    manifest = read_jsonl(root / "manifest.jsonl")

    if not inputs:
        sys.exit(f"No corpus at {root} — run 'scan' first.")

    print(f"Corpus root: {root}")
    print(f"  inputs        : {len(inputs)} unique "
          f"({sum(1 for r in inputs if r['kind'] == 'pdf')} PDF, "
          f"{sum(1 for r in inputs if r['kind'] == 'ps')} PS/EPS)")

    if not manifest:
        print("  corpus        : not built yet — run 'build'")
        return

    ok = [r for r in manifest if r["status"] in ("ok", "partial")]
    by_variant = {}
    for r in ok:
        v = by_variant.setdefault(r["variant"], {"n": 0, "bytes": 0})
        v["n"] += 1
        v["bytes"] += r["size"]

    print(f"  corpus files  : {len(ok)}")
    print(f"  corpus bytes  : {human(sum(r['size'] for r in ok))}")
    print()
    print(f"  {'variant':22s} {'files':>7s} {'bytes':>10s}")
    for v in sorted(by_variant):
        d = by_variant[v]
        print(f"  {v:22s} {d['n']:7d} {human(d['bytes']):>10s}")

    # Distinct output content — near-identical inputs collapse here.
    uniq = {r["sha256"] for r in ok if r.get("sha256")}
    print()
    print(f"  distinct content: {len(uniq)} of {len(ok)} files "
          f"({100 * len(uniq) / max(1, len(ok)):.1f}% unique)")

    failed = {}
    for r in manifest:
        if r["status"] not in ("ok", "partial"):
            failed[r["status"]] = failed.get(r["status"], 0) + 1
    if failed:
        print()
        print("  conversion failures (expected — malformed input is useful input):")
        for k, v in sorted(failed.items()):
            print(f"    {k:14s}: {v}")


# ---------------------------------------------------------------- main


def main():
    ap = argparse.ArgumentParser(
        description="Build a PostScript corpus for gs differential testing.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    ap.add_argument("--corpus-root", default=str(DEFAULT_CORPUS_ROOT),
                    help=f"corpus location (default: {DEFAULT_CORPUS_ROOT})")
    sub = ap.add_subparsers(dest="command", required=True)

    p = sub.add_parser("scan", help="discover and deduplicate inputs")
    p.add_argument("--scan-root", action="append", default=[],
                   help="directory to scan (repeatable; default: project sample dirs)")
    p.add_argument("--exclude", action="append", default=[],
                   help="extra path substring to exclude (repeatable)")
    p.set_defaults(func=cmd_scan)

    p = sub.add_parser("build", help="run the transcode matrix")
    p.add_argument("--jobs", type=int, default=max(1, (os.cpu_count() or 4) // 2))
    p.add_argument("--timeout", type=int, default=120, help="per-conversion seconds")
    p.add_argument("--max-gb", type=float, default=100.0, help="disk ceiling")
    p.add_argument("--max-output-mb", type=float, default=256.0,
                   help="discard any single output larger than this")
    p.add_argument("--extended-sample", type=int, default=800,
                   help="PDFs to run the extended variant matrix over (0 = none)")
    p.add_argument("--dry-run", action="store_true")
    p.set_defaults(func=cmd_build)

    p = sub.add_parser("status", help="summarise the corpus")
    p.set_defaults(func=cmd_status)

    args = ap.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
