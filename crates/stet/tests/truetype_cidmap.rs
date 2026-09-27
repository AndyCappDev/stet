// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A Type 2 (TrueType) CIDFont finds each CID's glyph through its `CIDMap`
//! (PLRM Table 5.17).
//!
//! stet used the CID as the glyph index, which is right only when the map is
//! the identity; pdftops writes non-identity maps for subset fonts, and 22
//! corpus files came out as gibberish. PDF output mapped CIDs through the
//! TrueType `cmap` table instead, which these fonts do not have, so every
//! glyph became `.notdef`.
//!
//! The font here has three glyphs at 1000 units per em: 0, an empty
//! `.notdef` advancing 250; 1, a full-em square advancing 1000; 2, a
//! half-em-wide bar advancing 500. Its map sends CID 5 to glyph 2 and CID 7
//! to glyph 1, and leaves CID 9 at glyph 0.

use stet::{DisplayElement, Interpreter};
use stet_fonts::geometry::PathSegment;

/// A TrueType font with the three glyphs described above.
fn sfnt() -> Vec<u8> {
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

    let tables: [(&[u8; 4], &[u8]); 6] = [
        (b"glyf", &glyf),
        (b"head", &head),
        (b"hhea", &hhea),
        (b"hmtx", &hmtx),
        (b"loca", &loca),
        (b"maxp", &maxp),
    ];
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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

/// The CIDMap for CIDs 0–9 (`GDBytes` 2): CID 5 → 2, CID 7 → 1.
fn cid_map() -> Vec<u8> {
    let mut map = vec![0u8; 20];
    map[11] = 2;
    map[15] = 1;
    map
}

/// A job showing `body` with `/F`, an Identity-H Type 0 font over the
/// CIDFont with the given `CIDMap` value, at 100 points on a 400 × 200
/// page.
fn job(cid_map: &str, body: &str) -> Vec<u8> {
    format!(
        "%!PS\n<< /PageSize [400 200] >> setpagedevice\n\
         /CIDT <<\n\
           /CIDFontType 2 /CIDFontName /CIDT /FontType 42\n\
           /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >>\n\
           /FontMatrix [1 0 0 1 0 0] /FontBBox [0 0 1000 1000]\n\
           /CIDCount 10 /GDBytes 2 /CIDMap {cid_map}\n\
           /Encoding [] /CharStrings << /.notdef 0 >>\n\
           /sfnts [<{}>]\n\
         >> /CIDFont defineresource pop\n\
         /F /Identity-H [/CIDT /CIDFont findresource] composefont pop\n\
         /F findfont 100 scalefont setfont\n{body}\nshowpage\n",
        hex(&sfnt())
    )
    .into_bytes()
}

/// A tiny square at the current point, read back by its left edge.
const MARK: &str = "currentpoint 0.5 0.5 rectfill";

/// Device bounding boxes of the fills in a display list.
fn boxes<'a>(elements: impl Iterator<Item = &'a DisplayElement>) -> Vec<[f64; 4]> {
    elements
        .filter_map(|e| match e {
            DisplayElement::Fill { path, .. } => {
                let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
                for s in &path.segments {
                    if let PathSegment::MoveTo(x, y) | PathSegment::LineTo(x, y) = *s {
                        b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
                    }
                }
                Some(b)
            }
            _ => None,
        })
        .collect()
}

fn ps_fills(ps: &[u8]) -> Vec<[f64; 4]> {
    let mut interp = Interpreter::builder().build();
    let pages = interp.render_to_display_list(ps, 72.0).expect("renders");
    boxes(pages[0].display_list.elements().iter())
}

fn close(a: [f64; 4], b: [f64; 4]) -> bool {
    a.iter().zip(b).all(|(a, b)| (a - b).abs() < 0.01)
}

/// CID 5 (the bar) at x 10, CID 7 (the square) 50 points on, then CID 9
/// (`.notdef`, empty) 100 points further, and the marker 25 after that.
/// Device y runs down from 200.
fn assert_mapped(fills: &[[f64; 4]]) {
    assert_eq!(fills.len(), 3, "{fills:?}");
    assert!(close(fills[0], [10.0, 50.0, 60.0, 150.0]), "{:?}", fills[0]);
    assert!(
        close(fills[1], [60.0, 50.0, 160.0, 150.0]),
        "{:?}",
        fills[1]
    );
    assert_eq!(fills[2][0], 185.0);
}

const SHOW: &str = "10 50 moveto <000500070009> show";

#[test]
fn a_string_cidmap_picks_each_glyph() {
    let map = format!("<{}>", hex(&cid_map()));
    assert_mapped(&ps_fills(&job(&map, &format!("{SHOW} {MARK}"))));
}

#[test]
fn an_array_cidmap_reads_as_one_table() {
    let map = cid_map();
    let split = format!("[<{}> <{}>]", hex(&map[..12]), hex(&map[12..]));
    assert_mapped(&ps_fills(&job(&split, &format!("{SHOW} {MARK}"))));
}

#[test]
fn stringwidth_and_charpath_use_the_map() {
    let map = format!("<{}>", hex(&cid_map()));
    let fills = ps_fills(&job(
        &map,
        &format!(
            "10 50 moveto <000500070009> stringwidth rmoveto {MARK}\n\
             10 50 moveto <000500070009> false charpath fill"
        ),
    ));
    assert_eq!(fills.len(), 2, "{fills:?}");
    assert_eq!(fills[0][0], 185.0);
    // The bar and the square, one path: 10 to 160.
    assert!(
        close(fills[1], [10.0, 50.0, 160.0, 150.0]),
        "{:?}",
        fills[1]
    );
}

/// A CID beyond `CIDCount` shows CID 0's glyph.
#[test]
fn a_cid_past_cidcount_shows_cid_0() {
    let map = format!("<{}>", hex(&cid_map()));
    let fills = ps_fills(&job(&map, &format!("10 50 moveto <0014> show {MARK}")));
    assert_eq!(fills.len(), 1, "{fills:?}");
    assert_eq!(fills[0][0], 35.0);
}

/// PDF output maps CIDs to glyphs through the same `CIDMap`.
#[cfg(feature = "pdf-output")]
#[test]
fn pdf_output_uses_the_map() {
    let map = format!("<{}>", hex(&cid_map()));
    let ps = job(&map, &format!("{SHOW} {MARK}"));
    let pdf = Interpreter::builder()
        .build()
        .render_to_pdf(&ps, 72.0)
        .expect("writes a PDF");
    let doc = stet_pdf_reader::PdfDocument::from_bytes(&pdf).expect("reads back");
    let list = doc.render_page(0, 72.0).expect("renders");
    let fills = boxes(list.elements().iter());
    assert_eq!(fills.len(), 3, "{fills:?}");
    for (pdf, ps) in fills.iter().zip(ps_fills(&ps)) {
        assert!(close(*pdf, ps), "PDF {pdf:?} PS {ps:?}");
    }
}
