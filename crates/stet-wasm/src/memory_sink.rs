// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! In-memory page sink for WASM builds.
//!
//! Collects rendered RGBA pixels into a `Vec<PageData>` instead of writing
//! to files or displaying in a window.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use stet_core::device::{PageSink, PageSinkFactory};

fn log(msg: &str) {
    web_sys::console::log_1(&wasm_bindgen::JsValue::from_str(msg));
}

/// Callback invoked when a page finishes rendering (index, width, height, rgba).
type PageCallback = Box<dyn Fn(u32, u32, u32, &[u8])>;

thread_local! {
    static ON_PAGE_READY: RefCell<Option<PageCallback>> = RefCell::new(None);
}

/// Set (or clear) the callback invoked after each page is rendered.
pub fn set_page_ready_callback(cb: Option<PageCallback>) {
    ON_PAGE_READY.with(|cell| {
        *cell.borrow_mut() = cb;
    });
}

/// Rendered page data: dimensions and RGBA pixel buffer.
pub struct PageData {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// In-memory page sink that accumulates RGBA pixel data.
pub struct MemorySink {
    pages: Arc<Mutex<Vec<PageData>>>,
    current_width: u32,
    current_height: u32,
    current_page: Vec<u8>,
}

impl PageSink for MemorySink {
    fn begin_page(&mut self, width: u32, height: u32) -> Result<(), String> {
        log(&format!("MemorySink::begin_page({}x{})", width, height));
        self.current_width = width;
        self.current_height = height;
        self.current_page.clear();
        self.current_page
            .reserve(width as usize * height as usize * 4);
        Ok(())
    }

    fn write_rows(&mut self, rgba_rows: &[u8], _num_rows: u32) -> Result<(), String> {
        self.current_page.extend_from_slice(rgba_rows);
        Ok(())
    }

    fn end_page(&mut self) -> Result<(), String> {
        let index = self.pages.lock().map(|g| g.len() as u32).unwrap_or(0);

        // Notify JS callback (if set) so it can display the page immediately
        ON_PAGE_READY.with(|cell| {
            if let Some(ref cb) = *cell.borrow() {
                cb(index, self.current_width, self.current_height, &self.current_page);
            }
        });

        let page = PageData {
            width: self.current_width,
            height: self.current_height,
            rgba: std::mem::take(&mut self.current_page),
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
        log("MemorySinkFactory::create_sink() called");
        Ok(Box::new(MemorySink {
            pages: Arc::clone(&self.pages),
            current_width: 0,
            current_height: 0,
            current_page: Vec::new(),
        }))
    }
}
