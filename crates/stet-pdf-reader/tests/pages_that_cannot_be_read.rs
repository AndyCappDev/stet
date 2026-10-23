// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A page the page tree names but that cannot be read is a blank page in
//! its place, so that the pages after it keep their numbers, and a
//! `PageTree` warning names it by its page number.

use stet_graphics::text::{text_lines, text_runs};
use stet_pdf_reader::{LayerSet, LocationHint, ParsePhase, PdfDocument, Severity, TextExtraction};

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

fn stream(body: &str) -> Vec<u8> {
    format!("<< /Length {} >>\nstream\n{body}\nendstream", body.len()).into_bytes()
}

/// Objects 1 to 6: a catalog, a page tree `tree`, and two pages (3 and 5)
/// each of 100 by 100 points, writing `first` and `second` in Helvetica;
/// `extra` from 7 up.
fn objects(tree: &str, first: &str, second: &str, extra: &[&str]) -> Vec<Vec<u8>> {
    let page = |contents: u32| {
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
             /Resources << /Font << /F1 << /Type /Font /Subtype /Type1 \
             /BaseFont /Helvetica >> >> >> /Contents {contents} 0 R >>"
        )
        .into_bytes()
    };
    let text = |words: &str| stream(&format!("BT /F1 12 Tf 10 50 Td ({words}) Tj ET"));
    let mut objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        tree.as_bytes().to_vec(),
        page(4),
        text(first),
        page(6),
        text(second),
    ];
    objects.extend(extra.iter().map(|o| o.as_bytes().to_vec()));
    objects
}

/// The example of the change: the tree names objects 3, 9 and 5 as its
/// three pages, and there is no object 9.
const NINE_MISSING: &str = "<< /Type /Pages /Kids [3 0 R 9 0 R 5 0 R] /Count 3 >>";

fn open(objects: &[Vec<u8>]) -> PdfDocument<'static> {
    let mut doc = PdfDocument::from_owned(pdf_from(objects)).expect("the document opens");
    doc.set_text_extraction(TextExtraction::Runs);
    doc
}

/// The text extracted from `page`, one line to an entry.
fn text_of(doc: &PdfDocument, page: usize) -> Vec<String> {
    let list = doc.render_page(page, 72.0).expect("the page renders");
    text_lines(text_runs(&list, &LayerSet::new()))
        .into_iter()
        .map(|line| line.text)
        .collect()
}

/// Every warning as `(phase, location, severity)`.
fn kinds(doc: &PdfDocument) -> Vec<(ParsePhase, Option<LocationHint>, Severity)> {
    doc.parse_warnings()
        .iter()
        .map(|w| (w.phase.clone(), w.location.clone(), w.severity))
        .collect()
}

fn page_tree_warning_at(page: usize) -> (ParsePhase, Option<LocationHint>, Severity) {
    (
        ParsePhase::PageTree,
        Some(LocationHint::Page(page)),
        Severity::Warning,
    )
}

/// The second page cannot be read: the document still opens with three
/// pages, the second is blank and the third is still the file's third.
#[test]
fn a_page_that_cannot_be_read_is_a_blank_page_in_its_place() {
    let doc = open(&objects(NINE_MISSING, "first", "third", &[]));
    assert_eq!(doc.page_count(), 3);
    assert_eq!(text_of(&doc, 0), ["first"]);
    assert_eq!(text_of(&doc, 2), ["third"]);
    assert_eq!(doc.page_info(2).unwrap().obj_num, 5);
}

/// The blank page renders, draws nothing and has no text.
#[test]
fn the_blank_page_draws_nothing_and_has_no_text() {
    let doc = open(&objects(NINE_MISSING, "first", "third", &[]));
    let list = doc.render_page(1, 72.0).expect("the blank page renders");
    assert!(list.is_empty(), "{:?}", list.elements());
    assert!(text_of(&doc, 1).is_empty());
}

/// The page lost is reported once, as a page tree warning at the page
/// standing in for it, counted from 0, and the message names the object.
#[test]
fn the_page_lost_is_a_page_tree_warning_at_its_page() {
    let doc = open(&objects(NINE_MISSING, "first", "third", &[]));
    for page in 0..doc.page_count() {
        doc.render_page(page, 72.0).unwrap();
    }
    assert_eq!(kinds(&doc), [page_tree_warning_at(1)]);
    let message = doc.parse_warnings()[0].message.clone();
    assert!(message.contains("object 9 0"), "{message}");
}

/// The blank page has the MediaBox, CropBox and rotation the tree gives
/// its siblings.
#[test]
fn the_blank_page_has_the_media_box_it_inherits() {
    let tree = "<< /Type /Pages /Kids [3 0 R 9 0 R 5 0 R] /Count 3 \
                /MediaBox [0 0 200 300] /CropBox [10 10 190 290] /Rotate 90 >>";
    let doc = open(&objects(tree, "first", "third", &[]));
    let boxes = doc.page_boxes(1).unwrap();
    assert_eq!(boxes.media_box, [0.0, 0.0, 200.0, 300.0]);
    assert_eq!(boxes.crop_box, Some([10.0, 10.0, 190.0, 290.0]));
    assert_eq!(boxes.rotate, 90);
    assert_eq!(doc.page_size(1).unwrap(), (280.0, 180.0));
}

/// With no MediaBox to inherit, the blank page is US Letter, as any page
/// with none is.
#[test]
fn the_blank_page_is_us_letter_when_it_inherits_no_media_box() {
    let doc = open(&objects(NINE_MISSING, "first", "third", &[]));
    assert_eq!(
        doc.page_boxes(1).unwrap().media_box,
        [0.0, 0.0, 612.0, 792.0]
    );
    assert_eq!(doc.page_size(1).unwrap(), (612.0, 792.0));
}

/// An object the tree names that is there but is not a dictionary was
/// passed over; it is a blank page too.
#[test]
fn an_object_that_is_not_a_dictionary_is_a_blank_page() {
    let tree = "<< /Type /Pages /Kids [3 0 R 7 0 R 5 0 R] /Count 3 >>";
    let doc = open(&objects(tree, "first", "third", &["42"]));
    assert_eq!(doc.page_count(), 3);
    assert!(doc.render_page(1, 72.0).unwrap().is_empty());
    assert_eq!(text_of(&doc, 2), ["third"]);
    assert_eq!(kinds(&doc), [page_tree_warning_at(1)]);
}

/// A branch that cannot be read is as many blank pages as the parent's
/// `/Count` leaves for it once the other kids are counted: a page counts
/// one, and a branch its own `/Count`.
#[test]
fn a_branch_that_cannot_be_read_is_as_many_blank_pages_as_the_count_leaves() {
    let tree = "<< /Type /Pages /Kids [3 0 R 9 0 R 5 0 R] /Count 5 >>";
    let doc = open(&objects(tree, "first", "fifth", &[]));
    assert_eq!(doc.page_count(), 5);
    for page in 1..=3 {
        assert!(
            doc.render_page(page, 72.0).unwrap().is_empty(),
            "page {page}"
        );
    }
    assert_eq!(text_of(&doc, 4), ["fifth"]);
    assert_eq!(
        kinds(&doc),
        [
            page_tree_warning_at(1),
            page_tree_warning_at(2),
            page_tree_warning_at(3)
        ]
    );

    // The other kid is a branch of two pages: the four the root counts
    // leave two for the branch that cannot be read.
    let tree = "<< /Type /Pages /Kids [7 0 R 9 0 R] /Count 4 >>";
    let branch = "<< /Type /Pages /Parent 2 0 R /Kids [3 0 R 5 0 R] /Count 2 >>";
    let doc = open(&objects(tree, "first", "second", &[branch]));
    assert_eq!(doc.page_count(), 4);
    assert_eq!(text_of(&doc, 1), ["second"]);
    assert!(doc.render_page(2, 72.0).unwrap().is_empty());
    assert!(doc.render_page(3, 72.0).unwrap().is_empty());
    assert_eq!(
        kinds(&doc),
        [page_tree_warning_at(2), page_tree_warning_at(3)]
    );
}

/// When the counts cannot tell how many pages a kid stood for, it is one
/// blank page.
#[test]
fn a_kid_whose_pages_cannot_be_told_is_one_blank_page() {
    // Each tree, and the text of each page it opens with: `None` for a
    // blank page.
    let first = Some("first");
    let last = Some("last");
    let cases = [
        // The parent gives no count.
        (
            "<< /Type /Pages /Kids [3 0 R 9 0 R 5 0 R] >>",
            vec![first, None, last],
        ),
        // The count leaves nothing for it.
        (
            "<< /Type /Pages /Kids [3 0 R 9 0 R 5 0 R] /Count 2 >>",
            vec![first, None, last],
        ),
        // Two kids cannot be read, and the count does not say how the
        // pages left are shared between them.
        (
            "<< /Type /Pages /Kids [3 0 R 9 0 R 10 0 R 5 0 R] /Count 7 >>",
            vec![first, None, None, last],
        ),
        // The count is more pages than the file has objects.
        (
            "<< /Type /Pages /Kids [3 0 R 9 0 R 5 0 R] /Count 1000000 >>",
            vec![first, None, last],
        ),
        // Another kid is a branch with no count.
        (
            "<< /Type /Pages /Kids [7 0 R 9 0 R] /Count 4 >>",
            vec![first, last, None],
        ),
    ];
    let branch = "<< /Type /Pages /Parent 2 0 R /Kids [3 0 R 5 0 R] >>";
    for (tree, pages) in cases {
        let doc = open(&objects(tree, "first", "last", &[branch]));
        assert_eq!(doc.page_count(), pages.len(), "{tree}");
        for (page, text) in pages.iter().enumerate() {
            match text {
                Some(text) => assert_eq!(text_of(&doc, page), [*text], "{tree}"),
                None => assert!(doc.render_page(page, 72.0).unwrap().is_empty(), "{tree}"),
            }
        }
    }
}

/// A cross-reference subsection that starts at a very high object number
/// raises the highest object number for a few bytes, but not the entries
/// the file writes out. A count past those entries is not believed, so the
/// branch that cannot be read is one blank page, not a million.
#[test]
fn a_count_past_the_entries_in_use_is_one_blank_page_whatever_the_object_numbers() {
    let tree = "<< /Type /Pages /Kids [3 0 R 9 0 R 5 0 R] /Count 1000000 >>";
    let mut pdf = pdf_from(&objects(tree, "first", "last", &[]));
    let trailer = pdf
        .windows(8)
        .rposition(|w| w == b"trailer\n")
        .expect("the file has a trailer");
    pdf.splice(
        trailer..trailer,
        b"1000000 1\n0000000000 00000 f\r\n".iter().copied(),
    );
    let mut doc = PdfDocument::from_owned(pdf).expect("the document opens");
    doc.set_text_extraction(TextExtraction::Runs);
    assert_eq!(doc.page_count(), 3);
    assert!(doc.render_page(1, 72.0).unwrap().is_empty());
    assert_eq!(text_of(&doc, 2), ["last"]);
    assert_eq!(kinds(&doc), [page_tree_warning_at(1)]);
}

/// An object that cannot be read listed twice is one blank page, as a
/// node listed twice is used once.
#[test]
fn a_kid_that_cannot_be_read_listed_twice_is_one_blank_page() {
    let tree = "<< /Type /Pages /Kids [3 0 R 9 0 R 9 0 R 5 0 R] /Count 3 >>";
    let doc = open(&objects(tree, "first", "third", &[]));
    assert_eq!(doc.page_count(), 3);
    assert_eq!(text_of(&doc, 2), ["third"]);
    assert_eq!(
        kinds(&doc),
        [
            (
                ParsePhase::PageTree,
                Some(LocationHint::Object {
                    obj_num: 9,
                    gen_num: 0
                }),
                Severity::Warning
            ),
            page_tree_warning_at(1)
        ]
    );
}

/// A tree naming a great many objects that are not in the file has a
/// blank page for each, and lists the first 64 and then says the list is
/// cut short: every warning recorded is checked against those before it,
/// and opening the file cannot be canceled.
#[test]
fn many_pages_that_cannot_be_read_list_a_bounded_number_of_warnings() {
    let missing: String = (100..400).map(|n| format!("{n} 0 R ")).collect();
    let tree = format!("<< /Type /Pages /Kids [3 0 R {missing}5 0 R] /Count 302 >>");
    let doc = open(&objects(&tree, "first", "last", &[]));
    assert_eq!(doc.page_count(), 302);
    assert_eq!(text_of(&doc, 301), ["last"]);
    let warnings = doc.parse_warnings();
    assert_eq!(warnings.len(), 65, "{:?}", warnings.last());
    assert!(warnings.iter().all(|w| w.phase == ParsePhase::PageTree));
    assert_eq!(warnings[63].location, Some(LocationHint::Page(64)));
    assert_eq!(warnings[64].location, None);
    assert!(
        warnings[64].message.contains("not listed"),
        "{:?}",
        warnings[64]
    );
}

/// The catalog has no page tree, and the one found by scanning the file
/// names a page that cannot be read: it is a blank page there too.
#[test]
fn a_page_tree_found_by_scanning_keeps_a_blank_page_in_its_place() {
    let mut objects = objects(NINE_MISSING, "first", "third", &[]);
    objects[0] = b"<< /Type /Catalog >>".to_vec();
    objects.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
    let doc = open(&objects);
    assert_eq!(doc.page_count(), 3);
    assert!(doc.render_page(1, 72.0).unwrap().is_empty());
    assert_eq!(text_of(&doc, 2), ["third"]);
    assert_eq!(
        kinds(&doc),
        [
            page_tree_warning_at(1),
            (ParsePhase::PageTree, None, Severity::Warning)
        ]
    );
}

/// A page tree found by scanning in which nothing can be read is not
/// taken: the pages are found by scanning for page objects, as before, and
/// no warning names a blank page.
#[test]
fn a_page_tree_found_by_scanning_with_nothing_readable_is_passed_over() {
    let mut objects = objects(NINE_MISSING, "first", "second", &[]);
    objects[0] = b"<< /Type /Catalog >>".to_vec();
    objects.push(b"<< /Type /Catalog /Pages 8 0 R >>".to_vec());
    objects.push(b"<< /Type /Pages /Kids [9 0 R] /Count 1 >>".to_vec());
    let doc = open(&objects);
    assert_eq!(doc.page_count(), 2);
    assert_eq!(text_of(&doc, 0), ["first"]);
    assert_eq!(text_of(&doc, 1), ["second"]);
    assert_eq!(
        kinds(&doc),
        [(ParsePhase::PageTree, None, Severity::Warning)]
    );
}
