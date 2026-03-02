// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Interactive egui viewer for the stet PostScript interpreter.
//!
//! Displays rendered PostScript pages in a window with zoom, pan, and page
//! navigation. The interpreter sends display lists via channels; the viewer
//! renders visible viewport regions on demand via `render_region()`.

mod viewer;

use std::sync::mpsc;

use stet_core::display_list::DisplayList;

/// Raw display list tuple sent by Context at each showpage:
/// (DisplayList, dpi, page_width, page_height).
pub type DisplayListMsg = (DisplayList, f64, u32, u32);

/// A page ready for display, carrying its resolution-independent display list.
pub struct PageReady {
    pub display_list: DisplayList,
    pub width: u32,
    pub height: u32,
    pub dpi: f64,
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
    /// Receives raw display list tuples from Context's display_list_sender.
    pub dl_receiver: mpsc::Receiver<DisplayListMsg>,
    /// Sends wrapped PageReady to the viewer.
    pub page_sender: mpsc::Sender<PageReady>,
    /// Receives screen info from the viewer for DPI calculation.
    pub screen_info_receiver: mpsc::Receiver<ScreenInfo>,
}

/// Viewer-side channel endpoints.
pub struct ViewerEnd {
    pub page_receiver: mpsc::Receiver<PageReady>,
    /// Sends screen info to the interpreter.
    pub screen_info_sender: mpsc::SyncSender<ScreenInfo>,
}

/// Create matched channel pairs for interpreter <-> viewer communication.
///
/// Returns `(InterpreterEnd, ViewerEnd, dl_sender)` where `dl_sender` should
/// be set on `Context.display_list_sender` for incremental delivery.
pub fn create_channels() -> (InterpreterEnd, ViewerEnd, mpsc::Sender<DisplayListMsg>)
{
    // Display list pipe: unbounded (interpreter never blocks at showpage)
    let (dl_tx, dl_rx) = mpsc::channel();
    // Page pipe: unbounded (display lists are lightweight metadata)
    let (page_tx, page_rx) = mpsc::channel();
    // Screen info: bounded (single message)
    let (info_tx, info_rx) = mpsc::sync_channel(1);

    (
        InterpreterEnd {
            dl_receiver: dl_rx,
            page_sender: page_tx,
            screen_info_receiver: info_rx,
        },
        ViewerEnd {
            page_receiver: page_rx,
            screen_info_sender: info_tx,
        },
        dl_tx,
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
    // places the window correctly from the start.
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
