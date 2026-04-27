// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF Optional Content (layers).
//!
//! Layers — formally Optional Content Groups (OCGs) in ISO 32000-2
//! §8.11 — let a PDF mark slices of its content for selective
//! visibility: CAD layers, watermarks, multilingual annotations,
//! print-only or screen-only overlays, and so on.
//!
//! This module exposes the read-only metadata side of the OCG model:
//!
//! - One [`Layer`] per OCG, carrying the layer's name, intent, lock
//!   state, default visibility, full `/Usage` sub-dict, and any
//!   `/CreatorInfo` hint.
//!
//! Hierarchy (`/Order`), alternate configurations, runtime visibility
//! overrides, OCMD policies, and `/AS` automatic-state rules land in
//! later phases. Phase 1 is just the per-layer record so consumers can
//! enumerate what a document contains.
//!
//! # Quick reference
//!
//! ```no_run
//! use stet_pdf_reader::PdfDocument;
//!
//! let data = std::fs::read("layered.pdf")?;
//! let doc = PdfDocument::from_bytes(&data)?;
//!
//! for layer in doc.layers() {
//!     println!(
//!         "{} (id={}, locked={}, default_visible={})",
//!         layer.name, layer.ocg_id, layer.locked, layer.default_visible
//!     );
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub mod metadata;

pub use metadata::{
    CreatorInfo, ExportUsage, LanguageUsage, Layer, LayerIntent, LayerUsage, PageElementSubtype,
    PrintUsage, UsageState, UserUsage, ViewUsage, ZoomUsage,
};
