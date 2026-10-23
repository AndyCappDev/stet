// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A document told to read no file from the machine
//! (`PdfDocument::set_host_files(false)`) reads none. It runs on a machine
//! of its own (see `host_machine`) that has a CMap in every folder the
//! reader looks in and, on macOS and Windows, a CMYK profile where the
//! system's is looked for; `host_files.rs` shows the reader finding all of
//! them when it may.

mod host_machine;

use host_machine::*;
use stet_graphics::icc::IccCache;
use stet_pdf_reader::{ParsePhase, PdfDocument};

#[test]
fn a_document_kept_from_the_machines_files_reads_none() {
    let machine = Machine::new("off");
    fonts_not_carried_are_drawn_with_stets_own(&machine);
    no_cmap_file_is_read();
    stets_cmyk_profile_stands_in_for_the_systems();
    assert!(
        !machine.font_cache_file().exists(),
        "the machine's fonts were listed"
    );
}

/// No font the document lacks is looked for among the machine's: not a
/// simple font, not a CID TrueType font, and not a CJK one. Each is drawn
/// with stet's own fonts and its text is read, and each is reported as a
/// font not embedded, which is all that is reported. Looking for any of
/// them would have listed the machine's fonts in `~/.cache/stet`.
fn fonts_not_carried_are_drawn_with_stets_own(machine: &Machine) {
    let (pdf, texts) = fonts_not_carried();
    let mut doc = PdfDocument::from_bytes(&pdf).unwrap();
    assert!(doc.host_files());
    doc.set_host_files(false);
    assert!(!doc.host_files());

    for (page, text) in texts.iter().enumerate() {
        let list = read(&mut doc, page);
        let (shown, glyphs) = text_and_glyphs(&list);
        assert_eq!(shown, *text, "page {page}");
        assert_eq!(glyphs, text.len(), "page {page}");
        assert_eq!(
            fills(&list),
            text.len(),
            "page {page}: a glyph drawn for each"
        );
    }
    assert_eq!(
        pages_warned(&doc, ParsePhase::MissingFont),
        [0, 1, 2, 3],
        "{:#?}",
        warnings(&doc)
    );
    assert_eq!(doc.parse_warnings().len(), 4, "{:#?}", warnings(&doc));
    assert!(
        !machine.font_cache_file().exists(),
        "the machine's fonts were listed"
    );
}

/// No CMap file is read: not from `STET_CMAP_DIR`, not from
/// `~/.local/share/stet/CMap`, whether the document names it as its
/// encoding or an embedded CMap names it with `usecmap`. Each of the four
/// would make `<41424344>` four one-byte codes; without them it is two
/// two-byte codes. A CMap named and not read is reported as not found, by
/// name, for each page.
fn no_cmap_file_is_read() {
    let pdf = cmaps_not_carried();
    let mut doc = PdfDocument::from_bytes(&pdf).unwrap();
    doc.set_host_files(false);

    for page in 0..4 {
        let (_, glyphs) = text_and_glyphs(&read(&mut doc, page));
        assert_eq!(glyphs, 2, "page {page} read a CMap file");
    }
    let warnings = warnings(&doc);
    for name in [ENV_CMAP, HOME_CMAP] {
        let not_found = format!("predefined CMap '{name}' not found");
        assert!(
            warnings.iter().any(|w| w.contains(&not_found)),
            "{name}: {warnings:#?}"
        );
    }
    assert_eq!(
        pages_warned(&doc, ParsePhase::MissingCMap),
        [0, 1, 2, 3],
        "{warnings:#?}"
    );
}

/// A document opened without a color cache takes stet's own CMYK profile,
/// not the system's, and draws with it; so does one given a cache with no
/// CMYK profile. A cache given with one keeps it.
fn stets_cmyk_profile_stands_in_for_the_systems() {
    let pdf = cmyk_page();
    let own = IccCache::hash_profile(OWN_PROFILE);
    let planted = IccCache::hash_profile(PLANTED_PROFILE);

    let mut doc = PdfDocument::from_bytes(&pdf).unwrap();
    doc.set_host_files(false);
    let (rgba, _, _) = doc.render_page_to_rgba(0, 9.0).unwrap();
    assert_eq!(doc.icc_cache().default_cmyk_hash(), Some(&own));

    let mut given = IccCache::new();
    given.load_cmyk_profile_bytes(OWN_PROFILE);
    let with_own = PdfDocument::from_bytes_with_icc(&pdf, given).unwrap();
    let (expected, _, _) = with_own.render_page_to_rgba(0, 9.0).unwrap();
    assert_eq!(rgba, expected, "drawn as with stet's profile");

    let without = PdfDocument::from_bytes_with_icc(&pdf, IccCache::new()).unwrap();
    let (unmanaged, _, _) = without.render_page_to_rgba(0, 9.0).unwrap();
    assert_ne!(rgba, unmanaged, "drawn with no profile at all");

    let mut doc = PdfDocument::from_bytes_with_password(&pdf, IccCache::new(), b"").unwrap();
    doc.set_host_files(false);
    assert_eq!(doc.icc_cache().default_cmyk_hash(), Some(&own));

    let mut mine = IccCache::new();
    mine.load_cmyk_profile_bytes(PLANTED_PROFILE);
    let mut doc = PdfDocument::from_bytes_with_icc(&pdf, mine).unwrap();
    doc.set_host_files(false);
    assert_eq!(doc.icc_cache().default_cmyk_hash(), Some(&planted));
}
