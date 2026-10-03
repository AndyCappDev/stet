// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF output embeds the fonts text was shown with, even when a `restore`
//! has since reclaimed or changed them.
//!
//! The PDF is built at the end of the job. It read each font out of the VM
//! by the id its text recorded, so a font defined inside a page's `save` —
//! as Ghostscript's `eps2write` and `ps2write` do, and as an EPS figure
//! placed with `save … restore` does — was gone: PDF output panicked, or
//! read whatever later object reused the id. A glyph a page added to a font
//! inside its `save` (PLRM 3e §5.9.2) was silently missing. The interpreter
//! now copies each font before a `restore` can change it, and PDF output
//! reads the copies.
#![cfg(feature = "pdf-output")]

use stet::{DisplayElement, Interpreter};
use stet_fonts::geometry::PathSegment;

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

/// The glyph boxes of each page as the PostScript draws them, and as its
/// PDF does; the PDF must have every glyph, in place.
fn same_in_pdf(job: &str) -> Vec<Vec<[i64; 4]>> {
    let ps: Vec<Vec<[i64; 4]>> = Interpreter::new()
        .render_to_display_list(job.as_bytes(), 72.0)
        .unwrap()
        .iter()
        .map(|page| boxes(page.display_list.elements().iter()))
        .collect();
    let pdf = Interpreter::new()
        .render_to_pdf(job.as_bytes(), 72.0)
        .unwrap();
    let doc = stet_pdf_reader::PdfDocument::from_bytes(&pdf).unwrap();
    let from_pdf: Vec<Vec<[i64; 4]>> = (0..doc.page_count())
        .map(|i| boxes(doc.render_page(i, 72.0).unwrap().elements().iter()))
        .collect();
    assert_eq!(from_pdf, ps);
    ps
}

/// A copy of Times-Roman named `name` (so PDF output embeds it rather than
/// naming a standard font) whose `CharStrings` has only `.notdef` and
/// `glyphs`, defined as `/name`.
fn subset_font(name: &str, glyphs: &str) -> String {
    format!(
        "/Base /Times-Roman findfont def\n\
         /{name} Base dup length dict begin\n\
           {{ 1 index /FID ne 2 index /CharStrings ne and {{ def }} {{ pop pop }} ifelse }} forall\n\
           /CharStrings 10 dict dup begin\n\
             /.notdef Base /CharStrings get /.notdef get def\n\
             [ {glyphs} ] {{ dup Base /CharStrings get exch get def }} forall\n\
           end def\n\
           /FontName /{name} def\n\
           currentdict end definefont pop\n"
    )
}

/// Each page defines its font inside its `save`, as `eps2write` does: the
/// second page's font reuses the first one's reclaimed id.
#[test]
fn fonts_defined_inside_each_pages_save() {
    let job = format!(
        "%!PS\n<< /PageSize [300 100] >> setpagedevice\n\
         save\n{}/One findfont 48 scalefont setfont 20 30 moveto (AAA) show showpage restore\n\
         save\n{}/Two findfont 48 scalefont setfont 20 30 moveto (BB) show showpage restore\n",
        subset_font("One", "/A"),
        subset_font("Two", "/B")
    );
    let pages = same_in_pdf(&job);
    assert_eq!(pages.iter().map(Vec::len).collect::<Vec<_>>(), [3, 2]);
}

/// Two figures on one page, each with its own font inside its own
/// `save … restore`: the second font reuses the first one's id, and its
/// text must not be drawn with the first font.
#[test]
fn two_fonts_sharing_a_reused_id_on_one_page() {
    let job = format!(
        "%!PS\n<< /PageSize [300 100] >> setpagedevice\n\
         save\n{}/One findfont 48 scalefont setfont 20 30 moveto (A) show restore\n\
         save\n{}/Two findfont 48 scalefont setfont 150 30 moveto (B) show restore\n\
         showpage\n",
        subset_font("One", "/A"),
        subset_font("Two", "/B")
    );
    assert_eq!(same_in_pdf(&job)[0].len(), 2);
}

/// An EPS figure placed with `save … restore`: its font is reclaimed before
/// the page is sent.
#[test]
fn an_eps_figure_placed_with_save_and_restore() {
    let job = format!(
        "%!PS\n<< /PageSize [300 100] >> setpagedevice\n\
         /b4_inc_state save def\n\
         {}/Fig findfont 48 scalefont setfont 20 30 moveto (AB) show\n\
         b4_inc_state restore\n\
         /Helvetica findfont 48 scalefont setfont 150 30 moveto (C) show\n\
         showpage\n",
        subset_font("Fig", "/A /B")
    );
    assert_eq!(same_in_pdf(&job)[0].len(), 3);
}

/// PLRM 3e §5.9.2: a page adds a glyph to a font defined outside its save,
/// shows it, and its `restore` removes it.
#[test]
fn a_glyph_added_inside_a_pages_save() {
    let job = format!(
        "%!PS\n<< /PageSize [300 100] >> setpagedevice\n{}\
         /IncS /Inc findfont 48 scalefont def\n\
         save\n\
           /Inc findfont /CharStrings get /B Base /CharStrings get /B get put\n\
           IncS setfont 20 30 moveto (AB) show showpage\n\
         restore\n",
        subset_font("Inc", "/A")
    );
    assert_eq!(same_in_pdf(&job)[0].len(), 2);
}

/// A cached Type 3 glyph that shows another font is replayed on a later
/// page, after the `restore` that reclaimed that font.
#[test]
fn a_cached_type3_glyph_replayed_after_a_restore() {
    let job = format!(
        "%!PS\n<< /PageSize [300 100] >> setpagedevice\n\
         /Wrap 8 dict begin\n\
           /FontType 3 def /FontMatrix [0.001 0 0 0.001 0 0] def\n\
           /FontBBox [0 0 1000 1000] def\n\
           /Encoding 256 array dup 0 1 255 {{ /.notdef put dup }} for pop dup 65 /A put def\n\
           /BuildChar {{ pop pop 1000 0 0 0 1000 1000 setcachedevice\n\
             /Inner findfont 1000 scalefont setfont 0 0 moveto (A) show }} def\n\
         currentdict end definefont pop\n\
         /WrapS /Wrap findfont 48 scalefont def\n\
         save\n{}WrapS setfont 20 30 moveto (A) show showpage restore\n\
         save WrapS setfont 20 30 moveto (AA) show showpage restore\n",
        subset_font("Inner", "/A")
    );
    let pages = same_in_pdf(&job);
    assert_eq!(pages.iter().map(Vec::len).collect::<Vec<_>>(), [1, 2]);
}

/// PDF output no longer holds the job's fonts in the VM: a job that shows
/// text, in fonts reclaimed mid-job and fonts live to the end, leaves no
/// more behind than an empty one, and its font copies go with it.
#[test]
fn the_job_leaves_nothing_of_its_fonts_behind() {
    let job = format!(
        "%!PS\nsave\n{}/One findfont 12 scalefont setfont 72 72 moveto (A) show showpage restore\n\
         /Times-Roman findfont 12 scalefont setfont 72 72 moveto (A) show showpage\n",
        subset_font("One", "/A")
    );
    let mut interp = Interpreter::new();
    let mut growth = |source: &[u8]| {
        let before = interp.context().dicts.local.entities.len();
        interp.render_to_pdf(source, 72.0).unwrap();
        assert!(interp.context().font_snapshots.is_empty());
        interp.context().dicts.local.entities.len() - before
    };
    // Load the fonts once, so what is left is what each job leaves.
    growth(job.as_bytes());
    assert_eq!(growth(job.as_bytes()), growth(b"%!PS\n"));
}
