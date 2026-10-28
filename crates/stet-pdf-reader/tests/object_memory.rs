// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The objects read from a file may take at most 256 bytes of memory for
//! each byte of the file, and never less than 16 MiB, decoded object
//! streams included: see `ParsePhase::ObjectMemoryLimit`.
//!
//! A few kilobytes of compressed object stream decode to megabytes of
//! object text, and every two bytes of that, `1 `, is a 40-byte object, so
//! nothing else bounds what reading a small file can hold. Each test here
//! shows that a file of a few kilobytes holds no more than the allowance
//! however much it declares, and that a large file compressed as far as
//! real files are is read in full.
//!
//! This file has its own allocator, which records the most memory live at
//! once.

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

/// The least the objects read from any file may take.
const ALLOWANCE: usize = 16 << 20;

/// `bytes` compressed as a `/FlateDecode` stream.
fn deflated(bytes: &[u8]) -> Vec<u8> {
    let mut packed = ZlibEncoder::new(Vec::new(), Compression::best());
    packed.write_all(bytes).unwrap();
    packed.finish().unwrap()
}

/// A PDF of the objects `written` out in the file and the objects
/// `compressed` in one object stream, each with its number. The object
/// stream and the cross-reference stream after it take the two numbers
/// after the highest of those. Also how many bytes the object stream's
/// data takes, and how many it decodes to.
fn pdf_of(written: &[(u32, Vec<u8>)], compressed: &[(u32, Vec<u8>)]) -> (Vec<u8>, usize, usize) {
    let highest = written
        .iter()
        .chain(compressed)
        .map(|(n, _)| *n)
        .max()
        .unwrap();
    let (stream_num, xref_num) = (highest + 1, highest + 2);

    let mut header = String::new();
    let mut bodies = Vec::new();
    for (num, body) in compressed {
        header.push_str(&format!("{num} {} ", bodies.len()));
        bodies.extend(body);
        bodies.push(b' ');
    }
    let decoded = [header.as_bytes(), &bodies].concat();
    let packed = deflated(&decoded);

    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = std::collections::HashMap::new();
    for (num, body) in written {
        offsets.insert(*num, pdf.len());
        pdf.extend(format!("{num} 0 obj\n").as_bytes());
        pdf.extend(body);
        pdf.extend(b"\nendobj\n");
    }
    offsets.insert(stream_num, pdf.len());
    pdf.extend(
        format!(
            "{stream_num} 0 obj\n<< /Type /ObjStm /N {} /First {} /Filter /FlateDecode \
             /Length {} >>\nstream\n",
            compressed.len(),
            header.len(),
            packed.len()
        )
        .as_bytes(),
    );
    pdf.extend(&packed);
    pdf.extend(b"\nendstream\nendobj\n");
    let xref = pdf.len();
    offsets.insert(xref_num, xref);

    // Each entry is its type, a four-byte offset or object stream number,
    // and a four-byte generation or index.
    let index: std::collections::HashMap<u32, usize> = compressed
        .iter()
        .enumerate()
        .map(|(i, (num, _))| (*num, i))
        .collect();
    let mut table = Vec::new();
    for num in 0..=xref_num {
        let (kind, field, second) = match (offsets.get(&num), index.get(&num)) {
            (Some(&offset), _) => (1u8, offset, 0),
            (None, Some(&i)) => (2, stream_num as usize, i),
            (None, None) => (0, 0, 0),
        };
        table.push(kind);
        table.extend((field as u32).to_be_bytes());
        table.extend((second as u32).to_be_bytes());
    }
    let table = deflated(&table);
    pdf.extend(
        format!(
            "{xref_num} 0 obj\n<< /Type /XRef /Size {} /W [1 4 4] /Root 1 0 R \
             /Filter /FlateDecode /Length {} >>\nstream\n",
            xref_num + 1,
            table.len()
        )
        .as_bytes(),
    );
    pdf.extend(&table);
    pdf.extend(format!("\nendstream\nendobj\nstartxref\n{xref}\n%%EOF\n").as_bytes());
    (pdf, packed.len(), decoded.len())
}

/// A catalog, object 1, and a page, object 3, written out in the file.
fn catalog_and_page() -> Vec<(u32, Vec<u8>)> {
    vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>".to_vec(),
        ),
    ]
}

/// What a warning is, without its message.
type Kind = (ParsePhase, Option<LocationHint>, Severity);

/// Opens `pdf`: its page count, its warnings, and the most memory live
/// while it was opened.
fn open(pdf: Vec<u8>) -> (usize, Vec<Kind>, usize) {
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let doc = PdfDocument::from_owned(pdf).expect("the document opens");
    let peak = PEAK.load(Ordering::Relaxed).saturating_sub(before);
    let warnings = doc
        .parse_warnings()
        .iter()
        .map(|w| (w.phase.clone(), w.location.clone(), w.severity))
        .collect();
    (doc.page_count(), warnings, peak)
}

/// What the object memory warning for object `obj_num` looks like.
fn over_the_allowance(obj_num: u32) -> Kind {
    (
        ParsePhase::ObjectMemoryLimit,
        Some(LocationHint::Object {
            obj_num,
            gen_num: 0,
        }),
        Severity::Warning,
    )
}

/// The page tree, object 2, compressed, with a million objects beside its
/// kid: 2 KB of object stream, 2 MB of text, 40 MB of objects. It is not
/// read, and the one warning of its kind names it; the page is found by
/// scanning the file instead. Opening the file holds no more than twice
/// the allowance at once, which is the room an array takes as it grows.
#[test]
fn a_million_objects_in_a_few_kilobytes_are_not_read() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let tree = format!(
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /Junk [{}] >>",
        "1 ".repeat(1_000_000)
    );
    let (pdf, _, decoded) = pdf_of(&catalog_and_page(), &[(2, tree.into_bytes())]);
    assert!(pdf.len() < 8 * 1024, "the file is {} bytes", pdf.len());
    assert!(decoded > 1_000_000);

    let (pages, warnings, peak) = open(pdf);
    assert_eq!(pages, 1);
    assert_eq!(
        warnings
            .iter()
            .filter(|w| w.0 == ParsePhase::ObjectMemoryLimit)
            .collect::<Vec<_>>(),
        [&over_the_allowance(2)]
    );
    assert!(
        peak < 2 * ALLOWANCE + (4 << 20),
        "{peak} bytes live at most"
    );
}

/// An object stream whose text alone is more than the allowance is not
/// decoded past it: here 20 MB of string from a few kilobytes.
#[test]
fn an_object_stream_that_decodes_past_the_allowance_is_not_read() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let tree = format!(
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /Junk ({}) >>",
        "a".repeat(20 << 20)
    );
    let (pdf, _, _) = pdf_of(&catalog_and_page(), &[(2, tree.into_bytes())]);
    assert!(pdf.len() < 64 * 1024, "the file is {} bytes", pdf.len());

    let (pages, warnings, peak) = open(pdf);
    assert_eq!(pages, 1);
    assert!(warnings.contains(&over_the_allowance(2)), "{warnings:?}");
    assert!(
        peak < 2 * ALLOWANCE + (4 << 20),
        "{peak} bytes live at most"
    );
}

/// Fifty thousand pages, every one in an object stream that compresses
/// as far as any real file's does, take more than the least allowance,
/// and well within what the file's length allows: every page is read.
#[test]
fn a_large_document_compressed_as_far_as_real_files_are_is_read_in_full() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    const PAGES: u32 = 50_000;
    let first_page = 10;
    let kids: String = (first_page..first_page + PAGES)
        .map(|n| format!("{n} 0 R "))
        .collect();
    let written = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            format!("<< /Type /Pages /Kids [{kids}] /Count {PAGES} >>").into_bytes(),
        ),
        (4, b"<< /Length 0 >>\nstream\n\nendstream".to_vec()),
        (
            5,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        ),
    ];
    let compressed: Vec<(u32, Vec<u8>)> = (first_page..first_page + PAGES)
        .map(|n| {
            let page = format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> /ProcSet [/PDF /Text] >> \
                 /StructParents {n} >>"
            );
            (n, page.into_bytes())
        })
        .collect();
    let (pdf, packed, decoded) = pdf_of(&written, &compressed);
    // As far as the most compressed of the test files' object streams,
    // 19 to 1, or near it.
    assert!(decoded > 15 * packed, "{decoded} bytes from {packed}");

    let (pages, warnings, peak) = open(pdf);
    assert_eq!(pages, PAGES as usize);
    assert!(
        !warnings
            .iter()
            .any(|w| w.0 == ParsePhase::ObjectMemoryLimit),
        "{warnings:?}"
    );
    // More than the least allowance, so the file's length is what lets
    // it be read.
    assert!(peak > ALLOWANCE, "{peak} bytes live at most");
}
