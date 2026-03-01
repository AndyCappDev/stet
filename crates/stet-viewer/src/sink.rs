// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Viewer page sink — sends rendered pages to the viewer via channels.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex};

use stet_core::device::{PageSink, PageSinkFactory};

use crate::PageImage;

/// Factory that creates `ViewerSink` instances for each page.
///
/// Cloneable so it can be moved into closures and device factories.
pub struct ViewerSinkFactory {
    page_sender: SyncSender<PageImage>,
    continue_receiver: Arc<Mutex<Receiver<()>>>,
    page_num: Arc<AtomicU32>,
}

impl ViewerSinkFactory {
    /// Create a new factory from the interpreter-side channel endpoints.
    pub fn new(interp_end: crate::InterpreterEnd) -> Self {
        Self {
            page_sender: interp_end.page_sender,
            continue_receiver: Arc::new(Mutex::new(interp_end.continue_receiver)),
            page_num: Arc::new(AtomicU32::new(1)),
        }
    }
}

impl Clone for ViewerSinkFactory {
    fn clone(&self) -> Self {
        Self {
            page_sender: self.page_sender.clone(),
            continue_receiver: self.continue_receiver.clone(),
            page_num: self.page_num.clone(),
        }
    }
}

impl PageSinkFactory for ViewerSinkFactory {
    fn create_sink(&self, _output_path: &str) -> Result<Box<dyn PageSink>, String> {
        Ok(Box::new(ViewerSink {
            page_sender: self.page_sender.clone(),
            continue_receiver: self.continue_receiver.clone(),
            page_num: self.page_num.fetch_add(1, Ordering::Relaxed),
            buffer: Vec::new(),
            width: 0,
            height: 0,
        }))
    }
}

/// Sends a rendered page to the viewer and blocks until the user advances.
struct ViewerSink {
    page_sender: SyncSender<PageImage>,
    continue_receiver: Arc<Mutex<Receiver<()>>>,
    page_num: u32,
    buffer: Vec<u8>,
    width: u32,
    height: u32,
}

impl PageSink for ViewerSink {
    fn begin_page(&mut self, width: u32, height: u32) -> Result<(), String> {
        self.width = width;
        self.height = height;
        self.buffer.clear();
        self.buffer.reserve((width * height * 4) as usize);
        Ok(())
    }

    fn write_rows(&mut self, rgba_rows: &[u8], _num_rows: u32) -> Result<(), String> {
        self.buffer.extend_from_slice(rgba_rows);
        Ok(())
    }

    fn end_page(&mut self) -> Result<(), String> {
        let image = PageImage {
            width: self.width,
            height: self.height,
            rgba_data: std::mem::take(&mut self.buffer),
            page_num: self.page_num,
        };

        // Send the page to the viewer
        self.page_sender
            .send(image)
            .map_err(|_| "Viewer closed".to_string())?;

        // Block until the viewer signals to continue (user pressed next page)
        let rx = self.continue_receiver.lock().unwrap();
        let _ = rx.recv(); // Returns Err if viewer closed, which is fine

        Ok(())
    }
}
