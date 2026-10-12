#!/usr/bin/env python3
"""Judge a rendered page against a supersampled reference.

For docs/PLAN-CLIP-AA.md section 5. The reference is the same page rendered
at N times the resolution with anti-aliasing off, averaged down N to 1: a
picture of what each pixel should hold that owes nothing to how stet
anti-aliases anything.

    clip_aa_measure.py render <file> [--page P] [--dpi D] [--factor N]
                              [--stet BIN] [--base-stet BIN] [--mark OUT.png]
    clip_aa_measure.py compare TEST.png REF.png [--factor N] [--base BASE.png]
                              [--mark OUT.png]

Two measurements:

  accuracy  mean error on the pixels the reference calls an edge, and the
            mean signed error (positive: the test is lighter). With a base
            image (the page from a build before the change) the same for
            only the pixels the change altered, before and after.

  seams     pixels where the reference is flat, and so are its neighbours,
            and the test differs from it. Nothing but a seam or a halo puts
            a different colour in the middle of a flat area.
"""

import argparse
import os
import subprocess
import sys
import tempfile

import numpy as np
from PIL import Image

Image.MAX_IMAGE_PIXELS = None

# A block of the reference is flat when no channel varies by more than this.
FLAT = 6
# A flat pixel is a seam when some channel is off by at least this.
SEAM = 6


def load(path):
    im = Image.open(path)
    if im.mode in ("RGBA", "LA"):
        white = Image.new("RGBA", im.size, (255, 255, 255, 255))
        im = Image.alpha_composite(white, im.convert("RGBA"))
    return np.asarray(im.convert("RGB"))


def fit(ref, height, width, anchor):
    """Crop or pad `ref` with white to height x width, keeping the top or
    the bottom rows in place. Columns keep their left edge."""
    out = np.full((height, width, 3), 255, dtype=np.uint8)
    h = min(height, ref.shape[0])
    w = min(width, ref.shape[1])
    if anchor == "top":
        out[:h, :w] = ref[:h, :w]
    else:
        out[height - h :, :w] = ref[ref.shape[0] - h :, :w]
    return out


def reduce(ref, n):
    """Average n x n blocks; also say which blocks are flat."""
    h, w = ref.shape[0] // n, ref.shape[1] // n
    mean = np.empty((h, w, 3), dtype=np.float32)
    flat = np.empty((h, w), dtype=bool)
    step = 64
    for y in range(0, h, step):
        rows = ref[y * n : min(y + step, h) * n].reshape(-1, n, w, n, 3)
        mean[y : y + step] = rows.mean(axis=(1, 3), dtype=np.float32)
        spread = rows.max(axis=(1, 3)).astype(np.int16) - rows.min(axis=(1, 3))
        flat[y : y + step] = (spread <= FLAT).all(axis=2)
    return mean, flat


def erode(mask):
    """True where the pixel and its eight neighbours are all true."""
    out = mask.copy()
    out[1:] &= mask[:-1]
    out[:-1] &= mask[1:]
    out[:, 1:] &= out[:, :-1].copy()
    out[:, :-1] &= out[:, 1:].copy()
    # The two passes above shrink by one along each axis in turn, which is
    # the 3x3 erosion.
    return out


def level(mean):
    """True where the 3x3 neighbourhood of the reduced reference is one
    colour. A flat block beside a flat block of another colour is an edge
    that happens to fall between pixels, not the middle of anything."""
    lo = mean.copy()
    hi = mean.copy()
    for axis in (0, 1):
        for shift in (1, -1):
            moved_lo = np.roll(lo, shift, axis=axis)
            moved_hi = np.roll(hi, shift, axis=axis)
            np.minimum(lo, moved_lo, out=lo)
            np.maximum(hi, moved_hi, out=hi)
    # Two rolls along each axis in turn reach the diagonal neighbours too,
    # and one pixel further along each axis, which only makes this stricter.
    return ((hi - lo) <= FLAT).all(axis=2)


def luma(a):
    return a[..., 0] * 0.299 + a[..., 1] * 0.587 + a[..., 2] * 0.114


def compare(test_path, ref_path, factor, base_path=None, mark=None):
    test = load(test_path)
    ref = load(ref_path)
    h, w = test.shape[:2]
    want = (h * factor, w * factor)
    note = "exact"
    if ref.shape[:2] != want:
        # The page's pixel size is rounded at each resolution, so the big
        # one is not always N times the small one. Which edge stays put
        # depends on the device; take the one that agrees better.
        best = None
        for anchor in ("bottom", "top"):
            mean, flat = reduce(fit(ref, want[0], want[1], anchor), factor)
            err = np.abs(test - mean).mean()
            if best is None or err < best[0]:
                best = (err, anchor, mean, flat)
        _, anchor, mean, flat = best
        note = "%dx%d not %dx%d, kept the %s" % (
            ref.shape[1],
            ref.shape[0],
            want[1],
            want[0],
            anchor,
        )
    else:
        mean, flat = reduce(ref, factor)

    diff = test.astype(np.float32) - mean
    absd = np.abs(diff)
    signed = luma(diff)

    edge = ~flat
    calm = erode(flat) & level(mean)
    seam = calm & (absd.max(axis=2) >= SEAM)

    out = {
        "size": "%dx%d" % (w, h),
        "fit": note,
        "edge_px": int(edge.sum()),
        "edge_err": float(absd[edge].mean()) if edge.any() else 0.0,
        "edge_bias": float(signed[edge].mean()) if edge.any() else 0.0,
        "seam_px": int(seam.sum()),
        "seam_light": int((seam & (signed > 0)).sum()),
        "seam_dark": int((seam & (signed < 0)).sum()),
    }

    if base_path:
        base = load(base_path)
        changed = (base != test).any(axis=2)
        bdiff = base.astype(np.float32) - mean
        babs = np.abs(bdiff)
        bseam = calm & (babs.max(axis=2) >= SEAM)
        n = int(changed.sum())
        out.update(
            {
                "changed_px": n,
                "changed_err_before": float(babs[changed].mean()) if n else 0.0,
                "changed_err_after": float(absd[changed].mean()) if n else 0.0,
                "changed_bias_before": float(luma(bdiff)[changed].mean()) if n else 0.0,
                "changed_bias_after": float(signed[changed].mean()) if n else 0.0,
                "seam_px_before": int(bseam.sum()),
                "seam_new": int((seam & ~bseam).sum()),
                "seam_gone": int((bseam & ~seam).sum()),
            }
        )

    if mark:
        shown = test.copy()
        shown[seam] = (255, 0, 0)
        Image.fromarray(shown).save(mark)
    return out


def show(out):
    for key, value in out.items():
        if isinstance(value, float):
            print("%-20s %.2f" % (key, value))
        else:
            print("%-20s %s" % (key, value))


def render(stet, src, page, dpi, out, extra=()):
    cmd = [stet, "--device", "png", "--dpi", str(dpi), "--pages", str(page)]
    cmd += list(extra) + ["-o", out, src]
    subprocess.run(cmd, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    if not os.path.exists(out):
        sys.exit("no output from: " + " ".join(cmd))


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="mode", required=True)
    r = sub.add_parser("render")
    r.add_argument("file")
    r.add_argument("--page", type=int, default=1)
    r.add_argument("--dpi", type=int, default=150)
    r.add_argument("--stet", default="./target/release/stet")
    r.add_argument("--base-stet")
    c = sub.add_parser("compare")
    c.add_argument("test")
    c.add_argument("ref")
    c.add_argument("--base")
    for p in (r, c):
        p.add_argument("--factor", type=int, default=8)
        p.add_argument("--mark")
    args = ap.parse_args()

    if args.mode == "compare":
        show(compare(args.test, args.ref, args.factor, args.base, args.mark))
        return

    with tempfile.TemporaryDirectory() as tmp:
        test = os.path.join(tmp, "test.png")
        ref = os.path.join(tmp, "ref.png")
        base = None
        render(args.stet, args.file, args.page, args.dpi, test)
        # Anti-aliasing off: the reference then depends on no build's idea
        # of a smooth edge, the clip's included.
        render(
            args.base_stet or args.stet,
            args.file,
            args.page,
            args.dpi * args.factor,
            ref,
            ["--no-aa"],
        )
        if args.base_stet:
            base = os.path.join(tmp, "base.png")
            render(args.base_stet, args.file, args.page, args.dpi, base)
        show(compare(test, ref, args.factor, base, args.mark))


if __name__ == "__main__":
    main()
