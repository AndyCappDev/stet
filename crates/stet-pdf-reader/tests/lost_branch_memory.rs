// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A branch of the page tree that cannot be read is believed to hold the
//! pages its parent's `/Count` leaves for it only while the file is long
//! enough for that many pages. Past that it is one blank page, so a small
//! file cannot make the reader hold millions of them.
//!
//! Neither the object numbers a file names nor the cross-reference entries
//! it declares are a measure of its pages: one subsection header can name
//! a very high object number, and a compressed cross-reference stream
//! declares millions of entries in a few kilobytes. Nor are the kids a
//! tree lists, as a compressed object stream lists a million in a few
//! kilobytes too: a kid naming object 0 is no page at all, and every blank
//! page counts against the file's length.
//!
//! This file has its own allocator, which records the most memory live at
//! once, so that each test can show that a count claiming millions of
//! pages costs no more than one claiming the pages the file has. The page
//! count alone would not show memory held and then let go.

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use flate2::Compression;
use flate2::write::ZlibEncoder;
use stet_pdf_reader::{LocationHint, ParsePhase, PdfDocument, Severity};

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

/// The tests take turns, so that one does not count the other's memory.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// The most memory live at once during `f`, beyond what was live when it
/// started.
fn peak_during<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let out = f();
    (out, PEAK.load(Ordering::Relaxed).saturating_sub(before))
}

/// Objects 1 to 6: a catalog, the page tree `tree`, and two pages (3 and
/// 5) with their content streams.
fn objects(tree: &str) -> Vec<Vec<u8>> {
    let page = |contents: u32| {
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents {contents} 0 R >>")
            .into_bytes()
    };
    let content = b"<< /Length 0 >>\nstream\n\nendstream".to_vec();
    vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        tree.as_bytes().to_vec(),
        page(4),
        content.clone(),
        page(6),
        content,
    ]
}

/// `objects`, numbered from 1, each written out, and the byte offset of
/// each.
fn body(objects: &[Vec<u8>]) -> (Vec<u8>, Vec<usize>) {
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend(format!("{} 0 obj\n", i + 1).as_bytes());
        pdf.extend(object);
        pdf.extend(b"\nendobj\n");
    }
    (pdf, offsets)
}

/// A PDF of `objects` with a cross-reference table, and one more
/// subsection naming the free object `high` alone.
fn with_table_reaching(objects: &[Vec<u8>], high: u32) -> Vec<u8> {
    let (mut pdf, offsets) = body(objects);
    let xref = pdf.len();
    pdf.extend(format!("xref\n0 {}\n0000000000 65535 f\r\n", objects.len() + 1).as_bytes());
    for off in offsets {
        pdf.extend(format!("{off:010} 00000 n\r\n").as_bytes());
    }
    pdf.extend(format!("{high} 1\n0000000000 00000 f\r\n").as_bytes());
    pdf.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}

/// A PDF of `objects` whose cross-reference stream, object 7, declares
/// `entries` objects in use: those of `objects`, itself, and the rest all
/// placed where object 3 is.
fn with_stream_of(objects: &[Vec<u8>], entries: u32) -> Vec<u8> {
    let (mut pdf, mut offsets) = body(objects);
    let xref = pdf.len();
    offsets.push(xref);
    // Each entry is its type, 1, and a four-byte offset.
    let entry = |offset: usize| {
        let mut e = vec![1u8];
        e.extend((offset as u32).to_be_bytes());
        e
    };
    let mut table = ZlibEncoder::new(Vec::new(), Compression::best());
    table.write_all(&[0, 0, 0, 0, 0]).unwrap();
    for &offset in &offsets {
        table.write_all(&entry(offset)).unwrap();
    }
    let same = entry(offsets[2]);
    for _ in offsets.len() + 1..entries as usize {
        table.write_all(&same).unwrap();
    }
    let table = table.finish().unwrap();
    pdf.extend(
        format!(
            "{} 0 obj\n<< /Type /XRef /Size {entries} /W [1 4 0] /Root 1 0 R \
             /Filter /FlateDecode /Length {} >>\nstream\n",
            offsets.len(),
            table.len()
        )
        .as_bytes(),
    );
    pdf.extend(&table);
    pdf.extend(format!("\nendstream\nendobj\nstartxref\n{xref}\n%%EOF\n").as_bytes());
    pdf
}

/// `bytes` compressed as a `/FlateDecode` stream.
fn deflated(bytes: &[u8]) -> Vec<u8> {
    let mut packed = ZlibEncoder::new(Vec::new(), Compression::best());
    packed.write_all(bytes).unwrap();
    packed.finish().unwrap()
}

/// A PDF of `objects` whose page tree, object 2, is written compressed in
/// an object stream, object 7, as a `/Kids` array of many kids can be. Its
/// cross-reference stream is object 8.
fn with_tree_compressed(objects: &[Vec<u8>]) -> Vec<u8> {
    let header = b"2 0 ";
    let packed = deflated(&[&header[..], &objects[1]].concat());
    let mut object_stream = format!(
        "<< /Type /ObjStm /N 1 /First {} /Filter /FlateDecode /Length {} >>\nstream\n",
        header.len(),
        packed.len()
    )
    .into_bytes();
    object_stream.extend(&packed);
    object_stream.extend(b"\nendstream");

    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    let written = [
        (1, &objects[0]),
        (3, &objects[2]),
        (4, &objects[3]),
        (5, &objects[4]),
        (6, &objects[5]),
        (7, &object_stream),
    ];
    for (number, object) in written {
        offsets.push(pdf.len());
        pdf.extend(format!("{number} 0 obj\n").as_bytes());
        pdf.extend(object);
        pdf.extend(b"\nendobj\n");
    }
    let xref = pdf.len();

    // Each entry is its type, a four-byte offset or object stream number,
    // and a two-byte generation or index.
    let entry = |kind: u8, field: usize| {
        let mut e = vec![kind];
        e.extend((field as u32).to_be_bytes());
        e.extend([0, 0]);
        e
    };
    let mut table = vec![0u8; 7];
    table.extend(entry(1, offsets[0]));
    // Object 2 is the first object of object stream 7.
    table.extend(entry(2, 7));
    for &offset in &offsets[1..] {
        table.extend(entry(1, offset));
    }
    table.extend(entry(1, xref));
    let packed = deflated(&table);
    pdf.extend(
        format!(
            "8 0 obj\n<< /Type /XRef /Size 9 /W [1 4 2] /Root 1 0 R \
             /Filter /FlateDecode /Length {} >>\nstream\n",
            packed.len()
        )
        .as_bytes(),
    );
    pdf.extend(&packed);
    pdf.extend(format!("\nendstream\nendobj\nstartxref\n{xref}\n%%EOF\n").as_bytes());
    pdf
}

/// What a warning is, without its message.
type Kind = (ParsePhase, Option<LocationHint>, Severity);

/// Opens `pdf`: its page count, its warnings, and the most memory live
/// while it was opened.
fn open(pdf: Vec<u8>) -> (usize, Vec<Kind>, usize) {
    let (doc, peak) =
        peak_during(move || PdfDocument::from_owned(pdf).expect("the document opens"));
    let warnings = doc
        .parse_warnings()
        .iter()
        .map(|w| (w.phase.clone(), w.location.clone(), w.severity))
        .collect();
    (doc.page_count(), warnings, peak)
}

/// What a page tree warning at `page` looks like.
fn lost_page_at(page: usize) -> Kind {
    (
        ParsePhase::PageTree,
        Some(LocationHint::Page(page)),
        Severity::Warning,
    )
}

/// A compressed cross-reference stream of two million entries in use, all
/// placed at one object, takes a few kilobytes. A lost branch whose count
/// claims that many pages is one blank page, and opening the file holds no
/// more memory than with a count that claims the one page the branch can
/// be.
#[test]
fn a_count_past_the_file_length_is_one_blank_page_whatever_the_xref_stream_declares() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    const ENTRIES: u32 = 2_000_000;
    // The lost kid is the object after the last the stream declares, and
    // the count claims a page for every entry the stream declares.
    let tree =
        |count: u32| format!("<< /Type /Pages /Kids [3 0 R {ENTRIES} 0 R 5 0 R] /Count {count} >>");
    let claimed = with_stream_of(&objects(&tree(ENTRIES)), ENTRIES);
    assert!(
        claimed.len() < 32 * 1024,
        "the file is {} bytes",
        claimed.len()
    );

    let (_, _, honest_peak) = open(with_stream_of(&objects(&tree(3)), ENTRIES));
    let (pages, warnings, claimed_peak) = open(claimed);
    assert_eq!(pages, 3);
    assert_eq!(warnings, [lost_page_at(1)]);
    assert!(
        claimed_peak <= honest_peak + (1 << 20),
        "{claimed_peak} bytes live at most, against {honest_peak} for a count of 3"
    );
}

/// A cross-reference subsection that starts at a very high object number
/// raises the highest object number for a few bytes. A lost branch whose
/// count claims that many pages is one blank page too, and opening the
/// file holds no more memory than with a count that claims the one page
/// the branch can be.
#[test]
fn a_count_past_the_file_length_is_one_blank_page_whatever_the_object_numbers() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    const HIGH: u32 = 1_000_000;
    let tree = |count: u32| format!("<< /Type /Pages /Kids [3 0 R 9 0 R 5 0 R] /Count {count} >>");

    let (_, _, honest_peak) = open(with_table_reaching(&objects(&tree(3)), HIGH));
    let (pages, warnings, claimed_peak) = open(with_table_reaching(&objects(&tree(HIGH)), HIGH));
    assert_eq!(pages, 3);
    assert_eq!(warnings, [lost_page_at(1)]);
    assert!(
        claimed_peak <= honest_peak + (1 << 20),
        "{claimed_peak} bytes live at most, against {honest_peak} for a count of 3"
    );
}

/// Object 0 is never a page, however many times a tree lists it. A
/// million kids `0 0 R` in a compressed object stream take a few
/// kilobytes: they are no pages, one warning names them, and opening the
/// file holds no more memory than with a million kids of the same length
/// that are not references, which the walk passes over.
#[test]
fn a_million_kids_naming_object_0_are_no_pages() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    const KIDS: usize = 1_000_000;
    let tree = |kid: &str| {
        format!(
            "<< /Type /Pages /Kids [3 0 R {}5 0 R] /Count {} >>",
            kid.repeat(KIDS),
            KIDS + 2
        )
    };
    let named = with_tree_compressed(&objects(&tree("0 0 R ")));
    assert!(named.len() < 32 * 1024, "the file is {} bytes", named.len());

    let (_, _, passed_over_peak) = open(with_tree_compressed(&objects(&tree("false "))));
    let (pages, warnings, named_peak) = open(named);
    assert_eq!(pages, 2);
    assert_eq!(warnings, [lost_page_at(1)]);
    assert!(
        named_peak <= passed_over_peak + (1 << 20),
        "{named_peak} bytes live at most, against {passed_over_peak} for kids passed over"
    );
}

/// A compressed object stream can list distinct objects that are not in
/// the file for a few bytes each, more of them than the file's length
/// allows blank pages for. Each is one blank page while the length allows
/// one more, the pages before them counted, and no page after that.
#[test]
fn many_kids_that_cannot_be_read_have_no_more_blank_pages_than_the_file_allows() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    const KIDS: u32 = 200_000;
    let missing: String = (1000..1000 + KIDS).map(|n| format!("{n} 0 R ")).collect();
    let tree = format!("<< /Type /Pages /Kids [3 0 R {missing}5 0 R] >>");
    let pdf = with_tree_compressed(&objects(&tree));
    let allowed = pdf.len() / 4;
    assert!(allowed < KIDS as usize, "the file is {} bytes", pdf.len());

    let (pages, warnings, _) = open(pdf);
    // The first page and the blank pages are as many as the file allows,
    // and the last page is still there.
    assert_eq!(pages, allowed + 1);
    // As many warnings as a tree lists, and the note that the list is cut
    // short.
    assert_eq!(warnings.len(), 65);
}
