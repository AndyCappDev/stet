// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `PdfDocument::from_owned`: a document that owns its bytes, and so
//! borrows nothing.

use std::sync::Arc;

use stet_graphics::icc::IccCache;
use stet_pdf_reader::{PdfDocument, PdfError};

/// One page, RC4-encrypted by qpdf; the user password is `user`.
const ENCRYPTED: &[u8] = include_bytes!("data/encryption/r3.pdf");

fn one_page() -> Vec<u8> {
    let content = "1 0 0 rg 10 10 50 50 re f";
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>".to_string(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
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

fn rendered(doc: &PdfDocument) -> String {
    format!("{:?}", doc.render_page(0, 72.0).expect("page renders"))
}

/// The point of it: nothing the caller holds has to outlive the document.
fn open_and_drop_the_buffer() -> PdfDocument<'static> {
    let bytes = one_page();
    PdfDocument::from_owned(bytes).expect("fixture parses")
}

#[test]
fn an_owned_document_outlives_the_buffer_it_was_made_from() {
    let doc = open_and_drop_the_buffer();
    assert_eq!(doc.page_count(), 1);

    let bytes = one_page();
    let borrowed = PdfDocument::from_bytes(&bytes).expect("fixture parses");
    assert_eq!(rendered(&doc), rendered(&borrowed));
}

#[test]
fn a_vec_a_box_and_an_arc_all_open_the_same_document() {
    let bytes = one_page();
    let expected = rendered(&PdfDocument::from_bytes(&bytes).unwrap());

    let from_vec = PdfDocument::from_owned(bytes.clone()).unwrap();
    let from_box = PdfDocument::from_owned(bytes.clone().into_boxed_slice()).unwrap();
    let shared: Arc<[u8]> = Arc::from(bytes);
    let from_arc = PdfDocument::from_owned(Arc::clone(&shared)).unwrap();
    for doc in [&from_vec, &from_box, &from_arc] {
        assert_eq!(rendered(doc), expected);
    }

    // An Arc is shared, not copied: the document holds the second handle
    // and gives it up when dropped.
    assert_eq!(Arc::strong_count(&shared), 2);
    drop(from_arc);
    assert_eq!(Arc::strong_count(&shared), 1);
}

/// A failed open consumes what it was given, so a caller that will ask for
/// a password and try again keeps an `Arc` of the bytes.
#[test]
fn an_arc_lets_a_caller_try_again_with_a_password() {
    let shared: Arc<[u8]> = Arc::from(ENCRYPTED);
    match PdfDocument::from_owned(Arc::clone(&shared)) {
        Err(PdfError::PasswordRequired) => {}
        Err(e) => panic!("expected PasswordRequired, got {e}"),
        Ok(_) => panic!("opened without a password"),
    }
    assert_eq!(Arc::strong_count(&shared), 1);
    let doc = PdfDocument::from_owned_with_password(Arc::clone(&shared), IccCache::new(), b"user")
        .expect("the user password opens it");
    assert_eq!(
        doc.metadata().title.as_deref(),
        Some("stet encryption fixture")
    );
    assert!(!doc.render_page(0, 72.0).unwrap().elements().is_empty());
}

#[test]
fn an_owned_document_takes_a_prepared_icc_cache() {
    let doc = PdfDocument::from_owned_with_icc(one_page(), IccCache::new()).unwrap();
    assert_eq!(doc.page_count(), 1);
}

#[test]
fn bytes_that_are_not_a_pdf_are_refused() {
    assert!(matches!(
        PdfDocument::from_owned(b"not a pdf".to_vec()),
        Err(PdfError::NotAPdf)
    ));
}

/// Whatever threads a borrowed document can cross, an owned one can too:
/// owning the bytes must not cost `Send`.
#[test]
fn owning_the_bytes_does_not_cost_send() {
    fn is_send<T: Send>() {}
    is_send::<stet_pdf_reader::PdfBytes>();
    is_send::<PdfDocument<'static>>();
}
