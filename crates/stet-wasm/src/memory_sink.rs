// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Page-size recording for WASM builds.
//!
//! Interpretation records page dimensions only — pixels are re-rendered on
//! demand from the retained display lists by `render_viewport()`. Keeping a
//! full page would OOM the browser at high DPI: a 139-page document at
//! 300 DPI accumulates roughly 4.6 GB of RGBA that nothing ever reads.

use std::sync::{Arc, Mutex};

use stet_core::device::OutputDevice;
use stet_fonts::geometry::PsPath;
use stet_graphics::device::{ClipParams, FillParams, ImageParams, StrokeParams};
use stet_graphics::display_list::DisplayList;

/// Rendered page data: the dimensions of one interpreted page.
pub struct PageData {
    pub width: u32,
    pub height: u32,
}

/// A new, empty collection of page sizes, to share between the devices a
/// job installs.
pub fn new_page_collection() -> Arc<Mutex<Vec<PageData>>> {
    Arc::new(Mutex::new(Vec::new()))
}

/// The device installed while interpreting: it records each page's size and
/// drops its display list, which the page-boundary capture has already kept
/// for `render_viewport()`. Rendering pages here as well — as a
/// `SkiaDevice` into a discarding sink once did — rasterised every page once
/// for nothing.
pub struct PageSizeRecorder {
    width: u32,
    height: u32,
    pages: Arc<Mutex<Vec<PageData>>>,
}

impl PageSizeRecorder {
    /// A device of `width` × `height` pixels recording into `pages`.
    pub fn new(width: u32, height: u32, pages: Arc<Mutex<Vec<PageData>>>) -> Self {
        Self {
            width,
            height,
            pages,
        }
    }
}

impl OutputDevice for PageSizeRecorder {
    fn fill_path(&mut self, _path: &PsPath, _params: &FillParams) {}
    fn stroke_path(&mut self, _path: &PsPath, _params: &StrokeParams) {}
    fn clip_path(&mut self, _path: &PsPath, _params: &ClipParams) {}
    fn init_clip(&mut self) {}
    fn erase_page(&mut self) {}
    fn show_page(&mut self, _output_path: &str) -> Result<(), String> {
        Ok(())
    }
    fn draw_image(&mut self, _sample_data: &[u8], _params: &ImageParams) {}
    fn page_size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    fn replay_and_show(&mut self, _list: DisplayList, _output_path: &str) -> Result<(), String> {
        self.pages
            .lock()
            .map_err(|e| e.to_string())?
            .push(PageData {
                width: self.width,
                height: self.height,
            });
        Ok(())
    }
}
