#!/usr/bin/env python3
# stet - A PostScript Interpreter
# Copyright (c) 2026 Scott Bowman
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Regenerate crates/stet-fonts/src/cid_<ordering>.bin from Adobe's CMaps.

Each Adobe CJK collection needs two mappings, and they come from two files
because neither direction can be derived from the other:

  Adobe-<Ordering>-UCS2   CID -> Unicode text. Adobe's own statement of what
                          each CID means, used for text extraction. Complete
                          (every CID), and a destination may be several
                          characters: a surrogate pair, a base plus a
                          variation selector, a ligature.
  Uni<XX>-UTF32-H         Unicode -> CID. What a Unicode-keyed encoding
                          selects. Many code points can share a CID, and
                          inverting this file to get CID -> Unicode keeps an
                          arbitrary one of them (Japan1 CID 3284 came out as
                          the Kangxi radical U+2F47 instead of U+65E5), which
                          is the bug these tables replace.

Usage:
    scripts/gen_cid_unicode_tables.py [CMAP_DIR]

CMAP_DIR defaults to /usr/share/poppler/cMap (Debian's poppler-data), which
holds Adobe's cmap-resources unmodified. Output is deterministic for a given
input, and the script prints each source CMap's %%Version so the commit can
record what the tables were built from.

Binary format, zlib-compressed, all integers little-endian:

    b"CIDU" u8 version=1
    u32 n_cids                       # max CID + 1
    n_cids x { u8 len, len bytes }   # UTF-8 text for the CID; len 0 = none
    u32 n_pairs
    n_pairs x { u32 code point, u16 CID }   # sorted by code point
"""

import re
import struct
import sys
import unicodedata
import zlib
from pathlib import Path

ORDERINGS = {
    "Japan1": "UniJIS-UTF32-H",
    "CNS1": "UniCNS-UTF32-H",
    "GB1": "UniGB-UTF32-H",
    "Korea1": "UniKS-UTF32-H",
}

OUT_DIR = Path(__file__).resolve().parent.parent / "crates" / "stet-fonts" / "src"

HEX = r"<([0-9A-Fa-f]+)>"


def body(path):
    """The CMap text with comments stripped, and its %%Version line."""
    raw = path.read_bytes().decode("latin-1")
    version = re.search(r"^%%Version:\s*(\S+)", raw, re.M)
    return re.sub(r"%[^\n]*", "", raw), version.group(1) if version else "?"


def sections(text, kind):
    """Yield (declared_count, section_text) for every `N begin<kind>`."""
    pattern = rf"(\d+)\s+begin{kind}(.*?)end{kind}"
    for m in re.finditer(pattern, text, re.S):
        yield int(m.group(1)), m.group(2)


def fail(msg):
    sys.exit(f"gen_cid_unicode_tables: {msg}")


def parse_ucs2(path):
    """CID -> text from a CMapType 2 CID-to-Unicode CMap."""
    text, version = body(path)
    out = {}
    for count, sec in sections(text, "bfchar"):
        entries = re.findall(rf"{HEX}\s*{HEX}", sec)
        if len(entries) != count:
            fail(f"{path}: bfchar declares {count} entries, found {len(entries)}")
        for src, dst in entries:
            out[int(src, 16)] = bytes.fromhex(dst)
    for count, sec in sections(text, "bfrange"):
        if "[" in sec:
            fail(f"{path}: array bfrange destinations are not handled")
        entries = re.findall(rf"{HEX}\s*{HEX}\s*{HEX}", sec)
        if len(entries) != count:
            fail(f"{path}: bfrange declares {count} entries, found {len(entries)}")
        for lo, hi, dst in entries:
            lo, hi, base, width = int(lo, 16), int(hi, 16), int(dst, 16), len(dst) // 2
            for i in range(hi - lo + 1):
                out[lo + i] = (base + i).to_bytes(width, "big")
    decoded = {}
    for cid, dst in out.items():
        try:
            text = dst.decode("utf-16-be")
        except UnicodeDecodeError:
            fail(f"{path}: CID {cid} destination {dst.hex()} is not UTF-16")
        # Adobe writes U+FFFD for a CID with no Unicode — .notdef, and a few
        # unencoded glyphs — and Korea1 maps one CID to a control character.
        # Neither says what the glyph is: as text they would put junk into
        # extracted text, and as a glyph-selection fallback they would draw
        # a substitute font's replacement glyph where nothing belongs.
        if any(c == "\ufffd" or unicodedata.category(c) == "Cc" for c in text):
            continue
        decoded[cid] = text
    return decoded, version


def parse_utf32(path):
    """Code point -> CID from a Uni*-UTF32-H CMap."""
    text, version = body(path)
    out = {}
    for count, sec in sections(text, "cidchar"):
        entries = re.findall(rf"{HEX}\s*(\d+)", sec)
        if len(entries) != count:
            fail(f"{path}: cidchar declares {count} entries, found {len(entries)}")
        for src, cid in entries:
            out[int(src, 16)] = int(cid)
    for count, sec in sections(text, "cidrange"):
        entries = re.findall(rf"{HEX}\s*{HEX}\s*(\d+)", sec)
        if len(entries) != count:
            fail(f"{path}: cidrange declares {count} entries, found {len(entries)}")
        for lo, hi, cid in entries:
            lo, hi, cid = int(lo, 16), int(hi, 16), int(cid)
            for i in range(hi - lo + 1):
                out[lo + i] = cid + i
    # notdefrange maps control characters to CID 1; they are not the
    # characters CID 1 stands for, so they are deliberately not read.
    return out, version


def encode(text_by_cid, cid_by_cp):
    n_cids = max(text_by_cid) + 1
    if n_cids > 0x10000 or max(cid_by_cp.values()) > 0xFFFF:
        fail("CIDs no longer fit in u16")
    out = bytearray(b"CIDU\x01")
    out += struct.pack("<I", n_cids)
    for cid in range(n_cids):
        utf8 = text_by_cid.get(cid, "").encode("utf-8")
        if len(utf8) > 255:
            fail(f"CID {cid} text is {len(utf8)} bytes")
        out.append(len(utf8))
        out += utf8
    out += struct.pack("<I", len(cid_by_cp))
    for cp in sorted(cid_by_cp):
        out += struct.pack("<IH", cp, cid_by_cp[cp])
    return zlib.compress(bytes(out), 9)


def main():
    cmap_dir = Path(sys.argv[1] if len(sys.argv) > 1 else "/usr/share/poppler/cMap")
    for ordering, uni_name in ORDERINGS.items():
        ucs2_path = cmap_dir / f"Adobe-{ordering}" / f"Adobe-{ordering}-UCS2"
        uni_path = cmap_dir / f"Adobe-{ordering}" / uni_name
        text_by_cid, ucs2_version = parse_ucs2(ucs2_path)
        cid_by_cp, uni_version = parse_utf32(uni_path)
        data = encode(text_by_cid, cid_by_cp)
        out = OUT_DIR / f"cid_{ordering.lower()}.bin"
        out.write_bytes(data)
        print(
            f"{out.name}: Adobe-{ordering}-UCS2 {ucs2_version} ({len(text_by_cid)} CIDs), "
            f"{uni_name} {uni_version} ({len(cid_by_cp)} code points), {len(data)} bytes"
        )


if __name__ == "__main__":
    main()
