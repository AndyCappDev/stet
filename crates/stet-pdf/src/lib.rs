// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF output device for stet — converts display lists into
//! print-production-quality PDF files.
//!
//! [`PdfDevice`] is an [`OutputDevice`](stet_core::device::OutputDevice)
//! implementation that walks a display list and emits the corresponding
//! PDF constructs. It preserves the full fidelity of the source, including
//! native CMYK and spot colours (without a lossy RGB round-trip),
//! transfer functions, halftone screens, black-generation / undercolour
//! removal, overprint settings, rendering intent, trim boxes, and ICC
//! output profiles (PDF/X-3 OutputIntent).
//!
//! Fonts are subsetted to the glyphs actually used on the page, with
//! per-font ToUnicode CMaps for text extraction and search.
//!
//! All seven PostScript shading types are translated to native PDF
//! shading objects (axial, radial, Gouraud mesh, Coons/tensor patch).
//!
//! Most users should use
//! [`stet::Interpreter::render_to_pdf`](https://docs.rs/stet) from the
//! [`stet`](https://crates.io/crates/stet) facade crate. Use
//! [`PdfDevice`] directly when you want to wire the PDF writer into a
//! custom rendering pipeline or capture the PDF bytes in-memory without
//! touching the filesystem.

mod content_stream;
mod font_embedder;
mod font_tracker;
mod image_ops;
mod pdf_device;
mod pdf_objects;
mod pdf_writer;
mod shading_ops;
mod text_ops;
mod unicode_mapping;

pub use pdf_device::PdfDevice;
