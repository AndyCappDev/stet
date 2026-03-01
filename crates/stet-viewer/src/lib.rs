// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Interactive egui viewer for the stet PostScript interpreter.
//!
//! Displays rendered pages in a window with zoom, pan, and page navigation.
//! The interpreter sends rendered pages via channels; the viewer blocks the
//! interpreter at each showpage until the user advances.

mod sink;
mod viewer;

use std::sync::mpsc;

pub use sink::ViewerSinkFactory;

/// A rendered page image ready for display.
pub struct PageImage {
    pub width: u32,
    pub height: u32,
    pub rgba_data: Vec<u8>,
    pub page_num: u32,
}

/// Interpreter-side channel endpoints.
pub struct InterpreterEnd {
    pub page_sender: mpsc::SyncSender<PageImage>,
    pub continue_receiver: mpsc::Receiver<()>,
}

/// Viewer-side channel endpoints.
pub struct ViewerEnd {
    pub page_receiver: mpsc::Receiver<PageImage>,
    pub continue_sender: mpsc::Sender<()>,
}

/// Create matched channel pairs for interpreter ↔ viewer communication.
///
/// The page channel is bounded (capacity 1) to provide backpressure —
/// the interpreter blocks if it gets more than 1 page ahead of the viewer.
pub fn create_channels() -> (InterpreterEnd, ViewerEnd) {
    let (page_tx, page_rx) = mpsc::sync_channel(1);
    let (cont_tx, cont_rx) = mpsc::channel();

    (
        InterpreterEnd {
            page_sender: page_tx,
            continue_receiver: cont_rx,
        },
        ViewerEnd {
            page_receiver: page_rx,
            continue_sender: cont_tx,
        },
    )
}

/// Run the viewer window on the current thread (must be main thread).
///
/// This function blocks until the viewer window is closed.
pub fn run_viewer(viewer_end: ViewerEnd) {
    let app = viewer::ViewerApp::new(viewer_end);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("stet")
            .with_inner_size([1024.0, 768.0]),
        ..Default::default()
    };
    eframe::run_native("stet", options, Box::new(|_cc| Ok(Box::new(app))))
        .expect("Failed to start viewer");
}
