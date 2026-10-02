#!/usr/bin/env python3
# stet - A PostScript Interpreter
# Copyright (c) 2026 Scott Bowman
# SPDX-License-Identifier: Apache-2.0 OR MIT

"""Generate the CMYK rendering-intent test profiles and lcms2's answers.

`cargo test` cannot call lcms2, the colour engine inside Ghostscript, so this
script runs it once and records what it produces. It writes, next to itself:

  split.icc      CMYK profile whose A2B0 (perceptual) and A2B1 (colorimetric)
                 tables differ; no A2B2.
  split_sat.icc  The same, plus an A2B2 (saturation) table unlike both.
  split_lut8.icc split.icc's tables as `lut8Type`, which stet's hand-rolled
                 sampler does not read, so stet bakes it through moxcms.
  same.icc       A2B0 a copy of A2B1, as in FOGRA39L and most profiles a
                 system installs: the intents should barely differ.
  inklimit.icc   split.icc with an ink-limited B2A0, as a real press profile
                 has: its relative colorimetric black point is not 400% ink.
  inklimit_lut8.icc, inklimit_scnr.icc, inklimit_v4.icc, inklimit_no_a2b0.icc
                 inklimit.icc as `lut8Type` (the shape of Adobe's profiles),
                 as an input-class profile, as ICC v4, and without A2B0: each
                 changes which black point lcms2 picks.
  split_xyz.icc  A CMYK profile with an XYZ PCS, A2B0 != A2B1, which stet's
                 hand-rolled evaluators do not read: the proofing chain's
                 moxcms fallback.
  shadow.icc, shadow_straight.icc, shadow_v4.icc
                 Output profiles whose B2A tables ease into an ink limit, so
                 lcms2's destination black-point detector fits a curve to
                 their shadows: one whose B2A1 bends the mid-tones, one whose
                 B2A1 does not, and the first as ICC v4.
  gray_trc.icc   A TRC-only Gray profile with an XYZ PCS and a black of
                 L* 20, like a press's black-ink profile.
  gray_gamma.icc A Gray profile whose tone curve is a pure gamma (1.8).
  rgb_gamma.icc  A matrix RGB profile (`mntr`, XYZ PCS) whose tone curves are
                 pure gammas (1.8), like Apple RGB or ColorMatch.
  rgb_lut.icc    An RGB profile with Lab `lut16Type` A2B1 and A2B2 tables that
                 are not affine, so tetrahedral and trilinear interpolation
                 differ, and no A2B0; it also carries rgb_gamma.icc's curves
                 and colorants, which lcms2 reads for perceptual.
  rgb_lut_v4.icc rgb_lut.icc's tables, plus A2B0, as ICC v4 without the
                 curves and colorants: lcms2 takes its perceptual and
                 saturation black point from the v4 perceptual black.
  srgb.icc       lcms2's built-in sRGB, an ICC v4 matrix-shaper with
                 parametric curves.
  gray_near_black.icc
                 A TRC-only Gray whose black is Y 0.0002: compensating it to
                 a black of zero is a change so small lcms2 drops it.
  mab.icc        An ICC v4 output profile whose tables are all `lutAToBType`
                 / `lutBToAType`, like Ghent's estprofile.icc: B2A CLUTs with a
                 different grid per input, 16- and 8-bit CLUTs, a matrix with
                 an offset, every kind of curve, an empty `curv` before
                 others in a set (which moxcms 0.8.1 misreads), and elements
                 left out.
  rgb_mab.icc    An ICC v4 RGB profile with `lutAToBType` A2B0 and A2B1.
  rgb_mixed.icc  An ICC v4 input-class RGB profile with an XYZ PCS and a
                 `lutAToBType` A2B0 with no CLUT: tone curves, a matrix that
                 mixes the channels, then curves flat near zero, as
                 `2142.pdf`'s second scanner profile has — the shape no
                 grid of RGB inputs samples well.
  reference.rs   lcms2's sRGB output for each profile, intent and BPC setting,
                 its black points as a source and as a destination, the
                 round trip behind them, and its proofing-chain stage 1
                 (CMYK, RGB, Gray or Lab source → output-intent CMYK) for each
                 intent, with and without black-point compensation, and its
                 tone-curve evaluation for each curve kind, and its
                 built-in sRGB output for each RGB profile with an A2B
                 table; included by `tests/cmyk_intent.rs` and
                 `tests/rgb_display.rs`.

The profiles are ICC v2 output (`prtr`) profiles with a Lab PCS unless named
otherwise. Most use `lut16Type` tables — the shape of FOGRA39, ISO Coated v2
or Japan Color 2001, and the shape stet's hand-rolled sampler reads. Each
A2B table is affine in CMYK on a two-point grid, so tetrahedral and
multilinear interpolation reproduce it exactly and the engines differ only
in what the test is about.

Two details steer what lcms2 does with these profiles:

- The destination is lcms2's built-in sRGB, an ICC v4 profile. lcms2 forces
  black-point compensation on for the perceptual and saturation intents when
  either profile is v4, so for those intents the BPC flag changes nothing.
- For relative colorimetric on a CMYK output profile lcms2 takes the source
  black point from a round trip of Lab L*=0 through `B2A0` then `A2B1`
  (`cmsDetectBlackPoint`, `BlackPointUsingPerceptualBlack`). In the `split`
  and `same` profiles `B2A0` maps L*=0 to 400% ink, so that black point is
  the darkest colorant's. The `inklimit` profiles' `B2A0` stops near 310%
  ink, so their black point is lighter; their `B2A1` still reaches 400%, so
  a round trip through the wrong table shows. lcms2 reads a Lab-indexed
  output table with trilinear interpolation, and the ink-limited table is
  not affine in a* and b*, so tetrahedral interpolation lands elsewhere
  (250% ink).

Run with Pillow (which bundles lcms2):  python3 generate.py
"""

import io
import math
import struct
import ctypes
import ctypes.util
from pathlib import Path

from PIL import Image, ImageCms
from PIL import __version__ as PILLOW_VERSION

HERE = Path(__file__).resolve().parent


# ------------------------------------------------------------------ profiles


def s15f16(v):
    return struct.pack(">i", round(v * 65536))


def u16(v):
    return struct.pack(">H", v)


def lut(bits, n_in, n_out, clut, grid=2, matrix=(1, 0, 0, 0, 1, 0, 0, 0, 1)):
    """An `mft2` (bits=16) or `mft1` (bits=8) tag on a `grid`-point grid with
    identity curves. `clut` maps grid coordinates (each in 0.0–1.0, first
    channel slowest) to normalised outputs. lcms2 applies `matrix`, row by
    row, to a three-input table's input before anything else."""
    t = (b"mft2" if bits == 16 else b"mft1") + bytes(4) + bytes([n_in, n_out, grid, 0])
    for v in matrix:
        t += s15f16(v)
    if bits == 16:
        t += u16(2) + u16(2)
        curves = lambda n: (u16(0) + u16(0xFFFF)) * n
        sample = lambda v: u16(round(v * 65535))
    else:
        # lut8Type curves have exactly 256 entries.
        curves = lambda n: bytes(range(256)) * n
        sample = lambda v: bytes([round(v * 255)])
    t += curves(n_in)
    for idx in range(grid**n_in):
        coords = [idx // grid ** (n_in - 1 - i) % grid / (grid - 1) for i in range(n_in)]
        out = clut(coords)
        assert len(out) == n_out
        for v in out:
            t += sample(min(max(v, 0.0), 1.0))
    t += curves(n_out)
    return t


def lab_table(bits, l, a, b, n_in=4, grid=2):
    """A2B clut: device (CMYK unless `n_in` says otherwise) → legacy v2 Lab.
    16-bit: L* 0..100 → 0..0xFF00, a*/b* → (v + 128) × 256. 8-bit: L*
    0..100 → 0..255, a*/b* → v + 128."""

    def clut(device):
        L = l(*device)
        A = a(*device)
        B = b(*device)
        assert 0 <= L <= 100 and -128 <= A < 128 and -128 <= B < 128
        if bits == 16:
            return [L * 652.8 / 65535, (A + 128) * 256 / 65535, (B + 128) * 256 / 65535]
        return [L / 100, (A + 128) / 255, (B + 128) / 255]

    return lut(bits, n_in, 3, clut, grid)


# A2B1, colorimetric: paper white L*=100, 400% black L*=8.
A2B1 = lambda bits: lab_table(
    bits,
    lambda c, m, y, k: 100 - 18 * c - 14 * m - 6 * y - 54 * k,
    lambda c, m, y, k: -32 * c + 62 * m - 6 * y,
    lambda c, m, y, k: -42 * c - 6 * m + 72 * y,
)
# A2B0, perceptual: a lighter black (L*=22) and less chroma, as a perceptual
# table compressing into a smaller gamut would have.
A2B0 = lambda bits: lab_table(
    bits,
    lambda c, m, y, k: 100 - 15 * c - 11 * m - 4 * y - 48 * k,
    lambda c, m, y, k: -26 * c + 52 * m - 4 * y,
    lambda c, m, y, k: -34 * c - 4 * m + 60 * y,
)
# A2B2, saturation: more chroma than either, black L*=7.
A2B2 = lambda bits: lab_table(
    bits,
    lambda c, m, y, k: 100 - 20 * c - 16 * m - 5 * y - 52 * k,
    lambda c, m, y, k: -45 * c + 75 * m - 8 * y,
    lambda c, m, y, k: -55 * c - 8 * m + 85 * y,
)


# B2A: Lab → equal ink, L*=0 → 400%. Only lcms2's black-point detection
# reads it.
def b2a(coords):
    ink = 1.0 - coords[0]
    return [ink, ink, ink, ink]


B2A = lambda bits: lut(bits, 3, 4, b2a)


# Ink-limited B2A: K follows L* as above, but C, M and Y stop at 50% where the
# a* and b* grid coordinates agree and 90% where they differ. At the neutral
# axis (a* = b* = 0, the middle of the grid) that is 70% each, ~310% in all,
# by trilinear interpolation; tetrahedral would give 50% each.
def inklimited_b2a(coords):
    l, a, b = coords
    shadow = 1.0 - l
    cmy = shadow * (0.5 if a == b else 0.9)
    return [cmy, cmy, cmy, shadow]


INKLIMITED_B2A = lambda bits: lut(bits, 3, 4, inklimited_b2a)


def xyz_tag(x, y, z):
    return b"XYZ " + bytes(4) + s15f16(x) + s15f16(y) + s15f16(z)


def desc_tag(text):
    ascii_ = text.encode("ascii") + b"\0"
    return (
        b"desc"
        + bytes(4)
        + struct.pack(">I", len(ascii_))
        + ascii_
        + bytes(4 + 4)  # Unicode: language code, count 0
        + bytes(2 + 1 + 67)  # ScriptCode: code, count 0, 67 reserved bytes
    )


def profile(
    description,
    with_saturation,
    bits=16,
    perceptual=A2B0,
    perceptual_b2a=B2A,
    device_class=b"prtr",
    version=0x02100000,
    with_b2a=True,
):
    tags = [
        (b"desc", desc_tag(description)),
        (b"wtpt", xyz_tag(0.9642, 1.0, 0.8249)),
    ]
    if perceptual is not None:
        tags.append((b"A2B0", perceptual(bits)))
    tags.append((b"A2B1", A2B1(bits)))
    if with_b2a:
        tags += [(b"B2A0", perceptual_b2a(bits)), (b"B2A1", B2A(bits))]
    if with_saturation:
        tags.append((b"A2B2", A2B2(bits)))
        if with_b2a:
            tags.append((b"B2A2", B2A(bits)))
    return assemble(tags, device_class, b"CMYK", b"Lab ", version)


def assemble(tags, device_class, colour_space, pcs, version):
    table_len = 4 + 12 * len(tags)
    table = struct.pack(">I", len(tags))
    data = b""
    for sig, body in tags:
        table += sig + struct.pack(">II", 128 + table_len + len(data), len(body))
        data += body
        data += bytes(-len(data) % 4)

    total = 128 + len(table) + len(data)
    header = bytearray(128)
    header[0:4] = struct.pack(">I", total)
    header[8:12] = struct.pack(">I", version)
    header[12:16] = device_class
    header[16:20] = colour_space
    header[20:24] = pcs
    header[36:40] = b"acsp"
    header[68:80] = xyz_tag(0.9642, 1.0, 0.8249)[8:]
    return bytes(header) + table + data


def xyz_table(x, y, z):
    """A2B clut: CMYK → XYZ, each affine in CMYK so every interpolation
    reproduces it. `lut16Type` XYZ is u1Fixed15: 1.0 at 0x8000."""

    def clut(cmyk):
        out = [x(*cmyk), y(*cmyk), z(*cmyk)]
        assert all(0 < v <= 1 for v in out)
        return [v * 32768 / 65535 for v in out]

    return lut(16, 4, 3, clut)


# Cyan absorbs mostly X, magenta Y, yellow Z; 400% ink keeps 5% of each.
XYZ_A2B1 = xyz_table(
    lambda c, m, y, k: 0.9642 * (1 - 0.36 * c - 0.16 * m - 0.03 * y - 0.40 * k),
    lambda c, m, y, k: 1 - 0.14 * c - 0.38 * m - 0.06 * y - 0.37 * k,
    lambda c, m, y, k: 0.8249 * (1 - 0.10 * c - 0.22 * m - 0.45 * y - 0.18 * k),
)
# A2B0: a lighter black and less contrast.
XYZ_A2B0 = xyz_table(
    lambda c, m, y, k: 0.9642 * (1 - 0.30 * c - 0.13 * m - 0.03 * y - 0.34 * k),
    lambda c, m, y, k: 1 - 0.12 * c - 0.32 * m - 0.05 * y - 0.32 * k,
    lambda c, m, y, k: 0.8249 * (1 - 0.08 * c - 0.18 * m - 0.40 * y - 0.15 * k),
)


def xyz_profile(description):
    """A source-only CMYK profile with an XYZ PCS: A2B0 and A2B1."""
    tags = [
        (b"desc", desc_tag(description)),
        (b"wtpt", xyz_tag(0.9642, 1.0, 0.8249)),
        (b"A2B0", XYZ_A2B0),
        (b"A2B1", XYZ_A2B1),
    ]
    return assemble(tags, b"prtr", b"CMYK", b"XYZ ", 0x02100000)


# Output profiles whose shadows level off as a press profile's do: each
# B2A table follows the equal ink A2B1 gives an L* until it eases into an
# ink limit (a soft minimum), so lcms2's destination black-point detector
# finds a flat black and a bend, and fits a quadratic to it. In `shadow.icc`
# B2A1 also bends the mid-tones (ink^0.85), which sends relative
# colorimetric to the fit too; `shadow_straight.icc` keeps B2A1 straight,
# where relative colorimetric takes its own black point instead. The tables
# only depend on L*, on a 17-point grid: on a 2-point grid every round trip
# is straight, and lcms2 then detects a black point of zero.
def eased_b2a(limit, gamma):
    softness = 0.03

    def clut(coords):
        l_star = coords[0] * 65535 / 65280 * 100
        ink = max((100 - l_star) / 92, 0.0) ** gamma
        ink = limit - softness * math.log1p(math.exp((limit - ink) / softness))
        return [ink, ink, ink, ink]

    return lut(16, 3, 4, clut, grid=17)


def shadow_profile(description, version=0x02100000, relcol_gamma=0.85):
    tags = [
        (b"desc", desc_tag(description)),
        (b"wtpt", xyz_tag(0.9642, 1.0, 0.8249)),
        (b"A2B0", A2B0(16)),
        (b"A2B1", A2B1(16)),
        (b"A2B2", A2B2(16)),
        (b"B2A0", eased_b2a(0.8, 1.0)),
        (b"B2A1", eased_b2a(0.85, relcol_gamma)),
        (b"B2A2", eased_b2a(0.9, 1.0)),
    ]
    return assemble(tags, b"prtr", b"CMYK", b"Lab ", version)


def curv_tag(values):
    return b"curv" + bytes(4) + struct.pack(">I", len(values)) + b"".join(
        u16(round(v * 65535)) for v in values
    )


def gray_profile(description):
    """A TRC-only Gray profile with an XYZ PCS, like a press's black-ink
    profile: gray 0 is the ink's solid, Y 0.03 (L* 20), not black."""
    trc = [0.03 + 0.97 * (i / 255) ** 1.8 for i in range(256)]
    tags = [
        (b"desc", desc_tag(description)),
        (b"wtpt", xyz_tag(0.9642, 1.0, 0.8249)),
        (b"kTRC", curv_tag(trc)),
    ]
    return assemble(tags, b"prtr", b"GRAY", b"XYZ ", 0x02100000)


def gamma_curv_tag(gamma):
    """A `curv` with one entry: a pure gamma, u8Fixed8."""
    return b"curv" + bytes(4) + struct.pack(">I", 1) + u16(round(gamma * 256))


def gray_gamma_profile(description):
    tags = [
        (b"desc", desc_tag(description)),
        (b"wtpt", xyz_tag(0.9642, 1.0, 0.8249)),
        (b"kTRC", gamma_curv_tag(1.8)),
    ]
    return assemble(tags, b"mntr", b"GRAY", b"XYZ ", 0x02100000)


# Colorants of a wide-gamut RGB, D50-adapted, each column summing to the
# white: ColorMatch RGB's.
RGB_COLORANTS = [
    (b"rXYZ", (0.5094, 0.2749, 0.0243)),
    (b"gXYZ", (0.3208, 0.6581, 0.1087)),
    (b"bXYZ", (0.1339, 0.0670, 0.6919)),
]


def matrix_tags(gamma):
    tags = [(sig, xyz_tag(*xyz)) for sig, xyz in RGB_COLORANTS]
    tags += [(sig, gamma_curv_tag(gamma)) for sig in (b"rTRC", b"gTRC", b"bTRC")]
    return tags


def rgb_gamma_profile(description):
    tags = [
        (b"desc", desc_tag(description)),
        (b"wtpt", xyz_tag(0.9642, 1.0, 0.8249)),
        *matrix_tags(1.8),
    ]
    return assemble(tags, b"mntr", b"RGB ", b"XYZ ", 0x02100000)


def rgb_lut_profile(description, v4=False):
    """A2B1 and A2B2 on a 5-point grid, curved in every input so tetrahedral
    and trilinear interpolation land apart; no A2B0. As ICC v4: A2B0 a copy
    of A2B1, and no curves or colorants."""
    a2b1 = lab_table(
        16,
        lambda r, g, b: 100 * (0.25 * r + 0.62 * g + 0.13 * b) ** 0.6,
        lambda r, g, b: 90 * (r * r - g) * (1 - 0.4 * b),
        lambda r, g, b: 80 * (0.7 * r * g - b * b) + 10 * r,
        n_in=3,
        grid=5,
    )
    a2b2 = lab_table(
        16,
        lambda r, g, b: 100 * (0.3 * r + 0.55 * g + 0.15 * b) ** 0.7,
        lambda r, g, b: 110 * (r - g * g) * (1 - 0.3 * b * r),
        lambda r, g, b: 95 * (r * g - b) * (1 - 0.2 * r),
        n_in=3,
        grid=5,
    )
    tags = [
        (b"desc", desc_tag(description)),
        (b"wtpt", xyz_tag(0.9642, 1.0, 0.8249)),
    ]
    if v4:
        tags.append((b"A2B0", a2b1))
    else:
        tags += matrix_tags(1.8)
    tags += [(b"A2B1", a2b1), (b"A2B2", a2b2)]
    return assemble(tags, b"mntr", b"RGB ", b"Lab ", 0x04200000 if v4 else 0x02100000)


def gray_near_black_profile(description):
    trc = [0.0002 + 0.9998 * (i / 255) ** 2.2 for i in range(256)]
    tags = [
        (b"desc", desc_tag(description)),
        (b"wtpt", xyz_tag(0.9642, 1.0, 0.8249)),
        (b"kTRC", curv_tag(trc)),
    ]
    return assemble(tags, b"mntr", b"GRAY", b"XYZ ", 0x02100000)


def para_tag(function, params):
    """A `para` curve of ICC function type `function` (0–4)."""
    return (
        b"para"
        + bytes(4)
        + struct.pack(">HH", function, 0)
        + b"".join(s15f16(p) for p in params)
    )


def padded(body):
    return body + bytes(-len(body) % 4)


def multi_tag(to_pcs, n_in, n_out, a=None, clut=None, m=None, matrix=None, b=None):
    """A `lutAToBType` (`to_pcs`) or `lutBToAType` tag. `a`, `m` and `b` are
    lists of curve tags; `clut` is (grid points per input, bytes per entry,
    function of the grid coordinates → normalised outputs); `matrix` is
    (3×3 rows, offset). `None` leaves an element out."""
    header = 32
    parts = []
    offsets = {}

    def place(name, body):
        offsets[name] = header + sum(len(p) for p in parts)
        parts.append(padded(body))

    def curves(set_):
        return b"".join(padded(c) for c in set_)

    order = ["a", "clut", "m", "matrix", "b"] if to_pcs else ["b", "matrix", "m", "clut", "a"]
    for name in order:
        if name == "a" and a is not None:
            place("a", curves(a))
        elif name == "m" and m is not None:
            place("m", curves(m))
        elif name == "b" and b is not None:
            place("b", curves(b))
        elif name == "matrix" and matrix is not None:
            rows, offset = matrix
            place("matrix", b"".join(s15f16(v) for row in rows for v in row) + b"".join(s15f16(v) for v in offset))
        elif name == "clut" and clut is not None:
            grids, precision, fn = clut
            body = bytes(grids) + bytes(16 - len(grids)) + bytes([precision]) + bytes(3)
            total = 1
            for g in grids:
                total *= g
            for idx in range(total):
                coords = []
                rest = idx
                for g in reversed(grids):
                    coords.append(rest % g / (g - 1))
                    rest //= g
                out = fn(list(reversed(coords)))
                for v in out:
                    v = min(max(v, 0.0), 1.0)
                    body += u16(round(v * 65535)) if precision == 2 else bytes([round(v * 255)])
            place("clut", body)
    sig = b"mAB " if to_pcs else b"mBA "
    head = sig + bytes(4) + bytes([n_in, n_out, 0, 0])
    head += struct.pack(
        ">IIIII",
        offsets.get("b", 0),
        offsets.get("matrix", 0),
        offsets.get("m", 0),
        offsets.get("clut", 0),
        offsets.get("a", 0),
    )
    return head + b"".join(parts)


def v4_lab(l, a, b):
    """Lab → the v4 encoding, each 0–1."""
    return [l / 100, (a + 128) / 255, (b + 128) / 255]


MAB_A_CURVES = [
    curv_tag([(i / 32) ** 1.15 for i in range(33)]),
    para_tag(0, [0.9]),
    para_tag(3, [2.4, 1 / 1.055, 0.055 / 1.055, 1 / 12.92, 0.04045]),
    curv_tag([]),
]


def mab_a2b(precision, chroma, grids=(4, 4, 4, 4), a=MAB_A_CURVES):
    """CMYK → Lab: the A2B1 tables' Lab, curved in every input and bent in
    C·M so tetrahedral interpolation matters, on a grid of `grids`. (moxcms
    0.8.1 refuses a four-input grid that differs per input; `mab.icc` keeps
    an even one so stage 1 can chain into it, and `mab_grid.icc` has the
    other.)"""

    def fn(cmyk):
        c, m, y, k = cmyk
        return v4_lab(
            100 - 18 * c - 14 * m - 6 * y - 54 * k + 6 * c * m,
            chroma * (-32 * c + 62 * m - 6 * y),
            chroma * (-42 * c - 6 * m + 72 * y),
        )

    return multi_tag(
        True,
        4,
        3,
        a=a,
        clut=(list(grids), precision, fn),
        m=[para_tag(0, [1.1]), gamma_curv_tag(0.95), curv_tag([])],
        matrix=(
            [[0.95, 0.02, 0.01], [0.01, 0.97, 0.0], [0.0, 0.02, 0.96]],
            [0.02, 0.005, 0.01],
        ),
        b=[curv_tag([0, 0.5, 1]), para_tag(0, [1.0]), curv_tag([])],
    )


def mab_b2a(limit, gamma, precision, with_m=True):
    """Lab → CMYK on a 17×5×5 grid: equal ink easing into `limit` from L*,
    the CMY shifted by a* and b* in proportion to the ink, so the grid is
    not separable and trilinear and tetrahedral interpolation differ."""
    softness = 0.03

    def fn(lab):
        l_star = lab[0] * 100
        a_star = lab[1] * 255 - 128
        b_star = lab[2] * 255 - 128
        ink = max((100 - l_star) / 92, 0.0) ** gamma
        ink = limit - softness * math.log1p(math.exp((limit - ink) / softness))
        return [
            ink - 0.003 * a_star * ink,
            ink + 0.004 * a_star * ink - 0.002 * b_star * ink,
            ink + 0.005 * b_star * ink,
            ink,
        ]

    return multi_tag(
        False,
        3,
        4,
        b=[curv_tag([]), curv_tag([]), curv_tag([])],
        m=[para_tag(0, [1.0]), curv_tag([0, 0.5, 1]), curv_tag([])] if with_m else None,
        clut=([17, 5, 5], precision, fn),
        a=[
            curv_tag([(i / 32) ** 0.95 for i in range(33)]),
            curv_tag([]),
            para_tag(0, [1.05]),
            curv_tag([]),
        ],
    )


def mab_profile(description):
    tags = [
        (b"desc", desc_tag(description)),
        (b"wtpt", xyz_tag(0.9642, 1.0, 0.8249)),
        (b"A2B0", mab_a2b(2, 0.8)),
        (b"A2B1", mab_a2b(2, 1.0)),
        (b"A2B2", mab_a2b(1, 1.15)),
        (b"B2A0", mab_b2a(0.8, 1.0, 2)),
        (b"B2A1", mab_b2a(0.85, 0.85, 2)),
        (b"B2A2", mab_b2a(0.9, 1.0, 1, with_m=False)),
    ]
    return assemble(tags, b"prtr", b"CMYK", b"Lab ", 0x04200000)


# ------------------------------------------------------- XYZ PCS

D50 = (0.9642, 1.0, 0.8249)
# lcms2's MAX_ENCODEABLE_XYZ: a table's XYZ PCS value 1 is this much XYZ.
XYZ_MAX = 1 + 32767 / 32768


def lab_to_xyz(lab):
    l, a, b = lab
    fy = (l + 16) / 116
    fx, fz = fy + a / 500, fy - b / 200

    def f_inv(t):
        return t**3 if t > 6 / 29 else 3 * (6 / 29) ** 2 * (t - 4 / 29)

    return [w * f_inv(t) for w, t in zip(D50, (fx, fy, fz))]


def xyz_to_lab(xyz):
    def f(t):
        return t ** (1 / 3) if t > (6 / 29) ** 3 else t / (3 * (6 / 29) ** 2) + 4 / 29

    fx, fy, fz = (f(v / w) for v, w in zip(xyz, D50))
    return [116 * fy - 16, 500 * (fx - fy), 200 * (fy - fz)]


def xyz_a2b(lab_of):
    """CMYK → XYZ PCS on a 5⁴ grid, from a CMYK → Lab function."""
    return lambda coords: [v / XYZ_MAX for v in lab_to_xyz(lab_of(*coords))]


# CMYK → Lab bent in C·M, with 400% ink at L* `black`; perceptual has a
# lighter black (L* 22) and less chroma.
def xyz_lab(black, chroma):
    return lambda c, m, y, k: (
        100 - 18 * c - 14 * m - 6 * y - (68 - black) * k + 6 * c * m,
        chroma * (-32 * c + 62 * m - 6 * y),
        chroma * (-42 * c - 6 * m + 72 * y),
    )


def xyz_ink(limit, gamma):
    """White-relative XYZ grid coordinates → ink: equal ink easing into
    `limit` from L*, CMY shifted by a* and b* in proportion to the ink."""
    softness = 0.03

    def fn(coords):
        l_star, a_star, b_star = xyz_to_lab([c * w for c, w in zip(coords, D50)])
        ink = max((100 - l_star) / 92, 0.0) ** gamma
        ink = limit - softness * math.log1p(math.exp((limit - ink) / softness))
        return [
            ink - 0.003 * a_star * ink,
            ink + 0.004 * a_star * ink - 0.002 * b_star * ink,
            ink + 0.005 * b_star * ink,
            ink,
        ]

    return fn


# A B2A table's matrix as Ghostscript's `ps_cmyk.icc` has it: the PCS value
# → XYZ relative to white, so the grid spans 0–1 on every axis.
XYZ_B2A_MATRIX = (XYZ_MAX / D50[0], 0, 0, 0, XYZ_MAX / D50[1], 0, 0, 0, XYZ_MAX / D50[2])


def xyz_v4_profile(description):
    """An output-class ICC v4 CMYK profile with an XYZ PCS and `lut16Type`
    tables, as Ghostscript's `ps_cmyk.icc` is, ink-limited in `B2A0` so the
    black-point round trip matters."""
    tags = [
        (b"desc", desc_tag(description)),
        (b"wtpt", xyz_tag(*D50)),
        (b"A2B0", lut(16, 4, 3, xyz_a2b(xyz_lab(22, 0.8)), grid=5)),
        (b"A2B1", lut(16, 4, 3, xyz_a2b(xyz_lab(8, 1.0)), grid=5)),
        (b"B2A0", lut(16, 3, 4, xyz_ink(0.8, 1.0), grid=9, matrix=XYZ_B2A_MATRIX)),
        (b"B2A1", lut(16, 3, 4, xyz_ink(0.9, 0.85), grid=9, matrix=XYZ_B2A_MATRIX)),
    ]
    return assemble(tags, b"prtr", b"CMYK", b"XYZ ", 0x04200000)


def xyz_mab_profile(description):
    """`xyz_v4.icc` as ICC v4 `lutAToBType` / `lutBToAType` tables, the
    B2A matrix in the `mBA` matrix element."""

    def a2b(black, chroma):
        fn = xyz_a2b(xyz_lab(black, chroma))
        return multi_tag(
            True,
            4,
            3,
            a=MAB_A_CURVES,
            clut=([4, 4, 4, 4], 2, fn),
            m=[para_tag(0, [1.1]), gamma_curv_tag(0.95), curv_tag([])],
            matrix=([[0.97, 0.02, 0.0], [0.01, 0.98, 0.0], [0.0, 0.01, 0.97]], [0.005, 0.0, 0.01]),
            b=[curv_tag([]), curv_tag([]), curv_tag([])],
        )

    def b2a(limit, gamma):
        m = XYZ_B2A_MATRIX
        return multi_tag(
            False,
            3,
            4,
            b=[curv_tag([]), curv_tag([]), curv_tag([])],
            matrix=([m[0:3], m[3:6], m[6:9]], [0.0, 0.0, 0.0]),
            m=[para_tag(0, [1.0]), curv_tag([0, 0.5, 1]), curv_tag([])],
            clut=([9, 7, 5], 2, xyz_ink(limit, gamma)),
            a=[curv_tag([]), para_tag(0, [1.05]), curv_tag([]), curv_tag([])],
        )

    tags = [
        (b"desc", desc_tag(description)),
        (b"wtpt", xyz_tag(*D50)),
        (b"A2B0", a2b(22, 0.8)),
        (b"A2B1", a2b(8, 1.0)),
        (b"B2A0", b2a(0.8, 1.0)),
        (b"B2A1", b2a(0.9, 0.85)),
    ]
    return assemble(tags, b"prtr", b"CMYK", b"XYZ ", 0x04200000)


def rgb_xyz_lab(scale):
    """The RGB → Lab of `rgb_lut.icc`'s A2B1, black lifted to L* 4 so the
    black point is not zero, chroma scaled by `scale`."""
    return lambda r, g, b: (
        4 + 96 * (0.25 * r + 0.62 * g + 0.13 * b) ** 0.6,
        scale * 90 * (r * r - g) * (1 - 0.4 * b),
        scale * (80 * (0.7 * r * g - b * b) + 10 * r),
    )


def rgb_xyz_profile(description):
    """An input-class RGB profile with an XYZ PCS and `lut16Type` A2B0 and
    A2B1 only, as camera profiles are."""
    tags = [
        (b"desc", desc_tag(description)),
        (b"wtpt", xyz_tag(*D50)),
        (b"A2B0", lut(16, 3, 3, xyz_a2b(rgb_xyz_lab(0.85)), grid=5)),
        (b"A2B1", lut(16, 3, 3, xyz_a2b(rgb_xyz_lab(1.0)), grid=5)),
    ]
    return assemble(tags, b"scnr", b"RGB ", b"XYZ ", 0x02100000)


def rgb_xyz_mab_profile(description):
    """An input-class ICC v4 RGB profile with an XYZ PCS and a
    `lutAToBType` A2B0 only, as `2142.pdf`'s scanner profiles are."""
    a2b0 = multi_tag(
        True,
        3,
        3,
        a=[para_tag(0, [2.2]), para_tag(0, [1.8]), curv_tag([(i / 32) ** 2 for i in range(33)])],
        clut=([5, 6, 7], 2, xyz_a2b(rgb_xyz_lab(0.85))),
        b=[curv_tag([]), curv_tag([]), curv_tag([])],
    )
    tags = [
        (b"desc", desc_tag(description)),
        (b"wtpt", xyz_tag(*D50)),
        (b"A2B0", a2b0),
    ]
    return assemble(tags, b"scnr", b"RGB ", b"XYZ ", 0x04200000)


def mab_grid_profile(description):
    """`mab.icc` with A2B tables on a 3×4×5×3 grid whose A curves hold an
    empty `curv` before two more: the two shapes moxcms 0.8.1 misreads."""
    a = [MAB_A_CURVES[0], curv_tag([]), MAB_A_CURVES[2], MAB_A_CURVES[1]]
    grids = (3, 4, 5, 3)
    tags = [
        (b"desc", desc_tag(description)),
        (b"wtpt", xyz_tag(0.9642, 1.0, 0.8249)),
        (b"A2B0", mab_a2b(2, 0.8, grids, a)),
        (b"A2B1", mab_a2b(2, 1.0, grids, a)),
        (b"A2B2", mab_a2b(1, 1.15, grids, a)),
        (b"B2A0", mab_b2a(0.8, 1.0, 2)),
        (b"B2A1", mab_b2a(0.85, 0.85, 2)),
        (b"B2A2", mab_b2a(0.9, 1.0, 1, with_m=False)),
    ]
    return assemble(tags, b"prtr", b"CMYK", b"Lab ", 0x04200000)


def rgb_mab_profile(description):
    def a2b(scale):
        def fn(rgb):
            r, g, b = rgb
            return v4_lab(
                100 * (0.25 * r + 0.62 * g + 0.13 * b) ** 0.6,
                scale * 90 * (r * r - g) * (1 - 0.4 * b),
                scale * (80 * (0.7 * r * g - b * b) + 10 * r),
            )

        return multi_tag(
            True,
            3,
            3,
            a=[para_tag(0, [2.2]), para_tag(0, [1.8]), curv_tag([(i / 32) ** 2 for i in range(33)])],
            clut=([5, 6, 7], 2, fn),
            b=[curv_tag([]), curv_tag([]), curv_tag([])],
        )

    tags = [
        (b"desc", desc_tag(description)),
        (b"wtpt", xyz_tag(0.9642, 1.0, 0.8249)),
        (b"A2B0", a2b(0.85)),
        (b"A2B1", a2b(1.0)),
    ]
    return assemble(tags, b"mntr", b"RGB ", b"Lab ", 0x04200000)


def rgb_mixed_profile(description):
    """An input-class ICC v4 RGB profile, XYZ PCS, whose `lutAToBType` A2B0
    has no CLUT: M curves, a matrix with an offset that mixes the channels,
    and B curves flat below 0.012 then steep."""
    # sRGB's colorants adapted to D50, in the XYZ encoding (1.0 ↦ 0x8000).
    rows = [
        [0.4360747, 0.3850649, 0.1430804],
        [0.2225045, 0.7168786, 0.0606169],
        [0.0139322, 0.0971045, 0.7141733],
    ]
    rows = [[v * 32768 / 65535 for v in row] for row in rows]

    def flat_then_steep(x):
        return 0.0 if x < 0.012 else min(1.0, 0.5 * ((x - 0.012) / 0.488) ** 1.1)

    b = curv_tag([flat_then_steep(i / 511) for i in range(512)])
    a2b0 = multi_tag(
        True,
        3,
        3,
        m=[
            para_tag(0, [2.2]),
            curv_tag([(i / 63) ** 1.8 for i in range(64)]),
            para_tag(3, [2.4, 1 / 1.055, 0.055 / 1.055, 1 / 12.92, 0.04045]),
        ],
        matrix=(rows, [0.006, 0.006, 0.006]),
        b=[b, b, b],
    )
    tags = [
        (b"desc", desc_tag(description)),
        (b"wtpt", xyz_tag(*D50)),
        (b"A2B0", a2b0),
    ]
    return assemble(tags, b"scnr", b"RGB ", b"XYZ ", 0x04200000)


# ------------------------------------------------------------------ lcms2

# CMYK samples, 0–255: paper, primaries, secondaries, black ramps, rich and
# total-ink black, and midtones. Avoid colours just outside sRGB where a
# channel climbs steeply off zero, such as (204, 51, 0, 26) on `A2B2`:
# there stet's 17⁴ table, interpolated in gamma-encoded RGB, misses lcms2
# by ~6 levels whatever the intent, and the test is about intents.
SAMPLES = [
    (0, 0, 0, 0),
    (255, 0, 0, 0),
    (0, 255, 0, 0),
    (0, 0, 255, 0),
    (0, 0, 0, 255),
    (0, 0, 0, 128),
    (0, 0, 0, 64),
    (255, 255, 0, 0),
    (0, 255, 255, 0),
    (255, 0, 255, 0),
    (153, 102, 102, 255),
    (255, 255, 255, 255),
    (77, 153, 26, 51),
    (38, 255, 255, 0),
    (128, 128, 128, 0),
    (102, 26, 0, 13),
]

# Index order of the generated tables; the test maps them to stet's intents.
INTENTS = [
    ("PERCEPTUAL", ImageCms.Intent.PERCEPTUAL),
    ("RELATIVE_COLORIMETRIC", ImageCms.Intent.RELATIVE_COLORIMETRIC),
    ("SATURATION", ImageCms.Intent.SATURATION),
]


def lcms_rgb(icc, intent, bpc):
    src = ImageCms.getOpenProfile(io.BytesIO(icc))
    dst = ImageCms.createProfile("sRGB")
    flags = ImageCms.Flags.NOOPTIMIZE
    if bpc:
        flags |= ImageCms.Flags.BLACKPOINTCOMPENSATION
    xform = ImageCms.buildTransform(src, dst, "CMYK", "RGB", intent, flags)
    img = Image.new("CMYK", (len(SAMPLES), 1))
    img.putdata(SAMPLES)
    out = ImageCms.applyTransform(img, xform)
    return [out.getpixel((i, 0)) for i in range(len(SAMPLES))]


# lcms2 itself, for what Pillow does not expose: the black point it detects
# and the round trip that finds it. Pillow links the same library.

LCMS = ctypes.CDLL(ctypes.util.find_library("lcms2") or "liblcms2.so.2")
LCMS.cmsOpenProfileFromMem.restype = ctypes.c_void_p
LCMS.cmsOpenProfileFromMem.argtypes = [ctypes.c_char_p, ctypes.c_uint32]
LCMS.cmsCreateLab4Profile.restype = ctypes.c_void_p
LCMS.cmsCreateLab4Profile.argtypes = [ctypes.c_void_p]
LCMS.cmsCloseProfile.argtypes = [ctypes.c_void_p]
LCMS.cmsCreateExtendedTransform.restype = ctypes.c_void_p
LCMS.cmsCreateExtendedTransform.argtypes = [
    ctypes.c_void_p,
    ctypes.c_uint32,
    ctypes.POINTER(ctypes.c_void_p),
    ctypes.POINTER(ctypes.c_int),
    ctypes.POINTER(ctypes.c_uint32),
    ctypes.POINTER(ctypes.c_double),
    ctypes.c_void_p,
    ctypes.c_uint32,
    ctypes.c_uint32,
    ctypes.c_uint32,
    ctypes.c_uint32,
]
LCMS.cmsDoTransform.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_uint32]
LCMS.cmsCreate_sRGBProfile.restype = ctypes.c_void_p
LCMS.cmsSaveProfileToMem.restype = ctypes.c_int
LCMS.cmsSaveProfileToMem.argtypes = [
    ctypes.c_void_p,
    ctypes.c_void_p,
    ctypes.POINTER(ctypes.c_uint32),
]
LCMS.cmsDeleteTransform.argtypes = [ctypes.c_void_p]
LCMS.cmsDetectBlackPoint.restype = ctypes.c_int
LCMS.cmsDetectBlackPoint.argtypes = [
    ctypes.POINTER(ctypes.c_double * 3),
    ctypes.c_void_p,
    ctypes.c_uint32,
    ctypes.c_uint32,
]

LCMS.cmsDetectDestinationBlackPoint.restype = ctypes.c_int
LCMS.cmsDetectDestinationBlackPoint.argtypes = [
    ctypes.POINTER(ctypes.c_double * 3),
    ctypes.c_void_p,
    ctypes.c_uint32,
    ctypes.c_uint32,
]

# lcms2.h: FLOAT_SH(1) | COLORSPACE_SH(PT_…) | CHANNELS_SH(n) | BYTES_SH(0).
TYPE_LAB_DBL = (1 << 22) | (10 << 16) | (3 << 3)
TYPE_CMYK_DBL = (1 << 22) | (6 << 16) | (4 << 3)
TYPE_GRAY_DBL = (1 << 22) | (3 << 16) | (1 << 3)
TYPE_RGB_DBL = (1 << 22) | (4 << 16) | (3 << 3)
FLAGS_NOCACHE_NOOPTIMIZE = 0x0040 | 0x0100
PERCEPTUAL, RELATIVE_COLORIMETRIC, SATURATION = 0, 1, 2


def lcms_chain(profiles, intents, in_format, out_format, inputs, n_out):
    """Transform `inputs` through `profiles` (each an lcms2 handle) as
    `cmsCreateExtendedTransform` does for lcms2's own black-point round
    trip: no BPC, full adaptation, unoptimised, in doubles."""
    n = len(profiles)
    xform = LCMS.cmsCreateExtendedTransform(
        None,
        n,
        (ctypes.c_void_p * n)(*profiles),
        (ctypes.c_int * n)(*([0] * n)),
        (ctypes.c_uint32 * n)(*intents),
        (ctypes.c_double * n)(*([1.0] * n)),
        None,
        0,
        in_format,
        out_format,
        FLAGS_NOCACHE_NOOPTIMIZE,
    )
    assert xform, "lcms2 could not build the transform"
    out = []
    for v in inputs:
        src = (ctypes.c_double * len(v))(*v)
        dst = (ctypes.c_double * n_out)()
        LCMS.cmsDoTransform(xform, src, dst, 1)
        out.append(list(dst))
    LCMS.cmsDeleteTransform(xform)
    return out


class Lcms:
    """An lcms2 handle on a profile, with a v4 Lab profile beside it."""

    def __init__(self, icc):
        self.icc = icc  # keeps the bytes alive while the handle is open
        self.profile = LCMS.cmsOpenProfileFromMem(icc, len(icc))
        self.lab = LCMS.cmsCreateLab4Profile(None)
        assert self.profile and self.lab

    def close(self):
        LCMS.cmsCloseProfile(self.profile)
        LCMS.cmsCloseProfile(self.lab)

    def black_point(self, intent):
        """`cmsDetectBlackPoint`, XYZ with white Y = 1."""
        xyz = (ctypes.c_double * 3)()
        LCMS.cmsDetectBlackPoint(ctypes.byref(xyz), self.profile, intent, 0)
        return list(xyz)

    def destination_black_point(self, intent):
        """`cmsDetectDestinationBlackPoint`, XYZ with white Y = 1."""
        xyz = (ctypes.c_double * 3)()
        LCMS.cmsDetectDestinationBlackPoint(ctypes.byref(xyz), self.profile, intent, 0)
        return list(xyz)

    def b2a0(self, labs):
        """The round trip's first leg, Lab → CMYK through `B2A0`; ink 0–1."""
        out = lcms_chain(
            [self.lab, self.profile],
            [RELATIVE_COLORIMETRIC, PERCEPTUAL],
            TYPE_LAB_DBL,
            TYPE_CMYK_DBL,
            labs,
            4,
        )
        return [[v / 100 for v in cmyk] for cmyk in out]

    def a2b1(self, cmyks):
        """The round trip's second leg, CMYK (ink 0–1) → Lab through `A2B1`."""
        return lcms_chain(
            [self.profile, self.lab],
            [RELATIVE_COLORIMETRIC, RELATIVE_COLORIMETRIC],
            TYPE_CMYK_DBL,
            TYPE_LAB_DBL,
            [[v * 100 for v in cmyk] for cmyk in cmyks],
            3,
        )

    def round_trip(self):
        """lcms2's whole round trip from Lab 0/0/0, before it neutralises
        and clips the result — `CreateRoundtripXForm` exactly."""
        return lcms_chain(
            [self.lab, self.profile, self.profile, self.lab],
            [RELATIVE_COLORIMETRIC, PERCEPTUAL, RELATIVE_COLORIMETRIC, RELATIVE_COLORIMETRIC],
            TYPE_LAB_DBL,
            TYPE_LAB_DBL,
            [[0.0, 0.0, 0.0]],
            3,
        )[0]


# Lab inputs for the `B2A0` leg: the black the round trip starts from, the
# neutral axis, and colours off it in each direction.
LAB_SAMPLES = [
    (0.0, 0.0, 0.0),
    (10.0, 5.0, -5.0),
    (35.0, -20.0, 30.0),
    (50.0, 0.0, 0.0),
    (72.0, 40.0, -25.0),
    (100.0, 0.0, 0.0),
]


def f64(v):
    return repr(float(v))


def f64_array(values):
    return "[" + ", ".join(f64(v) for v in values) + "]"


def rust_black_point(name, icc):
    lcms = Lcms(icc)
    rows = [f64_array(lcms.black_point(i)) for i in (PERCEPTUAL, RELATIVE_COLORIMETRIC, SATURATION)]
    lcms.close()
    return "\n".join(
        [
            f"/// lcms2's `cmsDetectBlackPoint` for `{name.lower()}.icc`, XYZ (D50,",
            "/// white Y = 1), indexed by intent: 0 perceptual, 1 relative",
            "/// colorimetric, 2 saturation. Zero means no compensation.",
            f"pub const {name}_BLACK_POINT: [[f64; 3]; 3] = [",
            *(f"    {row}," for row in rows),
            "];",
        ]
    )


def rust_round_trip(name, icc):
    """Each leg of lcms2's black-point round trip, for testing evaluators."""
    lcms = Lcms(icc)
    cmyks = lcms.b2a0(LAB_SAMPLES)
    ink = [v / 255 for v in (c for sample in SAMPLES for c in sample)]
    labs = lcms.a2b1([ink[i : i + 4] for i in range(0, len(ink), 4)])
    trip = lcms.round_trip()
    lcms.close()
    lower = name.lower()
    return "\n".join(
        [
            f"/// lcms2's `B2A0` of `{lower}.icc` at each of `LAB_SAMPLES`, ink 0–1.",
            f"pub const {name}_B2A0: [[f64; 4]; {len(LAB_SAMPLES)}] = [",
            *(f"    {f64_array(c)}," for c in cmyks),
            "];",
            f"/// lcms2's `A2B1` of `{lower}.icc` at each of `SAMPLES`, Lab.",
            f"pub const {name}_A2B1: [[f64; 3]; {len(SAMPLES)}] = [",
            *(f"    {f64_array(l)}," for l in labs),
            "];",
            f"/// lcms2's black-point round trip of `{lower}.icc` from Lab 0/0/0,",
            "/// before it sets a* = b* = 0 and clips L* to 50.",
            f"pub const {name}_ROUND_TRIP: [f64; 3] = {f64_array(trip)};",
        ]
    )


def rust_rgb_lab(name, icc):
    """An RGB profile's relative colorimetric Lab, as lcms2 computes it."""
    lcms = Lcms(icc)
    labs = lcms_chain(
        [lcms.profile, lcms.lab],
        [RELATIVE_COLORIMETRIC, RELATIVE_COLORIMETRIC],
        TYPE_RGB_DBL,
        TYPE_LAB_DBL,
        [[v / 255 for v in rgb] for rgb in RGB_SAMPLES],
        3,
    )
    lcms.close()
    return "\n".join(
        [
            f"/// lcms2's relative colorimetric Lab of `{name.lower()}.icc` at each of",
            "/// `RGB_SAMPLES`.",
            f"pub const {name}_LAB: [[f64; 3]; {len(RGB_SAMPLES)}] = [",
            *(f"    {f64_array(l)}," for l in labs),
            "];",
        ]
    )


def rust_a2b1(name, icc):
    """A profile's `A2B1` alone, as lcms2 reads it."""
    lcms = Lcms(icc)
    ink = [v / 255 for v in (c for sample in SAMPLES for c in sample)]
    labs = lcms.a2b1([ink[i : i + 4] for i in range(0, len(ink), 4)])
    lcms.close()
    return "\n".join(
        [
            f"/// lcms2's `A2B1` of `{name.lower()}.icc` at each of `SAMPLES`, Lab.",
            f"pub const {name}_A2B1: [[f64; 3]; {len(SAMPLES)}] = [",
            *(f"    {f64_array(l)}," for l in labs),
            "];",
        ]
    )


def rust_v4_legs(name, icc):
    """The relative colorimetric legs of an ICC v4 profile, `B2A1` and
    `A2B1`: lcms2 forces black-point compensation into a v4 profile under
    perceptual, so its `B2A0` through a transform is not the table alone."""
    lcms = Lcms(icc)
    cmyks = lcms_chain(
        [lcms.lab, lcms.profile],
        [RELATIVE_COLORIMETRIC, RELATIVE_COLORIMETRIC],
        TYPE_LAB_DBL,
        TYPE_CMYK_DBL,
        LAB_SAMPLES,
        4,
    )
    ink = [v / 255 for v in (c for sample in SAMPLES for c in sample)]
    labs = lcms.a2b1([ink[i : i + 4] for i in range(0, len(ink), 4)])
    lcms.close()
    lower = name.lower()
    return "\n".join(
        [
            f"/// lcms2's `B2A1` of `{lower}.icc` at each of `LAB_SAMPLES`, ink 0–1.",
            f"pub const {name}_B2A1: [[f64; 4]; {len(LAB_SAMPLES)}] = [",
            *(f"    {f64_array([v / 100 for v in c])}," for c in cmyks),
            "];",
            f"/// lcms2's `A2B1` of `{lower}.icc` at each of `SAMPLES`, Lab.",
            f"pub const {name}_A2B1: [[f64; 3]; {len(SAMPLES)}] = [",
            *(f"    {f64_array(l)}," for l in labs),
            "];",
        ]
    )


# Proofing-chain stage 1: source → output-intent CMYK. `inklimit.icc` is the
# output intent: its B2A0 differs from its B2A1 and it has no B2A2, so
# saturation reads B2A0. `split.icc` has no A2B2, so saturation reads its
# A2B0 too; `split_sat.icc` has one. `split_xyz.icc` goes into `split.icc`,
# whose B2A tables are affine in L*: moxcms interpolates a Lab-indexed table
# its own way, and the ink-limited B2A0 is built to tell interpolations
# apart.
CHAIN_PAIRS = [
    ("SPLIT", "INKLIMIT"),
    ("SPLIT_SAT", "INKLIMIT"),
    ("SPLIT_LUT8", "INKLIMIT_LUT8"),
    ("SPLIT_XYZ", "SPLIT"),
]


def rust_chain(source, oi, icc):
    src = Lcms(icc[source])
    out = Lcms(icc[oi])
    lines = [
        f"/// lcms2's proofing-chain stage 1, `{source.lower()}.icc` into",
        f"/// `{oi.lower()}.icc` without black-point compensation, at each of",
        "/// `SAMPLES`; ink 0–1, indexed by intent: 0 perceptual, 1 relative",
        "/// colorimetric, 2 saturation.",
        f"pub const CHAIN_{source}_{oi}: [[[f64; 4]; {len(SAMPLES)}]; 3] = [",
    ]
    for intent in (PERCEPTUAL, RELATIVE_COLORIMETRIC, SATURATION):
        inks = lcms_chain(
            [src.profile, out.profile],
            [intent, intent],
            TYPE_CMYK_DBL,
            TYPE_CMYK_DBL,
            [[v * 100 / 255 for v in sample] for sample in SAMPLES],
            4,
        )
        lines.append("    [")
        lines += [
            "        " + f64_array([min(max(v / 100, 0.0), 1.0) for v in ink]) + ","
            for ink in inks
        ]
        lines.append("    ],")
    lines.append("];")
    src.close()
    out.close()
    return "\n".join(lines)


# Output profiles whose destination black points the tests check: fitted,
# straight through the mid-tones (relative colorimetric), ICC v4, and
# profiles whose round trips are straight lines, where lcms2's fit gives
# zero.
DESTINATIONS = ["SHADOW", "SHADOW_STRAIGHT", "SHADOW_V4", "INKLIMIT", "SPLIT_SAT"]

# Gray levels for the Gray chain references.
GRAY_SAMPLES = [i / 16 for i in range(17)]

# Gray chains: `gray_trc.icc` into each output intent.
GRAY_CHAINS = ["SHADOW", "SHADOW_V4", "INKLIMIT"]


def rust_destination(name, icc):
    lcms = Lcms(icc)
    rows = [
        f64_array(lcms.destination_black_point(i))
        for i in (PERCEPTUAL, RELATIVE_COLORIMETRIC, SATURATION)
    ]
    lcms.close()
    return "\n".join(
        [
            f"/// lcms2's `cmsDetectDestinationBlackPoint` for `{name.lower()}.icc`,",
            "/// XYZ (D50, white Y = 1), indexed by intent: 0 perceptual, 1",
            "/// relative colorimetric, 2 saturation. Zero means no compensation.",
            f"pub const {name}_DESTINATION_BLACK_POINT: [[f64; 3]; 3] = [",
            *(f"    {row}," for row in rows),
            "];",
        ]
    )


def rust_gray_chain(oi, icc, source="GRAY_TRC"):
    src = Lcms(icc[source])
    out = Lcms(icc[oi])
    name = "GRAY_CHAIN" if source == "GRAY_TRC" else f"CHAIN_{source}"
    lines = [
        f"/// lcms2's proofing-chain stage 1, `{source.lower()}.icc` into `{oi.lower()}.icc`,",
        "/// at each of `GRAY_SAMPLES`; ink 0–1, indexed `[intent][bpc]`: intent",
        "/// 0 perceptual, 1 relative colorimetric, 2 saturation; black-point",
        "/// compensation 0 off, 1 on.",
        f"pub const {name}_{oi}: [[[[f64; 4]; {len(GRAY_SAMPLES)}]; 2]; 3] = [",
    ]
    for intent in (PERCEPTUAL, RELATIVE_COLORIMETRIC, SATURATION):
        lines.append("    [")
        for bpc in (False, True):
            xform = LCMS.cmsCreateExtendedTransform(
                None,
                2,
                (ctypes.c_void_p * 2)(src.profile, out.profile),
                (ctypes.c_int * 2)(int(bpc), int(bpc)),
                (ctypes.c_uint32 * 2)(intent, intent),
                (ctypes.c_double * 2)(1.0, 1.0),
                None,
                0,
                TYPE_GRAY_DBL,
                TYPE_CMYK_DBL,
                FLAGS_NOCACHE_NOOPTIMIZE,
            )
            assert xform, "lcms2 could not build the Gray transform"
            lines.append("        [")
            for g in GRAY_SAMPLES:
                s_ = (ctypes.c_double * 1)(g)
                d = (ctypes.c_double * 4)()
                LCMS.cmsDoTransform(xform, s_, d, 1)
                lines.append(
                    "            " + f64_array([min(max(v / 100, 0.0), 1.0) for v in d]) + ","
                )
            LCMS.cmsDeleteTransform(xform)
            lines.append("        ],")
        lines.append("    ],")
    lines.append("];")
    src.close()
    out.close()
    return "\n".join(lines)


# RGB inputs, 0–255: black, the primaries and secondaries, where a channel is
# zero, and colours between.
RGB_SAMPLES = [
    (0, 0, 0),
    (255, 0, 0),
    (0, 255, 0),
    (0, 0, 255),
    (255, 255, 0),
    (0, 255, 255),
    (255, 0, 255),
    (255, 255, 255),
    (128, 128, 128),
    (255, 128, 0),
    (64, 0, 192),
    (10, 200, 90),
    (230, 30, 140),
    (0, 0, 64),
    (37, 181, 222),
]

# RGB chains: each RGB profile into `inklimit.icc`.
RGB_CHAINS = ["RGB_GAMMA", "RGB_LUT"]


def rust_rgb_chain(source, oi, icc):
    src = Lcms(icc[source])
    out = Lcms(icc[oi])
    lines = [
        f"/// lcms2's proofing-chain stage 1, `{source.lower()}.icc` into",
        f"/// `{oi.lower()}.icc` without black-point compensation, at each of",
        "/// `RGB_SAMPLES`; ink 0–1, indexed by intent: 0 perceptual, 1 relative",
        "/// colorimetric, 2 saturation.",
        f"pub const CHAIN_{source}_{oi}: [[[f64; 4]; {len(RGB_SAMPLES)}]; 3] = [",
    ]
    for intent in (PERCEPTUAL, RELATIVE_COLORIMETRIC, SATURATION):
        inks = lcms_chain(
            [src.profile, out.profile],
            [intent, intent],
            TYPE_RGB_DBL,
            TYPE_CMYK_DBL,
            [[v / 255 for v in sample] for sample in RGB_SAMPLES],
            4,
        )
        lines.append("    [")
        lines += [
            "        " + f64_array([min(max(v / 100, 0.0), 1.0) for v in ink]) + ","
            for ink in inks
        ]
        lines.append("    ],")
    lines.append("];")
    src.close()
    out.close()
    return "\n".join(lines)


LCMS.cmsBuildParametricToneCurve.restype = ctypes.c_void_p
LCMS.cmsBuildParametricToneCurve.argtypes = [
    ctypes.c_void_p,
    ctypes.c_int32,
    ctypes.POINTER(ctypes.c_double),
]
LCMS.cmsBuildTabulatedToneCurve16.restype = ctypes.c_void_p
LCMS.cmsBuildTabulatedToneCurve16.argtypes = [
    ctypes.c_void_p,
    ctypes.c_uint32,
    ctypes.POINTER(ctypes.c_uint16),
]
LCMS.cmsEvalToneCurveFloat.restype = ctypes.c_float
LCMS.cmsEvalToneCurveFloat.argtypes = [ctypes.c_void_p, ctypes.c_float]
LCMS.cmsFreeToneCurve.argtypes = [ctypes.c_void_p]


def f32(v):
    """`v` rounded to the nearest `f32`, as moxcms holds a curve parameter."""
    return struct.unpack("<f", struct.pack("<f", v))[0]


def f32_literal(v):
    """The shortest decimal that is the `f32` `v`."""
    for digits in range(1, 10):
        text = f"{v:.{digits}g}"
        if f32(float(text)) == v:
            return text
    raise AssertionError(v)


# Where the curves are evaluated: the ends, the first 8-bit step, and either
# side of each curve's break.
CURVE_X = [0.0, 1 / 255, 0.02, 0.04045, 0.0625, 0.1, 0.2, 0.25, 0.3, 0.5, 0.75, 0.999, 1.0]

# Parametric curves of each ICC function type (lcms2 type 1–5) as moxcms
# reads them: `[g, a, b, c, d, e, f]`, as many as the type takes.
CURVE_PARAMETRIC = [
    [2.2],
    [2.4, 1.25, -0.25],
    [1.8, 1.5, -0.375, 0.0625],
    [2.4, 1 / 1.055, 0.055 / 1.055, 1 / 12.92, 0.04045],
    [2.2, 0.9, 0.08, 0.2, 0.1, 0.02, 0.01],
]
# A `curv` table and a `curv` pure gamma (u8Fixed8 461 = 1.80078125).
CURVE_TABLE = [round(65535 * (i / 36) ** 2.2) for i in range(37)]
CURVE_GAMMA = 461


def srgb_profile():
    """lcms2's built-in sRGB, as bytes."""
    handle = LCMS.cmsCreate_sRGBProfile()
    size = ctypes.c_uint32(0)
    assert LCMS.cmsSaveProfileToMem(handle, None, ctypes.byref(size))
    buf = ctypes.create_string_buffer(size.value)
    assert LCMS.cmsSaveProfileToMem(handle, buf, ctypes.byref(size))
    LCMS.cmsCloseProfile(handle)
    icc = bytearray(buf.raw[: size.value])
    # lcms2 stamps the time it saved the profile; pin it so the file
    # regenerates byte for byte.
    icc[24:36] = struct.pack(">6H", *SRGB_SAVED)
    assert icc[84:100] == bytes(16), "a profile ID would cover the date"
    return bytes(icc)


# When `srgb.icc` was first generated, as its header records it.
SRGB_SAVED = (2026, 10, 2, 3, 51, 56)


# Stage 1 with and without black-point compensation, into output intents
# whose blacks compensation moves to: `shadow.icc`, and `shadow_v4.icc`,
# where lcms2 forces it under perceptual and saturation. Each entry is
# (source, output intent, input kind); "LAB" is lcms2's Lab identity.
BPC_CHAINS = [
    ("SPLIT", "SHADOW", "CMYK"),
    ("SPLIT_SAT", "SHADOW_V4", "CMYK"),
    ("INKLIMIT", "SHADOW", "CMYK"),
    ("RGB_GAMMA", "SHADOW", "RGB"),
    ("RGB_LUT", "SHADOW", "RGB"),
    ("RGB_LUT_V4", "SHADOW", "RGB"),
    ("SRGB", "SHADOW", "RGB"),
    ("SRGB", "SHADOW_V4", "RGB"),
    ("LAB", "SHADOW", "LAB"),
    ("LAB", "SHADOW_V4", "LAB"),
    # ICC v4 `lutAToBType` / `lutBToAType` tables, as a source and as the
    # output intent.
    ("MAB", "SHADOW", "CMYK"),
    ("SPLIT", "MAB", "CMYK"),
    ("RGB_MAB", "SHADOW", "RGB"),
    ("RGB_GAMMA", "MAB", "RGB"),
    ("LAB", "MAB", "LAB"),
]


# Stage 1 with an XYZ PCS on either side.
XYZ_BPC_CHAINS = [
    ("XYZ_V4", "SHADOW", "CMYK"),
    ("XYZ_MAB", "SHADOW", "CMYK"),
    ("SPLIT", "XYZ_V4", "CMYK"),
    ("SPLIT", "XYZ_MAB", "CMYK"),
    ("XYZ_V4", "XYZ_MAB", "CMYK"),
    ("RGB_XYZ", "SHADOW", "RGB"),
    ("RGB_XYZ_MAB", "SHADOW", "RGB"),
    ("RGB_GAMMA", "XYZ_V4", "RGB"),
    ("LAB", "XYZ_V4", "LAB"),
    ("LAB", "XYZ_MAB", "LAB"),
]


def rust_bpc_chain(source, oi, kind, icc):
    out = Lcms(icc[oi])
    src = Lcms(icc[source]) if kind != "LAB" else None
    src_handle = src.profile if src else out.lab
    inputs, in_format, samples = {
        "CMYK": (
            [[v * 100 / 255 for v in sample] for sample in SAMPLES],
            TYPE_CMYK_DBL,
            "SAMPLES",
        ),
        "RGB": ([[v / 255 for v in sample] for sample in RGB_SAMPLES], TYPE_RGB_DBL, "RGB_SAMPLES"),
        "LAB": ([list(lab) for lab in LAB_SAMPLES], TYPE_LAB_DBL, "LAB_SAMPLES"),
    }[kind]
    lines = [
        f"/// lcms2's proofing-chain stage 1, `{source.lower()}` into `{oi.lower()}.icc`,",
        f"/// at each of `{samples}`; ink 0–1, indexed `[intent][bpc]`: intent 0",
        "/// perceptual, 1 relative colorimetric, 2 saturation; black-point",
        "/// compensation 0 off, 1 on.",
        f"pub const CHAIN_BPC_{source}_{oi}: [[[[f64; 4]; {len(inputs)}]; 2]; 3] = [",
    ]
    for intent in (PERCEPTUAL, RELATIVE_COLORIMETRIC, SATURATION):
        lines.append("    [")
        for bpc in (False, True):
            xform = LCMS.cmsCreateExtendedTransform(
                None,
                2,
                (ctypes.c_void_p * 2)(src_handle, out.profile),
                (ctypes.c_int * 2)(int(bpc), int(bpc)),
                (ctypes.c_uint32 * 2)(intent, intent),
                (ctypes.c_double * 2)(1.0, 1.0),
                None,
                0,
                in_format,
                TYPE_CMYK_DBL,
                FLAGS_NOCACHE_NOOPTIMIZE,
            )
            assert xform, f"lcms2 could not build {source} → {oi}"
            lines.append("        [")
            for v in inputs:
                s_ = (ctypes.c_double * len(v))(*v)
                d = (ctypes.c_double * 4)()
                LCMS.cmsDoTransform(xform, s_, d, 1)
                lines.append(
                    "            " + f64_array([min(max(x / 100, 0.0), 1.0) for x in d]) + ","
                )
            LCMS.cmsDeleteTransform(xform)
            lines.append("        ],")
        lines.append("    ],")
    lines.append("];")
    if src:
        src.close()
    out.close()
    return "\n".join(lines)


def rust_curves():
    xs = [f32(x) for x in CURVE_X]

    def evaluate(curve):
        assert curve
        ys = [LCMS.cmsEvalToneCurveFloat(curve, x) for x in xs]
        LCMS.cmsFreeToneCurve(curve)
        return ys

    parametric = []
    for params in CURVE_PARAMETRIC:
        params = [f32(p) for p in params]
        kind = {1: 1, 3: 2, 4: 3, 5: 4, 7: 5}[len(params)]
        ys = evaluate(
            LCMS.cmsBuildParametricToneCurve(
                None, kind, (ctypes.c_double * len(params))(*params)
            )
        )
        parametric.append((params, ys))
    table = evaluate(
        LCMS.cmsBuildTabulatedToneCurve16(
            None, len(CURVE_TABLE), (ctypes.c_uint16 * len(CURVE_TABLE))(*CURVE_TABLE)
        )
    )
    gamma = (ctypes.c_double * 1)(CURVE_GAMMA / 256)
    pure = evaluate(LCMS.cmsBuildParametricToneCurve(None, 1, gamma))
    n = len(xs)
    lines = [
        "/// Where the curves below are evaluated (each an `f32`).",
        f"pub const CURVE_X: [f64; {n}] = {f64_array(xs)};",
        "/// Parametric curves, `[g, a, b, c, d, e, f]` as far as each ICC",
        "/// function type takes, and lcms2's `cmsEvalToneCurveFloat` of each at",
        "/// `CURVE_X`.",
        f"pub const CURVE_PARAMETRIC: [(&[f32], [f64; {n}]); {len(parametric)}] = [",
        *(
            f"    (&[{', '.join(f32_literal(p) for p in params)}], {f64_array(ys)}),"
            for params, ys in parametric
        ),
        "];",
        "/// A `curv` table, and lcms2's evaluation of it at `CURVE_X`.",
        f"pub const CURVE_TABLE: [u16; {len(CURVE_TABLE)}] = [{', '.join(map(str, CURVE_TABLE))}];",
        f"pub const CURVE_TABLE_EVAL: [f64; {n}] = {f64_array(table)};",
        "/// A `curv` pure gamma (u8Fixed8), and lcms2's evaluation of it.",
        f"pub const CURVE_GAMMA: u16 = {CURVE_GAMMA};",
        f"pub const CURVE_GAMMA_EVAL: [f64; {n}] = {f64_array(pure)};",
    ]
    return "\n".join(lines)


def rust_table(name, icc):
    lines = [
        f"/// lcms2's sRGB output for `{name.lower()}.icc`, indexed",
        "/// `[intent][bpc][sample]`: intent 0 perceptual, 1 relative",
        "/// colorimetric, 2 saturation; bpc 0 off, 1 on.",
        f"pub const {name}: [[[[u8; 3]; {len(SAMPLES)}]; 2]; 3] = [",
    ]
    for intent_name, intent in INTENTS:
        lines.append(f"    // {intent_name}")
        lines.append("    [")
        for bpc in (False, True):
            rgb = lcms_rgb(icc, intent, bpc)
            row = ", ".join(f"[{r}, {g}, {b}]" for r, g, b in rgb)
            lines.append(f"        [{row}],")
        lines.append("    ],")
    lines.append("];")
    return "\n".join(lines)


# The reverse transform: lcms2's built-in sRGB into each profile, relative
# colorimetric, and back out through the profile's display.
REVERSE = ["INKLIMIT", "SHADOW", "SPLIT_LUT8", "MAB", "XYZ_V4"]


def lcms_pair(source, dest, bpc, in_format, out_format, inputs, n_out):
    """`inputs` through `source` → `dest`, relative colorimetric, with
    black-point compensation `bpc`, unoptimised, in doubles."""
    xform = LCMS.cmsCreateExtendedTransform(
        None,
        2,
        (ctypes.c_void_p * 2)(source, dest),
        (ctypes.c_int * 2)(int(bpc), int(bpc)),
        (ctypes.c_uint32 * 2)(RELATIVE_COLORIMETRIC, RELATIVE_COLORIMETRIC),
        (ctypes.c_double * 2)(1.0, 1.0),
        None,
        0,
        in_format,
        out_format,
        FLAGS_NOCACHE_NOOPTIMIZE,
    )
    assert xform, "lcms2 could not build the transform"
    out = []
    for v in inputs:
        src = (ctypes.c_double * len(v))(*v)
        dst = (ctypes.c_double * n_out)()
        LCMS.cmsDoTransform(xform, src, dst, 1)
        out.append(list(dst))
    LCMS.cmsDeleteTransform(xform)
    return out


def rust_reverse(name, icc):
    srgb = Lcms(icc["SRGB"])
    out = Lcms(icc[name])
    rgbs = [[v / 255 for v in sample] for sample in RGB_SAMPLES]
    inks, trips = [], []
    for bpc in (False, True):
        ink = lcms_pair(srgb.profile, out.profile, bpc, TYPE_RGB_DBL, TYPE_CMYK_DBL, rgbs, 4)
        ink = [[min(max(v / 100, 0.0), 1.0) for v in cmyk] for cmyk in ink]
        inks.append(ink)
        trips.append(
            lcms_pair(
                out.profile,
                srgb.profile,
                bpc,
                TYPE_CMYK_DBL,
                TYPE_RGB_DBL,
                [[v * 100 for v in cmyk] for cmyk in ink],
                3,
            )
        )
    srgb.close()
    out.close()
    lower = name.lower()
    n = len(RGB_SAMPLES)
    return "\n".join(
        [
            f"/// lcms2's built-in sRGB → `{lower}.icc`, relative colorimetric, at each",
            "/// of `RGB_SAMPLES`; ink 0–1, indexed by black-point compensation: 0",
            "/// off, 1 on.",
            f"pub const REVERSE_{name}: [[[f64; 4]; {n}]; 2] = [",
            *(
                line
                for ink in inks
                for line in ["    [", *(f"        {f64_array(c)}," for c in ink), "    ],"]
            ),
            "];",
            f"/// That ink back through `{lower}.icc` → lcms2's built-in sRGB, relative",
            "/// colorimetric, with the same compensation; sRGB 0–1, unclipped, so a",
            "/// channel outside it is a colour the display had to clip.",
            f"pub const ROUND_TRIP_RGB_{name}: [[[f64; 3]; {n}]; 2] = [",
            *(
                line
                for trip in trips
                for line in ["    [", *(f"        {f64_array(c)}," for c in trip), "    ],"]
            ),
            "];",
        ]
    )


# RGB inputs for the display tables, 0–255: `RGB_SAMPLES`, then a 5×5×5 grid.
DISPLAY_LEVELS = [0, 50, 128, 200, 255]
DISPLAY_SAMPLES = list(RGB_SAMPLES) + [
    (r, g, b) for r in DISPLAY_LEVELS for g in DISPLAY_LEVELS for b in DISPLAY_LEVELS
]

# RGB profiles with A2B tables, whose display tables the tests check.
DISPLAY = ["RGB_LUT", "RGB_LUT_V4", "RGB_MAB", "RGB_XYZ", "RGB_XYZ_MAB", "RGB_MIXED"]


def rust_rgb_display(name, icc):
    """lcms2's built-in sRGB output for an RGB profile, unoptimised, in
    doubles, for each intent and black-point compensation setting."""
    src = Lcms(icc[name])
    dst = Lcms(icc["SRGB"])
    lines = [
        f"/// lcms2's built-in sRGB of `{name.lower()}.icc` at each of",
        "/// `DISPLAY_SAMPLES`, 0–1, indexed `[intent][bpc]`: intent 0",
        "/// perceptual, 1 relative colorimetric, 2 saturation; black-point",
        "/// compensation 0 off, 1 on.",
        f"pub static DISPLAY_{name}: [[[[f64; 3]; {len(DISPLAY_SAMPLES)}]; 2]; 3] = [",
    ]
    for intent in (PERCEPTUAL, RELATIVE_COLORIMETRIC, SATURATION):
        lines.append("    [")
        for bpc in (False, True):
            xform = LCMS.cmsCreateExtendedTransform(
                None,
                2,
                (ctypes.c_void_p * 2)(src.profile, dst.profile),
                (ctypes.c_int * 2)(int(bpc), int(bpc)),
                (ctypes.c_uint32 * 2)(intent, intent),
                (ctypes.c_double * 2)(1.0, 1.0),
                None,
                0,
                TYPE_RGB_DBL,
                TYPE_RGB_DBL,
                FLAGS_NOCACHE_NOOPTIMIZE,
            )
            assert xform, f"lcms2 could not build {name}'s sRGB transform"
            lines.append("        [")
            for rgb in DISPLAY_SAMPLES:
                s_ = (ctypes.c_double * 3)(*(v / 255 for v in rgb))
                d = (ctypes.c_double * 3)()
                LCMS.cmsDoTransform(xform, s_, d, 1)
                values = ", ".join(repr(round(min(max(v, 0.0), 1.0), 6)) for v in d)
                lines.append(f"            [{values}],")
            LCMS.cmsDeleteTransform(xform)
            lines.append("        ],")
        lines.append("    ],")
    lines.append("];")
    src.close()
    dst.close()
    return "\n".join(lines)


def main():
    profiles = [
        ("SPLIT", profile("stet test: A2B0 != A2B1, no A2B2", with_saturation=False)),
        ("SPLIT_SAT", profile("stet test: A2B0 != A2B1 != A2B2", with_saturation=True)),
        (
            "SPLIT_LUT8",
            profile("stet test: split.icc as lut8", with_saturation=False, bits=8),
        ),
        (
            "SAME",
            profile("stet test: A2B0 == A2B1", with_saturation=False, perceptual=A2B1),
        ),
    ]
    inklimit = dict(with_saturation=False, perceptual_b2a=INKLIMITED_B2A)
    profiles += [
        ("INKLIMIT", profile("stet test: ink-limited B2A0", **inklimit)),
        (
            "INKLIMIT_LUT8",
            profile("stet test: inklimit.icc as lut8", bits=8, **inklimit),
        ),
        (
            "INKLIMIT_SCNR",
            profile("stet test: inklimit.icc as input class", device_class=b"scnr", **inklimit),
        ),
        (
            "INKLIMIT_V4",
            profile("stet test: inklimit.icc as ICC v4", version=0x04200000, **inklimit),
        ),
        (
            "INKLIMIT_NO_A2B0",
            profile("stet test: inklimit.icc without A2B0", perceptual=None, **inklimit),
        ),
    ]
    profiles.append(("SPLIT_XYZ", xyz_profile("stet test: CMYK, XYZ PCS, A2B0 != A2B1")))
    profiles += [
        ("SHADOW", shadow_profile("stet test: B2A tables easing into an ink limit")),
        (
            "SHADOW_STRAIGHT",
            shadow_profile("stet test: shadow.icc, straight B2A1", relcol_gamma=1.0),
        ),
        ("SHADOW_V4", shadow_profile("stet test: shadow.icc as ICC v4", version=0x04200000)),
        ("GRAY_TRC", gray_profile("stet test: Gray TRC, XYZ PCS, black L* 20")),
        ("GRAY_GAMMA", gray_gamma_profile("stet test: Gray, pure gamma 1.8")),
        ("RGB_GAMMA", rgb_gamma_profile("stet test: matrix RGB, pure gamma 1.8")),
        ("RGB_LUT", rgb_lut_profile("stet test: RGB A2B1 + A2B2, no A2B0")),
        ("RGB_LUT_V4", rgb_lut_profile("stet test: RGB A2B0-2, ICC v4, no matrix", v4=True)),
        ("SRGB", srgb_profile()),
        ("GRAY_NEAR_BLACK", gray_near_black_profile("stet test: Gray, black Y 0.0002")),
        ("MAB", mab_profile("stet test: ICC v4 lutAToBType / lutBToAType")),
        ("RGB_MAB", rgb_mab_profile("stet test: ICC v4 RGB lutAToBType")),
        ("MAB_GRID", mab_grid_profile("stet test: mab.icc, A2B grid 3x4x5x3")),
        ("XYZ_V4", xyz_v4_profile("stet test: CMYK, XYZ PCS, ICC v4 lut16")),
        ("XYZ_MAB", xyz_mab_profile("stet test: CMYK, XYZ PCS, ICC v4 mAB/mBA")),
        ("RGB_XYZ", rgb_xyz_profile("stet test: RGB, XYZ PCS, lut16 A2B0/A2B1")),
        ("RGB_XYZ_MAB", rgb_xyz_mab_profile("stet test: RGB, XYZ PCS, ICC v4 mAB A2B0")),
        ("RGB_MIXED", rgb_mixed_profile("stet test: RGB, XYZ PCS, mAB with no CLUT")),
        (
            "INKLIMIT_A2B",
            profile(
                "stet test: inklimit.icc as input class, no B2A",
                device_class=b"scnr",
                with_b2a=False,
                **inklimit,
            ),
        ),
        (
            "INKLIMIT_MATRIX",
            profile(
                "stet test: inklimit.icc, B2A0 with a matrix",
                with_saturation=False,
                perceptual_b2a=lambda bits: lut(
                    bits, 3, 4, inklimited_b2a, matrix=(1, 0, 0, 0, 0.9, 0, 0, 0, 1.1)
                ),
            ),
        ),
    ]
    for name, icc in profiles:
        (HERE / f"{name.lower()}.icc").write_bytes(icc)
    icc = dict(profiles)

    samples = ", ".join(f"[{c}, {m}, {y}, {k}]" for c, m, y, k in SAMPLES)
    labs = ", ".join(f64_array(lab) for lab in LAB_SAMPLES)
    lcms = ImageCms.core.littlecms_version
    linked = LCMS.cmsGetEncodedCMMversion()
    assert lcms == f"{linked // 1000}.{linked % 1000 // 10}", (lcms, linked)
    out = [
        "// @generated by generate.py — do not edit; re-run the script.",
        f"// lcms2 {lcms} (Pillow {PILLOW_VERSION}), destination: lcms2's built-in",
        "// sRGB (ICC v4), cmsFLAGS_NOOPTIMIZE.",
        "",
        "/// CMYK inputs, 0–255, in the order of every table below.",
        f"pub const SAMPLES: [[u8; 4]; {len(SAMPLES)}] = [{samples}];",
        "",
        "/// Lab inputs to the `B2A0` tables below.",
        f"pub const LAB_SAMPLES: [[f64; 3]; {len(LAB_SAMPLES)}] = [{labs}];",
        "",
    ]
    # No sRGB tables for the profile without A2B0: lcms2 has no perceptual
    # transform for it to compare against.
    for name in ["SPLIT", "SPLIT_SAT", "SPLIT_LUT8", "SAME", "INKLIMIT", "INKLIMIT_LUT8", "INKLIMIT_SCNR", "INKLIMIT_V4"]:
        out += [rust_table(name, icc[name]), ""]
    for name, _ in profiles:
        if name not in (
            "SPLIT_XYZ",
            "SHADOW",
            "SHADOW_STRAIGHT",
            "SHADOW_V4",
            "GRAY_GAMMA",
            "RGB_GAMMA",
            "RGB_LUT",
            "RGB_LUT_V4",
            "SRGB",
            "GRAY_NEAR_BLACK",
            "MAB",
            "RGB_MAB",
            "MAB_GRID",
            "XYZ_V4",
            "XYZ_MAB",
            "INKLIMIT_MATRIX",
            "RGB_XYZ",
            "RGB_XYZ_MAB",
            "RGB_MIXED",
            "INKLIMIT_A2B",
        ):
            out += [rust_black_point(name, icc[name]), ""]
    for name in ["INKLIMIT", "INKLIMIT_LUT8"]:
        out += [rust_round_trip(name, icc[name]), ""]
    for source, oi in CHAIN_PAIRS:
        out += [rust_chain(source, oi, icc), ""]
    grays = ", ".join(f64(g) for g in GRAY_SAMPLES)
    out += [
        "/// Gray inputs, 0–1, in the order of the Gray chain tables below.",
        f"pub const GRAY_SAMPLES: [f64; {len(GRAY_SAMPLES)}] = [{grays}];",
        "",
    ]
    for name in DESTINATIONS:
        out += [rust_destination(name, icc[name]), ""]
    for oi in GRAY_CHAINS:
        out += [rust_gray_chain(oi, icc), ""]
    rgbs = ", ".join(f"[{r}, {g}, {b}]" for r, g, b in RGB_SAMPLES)
    out += [
        "/// RGB inputs, 0–255, in the order of the RGB chain tables below.",
        f"pub const RGB_SAMPLES: [[u8; 3]; {len(RGB_SAMPLES)}] = [{rgbs}];",
        "",
    ]
    for source in RGB_CHAINS:
        out += [rust_rgb_chain(source, "INKLIMIT", icc), ""]
    out += [rust_gray_chain("INKLIMIT", icc, source="GRAY_GAMMA"), ""]
    out += [rust_curves(), ""]
    for name in ["RGB_GAMMA", "RGB_LUT", "RGB_LUT_V4", "SRGB"]:
        out += [rust_black_point(name, icc[name]), ""]
    for source, oi, kind in BPC_CHAINS:
        out += [rust_bpc_chain(source, oi, kind, icc), ""]
    out += [rust_gray_chain("INKLIMIT", icc, source="GRAY_NEAR_BLACK"), ""]
    out += [rust_black_point("MAB", icc["MAB"]), ""]
    out += [rust_black_point("RGB_MAB", icc["RGB_MAB"]), ""]
    out += [rust_destination("MAB", icc["MAB"]), ""]
    out += [rust_v4_legs("MAB", icc["MAB"]), ""]
    out += [rust_gray_chain("MAB", icc), ""]
    for name in ["MAB", "MAB_GRID", "SPLIT_XYZ"]:
        out += [rust_table(name, icc[name]), ""]
    for name in ["XYZ_V4", "XYZ_MAB", "INKLIMIT_MATRIX"]:
        out += [rust_table(name, icc[name]), ""]
        out += [rust_black_point(name, icc[name]), ""]
    for name in ["XYZ_V4", "XYZ_MAB"]:
        out += [rust_v4_legs(name, icc[name]), ""]
    out += [rust_round_trip("INKLIMIT_MATRIX", icc["INKLIMIT_MATRIX"]), ""]
    out += [rust_a2b1("SPLIT_XYZ", icc["SPLIT_XYZ"]), ""]
    for name in ["RGB_XYZ", "RGB_XYZ_MAB"]:
        out += [rust_black_point(name, icc[name]), ""]
    for source, oi, kind in XYZ_BPC_CHAINS:
        out += [rust_bpc_chain(source, oi, kind, icc), ""]
    for oi in ["XYZ_V4", "XYZ_MAB"]:
        out += [rust_gray_chain(oi, icc), ""]
    for name in ["RGB_GAMMA", "SRGB"]:
        out += [rust_rgb_lab(name, icc[name]), ""]
    for name in REVERSE:
        out += [rust_reverse(name, icc), ""]
    displays = ", ".join(f"[{r}, {g}, {b}]" for r, g, b in DISPLAY_SAMPLES)
    out += [
        "/// RGB inputs, 0–255, in the order of the display tables below.",
        f"pub const DISPLAY_SAMPLES: [[u8; 3]; {len(DISPLAY_SAMPLES)}] = [{displays}];",
        "",
    ]
    for name in DISPLAY:
        out += [rust_rgb_display(name, icc), ""]
    (HERE / "reference.rs").write_text("\n".join(out))


if __name__ == "__main__":
    main()
