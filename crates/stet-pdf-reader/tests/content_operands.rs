// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The operands waiting for an operator may take at most 16 MiB between
//! them, and an inline image's dictionary as much again of its own.
//!
//! A content stream decodes to as much as hundreds of megabytes from a few
//! kilobytes, and an operator takes a few operands, so nothing else bounds
//! what they hold: fifty million `1 ` with no operator after them took
//! 1.7 GB to render, and as one array 4 GB. Past the allowance the
//! operands waiting are dropped, as no operator's; an array or dictionary
//! too large on its own is read past, so the stream after it is read as it
//! would have been; and the page gets one warning. The most any operator
//! on any page of 717 test files takes is 14.7 KB.
//!
//! This file has its own allocator, which records the most memory live at
//! once while a page is drawn.

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use flate2::Compression;
use flate2::write::ZlibEncoder;
use stet_graphics::display_list::{DisplayElement, DisplayList};
use stet_pdf_reader::{ParsePhase, PdfDocument, Severity};

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

/// What the operands waiting for an operator may take.
const ALLOWANCE: usize = 16 << 20;

/// A one-page PDF whose content stream is `content`, compressed.
fn page_of(content: &[u8]) -> Vec<u8> {
    let mut packed = ZlibEncoder::new(Vec::new(), Compression::best());
    packed.write_all(content).unwrap();
    let packed = packed.finish().unwrap();
    let mut stream = format!(
        "<< /Length {} /Filter /FlateDecode >>\nstream\n",
        packed.len()
    )
    .into_bytes();
    stream.extend(&packed);
    stream.extend(b"\nendstream");
    let objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>".to_vec(),
        stream,
    ];
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

/// Draws the one page of `pdf`: what it draws, its warnings' phases, and
/// the most memory live while it was drawn, beyond the decoded content
/// stream, which is `content_len` bytes. The stream takes up to twice that
/// while it is decoded, as its buffer grows.
fn draw(pdf: &[u8], content_len: usize) -> (DisplayList, Vec<(ParsePhase, Severity)>, usize) {
    let doc = PdfDocument::from_bytes(pdf).unwrap();
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let list = doc.render_page(0, 72.0).unwrap();
    let peak = PEAK
        .load(Ordering::Relaxed)
        .saturating_sub(before)
        .saturating_sub(content_len);
    let warnings = doc
        .parse_warnings()
        .iter()
        .map(|w| (w.phase.clone(), w.severity))
        .collect();
    (list, warnings, peak)
}

/// The gray levels of the page's fills, in paint order.
fn fills(list: &DisplayList) -> Vec<f64> {
    list.elements()
        .iter()
        .filter_map(|e| match e {
            DisplayElement::Fill { params, .. } => Some(params.color.r),
            _ => None,
        })
        .collect()
}

/// Ten million numbers with no operator after them, twenty megabytes of
/// content, are more than the allowance: they are dropped, with one
/// warning, and what follows them is drawn, the operator just after them
/// taking its operands.
#[test]
fn operands_with_no_operator_past_the_allowance_are_dropped() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let mut content = b"1 ".repeat(10_000_000);
    content.extend(b"0.5 g 0 0 10 10 re f");
    let pdf = page_of(&content);
    assert!(pdf.len() < 64 * 1024, "the file is {} bytes", pdf.len());

    let (list, warnings, peak) = draw(&pdf, content.len());
    assert_eq!(fills(&list), [0.5]);
    assert_eq!(warnings, [(ParsePhase::Content, Severity::Warning)]);
    assert!(
        peak < 2 * ALLOWANCE + content.len(),
        "{peak} bytes live at most"
    );
}

/// One array of five million numbers is more than the allowance on its
/// own: it is read past, the `TJ` that would take it has nothing to show,
/// and the stream after it is drawn.
#[test]
fn an_array_operand_past_the_allowance_is_read_past() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let mut content = b"BT [".to_vec();
    content.extend(b"1 ".repeat(5_000_000));
    content.extend(b"] TJ ET 0.25 g 0 0 10 10 re f");
    let pdf = page_of(&content);

    let (list, warnings, peak) = draw(&pdf, content.len());
    assert_eq!(fills(&list), [0.25]);
    assert!(
        warnings.contains(&(ParsePhase::Content, Severity::Warning)),
        "{warnings:?}"
    );
    assert!(
        peak < 2 * ALLOWANCE + content.len(),
        "{peak} bytes live at most"
    );
}

/// The same array inside a dictionary operand, nested in arrays of its
/// own, is read past the same way: the `BDC` has no properties, and the
/// stream after it is drawn.
#[test]
fn a_dictionary_operand_past_the_allowance_is_read_past() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let mut content = b"/P << /A [[[".to_vec();
    content.extend(b"1 ".repeat(5_000_000));
    content.extend(b"]]] /B (>>]) >> BDC EMC 0.75 g 0 0 10 10 re f");
    let pdf = page_of(&content);

    let (list, _, peak) = draw(&pdf, content.len());
    assert_eq!(fills(&list), [0.75]);
    assert!(
        peak < 2 * ALLOWANCE + content.len(),
        "{peak} bytes live at most"
    );
}

/// An inline image whose dictionary is more than its allowance keeps the
/// entries before that: the image is drawn, and so is what follows it.
#[test]
fn an_inline_image_dictionary_past_the_allowance_keeps_its_first_entries() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let mut content = b"q 10 0 0 10 0 0 cm BI /W 1 /H 1 /BPC 8 /CS /G /X [".to_vec();
    content.extend(b"1 ".repeat(5_000_000));
    content.extend(b"] /Y 1 ID \x80 EI Q 0.5 g 0 0 10 10 re f");
    let pdf = page_of(&content);

    let (list, warnings, peak) = draw(&pdf, content.len());
    let images = list
        .elements()
        .iter()
        .filter(|e| matches!(e, DisplayElement::Image { .. }))
        .count();
    assert_eq!(images, 1, "{list:?}");
    assert_eq!(fills(&list), [0.5]);
    assert!(
        warnings.contains(&(ParsePhase::Content, Severity::Warning)),
        "{warnings:?}"
    );
    assert!(
        peak < 2 * ALLOWANCE + content.len(),
        "{peak} bytes live at most"
    );
}

/// An inline image with two hundred thousand keys is read in time: each key
/// is looked for among those before it in a set, not one by one.
#[test]
fn an_inline_image_with_many_keys_is_read_in_time() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let keys: String = (0..200_000).map(|i| format!("/K{i} 1 ")).collect();
    let content =
        format!("q 10 0 0 10 0 0 cm BI /W 1 /H 1 /BPC 8 /CS /G {keys}ID \x7f EI Q 0 0 10 10 re f");
    let pdf = page_of(content.as_bytes());
    let started = std::time::Instant::now();
    let (list, _, _) = draw(&pdf, content.len());
    assert_eq!(fills(&list).len(), 1);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

/// Operands as large as a real page's, and much larger, are read as they
/// are: a `TJ` of fifty thousand pieces, ten times the size of the largest
/// any test file shows at once, draws every glyph, with no warning.
#[test]
fn a_large_real_operand_is_read() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let pieces: String = (0..50_000).map(|i| format!("(a) {} ", i % 7)).collect();
    let content = format!("BT /F1 1 Tf 0 50 Td [{pieces}] TJ ET 0.5 g 0 0 10 10 re f");
    let pdf = page_of(content.as_bytes());
    let (list, warnings, _) = draw(&pdf, content.len());
    // Each glyph is a fill of its own, and the square after them.
    let fills = fills(&list);
    assert_eq!(fills.len(), 50_001);
    assert_eq!(fills.last(), Some(&0.5));
    assert!(
        !warnings.iter().any(|w| w.0 == ParsePhase::Content),
        "{warnings:?}"
    );
}
