// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `PdfDocument::set_render_annotations`: with annotations off, a page
//! renders exactly as it would with no annotations at all.

use stet_graphics::layer_set::LayerSet;
use stet_graphics::text::text_runs;
use stet_pdf_reader::{PdfDocument, TextExtraction};

/// A one-page PDF: a blue square of content, and — when `annots` — a
/// Square annotation with an appearance stream that paints and shows
/// text, and one with none, whose appearance stet synthesizes.
fn build_pdf(annots: bool) -> Vec<u8> {
    let content = "0 0 1 rg 100 100 200 200 re f";
    let appearance = "1 0 0 rg 0 0 40 20 re f BT /F1 12 Tf 2 5 Td (note) Tj ET";
    let page_annots = if annots { "/Annots [6 0 R 7 0 R]" } else { "" };
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R {page_annots} >>"
        ),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        "<< /Type /Annot /Subtype /Square /Rect [400 400 440 420] \
         /AP << /N 8 0 R >> >>"
            .to_string(),
        "<< /Type /Annot /Subtype /Square /Rect [400 500 460 540] /C [0 1 0] >>".to_string(),
        format!(
            "<< /Type /XObject /Subtype /Form /BBox [0 0 40 20] \
             /Resources << /Font << /F1 5 0 R >> >> /Length {} >>\nstream\n{appearance}\nendstream",
            appearance.len()
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

fn render(pdf: &[u8], annotations: bool) -> String {
    let mut doc = PdfDocument::from_bytes(pdf).expect("fixture parses");
    doc.set_render_annotations(annotations);
    format!("{:?}", doc.render_page(0, 72.0).expect("fixture renders"))
}

#[test]
fn annotations_are_drawn_by_default() {
    let pdf = build_pdf(true);
    let doc = PdfDocument::from_bytes(&pdf).expect("fixture parses");
    assert!(doc.render_annotations());
    assert_eq!(doc.page_annotations(0).expect("annotations").len(), 2);
    assert_ne!(render(&pdf, true), render(&build_pdf(false), true));
}

#[test]
fn without_annotations_a_page_is_its_content_alone() {
    // Both appearances go: the stream and the synthesized one.
    assert_eq!(
        render(&build_pdf(true), false),
        render(&build_pdf(false), true)
    );
    // The annotations are still there to read.
    let pdf = build_pdf(true);
    let mut doc = PdfDocument::from_bytes(&pdf).expect("fixture parses");
    doc.set_render_annotations(false);
    assert!(!doc.render_annotations());
    assert_eq!(doc.page_annotations(0).expect("annotations").len(), 2);
}

#[test]
fn text_in_appearances_goes_with_them() {
    let pdf = build_pdf(true);
    let texts = |annotations: bool| {
        let mut doc = PdfDocument::from_bytes(&pdf).expect("fixture parses");
        doc.set_render_annotations(annotations);
        doc.set_text_extraction(TextExtraction::Runs);
        let list = doc.render_page(0, 72.0).expect("fixture renders");
        text_runs(&list, &LayerSet::new())
            .iter()
            .map(|r| r.text.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(texts(true), ["note"]);
    assert!(texts(false).is_empty());
}
