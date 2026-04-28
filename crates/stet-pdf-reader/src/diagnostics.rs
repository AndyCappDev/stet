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
#[derive(Debug, Clone, PartialEq, Eq)]
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
    pub fn push(&self, w: ParseWarning) {
        self.inner.borrow_mut().push(w);
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
