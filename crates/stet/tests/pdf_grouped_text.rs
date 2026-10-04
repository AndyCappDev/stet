// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF output writes the text inside transparency groups, soft masks,
//! layers and pattern tiles as text, in fonts it embeds, with widths for
//! every character.
//!
//! Only a page's top-level text had its fonts tracked. Text inside a
//! container in a font used nowhere else vanished — no text, and its glyph
//! outlines skipped as the text's companions; characters used only there
//! were missing from the font's `/Widths`, so viewers stacked them; and a
//! page whose only text was in a group drew its outlines as well as text in
//! a font the page's resources did not name. A pattern tile's fonts were
//! tracked after the fonts were embedded, so a font used only in a tile was
//! named but never written.
#![cfg(feature = "pdf-output")]

use stet::{DisplayElement, Interpreter, PsDisplayList, TextExtraction};
use stet_fonts::geometry::PathSegment;

/// Each fill's bounding box, at every depth: through groups, soft masks
/// (mask, then content), layers and pattern tiles.
fn boxes(list: &PsDisplayList, out: &mut Vec<[f64; 4]>) {
    for e in list.elements() {
        match e {
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
                out.push(b);
            }
            DisplayElement::Group { elements, .. } | DisplayElement::OcgGroup { elements, .. } => {
                boxes(elements, out)
            }
            DisplayElement::SoftMasked { mask, content, .. } => {
                boxes(mask, out);
                boxes(content, out);
            }
            DisplayElement::PatternFill { params } => boxes(&params.tile, out),
            _ => {}
        }
    }
}

/// The text of each run, at every depth.
fn texts(list: &PsDisplayList, out: &mut Vec<String>) {
    for e in list.elements() {
        match e {
            DisplayElement::TextRun { params } => out.push(params.text.clone()),
            DisplayElement::Group { elements, .. } | DisplayElement::OcgGroup { elements, .. } => {
                texts(elements, out)
            }
            DisplayElement::SoftMasked { mask, content, .. } => {
                texts(mask, out);
                texts(content, out);
            }
            DisplayElement::PatternFill { params } => texts(&params.tile, out),
            _ => {}
        }
    }
}

/// The job's PDF, checked to draw every glyph where the PostScript does —
/// no glyph missing, moved, drawn in another font, or drawn twice — and
/// each page's text, as a reader extracts it.
fn pdf_text(job: &str) -> Vec<Vec<String>> {
    let ps: Vec<Vec<[f64; 4]>> = Interpreter::new()
        .render_to_display_list(job.as_bytes(), 72.0)
        .unwrap()
        .iter()
        .map(|page| {
            let mut b = Vec::new();
            boxes(&page.display_list, &mut b);
            b
        })
        .collect();
    let pdf = Interpreter::new()
        .render_to_pdf(job.as_bytes(), 72.0)
        .unwrap();
    let mut doc = stet_pdf_reader::PdfDocument::from_bytes(&pdf).unwrap();
    doc.set_text_extraction(TextExtraction::Runs);
    assert_eq!(doc.page_count(), ps.len());
    (0..doc.page_count())
        .map(|i| {
            let list = doc.render_page(i, 72.0).unwrap();
            let mut from_pdf = Vec::new();
            boxes(&list, &mut from_pdf);
            assert_eq!(from_pdf.len(), ps[i].len(), "page {i}: fills");
            for (pdf, ps) in from_pdf.iter().zip(&ps[i]) {
                assert!(
                    pdf.iter().zip(ps).all(|(a, b)| (a - b).abs() < 0.02),
                    "page {i}: PDF {pdf:?}, PS {ps:?}"
                );
            }
            let mut t = Vec::new();
            texts(&list, &mut t);
            t
        })
        .collect()
}

const PAGE: &str = "%!PS\n<< /PageSize [300 200] >> setpagedevice\n";

/// Text in a group, a layer, a soft mask's content and its mask, each in a
/// font the page uses nowhere else, beside top-level text in another. A
/// reader extracts no text from a mask, so the mask's `abc` is checked by
/// its glyphs alone.
#[test]
fn text_in_containers_is_written_in_its_own_fonts() {
    let job = format!(
        "{PAGE}/Times-Roman findfont 12 scalefont setfont 20 170 moveto (Top) show\n\
         << >> begintransparencygroup\n\
           /Helvetica findfont 12 scalefont setfont 20 140 moveto (Group) show\n\
         endtransparencygroup\n\
         << /Name (L) >> defineocg beginoptionalcontent\n\
           /Courier findfont 12 scalefont setfont 20 110 moveto (Layer) show\n\
         endoptionalcontent\n\
         << /Subtype /Luminosity /BBox [0 0 300 200] >> beginsoftmask\n\
           1 setgray 0 0 300 200 rectfill\n\
           0 setgray /Symbol findfont 12 scalefont setfont 200 80 moveto (abc) show\n\
         endsoftmask\n\
           /Helvetica-Bold findfont 12 scalefont setfont 20 80 moveto (Masked) show\n\
         clearsoftmask\n\
         showpage\n"
    );
    assert_eq!(pdf_text(&job), [["Top", "Group", "Layer", "Masked"]]);
}

/// The font is used at top level, but these characters only in a group: the
/// font's widths must cover them, or a viewer advances by 0 and stacks the
/// glyphs. (The glyph positions are what `pdf_text` compares.)
#[test]
fn characters_used_only_in_a_group_have_widths() {
    let job = format!(
        "{PAGE}/Times-Roman findfont 12 scalefont setfont 20 170 moveto (Top) show\n\
         << >> begintransparencygroup 20 140 moveto (InGroup) show endtransparencygroup\n\
         << /Name (L) >> defineocg beginoptionalcontent\n\
           20 110 moveto (yawning) show\n\
         endoptionalcontent\n\
         showpage\n"
    );
    assert_eq!(pdf_text(&job), [["Top", "InGroup", "yawning"]]);
}

/// A page whose only text is in a group: once, as text — not also as glyph
/// outlines, which `pdf_text` would count twice — on a page whose resources
/// name its font. Page 1 tracks the font, which is what made page 2 write
/// text against a font it lacked; page 3's font appears nowhere else.
#[test]
fn a_page_with_text_only_in_a_group_writes_it_once() {
    let job = format!(
        "{PAGE}/Times-Roman findfont 12 scalefont setfont\n\
         20 170 moveto (First) show showpage\n\
         << >> begintransparencygroup 20 140 moveto (Only) show endtransparencygroup\n\
         showpage\n\
         /Helvetica findfont 12 scalefont setfont\n\
         << >> begintransparencygroup 20 140 moveto (Alone) show endtransparencygroup\n\
         showpage\n"
    );
    assert_eq!(pdf_text(&job), [["First"], ["Only"], ["Alone"]]);
}

/// Text in a pattern tile, in a font the document uses nowhere else, and in
/// characters of the page's font that only the tile uses. A reader extracts
/// no text from a tile — it is paint, repeated — so the tile's text is
/// checked by its glyphs alone: in a substitute font, or without widths,
/// they would be elsewhere.
#[test]
fn text_in_a_pattern_tile_has_its_font() {
    let job = format!(
        "{PAGE}/Times-Roman findfont 12 scalefont setfont 20 170 moveto (Top) show\n\
         << /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 60 40]\n\
            /XStep 60 /YStep 40\n\
            /PaintProc {{ pop\n\
              /Courier findfont 10 scalefont setfont 2 5 moveto (Tile) show\n\
              /Times-Roman findfont 10 scalefont setfont 2 20 moveto (wavy) show }} >>\n\
         matrix makepattern setpattern\n\
         0 0 120 120 rectfill\n\
         showpage\n"
    );
    assert_eq!(pdf_text(&job), [["Top"]]);
}
