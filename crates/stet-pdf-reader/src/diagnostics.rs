// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Parse-time diagnostics — non-fatal warnings the structural parsers
//! emit when they encounter recoverable malformations.
//!
//! Every accessor on [`PdfDocument`] is fail-soft: it returns valid
//! data on a best-effort basis and skips entries it can't interpret.
//! When something is *skipped*, a [`ParseWarning`] is recorded so
//! callers can surface a warning to their users (e.g. "this PDF's
//! outline contained a cycle and was truncated at depth N") without
//! the absence of the data being silent.
//!
//! Warnings accumulate on the `PdfDocument` as accessors are called
//! for the first time; subsequent cached calls don't re-emit. Use
//! [`PdfDocument::parse_warnings`] to inspect.
//!
//! [`PdfDocument`]: crate::PdfDocument
//! [`PdfDocument::parse_warnings`]: crate::PdfDocument::parse_warnings

use std::cell::RefCell;

/// One non-fatal parsing problem.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseWarning {
    /// Which structural area produced this warning.
    pub phase: ParsePhase,
    /// Where in the document the problem was — page index, object
    /// number, or the field name (for AcroForm warnings).
    pub location: Option<LocationHint>,
    /// Human-readable message.
    pub message: String,
    /// How seriously to surface this. The reader itself does not act
    /// on severity; it's purely a hint for consumers building UI or
    /// log output.
    pub severity: Severity,
}

/// The structural area a warning came from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ParsePhase {
    Metadata,
    ViewerPreferences,
    Outline,
    Destinations,
    Annotations {
        page: usize,
    },
    Form,
    PageBoxes {
        page: usize,
    },
    EmbeddedFiles,
    /// Optional Content (layers): metadata, hierarchy, configurations.
    Layers,
    /// Page content, found while a page was being rendered: a content
    /// stream that stops making sense part-way, a font or a soft mask that
    /// could not be loaded, an image stream decoded only in part. The page
    /// is in the warning's [`location`](ParseWarning::location), as
    /// [`LocationHint::Page`], when the render was of a page.
    ///
    /// These are recorded as pages are rendered, not when the document is
    /// opened, and once each: rendering a page again adds nothing.
    Content,
    /// The page tree, read when the document is opened: a branch nested
    /// too deeply to follow, or a node listed under two parents. The pages
    /// below such a node are not in the document's page list. Also a page
    /// list found by scanning the file for page objects because the tree
    /// could not be followed, which may hold pages from an earlier
    /// revision or miss some.
    PageTree,
    /// The cross-reference table, read when the document is opened. A
    /// table rebuilt by scanning the file for objects, because the one the
    /// file gives is missing or damaged, is reported at
    /// [`Severity::Info`]: the objects were found where they are. What
    /// could not be found is reported where it is missed, a page in
    /// [`PageTree`](Self::PageTree) and page content in
    /// [`Content`](Self::Content).
    CrossReference,
    /// An image left out of a page because it has more samples than
    /// [`set_max_image_pixels`](crate::PdfDocument::set_max_image_pixels)
    /// allows. The page is in the warning's
    /// [`location`](ParseWarning::location); `width` and `height` are the
    /// image's size as the file stores it, which for a JPEG 2000 image
    /// may be larger than the size it would have been decoded at.
    ///
    /// Recorded as pages are rendered, once for each page and size: an
    /// image used on two pages is reported for each of them. A page lists
    /// at most 64 of them; the rest of what it leaves out is not listed.
    ImageLeftOut {
        width: u32,
        height: u32,
    },
    /// A font the document uses without embedding its program, that is
    /// not one of the standard 14 fonts the reader carries. A substitute is
    /// drawn in its place, so its text is still read, but its glyphs may
    /// not look as they should. The page is in the warning's
    /// [`location`](ParseWarning::location); the message does not name the
    /// font.
    ///
    /// An embedded font program that cannot be read is a
    /// [`Content`](Self::Content) warning instead.
    MissingFont,
    /// A predefined CMap a composite font names for its encoding that is
    /// neither embedded nor found among the CMap files the reader looks
    /// in. The text in that font is still drawn and read: its codes are
    /// taken two bytes at a time, and turned into text through the
    /// document's own `/ToUnicode` where it gives one. Without the CMap
    /// that can be wrong, so a program that needs the text to be right can
    /// treat it as not to be trusted. `Identity-H` and `Identity-V` need
    /// no file and are never reported. The page is in the warning's
    /// [`location`](ParseWarning::location); the message names the CMap as
    /// the document gives it.
    ///
    /// An embedded CMap that names another with `usecmap` that cannot be
    /// found is reported the same way, naming the one not found; the
    /// codes the embedded CMap maps itself are read as before.
    MissingCMap,
}

/// Where in the document a problem occurred.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LocationHint {
    /// 0-based page index.
    Page(usize),
    /// Indirect object reference.
    Object { obj_num: u32, gen_num: u16 },
    /// Fully-qualified form-field name.
    FieldName(String),
    /// Outline-item title (the closest thing to a stable ID outline
    /// entries have).
    OutlineTitle(String),
    /// Embedded-file or named-destination key.
    Name(String),
}

/// Severity hint for consumers. The reader treats all of these the
/// same internally; they only inform UI/log presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Severity {
    /// Worth noting but the data is fine — e.g. "this PDF used a
    /// truncated date string but we recovered the year/month".
    Info,
    /// Some data was dropped or defaulted — e.g. "annotation
    /// without /Rect was skipped".
    Warning,
    /// A whole structural area couldn't be parsed at all — e.g.
    /// "name tree was too deep and traversal stopped".
    Error,
}

/// Internal accumulator used by parsers to record warnings.
///
/// Wraps a [`RefCell`] so the document-level accessor closures can
/// pass `&WarningSink` to parsers without juggling exclusive
/// references. Pushes are interior-mutable; the document's
/// `parse_warnings()` accessor reads from the same cell.
#[derive(Debug, Default)]
pub struct WarningSink {
    inner: RefCell<Vec<ParseWarning>>,
}

impl WarningSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a warning into the sink.
    ///
    /// A warning raised while a caller still holds the slice from
    /// [`borrow_slice`](Self::borrow_slice) is dropped: the list cannot
    /// grow under a reader, and losing a diagnostic is better than a
    /// panic in the middle of a render.
    pub fn push(&self, w: ParseWarning) {
        if let Ok(mut warnings) = self.inner.try_borrow_mut() {
            warnings.push(w);
        }
    }

    /// Convenience: build and push a warning with the given pieces.
    pub fn record(
        &self,
        phase: ParsePhase,
        location: Option<LocationHint>,
        severity: Severity,
        message: impl Into<String>,
    ) {
        self.push(ParseWarning {
            phase,
            location,
            severity,
            message: message.into(),
        });
    }

    /// Like [`record`](Self::record), unless the same warning — phase,
    /// location and message — is already in the sink. For warnings raised
    /// by work that is repeated, such as rendering a page.
    pub fn record_once(
        &self,
        phase: ParsePhase,
        location: Option<LocationHint>,
        severity: Severity,
        message: impl Into<String>,
    ) {
        self.record_if_new(phase, location, severity, message.into());
    }

    /// [`record_once`](Self::record_once), saying whether the warning was
    /// new.
    pub(crate) fn record_if_new(
        &self,
        phase: ParsePhase,
        location: Option<LocationHint>,
        severity: Severity,
        message: String,
    ) -> bool {
        let known = self
            .inner
            .borrow()
            .iter()
            .any(|w| w.phase == phase && w.location == location && w.message == message);
        if !known {
            self.push(ParseWarning {
                phase,
                location,
                message,
                severity,
            });
        }
        !known
    }

    /// Borrow the underlying slice for read-only access. Held borrow
    /// blocks further pushes until dropped, but parsers rarely hold
    /// this — they only push.
    pub fn borrow_slice(&self) -> std::cell::Ref<'_, [ParseWarning]> {
        std::cell::Ref::map(self.inner.borrow(), Vec::as_slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_borrow() {
        let sink = WarningSink::new();
        sink.record(
            ParsePhase::Outline,
            Some(LocationHint::OutlineTitle("Chapter 1".to_string())),
            Severity::Warning,
            "cycle detected; truncating",
        );
        sink.record(
            ParsePhase::Annotations { page: 3 },
            None,
            Severity::Info,
            "missing /Rect; entry skipped",
        );

        let view = sink.borrow_slice();
        assert_eq!(view.len(), 2);
        assert_eq!(view[0].phase, ParsePhase::Outline);
        assert_eq!(view[0].severity, Severity::Warning);
        assert_eq!(view[1].phase, ParsePhase::Annotations { page: 3 });
        assert_eq!(view[1].severity, Severity::Info);
    }

    #[test]
    fn location_hint_variants_are_distinguishable() {
        let p = LocationHint::Page(5);
        let o = LocationHint::Object {
            obj_num: 42,
            gen_num: 0,
        };
        let f = LocationHint::FieldName("user.email".to_string());
        let n = LocationHint::Name("attachment.csv".to_string());
        assert_ne!(p, o);
        assert_ne!(o, f);
        assert_ne!(f, n);
    }
}
