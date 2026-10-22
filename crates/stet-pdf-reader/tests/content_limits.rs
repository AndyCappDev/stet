// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The limits on what one page's content may ask of the reader:
//! [`MAX_SAVED_STATES`] graphics states saved at once, and the bytes of
//! content set with `PdfDocument::set_max_content_bytes`.
//!
//! Both count the forms the page draws and the appearances of its
//! annotations and form fields.
//!
//! A page over either is not drawn. `render_page` returns
//! `PdfError::ContentLimit`, which is its own kind of error, nothing is
//! added to the parse warnings for it, and the document's other pages read
//! as before. A page within both reads exactly as it does with no limit.

use std::io::Write;

use flate2::Compression;
use flate2::write::ZlibEncoder;
use stet_graphics::display_list::DisplayList;
use stet_graphics::text::{text_lines, text_runs};
use stet_pdf_reader::{
    ContentLimit, LayerSet, MAX_SAVED_STATES, PdfDocument, PdfError, TextExtraction,
};

/// A PDF of `objects`, numbered from 1, with a cross-reference table.
fn pdf_from(objects: &[Vec<u8>]) -> Vec<u8> {
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend(format!("{} 0 obj\n", i + 1).as_bytes());
        pdf.extend(body);
        pdf.extend(b"\nendobj\n");
    }
    let xref = pdf.len();
    pdf.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for off in offsets {
        pdf.extend(format!("{off:010} 00000 n \n").as_bytes());
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

/// A stream object with `dict` entries and `data`.
fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut out = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    out.extend(data);
    out.extend(b"\nendstream");
    out
}

/// `data` compressed with Flate.
fn deflate(data: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

/// A form XObject showing `data`, with the page's font as its own.
fn form(data: &[u8]) -> Vec<u8> {
    stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 612 792] \
         /Resources << /Font << /F1 3 0 R >> >>",
        data,
    )
}

/// A document of one page for each content stream in `pages`, each page
/// with the font `/F1` and the form `/Fm`, which is `form_object`.
/// Objects: 1 catalog, 2 pages, 3 font, 4 the form, then each page and its
/// content.
fn document(pages: &[Vec<u8>], form_object: Vec<u8>) -> Vec<u8> {
    let first_page = 5;
    let kids: Vec<String> = (0..pages.len())
        .map(|i| format!("{} 0 R", first_page + 2 * i))
        .collect();
    let mut objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>",
            kids.join(" "),
            pages.len()
        )
        .into_bytes(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_vec(),
        form_object,
    ];
    for (i, content) in pages.iter().enumerate() {
        let page = first_page + 2 * i;
        objects.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {} 0 R \
                 /Resources << /Font << /F1 3 0 R >> /XObject << /Fm 4 0 R >> >> >>",
                page + 1
            )
            .into_bytes(),
        );
        objects.push(content.clone());
    }
    pdf_from(&objects)
}

/// A document of one page with `content` as its stream, unfiltered, and a
/// form showing "form".
fn one_page(content: &[u8]) -> Vec<u8> {
    document(
        &[stream("", content)],
        form(b"BT /F1 12 Tf 72 600 Td (form) Tj ET"),
    )
}

/// The text lines of `list`.
fn text_of(list: &DisplayList) -> Vec<String> {
    text_lines(text_runs(list, &LayerSet::new()))
        .into_iter()
        .map(|line| line.text)
        .collect()
}

fn open(pdf: &[u8]) -> PdfDocument<'_> {
    let mut doc = PdfDocument::from_bytes(pdf).unwrap();
    doc.set_text_extraction(TextExtraction::Runs);
    doc
}

/// `count` levels of `q`, then text, then the `Q` that close them.
fn nested_saves(count: usize) -> Vec<u8> {
    let mut content = b"q ".repeat(count);
    content.extend(b"BT /F1 12 Tf 72 700 Td (inside) Tj ET ");
    content.extend(b"Q ".repeat(count));
    content
}

#[test]
fn a_page_saving_as_many_states_as_the_limit_is_read() {
    let pdf = one_page(&nested_saves(MAX_SAVED_STATES));
    let doc = open(&pdf);
    let list = doc.render_page(0, 72.0).unwrap();
    assert_eq!(text_of(&list), ["inside"]);
    assert!(doc.parse_warnings().is_empty());
}

#[test]
fn a_page_saving_one_state_more_than_the_limit_is_stopped() {
    let pdf = one_page(&nested_saves(MAX_SAVED_STATES + 1));
    let doc = open(&pdf);
    let err = doc.render_page(0, 72.0).unwrap_err();
    assert!(
        matches!(
            err,
            PdfError::ContentLimit(ContentLimit::SavedStates(MAX_SAVED_STATES))
        ),
        "{err:?}"
    );
    assert!(
        doc.parse_warnings().is_empty(),
        "{:?}",
        doc.parse_warnings()
    );
}

/// The file that showed the need: a small Flate stream decoding to `q`
/// after `q`, two million of them here. It ends at the limit, not at the
/// end of the stream.
#[test]
fn a_stream_of_unmatched_saves_is_stopped_at_the_limit() {
    let content = deflate(&b"q ".repeat(2_000_000));
    let pdf = document(
        &[stream("/Filter /FlateDecode", &content)],
        form(b"BT /F1 12 Tf 72 600 Td (form) Tj ET"),
    );
    assert!(pdf.len() < 20_000, "{} bytes", pdf.len());
    let doc = open(&pdf);
    let err = doc.render_page(0, 72.0).unwrap_err();
    assert!(
        matches!(
            err,
            PdfError::ContentLimit(ContentLimit::SavedStates(MAX_SAVED_STATES))
        ),
        "{err:?}"
    );
}

/// The state a form saves around its content counts with the page's own.
/// Here the page saves two short of the limit and the form one more, so
/// the form's own `q` is the last there is room for.
#[test]
fn the_state_a_form_saves_counts_toward_the_limit() {
    let mut content = b"q ".repeat(MAX_SAVED_STATES - 2);
    content.extend(b"/Fm Do ");
    content.extend(b"Q ".repeat(MAX_SAVED_STATES - 2));
    let at_limit = document(&[stream("", &content)], form(b"q Q"));
    open(&at_limit).render_page(0, 72.0).unwrap();

    let over = document(&[stream("", &content)], form(b"q q Q Q"));
    let err = open(&over).render_page(0, 72.0).unwrap_err();
    assert!(
        matches!(err, PdfError::ContentLimit(ContentLimit::SavedStates(_))),
        "{err:?}"
    );
}

#[test]
fn content_up_to_the_byte_limit_is_read_and_one_byte_more_is_not() {
    let content = b"BT /F1 12 Tf 72 700 Td (page) Tj ET";
    let pdf = one_page(content);
    let len = content.len() as u64;

    let mut doc = open(&pdf);
    doc.set_max_content_bytes(Some(len));
    assert_eq!(doc.max_content_bytes(), Some(len));
    assert_eq!(text_of(&doc.render_page(0, 72.0).unwrap()), ["page"]);

    doc.set_max_content_bytes(Some(len - 1));
    let err = doc.render_page(0, 72.0).unwrap_err();
    assert!(
        matches!(err, PdfError::ContentLimit(ContentLimit::ContentBytes(l)) if l == len - 1),
        "{err:?}"
    );
    assert!(
        doc.parse_warnings().is_empty(),
        "{:?}",
        doc.parse_warnings()
    );
}

/// A form counts each time it is drawn, not once.
#[test]
fn a_form_counts_each_time_it_is_drawn() {
    let page = b"/Fm Do /Fm Do /Fm Do";
    let shown = b"BT /F1 12 Tf 72 600 Td (form) Tj ET";
    let pdf = document(&[stream("", page)], form(shown));
    let all = (page.len() + 3 * shown.len()) as u64;

    let mut doc = open(&pdf);
    doc.set_max_content_bytes(Some(all));
    assert_eq!(
        text_of(&doc.render_page(0, 72.0).unwrap()),
        ["form", "form", "form"]
    );

    doc.set_max_content_bytes(Some(all - 1));
    let err = doc.render_page(0, 72.0).unwrap_err();
    assert!(
        matches!(err, PdfError::ContentLimit(ContentLimit::ContentBytes(l)) if l == all - 1),
        "{err:?}"
    );
    assert!(
        doc.parse_warnings().is_empty(),
        "{:?}",
        doc.parse_warnings()
    );
}

/// A page stream that decodes to far more than the limit is stopped at the
/// limit; so is a form drawn from a page that fits.
#[test]
fn a_stream_decoding_past_the_byte_limit_is_stopped() {
    let limit = 1 << 20;
    let big = deflate(&b"0 0 m ".repeat(2_000_000));

    let on_the_page = document(
        &[stream("/Filter /FlateDecode", &big)],
        form(b"BT /F1 12 Tf 72 600 Td (form) Tj ET"),
    );
    let mut doc = open(&on_the_page);
    doc.set_max_content_bytes(Some(limit));
    let err = doc.render_page(0, 72.0).unwrap_err();
    assert!(
        matches!(err, PdfError::ContentLimit(ContentLimit::ContentBytes(l)) if l == limit),
        "{err:?}"
    );

    let in_a_form = document(
        &[stream("", b"/Fm Do")],
        stream(
            "/Type /XObject /Subtype /Form /BBox [0 0 612 792] /Filter /FlateDecode",
            &big,
        ),
    );
    let mut doc = open(&in_a_form);
    doc.set_max_content_bytes(Some(limit));
    let err = doc.render_page(0, 72.0).unwrap_err();
    assert!(
        matches!(err, PdfError::ContentLimit(ContentLimit::ContentBytes(l)) if l == limit),
        "{err:?}"
    );
    assert!(
        doc.parse_warnings().is_empty(),
        "{:?}",
        doc.parse_warnings()
    );
}

/// A page over a limit leaves the document as it was: the next page, and
/// the same page under a higher limit, read as they would have.
#[test]
fn the_page_after_one_over_a_limit_is_read() {
    let saves = stream("", &nested_saves(MAX_SAVED_STATES + 1));
    let bytes = stream("", &b"0 0 m ".repeat(1000));
    let fine = stream("", b"BT /F1 12 Tf 72 700 Td (next) Tj ET");
    let pdf = document(
        &[saves, bytes, fine],
        form(b"BT /F1 12 Tf 72 600 Td (form) Tj ET"),
    );
    let mut doc = open(&pdf);
    assert!(matches!(
        doc.render_page(0, 72.0),
        Err(PdfError::ContentLimit(ContentLimit::SavedStates(_)))
    ));
    doc.set_max_content_bytes(Some(1000));
    assert!(matches!(
        doc.render_page(1, 72.0),
        Err(PdfError::ContentLimit(ContentLimit::ContentBytes(1000)))
    ));
    assert_eq!(text_of(&doc.render_page(2, 72.0).unwrap()), ["next"]);
    doc.set_max_content_bytes(None);
    doc.render_page(1, 72.0).unwrap();
    assert!(
        doc.parse_warnings().is_empty(),
        "{:?}",
        doc.parse_warnings()
    );
}

/// An ordinary page, nesting a few levels and drawing a form twice, reads
/// the same text and builds the same display list with a byte limit set
/// as without one.
#[test]
fn a_page_within_the_limits_reads_as_it_does_without_them() {
    let content = b"q 1 0 0 1 10 10 cm q 0.5 g 72 72 200 100 re f Q \
        q BT /F1 12 Tf 72 700 Td (first line) Tj ET Q \
        q 1 0 0 1 0 -40 cm /Fm Do Q /Fm Do Q \
        q q q BT /F1 10 Tf 72 500 Td (deeper) Tj ET Q Q Q";
    let pdf = one_page(content);

    let unlimited = open(&pdf);
    let without = unlimited.render_page(0, 72.0).unwrap();
    let mut limited = open(&pdf);
    limited.set_max_content_bytes(Some(64 << 20));
    let with = limited.render_page(0, 72.0).unwrap();

    assert_eq!(text_of(&without), ["first line", "form", "form", "deeper"]);
    assert_eq!(text_of(&with), text_of(&without));
    assert_eq!(format!("{with:?}"), format!("{without:?}"));
    assert!(limited.parse_warnings().is_empty());
}

/// A page with `content`, a form field and a square annotation, both drawn
/// by the one appearance stream `appearance`, which shows its text in the
/// page's font.
fn annotated(content: &[u8], appearance: &[u8]) -> Vec<u8> {
    pdf_from(&[
        b"<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [5 0 R] >> >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
           /Resources << /Font << /F1 8 0 R >> >> /Annots [5 0 R 6 0 R] >>"
            .to_vec(),
        stream("", content),
        b"<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /P 3 0 R \
           /Rect [72 600 272 620] /AP << /N 7 0 R >> >>"
            .to_vec(),
        b"<< /Type /Annot /Subtype /Square /P 3 0 R \
           /Rect [72 500 272 520] /AP << /N 7 0 R >> >>"
            .to_vec(),
        stream(
            "/Type /XObject /Subtype /Form /BBox [0 0 200 20] \
             /Resources << /Font << /F1 8 0 R >> >>",
            appearance,
        ),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_vec(),
    ])
}

/// An appearance that saves `count` levels of `q` around its text.
fn appearance_saving(count: usize) -> Vec<u8> {
    let mut appearance = b"q ".repeat(count);
    appearance.extend(b"BT /F1 12 Tf 2 5 Td (shown) Tj ET ");
    appearance.extend(b"Q ".repeat(count));
    appearance
}

/// The saves in the appearance of an annotation or a form field count
/// toward the limit as the page's own do.
#[test]
fn the_states_an_appearance_saves_count_toward_the_limit() {
    let page = b"BT /F1 12 Tf 72 700 Td (page) Tj ET";
    let at_limit = annotated(page, &appearance_saving(MAX_SAVED_STATES));
    let doc = open(&at_limit);
    assert_eq!(
        text_of(&doc.render_page(0, 72.0).unwrap()),
        ["page", "shown", "shown"]
    );

    let over = annotated(page, &appearance_saving(MAX_SAVED_STATES + 1));
    let doc = open(&over);
    let err = doc.render_page(0, 72.0).unwrap_err();
    assert!(
        matches!(
            err,
            PdfError::ContentLimit(ContentLimit::SavedStates(MAX_SAVED_STATES))
        ),
        "{err:?}"
    );
    assert!(
        doc.parse_warnings().is_empty(),
        "{:?}",
        doc.parse_warnings()
    );
}

/// The appearance of a form field and of an annotation count toward the
/// byte limit each time one is drawn: here one stream draws both.
#[test]
fn an_appearance_counts_each_time_it_is_drawn() {
    let page = b"BT /F1 12 Tf 72 700 Td (page) Tj ET";
    let appearance = b"BT /F1 12 Tf 2 5 Td (shown) Tj ET";
    let pdf = annotated(page, appearance);
    let all = (page.len() + 2 * appearance.len()) as u64;

    let mut doc = open(&pdf);
    doc.set_max_content_bytes(Some(all));
    assert_eq!(
        text_of(&doc.render_page(0, 72.0).unwrap()),
        ["page", "shown", "shown"]
    );

    doc.set_max_content_bytes(Some(all - 1));
    let err = doc.render_page(0, 72.0).unwrap_err();
    assert!(
        matches!(err, PdfError::ContentLimit(ContentLimit::ContentBytes(l)) if l == all - 1),
        "{err:?}"
    );
    assert!(
        doc.parse_warnings().is_empty(),
        "{:?}",
        doc.parse_warnings()
    );
}
