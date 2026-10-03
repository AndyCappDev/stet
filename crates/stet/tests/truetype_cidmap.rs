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

mod common;

use common::{hex, sfnt};
use stet::{DisplayElement, Interpreter};
use stet_fonts::geometry::PathSegment;

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

/// PDF output joins strings on one baseline into a `TJ` run, spacing them
/// by the widths of their CIDs. It summed the widths of the strings'
/// bytes, which a 2-byte CID's are not: with CID 0 also on the page,
/// `<0005>` measured CID 0's 250 plus CID 5's 500, and the string after it
/// landed 25 points early.
#[cfg(feature = "pdf-output")]
#[test]
fn pdf_output_spaces_runs_by_cid_widths() {
    let map = format!("<{}>", hex(&cid_map()));
    let ps = job(
        &map,
        &format!(
            "10 50 moveto <0005> show 65 50 moveto <0007> show 300 20 moveto <0000> show {MARK}"
        ),
    );
    let pdf = Interpreter::builder()
        .build()
        .render_to_pdf(&ps, 72.0)
        .expect("writes a PDF");
    let doc = stet_pdf_reader::PdfDocument::from_bytes(&pdf).expect("reads back");
    let fills = boxes(doc.render_page(0, 72.0).expect("renders").elements().iter());
    let expected = ps_fills(&ps);
    assert_eq!(fills.len(), expected.len(), "{fills:?}");
    assert!(
        close(fills[1], [65.0, 50.0, 165.0, 150.0]),
        "{:?}",
        fills[1]
    );
    for (pdf, ps) in fills.iter().zip(expected) {
        assert!(close(*pdf, ps), "PDF {pdf:?} PS {ps:?}");
    }
}

/// Strings placed one by one along a line join into one `TJ` run, whose
/// kerns add up: whole-thousandth kerns drifted 0.03 points a glyph here.
#[cfg(feature = "pdf-output")]
#[test]
fn pdf_output_places_each_string_of_a_run_exactly() {
    let map = format!("<{}>", hex(&cid_map()));
    let body: String = (0..7)
        .map(|i| format!("{} 50 moveto <0005> show\n", 10.0 + f64::from(i) * 50.37))
        .collect();
    let ps = job(&map, &body);
    let pdf = Interpreter::builder()
        .build()
        .render_to_pdf(&ps, 72.0)
        .expect("writes a PDF");
    let doc = stet_pdf_reader::PdfDocument::from_bytes(&pdf).expect("reads back");
    let fills = boxes(doc.render_page(0, 72.0).expect("renders").elements().iter());
    let expected = ps_fills(&ps);
    assert_eq!(fills.len(), 7, "{fills:?}");
    for (pdf, ps) in fills.iter().zip(expected) {
        assert!(close(*pdf, ps), "PDF {pdf:?} PS {ps:?}");
    }
}

/// Strings on one baseline join into a `TJ` run in a second instance of the
/// font and for a CID first shown on a later page, and land where the
/// PostScript puts them. Runs joined only in a font's first instance, and
/// only for the CIDs its first page used.
#[cfg(feature = "pdf-output")]
#[test]
fn pdf_output_joins_runs_in_any_instance_and_page() {
    let map = format!("<{}>", hex(&cid_map()));
    let ps = job(
        &map,
        "10 120 moveto <0005> show 70 120 moveto <0005> show 130 120 moveto <0005> show\n\
         /F findfont 50 scalefont setfont\n\
         10 40 moveto <0005> show 40 40 moveto <0005> show 70 40 moveto <0005> show\n\
         showpage\n\
         /F findfont 30 scalefont setfont\n\
         10 120 moveto <0007> show 50 120 moveto <0007> show 90 120 moveto <0007> show",
    );
    let pages = Interpreter::builder()
        .build()
        .render_to_display_list(&ps, 72.0)
        .expect("renders");
    let pdf = Interpreter::builder()
        .build()
        .render_to_pdf(&ps, 72.0)
        .expect("writes a PDF");
    let doc = stet_pdf_reader::PdfDocument::from_bytes(&pdf).expect("reads back");
    assert_eq!(doc.page_count(), 2);
    for (i, page) in pages.iter().enumerate() {
        let fills = boxes(doc.render_page(i, 72.0).expect("renders").elements().iter());
        let expected = boxes(page.display_list.elements().iter());
        assert_eq!(fills.len(), expected.len(), "page {i}: {fills:?}");
        for (pdf, ps) in fills.iter().zip(expected) {
            assert!(close(*pdf, ps), "page {i}: PDF {pdf:?} PS {ps:?}");
        }
        let contents = String::from_utf8_lossy(&doc.page_contents(i).unwrap()).into_owned();
        let tj = contents.lines().filter(|l| l.ends_with(" TJ")).count();
        let single = contents.lines().filter(|l| l.ends_with(" Tj")).count();
        assert_eq!((tj, single), (2 - i, 0), "page {i}:\n{contents}");
    }
}
