// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF parsing error types.

use thiserror::Error;

/// Errors that can occur during PDF parsing and object resolution.
///
/// Marked `#[non_exhaustive]` so new error variants can land additively;
/// downstream consumers must include a wildcard arm in `match` sites.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PdfError {
    #[error("not a PDF file (missing %PDF header)")]
    NotAPdf,

    #[error("unsupported PDF version: {0}")]
    UnsupportedVersion(String),

    #[error("startxref not found")]
    NoStartXref,

    #[error("malformed xref table at offset {0}")]
    MalformedXref(usize),

    #[error("malformed trailer dictionary")]
    MalformedTrailer,

    #[error("object {obj_num} {gen_num} not found")]
    ObjectNotFound { obj_num: u32, gen_num: u16 },

    #[error("unexpected token: expected {expected}, got {got}")]
    UnexpectedToken { expected: String, got: String },

    #[error("unterminated {0}")]
    Unterminated(&'static str),

    #[error("invalid object at offset {0}")]
    InvalidObject(usize),

    #[error("stream missing /Length")]
    StreamMissingLength,

    #[error("unsupported filter: {0}")]
    UnsupportedFilter(String),

    #[error("decompression error: {0}")]
    DecompressionError(String),

    #[error("missing required key /{0} in dictionary")]
    MissingKey(&'static str),

    #[error("type mismatch: expected {expected} for /{key}")]
    TypeMismatch { key: String, expected: &'static str },

    #[error("page index {0} out of range (document has {1} pages)")]
    PageOutOfRange(usize, usize),

    #[error("circular reference detected for object {0} {1}")]
    CircularReference(u32, u16),

    /// A recursive-descent parse hit its nesting cap.
    ///
    /// Raised instead of letting a crafted file exhaust the native stack,
    /// which aborts the process rather than unwinding.
    #[error("{context} nesting exceeded the limit of {limit}")]
    NestingTooDeep {
        /// What was being parsed, e.g. `"array/dictionary"`.
        context: &'static str,
        /// The cap that was reached.
        limit: u32,
    },

    #[error("PDF requires a password")]
    PasswordRequired,

    /// An image has more samples than
    /// [`PdfDocument::set_max_image_pixels`](crate::PdfDocument::set_max_image_pixels)
    /// allows, so it is not decoded. A page leaves such an image out and
    /// reports it as
    /// [`ParsePhase::ImageLeftOut`](crate::ParsePhase::ImageLeftOut).
    #[error(
        "image of {width} x {height} samples is over the limit of {limit} set for this document"
    )]
    ImageOverLimit {
        /// The size that was checked: for a JPEG 2000 image the one it
        /// would be decoded at.
        width: u32,
        height: u32,
        /// The limit in force.
        limit: u64,
    },

    /// The [`PageArea`](crate::PageArea) chosen with
    /// [`PdfDocument::set_page_area`](crate::PdfDocument::set_page_area)
    /// does not overlap the page's MediaBox, or is not a finite rectangle,
    /// so there is nothing to render.
    #[error("page index {page}: the requested page area does not overlap the MediaBox")]
    EmptyPageArea {
        /// The 0-based page index.
        page: usize,
    },

    /// The page asked for more than the reader will interpret, and was
    /// stopped: see [`ContentLimit`].
    ///
    /// The page is the one asked for. It is not drawn at all, rather than
    /// drawn in part, and what was cut short is not repeated in
    /// [`parse_warnings`](crate::PdfDocument::parse_warnings): this error
    /// is the report. The document is unaffected, and its other pages can
    /// be read as before.
    #[error("page content over the reader's limit: {0}")]
    ContentLimit(ContentLimit),

    #[error("{0}")]
    Other(String),
}

/// Which limit a page's content passed, carried by
/// [`PdfError::ContentLimit`], with the limit that was in force.
///
/// Marked `#[non_exhaustive]` so new limits can land additively.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ContentLimit {
    /// More graphics states saved with `q` and not yet restored than
    /// [`MAX_SAVED_STATES`](crate::MAX_SAVED_STATES).
    #[error("more than {0} graphics states saved with q at once")]
    SavedStates(usize),
    /// More bytes of content than the limit set with
    /// [`set_max_content_bytes`](crate::PdfDocument::set_max_content_bytes).
    #[error("more than {0} bytes of content")]
    ContentBytes(u64),
}
