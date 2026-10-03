// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF output joins the strings of a baseline into one `TJ` run in every
//! instance of a font and on every page, kerned by the widths the font's
//! `/Widths` gives a viewer.
//!
//! Runs joined only in a font resource's first instance — a second
//! `scalefont` of the same face never did — and only for the characters the
//! font's first page used, since widths were measured once, there. And the
//! kerns were computed from a copy of the `/Widths` code that had drifted
//! from it; a kern a viewer's widths disagree with moves the text after it.
#![cfg(feature = "pdf-output")]

mod common;

use common::{hex, sfnt_with_cmap};
use stet::{DisplayElement, Interpreter};
use stet_fonts::geometry::PathSegment;

/// Each fill's bounding box.
fn boxes<'a>(elements: impl Iterator<Item = &'a DisplayElement>) -> Vec<[f64; 4]> {
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
                Some(b)
            }
            _ => None,
        })
        .collect()
}

/// The job's PDF, checked to draw every glyph where the PostScript does,
/// and each page's count of `TJ` and of `Tj` operators.
fn runs(job: &str) -> Vec<(usize, usize)> {
    let ps: Vec<Vec<[f64; 4]>> = Interpreter::new()
        .render_to_display_list(job.as_bytes(), 72.0)
        .unwrap()
        .iter()
        .map(|page| boxes(page.display_list.elements().iter()))
        .collect();
    let pdf = Interpreter::new()
        .render_to_pdf(job.as_bytes(), 72.0)
        .unwrap();
    let doc = stet_pdf_reader::PdfDocument::from_bytes(&pdf).unwrap();
    assert_eq!(doc.page_count(), ps.len());
    (0..doc.page_count())
        .map(|i| {
            let from_pdf = boxes(doc.render_page(i, 72.0).unwrap().elements().iter());
            assert_eq!(from_pdf.len(), ps[i].len(), "page {i}");
            for (pdf, ps) in from_pdf.iter().zip(&ps[i]) {
                assert!(
                    pdf.iter().zip(ps).all(|(a, b)| (a - b).abs() < 0.02),
                    "page {i}: PDF {pdf:?}, PS {ps:?}"
                );
            }
            let contents = String::from_utf8_lossy(&doc.page_contents(i).unwrap()).into_owned();
            let count = |op: &str| contents.lines().filter(|l| l.ends_with(op)).count();
            (count(" TJ"), count(" Tj"))
        })
        .collect()
}

/// Three words on each of two baselines: one `TJ` each, no `Tj`.
const JOINED: (usize, usize) = (2, 0);

/// The face at two sizes: the second size is a second instance of the
/// same font resource.
#[test]
fn a_second_instance_joins_its_strings() {
    let job = "%!PS\n<< /PageSize [300 200] >> setpagedevice\n\
               /Times-Roman findfont 12 scalefont setfont\n\
               20 150 moveto (abc) show 40 150 moveto (abc) show 60 150 moveto (abc) show\n\
               /Times-Roman findfont 18 scalefont setfont\n\
               20 80 moveto (abc) show 50 80 moveto (abc) show 80 80 moveto (abc) show\n\
               showpage\n";
    assert_eq!(runs(job), [JOINED]);
}

/// The second page's characters are not the first page's.
#[test]
fn a_later_pages_characters_join() {
    let job = "%!PS\n<< /PageSize [300 200] >> setpagedevice\n\
               /Times-Roman findfont 12 scalefont setfont\n\
               20 150 moveto (abc) show 40 150 moveto (abc) show 60 150 moveto (abc) show\n\
               20 80 moveto (abc) show 40 80 moveto (abc) show 60 80 moveto (abc) show\n\
               showpage\n\
               20 150 moveto (xyz) show 40 150 moveto (xyz) show 60 150 moveto (xyz) show\n\
               20 80 moveto (xyz) show 40 80 moveto (xyz) show 60 80 moveto (xyz) show\n\
               showpage\n";
    assert_eq!(runs(job), [JOINED, JOINED]);
}

/// Two instances of one font, each encoding a different subset, as dvips
/// writes them. The second instance's characters are `.notdef` in the
/// first's encoding: their widths come from the instance that encodes them,
/// as the font's `/Widths` do.
#[test]
fn a_reencoded_instance_kerns_by_its_own_encoding() {
    let job = "%!PS\n<< /PageSize [300 200] >> setpagedevice\n\
               /Base /Times-Roman findfont def\n\
               /Re Base dup length dict begin\n\
                 { 1 index /FID ne { def } { pop pop } ifelse } forall\n\
                 /FontName /Re def\n\
               currentdict end definefont pop\n\
               /encoding { 256 array 0 1 255 { 1 index exch /.notdef put } for } def\n\
               /ReA /Re findfont dup length dict copy dup /Encoding encoding dup 97 /a put put\n\
                 definefont pop\n\
               /ReB /Re findfont dup length dict copy dup /Encoding encoding dup 120 /x put put\n\
                 definefont pop\n\
               /ReA findfont 20 scalefont setfont\n\
               20 150 moveto (aa) show 45 150 moveto (aa) show 70 150 moveto (aa) show\n\
               /ReB findfont 20 scalefont setfont\n\
               20 80 moveto (xx) show 45 80 moveto (xx) show 70 80 moveto (xx) show\n\
               showpage\n";
    assert_eq!(runs(job), [JOINED]);
}

/// A Type 42 font: the second size, and a later page's glyph.
#[test]
fn type42_runs_join_in_any_instance_and_page() {
    let job = format!(
        "%!PS\n<< /PageSize [300 200] >> setpagedevice\n\
         /T42 <<\n\
           /FontType 42 /FontName /T42 /PaintType 0\n\
           /FontMatrix [1 0 0 1 0 0] /FontBBox [0 0 1000 1000]\n\
           /Encoding 256 array dup 0 1 255 {{ /.notdef put dup }} for pop\n\
             dup 97 /square put dup 98 /bar put\n\
           /CharStrings << /.notdef 0 /square 1 /bar 2 >>\n\
           /sfnts [<{}>]\n\
         >> definefont pop\n\
         /T42 findfont 10 scalefont setfont\n\
         20 150 moveto (a) show 33 150 moveto (a) show 46 150 moveto (a) show\n\
         /T42 findfont 20 scalefont setfont\n\
         20 80 moveto (a) show 46 80 moveto (a) show 72 80 moveto (a) show\n\
         showpage\n\
         /T42 findfont 10 scalefont setfont\n\
         20 150 moveto (b) show 28 150 moveto (b) show 36 150 moveto (b) show\n\
         20 80 moveto (b) show 28 80 moveto (b) show 36 80 moveto (b) show\n\
         showpage\n",
        hex(&sfnt_with_cmap(&[(97, 1), (98, 2)]))
    );
    assert_eq!(runs(&job), [JOINED, JOINED]);
}
