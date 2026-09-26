// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! CIDFontType 0 fonts whose glyphs are Type 1 charstrings reached through a
//! CID map — the form `CIDInit`'s `StartData` builds (Adobe TN 5014), and
//! how pdftops writes CFF-based CID fonts.
//!
//! stet used to read the glyph data only to skip it, so `show` raised
//! `invalidfont` and all such text vanished (161 corpus files). The font
//! below exercises each part of the format: two FDs, one with a `FontMatrix`
//! that doubles its glyphs; an unencrypted (`lenIV -1`) and an encrypted
//! (`lenIV` 4) charstring; a subroutine reached through `SubrMapOffset`; and
//! a CID whose map entry is empty.
//!
//! Every glyph is a 100-unit square drawn from (100, 100) that advances 500
//! units, so through FD 1 it is a 200-unit square from (200, 200) advancing
//! 1000. Shown at 100 points, a unit is 0.1 point.

use stet::{DisplayElement, Interpreter};
use stet_fonts::geometry::PathSegment;

/// A Type 1 charstring number.
fn num(v: i32) -> Vec<u8> {
    match v {
        -107..=107 => vec![(v + 139) as u8],
        108..=1131 => {
            let v = v - 108;
            vec![(v / 256 + 247) as u8, (v % 256) as u8]
        }
        -1131..=-108 => {
            let v = -v - 108;
            vec![(v / 256 + 251) as u8, (v % 256) as u8]
        }
        _ => unreachable!("not needed here"),
    }
}

fn ops(parts: &[&[i32]]) -> Vec<u8> {
    // Each part: operands followed by one operator byte.
    let mut out = Vec::new();
    for part in parts {
        let (&op, operands) = part.split_last().unwrap();
        for &v in operands {
            out.extend(num(v));
        }
        out.push(op as u8);
    }
    out
}

/// Type 1 charstring encryption with four leading random bytes (lenIV 4).
fn encrypt(plain: &[u8]) -> Vec<u8> {
    let mut r: u32 = 4330;
    let mut out = Vec::new();
    for &p in [0u8; 4].iter().chain(plain) {
        let c = (p as u32 ^ (r >> 8)) as u8;
        r = ((c as u32 + r) * 52845 + 22719) & 0xFFFF;
        out.push(c);
    }
    out
}

const HSBW: i32 = 13;
const RMOVETO: i32 = 21;
const RLINETO: i32 = 5;
const CLOSEPATH: i32 = 9;
const ENDCHAR: i32 = 14;
const CALLSUBR: i32 = 10;
const RETURN: i32 = 11;

/// The glyph data and header of `/T1CID`: CID 1 through FD 0 (identity,
/// unencrypted), CID 2 through FD 1 (doubled, encrypted, drawn by a
/// subroutine), CID 3 empty.
fn font(binary: bool) -> Vec<u8> {
    let square: &[&[i32]] = &[
        &[0, 500, HSBW],
        &[100, 100, RMOVETO],
        &[100, 0, RLINETO],
        &[0, 100, RLINETO],
        &[-100, 0, RLINETO],
        &[CLOSEPATH],
        &[ENDCHAR],
    ];
    let cid1 = ops(square);
    let subr = encrypt(&ops(&[
        &[100, 0, RLINETO],
        &[0, 100, RLINETO],
        &[-100, 0, RLINETO],
        &[RETURN],
    ]));
    let cid2 = encrypt(&ops(&[
        &[0, 500, HSBW],
        &[100, 100, RMOVETO],
        &[0, CALLSUBR],
        &[CLOSEPATH],
        &[ENDCHAR],
    ]));

    // CID map: CIDCount 4 + 1 entries of FDBytes 1 + GDBytes 2.
    let map_len = 5 * 3;
    let subr_map = map_len; // one subroutine: two 2-byte offsets
    let subr_at = subr_map + 4;
    let cid1_at = subr_at + subr.len();
    let cid2_at = cid1_at + cid1.len();
    let end = cid2_at + cid2.len();
    let mut data = Vec::new();
    for (fd, off) in [
        (0u8, cid1_at),
        (0, cid1_at),
        (1, cid2_at),
        (0, end),
        (0, end),
    ] {
        data.push(fd);
        data.extend((off as u16).to_be_bytes());
    }
    data.extend((subr_at as u16).to_be_bytes());
    data.extend((cid1_at as u16).to_be_bytes());
    data.extend(&subr);
    data.extend(&cid1);
    data.extend(&cid2);

    let mut ps = format!(
        "/CIDInit /ProcSet findresource begin\n20 dict begin\n\
         /CIDFontName /T1CID def /CIDFontType 0 def\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> def\n\
         /FontMatrix [0.001 0 0 0.001 0 0] def /FontBBox [0 0 1000 1000] def\n\
         /CIDCount 4 def /FDBytes 1 def /GDBytes 2 def /CIDMapOffset 0 def\n\
         /FDArray [\n\
           << /FontType 1 /FontMatrix [1 0 0 1 0 0] /Private << /lenIV -1 >> >>\n\
           << /FontType 1 /FontMatrix [2 0 0 2 0 0]\n\
              /Private << /SubrMapOffset {subr_map} /SDBytes 2 /SubrCount 1 >> >>\n\
         ] def\n"
    )
    .into_bytes();
    if binary {
        ps.extend(format!("(Binary) {} StartData ", data.len()).bytes());
        ps.extend(&data);
    } else {
        ps.extend(format!("(Hex) {} StartData\n", data.len()).bytes());
        for b in &data {
            ps.extend(format!("{b:02X}").bytes());
        }
        ps.push(b'>');
    }
    ps.extend(b"\n/F /Identity-H [/T1CID /CIDFont findresource] composefont pop\n");
    ps
}

/// Device bounding boxes of the fills `body` draws with `/F` at 100 points
/// on a 400-point-high page at 72 dpi (device y is flipped).
fn fills_with(binary: bool, body: &str) -> Vec<[f64; 4]> {
    let mut ps = b"%!PS\n<< /PageSize [600 400] >> setpagedevice\n".to_vec();
    ps.extend(font(binary));
    ps.extend(format!("/F findfont 100 scalefont setfont\n{body}\nshowpage\n").bytes());
    let mut interp = Interpreter::builder().build();
    let pages = interp.render_to_display_list(&ps, 72.0).expect("renders");
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

fn fills(body: &str) -> Vec<[f64; 4]> {
    fills_with(false, body)
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

fn close(a: [f64; 4], b: [f64; 4]) -> bool {
    a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-6)
}

/// A tiny square at the current point, read back by [`marker_x`].
const MARK: &str = "currentpoint 0.5 0.5 rectfill";

fn marker_x(device_box: [f64; 4]) -> f64 {
    device_box[0]
}

/// CID 1 at (10, 100): a 10-point square from (20, 110), device y flipped.
const CID1_AT_10_100: [f64; 4] = [20.0, 280.0, 30.0, 290.0];
/// CID 2 at (60, 100): FD 1 doubles it — 20 points from (80, 120).
const CID2_AT_60_100: [f64; 4] = [80.0, 260.0, 100.0, 280.0];

#[test]
fn show_draws_each_fd_with_its_own_matrix_encryption_and_subrs() {
    let boxes = fills(&format!("10 100 moveto <00010002> show {MARK}"));
    assert_eq!(boxes.len(), 3, "{boxes:?}");
    assert!(close(boxes[0], CID1_AT_10_100), "{:?}", boxes[0]);
    assert!(close(boxes[1], CID2_AT_60_100), "{:?}", boxes[1]);
    // 50 points for CID 1, 100 for CID 2.
    assert_eq!(marker_x(boxes[2]), 160.0);
}

#[test]
fn binary_glyph_data_reads_like_hex() {
    let boxes = fills_with(true, "10 100 moveto <00010002> show");
    assert_eq!(boxes.len(), 2, "{boxes:?}");
    assert!(close(boxes[0], CID1_AT_10_100), "{:?}", boxes[0]);
    assert!(close(boxes[1], CID2_AT_60_100), "{:?}", boxes[1]);
}

/// An empty map entry is a missing glyph: nothing drawn, and the default
/// width (`DW`, 1000 when absent) advanced — as for a missing CFF CID.
#[test]
fn missing_cid_draws_nothing_and_advances_the_default_width() {
    let boxes = fills(&format!("10 100 moveto <0003> show {MARK}"));
    assert_eq!(boxes.len(), 1, "{boxes:?}");
    assert_eq!(marker_x(boxes[0]), 110.0);
}

#[test]
fn stringwidth_uses_the_charstring_widths() {
    let boxes = fills(&format!(
        "10 100 moveto <00010002> stringwidth rmoveto {MARK}"
    ));
    assert_eq!(marker_x(boxes[0]), 160.0);
}

#[test]
fn charpath_appends_the_outlines() {
    let boxes = fills(&format!(
        "10 100 moveto <00010002> false charpath currentpoint \
         /y exch def /x exch def fill x y moveto {MARK}"
    ));
    assert_eq!(boxes.len(), 2, "{boxes:?}");
    assert_eq!(boxes[0], [20.0, 260.0, 100.0, 290.0]);
    assert_eq!(marker_x(boxes[1]), 160.0);
}

/// pdftops shows each run with its PDF widths through `xyshow`.
#[test]
fn xyshow_draws_the_glyphs() {
    let boxes = fills("10 100 moveto <00010002> [50 0 100 0] xyshow");
    assert_eq!(boxes.len(), 2, "{boxes:?}");
    assert!(close(boxes[0], CID1_AT_10_100), "{:?}", boxes[0]);
    assert!(close(boxes[1], CID2_AT_60_100), "{:?}", boxes[1]);
}
