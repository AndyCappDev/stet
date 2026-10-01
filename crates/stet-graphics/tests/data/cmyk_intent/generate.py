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
  reference.rs   lcms2's sRGB output for each profile, intent and BPC setting,
                 included by `tests/cmyk_intent.rs`.

The profiles are ICC v2 output (`prtr`) profiles with a Lab PCS. The first
two use `lut16Type` tables — the shape of FOGRA39, ISO Coated v2 or Japan
Color 2001, and the shape stet's hand-rolled sampler reads. Each table is affine in CMYK
on a two-point grid, so tetrahedral and multilinear interpolation reproduce
it exactly and the engines differ only in what the test is about.

Two details steer what lcms2 does with these profiles:

- The destination is lcms2's built-in sRGB, an ICC v4 profile. lcms2 forces
  black-point compensation on for the perceptual and saturation intents when
  either profile is v4, so for those intents the BPC flag changes nothing.
- For relative colorimetric on a CMYK output profile lcms2 takes the source
  black point from a round trip of Lab L*=0 through `B2A0` then `A2B1`.
  `B2A0` here maps L*=0 to 400% ink, so that black point is the same as the
  darkest colorant, which is how stet finds it.

Run with Pillow (which bundles lcms2):  python3 generate.py
"""

import io
import struct
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


def profile(description, with_saturation, bits=16):
    tags = [
        (b"desc", desc_tag(description)),
        (b"wtpt", xyz_tag(0.9642, 1.0, 0.8249)),
        (b"A2B0", A2B0(bits)),
        (b"A2B1", A2B1(bits)),
        (b"B2A0", B2A(bits)),
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
    header[8:12] = struct.pack(">I", 0x02100000)
    header[12:16] = b"prtr"
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
    split = profile("stet test: A2B0 != A2B1, no A2B2", with_saturation=False)
    split_sat = profile("stet test: A2B0 != A2B1 != A2B2", with_saturation=True)
    (HERE / "split.icc").write_bytes(split)
    (HERE / "split_sat.icc").write_bytes(split_sat)
    split_lut8 = profile("stet test: split.icc as lut8", with_saturation=False, bits=8)
    (HERE / "split_lut8.icc").write_bytes(split_lut8)

    samples = ", ".join(f"[{c}, {m}, {y}, {k}]" for c, m, y, k in SAMPLES)
    lcms = ImageCms.core.littlecms_version
    out = [
        "// @generated by generate.py — do not edit; re-run the script.",
        f"// lcms2 {lcms} (Pillow {PILLOW_VERSION}), destination: lcms2's built-in",
        "// sRGB (ICC v4), cmsFLAGS_NOOPTIMIZE.",
        "",
        "/// CMYK inputs, 0–255, in the order of every table below.",
        f"pub const SAMPLES: [[u8; 4]; {len(SAMPLES)}] = [{samples}];",
        "",
        rust_table("SPLIT", split),
        "",
        rust_table("SPLIT_SAT", split_sat),
        "",
        rust_table("SPLIT_LUT8", split_lut8),
        "",
    ]
    (HERE / "reference.rs").write_text("\n".join(out))


if __name__ == "__main__":
    main()
