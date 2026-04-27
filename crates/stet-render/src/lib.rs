// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `stet-tiny-skia`-based rasterizer for stet display lists.
//!
//! This crate rasterizes any
//! [`DisplayList`](stet_graphics::display_list::DisplayList) to RGBA pixels
//! — whether the list came from the PostScript interpreter
//! ([`stet-core`](https://crates.io/crates/stet-core)) or the PDF reader
//! ([`stet-pdf-reader`](https://crates.io/crates/stet-pdf-reader)) makes
//! no difference. Rendering is multi-threaded and banded (sized to fit
//! L2), with mask caching, clip fast paths, and ICC-aware CMYK handling.
//!
//! # Two ways to render
//!
//! - [`render_to_rgba`] — one-shot: a `DisplayList` in, a `Vec<u8>` of
//!   RGBA out. Banded page rasterizer with automatic parallelism.
//! - [`prepare_display_list`] + [`render_region_prepared`] — viewport
//!   rendering: pre-compute bounding boxes once, then render any
//!   rectangular region at any zoom without reinterpreting or
//!   reallocating. Used by the interactive viewer and the WASM frontend.
//!
//! # Sinks
//!
//! [`PngSinkFactory`] streams banded output straight to a PNG file.
//! Implement [`stet_graphics::device::PageSink`] /
//! [`PageSinkFactory`](stet_graphics::device::PageSinkFactory) yourself
//! for other streaming destinations.
//!
//! Most users should use the [`stet`](https://crates.io/crates/stet)
//! facade crate rather than depending on `stet-render` directly.

mod png_sink;
mod skia_device;

pub use png_sink::PngSinkFactory;
pub use skia_device::ImageCache;
pub use skia_device::PreparedDisplayList;
pub use skia_device::SkiaDevice;
pub use skia_device::build_icc_cache_for_list;
pub use skia_device::debug_bbox_comparison;
pub use skia_device::prepare_display_list;
pub use skia_device::render_region;
pub use skia_device::render_region_prepared;
pub use skia_device::render_region_prepared_parallel;
pub use skia_device::render_region_prepared_parallel_cancellable;
pub use skia_device::render_region_prepared_parallel_with_progress;
pub use skia_device::render_region_single_band;
pub use skia_device::render_to_rgba;
pub use skia_device::render_to_rgba_viewport;
pub use skia_device::render_to_rgba_with_layers;
pub use skia_device::viewport_band_count;
