// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `PdfDocument::render_page_cancellable` gives up on a page once its flag
//! is set: it returns `Ok(None)`, stops at the next token of whichever
//! content stream is running, and reports nothing about the content it did
//! not reach.
//!
//! To set the flag at a known point inside a page, these tests use the
//! font provider, which the reader calls when the page first shows text in
//! a font the file does not embed. Content after that point holds an
//! operator the reader warns about, so a warning means it was reached.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use stet_pdf_reader::PdfDocument;

fn pdf_from(objects: &[String]) -> Vec<u8> {
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

fn stream(dict: &str, body: &str) -> String {
    format!(
        "<< {dict} /Length {} >>\nstream\n{body}\nendstream",
        body.len()
    )
}

/// Text in a font the file does not embed: the reader asks the font
/// provider for it.
const TEXT: &str = "BT /F1 12 Tf 10 50 Td (a) Tj ET";

/// An operator the reader skips with a warning: the XObject is not there.
const WARNS: &str = "/Missing Do";

/// One page drawing `page`, with a form `/Fm` drawing `form` and a widget
/// annotation whose appearance draws `appearance`.
fn document(page: &str, form: &str, appearance: &str) -> Vec<u8> {
    let font = "/Font << /F1 5 0 R >>";
    pdf_from(&[
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R \
             /Annots [7 0 R] /Resources << {font} /XObject << /Fm 6 0 R >> >> >>"
        ),
        stream("", page),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        stream(
            &format!("/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Resources << {font} >>"),
            form,
        ),
        "<< /Type /Annot /Subtype /Widget /Rect [0 0 100 100] /F 4 /AP << /N 8 0 R >> >>".into(),
        stream(
            &format!("/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Resources << {font} >>"),
            appearance,
        ),
    ])
}

/// Open `pdf` with a font provider that sets `flag` when asked for a font.
fn open_setting<'a>(pdf: &'a [u8], flag: &Arc<AtomicBool>) -> PdfDocument<'a> {
    let mut doc = PdfDocument::from_bytes(pdf).unwrap();
    let flag = Arc::clone(flag);
    doc.set_font_provider(Arc::new(move |_| {
        flag.store(true, Ordering::Relaxed);
        None
    }));
    doc
}

#[test]
fn an_unset_flag_changes_nothing() {
    let pdf = document(&format!("{TEXT} /Fm Do 0 0 10 10 re f"), TEXT, TEXT);
    let doc = PdfDocument::from_bytes(&pdf).unwrap();
    let plain = doc.render_page(0, 72.0).unwrap();
    let flag = AtomicBool::new(false);
    let list = doc.render_page_cancellable(0, 72.0, &flag).unwrap();
    assert_eq!(format!("{:?}", list.unwrap()), format!("{plain:?}"));
}

#[test]
fn a_flag_set_beforehand_reads_nothing() {
    let pdf = document(WARNS, "", "");
    let doc = PdfDocument::from_bytes(&pdf).unwrap();
    let flag = AtomicBool::new(true);
    assert!(
        doc.render_page_cancellable(0, 72.0, &flag)
            .unwrap()
            .is_none()
    );
    assert!(doc.parse_warnings().is_empty());

    // The document is as usable as before: the same page, read through,
    // reaches the operator and says so.
    flag.store(false, Ordering::Relaxed);
    assert!(
        doc.render_page_cancellable(0, 72.0, &flag)
            .unwrap()
            .is_some()
    );
    assert_eq!(doc.parse_warnings().len(), 1);

    // A page that is not there is still an error, cancelled or not.
    flag.store(true, Ordering::Relaxed);
    assert!(doc.render_page_cancellable(1, 72.0, &flag).is_err());
}

#[test]
fn the_page_stops_where_the_flag_was_set() {
    let pdf = document(&format!("{TEXT} {WARNS}"), "", "");
    // Read through, the page reaches the operator after the text.
    let through = PdfDocument::from_bytes(&pdf).unwrap();
    through.render_page(0, 72.0).unwrap();
    assert_eq!(through.parse_warnings().len(), 1);

    let flag = Arc::new(AtomicBool::new(false));
    let doc = open_setting(&pdf, &flag);
    assert!(
        doc.render_page_cancellable(0, 72.0, &flag)
            .unwrap()
            .is_none()
    );
    assert!(flag.load(Ordering::Relaxed), "the provider was not asked");
    assert!(
        doc.parse_warnings().is_empty(),
        "{:?}",
        doc.parse_warnings()
    );
}

#[test]
fn a_flag_set_inside_a_form_stops_the_form_and_the_page() {
    let pdf = document(
        &format!("/Fm Do {WARNS}"),
        &format!("{TEXT} {WARNS}"),
        WARNS,
    );
    let through = PdfDocument::from_bytes(&pdf).unwrap();
    through.render_page(0, 72.0).unwrap();
    // The page's, and the one the form and the annotation appearance
    // share: a warning repeated on a page is reported once.
    assert_eq!(through.parse_warnings().len(), 2);

    let flag = Arc::new(AtomicBool::new(false));
    let doc = open_setting(&pdf, &flag);
    assert!(
        doc.render_page_cancellable(0, 72.0, &flag)
            .unwrap()
            .is_none()
    );
    assert!(flag.load(Ordering::Relaxed), "the provider was not asked");
    assert!(
        doc.parse_warnings().is_empty(),
        "{:?}",
        doc.parse_warnings()
    );
}

#[test]
fn a_flag_set_inside_an_annotation_appearance_abandons_the_page() {
    let pdf = document("0 0 10 10 re f", "", &format!("{TEXT} {WARNS}"));
    let flag = Arc::new(AtomicBool::new(false));
    let doc = open_setting(&pdf, &flag);
    assert!(
        doc.render_page_cancellable(0, 72.0, &flag)
            .unwrap()
            .is_none()
    );
    assert!(flag.load(Ordering::Relaxed), "the provider was not asked");
    assert!(
        doc.parse_warnings().is_empty(),
        "{:?}",
        doc.parse_warnings()
    );
}
