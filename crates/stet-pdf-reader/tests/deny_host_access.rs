// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Once the process denies host access (`stet_pdf_reader::deny_host_access`),
//! a document reads no file from the machine even with its own switch,
//! `PdfDocument::set_host_files`, left on: it is read as
//! `no_host_files.rs` reads a document switched off. It runs on the same
//! machine of its own (see `host_machine`), which has a CMap in every
//! folder the reader looks in and, on macOS and Windows, a CMYK profile
//! where the system's is looked for.
//!
//! The switch is process-wide and one-way, so this file holds a single test.

mod host_machine;

use host_machine::*;
use stet_graphics::icc::IccCache;
use stet_pdf_reader::{ParsePhase, PdfDocument};

#[test]
fn denied_host_access_keeps_every_document_from_the_machine() {
    let machine = Machine::new("denied");
    assert!(stet_pdf_reader::host_access_allowed());
    stet_pdf_reader::deny_host_access();
    assert!(!stet_pdf_reader::host_access_allowed());

    // Fonts the document does not carry are drawn with stet's own, and
    // the machine's fonts are never listed.
    let (pdf, texts) = fonts_not_carried();
    let mut doc = PdfDocument::from_bytes(&pdf).unwrap();
    assert!(!doc.host_files(), "the process's denial is the document's");
    doc.set_host_files(true);
    assert!(!doc.host_files(), "and the document cannot lift it");
    for (page, text) in texts.iter().enumerate() {
        let list = read(&mut doc, page);
        let (shown, glyphs) = text_and_glyphs(&list);
        assert_eq!(shown, *text, "page {page}");
        assert_eq!(glyphs, text.len(), "page {page}");
        assert_eq!(fills(&list), text.len(), "page {page}");
    }
    assert_eq!(
        pages_warned(&doc, ParsePhase::MissingFont),
        [0, 1, 2, 3],
        "{:#?}",
        warnings(&doc)
    );

    // No CMap file is read: `<41424344>` stays two two-byte codes.
    let pdf = cmaps_not_carried();
    let mut doc = PdfDocument::from_bytes(&pdf).unwrap();
    for page in 0..4 {
        let (text, glyphs) = text_and_glyphs(&read(&mut doc, page));
        assert_eq!(glyphs, 2, "page {page} read a CMap file");
        assert_eq!(text, "ab", "page {page}");
    }

    // stet's own CMYK profile stands in for the system's.
    let pdf = cmyk_page();
    let doc = PdfDocument::from_bytes(&pdf).unwrap();
    doc.render_page_to_rgba(0, 9.0).unwrap();
    assert_eq!(
        doc.icc_cache().default_cmyk_hash(),
        Some(&IccCache::hash_profile(OWN_PROFILE))
    );

    assert!(
        !machine.font_cache_file().exists(),
        "the machine's fonts were listed"
    );
}
