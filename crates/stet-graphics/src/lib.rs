// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Graphics foundation for the stet PostScript and PDF rendering stack:
//! colours, the display list, ICC profile management, and mesh / patch
//! shading parsers.
//!
//! Only depends on [`stet-fonts`](https://crates.io/crates/stet-fonts), so
//! it is usable on its own when any of these subsystems is what you
//! actually want:
//!
//! - [`icc`] — ICC colour management (CMYK↔sRGB, bulk image conversion,
//!   black-point compensation) via `moxcms`.
//! - [`mesh_shading`] — decoders for PDF Type 4/5 (Gouraud triangle mesh)
//!   and Type 6/7 (Coons / tensor patch) shading streams.
//! - [`display_list`] — the [`DisplayList`](display_list::DisplayList) /
//!   [`DisplayElement`](display_list::DisplayElement) types that both the
//!   PostScript interpreter and `stet-pdf-reader` emit into. Custom
//!   output devices, PDF rewriters, and diff tools consume these.
//! - [`color`] and [`device`] — colour types, line-style primitives,
//!   paint-parameter structs, and the [`PageSink`](device::PageSink) /
//!   [`PageSinkFactory`](device::PageSinkFactory) streaming traits.
//!
//! Most users should use the [`stet`](https://crates.io/crates/stet)
//! facade crate to render PostScript or PDF, and
//! [`stet-pdf-reader`](https://crates.io/crates/stet-pdf-reader) to parse
//! PDFs into `DisplayList`s.

pub mod color;
pub mod device;
pub mod display_list;
pub mod icc;
pub mod layer_set;
pub mod mesh_shading;
