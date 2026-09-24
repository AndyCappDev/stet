// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Text extraction from PDF: `PdfDocument::set_extract_text`.
//!
//! The switch only adds `DisplayElement::TextRun` elements. Everything else
//! in the display list — and so everything rendered or written to PDF — must
//! be identical with it on and off, and with it off no `TextRun` may appear
//! at all.

use stet_graphics::display_list::{DisplayElement, DisplayList};
use stet_pdf_reader::PdfDocument;

/// One page showing text every way a content stream can, and in the places
/// text can hide: rotated, invisible, in a Type 3 font, in a form XObject,
/// and in an optional-content layer.
const CONTENT: &str = "\
BT /F1 12 Tf 72 700 Td (Tj) Tj ET
BT /F1 12 Tf 72 680 Td [(T) 120 (J)] TJ ET
BT /F1 12 Tf 14 TL 72 660 Td (quote) ' ET
BT /F1 12 Tf 14 TL 72 640 Td 1 2 (dquote) \" ET
BT /F1 12 Tf 0.866 0.5 -0.5 0.866 72 600 Tm (rotated) Tj ET
q BT /F1 12 Tf 3 Tr 72 560 Td (invisible) Tj ET Q
BT /F2 12 Tf 72 540 Td (AAA) Tj ET
q 1 0 0 1 72 500 cm /Fm1 Do Q
/OC /L1 BDC BT /F1 12 Tf 72 460 Td (layer) Tj ET EMC
";

fn build_pdf() -> Vec<u8> {
    let form = "BT /F1 12 Tf 0 5 Td (form) Tj ET";
    let type3_glyph = "1000 0 0 0 1000 1000 d1 0 0 1000 1000 re f";
    let objects: Vec<String> = vec![
        // 1: catalog, with the layer in /OCProperties.
        "<< /Type /Catalog /Pages 2 0 R \
         /OCProperties << /OCGs [7 0 R] /D << /Order [7 0 R] >> >> >>"
            .into(),
        // 2: pages
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        // 3: page
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
         /Resources << /Font << /F1 5 0 R /F2 6 0 R >> /XObject << /Fm1 8 0 R >> \
         /Properties << /L1 7 0 R >> >> >>"
            .into(),
        // 4: content
        format!(
            "<< /Length {} >>\nstream\n{CONTENT}\nendstream",
            CONTENT.len()
        ),
        // 5: a standard-14 font
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".into(),
        // 6: a Type 3 font with one glyph, /A
        "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
         /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << /A 9 0 R >> \
         /Encoding << /Type /Encoding /Differences [65 /A] >> \
         /FirstChar 65 /LastChar 65 /Widths [1000] >>"
            .into(),
        // 7: the layer
        "<< /Type /OCG /Name (Layer) >>".into(),
        // 8: form XObject showing text
        format!(
            "<< /Type /XObject /Subtype /Form /BBox [0 0 200 20] \
             /Resources << /Font << /F1 5 0 R >> >> /Length {} >>\nstream\n{form}\nendstream",
            form.len()
        ),
        // 9: the Type 3 glyph procedure
        format!(
            "<< /Length {} >>\nstream\n{type3_glyph}\nendstream",
            type3_glyph.len()
        ),
    ];

    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = pdf.len();
    pdf.extend(format!("xref\n0 {}\n0000000000 65535 f\r\n", objects.len() + 1).as_bytes());
    for off in offsets {
        pdf.extend(format!("{off:010} 00000 n\r\n").as_bytes());
    }
    pdf.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}

fn render(pdf: &[u8], extract: bool) -> DisplayList {
    let mut doc = PdfDocument::from_bytes(pdf).expect("fixture parses");
    doc.set_extract_text(extract);
    doc.render_page(0, 72.0).expect("fixture renders")
}

/// `list` with every `TextRun` removed, at every depth — including
/// transparency groups, soft masks, layers and pattern cells.
fn without_text_runs(list: &DisplayList) -> DisplayList {
    let mut out = list.clone();
    out.clear();
    for element in list.elements() {
        let kept = match element {
            DisplayElement::TextRun { .. } => continue,
            DisplayElement::Group { elements, params } => DisplayElement::Group {
                elements: without_text_runs(elements),
                params: params.clone(),
            },
            DisplayElement::OcgGroup {
                elements,
                visibility,
            } => DisplayElement::OcgGroup {
                elements: without_text_runs(elements),
                visibility: visibility.clone(),
            },
            DisplayElement::SoftMasked {
                mask,
                content,
                params,
                mask_cache,
            } => DisplayElement::SoftMasked {
                mask: without_text_runs(mask),
                content: without_text_runs(content),
                params: params.clone(),
                mask_cache: mask_cache.clone(),
            },
            DisplayElement::PatternFill { params } => {
                let mut params = params.clone();
                params.tile = without_text_runs(&params.tile);
                DisplayElement::PatternFill { params }
            }
            other => other.clone(),
        };
        out.push(kept);
    }
    out
}

#[test]
fn switch_changes_nothing_but_text_runs() {
    let pdf = build_pdf();
    let off = render(&pdf, false);
    let on = render(&pdf, true);
    // With the switch off, not a single TextRun at any depth.
    assert_eq!(format!("{off:?}"), format!("{:?}", without_text_runs(&off)));
    // With it on, removing the TextRuns gives back the list exactly.
    assert_eq!(format!("{off:?}"), format!("{:?}", without_text_runs(&on)));
    // No show operator records a TextRun yet (emission lands in a later
    // change), so the two lists are identical outright.
    assert_eq!(format!("{off:?}"), format!("{on:?}"));
}

#[test]
fn fixture_draws_every_text_construct() {
    // Guards the test above against a fixture that silently stopped
    // drawing: each visible string paints glyph fills, and the layer and
    // form arrive as their own containers.
    let list = render(&build_pdf(), false);
    fn count(list: &DisplayList, f: &dyn Fn(&DisplayElement) -> bool) -> usize {
        list.elements()
            .iter()
            .map(|e| {
                let nested = match e {
                    DisplayElement::Group { elements, .. }
                    | DisplayElement::OcgGroup { elements, .. } => count(elements, f),
                    _ => 0,
                };
                nested + usize::from(f(e))
            })
            .sum()
    }
    let fills = count(&list, &|e| matches!(e, DisplayElement::Fill { .. }));
    let layers = count(&list, &|e| matches!(e, DisplayElement::OcgGroup { .. }));
    assert!(fills >= 30, "only {fills} fills");
    assert_eq!(layers, 1);
}
