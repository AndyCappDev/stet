// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Vertical writing (WMode 1) with CIDFonts: where glyphs are drawn and how
//! far the current point moves.
//!
//! PLRM 5.4: in writing mode 1 the current point is a glyph's origin 1, and
//! its outline is drawn from origin 0 = origin 1 − v. With no `Metrics2`,
//! v = (w0 / 2, 880) and the advance is (0, −1000), in 1000-unit text space.
//!
//! Every glyph here is a 1000 × 1000 square from its origin 0, shown at 12
//! points at (300, 700) on a 792-point page at 72 dpi (device y flipped).
//! PLRM puts it at x 294..306 (v's x is 6 points), and y 689.44..701.44
//! (v's y is 10.56 points) — device y 90.56..102.56 — and moves the
//! current point to (300, 688).

use stet::{DisplayElement, Interpreter};
use stet_fonts::geometry::PathSegment;

/// A CFF CIDFont (`/CIDC`) whose CID 1 is `1000 0 0 rmoveto 1000 0
/// rlineto 0 1000 rlineto -1000 0 rlineto endchar`: a square 1000 wide.
const CFF_CIDFONT: &str = r#"
/CIDC <<
  /CIDFontType 0 /CIDFontName /CIDC /FontType 9
  /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >>
  /FontMatrix [0.001 0 0 0.001 0 0] /FontBBox [0 0 1000 1000]
  /CIDCount 2 /DW 1000
  /CharStrings << 1 <1C03E88B8B151C03E88B058B1C03E8051CFC188B050E> >>
>> /CIDFont defineresource pop
"#;

/// A TrueType font with `units_per_em` units per em whose glyph 1 is a
/// square as wide as the em, drawn from (0, 0), and advances one em.
fn square_sfnt(units_per_em: u16) -> Vec<u8> {
    let em = units_per_em as i16;
    let mut head = vec![0u8; 54];
    head[12..16].copy_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
    head[18..20].copy_from_slice(&units_per_em.to_be_bytes());
    let mut hhea = vec![0u8; 36];
    hhea[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    hhea[34..36].copy_from_slice(&2u16.to_be_bytes());
    let mut hmtx = Vec::new();
    for advance in [0u16, units_per_em] {
        hmtx.extend(advance.to_be_bytes());
        hmtx.extend(0i16.to_be_bytes());
    }
    let mut maxp = vec![0u8; 6];
    maxp[0..4].copy_from_slice(&0x0000_5000u32.to_be_bytes());
    maxp[4..6].copy_from_slice(&2u16.to_be_bytes());
    // Glyph 1: one contour, four on-curve points with 16-bit deltas.
    let mut glyf = Vec::new();
    for v in [1i16, 0, 0, em, em] {
        glyf.extend(v.to_be_bytes()); // contours, xMin, yMin, xMax, yMax
    }
    glyf.extend(3u16.to_be_bytes()); // endPtsOfContours
    glyf.extend(0u16.to_be_bytes()); // instructionLength
    glyf.extend([1u8; 4]); // flags: on curve
    for dx in [0i16, em, 0, -em] {
        glyf.extend(dx.to_be_bytes());
    }
    for dy in [0i16, 0, em, 0] {
        glyf.extend(dy.to_be_bytes());
    }
    let glyf_len = glyf.len() as u16;
    let mut loca = Vec::new();
    for offset in [0u16, 0, glyf_len / 2] {
        loca.extend(offset.to_be_bytes());
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

/// A TrueType CIDFont (`/CIDT`) over [`square_sfnt`] at 2048 units per em.
fn truetype_cidfont() -> String {
    let hex: String = square_sfnt(2048)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect();
    format!(
        r#"
/CIDT <<
  /CIDFontType 2 /CIDFontName /CIDT /FontType 42
  /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >>
  /FontMatrix [1 0 0 1 0 0] /FontBBox [0 0 1 1]
  /CIDCount 2 /GDBytes 2 /CIDMap 0 /Encoding [] /CharStrings << /.notdef 0 >>
  /sfnts [<{hex}>]
>> /CIDFont defineresource pop
"#
    )
}

/// Run `body` with `/V` set to CIDFont `cidfont` through Identity-V at 12
/// points, returning the device bounding boxes of every fill in order.
fn fills(fonts: &str, cidfont: &str, body: &str) -> Vec<[f64; 4]> {
    let ps = format!(
        "%!PS\n{fonts}\n/V /Identity-V [/{cidfont} /CIDFont findresource] composefont pop\n\
         /V findfont 12 scalefont setfont\n{body}\nshowpage\n"
    );
    let mut interp = Interpreter::builder().build();
    let pages = interp
        .render_to_display_list(ps.as_bytes(), 72.0)
        .expect("renders");
    pages[0]
        .display_list
        .elements()
        .iter()
        .filter_map(|e| match e {
            DisplayElement::Fill { path, .. } => Some(bbox(&path.segments)),
            _ => None,
        })
        .collect()
}

fn bbox(segments: &[PathSegment]) -> [f64; 4] {
    let mut b = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for s in segments {
        if let PathSegment::MoveTo(x, y) | PathSegment::LineTo(x, y) = s {
            b = [b[0].min(*x), b[1].min(*y), b[2].max(*x), b[3].max(*y)];
        }
    }
    b
}

/// A PostScript fragment marking the current point with a tiny square,
/// read back by [`marker_point`].
const MARK: &str = "currentpoint 0.5 0.5 rectfill";

/// The user-space point a [`MARK`] square was drawn at.
fn marker_point(device_box: [f64; 4]) -> (f64, f64) {
    (device_box[0], 792.0 - device_box[3])
}

fn close(a: [f64; 4], b: [f64; 4]) -> bool {
    a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-6)
}

/// PLRM's placement of the square, in device space.
const EXPECTED: [f64; 4] = [294.0, 90.56, 306.0, 102.56];

fn both_fonts() -> [(String, &'static str); 2] {
    [
        (CFF_CIDFONT.to_string(), "CIDC"),
        (truetype_cidfont(), "CIDT"),
    ]
}

#[test]
fn show_draws_vertical_glyphs_from_origin_0() {
    for (fonts, cidfont) in both_fonts() {
        let boxes = fills(
            &fonts,
            cidfont,
            &format!("300 700 moveto <0001> show {MARK}"),
        );
        assert_eq!(boxes.len(), 2, "{cidfont}");
        assert!(close(boxes[0], EXPECTED), "{cidfont}: {:?}", boxes[0]);
        // One em down the column, whatever the font's units per em.
        assert_eq!(marker_point(boxes[1]), (300.0, 688.0), "{cidfont}");
    }
}

#[test]
fn xyshow_draws_vertical_glyphs_from_origin_0() {
    for (fonts, cidfont) in both_fonts() {
        let boxes = fills(&fonts, cidfont, "300 700 moveto <0001> [0 -20] xyshow");
        assert_eq!(boxes.len(), 1, "{cidfont}");
        assert!(close(boxes[0], EXPECTED), "{cidfont}: {:?}", boxes[0]);
    }
}

#[test]
fn charpath_places_and_advances_vertical_glyphs() {
    for (fonts, cidfont) in both_fonts() {
        let boxes = fills(
            &fonts,
            cidfont,
            &format!(
                "300 700 moveto <0001> false charpath currentpoint \
                 /y exch def /x exch def fill x y moveto {MARK}"
            ),
        );
        assert_eq!(boxes.len(), 2, "{cidfont}");
        assert!(close(boxes[0], EXPECTED), "{cidfont}: {:?}", boxes[0]);
        assert_eq!(marker_point(boxes[1]), (300.0, 688.0), "{cidfont}");
    }
}

#[test]
fn stringwidth_of_vertical_text_is_one_em_per_glyph() {
    for (fonts, cidfont) in both_fonts() {
        let boxes = fills(
            &fonts,
            cidfont,
            &format!("100 400 moveto <00010001> stringwidth rmoveto {MARK}"),
        );
        assert_eq!(boxes.len(), 1, "{cidfont}");
        assert_eq!(marker_point(boxes[0]), (100.0, 376.0), "{cidfont}");
    }
}
