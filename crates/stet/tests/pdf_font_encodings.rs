// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Two instances of one font whose encodings disagree keep their own glyphs
//! in PDF output.
//!
//! stet-pdf gave every instance of a font name one PDF font resource, with
//! the encodings merged code by code, first instance first. pdftops
//! re-encodes a font beside the original — ZapfDingbats with a custom
//! encoding, say — so the second instance drew the first one's glyphs. A
//! PDF font has one encoding, so each such instance now gets its own
//! resource; instances whose encodings agree still share one, as dvips's
//! re-encoded copies do.
#![cfg(feature = "pdf-output")]

use stet::{DisplayElement, Interpreter};
use stet_fonts::geometry::PathSegment;

/// `font` twice: re-encoded so that code 65 (`A`) names `/period`, then as it
/// is, each showing `(A)`.
fn job(font: &str) -> Vec<u8> {
    format!(
        "%!PS\n<< /PageSize [300 100] >> setpagedevice\n\
         /{font} findfont dup length dict begin\n\
           {{ 1 index /FID ne {{ def }} {{ pop pop }} ifelse }} forall\n\
           /Encoding 256 array 0 1 255 {{ 1 index exch /.notdef put }} for\n\
             dup 65 /period put def\n\
           currentdict end /Recoded exch definefont pop\n\
         /Recoded findfont 50 scalefont setfont 20 30 moveto (A) show\n\
         /{font} findfont 50 scalefont setfont 150 30 moveto (A) show\n\
         showpage\n"
    )
    .into_bytes()
}

/// Each fill's bounding box, rounded to a tenth of a point.
fn boxes<'a>(elements: impl Iterator<Item = &'a DisplayElement>) -> Vec<[i64; 4]> {
    elements
        .filter_map(|e| match e {
            DisplayElement::Fill { path, .. } => {
                let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
                for s in &path.segments {
                    let pts: &[(f64, f64)] = match s {
                        PathSegment::MoveTo(x, y) | PathSegment::LineTo(x, y) => &[(*x, *y)],
                        PathSegment::CurveTo { x3, y3, .. } => &[(*x3, *y3)],
                        PathSegment::ClosePath => &[],
                    };
                    for &(x, y) in pts {
                        b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
                    }
                }
                Some(b.map(|v| (v * 10.0).round() as i64))
            }
            _ => None,
        })
        .collect()
}

fn same_in_pdf(font: &str) {
    let ps = job(font);
    let mut interp = Interpreter::builder().build();
    let pages = interp.render_to_display_list(&ps, 72.0).unwrap();
    let expected = boxes(pages[0].display_list.elements().iter());
    // A period and an A: different glyphs.
    assert_eq!(expected.len(), 2, "{expected:?}");
    assert_ne!(
        expected[0][2] - expected[0][0],
        expected[1][2] - expected[1][0]
    );

    let pdf = Interpreter::builder()
        .build()
        .render_to_pdf(&ps, 72.0)
        .unwrap();
    let doc = stet_pdf_reader::PdfDocument::from_bytes(&pdf).unwrap();
    let list = doc.render_page(0, 72.0).unwrap();
    assert_eq!(boxes(list.elements().iter()), expected);
}

/// A standard 14 font is referenced, not embedded: each resource carries
/// its own `/Differences`.
#[test]
fn referenced_font_keeps_each_encoding() {
    same_in_pdf("Helvetica");
}

/// An embedded font: each resource embeds the glyphs its encoding names.
#[test]
fn embedded_font_keeps_each_encoding() {
    same_in_pdf("NimbusRoman-Regular");
}
