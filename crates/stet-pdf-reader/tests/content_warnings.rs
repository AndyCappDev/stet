// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What goes wrong in a page's content is reported through
//! `PdfDocument::parse_warnings`, as `ParsePhase::Content` with the page as
//! its location — and not on stderr, which belongs to the application.
//! (That the reader prints nothing is enforced at build time, by the
//! `clippy::print_stderr` lint at the top of the crate.)

use stet_pdf_reader::{LocationHint, ParsePhase, PdfDocument, Severity};

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

/// Two pages with the same resources, and `extra` objects from 7 up.
fn two_pages(resources: &str, first: &str, second: &str, extra: &[String]) -> Vec<u8> {
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".into(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << {resources} >> /Contents 4 0 R >>"
        ),
        stream("", first),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << {resources} >> /Contents 6 0 R >>"
        ),
        stream("", second),
    ];
    objects.extend_from_slice(extra);
    pdf_from(&objects)
}

/// Every warning as `(page, severity, message)`.
fn content_warnings(doc: &PdfDocument) -> Vec<(Option<usize>, Severity, String)> {
    doc.parse_warnings()
        .iter()
        .map(|w| {
            assert_eq!(w.phase, ParsePhase::Content, "{w:?}");
            let page = match w.location {
                Some(LocationHint::Page(page)) => Some(page),
                None => None,
                ref other => panic!("unexpected location {other:?}"),
            };
            (page, w.severity, w.message.clone())
        })
        .collect()
}

const FINE: &str = "1 0 0 rg 10 10 50 50 re f";

#[test]
fn a_clean_page_reports_nothing() {
    let pdf = two_pages("", FINE, FINE, &[]);
    let doc = PdfDocument::from_bytes(&pdf).unwrap();
    doc.render_page(0, 72.0).unwrap();
    doc.render_page(1, 72.0).unwrap();
    assert!(doc.parse_warnings().is_empty());
}

#[test]
fn a_content_stream_error_is_reported_for_its_page() {
    // An inline image whose dictionary runs into an unterminated string
    // stops the stream. What came before it is still drawn.
    let broken = format!("{FINE} BI /W (unterminated");
    let pdf = two_pages("", FINE, &broken, &[]);
    let doc = PdfDocument::from_bytes(&pdf).unwrap();
    // Nothing is known until a page is rendered.
    assert!(doc.parse_warnings().is_empty());
    doc.render_page(0, 72.0).unwrap();
    assert!(doc.parse_warnings().is_empty());
    let list = doc.render_page(1, 72.0).unwrap();
    assert!(!list.elements().is_empty());
    let warnings = content_warnings(&doc);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    let (page, severity, message) = &warnings[0];
    assert_eq!(*page, Some(1));
    assert_eq!(*severity, Severity::Error);
    assert_eq!(message, "content stream error: unterminated string");
}

#[test]
fn rendering_a_page_again_reports_nothing_new() {
    let pdf = two_pages(
        "/Font << /F1 99 0 R >>",
        "BT /F1 12 Tf (x) Tj ET",
        FINE,
        &[],
    );
    let doc = PdfDocument::from_bytes(&pdf).unwrap();
    for _ in 0..3 {
        doc.render_page(0, 72.0).unwrap();
        doc.render_page(1, 72.0).unwrap();
    }
    assert_eq!(
        content_warnings(&doc),
        [(
            Some(0),
            Severity::Warning,
            "font /F1: object 99 0 not found".to_string()
        )]
    );
}

#[test]
fn the_same_fault_on_two_pages_is_reported_for_each() {
    let text = "BT /F1 12 Tf (x) Tj ET";
    let pdf = two_pages("/Font << /F1 99 0 R >>", text, text, &[]);
    let doc = PdfDocument::from_bytes(&pdf).unwrap();
    doc.render_page(1, 72.0).unwrap();
    doc.render_page(0, 72.0).unwrap();
    let pages: Vec<_> = content_warnings(&doc).iter().map(|w| w.0).collect();
    assert_eq!(pages, [Some(1), Some(0)]);
}

#[test]
fn a_soft_mask_or_a_graphics_state_font_that_will_not_load_is_reported() {
    let pdf = two_pages(
        "/ExtGState << /G1 << /SMask << /S /Luminosity /G 99 0 R >> >> \
         /G2 << /Font [98 0 R 12] >> >>",
        "/G1 gs 0 0 50 50 re f",
        "/G2 gs BT (x) Tj ET",
        &[],
    );
    let doc = PdfDocument::from_bytes(&pdf).unwrap();
    doc.render_page(0, 72.0).unwrap();
    doc.render_page(1, 72.0).unwrap();
    assert_eq!(
        content_warnings(&doc),
        [
            (
                Some(0),
                Severity::Warning,
                "SMask resolve error: object 99 0 not found".to_string()
            ),
            (
                Some(1),
                Severity::Warning,
                "ExtGState Font: object 98 0 not found".to_string()
            ),
        ]
    );
}

#[test]
fn a_predefined_cmap_that_is_not_installed_is_reported() {
    let font = "/Font << /F1 << /Type /Font /Subtype /Type0 /BaseFont /X \
        /Encoding /NoSuchCMap-H /DescendantFonts [<< /Type /Font \
        /Subtype /CIDFontType2 /BaseFont /X /CIDSystemInfo \
        << /Registry (Adobe) /Ordering (Japan1) /Supplement 0 >> >>] >> >>";
    let pdf = two_pages(font, "BT /F1 12 Tf <0001> Tj ET", FINE, &[]);
    let doc = PdfDocument::from_bytes(&pdf).unwrap();
    doc.render_page(0, 72.0).unwrap();
    let warnings = content_warnings(&doc);
    assert!(
        warnings.iter().any(|(page, _, message)| *page == Some(0)
            && message.starts_with("predefined CMap 'NoSuchCMap-H' not found")),
        "{warnings:?}"
    );
}

#[test]
fn an_image_that_needed_the_lenient_decoder_is_a_note_not_a_warning() {
    // Seven bytes that are not a Group 4 stream.
    let image = "BI /W 8 /H 8 /BPC 1 /CS /G /F /CCF /DP << /K -1 /Columns 8 >> ID \
        \u{1}\u{2}\u{3}\u{4}\u{5}\u{6}\u{7} EI";
    let pdf = two_pages("", image, FINE, &[]);
    let doc = PdfDocument::from_bytes(&pdf).unwrap();
    doc.render_page(0, 72.0).unwrap();
    let warnings = content_warnings(&doc);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0].0, Some(0));
    assert_eq!(warnings[0].1, Severity::Info);
    assert!(
        warnings[0].2.starts_with("CCITT image:"),
        "{}",
        warnings[0].2
    );
}

/// Nineteen forms, each drawing the next twice, spend the page's budget
/// for nested content. The page is cut short, and says so once however
/// many of the quarter-million refused runs there were.
#[test]
fn a_page_that_spends_the_nesting_budget_says_so_once() {
    let depth = 19;
    let form = |resources: &str, body: &str| {
        stream(
            &format!("/Type /XObject /Subtype /Form /BBox [0 0 100 100] {resources}"),
            body,
        )
    };
    let extra: Vec<String> = (0..depth)
        .map(|i| {
            if i + 1 < depth {
                form(
                    &format!("/Resources << /XObject << /N {} 0 R >> >>", 8 + i),
                    "/N Do /N Do",
                )
            } else {
                form("", "20 20 10 10 re f")
            }
        })
        .collect();
    let pdf = two_pages("/XObject << /N 7 0 R >>", "/N Do", FINE, &extra);
    let doc = PdfDocument::from_bytes(&pdf).unwrap();
    doc.render_page(0, 72.0).unwrap();
    let budget: Vec<_> = content_warnings(&doc)
        .into_iter()
        .filter(|(_, _, message)| message.starts_with("page has more nested content"))
        .collect();
    assert_eq!(budget.len(), 1, "{budget:?}");
    assert_eq!((budget[0].0, &budget[0].1), (Some(0), &Severity::Warning));
}

/// The list cannot grow while a caller is reading it. A render in that
/// window loses its warnings; it does not panic.
#[test]
fn rendering_while_the_warnings_are_borrowed_does_not_panic() {
    let pdf = two_pages(
        "/Font << /F1 99 0 R >>",
        "BT /F1 12 Tf (x) Tj ET",
        FINE,
        &[],
    );
    let doc = PdfDocument::from_bytes(&pdf).unwrap();
    {
        let held = doc.parse_warnings();
        doc.render_page(0, 72.0).unwrap();
        assert!(held.is_empty());
    }
    assert!(doc.parse_warnings().is_empty());
}
