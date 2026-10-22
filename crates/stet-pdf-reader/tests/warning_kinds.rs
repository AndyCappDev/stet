// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What the reader repaired, and what it had to leave out, is told apart in
//! `PdfDocument::parse_warnings` by kind, so that an application can act on
//! it without reading messages: an image left out for its size, an image
//! that cannot be read, a cross-reference table rebuilt, a page tree that
//! could not be followed, a content stream the end of the file cuts short.

use stet_graphics::display_list::DisplayElement;
use stet_pdf_reader::{LocationHint, ParsePhase, ParseWarning, PdfDocument, Severity};

/// A PDF of `objects`, numbered from 1, with a cross-reference table.
fn pdf_from(objects: &[Vec<u8>]) -> Vec<u8> {
    let mut pdf = body_of(objects);
    let offsets = offsets_of(&pdf, objects.len());
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

/// The header and the objects of a PDF, with nothing after them.
fn body_of(objects: &[Vec<u8>]) -> Vec<u8> {
    let mut pdf = b"%PDF-1.7\n".to_vec();
    for (i, body) in objects.iter().enumerate() {
        pdf.extend(format!("{} 0 obj\n", i + 1).as_bytes());
        pdf.extend(body);
        pdf.extend(b"\nendobj\n");
    }
    pdf
}

/// Where each of the first `count` objects starts in `pdf`.
fn offsets_of(pdf: &[u8], count: usize) -> Vec<usize> {
    (1..=count)
        .map(|n| {
            let header = format!("\n{n} 0 obj\n");
            pdf.windows(header.len())
                .position(|w| w == header.as_bytes())
                .expect("object header")
                + 1
        })
        .collect()
}

fn stream(dict: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!("<< {dict} /Length {} >>\nstream\n", body.len()).into_bytes();
    out.extend_from_slice(body);
    out.extend_from_slice(b"\nendstream");
    out
}

/// Objects 1 to 6: a catalog, a page tree of two pages (3 and 5) with
/// `resources`, and their content streams (4 and 6); `extra` from 7 up.
fn two_page_objects(
    resources: &str,
    first: &str,
    second: &str,
    extra: Vec<Vec<u8>>,
) -> Vec<Vec<u8>> {
    let page = |contents: u32| {
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
             /Resources << {resources} >> /Contents {contents} 0 R >>"
        )
        .into_bytes()
    };
    let mut objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".to_vec(),
        page(4),
        stream("", first.as_bytes()),
        page(6),
        stream("", second.as_bytes()),
    ];
    objects.extend(extra);
    objects
}

const FINE: &str = "1 0 0 rg 10 10 50 50 re f";

/// Every warning as `(phase, location, severity)`.
fn kinds(doc: &PdfDocument) -> Vec<(ParsePhase, Option<LocationHint>, Severity)> {
    doc.parse_warnings()
        .iter()
        .map(|w| (w.phase.clone(), w.location.clone(), w.severity))
        .collect()
}

fn render_all(doc: &PdfDocument) {
    for page in 0..doc.page_count() {
        doc.render_page(page, 72.0).unwrap();
    }
}

// === Images left out for their size ========================================

/// A 400 x 400 grey image, run-length coded (0x81 0x00 is "two zeros";
/// 80,000 of them make 160,000 samples).
fn grey_image() -> Vec<u8> {
    stream(
        "/Type /XObject /Subtype /Image /Width 400 /Height 400 \
         /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /RunLengthDecode",
        &[0x81, 0x00].repeat(80_000),
    )
}

/// The image is over the ceiling on both pages that draw it, and each page
/// reports it once, with its size, however often it is rendered. None of
/// it is a content warning.
#[test]
fn an_image_over_the_ceiling_is_its_own_kind_once_for_each_page() {
    let draw = "q 100 0 0 100 0 0 cm /G Do Q";
    let pdf = pdf_from(&two_page_objects(
        "/XObject << /G 7 0 R >>",
        draw,
        draw,
        vec![grey_image()],
    ));
    let mut doc = PdfDocument::from_bytes(&pdf).unwrap();
    doc.set_max_image_pixels(Some(100_000));
    for _ in 0..2 {
        render_all(&doc);
    }
    let left_out = ParsePhase::ImageLeftOut {
        width: 400,
        height: 400,
    };
    assert_eq!(
        kinds(&doc),
        [
            (
                left_out.clone(),
                Some(LocationHint::Page(0)),
                Severity::Warning
            ),
            (left_out, Some(LocationHint::Page(1)), Severity::Warning),
        ]
    );

    // Under no ceiling it is drawn and nothing is said.
    let doc = PdfDocument::from_bytes(&pdf).unwrap();
    render_all(&doc);
    assert!(
        doc.parse_warnings().is_empty(),
        "{:?}",
        doc.parse_warnings()
    );
}

/// An inline image over the ceiling is reported the same way.
#[test]
fn an_inline_image_over_the_ceiling_is_its_own_kind() {
    let inline = "q 100 0 0 100 0 0 cm BI /W 400 /H 300 /BPC 8 /CS /G ID \u{0} EI Q";
    let pdf = pdf_from(&two_page_objects("", inline, FINE, Vec::new()));
    let mut doc = PdfDocument::from_bytes(&pdf).unwrap();
    doc.set_max_image_pixels(Some(100_000));
    render_all(&doc);
    assert_eq!(
        kinds(&doc),
        [(
            ParsePhase::ImageLeftOut {
                width: 400,
                height: 300
            },
            Some(LocationHint::Page(0)),
            Severity::Warning
        )]
    );
}

/// A JPEG 2000 image drawn small is decoded at a reduced size, and it is
/// that size the ceiling is held to. Left out all the same, it is reported
/// at the size the file stores it at. So is an image whose soft mask is
/// the part over the ceiling.
#[cfg(feature = "jpx")]
#[test]
fn an_image_left_out_is_reported_at_its_stored_size() {
    use stet_pdf_reader::ImageResolution;

    let photo = |extra: &str| {
        stream(
            &format!(
                "/Type /XObject /Subtype /Image /Width 512 /Height 384 \
                 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /JPXDecode {extra}"
            ),
            include_bytes!("data/jpx/photo-512x384.jp2"),
        )
    };
    // Drawn at 128 x 96 pixels, which is what it is decoded at.
    let small = "q 128 0 0 96 0 0 cm /Im Do Q";
    let stored = ParsePhase::ImageLeftOut {
        width: 512,
        height: 384,
    };

    // 128 x 96 is 12,288 samples: under 20,000 it is drawn, under 10,000
    // it is not.
    for (limit, expected) in [(20_000, Vec::new()), (10_000, vec![stored.clone()])] {
        let pdf = pdf_from(&two_page_objects(
            "/XObject << /Im 7 0 R >>",
            small,
            FINE,
            vec![photo("")],
        ));
        let mut doc = PdfDocument::from_bytes(&pdf).unwrap();
        doc.set_image_resolution(ImageResolution::Rendered);
        doc.set_max_image_pixels(Some(limit));
        doc.render_page(0, 72.0).unwrap();
        let phases: Vec<_> = doc
            .parse_warnings()
            .iter()
            .map(|w| w.phase.clone())
            .collect();
        assert_eq!(phases, expected, "limit {limit}");
    }

    // The photo would pass at its reduced size; its 400 x 400 soft mask
    // does not, and takes the photo with it.
    let pdf = pdf_from(&two_page_objects(
        "/XObject << /Im 7 0 R >>",
        small,
        FINE,
        vec![photo("/SMask 8 0 R"), grey_image()],
    ));
    let mut doc = PdfDocument::from_bytes(&pdf).unwrap();
    doc.set_image_resolution(ImageResolution::Rendered);
    doc.set_max_image_pixels(Some(100_000));
    doc.render_page(0, 72.0).unwrap();
    assert_eq!(
        kinds(&doc),
        [(stored, Some(LocationHint::Page(0)), Severity::Warning)]
    );
    assert!(
        doc.parse_warnings()[0].message.contains("400 x 400"),
        "the message says what was over the limit: {:?}",
        doc.parse_warnings()
    );
}

// === Images that cannot be read ===========================================

/// A 40 x 40 gray JPEG cut off after its first marker: its data cannot be
/// decoded.
fn broken_jpeg() -> Vec<u8> {
    stream(
        "/Type /XObject /Subtype /Image /Width 40 /Height 40 \
         /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /DCTDecode",
        b"\xff\xd8\xff\xe0 cut short",
    )
}

/// An image whose data cannot be decoded is its own kind, for the page that
/// draws it, and none of it is a content warning: the image is left out and
/// the rest of the page is drawn.
#[test]
fn an_image_that_cannot_be_read_is_its_own_kind_and_the_rest_of_the_page_is_drawn() {
    let draw = "q 100 0 0 100 0 0 cm /J Do Q 1 0 0 rg 10 10 50 50 re f";
    let pdf = pdf_from(&two_page_objects(
        "/XObject << /J 7 0 R >>",
        draw,
        FINE,
        vec![broken_jpeg()],
    ));
    let doc = PdfDocument::from_bytes(&pdf).unwrap();
    let list = doc.render_page(0, 72.0).unwrap();
    doc.render_page(1, 72.0).unwrap();
    assert_eq!(
        kinds(&doc),
        [(
            ParsePhase::ImageUnreadable,
            Some(LocationHint::Page(0)),
            Severity::Warning
        )]
    );
    let drawn: Vec<&str> = list
        .elements()
        .iter()
        .map(|element| match element {
            DisplayElement::Image { .. } => "image",
            DisplayElement::Fill { .. } => "fill",
            _ => "other",
        })
        .collect();
    assert_eq!(drawn, ["fill"], "the image is left out, the rest is drawn");
}

// === The cross-reference table ============================================

/// With no cross-reference table at all, the objects are found by scanning
/// the file. Every page is there, so that is a note, not a warning.
#[test]
fn a_rebuilt_cross_reference_table_with_every_page_found_is_info() {
    let mut pdf = body_of(&two_page_objects("", FINE, FINE, Vec::new()));
    pdf.extend(b"%%EOF\n");
    let doc = PdfDocument::from_bytes(&pdf).unwrap();
    assert_eq!(doc.page_count(), 2);
    render_all(&doc);
    assert_eq!(
        kinds(&doc),
        [(ParsePhase::CrossReference, None, Severity::Info)]
    );
}

/// A table that cannot be read is rebuilt the same way.
#[test]
fn a_damaged_cross_reference_table_is_rebuilt_and_said_so() {
    let pdf = pdf_from(&two_page_objects("", FINE, FINE, Vec::new()));
    let text = String::from_utf8(pdf).unwrap();
    // startxref still points at the table, which no longer starts with
    // the keyword that makes it one.
    let pdf = text.replace("\nxref\n0 ", "\nxxxx\n0 ").into_bytes();
    let doc = PdfDocument::from_bytes(&pdf).unwrap();
    assert_eq!(doc.page_count(), 2);
    assert_eq!(
        kinds(&doc),
        [(ParsePhase::CrossReference, None, Severity::Info)]
    );
}

// === The page tree =======================================================

/// The catalog names a page tree that is not in the file. The pages are
/// found by scanning for page objects instead, and that is a warning: a
/// scan can miss pages, or find ones an update replaced.
#[test]
fn a_page_list_found_by_scanning_is_a_page_tree_warning() {
    let mut objects = two_page_objects("", FINE, FINE, Vec::new());
    objects[0] = b"<< /Type /Catalog /Pages 99 0 R >>".to_vec();
    let pdf = pdf_from(&objects);
    let doc = PdfDocument::from_bytes(&pdf).unwrap();
    assert_eq!(doc.page_count(), 2);
    assert_eq!(
        kinds(&doc),
        [(ParsePhase::PageTree, None, Severity::Warning)]
    );
}

/// A tree listing a great many nodes twice lists the first 64 and then
/// says the list is cut short, rather than one warning for each: every
/// warning recorded is checked against those before it, and opening the
/// file cannot be canceled.
#[test]
fn a_page_tree_lists_a_bounded_number_of_warnings() {
    let nodes: Vec<Vec<u8>> = (0..300)
        .map(|_| b"<< /Type /Pages /Kids [] /Count 0 >>".to_vec())
        .collect();
    let twice: String = (7..307).map(|n| format!("{n} 0 R {n} 0 R ")).collect();
    let mut objects = two_page_objects("", FINE, FINE, nodes);
    objects[1] = format!("<< /Type /Pages /Kids [3 0 R {twice}5 0 R] /Count 2 >>").into_bytes();
    let pdf = pdf_from(&objects);
    let doc = PdfDocument::from_bytes(&pdf).unwrap();
    assert_eq!(doc.page_count(), 2);
    let warnings = doc.parse_warnings();
    assert_eq!(warnings.len(), 65, "{:?}", warnings.last());
    assert!(warnings.iter().all(|w| w.phase == ParsePhase::PageTree));
    assert_eq!(
        warnings[63].location,
        Some(LocationHint::Object {
            obj_num: 70,
            gen_num: 0
        })
    );
    assert_eq!(warnings[64].location, None);
    assert!(
        warnings[64].message.contains("not listed"),
        "{:?}",
        warnings[64]
    );
}

/// One node listed many times is one warning, and does not use up the
/// tree's share: no note says that problems are not listed.
#[test]
fn a_node_listed_many_times_is_one_page_tree_warning() {
    let node = b"<< /Type /Pages /Kids [] /Count 0 >>".to_vec();
    let again: String = "7 0 R ".repeat(300);
    let mut objects = two_page_objects("", FINE, FINE, vec![node]);
    objects[1] = format!("<< /Type /Pages /Kids [3 0 R {again}5 0 R] /Count 2 >>").into_bytes();
    let pdf = pdf_from(&objects);
    let doc = PdfDocument::from_bytes(&pdf).unwrap();
    assert_eq!(doc.page_count(), 2);
    assert_eq!(
        kinds(&doc),
        [(
            ParsePhase::PageTree,
            Some(LocationHint::Object {
                obj_num: 7,
                gen_num: 0
            }),
            Severity::Warning
        )]
    );
}

/// A page with a hundred images over the ceiling, each a different size,
/// lists the first 64 of them. Other kinds have limits of their own: the
/// font the page does not embed is still reported after them.
#[test]
fn a_page_lists_a_bounded_number_of_images_left_out() {
    let images: String = (0..100)
        .map(|i| format!("BI /W {} /H 400 /BPC 8 /CS /G ID \u{0} EI\n", 300 + i))
        .collect();
    let content = format!("{images}BT /F1 12 Tf 10 10 Td (x) Tj ET");
    let font = "/Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Garamond >> >>";
    let pdf = pdf_from(&two_page_objects(font, &content, FINE, Vec::new()));
    let mut doc = PdfDocument::from_bytes(&pdf).unwrap();
    doc.set_max_image_pixels(Some(100_000));
    doc.render_page(0, 72.0).unwrap();
    let warnings = doc.parse_warnings();
    let left_out: Vec<_> = warnings
        .iter()
        .filter(|w| matches!(w.phase, ParsePhase::ImageLeftOut { .. }))
        .collect();
    assert_eq!(left_out.len(), 64);
    assert_eq!(
        left_out[63].phase,
        ParsePhase::ImageLeftOut {
            width: 363,
            height: 400
        }
    );
    assert!(
        warnings.iter().any(
            |w| w.phase == ParsePhase::MissingFont && w.location == Some(LocationHint::Page(0))
        ),
        "{warnings:?}"
    );
}

// === Content cut short ======================================================

/// A file cut off part-way through the second page's content stream. The
/// cross-reference table went with the end of the file and is rebuilt;
/// the first page is whole, and the second says what it lost.
#[test]
fn a_content_stream_cut_by_the_end_of_the_file_is_a_content_warning() {
    let long = format!("{FINE}\n").repeat(40);
    let pdf = pdf_from(&two_page_objects("", FINE, &long, Vec::new()));
    let start = offsets_of(&pdf, 6)[5];
    let cut = start + 100;
    let doc = PdfDocument::from_bytes(&pdf[..cut]).unwrap();
    assert_eq!(doc.page_count(), 2);
    doc.render_page(0, 72.0).unwrap();
    assert_eq!(
        kinds(&doc),
        [(ParsePhase::CrossReference, None, Severity::Info)]
    );
    let list = doc.render_page(1, 72.0).unwrap();
    assert!(!list.elements().is_empty(), "what is left is drawn");
    let cut_short: Vec<ParseWarning> = doc
        .parse_warnings()
        .iter()
        .filter(|w| w.phase == ParsePhase::Content)
        .cloned()
        .collect();
    assert_eq!(cut_short.len(), 1, "{:?}", doc.parse_warnings());
    assert_eq!(cut_short[0].location, Some(LocationHint::Page(1)));
    assert!(
        matches!(cut_short[0].severity, Severity::Warning | Severity::Error),
        "{:?}",
        cut_short[0]
    );
    assert!(
        cut_short[0]
            .message
            .contains("cut short by the end of the file"),
        "{:?}",
        cut_short[0]
    );

    // The whole file says nothing of the kind.
    let doc = PdfDocument::from_bytes(&pdf).unwrap();
    render_all(&doc);
    assert!(
        doc.parse_warnings().is_empty(),
        "{:?}",
        doc.parse_warnings()
    );
}

/// A form whose stream the end of the file cuts short is reported for the
/// page that draws it.
#[test]
fn a_form_cut_by_the_end_of_the_file_is_a_content_warning() {
    let long = format!("{FINE}\n").repeat(40);
    let form = stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 100 100]",
        long.as_bytes(),
    );
    let pdf = pdf_from(&two_page_objects(
        "/XObject << /Fm 7 0 R >>",
        FINE,
        "/Fm Do",
        vec![form],
    ));
    let start = offsets_of(&pdf, 7)[6];
    let doc = PdfDocument::from_bytes(&pdf[..start + 120]).unwrap();
    render_all(&doc);
    let content: Vec<_> = doc
        .parse_warnings()
        .iter()
        .filter(|w| w.phase == ParsePhase::Content)
        .map(|w| (w.location.clone(), w.severity))
        .collect();
    assert_eq!(
        content,
        [(Some(LocationHint::Page(1)), Severity::Warning)],
        "{:?}",
        doc.parse_warnings()
    );
}
