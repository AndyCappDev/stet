// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Page sink for WASM builds.
//!
//! Interpretation records page dimensions only — pixels are re-rendered on
//! demand from the retained display lists by `render_viewport()`. Keeping a
//! full page would OOM the browser at high DPI: a 139-page document at
//! 300 DPI accumulates roughly 4.6 GB of RGBA that nothing ever reads.

use std::sync::{Arc, Mutex};

use stet_graphics::device::{PageSink, PageSinkFactory};

/// Rendered page data: the dimensions of one interpreted page.
pub struct PageData {
    pub width: u32,
    pub height: u32,
}

/// Lightweight sink that records page dimensions but discards all pixel data.
///
/// Used during PostScript interpretation in the WASM viewport workflow where
/// only display lists and page dimensions are needed — the actual rendering
/// happens on demand via `render_viewport()`. This avoids accumulating ~33 MB
/// of RGBA data per page, which would OOM on large documents (e.g. 139 pages
/// at 300 DPI = ~4.6 GB).
pub struct NullSink {
    pages: Arc<Mutex<Vec<PageData>>>,
    current_width: u32,
    current_height: u32,
}

impl PageSink for NullSink {
    fn begin_page(&mut self, width: u32, height: u32) -> Result<(), String> {
        self.current_width = width;
        self.current_height = height;
        Ok(())
    }

    fn write_rows(&mut self, _rgba_rows: &[u8], _num_rows: u32) -> Result<(), String> {
        Ok(())
    }

    fn end_page(&mut self) -> Result<(), String> {
        let page = PageData {
            width: self.current_width,
            height: self.current_height,
        };
        self.pages
            .lock()
            .map_err(|e| e.to_string())?
            .push(page);
        Ok(())
    }
}

/// Factory that creates `NullSink` instances sharing a page collection.
pub struct NullSinkFactory {
    pages: Arc<Mutex<Vec<PageData>>>,
}

impl NullSinkFactory {
    pub fn new() -> (Self, Arc<Mutex<Vec<PageData>>>) {
        let pages = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                pages: Arc::clone(&pages),
            },
            pages,
        )
    }

    /// Create a factory that shares the same page collection as an existing one.
    pub fn from_shared(pages: Arc<Mutex<Vec<PageData>>>) -> Self {
        Self { pages }
    }
}

impl PageSinkFactory for NullSinkFactory {
    fn create_sink(&self, _output_path: &str) -> Result<Box<dyn PageSink>, String> {
        Ok(Box::new(NullSink {
            pages: Arc::clone(&self.pages),
            current_width: 0,
            current_height: 0,
        }))
    }
}
