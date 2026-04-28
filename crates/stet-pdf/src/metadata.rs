// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `/ViewerPreferences` and `/Metadata` (XMP) writers.
//!
//! Both attach to `/Catalog` at end-of-job. `ViewerPreferences` is a
//! plain dict; `Metadata` is a stream object with `/Type /Metadata` /
//! `/Subtype /XML` carrying raw XMP XML.

use stet_core::pdfmark::{MetadataRecord, ViewerPrefsRecord};

use crate::pdf_objects::PdfObj;
use crate::pdf_writer::PdfWriter;

/// Allowed `/PageLayout` values per PDF spec. Anything outside this set
/// is dropped silently — Adobe pdfwrite behaves the same.
const PAGE_LAYOUT_VALUES: &[&[u8]] = &[
    b"SinglePage",
    b"OneColumn",
    b"TwoColumnLeft",
    b"TwoColumnRight",
    b"TwoPageLeft",
    b"TwoPageRight",
];

/// Allowed catalog-level `/PageMode` values.
const PAGE_MODE_VALUES: &[&[u8]] = &[
    b"UseNone",
    b"UseOutlines",
    b"UseThumbs",
    b"FullScreen",
    b"UseOC",
    b"UseAttachments",
];

/// Allowed `/NonFullScreenPageMode` values (subset of `/PageMode` —
/// `FullScreen` and `UseAttachments` are not legal here).
const NON_FULL_SCREEN_PAGE_MODE_VALUES: &[&[u8]] =
    &[b"UseNone", b"UseOutlines", b"UseThumbs", b"UseOC"];

/// Allowed `/Direction` values.
const DIRECTION_VALUES: &[&[u8]] = &[b"L2R", b"R2L"];

/// Build the `/ViewerPreferences` indirect object from an effective
/// merged record. Returns `None` when no nested entry is set.
pub fn write_viewer_prefs(writer: &mut PdfWriter, prefs: &ViewerPrefsRecord) -> Option<u32> {
    let mut entries: Vec<(Vec<u8>, PdfObj)> = Vec::new();
    push_bool(&mut entries, b"HideToolbar", prefs.hide_toolbar);
    push_bool(&mut entries, b"HideMenubar", prefs.hide_menubar);
    push_bool(&mut entries, b"HideWindowUI", prefs.hide_window_ui);
    push_bool(&mut entries, b"FitWindow", prefs.fit_window);
    push_bool(&mut entries, b"CenterWindow", prefs.center_window);
    push_bool(&mut entries, b"DisplayDocTitle", prefs.display_doc_title);
    push_validated_name(
        &mut entries,
        b"NonFullScreenPageMode",
        prefs.non_full_screen_page_mode.as_deref(),
        NON_FULL_SCREEN_PAGE_MODE_VALUES,
    );
    push_validated_name(
        &mut entries,
        b"Direction",
        prefs.direction.as_deref(),
        DIRECTION_VALUES,
    );
    if entries.is_empty() {
        return None;
    }
    Some(writer.add_object(&PdfObj::Dict(entries)))
}

/// Validate a `/PageLayout` value and return the canonical form, or
/// `None` if the producer-supplied name isn't on the spec list.
pub fn validated_page_layout(s: &str) -> Option<&'static [u8]> {
    PAGE_LAYOUT_VALUES
        .iter()
        .copied()
        .find(|v| *v == s.as_bytes())
}

/// Validate a `/PageMode` value the same way.
pub fn validated_page_mode(s: &str) -> Option<&'static [u8]> {
    PAGE_MODE_VALUES
        .iter()
        .copied()
        .find(|v| *v == s.as_bytes())
}

/// Build the `/Metadata` stream object carrying the supplied XMP
/// bytes. Always emits `/Type /Metadata /Subtype /XML`. The XMP is
/// stored uncompressed: PDF spec requires this for grep-friendly
/// metadata extraction by downstream tools.
pub fn write_xmp_metadata(writer: &mut PdfWriter, record: &MetadataRecord) -> u32 {
    let entries = vec![
        (b"Type".to_vec(), PdfObj::name("Metadata")),
        (b"Subtype".to_vec(), PdfObj::name("XML")),
    ];
    writer.add_stream(entries, &record.xmp_bytes, false)
}

fn push_bool(entries: &mut Vec<(Vec<u8>, PdfObj)>, key: &[u8], value: Option<bool>) {
    if let Some(v) = value {
        entries.push((key.to_vec(), PdfObj::Bool(v)));
    }
}

fn push_validated_name(
    entries: &mut Vec<(Vec<u8>, PdfObj)>,
    key: &[u8],
    value: Option<&str>,
    allowed: &[&[u8]],
) {
    let Some(s) = value else {
        return;
    };
    if let Some(v) = allowed.iter().copied().find(|v| *v == s.as_bytes()) {
        entries.push((key.to_vec(), PdfObj::Name(v.to_vec())));
    }
}
