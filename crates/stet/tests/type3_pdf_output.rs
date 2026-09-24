// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A Type 3 font whose glyph procedure `show`s another font keeps every
//! glyph in PDF output.
//!
//! The procedure's `show` records a `Text` element, which the PDF writer
//! re-emits as real text while skipping the glyph fills beside it. The
//! Type 3 glyph cache replays a cached glyph's elements at each later use,
//! and it used to move the fills but not the `Text` — so in PDF output every
//! cache hit drew its text on top of the first glyph, and `AAAA` came out as
//! one `A`.
#![cfg(feature = "pdf-output")]

use stet::Interpreter;
use stet_graphics::display_list::{DisplayElement, DisplayList};
use stet_pdf_reader::PdfDocument;

const WRAPPER_FONT: &str = "%!PS
<< /PageSize [300 100] >> setpagedevice
/Wrap <<
  /FontType 3 /FontMatrix [0.01 0 0 0.01 0 0] /FontBBox [0 0 100 100]
  /Encoding 256 array dup 0 1 255 { /.notdef put dup } for pop dup 65 /A put
  /BuildChar { pop pop 100 0 0 0 100 100 setcachedevice
               /Helvetica findfont 100 scalefont setfont 0 0 moveto (A) show }
>> definefont pop
/Wrap findfont 60 scalefont setfont
20 20 moveto (AAAA) show
showpage
";

/// The left edge of every filled shape on the page, rounded to a point.
fn fill_left_edges(list: &DisplayList) -> Vec<i64> {
    let mut edges: Vec<i64> = list
        .elements()
        .iter()
        .filter_map(|e| match e {
            DisplayElement::Fill { path, .. } => path
                .segments
                .iter()
                .filter_map(|s| match *s {
                    stet_fonts::geometry::PathSegment::MoveTo(x, _)
                    | stet_fonts::geometry::PathSegment::LineTo(x, _) => Some(x),
                    _ => None,
                })
                .reduce(f64::min),
            _ => None,
        })
        .map(|x| x.round() as i64)
        .collect();
    edges.sort_unstable();
    edges.dedup();
    edges
}

#[test]
fn every_cached_glyph_reaches_the_pdf() {
    let mut interp = Interpreter::new();
    let pdf = interp.render_to_pdf(WRAPPER_FONT.as_bytes(), 72.0).unwrap();
    let doc = PdfDocument::from_bytes(&pdf).unwrap();
    let list = doc.render_page(0, 72.0).unwrap();
    let edges = fill_left_edges(&list);
    // Four A's, 60 points apart, starting near x = 20.
    assert_eq!(edges.len(), 4, "left edges {edges:?}");
    for pair in edges.windows(2) {
        assert!(
            (55..=65).contains(&(pair[1] - pair[0])),
            "left edges {edges:?}"
        );
    }
}
