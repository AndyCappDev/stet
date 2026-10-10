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

/// An operator that fails is skipped and the stream goes on. What it would
/// have drawn is missing, so the page says which operator and why.
#[test]
fn an_operator_that_fails_is_reported_and_the_stream_goes_on() {
    let data = two_pages(
        "",
        "/Missing Do 1 2 m 0 0 10 10 re f",
        "0 0 10 10 re f",
        &[],
    );
    let doc = PdfDocument::from_bytes(&data).unwrap();
    let list = doc.render_page(0, 72.0).unwrap();
    assert!(!list.elements().is_empty(), "the fill after it is drawn");
    doc.render_page(1, 72.0).unwrap();

    let warnings = content_warnings(&doc);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    let (page, severity, message) = &warnings[0];
    assert_eq!((*page, *severity), (Some(0), Severity::Warning));
    assert!(message.starts_with("operator Do: "), "{message}");
}

/// The same failure from every operator in a stream is one warning.
#[test]
fn an_operator_failing_the_same_way_many_times_is_reported_once() {
    let data = two_pages("", &"/Missing Do ".repeat(5000), "", &[]);
    let doc = PdfDocument::from_bytes(&data).unwrap();
    doc.render_page(0, 72.0).unwrap();
    assert_eq!(content_warnings(&doc).len(), 1);
}

/// A message can carry numbers from the file, so a hostile stream can make
/// each one different. A page keeps 64 and then says the list is cut short.
#[test]
fn a_page_lists_a_bounded_number_of_problems() {
    let xobjects: String = (0..500)
        .map(|i| format!("/X{i} {} 0 R ", 1000 + i))
        .collect();
    let content: String = (0..500).map(|i| format!("/X{i} Do ")).collect();
    let data = two_pages(
        &format!("/XObject << {xobjects} >>"),
        &content,
        "/X0 Do",
        &[],
    );
    let doc = PdfDocument::from_bytes(&data).unwrap();
    doc.render_page(0, 72.0).unwrap();

    let warnings = content_warnings(&doc);
    assert_eq!(warnings.len(), 65, "{}", warnings.len());
    assert_eq!(
        warnings[64].2,
        "further problems with this page's content are not listed"
    );

    // The next page starts its own count.
    doc.render_page(1, 72.0).unwrap();
    assert_eq!(content_warnings(&doc).len(), 66);
}

/// A form whose content ends in an error: the page carries on, and used to
/// say nothing.
#[test]
fn a_form_whose_stream_fails_is_reported() {
    let data = two_pages(
        "/XObject << /F 7 0 R >>",
        "/F Do 0 0 10 10 re f",
        "",
        &[stream(
            "/Type /XObject /Subtype /Form /BBox [0 0 100 100]",
            "0 0 5 5 re f BI /W (unterminated",
        )],
    );
    let doc = PdfDocument::from_bytes(&data).unwrap();
    doc.render_page(0, 72.0).unwrap();

    let warnings = content_warnings(&doc);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0].1, Severity::Error);
    assert!(
        warnings[0].2.starts_with("content stream error: "),
        "{}",
        warnings[0].2
    );
}

/// One of a page's content streams cannot be decoded: the others are
/// drawn, and the page names the one that was not.
#[test]
fn a_content_stream_that_cannot_be_decoded_is_reported() {
    let objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents [4 0 R 5 0 R] >>".into(),
        stream("", "0 0 10 10 re f"),
        stream("/Filter /JBIG2Decode", "not a JBIG2 stream"),
    ];
    let data = pdf_from(&objects);
    let doc = PdfDocument::from_bytes(&data).unwrap();
    let list = doc.render_page(0, 72.0).unwrap();
    assert!(!list.elements().is_empty());

    let warnings = content_warnings(&doc);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!((warnings[0].0, warnings[0].1), (Some(0), Severity::Error));
    assert!(
        warnings[0]
            .2
            .starts_with("content stream 5 0 could not be read: "),
        "{}",
        warnings[0].2
    );
}

/// A page tree nested past the reader's limit loses the pages below it,
/// and one that lists a node twice uses it once. Both are said when the
/// document is opened.
#[test]
fn a_page_tree_that_loses_pages_says_so() {
    // 300 /Pages nodes in a chain, one page at the bottom.
    let mut objects = vec!["<< /Type /Catalog /Pages 2 0 R >>".to_string()];
    for i in 0..300 {
        objects.push(format!("<< /Type /Pages /Kids [{} 0 R] /Count 1 >>", i + 3));
    }
    objects.push("<< /Type /Page /MediaBox [0 0 100 100] >>".into());
    let data = pdf_from(&objects);
    let doc = PdfDocument::from_bytes(&data).unwrap();
    assert_eq!(doc.page_count(), 0);
    let warnings = doc.parse_warnings();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0].phase, ParsePhase::PageTree);
    assert!(warnings[0].message.contains("256 levels"), "{warnings:?}");
    drop(warnings);

    let data = pdf_from(&[
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R 2 0 R 3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>".into(),
    ]);
    let doc = PdfDocument::from_bytes(&data).unwrap();
    assert_eq!(doc.page_count(), 1);
    let warnings = doc.parse_warnings();
    assert!(
        warnings.iter().all(|w| w.phase == ParsePhase::PageTree
            && matches!(w.location, Some(LocationHint::Object { .. }))),
        "{warnings:?}"
    );
    assert_eq!(warnings.len(), 2, "{warnings:?}");
}

/// A page filling a square in an ICCBased colour space whose profile
/// stream holds `profile`.
fn icc_page(profile: &str) -> Vec<u8> {
    two_pages(
        "/ColorSpace << /CS0 [/ICCBased 7 0 R] >>",
        "/CS0 cs 1 0 0 sc 0 0 50 50 re f",
        "0 0 50 50 re f",
        &[stream("/N 3 /Alternate /DeviceRGB", profile)],
    )
}

#[test]
fn an_icc_profile_that_cannot_be_used_is_reported_for_its_page() {
    let data = icc_page("this is not an ICC profile, however long it goes on for");
    let doc = PdfDocument::from_bytes(&data).unwrap();
    assert!(content_warnings(&doc).is_empty());

    // The page is still drawn, in the alternate colour space.
    assert!(!doc.render_page(0, 72.0).unwrap().is_empty());
    let warnings = content_warnings(&doc);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    let (page, severity, message) = &warnings[0];
    assert_eq!((*page, *severity), (Some(0), Severity::Warning));
    assert!(message.starts_with("ICC profile: "), "{message}");

    // Again, and through the rasterising path, adds nothing; nor does the
    // page that does not use the profile.
    doc.render_page(0, 72.0).unwrap();
    doc.render_page_to_rgba(0, 72.0).unwrap();
    doc.render_page(1, 72.0).unwrap();
    assert_eq!(content_warnings(&doc).len(), 1);
}
