// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Vertical writing (WMode 1) with CIDFonts: where glyphs are drawn and how
//! far the current point moves.
//!
//! PLRM 5.4: in writing mode 1 the current point is a glyph's origin 1, and
//! its outline is drawn from origin 0 = origin 1 − v. A glyph gets w1 and v
//! from the CIDFont's `Metrics2` or `CDevProc` (PLRM 5.9.2); one with
//! neither has a single set of metrics, and "the WMode parameter is
//! ignored": it is set as in writing mode 0. Ghostscript does the same.
//! PDF output writes the metrics the interpreter used, and the
//! `pdf_output_*` tests read the PDF back to check the glyphs land where
//! the interpreter drew them.
//!
//! Every glyph here is a 1000 × 1000 square from its origin 0, shown at 12
//! points at (300, 700) on a 792-point page at 72 dpi (device y flipped).
//! With Adobe's `CDevProc` for CJK fonts, v = (w0 / 2, 880) and w1 =
//! (0, −1000) in a 1000-unit em. That puts the square at x 294..306 (v's x
//! is 6 points), and y 689.44..701.44 (v's y is 10.56 points) — device y
//! 90.56..102.56 — and moves the current point to (300, 688).

use stet::{DisplayElement, Interpreter};
use stet_fonts::geometry::PathSegment;

/// A CFF CIDFont (`/CIDC`) loaded through `FontSetInit`, whose CID 1 is
/// `1000 0 0 rmoveto 1000 0 rlineto 0 1000 rlineto -1000 0 rlineto
/// endchar`: a square 1000 wide, the first operand being its width.
fn cff_cidfont() -> Vec<u8> {
    use stet_fonts::cff_writer::{CidFont, FontDict, Glyph, write_cid_font};
    let mut cff = CidFont::new("CIDC", "Adobe", "Identity", 0);
    cff.cid_count = 2;
    cff.font_dicts = vec![FontDict::default()];
    cff.glyphs = vec![
        Glyph::new(0, 0, vec![14]),
        Glyph::new(
            1,
            0,
            [
                &[0x1C, 0x03, 0xE8, 0x8B, 0x8B, 0x15][..],
                &[0x1C, 0x03, 0xE8, 0x8B, 0x05],
                &[0x8B, 0x1C, 0x03, 0xE8, 0x05],
                &[0x1C, 0xFC, 0x18, 0x8B, 0x05, 0x0E],
            ]
            .concat(),
        ),
    ];
    let data = write_cid_font(&cff).expect("writes the CFF");
    let mut ps = b"/FontSetInit /ProcSet findresource begin\n".to_vec();
    ps.extend(format!("/CIDC {} StartData ", data.len()).bytes());
    ps.extend(data);
    ps.extend(b"\nend\n");
    ps
}

/// The same square in a CIDFontType 0 font of Type 1 charstrings reached
/// through a CID map (`/CIDS`), the form `CIDInit`'s `StartData` builds and
/// pdftops writes. CID 1 is the unencrypted (`lenIV -1`) charstring
/// `0 1000 hsbw 0 0 rmoveto 1000 0 rlineto 0 1000 rlineto -1000 0 rlineto
/// closepath endchar`; the map is three `FDBytes 1` + `GDBytes 1` entries,
/// with CID 0 empty.
const TYPE1_CIDFONT: &str = r#"
/CIDInit /ProcSet findresource begin
20 dict begin
/CIDFontName /CIDS def /CIDFontType 0 def
/CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> def
/FontMatrix [0.001 0 0 0.001 0 0] def /FontBBox [0 0 1000 1000] def
/CIDCount 2 def /FDBytes 1 def /GDBytes 1 def /CIDMapOffset 0 def
/FDArray [ << /FontType 1 /FontMatrix [1 0 0 1 0 0]
              /Private << /lenIV -1 >> >> ] def
(Hex) 27 StartData
00060006001B 8BFA7C0D8B8B15FA7C8B058BFA7C05FE7C8B05090E>
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
fn truetype_cidfont() -> Vec<u8> {
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
    .into_bytes()
}

/// A job defining `fonts` and showing `body` with `/V`, CIDFont `cidfont`
/// through Identity-V at 12 points.
fn job(fonts: &[u8], cidfont: &str, body: &str) -> Vec<u8> {
    let mut ps = b"%!PS\n".to_vec();
    ps.extend(fonts);
    ps.extend(
        format!(
            "\n/V /Identity-V [/{cidfont} /CIDFont findresource] composefont pop\n\
             /V findfont 12 scalefont setfont\n{body}\nshowpage\n"
        )
        .bytes(),
    );
    ps
}

/// The device bounding boxes of every fill in `elements`, in order.
fn boxes<'a>(elements: impl Iterator<Item = &'a DisplayElement>) -> Vec<[f64; 4]> {
    elements
        .filter_map(|e| match e {
            DisplayElement::Fill { path, .. } => Some(bbox(&path.segments)),
            _ => None,
        })
        .collect()
}

/// Run `body` as [`job`] does, returning the device bounding boxes of
/// every fill in order.
fn fills(fonts: &[u8], cidfont: &str, body: &str) -> Vec<[f64; 4]> {
    let mut interp = Interpreter::builder().build();
    let pages = interp
        .render_to_display_list(&job(fonts, cidfont, body), 72.0)
        .expect("renders");
    boxes(pages[0].display_list.elements().iter())
}

/// The same fills from the job written as a PDF and read back.
#[cfg(feature = "pdf-output")]
fn pdf_fills(fonts: &[u8], cidfont: &str, body: &str) -> Vec<[f64; 4]> {
    let pdf = Interpreter::builder()
        .build()
        .render_to_pdf(&job(fonts, cidfont, body), 72.0)
        .expect("writes a PDF");
    let doc = stet_pdf_reader::PdfDocument::from_bytes(&pdf).expect("reads back");
    boxes(doc.render_page(0, 72.0).expect("renders").elements().iter())
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

/// The square with Adobe's `CDevProc`, in device space.
const EXPECTED: [f64; 4] = [294.0, 90.56, 306.0, 102.56];

/// The three CIDFonts, none with writing-mode-1 metrics, each with its
/// glyph-space units to the em: 1000 for the CIDFontType 0 fonts, 1 for the
/// TrueType one, whose glyph space is the em.
fn cid_fonts() -> [(Vec<u8>, &'static str, f64); 3] {
    [
        (cff_cidfont(), "CIDC", 1000.0),
        (truetype_cidfont(), "CIDT", 1.0),
        (TYPE1_CIDFONT.as_bytes().to_vec(), "CIDS", 1000.0),
    ]
}

/// PostScript redefining CIDFont `cidfont` with the definitions `entries`
/// added.
fn redefine(cidfont: &str, entries: &str) -> String {
    format!(
        "\n/{cidfont} /CIDFont findresource dup length 4 add dict copy begin\n\
         {entries}\ncurrentdict end /{cidfont} exch /CIDFont defineresource pop\n"
    )
}

/// Adobe's `CDevProc` for CJK CIDFonts, for a glyph space of `em` units to
/// the em: w1 = (0, −em), v = (w0 / 2, 0.88 em).
fn std_cdevproc(em: f64) -> String {
    format!(
        "/CDevProc {{pop pop pop pop pop 0 {} 7 index 2 div {}}} def",
        -em,
        880.0 * em / 1000.0
    )
}

/// The three CIDFonts with [`std_cdevproc`].
fn vertical_fonts() -> Vec<(Vec<u8>, &'static str, f64)> {
    cid_fonts()
        .into_iter()
        .map(|(mut fonts, cidfont, em)| {
            fonts.extend(redefine(cidfont, &std_cdevproc(em)).bytes());
            (fonts, cidfont, em)
        })
        .collect()
}

#[test]
fn show_draws_vertical_glyphs_from_origin_0() {
    for (fonts, cidfont, _) in vertical_fonts() {
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
    for (fonts, cidfont, _) in vertical_fonts() {
        let boxes = fills(&fonts, cidfont, "300 700 moveto <0001> [0 -20] xyshow");
        assert_eq!(boxes.len(), 1, "{cidfont}");
        assert!(close(boxes[0], EXPECTED), "{cidfont}: {:?}", boxes[0]);
    }
}

#[test]
fn charpath_places_and_advances_vertical_glyphs() {
    for (fonts, cidfont, _) in vertical_fonts() {
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
    for (fonts, cidfont, _) in vertical_fonts() {
        let boxes = fills(
            &fonts,
            cidfont,
            &format!("100 400 moveto <00010001> stringwidth rmoveto {MARK}"),
        );
        assert_eq!(boxes.len(), 1, "{cidfont}");
        assert_eq!(marker_point(boxes[0]), (100.0, 376.0), "{cidfont}");
    }
}

/// Two squares along a line from (300, 700), and the current point after.
fn assert_horizontal(cidfont: &str, boxes: &[[f64; 4]]) {
    assert_eq!(boxes.len(), 3, "{cidfont}: {boxes:?}");
    assert!(
        close(boxes[0], [300.0, 80.0, 312.0, 92.0]),
        "{cidfont}: {boxes:?}"
    );
    assert!(
        close(boxes[1], [312.0, 80.0, 324.0, 92.0]),
        "{cidfont}: {boxes:?}"
    );
    assert_eq!(marker_point(boxes[2]), (324.0, 700.0), "{cidfont}");
}

/// Without `Metrics2` or `CDevProc` a glyph has one set of metrics, and
/// writing mode 1 sets it as mode 0 does. `DW2`, PDF's default vertical
/// metrics, is not a PostScript CIDFont key.
#[test]
fn without_vertical_metrics_the_writing_mode_is_ignored() {
    let body = format!("300 700 moveto <00010001> show {MARK}");
    for (fonts, cidfont, _) in cid_fonts() {
        assert_horizontal(cidfont, &fills(&fonts, cidfont, &body));
        let mut with_dw2 = fonts.clone();
        with_dw2.extend(redefine(cidfont, "/DW2 [880 -1000] def").bytes());
        assert_horizontal(cidfont, &fills(&with_dw2, cidfont, &body));
    }
}

/// `Metrics2` gives CID 1 w1 = (0, −1.5 em) and v = (0.5 em, 1 em).
fn with_metrics2(fonts: &[u8], cidfont: &str, em: f64) -> Vec<u8> {
    let mut fonts = fonts.to_vec();
    let entry = format!(
        "/Metrics2 << 1 [0 {} {} {}] >> def",
        -1.5 * em,
        0.5 * em,
        em
    );
    fonts.extend(redefine(cidfont, &entry).bytes());
    fonts
}

#[test]
fn metrics2_sets_glyphs_vertically() {
    for (fonts, cidfont, em) in cid_fonts() {
        let boxes = fills(
            &with_metrics2(&fonts, cidfont, em),
            cidfont,
            &format!("300 700 moveto <00010001> show {MARK}"),
        );
        assert_eq!(boxes.len(), 3, "{cidfont}: {boxes:?}");
        // v = (6, 12) points, w1 = (0, −18).
        assert!(
            close(boxes[0], [294.0, 92.0, 306.0, 104.0]),
            "{cidfont}: {boxes:?}"
        );
        assert!(
            close(boxes[1], [294.0, 110.0, 306.0, 122.0]),
            "{cidfont}: {boxes:?}"
        );
        assert_eq!(marker_point(boxes[2]), (300.0, 664.0), "{cidfont}");
    }
}

/// `Metrics2` gives CID 0 w1 = (0, −1 em) and v zero, and nothing to CID
/// 1, which keeps its mode-0 metrics.
fn with_partial_metrics2(fonts: &[u8], cidfont: &str, em: f64) -> Vec<u8> {
    let mut fonts = fonts.to_vec();
    let entry = format!("/Metrics2 << 0 [0 {} 0 0] >> def", -em);
    fonts.extend(redefine(cidfont, &entry).bytes());
    fonts
}

/// A CID `Metrics2` lacks is set as in writing mode 0, beside CIDs it has.
#[test]
fn metrics2_leaves_the_cids_it_lacks_horizontal() {
    for (fonts, cidfont, em) in cid_fonts() {
        let boxes = fills(
            &with_partial_metrics2(&fonts, cidfont, em),
            cidfont,
            &format!("300 700 moveto <000100000001> show {MARK}"),
        );
        // Right along the line, down a line with CID 0, right again.
        assert_eq!(boxes.len(), 3, "{cidfont}: {boxes:?}");
        assert!(
            close(boxes[0], [300.0, 80.0, 312.0, 92.0]),
            "{cidfont}: {boxes:?}"
        );
        assert!(
            close(boxes[1], [312.0, 92.0, 324.0, 104.0]),
            "{cidfont}: {boxes:?}"
        );
        assert_eq!(marker_point(boxes[2]), (324.0, 688.0), "{cidfont}");
    }
}

/// A `CDevProc` doubling w0, which applies in writing mode 0 too.
const WIDE_CDEVPROC: &str = "/CDevProc {pop 10 -1 roll 2 mul 10 1 roll} def";

/// Show `<00010001>` horizontally with CIDFont `cidfont`.
fn horizontal_body(cidfont: &str) -> String {
    format!(
        "/H /Identity-H [/{cidfont} /CIDFont findresource] composefont pop\n\
         /H findfont 12 scalefont setfont 300 700 moveto <00010001> show {MARK}"
    )
}

#[test]
fn a_cdevproc_changes_the_width_in_writing_mode_0() {
    for (mut fonts, cidfont, _) in cid_fonts() {
        fonts.extend(redefine(cidfont, WIDE_CDEVPROC).bytes());
        let boxes = fills(&fonts, cidfont, &horizontal_body(cidfont));
        assert_eq!(boxes.len(), 3, "{cidfont}: {boxes:?}");
        assert!(
            close(boxes[1], [324.0, 80.0, 336.0, 92.0]),
            "{cidfont}: {boxes:?}"
        );
        assert_eq!(marker_point(boxes[2]), (348.0, 700.0), "{cidfont}");
    }
}

/// A `CDevProc` that is not a procedure is a `typecheck` when a glyph is
/// shown.
#[test]
fn a_cdevproc_must_be_a_procedure() {
    for (mut fonts, cidfont, _) in cid_fonts() {
        fonts.extend(redefine(cidfont, "/CDevProc 5 def").bytes());
        let boxes = fills(
            &fonts,
            cidfont,
            "{300 700 moveto <0001> show} stopped\n\
             {$error /errorname get /typecheck eq {0 0 1 1 rectfill} if} if",
        );
        assert_eq!(boxes, [[0.0, 791.0, 1.0, 792.0]], "{cidfont}");
    }
}

/// Assert that `body` draws the same in PDF output as in the interpreter.
#[cfg(feature = "pdf-output")]
fn assert_pdf_matches(fonts: &[u8], cidfont: &str, body: &str, count: usize) {
    let ps = fills(fonts, cidfont, body);
    let pdf = pdf_fills(fonts, cidfont, body);
    assert_eq!(ps.len(), count, "{cidfont}: {ps:?}");
    assert_eq!(ps.len(), pdf.len(), "{cidfont}: PS {ps:?} PDF {pdf:?}");
    for (p, q) in ps.iter().zip(&pdf) {
        assert!(close_enough(*p, *q), "{cidfont}: PS {p:?} PDF {q:?}");
    }
}

/// PDF output draws vertical text where the interpreter does: through an
/// Identity-V font, with each string placed on its own.
#[cfg(feature = "pdf-output")]
#[test]
fn pdf_output_draws_vertical_text_as_the_interpreter_does() {
    // Two glyphs down a column, then single glyphs side by side on one
    // row, which horizontal text would join into one run.
    let body = format!(
        "300 700 moveto <00010001> show {MARK}\n\
         100 500 moveto <0001> show 115 500 moveto <0001> show {MARK}"
    );
    for (fonts, cidfont, _) in vertical_fonts() {
        assert_pdf_matches(&fonts, cidfont, &body, 6);
    }
}

/// A CIDFont shown both horizontally and vertically becomes two PDF fonts,
/// one per writing mode.
#[cfg(feature = "pdf-output")]
#[test]
fn pdf_output_keeps_each_writing_mode_of_a_cidfont() {
    for (fonts, cidfont, _) in vertical_fonts() {
        let body = format!(
            "300 700 moveto <00010001> show\n\
             /H /Identity-H [/{cidfont} /CIDFont findresource] composefont pop\n\
             /H findfont 12 scalefont setfont 100 400 moveto <00010001> show {MARK}"
        );
        assert_pdf_matches(&fonts, cidfont, &body, 5);
    }
}

/// PDF output follows the interpreter for each source of metrics: none
/// (horizontal text), `Metrics2`, `Metrics2` that leaves a CID horizontal,
/// and a `CDevProc` that changes widths.
#[cfg(feature = "pdf-output")]
#[test]
fn pdf_output_uses_the_metrics_the_interpreter_does() {
    let body = format!("300 700 moveto <00010001> show {MARK}");
    for (fonts, cidfont, em) in cid_fonts() {
        assert_pdf_matches(&fonts, cidfont, &body, 3);
        assert_pdf_matches(&with_metrics2(&fonts, cidfont, em), cidfont, &body, 3);
        assert_pdf_matches(
            &with_partial_metrics2(&fonts, cidfont, em),
            cidfont,
            &format!("300 700 moveto <000100000001> show {MARK}"),
            3,
        );
        let mut wide = fonts.clone();
        wide.extend(redefine(cidfont, WIDE_CDEVPROC).bytes());
        assert_pdf_matches(&wide, cidfont, &horizontal_body(cidfont), 3);
        assert_pdf_matches(&wide, cidfont, &body, 3);
    }
}

/// PDF output rounds widths and metrics to thousandths of an em, 0.012
/// points at 12 points.
#[cfg(feature = "pdf-output")]
fn close_enough(a: [f64; 4], b: [f64; 4]) -> bool {
    a.iter().zip(b).all(|(a, b)| (a - b).abs() < 0.02)
}

/// `ashow` and `widthshow` spacing survives PDF output, down a column and
/// along a line: a PDF font advances each glyph by its width alone, so
/// each CID of such a show is placed on its own.
#[cfg(feature = "pdf-output")]
#[test]
fn pdf_output_keeps_added_spacing() {
    for (fonts, cidfont, _) in vertical_fonts() {
        let body = format!(
            "300 700 moveto 0 -5 <00010001> ashow {MARK}\n\
             200 700 moveto 0 -5 1 <00010001> widthshow {MARK}\n\
             /H /Identity-H [/{cidfont} /CIDFont findresource] composefont pop\n\
             /H findfont 12 scalefont setfont 10 300 moveto 5 0 <00010001> ashow {MARK}"
        );
        let ps = fills(&fonts, cidfont, &body);
        assert_eq!(ps.len(), 9, "{cidfont}: {ps:?}");
        // The second glyph of each is 5 points further on.
        assert!(close(ps[1], [294.0, 107.56, 306.0, 119.56]), "{ps:?}");
        assert!(close(ps[7], [27.0, 480.0, 39.0, 492.0]), "{ps:?}");
        assert_pdf_matches(&fonts, cidfont, &body, 9);
    }
}
