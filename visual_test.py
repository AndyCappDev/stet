#!/usr/bin/env python3
# xforge - A PostScript Interpreter
# Copyright (c) 2026 Scott Bowman
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Visual regression testing for xforge sample PostScript files.

Usage:
    # Generate baseline reference images
    ./visual_test.sh --baseline

    # Compare current output against baseline
    ./visual_test.sh

    # Compare with custom threshold (default 0.0% pixel difference)
    ./visual_test.sh --threshold 0.5

    # Test specific samples only
    ./visual_test.sh --samples tiger.ps hospital.eps

    # Exclude specific samples
    ./visual_test.sh --exclude eazybbs.ps

    # Pass extra flags to xforge-cli
    ./visual_test.sh -- --dpi 600
"""

import argparse
import concurrent.futures
import html as html_mod
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

try:
    from PIL import Image
except ImportError:
    Image = None


PROJECT_ROOT = Path(__file__).resolve().parent
SAMPLES_DIR = PROJECT_ROOT / "samples"
XFORGE_CLI = PROJECT_ROOT / "target" / "release" / "xforge-cli"


def get_dirs():
    """Get directory paths for visual tests."""
    base = PROJECT_ROOT / "visual_tests_png"
    return {
        "base": base,
        "baseline": base / "baseline",
        "current": base / "current",
        "diff": base / "diff",
        "timings": base / "baseline_timings.txt",
        "report": base / "report.html",
        "config": PROJECT_ROOT / "visual_tests_png.conf",
    }


def load_config(config_file):
    """Load per-sample threshold overrides from a config file."""
    overrides = {}
    if config_file.exists():
        for line in config_file.read_text().splitlines():
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            parts = line.split()
            if len(parts) >= 2:
                try:
                    overrides[parts[0]] = float(parts[1])
                except ValueError:
                    pass
    return overrides


def format_duration(seconds):
    if seconds < 1:
        return f"{seconds * 1000:.0f}ms"
    if seconds < 60:
        return f"{seconds:.1f}s"
    m, s = divmod(seconds, 60)
    return f"{int(m)}m {s:.1f}s"


def get_sample_files(specific=None, exclude=None):
    if specific:
        samples = [SAMPLES_DIR / s for s in specific if (SAMPLES_DIR / s).exists()]
    else:
        samples = sorted(
            list(SAMPLES_DIR.glob("*.ps")) + list(SAMPLES_DIR.glob("*.eps"))
        )
    if exclude:
        exclude_set = set(exclude)
        samples = [s for s in samples if s.name not in exclude_set]
    return samples


def render_one(ps_file, output_dir, timeout, extra_flags):
    """Render a single sample file. Copies input to output dir, runs xforge,
    removes the copy, keeps the PNGs."""
    name = ps_file.stem
    sample_out = output_dir / name
    sample_out.mkdir(parents=True, exist_ok=True)

    # Copy sample into output dir so xforge writes PNGs there
    local_copy = sample_out / ps_file.name
    shutil.copy2(ps_file, local_copy)

    start = time.monotonic()
    try:
        cmd = [str(XFORGE_CLI)]
        if extra_flags:
            cmd.extend(extra_flags)
        cmd.append(local_copy.name)
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=timeout,
            cwd=str(sample_out),
        )
        elapsed = time.monotonic() - start

        # Remove the copied input file, keep only PNGs
        local_copy.unlink(missing_ok=True)

        pngs = sorted(sample_out.glob("*.png"))
        stderr_errors = check_stderr_for_errors(result.stderr)

        if pngs:
            return name, ps_file.name, pngs, elapsed, stderr_errors
        elif stderr_errors:
            return name, ps_file.name, None, elapsed, stderr_errors
        else:
            return name, ps_file.name, "no_pages", elapsed, []
    except subprocess.TimeoutExpired:
        local_copy.unlink(missing_ok=True)
        elapsed = time.monotonic() - start
        return name, ps_file.name, "timeout", elapsed, []
    except Exception as e:
        local_copy.unlink(missing_ok=True)
        elapsed = time.monotonic() - start
        return name, ps_file.name, ("exception", str(e)), elapsed, []


PS_ERROR_RE = re.compile(r"%%\[\s*Error:.*?\]%%")


def check_stderr_for_errors(stderr):
    """Check stderr for PostScript errors."""
    errors = []
    for match in PS_ERROR_RE.finditer(stderr):
        errors.append(match.group(0))
    # Check for Rust panics
    if "thread 'main' panicked" in stderr:
        idx = stderr.index("thread 'main' panicked")
        errors.append(stderr[idx:idx + 500].strip())
    return errors


def _report_render_result(name, fname, pngs, elapsed, stderr_errors,
                          results, timings, errors):
    """Print status and record results for a single render."""
    if isinstance(pngs, list):
        if stderr_errors:
            print(f"OK ({len(pngs)} page(s), {format_duration(elapsed)}) [with errors]")
            errors[name] = stderr_errors
        else:
            print(f"OK ({len(pngs)} page(s), {format_duration(elapsed)})")
        results[name] = pngs
    elif pngs == "no_pages":
        print(f"OK (no pages, {format_duration(elapsed)})")
        results[name] = "no_pages"
    elif pngs == "timeout":
        print(f"TIMEOUT ({format_duration(elapsed)})")
        results[name] = None
    elif isinstance(pngs, tuple) and pngs[0] == "exception":
        print(f"ERROR ({pngs[1]}, {format_duration(elapsed)})")
        results[name] = None
    elif pngs is None:
        print(f"ERROR ({format_duration(elapsed)})")
        errors[name] = stderr_errors
        results[name] = None
    timings[name] = elapsed


def render_samples(samples, output_dir, timeout=60, extra_flags=None, jobs=1):
    output_dir.mkdir(parents=True, exist_ok=True)
    results = {}
    timings = {}
    errors = {}
    total_start = time.monotonic()

    if jobs == 1:
        for ps_file in samples:
            print(f"  Rendering {ps_file.name}...", end=" ", flush=True)
            name, fname, pngs, elapsed, stderr_errors = render_one(
                ps_file, output_dir, timeout, extra_flags)
            _report_render_result(name, fname, pngs, elapsed, stderr_errors,
                                  results, timings, errors)
    else:
        # Sort smallest first so short jobs finish early, keeping all workers busy
        ordered = sorted(samples, key=lambda s: s.stat().st_size)
        with concurrent.futures.ProcessPoolExecutor(max_workers=jobs) as pool:
            future_to_file = {
                pool.submit(render_one, ps_file, output_dir, timeout,
                            extra_flags): ps_file
                for ps_file in ordered
            }
            for future in concurrent.futures.as_completed(future_to_file):
                name, fname, pngs, elapsed, stderr_errors = future.result()
                print(f"  Rendered {fname}...", end=" ", flush=True)
                _report_render_result(name, fname, pngs, elapsed, stderr_errors,
                                      results, timings, errors)

    wall_clock = time.monotonic() - total_start
    summed = sum(timings.values())
    print(f"\n  Wall-clock render time: {format_duration(wall_clock)}")
    print(f"  Summed render time: {format_duration(summed)}")
    return results, timings, wall_clock, errors


def compare_images(baseline_path, current_path):
    img_base = Image.open(baseline_path).convert("RGB")
    img_curr = Image.open(current_path).convert("RGB")

    if img_base.size != img_curr.size:
        return 100.0, None

    pixels_base = img_base.load()
    pixels_curr = img_curr.load()
    w, h = img_base.size
    total = w * h
    diff_count = 0
    diff_img = Image.new("RGB", (w, h), (0, 0, 0))
    diff_pixels = diff_img.load()

    for y in range(h):
        for x in range(w):
            rb, gb, bb = pixels_base[x, y]
            rc, gc, bc = pixels_curr[x, y]
            if rb != rc or gb != gc or bb != bc:
                diff_count += 1
                dr = min(255, abs(rc - rb) * 4)
                dg = min(255, abs(gc - gb) * 4)
                db = min(255, abs(bc - bb) * 4)
                diff_pixels[x, y] = (dr, dg, db)

    pct = (diff_count / total) * 100.0 if total > 0 else 0.0
    return pct, diff_img


def compare_one(name, ext, current_pngs, render_errs, default_threshold,
                config_overrides, baseline_dir, diff_dir):
    """Compare one sample against baseline."""
    if current_pngs == "no_pages":
        if render_errs:
            return (name, "error", 0, None, render_errs, "ERROR (no pages, has errors)")
        else:
            return (name, "skip", 0, None, None, "SKIP (no pages)")

    if current_pngs is None:
        return (name, "error", 0, None, render_errs, "ERROR (render failed)")

    baseline_sample_dir = baseline_dir / name
    if not baseline_sample_dir.exists():
        return (name, "new", 0, None, None, "NEW (no baseline)")

    baseline_pngs = sorted(baseline_sample_dir.glob("*.png"))
    if not baseline_pngs:
        return (name, "missing", 0, None, None, "MISSING (baseline empty)")

    max_pct = 0.0
    page_data = []
    page_count = max(len(baseline_pngs), len(current_pngs))

    for i in range(page_count):
        bp = baseline_pngs[i] if i < len(baseline_pngs) else None
        cp = current_pngs[i] if i < len(current_pngs) else None

        if bp and cp:
            pct, diff_img = compare_images(bp, cp)
            max_pct = max(max_pct, pct)
            diff_path = None
            if diff_img and pct > 0:
                diff_sample = diff_dir / name
                diff_sample.mkdir(parents=True, exist_ok=True)
                diff_path = diff_sample / f"diff-{i:04d}.png"
                diff_img.save(diff_path)
            page_data.append((bp, cp, diff_path))
        else:
            max_pct = 100.0
            page_data.append((bp, cp, None))

    sample_threshold = config_overrides.get(f"{name}{ext}", default_threshold)
    if max_pct <= sample_threshold:
        if sample_threshold != default_threshold:
            msg = f"PASS ({max_pct:.3f}%, threshold {sample_threshold}%)"
        else:
            msg = f"PASS ({max_pct:.3f}%)"
        return (name, "pass", max_pct, page_data, render_errs, msg)
    else:
        msg = f"FAIL ({max_pct:.6f}% difference)"
        return (name, "fail", max_pct, page_data, render_errs, msg)


def save_timings(timings, wall_clock, timings_file):
    timings_file.parent.mkdir(parents=True, exist_ok=True)
    with open(timings_file, "w") as f:
        f.write(f"__wall_clock__\t{wall_clock}\n")
        for name, elapsed in sorted(timings.items()):
            f.write(f"{name}\t{elapsed}\n")


def load_timings(timings_file):
    timings = {}
    wall_clock = None
    if timings_file.exists():
        for line in timings_file.read_text().splitlines():
            parts = line.split("\t")
            if len(parts) == 2:
                if parts[0] == "__wall_clock__":
                    wall_clock = float(parts[1])
                else:
                    timings[parts[0]] = float(parts[1])
    return timings, wall_clock


# ── HTML Report ──────────────────────────────────────────────────────────

REPORT_CSS = """\
body { font-family: sans-serif; margin: 20px; }
table { border-collapse: collapse; width: 100%; }
th, td { border: 1px solid #ccc; padding: 8px; text-align: left; vertical-align: top; }
th { background: #f0f0f0; }
tfoot td { font-weight: bold; background: #f8f8f8; }
.summary { margin-bottom: 20px; padding: 15px; background: #f8f8f8; border: 1px solid #ddd; border-radius: 4px; }
.summary span { margin-right: 20px; }
.lightbox { display:none; position:fixed; top:0; left:0; width:100%; height:100%;
            background:rgba(0,0,0,0.9); z-index:1000; cursor:pointer;
            justify-content:center; align-items:center; }
.lightbox.active { display:flex; }
.lightbox img { max-width:95%; max-height:95%; object-fit:contain; }
td img { cursor: pointer; }"""

REPORT_JS = """\
<div class="lightbox" onclick="this.classList.remove('active')"><img id="lb-img"></div>
<script>
function lb(src){var el=document.querySelector('.lightbox');document.getElementById('lb-img').src=src;el.classList.add('active');}
document.addEventListener('keydown',function(e){if(e.key==='Escape')document.querySelector('.lightbox').classList.remove('active');});
</script>"""


def generate_html_report(report_data, html_path, baseline_timings, current_timings,
                         config_overrides=None, default_threshold=0,
                         baseline_wall_clock=None, current_wall_clock=None,
                         compare_time=None):
    # Summary stats
    counts = {}
    for _, status, _, _, _ in report_data:
        counts[status] = counts.get(status, 0) + 1
    total = len(report_data)
    n_pass = counts.get("pass", 0)
    n_fail = counts.get("fail", 0)
    n_error = counts.get("error", 0)
    n_new = counts.get("new", 0)
    n_missing = counts.get("missing", 0)
    n_skip = counts.get("skip", 0)

    total_bt = sum(baseline_timings.values()) if baseline_timings else 0
    total_ct = sum(current_timings.values()) if current_timings else 0

    summary = f"""<div class="summary">
<div><strong>Total samples:</strong> {total}</div>
<div style="margin-top:8px">
<span style="color:green"><strong>{n_pass}</strong> passed</span>
<span style="color:red"><strong>{n_fail}</strong> failed</span>
<span style="color:gray"><strong>{n_error}</strong> errors</span>
<span style="color:blue"><strong>{n_new}</strong> new</span>
<span style="color:orange"><strong>{n_missing}</strong> missing</span>
<span style="color:gray"><strong>{n_skip}</strong> skipped</span>
</div>
<div style="margin-top:8px">
<strong>Baseline total:</strong> {format_duration(total_bt)} &nbsp;|&nbsp;
<strong>Current total:</strong> {format_duration(total_ct)}
</div>
<div style="margin-top:4px">
<strong>Baseline wall-clock:</strong> {format_duration(baseline_wall_clock) if baseline_wall_clock else "-"} &nbsp;|&nbsp;
<strong>Current wall-clock:</strong> {format_duration(current_wall_clock) if current_wall_clock else "-"} &nbsp;|&nbsp;
<strong>Comparison time:</strong> {format_duration(compare_time) if compare_time else "-"}
</div>
</div>"""

    # Table rows (failures first, then sorted by name)
    rows = []
    for name, status, pct, pages, errs in sorted(report_data, key=lambda r: (r[1] != "fail", r[0])):
        if status == "pass":
            badge = '<span style="color:green">PASS</span>'
        elif status == "fail":
            badge = f'<span style="color:red">FAIL ({pct:.6f}%)</span>'
        elif status == "new":
            badge = '<span style="color:blue">NEW</span>'
        elif status == "missing":
            badge = '<span style="color:orange">MISSING</span>'
        elif status == "skip":
            badge = '<span style="color:gray">SKIP (no pages)</span>'
        elif status == "error":
            badge = '<span style="color:red">ERROR</span>'
        else:
            badge = f'<span style="color:gray">{status.upper()}</span>'

        bt = baseline_timings.get(name)
        ct = current_timings.get(name)
        time_baseline = format_duration(bt) if bt is not None else "-"
        time_current = format_duration(ct) if ct is not None else "-"

        threshold = (config_overrides or {}).get(f"{name}.ps",
                     (config_overrides or {}).get(f"{name}.eps", default_threshold))
        threshold_str = f"{threshold:g}%"

        imgs = ""
        if pages:
            failed_indices = [i for i, (_, _, diff_p) in enumerate(pages) if diff_p is not None]
            show_indices = failed_indices if (status == "fail" and failed_indices) else [0]
            for i in show_indices:
                base_p, curr_p, diff_p = pages[i]
                page_label = f"Page {i + 1}" if len(pages) > 1 else ""
                base_rel = os.path.relpath(base_p, html_path.parent) if base_p else ""
                curr_rel = os.path.relpath(curr_p, html_path.parent) if curr_p else ""
                diff_rel = os.path.relpath(diff_p, html_path.parent) if diff_p else ""
                if page_label:
                    imgs += f'<div style="margin:5px 0"><strong>{page_label}</strong></div>'
                imgs += '<div style="display:flex;gap:10px;margin:5px 0">'
                if base_rel:
                    imgs += f'<div><div>Baseline</div><img src="{base_rel}" style="max-width:300px" onclick="lb(this.src)"></div>'
                if curr_rel:
                    imgs += f'<div><div>Current</div><img src="{curr_rel}" style="max-width:300px" onclick="lb(this.src)"></div>'
                if diff_rel:
                    imgs += f'<div><div>Diff</div><img src="{diff_rel}" style="max-width:300px" onclick="lb(this.src)"></div>'
                imgs += "</div>"

        error_html = ""
        if errs:
            error_html = '<div style="margin-top:5px">'
            for e in errs:
                error_html += f'<pre style="color:red;margin:2px 0;white-space:pre-wrap">{html_mod.escape(e)}</pre>'
            error_html += "</div>"

        page_count = len(pages) if pages else 0
        threshold_display = f'<span style="color:blue">{threshold_str}</span>' if threshold > 0 else threshold_str
        if pct is not None:
            diff_color = "green" if pct <= threshold else "red"
            diff_display = f'Diff: <span style="color:{diff_color}">{pct:.2f}%</span>'
        else:
            diff_display = ""
        if page_count:
            name_cell = f"{name}<br><br>Pages: {page_count}<br>Threshold: {threshold_display}"
        else:
            name_cell = f"{name}<br><br>Threshold: {threshold_display}"
        if diff_display:
            name_cell += f"<br>{diff_display}"

        rows.append(
            f"<tr><td>{name_cell}</td><td>{badge}</td>"
            f"<td>{time_baseline}</td><td>{time_current}</td>"
            f"<td>{imgs}{error_html}</td></tr>"
        )

    html = f"""<!DOCTYPE html>
<html><head><title>xforge Visual Regression Report</title>
<style>
{REPORT_CSS}
</style></head><body>
<h1>xforge Visual Regression Report</h1>
{summary}
<table>
<thead><tr><th>Sample</th><th>Status</th><th>Baseline Time</th><th>Current Time</th><th>Images</th></tr></thead>
<tbody>
{"".join(rows)}
</tbody>
<tfoot><tr><td>Total</td><td></td><td>{format_duration(total_bt)}</td><td>{format_duration(total_ct)}</td><td></td></tr></tfoot>
</table>
{REPORT_JS}
</body></html>"""

    html_path.parent.mkdir(parents=True, exist_ok=True)
    html_path.write_text(html)
    print(f"HTML report written to {html_path}")


# ── Commands ─────────────────────────────────────────────────────────────

def cmd_baseline(args, dirs):
    samples = get_sample_files(args.samples, args.exclude)
    if not samples:
        print("No sample files found.")
        return 1

    if dirs["baseline"].exists():
        shutil.rmtree(dirs["baseline"])

    print(f"Generating baseline for {len(samples)} samples...")
    results, timings, wall_clock, errors = render_samples(
        samples, dirs["baseline"], timeout=args.timeout, extra_flags=args.flags,
        jobs=args.jobs)
    save_timings(timings, wall_clock, dirs["timings"])

    ok = sum(1 for v in results.values() if v is not None)
    fail = sum(1 for v in results.values() if v is None)
    print(f"Baseline complete: {ok} succeeded, {fail} failed")
    if errors:
        print(f"  {len(errors)} sample(s) had errors:")
        for name, errs in sorted(errors.items()):
            for e in errs:
                print(f"    {name}: {e}")
    return 0


def cmd_compare(args, dirs):
    if Image is None:
        print("Error: Pillow is required for comparison. Install with: pip install Pillow")
        return 1

    if not dirs["baseline"].exists():
        print("No baseline found. Run with --baseline first.")
        return 1

    samples = get_sample_files(args.samples, args.exclude)
    if not samples:
        print("No sample files found.")
        return 1

    if dirs["current"].exists():
        shutil.rmtree(dirs["current"])
    if dirs["diff"].exists():
        shutil.rmtree(dirs["diff"])

    print(f"Rendering {len(samples)} samples...")
    results, current_timings, current_wall_clock, render_errors = render_samples(
        samples, dirs["current"], timeout=args.timeout, extra_flags=args.flags,
        jobs=args.jobs)
    baseline_timings, baseline_wall_clock = load_timings(dirs["timings"])

    print("Comparing against baseline...")
    compare_start = time.monotonic()
    config_overrides = load_config(dirs["config"])
    report_data = []
    pass_count = 0
    fail_count = 0
    new_count = 0
    error_count = 0
    skip_count = 0

    compare_items = []
    for name, current_pngs in sorted(results.items()):
        matching = [s for s in samples if s.stem == name]
        ext = matching[0].suffix if matching else ".ps"
        compare_items.append((name, ext, current_pngs, render_errors.get(name)))

    if args.jobs == 1:
        for name, ext, current_pngs, errs in compare_items:
            print(f"  Comparing {name}{ext}...", end=" ", flush=True)
            r = compare_one(name, ext, current_pngs, errs,
                            args.threshold, config_overrides,
                            dirs["baseline"], dirs["diff"])
            _name, status, max_pct, page_data, _errs, msg = r
            print(msg)
            report_data.append((name, status, max_pct, page_data, _errs))
    else:
        # Sort by page count descending so multi-page samples start first
        def _page_count(item):
            pngs = item[2]
            return -(len(pngs) if isinstance(pngs, list) else 0)
        ordered = sorted(compare_items, key=_page_count)
        with concurrent.futures.ProcessPoolExecutor(max_workers=args.jobs) as pool:
            futures = {
                pool.submit(compare_one, name, ext, current_pngs, errs,
                            args.threshold, config_overrides,
                            dirs["baseline"], dirs["diff"]): (name, ext)
                for name, ext, current_pngs, errs in ordered
            }
            for future in concurrent.futures.as_completed(futures):
                name, ext = futures[future]
                _name, status, max_pct, page_data, _errs, msg = future.result()
                print(f"  Compared {name}{ext}... {msg}")
                report_data.append((name, status, max_pct, page_data, _errs))

    for _, status, _, _, _ in report_data:
        if status == "pass":
            pass_count += 1
        elif status == "fail":
            fail_count += 1
        elif status == "new":
            new_count += 1
        elif status == "error":
            error_count += 1
        elif status == "skip":
            skip_count += 1

    compare_elapsed = time.monotonic() - compare_start
    print(f"\n  Comparison time: {format_duration(compare_elapsed)}")
    if error_count > 0:
        print(f"ERRORS: {error_count} (render failed)")
    print(f"Results: {pass_count} passed, {fail_count} failed, "
          f"{skip_count} skipped, {new_count} new, {error_count} errors")

    report_path = Path(args.html) if args.html else dirs["report"]
    generate_html_report(report_data, report_path, baseline_timings, current_timings,
                         config_overrides, args.threshold,
                         baseline_wall_clock, current_wall_clock, compare_elapsed)

    return 1 if fail_count > 0 else 0


def main():
    parser = argparse.ArgumentParser(description="Visual regression testing for xforge")
    parser.add_argument("--baseline", action="store_true", help="Generate baseline images")
    parser.add_argument("--threshold", type=float, default=0,
                        help="Max allowed pixel difference %% (default: 0)")
    parser.add_argument("--timeout", type=int, default=600,
                        help="Per-sample render timeout in seconds (default: 600)")
    parser.add_argument("--samples", nargs="*", help="Specific sample filenames to test")
    parser.add_argument("--html", type=str, help="Path for HTML report")
    parser.add_argument("--exclude", nargs="*", default=None,
                        help="Sample filenames to exclude")
    parser.add_argument("-j", "--jobs", type=int, default=4,
                        help="Number of parallel render workers (default: 4)")
    parser.add_argument("--flags", nargs=argparse.REMAINDER, default=None,
                        help="Extra flags to pass to xforge-cli (must be last argument)")
    args = parser.parse_args()

    if not XFORGE_CLI.exists():
        print(f"Error: xforge-cli not found at {XFORGE_CLI}")
        print("Run 'cargo build --release' first.")
        return 1

    dirs = get_dirs()

    if args.baseline:
        return cmd_baseline(args, dirs)
    else:
        return cmd_compare(args, dirs)


if __name__ == "__main__":
    sys.exit(main())
