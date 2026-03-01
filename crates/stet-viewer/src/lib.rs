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

/// Screen information sent from viewer to interpreter for DPI calculation.
pub enum ScreenInfo {
    /// User specified an explicit DPI override via --dpi.
    DpiOverride(f64),
    /// Available pixel height for rendering (monitor_h * 0.85, in physical pixels).
    /// The interpreter calculates DPI from this and the actual page height.
    AvailableHeight(f64),
}

/// Interpreter-side channel endpoints.
pub struct InterpreterEnd {
    pub page_sender: mpsc::SyncSender<PageImage>,
    pub continue_receiver: mpsc::Receiver<()>,
    /// Receives screen info from the viewer for DPI calculation.
    pub screen_info_receiver: mpsc::Receiver<ScreenInfo>,
}

/// Viewer-side channel endpoints.
pub struct ViewerEnd {
    pub page_receiver: mpsc::Receiver<PageImage>,
    pub continue_sender: mpsc::Sender<()>,
    /// Sends screen info to the interpreter.
    pub screen_info_sender: mpsc::SyncSender<ScreenInfo>,
}

/// Create matched channel pairs for interpreter ↔ viewer communication.
///
/// The page channel is bounded (capacity 1) to provide backpressure —
/// the interpreter blocks if it gets more than 1 page ahead of the viewer.
pub fn create_channels() -> (InterpreterEnd, ViewerEnd) {
    let (page_tx, page_rx) = mpsc::sync_channel(1);
    let (cont_tx, cont_rx) = mpsc::channel();
    let (info_tx, info_rx) = mpsc::sync_channel(1);

    (
        InterpreterEnd {
            page_sender: page_tx,
            continue_receiver: cont_rx,
            screen_info_receiver: info_rx,
        },
        ViewerEnd {
            page_receiver: page_rx,
            continue_sender: cont_tx,
            screen_info_sender: info_tx,
        },
    )
}

/// Default page dimensions in points (US Letter).
const DEFAULT_PAGE_W: f64 = 612.0;
const DEFAULT_PAGE_H: f64 = 792.0;

/// Run the viewer window on the current thread (must be main thread).
///
/// `dpi_override`: if `Some`, use this DPI instead of auto-calculating from
/// monitor size. The chosen DPI is sent to the interpreter via the channel.
///
/// This function blocks until the viewer window is closed.
pub fn run_viewer(viewer_end: ViewerEnd, dpi_override: Option<f64>, filename: Option<&str>) {
    let app = viewer::ViewerApp::new(viewer_end, dpi_override);

    let title = match filename {
        Some(name) => {
            let base = std::path::Path::new(name)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| name.to_string());
            format!("stet — {}", base)
        }
        None => "stet".to_string(),
    };

    // Estimate the initial window size so the compositor (especially Wayland)
    // places the window correctly from the start. We don't know the actual
    // monitor dimensions yet, so overestimate based on 1440p — if the window
    // is too large the compositor clamps it, and when the first page arrives
    // the resize only SHRINKS (stays within bounds). Underestimating causes
    // the resize to grow the window off-screen since Wayland compositors
    // keep the top-left fixed on resize.
    let est_win_h = 1440.0 * 0.85;
    let init_h = est_win_h as f32;
    let init_w = init_h * (DEFAULT_PAGE_W / DEFAULT_PAGE_H) as f32;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(&title)
            .with_inner_size([init_w, init_h]),
        centered: true,
        ..Default::default()
    };
    eframe::run_native("stet", options, Box::new(|_cc| Ok(Box::new(app))))
        .expect("Failed to start viewer");
}
