#!/usr/bin/env python3
# stet - A PostScript Interpreter
# Copyright (c) 2026 Scott Bowman
# SPDX-License-Identifier: Apache-2.0 OR MIT

"""Visual regression testing for stet PDF rendering.

Usage:
    # Generate baseline reference images
    ./pdf_visual_test.sh --baseline

    # Compare current output against baseline
    ./pdf_visual_test.sh

    # Compare with custom threshold (default 0.0% pixel difference)
    ./pdf_visual_test.sh --threshold 0.5

    # Test specific samples only
    ./pdf_visual_test.sh --samples 509.pdf 5611.pdf

    # Exclude specific samples
    ./pdf_visual_test.sh --exclude 1058.pdf

    # Pass extra flags to stet-cli
    ./pdf_visual_test.sh -- --dpi 300
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
    if Image:
        Image.MAX_IMAGE_PIXELS = None  # Disable DecompressionBomb warnings for large PDFs
except ImportError:
    Image = None


PROJECT_ROOT = Path(__file__).resolve().parent
SAMPLES_DIR = PROJECT_ROOT / "pdf_samples"
STET_CLI = PROJECT_ROOT / "target" / "release" / "stet"


def get_dirs():
    """Get directory paths for visual tests.

    The banded PNG path is the authoritative baseline. The viewport path
    is a second render of the same samples through the viewer's pipeline;
    it's compared against the same baseline so drift between the two
    paths flags a bug in whichever path diverged.
    """
    base = PROJECT_ROOT / "visual_tests_pdf_png"
    return {
        "base": base,
        "baseline": base / "baseline",
        # Banded PNG (existing paths — unchanged for back-compat).
        "current": base / "current",
        "diff": base / "diff",
        # Viewport PNG (audit path).
        "current_viewport": base / "current_viewport",
        "diff_viewport": base / "diff_viewport",
        "timings": base / "baseline_timings.txt",
        "report": base / "report.html",
        "config": PROJECT_ROOT / "visual_tests_pdf_png.conf",
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
        samples = sorted(SAMPLES_DIR.glob("*.pdf"))
    if exclude:
        exclude_set = set(exclude)
        samples = [s for s in samples if s.name not in exclude_set]
    return samples


def render_one(pdf_file, output_dir, timeout, extra_flags, device="png"):
    """Render a single PDF file. Copies input to output dir, runs stet,
    removes the copy, keeps the PNGs.

    `device` selects which stet rendering path to exercise. "png" is the
    authoritative banded-page path (what baselines are generated from);
    "viewport-png" is the audit path that routes through the same pipeline
    the interactive viewer uses.
    """
    name = pdf_file.stem
    sample_out = output_dir / name
    sample_out.mkdir(parents=True, exist_ok=True)

    # Copy PDF into output dir so stet writes PNGs there
    local_copy = sample_out / pdf_file.name
    shutil.copy2(pdf_file, local_copy)

    start = time.monotonic()
    try:
        cmd = [str(STET_CLI), "--device", device, "--dpi", "150"]
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
            return name, pdf_file.name, pngs, elapsed, stderr_errors
        elif stderr_errors:
            return name, pdf_file.name, None, elapsed, stderr_errors
        else:
            return name, pdf_file.name, "no_pages", elapsed, []
    except subprocess.TimeoutExpired:
        local_copy.unlink(missing_ok=True)
        elapsed = time.monotonic() - start
        return name, pdf_file.name, "timeout", elapsed, []
    except Exception as e:
        local_copy.unlink(missing_ok=True)
        elapsed = time.monotonic() - start
        return name, pdf_file.name, ("exception", str(e)), elapsed, []


PDF_ERROR_RE = re.compile(r"(render error|cannot parse|cannot read|Error:)")


def check_stderr_for_errors(stderr):
    """Check stderr for PDF render errors."""
    errors = []
    for line in stderr.splitlines():
        if PDF_ERROR_RE.search(line):
            errors.append(line.strip())
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


def render_samples(samples, output_dir, timeout=60, extra_flags=None, jobs=1,
                   device="png"):
    output_dir.mkdir(parents=True, exist_ok=True)
    results = {}
    timings = {}
    errors = {}
    total_start = time.monotonic()

    if jobs == 1:
        for pdf_file in samples:
            print(f"  Rendering {pdf_file.name}...", end=" ", flush=True)
            name, fname, pngs, elapsed, stderr_errors = render_one(
                pdf_file, output_dir, timeout, extra_flags, device)
            _report_render_result(name, fname, pngs, elapsed, stderr_errors,
                                  results, timings, errors)
    else:
        # Sort smallest first so short jobs finish early, keeping all workers busy
        ordered = sorted(samples, key=lambda s: s.stat().st_size)
        with concurrent.futures.ProcessPoolExecutor(max_workers=jobs) as pool:
            future_to_file = {
                pool.submit(render_one, pdf_file, output_dir, timeout,
                            extra_flags, device): pdf_file
                for pdf_file in ordered
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


def _status_badge(status, pct):
    if status == "pass":
        return '<span style="color:green">PASS</span>'
    if status == "fail":
        return f'<span style="color:red">FAIL ({pct:.6f}%)</span>'
    if status == "new":
        return '<span style="color:blue">NEW</span>'
    if status == "missing":
        return '<span style="color:orange">MISSING</span>'
    if status == "skip":
        return '<span style="color:gray">SKIP</span>'
    if status == "error":
        return '<span style="color:red">ERROR</span>'
    return f'<span style="color:gray">{status.upper()}</span>'


def _images_block(pages, status, html_path_parent):
    if not pages:
        return ""
    failed_indices = [i for i, (_, _, diff_p) in enumerate(pages) if diff_p is not None]
    show_indices = failed_indices if (status == "fail" and failed_indices) else [0]
    out = ""
    for i in show_indices:
        base_p, curr_p, diff_p = pages[i]
        page_label = f"Page {i + 1}" if len(pages) > 1 else ""
        base_rel = os.path.relpath(base_p, html_path_parent) if base_p else ""
        curr_rel = os.path.relpath(curr_p, html_path_parent) if curr_p else ""
        diff_rel = os.path.relpath(diff_p, html_path_parent) if diff_p else ""
        if page_label:
            out += f'<div style="margin:5px 0"><strong>{page_label}</strong></div>'
        out += '<div style="display:flex;gap:10px;margin:5px 0">'
        if base_rel:
            out += f'<div><div>Baseline</div><img src="{base_rel}" style="max-width:300px" onclick="lb(this.src)"></div>'
        if curr_rel:
            out += f'<div><div>Current</div><img src="{curr_rel}" style="max-width:300px" onclick="lb(this.src)"></div>'
        if diff_rel:
            out += f'<div><div>Diff</div><img src="{diff_rel}" style="max-width:300px" onclick="lb(this.src)"></div>'
        out += "</div>"
    return out


def generate_html_report(merged, html_path, baseline_timings, current_timings,
                         config_overrides=None, default_threshold=0,
                         baseline_wall_clock=None, current_wall_clock=None,
                         compare_time=None, viewport_timings=None,
                         viewport_wall_clock=None):
    """Render the report. `merged` is a list of tuples
    `(name, banded_entry, viewport_entry)` where each entry is the tuple
    `(name, status, pct, pages, errs)` or `None` if that path was skipped.

    Paths whose entries are entirely absent are omitted from the report
    (their column, summary counts, and totals are all hidden).
    """
    total = len(merged)

    # Determine which paths have any data. When a path was skipped via a
    # CLI flag, every entry is None and the whole column is hidden.
    has_banded = any(b is not None for _n, b, _v in merged)
    has_viewport = any(v is not None for _n, _b, v in merged)

    # Summary: worst-of-both status per sample, plus per-path counts.
    worst_status_rank = {"error": 0, "fail": 1, "missing": 2, "new": 3, "skip": 4, "pass": 5}
    n_pass = n_fail = n_error = n_new = n_missing = n_skip = 0
    banded_counts = {"pass": 0, "fail": 0, "new": 0, "error": 0, "skip": 0, "missing": 0}
    vp_counts = dict(banded_counts)
    for _name, b_entry, vp_entry in merged:
        statuses = []
        if b_entry is not None:
            b_status = b_entry[1]
            banded_counts[b_status] = banded_counts.get(b_status, 0) + 1
            statuses.append(b_status)
        if vp_entry is not None:
            v_status = vp_entry[1]
            vp_counts[v_status] = vp_counts.get(v_status, 0) + 1
            statuses.append(v_status)
        if not statuses:
            continue
        worst = min(statuses, key=lambda s: worst_status_rank.get(s, 6))
        if worst == "pass":
            n_pass += 1
        elif worst == "fail":
            n_fail += 1
        elif worst == "error":
            n_error += 1
        elif worst == "new":
            n_new += 1
        elif worst == "missing":
            n_missing += 1
        elif worst == "skip":
            n_skip += 1

    total_bt = sum(baseline_timings.values()) if baseline_timings else 0
    total_ct = sum(current_timings.values()) if current_timings else 0
    total_vt = sum((viewport_timings or {}).values())

    # Per-path pass/fail summary line — only when both paths ran.
    per_path_line = ""
    if has_banded and has_viewport:
        per_path_line = f"""<div style="margin-top:4px">
<strong>Per-path results:</strong>
banded: <span style="color:green">{banded_counts['pass']} pass</span>,
<span style="color:red">{banded_counts['fail']} fail</span>
&nbsp;·&nbsp;
viewport: <span style="color:green">{vp_counts['pass']} pass</span>,
<span style="color:red">{vp_counts['fail']} fail</span>
</div>"""

    header_line = "overall status uses the worse of banded vs viewport"
    if has_banded and not has_viewport:
        header_line = "banded path only"
    elif has_viewport and not has_banded:
        header_line = "viewport path only"

    totals_parts = [f"<strong>Baseline total:</strong> {format_duration(total_bt)}"]
    if has_banded:
        totals_parts.append(f"<strong>Banded total:</strong> {format_duration(total_ct)}")
    if has_viewport:
        totals_parts.append(f"<strong>Viewport total:</strong> {format_duration(total_vt)}")
    totals_line = " &nbsp;|&nbsp; ".join(totals_parts)

    wall_parts = [f'<strong>Baseline wall-clock:</strong> {format_duration(baseline_wall_clock) if baseline_wall_clock else "-"}']
    if has_banded:
        wall_parts.append(f'<strong>Banded wall-clock:</strong> {format_duration(current_wall_clock) if current_wall_clock else "-"}')
    if has_viewport and viewport_wall_clock is not None:
        wall_parts.append(f'<strong>Viewport wall-clock:</strong> {format_duration(viewport_wall_clock)}')
    wall_parts.append(f'<strong>Comparison time:</strong> {format_duration(compare_time) if compare_time else "-"}')
    wall_line = " &nbsp;|&nbsp; ".join(wall_parts)

    summary = f"""<div class="summary">
<div><strong>Total samples:</strong> {total}
&nbsp;—&nbsp; {header_line}
</div>
<div style="margin-top:8px">
<span style="color:green"><strong>{n_pass}</strong> passed</span>
<span style="color:red"><strong>{n_fail}</strong> failed</span>
<span style="color:gray"><strong>{n_error}</strong> errors</span>
<span style="color:blue"><strong>{n_new}</strong> new</span>
<span style="color:orange"><strong>{n_missing}</strong> missing</span>
<span style="color:gray"><strong>{n_skip}</strong> skipped</span>
</div>
{per_path_line}
<div style="margin-top:8px">{totals_line}</div>
<div style="margin-top:4px">{wall_line}</div>
</div>"""

    # Sort: samples where any active path fails come first.
    def _row_order(item):
        _n, b, v = item
        statuses = []
        if b is not None:
            statuses.append(b[1])
        if v is not None:
            statuses.append(v[1])
        worst = min(statuses, key=lambda s: worst_status_rank.get(s, 6)) if statuses else "pass"
        return (worst_status_rank.get(worst, 6), _n)

    rows = []
    for name, b_entry, vp_entry in sorted(merged, key=_row_order):
        if b_entry is not None:
            _n, b_status, b_pct, b_pages, b_errs = b_entry
            b_badge = _status_badge(b_status, b_pct)
        else:
            b_status, b_pct, b_pages, b_errs, b_badge = "pass", None, None, None, ""

        if vp_entry is not None:
            _vn, v_status, v_pct, v_pages, v_errs = vp_entry
            v_badge = _status_badge(v_status, v_pct)
        else:
            v_status, v_pct, v_pages, v_errs, v_badge = "pass", None, None, None, ""

        bt = baseline_timings.get(name)
        ct = current_timings.get(name)
        vt = (viewport_timings or {}).get(name)
        time_baseline = format_duration(bt) if bt is not None else "-"
        time_banded = format_duration(ct) if ct is not None else "-"
        time_viewport = format_duration(vt) if vt is not None else "-"

        threshold = (config_overrides or {}).get(f"{name}.pdf", default_threshold)
        threshold_str = f"{threshold:g}%"
        threshold_display = (
            f'<span style="color:blue">{threshold_str}</span>'
            if threshold > 0 else threshold_str
        )

        def _diff_line(pct, status):
            if pct is None:
                return ""
            color = "green" if pct <= threshold else "red"
            return f'<span style="color:{color}">{pct:.4f}%</span>'

        if b_pages:
            page_count = len(b_pages)
        elif v_pages:
            page_count = len(v_pages)
        else:
            page_count = 0
        name_cell = f"{name}<br><br>Pages: {page_count}<br>Threshold: {threshold_display}"

        b_imgs = _images_block(b_pages, b_status, html_path.parent) if b_entry is not None else ""
        v_imgs = _images_block(v_pages, v_status, html_path.parent) if vp_entry is not None else ""

        def _err_block(errs):
            if not errs:
                return ""
            out = '<div style="margin-top:5px">'
            for e in errs:
                out += f'<pre style="color:red;margin:2px 0;white-space:pre-wrap">{html_mod.escape(e)}</pre>'
            return out + "</div>"

        cells = [f"<td>{name_cell}</td>", f"<td>Baseline<br>{time_baseline}</td>"]
        if has_banded:
            banded_cell = (
                f"{b_badge}<br>Diff: {_diff_line(b_pct, b_status)}<br>"
                f"Time: {time_banded}{_err_block(b_errs)}{b_imgs}"
            )
            cells.append(f"<td>{banded_cell}</td>")
        if has_viewport:
            viewport_cell = (
                f"{v_badge}<br>Diff: {_diff_line(v_pct, v_status)}<br>"
                f"Time: {time_viewport}{_err_block(v_errs)}{v_imgs}"
            )
            cells.append(f"<td>{viewport_cell}</td>")
        rows.append("<tr>" + "".join(cells) + "</tr>")

    headers = ["<th>Sample</th>", "<th>Baseline</th>"]
    if has_banded:
        headers.append('<th>Banded PNG<br><small>(--device png)</small></th>')
    if has_viewport:
        headers.append('<th>Viewport PNG<br><small>(--device viewport-png)</small></th>')
    header_row = "".join(headers)

    html = f"""<!DOCTYPE html>
<html><head><title>stet PDF Visual Regression Report</title>
<style>
{REPORT_CSS}
</style></head><body>
<h1>stet PDF Visual Regression Report</h1>
{summary}
<table>
<thead><tr>{header_row}</tr></thead>
<tbody>
{"".join(rows)}
</tbody>
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
        print("No PDF sample files found.")
        return 1

    if dirs["baseline"].exists():
        shutil.rmtree(dirs["baseline"])

    print(f"Generating baseline for {len(samples)} PDF samples...")
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


def _run_compare_pass(samples, results, render_errors, config_overrides,
                      default_threshold, baseline_dir, diff_dir, jobs):
    """Diff one set of rendered results against the baseline. Returns the
    standard report_data list (one entry per sample)."""
    compare_items = []
    for name, current_pngs in sorted(results.items()):
        compare_items.append((name, ".pdf", current_pngs, render_errors.get(name)))

    report_data = []
    if jobs == 1:
        for name, ext, current_pngs, errs in compare_items:
            print(f"  Comparing {name}{ext}...", end=" ", flush=True)
            r = compare_one(name, ext, current_pngs, errs,
                            default_threshold, config_overrides,
                            baseline_dir, diff_dir)
            _name, status, max_pct, page_data, _errs, msg = r
            print(msg)
            report_data.append((name, status, max_pct, page_data, _errs))
    else:
        def _page_count(item):
            pngs = item[2]
            return -(len(pngs) if isinstance(pngs, list) else 0)
        ordered = sorted(compare_items, key=_page_count)
        with concurrent.futures.ProcessPoolExecutor(max_workers=jobs) as pool:
            futures = {
                pool.submit(compare_one, name, ext, current_pngs, errs,
                            default_threshold, config_overrides,
                            baseline_dir, diff_dir): (name, ext)
                for name, ext, current_pngs, errs in ordered
            }
            for future in concurrent.futures.as_completed(futures):
                name, ext = futures[future]
                _name, status, max_pct, page_data, _errs, msg = future.result()
                print(f"  Compared {name}{ext}... {msg}")
                report_data.append((name, status, max_pct, page_data, _errs))
    return report_data


def _summarise(report_data, label):
    counts = {"pass": 0, "fail": 0, "new": 0, "error": 0, "skip": 0, "missing": 0}
    for _, status, _, _, _ in report_data:
        counts[status] = counts.get(status, 0) + 1
    print(f"  {label}: {counts['pass']} passed, {counts['fail']} failed, "
          f"{counts['skip']} skipped, {counts['new']} new, "
          f"{counts['error']} errors")
    return counts


def cmd_compare(args, dirs):
    if Image is None:
        print("Error: Pillow is required for comparison. Install with: pip install Pillow")
        return 1

    if not dirs["baseline"].exists():
        print("No baseline found. Run with --baseline first.")
        return 1

    samples = get_sample_files(args.samples, args.exclude)
    if not samples:
        print("No PDF sample files found.")
        return 1

    # Positive-selection path flags: default (no flag set) runs both paths.
    # Passing --banded alone runs only banded; --viewport alone runs only
    # viewport; passing both is equivalent to the default.
    if not args.banded and not args.viewport:
        run_banded = run_viewport = True
    else:
        run_banded = args.banded
        run_viewport = args.viewport

    for key in ("current", "diff", "current_viewport", "diff_viewport"):
        if dirs[key].exists():
            shutil.rmtree(dirs[key])

    # ── Banded PNG render (authoritative against baseline) ──
    banded_results = {}
    current_timings = {}
    current_wall_clock = 0.0
    render_errors = {}
    if run_banded:
        print(f"Rendering {len(samples)} PDF samples (banded PNG)...")
        banded_results, current_timings, current_wall_clock, render_errors = render_samples(
            samples, dirs["current"], timeout=args.timeout, extra_flags=args.flags,
            jobs=args.jobs, device="png")
    baseline_timings, baseline_wall_clock = load_timings(dirs["timings"])

    # ── Viewport PNG render (audit against same baseline) ──
    vp_results = {}
    vp_timings = {}
    vp_wall_clock = 0.0
    vp_render_errors = {}
    if run_viewport:
        print(f"\nRendering {len(samples)} PDF samples (viewport PNG)...")
        vp_results, vp_timings, vp_wall_clock, vp_render_errors = render_samples(
            samples, dirs["current_viewport"], timeout=args.timeout,
            extra_flags=args.flags, jobs=args.jobs, device="viewport-png")

    compare_start = time.monotonic()
    config_overrides = load_config(dirs["config"])

    banded_report = []
    if run_banded:
        print("\nComparing banded against baseline...")
        banded_report = _run_compare_pass(
            samples, banded_results, render_errors, config_overrides,
            args.threshold, dirs["baseline"], dirs["diff"], args.jobs)

    viewport_report = []
    if run_viewport:
        print("\nComparing viewport against baseline...")
        viewport_report = _run_compare_pass(
            samples, vp_results, vp_render_errors, config_overrides,
            args.threshold, dirs["baseline"], dirs["diff_viewport"], args.jobs)

    compare_elapsed = time.monotonic() - compare_start
    print(f"\n  Comparison time: {format_duration(compare_elapsed)}")

    banded_counts = _summarise(banded_report, "Banded path") if run_banded else {}
    vp_counts = _summarise(viewport_report, "Viewport path") if run_viewport else {}

    # Merged per-sample report: one row per sample, holding whichever path
    # results are active. Either slot may be None when the corresponding
    # path was skipped.
    banded_by_name = {r[0]: r for r in banded_report}
    vp_by_name = {r[0]: r for r in viewport_report}
    all_names = sorted(set(banded_by_name) | set(vp_by_name))
    merged = [
        (name, banded_by_name.get(name), vp_by_name.get(name))
        for name in all_names
    ]

    report_path = Path(args.html) if args.html else dirs["report"]
    generate_html_report(merged, report_path, baseline_timings, current_timings,
                         config_overrides, args.threshold,
                         baseline_wall_clock, current_wall_clock, compare_elapsed,
                         viewport_timings=vp_timings,
                         viewport_wall_clock=vp_wall_clock)

    fail_count = banded_counts.get("fail", 0) + vp_counts.get("fail", 0)
    return 1 if fail_count > 0 else 0


def main():
    parser = argparse.ArgumentParser(description="Visual regression testing for stet PDF rendering")
    parser.add_argument("--baseline", action="store_true", help="Generate baseline images")
    parser.add_argument("--threshold", type=float, default=0,
                        help="Max allowed pixel difference %% (default: 0)")
    parser.add_argument("--timeout", type=int, default=600,
                        help="Per-sample render timeout in seconds (default: 600)")
    parser.add_argument("--samples", nargs="*", help="Specific PDF sample filenames to test")
    parser.add_argument("--html", type=str, help="Path for HTML report")
    parser.add_argument("--exclude", nargs="*", default=None,
                        help="PDF sample filenames to exclude")
    parser.add_argument("-j", "--jobs", type=int, default=4,
                        help="Number of parallel render workers (default: 4)")
    parser.add_argument("--banded", action="store_true",
                        help="Run only the banded PNG path. Default (no path "
                             "flag) is to run both banded and viewport.")
    parser.add_argument("--viewport", action="store_true",
                        help="Run only the viewport PNG path. Default (no path "
                             "flag) is to run both banded and viewport. "
                             "Pass both --banded and --viewport to force both "
                             "paths (same as the default).")
    parser.add_argument("--flags", nargs=argparse.REMAINDER, default=None,
                        help="Extra flags to pass to stet-cli (must be last argument)")
    args = parser.parse_args()

    if not STET_CLI.exists():
        print(f"Error: stet-cli not found at {STET_CLI}")
        print("Run 'cargo build --release' first.")
        return 1

    dirs = get_dirs()

    if args.baseline:
        return cmd_baseline(args, dirs)
    else:
        return cmd_compare(args, dirs)


if __name__ == "__main__":
    sys.exit(main())
