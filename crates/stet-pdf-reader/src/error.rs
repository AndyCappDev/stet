// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF parsing error types.

use thiserror::Error;

/// Errors that can occur during PDF parsing and object resolution.
#[derive(Debug, Error)]
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

    #[error("encrypted PDF (not yet supported)")]
    Encrypted,

    #[error("{0}")]
    Other(String),
}
