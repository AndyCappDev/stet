// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! PDF output device for stet — converts display lists to PDF files.

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
