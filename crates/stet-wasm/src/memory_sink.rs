// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! In-memory page sink for WASM builds.
//!
//! When a streaming callback is registered, rendered bands are forwarded
//! directly to JS without accumulating the full page in WASM memory.
//! This is critical at high DPI where a single page can exceed 2 GB.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use stet_core::device::{PageSink, PageSinkFactory};

/// Streaming callback events:
///   event=0 (begin): index, width, height, data=empty
///   event=1 (rows):  0, 0, 0, data=rgba_rows
///   event=2 (end):   index, 0, 0, data=empty
type SinkCallback = Box<dyn Fn(u32, u32, u32, u32, &[u8])>;

thread_local! {
    static SINK_CALLBACK: RefCell<Option<SinkCallback>> = RefCell::new(None);
}

/// Set (or clear) the streaming callback.
pub fn set_sink_callback(cb: Option<SinkCallback>) {
    SINK_CALLBACK.with(|cell| {
        *cell.borrow_mut() = cb;
    });
}

/// Rendered page data: dimensions (and optionally RGBA pixel buffer).
pub struct PageData {
    pub width: u32,
    pub height: u32,
    /// RGBA pixels — populated in non-streaming mode, empty in streaming mode.
    /// May be unused when only page dimensions are needed (viewport rendering).
    #[allow(dead_code)]
    pub rgba: Vec<u8>,
}

/// In-memory page sink that accumulates RGBA pixel data.
///
/// When a streaming callback is active, bands are forwarded directly
/// and no RGBA data is accumulated locally.
pub struct MemorySink {
    pages: Arc<Mutex<Vec<PageData>>>,
    current_width: u32,
    current_height: u32,
    current_page: Vec<u8>,
    streaming: bool,
}

impl PageSink for MemorySink {
    fn begin_page(&mut self, width: u32, height: u32) -> Result<(), String> {
        self.current_width = width;
        self.current_height = height;

        // Check if streaming callback is active
        self.streaming = SINK_CALLBACK.with(|cell| cell.borrow().is_some());

        if self.streaming {
            let index = self.pages.lock().map(|g| g.len() as u32).unwrap_or(0);
            SINK_CALLBACK.with(|cell| {
                if let Some(ref cb) = *cell.borrow() {
                    cb(0, index, width, height, &[]);
                }
            });
        } else {
            self.current_page.clear();
            self.current_page
                .reserve(width as usize * height as usize * 4);
        }
        Ok(())
    }

    fn write_rows(&mut self, rgba_rows: &[u8], _num_rows: u32) -> Result<(), String> {
        if self.streaming {
            SINK_CALLBACK.with(|cell| {
                if let Some(ref cb) = *cell.borrow() {
                    cb(1, 0, 0, 0, rgba_rows);
                }
            });
        } else {
            self.current_page.extend_from_slice(rgba_rows);
        }
        Ok(())
    }

    fn end_page(&mut self) -> Result<(), String> {
        let index = self.pages.lock().map(|g| g.len() as u32).unwrap_or(0);

        if self.streaming {
            SINK_CALLBACK.with(|cell| {
                if let Some(ref cb) = *cell.borrow() {
                    cb(2, index, 0, 0, &[]);
                }
            });
        }

        let rgba = if self.streaming {
            Vec::new()
        } else {
            std::mem::take(&mut self.current_page)
        };

        let page = PageData {
            width: self.current_width,
            height: self.current_height,
            rgba,
        };
        self.pages
            .lock()
            .map_err(|e| e.to_string())?
            .push(page);
        Ok(())
    }
}

/// Factory that creates `MemorySink` instances sharing a page collection.
pub struct MemorySinkFactory {
    pages: Arc<Mutex<Vec<PageData>>>,
}

impl MemorySinkFactory {
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

impl PageSinkFactory for MemorySinkFactory {
    fn create_sink(&self, _output_path: &str) -> Result<Box<dyn PageSink>, String> {
        Ok(Box::new(MemorySink {
            pages: Arc::clone(&self.pages),
            current_width: 0,
            current_height: 0,
            current_page: Vec::new(),
            streaming: false,
        }))
    }
}
