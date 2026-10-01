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
  reference.rs   lcms2's sRGB output for each profile, intent and BPC setting,
                 its black points, and the round trip behind them; included
                 by `tests/cmyk_intent.rs`.

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


def lut(bits, n_in, n_out, clut):
    """An `mft2` (bits=16) or `mft1` (bits=8) tag on a 2-point grid with
    identity curves. `clut` maps grid coordinates (each 0.0 or 1.0, first
    channel slowest) to normalised outputs."""
    t = (b"mft2" if bits == 16 else b"mft1") + bytes(4) + bytes([n_in, n_out, 2, 0])
    for v in (1, 0, 0, 0, 1, 0, 0, 0, 1):
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
    for idx in range(2**n_in):
        coords = [(idx >> (n_in - 1 - i)) & 1 for i in range(n_in)]
        out = clut([float(c) for c in coords])
        assert len(out) == n_out
        for v in out:
            t += sample(min(max(v, 0.0), 1.0))
    t += curves(n_out)
    return t


def lab_table(bits, l, a, b):
    """A2B clut: CMYK → legacy v2 Lab. 16-bit: L* 0..100 → 0..0xFF00, a*/b*
    → (v + 128) × 256. 8-bit: L* 0..100 → 0..255, a*/b* → v + 128."""

    def clut(cmyk):
        L = l(*cmyk)
        A = a(*cmyk)
        B = b(*cmyk)
        assert 0 <= L <= 100 and -128 <= A < 128 and -128 <= B < 128
        if bits == 16:
            return [L * 652.8 / 65535, (A + 128) * 256 / 65535, (B + 128) * 256 / 65535]
        return [L / 100, (A + 128) / 255, (B + 128) / 255]

    return lut(bits, 4, 3, clut)


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
):
    tags = [
        (b"desc", desc_tag(description)),
        (b"wtpt", xyz_tag(0.9642, 1.0, 0.8249)),
    ]
    if perceptual is not None:
        tags.append((b"A2B0", perceptual(bits)))
    tags += [
        (b"A2B1", A2B1(bits)),
        (b"B2A0", perceptual_b2a(bits)),
        (b"B2A1", B2A(bits)),
    ]
    if with_saturation:
        tags += [(b"A2B2", A2B2(bits)), (b"B2A2", B2A(bits))]

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
    header[16:20] = b"CMYK"
    header[20:24] = b"Lab "
    header[36:40] = b"acsp"
    header[68:80] = xyz_tag(0.9642, 1.0, 0.8249)[8:]
    return bytes(header) + table + data


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
LCMS.cmsDeleteTransform.argtypes = [ctypes.c_void_p]
LCMS.cmsDetectBlackPoint.restype = ctypes.c_int
LCMS.cmsDetectBlackPoint.argtypes = [
    ctypes.POINTER(ctypes.c_double * 3),
    ctypes.c_void_p,
    ctypes.c_uint32,
    ctypes.c_uint32,
]

# lcms2.h: FLOAT_SH(1) | COLORSPACE_SH(PT_…) | CHANNELS_SH(n) | BYTES_SH(0).
TYPE_LAB_DBL = (1 << 22) | (10 << 16) | (3 << 3)
TYPE_CMYK_DBL = (1 << 22) | (6 << 16) | (4 << 3)
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
        out += [rust_black_point(name, icc[name]), ""]
    for name in ["INKLIMIT", "INKLIMIT_LUT8"]:
        out += [rust_round_trip(name, icc[name]), ""]
    (HERE / "reference.rs").write_text("\n".join(out))


if __name__ == "__main__":
    main()
