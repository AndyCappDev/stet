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
//! These fonts reach PDF output as embedded CFF (`/CIDFontType0C`); the
//! last tests check that the PDF draws what the interpreter draws.
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

/// The glyph data and header of `/T1CID`: CID 0 an empty `.notdef`
/// advancing 250, CID 1 through FD 0 (identity, unencrypted), CID 2 through
/// FD 1 (doubled, encrypted, drawn by a subroutine), CID 3 empty.
fn font(binary: bool) -> Vec<u8> {
    let notdef = ops(&[&[0, 250, HSBW], &[ENDCHAR]]);
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
    let cid0_at = subr_at + subr.len();
    let cid1_at = cid0_at + notdef.len();
    let cid2_at = cid1_at + cid1.len();
    let end = cid2_at + cid2.len();
    let mut data = Vec::new();
    for (fd, off) in [
        (0u8, cid0_at),
        (0, cid1_at),
        (1, cid2_at),
        (0, end),
        (0, end),
    ] {
        data.push(fd);
        data.extend((off as u16).to_be_bytes());
    }
    data.extend((subr_at as u16).to_be_bytes());
    data.extend((cid0_at as u16).to_be_bytes());
    data.extend(&subr);
    data.extend(&notdef);
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

/// An empty map entry is a missing glyph, shown as CID 0 (Adobe TN 5014
/// §2.5): here an empty `.notdef`, so nothing drawn and 250 units advanced.
#[test]
fn missing_cid_shows_cid_0() {
    let boxes = fills(&format!("10 100 moveto <0003> show {MARK}"));
    assert_eq!(boxes.len(), 1, "{boxes:?}");
    assert_eq!(marker_x(boxes[0]), 35.0);
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

/// A CMap can send codes to a CIDFont and to a base font in one string,
/// each by the font number of its `usefont` (PLRM 5.11.4). Here the CIDFont
/// is font 1 and Helvetica font 0: `A` and `C` are CIDs 1 and 2, `B` is
/// Helvetica's `B`, 66.7 points wide at 100.
#[test]
fn one_string_shows_cids_and_base_font_glyphs() {
    let boxes = fills(&format!(
        "/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
         /CMapName /Mixed def /CMapType 1 def\n\
         1 begincodespacerange <00> <FF> endcodespacerange\n\
         0 usefont 1 beginbfchar <42> /B endbfchar\n\
         1 usefont 2 begincidchar <41> 1 <43> 2 endcidchar\n\
         endcmap CMapName currentdict /CMap defineresource pop end end\n\
         /M /Mixed [/Helvetica findfont /T1CID /CIDFont findresource] composefont\n\
         100 scalefont setfont\n\
         10 100 moveto (ABC) show {MARK}\n\
         10 100 moveto (ABC) stringwidth rmoveto {MARK}"
    ));
    assert_eq!(boxes.len(), 5, "{boxes:?}");
    assert!(close(boxes[0], CID1_AT_10_100), "{:?}", boxes[0]);
    // Helvetica's B from x = 60: its left side bearing is 79 units.
    assert!((boxes[1][0] - 67.9).abs() < 0.05, "{:?}", boxes[1]);
    // CID 2 after 50 + 66.7 points: FD 1 doubles it.
    let cid2 = [146.7, 260.0, 166.7, 280.0];
    assert!(
        boxes[2].iter().zip(cid2).all(|(a, b)| (a - b).abs() < 1e-6),
        "{:?}",
        boxes[2]
    );
    assert!((marker_x(boxes[3]) - 226.7).abs() < 1e-6, "{:?}", boxes[3]);
    assert!((marker_x(boxes[4]) - 226.7).abs() < 1e-6, "{:?}", boxes[4]);
}

/// The CMap of [`one_string_shows_cids_and_base_font_glyphs`].
const MIXED: &str = "/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
     /CMapName /Mixed def /CMapType 1 def\n\
     1 begincodespacerange <00> <FF> endcodespacerange\n\
     0 usefont 1 beginbfchar <42> /B endbfchar\n\
     1 usefont 2 begincidchar <41> 1 <43> 2 endcidchar\n\
     endcmap CMapName currentdict /CMap defineresource pop end end\n\
     /M /Mixed [/Helvetica findfont /T1CID /CIDFont findresource] composefont\n\
     100 scalefont setfont\n";

/// pdftops clips text by showing each character from a `cshow` procedure:
/// `1 string dup 0 4 3 roll put show`. With a CIDFont selected the byte
/// can't name the CID, which still draws; with Helvetica the byte is
/// Helvetica's. The procedure gets each glyph's width, and the CIDFont, at
/// the composite font's size, as the current font.
#[test]
fn cshow_shows_cids_and_base_font_glyphs() {
    let boxes = fills(&format!(
        "{MIXED}10 100 moveto\n\
         {{ pop pop 1 string dup 0 4 -1 roll put show }} (ABC) cshow {MARK}\n\
         [ {{ pop exch pop currentfont /CIDFontName known }} (ABC) cshow ] /r exch def\n\
         r 0 get 50 sub abs 0.01 gt {{ 1 0 div }} if r 1 get not {{ 1 0 div }} if\n\
         r 2 get 66.7 sub abs 0.05 gt {{ 1 0 div }} if r 3 get {{ 1 0 div }} if\n\
         r 4 get 100 sub abs 0.01 gt {{ 1 0 div }} if r 5 get not {{ 1 0 div }} if"
    ));
    assert_eq!(boxes.len(), 4, "{boxes:?}");
    assert!(close(boxes[0], CID1_AT_10_100), "{:?}", boxes[0]);
    assert!((boxes[1][0] - 67.9).abs() < 0.05, "{:?}", boxes[1]);
    let cid2 = [146.7, 260.0, 166.7, 280.0];
    assert!(
        boxes[2].iter().zip(cid2).all(|(a, b)| (a - b).abs() < 1e-6),
        "{:?}",
        boxes[2]
    );
    assert!((marker_x(boxes[3]) - 226.7).abs() < 1e-6, "{:?}", boxes[3]);
}

#[test]
fn xshow_places_cids_and_base_font_glyphs() {
    let boxes = fills(&format!(
        "{MIXED}10 100 moveto (ABC) [50 70 100] xshow {MARK}"
    ));
    assert_eq!(boxes.len(), 4, "{boxes:?}");
    assert!(close(boxes[0], CID1_AT_10_100), "{:?}", boxes[0]);
    assert!((boxes[1][0] - 67.9).abs() < 0.05, "{:?}", boxes[1]);
    // CID 2 at 130: FD 1 doubles it, from (150, 120).
    assert!(
        close(boxes[2], [150.0, 260.0, 170.0, 280.0]),
        "{:?}",
        boxes[2]
    );
    assert_eq!(marker_x(boxes[3]), 230.0);
}

// ---------------------------------------------------------------------------
// PDF output
// ---------------------------------------------------------------------------

/// Device bounding boxes of the fills in the PDF stet writes for `ps`, as
/// the PDF reader draws them: the embedded font's glyphs and the markers.
#[cfg(feature = "pdf-output")]
fn pdf_fills(ps: &[u8]) -> Vec<[f64; 4]> {
    let pdf = Interpreter::builder()
        .build()
        .render_to_pdf(ps, 72.0)
        .expect("writes a PDF");
    let text = String::from_utf8_lossy(&pdf);
    assert!(text.contains("/CIDFontType0C"), "an embedded CFF CIDFont");
    let doc = stet_pdf_reader::PdfDocument::from_bytes(&pdf).expect("reads back");
    doc.render_page(0, 72.0)
        .expect("renders")
        .elements()
        .iter()
        .filter_map(|e| match e {
            DisplayElement::Fill { path, .. } => Some(bbox(&path.segments)),
            _ => None,
        })
        .collect()
}

/// The interpreter's fills for the same job, for comparison.
fn ps_fills(ps: &[u8]) -> Vec<[f64; 4]> {
    let mut interp = Interpreter::builder().build();
    let pages = interp.render_to_display_list(ps, 72.0).expect("renders");
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

/// Every fill in `pdf` matches the corresponding one in `ps`.
fn assert_same_fills(ps: &[[f64; 4]], pdf: &[[f64; 4]]) {
    assert_eq!(ps.len(), pdf.len(), "PS {ps:?}\nPDF {pdf:?}");
    for (a, b) in ps.iter().zip(pdf) {
        assert!(
            a.iter().zip(b).all(|(a, b)| (a - b).abs() < 0.01),
            "PS {a:?} PDF {b:?}"
        );
    }
}

/// The GlyphData font becomes a CFF CIDFont: both FDs, the subroutine,
/// and the missing CID, which shows CID 0 and advances its width.
#[cfg(feature = "pdf-output")]
#[test]
fn pdf_output_draws_glyph_data_fonts_as_the_interpreter_does() {
    let mut ps = b"%!PS\n<< /PageSize [600 400] >> setpagedevice\n".to_vec();
    ps.extend(font(false));
    ps.extend(
        format!(
            "/F findfont 100 scalefont setfont\n\
             10 100 moveto <0001000200030001> show {MARK}\nshowpage\n"
        )
        .bytes(),
    );
    let expected = ps_fills(&ps);
    // CID 1, CID 2, then CID 1 again after the 25-point notdef; the marker.
    assert_eq!(expected.len(), 4, "{expected:?}");
    assert_eq!(expected[2][0], 20.0 + 50.0 + 100.0 + 25.0);
    assert_same_fills(&expected, &pdf_fills(&ps));
}

/// A CIDFont that is not a Type 0 font's first descendant reaches PDF
/// output too: a device takes a Type 0 font's glyphs from its first
/// descendant, Helvetica in [`MIXED`], so the CIDs drew nothing there.
#[cfg(feature = "pdf-output")]
#[test]
fn pdf_output_draws_a_cidfont_at_any_font_number() {
    let mut ps = b"%!PS\n<< /PageSize [600 400] >> setpagedevice\n".to_vec();
    ps.extend(font(false));
    ps.extend(
        format!(
            "{MIXED}10 100 moveto (ABC) show {MARK}\n\
             10 200 moveto (ABC) [50 70 100] xshow {MARK}\n\
             10 300 moveto {{ pop pop 1 string dup 0 4 -1 roll put show }} (ABC) cshow\n\
             {MARK}\nshowpage\n"
        )
        .bytes(),
    );
    let expected = ps_fills(&ps);
    // CID 1, Helvetica's B, CID 2 and the marker, three times.
    assert_eq!(expected.len(), 12, "{expected:?}");
    assert_same_fills(&expected, &pdf_fills(&ps));
}

/// A subset CFF CIDFont, as a producer embeds one: its charset maps GIDs
/// to scattered CIDs, and its FDSelect picks between two Font DICTs with
/// different default widths. CID 3 is a 100-unit square at (100, 100),
/// CID 7 a 200-unit one through FD 1, which advances 800; CID 5 is not in
/// the font and shows CID 0, an empty glyph advancing 500.
fn subset_cff_font() -> Vec<u8> {
    use stet_fonts::cff_writer::{CidFont, FontDict, Glyph, write_cid_font};

    fn square(size: i32) -> Vec<u8> {
        // 100 100 rmoveto size 0 rlineto 0 size rlineto -size 0 rlineto endchar
        let n = |v: i32| num(v);
        [
            n(100),
            n(100),
            vec![21],
            n(size),
            n(0),
            n(0),
            n(size),
            n(-size),
            n(0),
            vec![5],
            vec![14],
        ]
        .concat()
    }

    let mut cff = CidFont::new("SubCID", "Adobe", "Identity", 0);
    cff.cid_count = 10;
    let mut fd0 = FontDict::default();
    fd0.private.default_width_x = 500.0;
    let mut fd1 = FontDict::default();
    fd1.private.default_width_x = 800.0;
    cff.font_dicts = vec![fd0, fd1];
    cff.glyphs = vec![
        Glyph::new(0, 0, vec![14]),
        Glyph::new(3, 0, square(100)),
        Glyph::new(7, 1, square(200)),
    ];
    let data = write_cid_font(&cff).unwrap();

    let mut ps = b"%!PS\n<< /PageSize [600 400] >> setpagedevice\n\
        /FontSetInit /ProcSet findresource begin\n"
        .to_vec();
    ps.extend(format!("/SubCID {} StartData ", data.len()).bytes());
    ps.extend(&data);
    ps.extend(
        format!(
            "\n/F /Identity-H [/SubCID /CIDFont findresource] composefont pop\n\
             /F findfont 100 scalefont setfont\n\
             10 100 moveto <000300070005> show {MARK}\nshowpage\n"
        )
        .bytes(),
    );
    ps
}

#[test]
fn a_subset_cff_cidfont_draws_its_cids() {
    let fills = ps_fills(&subset_cff_font());
    assert_eq!(fills.len(), 3, "{fills:?}");
    // CID 3 at x 10: 10 points from 20.
    assert!(
        close(fills[0], [20.0, 280.0, 30.0, 290.0]),
        "{:?}",
        fills[0]
    );
    // CID 7 at x 60: 20 points from 70.
    assert!(
        close(fills[1], [70.0, 270.0, 90.0, 290.0]),
        "{:?}",
        fills[1]
    );
    // 50 + 80 + 50 points on.
    assert_eq!(marker_x(fills[2]), 190.0);
}

#[cfg(feature = "pdf-output")]
#[test]
fn pdf_output_subsets_cff_cidfonts() {
    let ps = subset_cff_font();
    assert_same_fills(&ps_fills(&ps), &pdf_fills(&ps));
}
