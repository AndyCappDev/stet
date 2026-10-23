// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! By default the reader reads what it needs from the machine: the fonts
//! installed on it, CMap files and the system's CMYK profile. This runs on
//! the same machine of its own as `no_host_files.rs` (see `host_machine`)
//! and finds there what that test does not read.

mod host_machine;

use host_machine::*;
use stet_pdf_reader::{ParsePhase, PdfDocument};

#[test]
fn a_document_reads_the_machines_files_by_default() {
    let machine = Machine::new("on");
    fonts_not_carried_are_looked_for_on_the_machine(&machine);
    cmap_files_are_read();
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    the_systems_cmyk_profile_is_taken();
}

/// A font the document lacks is looked for among the machine's, which
/// lists them in `~/.cache/stet` the first time.
fn fonts_not_carried_are_looked_for_on_the_machine(machine: &Machine) {
    let (pdf, texts) = fonts_not_carried();
    let mut doc = PdfDocument::from_bytes(&pdf).unwrap();
    assert!(doc.host_files());
    for page in 0..texts.len() {
        read(&mut doc, page);
    }
    assert!(
        machine.font_cache_file().exists(),
        "the machine's fonts were not looked for"
    );
}

/// Each CMap the document names is read from the folder that has it, so
/// `<41424344>` is four one-byte codes, and none is reported missing.
fn cmap_files_are_read() {
    let pdf = cmaps_not_carried();
    let mut doc = PdfDocument::from_bytes(&pdf).unwrap();
    for page in 0..4 {
        let (_, glyphs) = text_and_glyphs(&read(&mut doc, page));
        assert_eq!(glyphs, 4, "page {page} did not read its CMap");
    }
    assert!(
        pages_warned(&doc, ParsePhase::MissingCMap).is_empty(),
        "{:#?}",
        warnings(&doc)
    );
}

/// A document opened without a color cache takes the system's CMYK
/// profile: here, the one planted where the search starts.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn the_systems_cmyk_profile_is_taken() {
    use stet_graphics::icc::IccCache;

    let doc = PdfDocument::from_owned(cmyk_page()).unwrap();
    assert_eq!(
        doc.icc_cache().default_cmyk_hash(),
        Some(&IccCache::hash_profile(PLANTED_PROFILE))
    );
}
