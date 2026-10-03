// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Fixtures shared by integration tests.

/// A TrueType font of three glyphs at 1000 units per em: 0, an empty
/// `.notdef` advancing 250; 1, a full-em square advancing 1000; 2, a
/// half-em-wide bar advancing 500. It has no `cmap`.
#[allow(dead_code)]
pub fn sfnt() -> Vec<u8> {
    sfnt_with(None)
}

/// [`sfnt`] with a Macintosh Roman `cmap` (format 0) mapping each
/// `(code, glyph)` — what a PDF reader maps a simple TrueType font's codes
/// through.
#[allow(dead_code)]
pub fn sfnt_with_cmap(map: &[(u8, u8)]) -> Vec<u8> {
    let mut cmap = Vec::new();
    cmap.extend(0u16.to_be_bytes()); // version
    cmap.extend(1u16.to_be_bytes()); // one subtable
    cmap.extend(1u16.to_be_bytes()); // platform: Macintosh
    cmap.extend(0u16.to_be_bytes()); // encoding: Roman
    cmap.extend(12u32.to_be_bytes()); // offset
    cmap.extend(0u16.to_be_bytes()); // format 0
    cmap.extend(262u16.to_be_bytes()); // length
    cmap.extend(0u16.to_be_bytes()); // language
    let mut glyphs = [0u8; 256];
    for &(code, glyph) in map {
        glyphs[code as usize] = glyph;
    }
    cmap.extend(glyphs);
    sfnt_with(Some(cmap))
}

fn sfnt_with(cmap: Option<Vec<u8>>) -> Vec<u8> {
    let mut head = vec![0u8; 54];
    head[12..16].copy_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
    head[18..20].copy_from_slice(&1000u16.to_be_bytes());
    let mut hhea = vec![0u8; 36];
    hhea[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    hhea[34..36].copy_from_slice(&3u16.to_be_bytes());
    let mut hmtx = Vec::new();
    for advance in [250u16, 1000, 500] {
        hmtx.extend(advance.to_be_bytes());
        hmtx.extend(0i16.to_be_bytes());
    }
    let mut maxp = vec![0u8; 6];
    maxp[0..4].copy_from_slice(&0x0000_5000u32.to_be_bytes());
    maxp[4..6].copy_from_slice(&3u16.to_be_bytes());

    // One rectangle contour of `w` by 1000, four on-curve points.
    let rect = |w: i16| {
        let mut g = Vec::new();
        for v in [1i16, 0, 0, w, 1000] {
            g.extend(v.to_be_bytes()); // contours, xMin, yMin, xMax, yMax
        }
        g.extend(3u16.to_be_bytes()); // endPtsOfContours
        g.extend(0u16.to_be_bytes()); // instructionLength
        g.extend([1u8; 4]); // flags: on curve
        for dx in [0i16, w, 0, -w] {
            g.extend(dx.to_be_bytes());
        }
        for dy in [0i16, 0, 1000, 0] {
            g.extend(dy.to_be_bytes());
        }
        g
    };
    let square = rect(1000);
    let bar = rect(500);
    let mut glyf = square.clone();
    glyf.extend(&bar);
    let mut loca = Vec::new();
    for offset in [0, 0, square.len(), glyf.len()] {
        loca.extend((offset as u16 / 2).to_be_bytes());
    }

    // Sorted by tag, as the table directory must be.
    let mut tables: Vec<(&[u8; 4], &[u8])> = Vec::new();
    if let Some(cmap) = &cmap {
        tables.push((b"cmap", cmap));
    }
    tables.extend([
        (b"glyf", glyf.as_slice()),
        (b"head", &head),
        (b"hhea", &hhea),
        (b"hmtx", &hmtx),
        (b"loca", &loca),
        (b"maxp", &maxp),
    ]);
    let mut font = Vec::new();
    font.extend(0x0001_0000u32.to_be_bytes());
    font.extend((tables.len() as u16).to_be_bytes());
    font.extend([0u8; 6]);
    let mut offset = 12 + 16 * tables.len();
    let mut bodies = Vec::new();
    for (tag, body) in tables {
        font.extend(tag);
        font.extend(0u32.to_be_bytes());
        font.extend((offset as u32).to_be_bytes());
        font.extend((body.len() as u32).to_be_bytes());
        let mut padded = body.to_vec();
        padded.resize(body.len().div_ceil(4) * 4, 0);
        offset += padded.len();
        bodies.extend(padded);
    }
    font.extend(bodies);
    font
}

/// `bytes` as a hex string, for a PostScript `<…>` literal.
#[allow(dead_code)]
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}
