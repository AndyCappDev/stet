// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! tiny-skia rendering backend for the stet PostScript interpreter.

mod png_sink;
mod skia_device;

pub use png_sink::PngSinkFactory;
pub use skia_device::SkiaDevice;
pub use skia_device::render_region;
pub use skia_device::PreparedDisplayList;
pub use skia_device::prepare_display_list;
pub use skia_device::render_region_prepared;
