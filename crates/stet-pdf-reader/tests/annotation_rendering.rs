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

// --- `PdfDocument::set_annotation_filter` -------------------------------

use stet_pdf_reader::{AnnotationClass, AnnotationFilter, RenderIntent};

const HIDDEN: u32 = 0x02;
const PRINT: u32 = 0x04;
const NO_VIEW: u32 = 0x20;

/// A one-page PDF with one annotation per `(subtype, flags, label)`, each
/// with an appearance stream that shows its label — so the text a render
/// extracts is the list of annotations it drew.
fn labelled_pdf(annots: &[(&str, u32, &str)]) -> Vec<u8> {
    let content = "0 0 1 rg 100 100 200 200 re f";
    // Objects 1 to 5 are fixed; annotation `i` is object 6 + 2i and its
    // appearance is 7 + 2i.
    let refs: Vec<String> = (0..annots.len())
        .map(|i| format!("{} 0 R", 6 + 2 * i))
        .collect();
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
             /Annots [{}] >>",
            refs.join(" ")
        ),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
    for (i, (subtype, flags, label)) in annots.iter().enumerate() {
        let y = 100 + 30 * i;
        objects.push(format!(
            "<< /Type /Annot /Subtype /{subtype} /F {flags} /Rect [400 {y} 500 {}] \
             /AP << /N {} 0 R >> >>",
            y + 20,
            7 + 2 * i
        ));
        let appearance = format!("BT /F1 12 Tf 2 5 Td ({label}) Tj ET");
        objects.push(format!(
            "<< /Type /XObject /Subtype /Form /BBox [0 0 100 20] \
             /Resources << /Font << /F1 5 0 R >> >> /Length {} >>\nstream\n{appearance}\nendstream",
            appearance.len()
        ));
    }
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

/// The labels of the annotations a render draws: under `filter`, or under
/// the document's default when `None`.
fn drawn(pdf: &[u8], filter: Option<AnnotationFilter>) -> Vec<String> {
    let mut doc = PdfDocument::from_bytes(pdf).expect("fixture parses");
    if let Some(filter) = filter {
        doc.set_annotation_filter(filter);
        assert_eq!(doc.annotation_filter(), filter);
    }
    doc.set_text_extraction(TextExtraction::Runs);
    let list = doc.render_page(0, 72.0).expect("fixture renders");
    text_runs(&list, &LayerSet::new())
        .iter()
        .map(|r| r.text.clone())
        .collect()
}

/// One annotation for each combination of the three flags that decide
/// whether it is drawn.
fn flag_fixture() -> Vec<u8> {
    labelled_pdf(&[
        ("Square", 0, "plain"),
        ("Square", PRINT, "print"),
        ("Square", NO_VIEW, "noview"),
        ("Square", NO_VIEW | PRINT, "noview-print"),
        ("Square", HIDDEN, "hidden"),
        ("Square", HIDDEN | PRINT, "hidden-print"),
    ])
}

#[test]
fn a_render_is_for_viewing_by_default() {
    // Hidden and NoView annotations are not shown on screen; the Print
    // flag has no say in it.
    assert_eq!(drawn(&flag_fixture(), None), ["plain", "print"]);
    assert_eq!(
        drawn(&flag_fixture(), Some(AnnotationFilter::default())),
        ["plain", "print"]
    );
    assert_eq!(AnnotationFilter::default().intent(), RenderIntent::View);
    // Export reads the flags as viewing does.
    assert_eq!(
        drawn(
            &flag_fixture(),
            Some(AnnotationFilter::new(RenderIntent::Export))
        ),
        ["plain", "print"]
    );
}

#[test]
fn a_render_for_print_draws_what_has_the_print_flag() {
    // NoView does not matter on paper; Hidden still does.
    assert_eq!(
        drawn(
            &flag_fixture(),
            Some(AnnotationFilter::new(RenderIntent::Print))
        ),
        ["print", "noview-print"]
    );
    assert_eq!(
        drawn(
            &flag_fixture(),
            Some(AnnotationFilter::default().with_intent(RenderIntent::Print))
        ),
        ["print", "noview-print"]
    );
}

/// One annotation of each class, and of several subtypes within the
/// markup class.
fn class_fixture() -> Vec<u8> {
    labelled_pdf(&[
        ("Highlight", 0, "highlight"),
        ("Widget", 0, "widget"),
        ("Link", 0, "link"),
        ("Stamp", 0, "stamp"),
        ("Watermark", 0, "watermark"),
        ("Redact", 0, "redact"),
        ("FreeText", 0, "freetext"),
        ("Invented", 0, "invented"),
    ])
}

#[test]
fn classes_are_drawn_or_not_independently() {
    let pdf = class_fixture();
    let all = AnnotationFilter::default();
    assert_eq!(
        drawn(&pdf, Some(all)),
        [
            "highlight",
            "widget",
            "link",
            "stamp",
            "watermark",
            "redact",
            "freetext",
            "invented"
        ]
    );
    // An editor that draws review comments itself keeps the form fields.
    assert_eq!(
        drawn(&pdf, Some(all.with_class(AnnotationClass::Markup, false))),
        ["widget", "link", "watermark", "invented"]
    );
    assert_eq!(
        drawn(&pdf, Some(all.with_class(AnnotationClass::Widget, false))),
        [
            "highlight",
            "link",
            "stamp",
            "watermark",
            "redact",
            "freetext",
            "invented"
        ]
    );
    assert_eq!(
        drawn(&pdf, Some(all.with_class(AnnotationClass::Link, false))).len(),
        7
    );
    assert_eq!(
        drawn(&pdf, Some(all.with_class(AnnotationClass::Other, false))),
        ["highlight", "widget", "link", "stamp", "redact", "freetext"]
    );
    // Only the widgets.
    let widgets_only = all
        .with_class(AnnotationClass::Markup, false)
        .with_class(AnnotationClass::Link, false)
        .with_class(AnnotationClass::Other, false);
    assert_eq!(drawn(&pdf, Some(widgets_only)), ["widget"]);
    // Turning a class back on restores it.
    assert_eq!(
        widgets_only
            .with_class(AnnotationClass::Markup, true)
            .with_class(AnnotationClass::Link, true)
            .with_class(AnnotationClass::Other, true),
        all
    );
}

/// `AnnotationFilter::draws` is the renderer's own test, for a caller that
/// draws the remainder: asked of each annotation the structural API
/// returns, it names exactly the ones a render includes.
#[test]
fn the_filter_answers_for_an_annotation_what_the_renderer_does() {
    let filters = [
        AnnotationFilter::default(),
        AnnotationFilter::new(RenderIntent::Print),
        AnnotationFilter::default().with_class(AnnotationClass::Markup, false),
        AnnotationFilter::new(RenderIntent::Print).with_class(AnnotationClass::Widget, false),
    ];
    let labels = [
        ("Highlight", 0, "a"),
        ("Highlight", PRINT, "b"),
        ("Widget", NO_VIEW | PRINT, "c"),
        ("Widget", PRINT, "d"),
        ("Link", HIDDEN, "e"),
        ("Link", 0, "f"),
        ("Screen", PRINT, "g"),
    ];
    let pdf = labelled_pdf(&labels);
    let doc = PdfDocument::from_bytes(&pdf).expect("fixture parses");
    let annots = doc.page_annotations(0).expect("annotations");
    assert_eq!(annots.len(), labels.len());
    for filter in filters {
        let predicted: Vec<&str> = annots
            .iter()
            .zip(labels)
            .filter(|(a, _)| filter.draws(a))
            .map(|(_, (_, _, label))| label)
            .collect();
        assert_eq!(drawn(&pdf, Some(filter)), predicted, "{filter:?}");
        assert!(!predicted.is_empty() && predicted.len() < labels.len());
    }
}

#[test]
fn the_filter_does_nothing_while_annotations_are_off() {
    let pdf = class_fixture();
    let mut doc = PdfDocument::from_bytes(&pdf).expect("fixture parses");
    doc.set_render_annotations(false);
    doc.set_annotation_filter(AnnotationFilter::default());
    doc.set_text_extraction(TextExtraction::Runs);
    let list = doc.render_page(0, 72.0).expect("fixture renders");
    assert!(text_runs(&list, &LayerSet::new()).is_empty());
}
