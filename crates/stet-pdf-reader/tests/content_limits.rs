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
//! A page's `q` past [`MAX_SAVED_STATES`] is skipped and counted, so the
//! restore that matches it restores nothing; the page is drawn on, with one
//! `ParsePhase::SavedStateLimit` warning. The saves around a form, pattern,
//! glyph procedure or soft mask are always made, and count toward the
//! limit.
//!
//! A page over the byte limit is not drawn. `render_page` returns
//! `PdfError::ContentLimit`, which is its own kind of error, nothing is
//! added to the parse warnings for it, and the document's other pages read
//! as before. A page within both reads exactly as it does with no limit.

use std::io::Write;

use flate2::Compression;
use flate2::write::ZlibEncoder;
use stet_graphics::display_list::{DisplayElement, DisplayList};
use stet_graphics::text::{text_lines, text_runs};
use stet_pdf_reader::{
    ContentLimit, LayerSet, LocationHint, MAX_SAVED_STATES, ParsePhase, PdfDocument, PdfError,
    Severity, TextExtraction,
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

/// `doc` warned once that page `page` went past the limit, at
/// `Severity::Warning`, and gave no other warning.
#[track_caller]
fn assert_warned_once(doc: &PdfDocument, page: usize) {
    let warnings = doc.parse_warnings().to_vec();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    let warning = &warnings[0];
    assert_eq!(warning.phase, ParsePhase::SavedStateLimit);
    assert_eq!(warning.severity, Severity::Warning);
    assert_eq!(warning.location, Some(LocationHint::Page(page)));
}

#[test]
fn a_page_saving_past_the_limit_is_drawn_and_warned_once() {
    let pdf = one_page(&nested_saves(MAX_SAVED_STATES + 1));
    let doc = open(&pdf);
    let list = doc.render_page(0, 72.0).unwrap();
    assert_eq!(text_of(&list), ["inside"]);
    assert_warned_once(&doc, 0);
    // Drawing the page again adds nothing.
    doc.render_page(0, 72.0).unwrap();
    assert_warned_once(&doc, 0);
}

/// Some generators write `q` without its `Q`. A page with 1,100 of them and
/// then a fill draws the fill, as it does with fewer, and is not failed.
#[test]
fn a_page_leaving_more_saves_unclosed_than_the_limit_draws_its_fill() {
    let page = |saves: usize| {
        let mut content = b"q ".repeat(saves);
        content.extend(b"0 0 1 rg 72 72 200 100 re f");
        one_page(&content)
    };
    let over = page(1100);
    let doc = open(&over);
    let list = doc.render_page(0, 72.0).unwrap();
    let fills = list
        .elements()
        .iter()
        .filter(|e| matches!(e, DisplayElement::Fill { .. }))
        .count();
    assert_eq!(fills, 1, "{list:?}");
    assert_warned_once(&doc, 0);

    // Drawn as the same page within the limit draws it.
    let within = page(MAX_SAVED_STATES);
    let doc_within = open(&within);
    let list_within = doc_within.render_page(0, 72.0).unwrap();
    assert_eq!(format!("{list:?}"), format!("{list_within:?}"));
    assert!(doc_within.parse_warnings().is_empty());
}

/// The file that showed the need: a small Flate stream decoding to `q`
/// after `q`, two million of them here. Past the limit nothing more is
/// saved, so it is read to its end, and what follows is drawn.
#[test]
fn a_stream_of_unmatched_saves_is_read_to_its_end() {
    let mut saves = b"q ".repeat(2_000_000);
    saves.extend(b"BT /F1 12 Tf 72 700 Td (after) Tj ET");
    let content = deflate(&saves);
    let pdf = document(
        &[stream("/Filter /FlateDecode", &content)],
        form(b"BT /F1 12 Tf 72 600 Td (form) Tj ET"),
    );
    assert!(pdf.len() < 20_000, "{} bytes", pdf.len());
    let doc = open(&pdf);
    let list = doc.render_page(0, 72.0).unwrap();
    assert_eq!(text_of(&list), ["after"]);
    assert_warned_once(&doc, 0);
}

/// The color and line width of the page's strokes, in paint order,
/// through groups, but not in a soft mask's own drawing.
fn strokes(list: &DisplayList) -> Vec<([f64; 3], f64)> {
    let mut found = Vec::new();
    for element in list.elements() {
        match element {
            DisplayElement::Stroke { params, .. } => found.push((
                [params.color.r, params.color.g, params.color.b],
                params.line_width,
            )),
            DisplayElement::Group { elements, .. } | DisplayElement::OcgGroup { elements, .. } => {
                found.extend(strokes(elements));
            }
            DisplayElement::SoftMasked { content, .. } => found.extend(strokes(content)),
            _ => {}
        }
    }
    found
}

fn stroke_colors(list: &DisplayList) -> Vec<[f64; 3]> {
    strokes(list).into_iter().map(|(color, _)| color).collect()
}

const RED: [f64; 3] = [1.0, 0.0, 0.0];
const BLUE: [f64; 3] = [0.0, 0.0, 1.0];

/// A page that fills the stack to the limit, the last state saved with a
/// red stroke 7 wide, sets a blue stroke 3 wide, runs `between`, and
/// strokes a line.
///
/// A `q` in `between` is past the limit and skipped, and its restore uses
/// up the skip. Were it not counted, the restore would take the last saved
/// state off the stack, and the line would be red and 7 wide.
fn last_saved_red_then_blue(between: &[u8]) -> Vec<u8> {
    let mut content = b"q ".repeat(MAX_SAVED_STATES - 1);
    content.extend(b"1 0 0 RG 7 w q 0 0 1 RG 3 w ");
    content.extend(between);
    content.extend(b" 72 72 m 300 300 l S");
    content
}

/// A `Q` whose `q` was skipped restores nothing, rather than the state
/// saved before it.
#[test]
fn the_restore_of_a_skipped_save_restores_nothing() {
    let pdf = one_page(&last_saved_red_then_blue(b"q 0 1 0 RG Q q Q"));
    let doc = open(&pdf);
    let list = doc.render_page(0, 72.0).unwrap();
    // The green set between the skipped `q` and its `Q` stays: there was
    // nothing saved to go back to.
    assert_eq!(stroke_colors(&list), [[0.0, 1.0, 0.0]]);
    assert_warned_once(&doc, 0);

    // The restores past the skipped ones still restore what was saved.
    let mut content = last_saved_red_then_blue(b"q Q");
    content.extend(b" Q");
    content.extend(b" 72 72 m 300 300 l S");
    let pdf = one_page(&content);
    let doc = open(&pdf);
    assert_eq!(
        stroke_colors(&doc.render_page(0, 72.0).unwrap()),
        [BLUE, RED]
    );
}

/// A page of objects 1 catalog, 2 pages, 3 the page, 4 its `content`,
/// 5 its font `/F1`, then `extra` from 6 on, with `resources` added to
/// the page's font.
fn page_with(content: &[u8], resources: &str, extra: Vec<Vec<u8>>) -> Vec<u8> {
    let mut objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> {resources} >> >>"
        )
        .into_bytes(),
        stream("", content),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_vec(),
    ];
    objects.extend(extra);
    pdf_from(&objects)
}

/// Nested content run by `between`, and the objects it needs from 6 on.
struct Nested {
    what: &'static str,
    between: &'static [u8],
    resources: &'static str,
    extra: Vec<Vec<u8>>,
}

/// A form, a pattern cell, a glyph procedure and a soft mask's group, each
/// running `inside` as its own content, after what it must begin with.
fn nested_cases(inside: &str) -> [Nested; 4] {
    [
        Nested {
            what: "form",
            between: b"/Fm Do",
            resources: "/XObject << /Fm 6 0 R >>",
            extra: vec![form(inside.as_bytes())],
        },
        Nested {
            what: "pattern",
            between: b"/Pattern cs /P1 scn 72 72 100 100 re f",
            resources: "/Pattern << /P1 6 0 R >>",
            extra: vec![stream(
                "/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 \
                 /BBox [0 0 10 10] /XStep 10 /YStep 10 /Resources << >>",
                format!("{inside} 0 0 5 5 re f").as_bytes(),
            )],
        },
        Nested {
            what: "glyph procedure",
            between: b"BT /T3 12 Tf 72 600 Td (a) Tj ET",
            resources: "/Font << /F1 5 0 R /T3 6 0 R >>",
            extra: vec![
                b"<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
                   /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << /a 7 0 R >> \
                   /Encoding << /Type /Encoding /Differences [97 /a] >> \
                   /FirstChar 97 /LastChar 97 /Widths [1000] /Resources << >> >>"
                    .to_vec(),
                stream(
                    "",
                    format!("1000 0 0 0 1000 1000 d1 {inside} 0 0 1000 1000 re f").as_bytes(),
                ),
            ],
        },
        Nested {
            what: "soft mask",
            between: b"/GS1 gs",
            resources: "/ExtGState << /GS1 6 0 R >>",
            extra: vec![
                b"<< /Type /ExtGState /SMask << /Type /Mask /S /Luminosity /G 7 0 R >> >>".to_vec(),
                stream(
                    "/Type /XObject /Subtype /Form /BBox [0 0 612 792] \
                     /Group << /S /Transparency /CS /DeviceGray >>",
                    format!("{inside} 1 g 0 0 612 792 re f").as_bytes(),
                ),
            ],
        },
    ]
}

/// The state saved around a form, a pattern cell, a glyph procedure and a
/// soft mask's group is saved even past the limit, so the page's state is
/// back after each as it was before.
///
/// The page goes one `q` past the limit, sets a blue stroke 3 wide, runs
/// the nested content, and strokes a line. The nested content sets a green
/// stroke 9 wide without `q`; the glyph's `d1` also sets the stroke color to
/// the fill color, black. Were the save around it skipped, the line would
/// take what the nested content set.
#[test]
fn past_the_limit_nested_content_still_restores_the_page_state() {
    let mut found = Vec::new();
    for case in nested_cases("0 1 0 RG 9 w") {
        let what = case.what;
        let mut content = b"q ".repeat(MAX_SAVED_STATES + 1);
        content.extend(b"0 0 1 RG 3 w ");
        content.extend(case.between);
        content.extend(b" 72 72 m 300 300 l S");
        let pdf = page_with(&content, case.resources, case.extra);
        let doc = open(&pdf);
        let list = doc
            .render_page(0, 72.0)
            .unwrap_or_else(|e| panic!("{what}: {e}"));
        assert_warned_once(&doc, 0);
        found.push((what, strokes(&list)));
    }
    let expected =
        ["form", "pattern", "glyph procedure", "soft mask"].map(|what| (what, vec![(BLUE, 3.0)]));
    assert_eq!(found, expected);
}

/// The state saved around a form, a pattern cell, a glyph procedure and a
/// soft mask's group counts toward the limit. With the page one save short
/// of it, the nested save is the last there is room for, and the nested
/// content's own `q` is past it; two short, that `q` is still within it.
#[test]
fn the_saves_around_nested_content_count_toward_the_limit() {
    let mut found = Vec::new();
    for short in [1, 2] {
        for case in nested_cases("q Q") {
            let what = case.what;
            let mut content = b"q ".repeat(MAX_SAVED_STATES - short);
            content.extend(case.between);
            content.extend(b" ");
            content.extend(b"Q ".repeat(MAX_SAVED_STATES - short));
            let pdf = page_with(&content, case.resources, case.extra);
            let doc = open(&pdf);
            doc.render_page(0, 72.0)
                .unwrap_or_else(|e| panic!("{what}: {e}"));
            let phases: Vec<_> = doc
                .parse_warnings()
                .iter()
                .map(|w| (w.phase.clone(), w.severity, w.location.clone()))
                .collect();
            found.push((short, what, phases));
        }
    }
    let warned = vec![(
        ParsePhase::SavedStateLimit,
        Severity::Warning,
        Some(LocationHint::Page(0)),
    )];
    let expected: Vec<_> = [(1, warned), (2, Vec::new())]
        .into_iter()
        .flat_map(|(short, phases)| {
            ["form", "pattern", "glyph procedure", "soft mask"]
                .map(|what| (short, what, phases.clone()))
        })
        .collect();
    assert_eq!(found, expected);
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
    let doc = open(&at_limit);
    doc.render_page(0, 72.0).unwrap();
    assert!(doc.parse_warnings().is_empty());

    let over = document(&[stream("", &content)], form(b"q q Q Q"));
    let doc = open(&over);
    doc.render_page(0, 72.0).unwrap();
    assert_warned_once(&doc, 0);
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
    assert_eq!(text_of(&doc.render_page(0, 72.0).unwrap()), ["inside"]);
    assert_warned_once(&doc, 0);
    doc.set_max_content_bytes(Some(1000));
    assert!(matches!(
        doc.render_page(1, 72.0),
        Err(PdfError::ContentLimit(ContentLimit::ContentBytes(1000)))
    ));
    assert_eq!(text_of(&doc.render_page(2, 72.0).unwrap()), ["next"]);
    doc.set_max_content_bytes(None);
    doc.render_page(1, 72.0).unwrap();
    assert_warned_once(&doc, 0);
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
/// toward the limit as the page's own do. Past it they are skipped, the
/// appearances are drawn, and the page is warned once for both.
#[test]
fn the_states_an_appearance_saves_count_toward_the_limit() {
    let page = b"BT /F1 12 Tf 72 700 Td (page) Tj ET";
    let at_limit = annotated(page, &appearance_saving(MAX_SAVED_STATES));
    let doc = open(&at_limit);
    assert_eq!(
        text_of(&doc.render_page(0, 72.0).unwrap()),
        ["page", "shown", "shown"]
    );
    assert!(doc.parse_warnings().is_empty());

    let over = annotated(page, &appearance_saving(MAX_SAVED_STATES + 1));
    let doc = open(&over);
    assert_eq!(
        text_of(&doc.render_page(0, 72.0).unwrap()),
        ["page", "shown", "shown"]
    );
    assert_warned_once(&doc, 0);
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
