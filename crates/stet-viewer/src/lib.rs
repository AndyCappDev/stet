// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Interactive egui viewer for the stet PostScript interpreter.
//!
//! Displays rendered PostScript pages in a window with zoom, pan, and page
//! navigation. The interpreter sends display lists via channels; the viewer
//! renders visible viewport regions on demand via `render_region()`.

mod viewer;

use std::sync::mpsc;

use stet_graphics::display_list::DisplayList;

/// Raw display list tuple sent by Context at each showpage:
/// (DisplayList, dpi, page_width, page_height).
pub type DisplayListMsg = (DisplayList, f64, u32, u32);

/// Message from interpreter to viewer via the relay thread.
pub enum ViewerMsg {
    /// A page is ready for display.
    Page(PageReady),
    /// A new job is starting — clear accumulated pages.
    NewJob,
    /// Current job is finished — all pages for this job have been sent.
    JobDone,
}

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
    /// Sends wrapped ViewerMsg to the viewer.
    pub page_sender: mpsc::Sender<ViewerMsg>,
    /// Receives screen info from the viewer for DPI calculation.
    pub screen_info_receiver: mpsc::Receiver<ScreenInfo>,
}

/// Viewer-side channel endpoints.
pub struct ViewerEnd {
    pub page_receiver: mpsc::Receiver<ViewerMsg>,
    /// Sends screen info to the interpreter.
    pub screen_info_sender: mpsc::SyncSender<ScreenInfo>,
    /// Signals the interpreter to advance to the next job.
    pub advance_sender: mpsc::SyncSender<()>,
}

/// Create matched channel pairs for interpreter <-> viewer communication.
///
/// Returns `(InterpreterEnd, ViewerEnd, dl_sender, advance_receiver)`.
/// - `dl_sender` should be set on `Context.display_list_sender`.
/// - `advance_receiver` is used by the interpreter to wait between jobs.
pub fn create_channels() -> (
    InterpreterEnd,
    ViewerEnd,
    mpsc::Sender<DisplayListMsg>,
    mpsc::Receiver<()>,
) {
    // Display list pipe: unbounded (interpreter never blocks at showpage)
    let (dl_tx, dl_rx) = mpsc::channel();
    // Page pipe: unbounded (display lists are lightweight metadata)
    let (page_tx, page_rx) = mpsc::channel();
    // Screen info: bounded (single message)
    let (info_tx, info_rx) = mpsc::sync_channel(1);
    // Job advance: bounded (interpreter blocks until viewer signals)
    let (advance_tx, advance_rx) = mpsc::sync_channel(0);

    (
        InterpreterEnd {
            dl_receiver: dl_rx,
            page_sender: page_tx,
            screen_info_receiver: info_rx,
        },
        ViewerEnd {
            page_receiver: page_rx,
            screen_info_sender: info_tx,
            advance_sender: advance_tx,
        },
        dl_tx,
        advance_rx,
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
/// `page_size`: optional (width, height) in PostScript points for the first
/// page. Used to compute the initial window aspect ratio so the compositor
/// (especially Wayland, which ignores client-side repositioning) places the
/// window correctly from the start.
///
/// This function blocks until the viewer window is closed.
pub fn run_viewer(
    viewer_end: ViewerEnd,
    dpi_override: Option<f64>,
    filename: Option<&str>,
    page_size: Option<(f64, f64)>,
    system_cmyk_bytes: Option<std::sync::Arc<Vec<u8>>>,
    no_aa: bool,
) {
    let app = viewer::ViewerApp::new(viewer_end, dpi_override, system_cmyk_bytes, no_aa);

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

    // Compute initial window size from the first page's dimensions.
    // This ensures the compositor (especially Wayland) centers the window
    // at the correct aspect ratio — we cannot reposition after creation.
    let (page_w, page_h) = page_size.unwrap_or((DEFAULT_PAGE_W, DEFAULT_PAGE_H));
    let aspect = page_w / page_h;
    let est_win_h = 1440.0_f32 * 0.85;
    let mut init_h = est_win_h;
    let mut init_w = init_h * aspect as f32;
    let est_win_w = 2560.0_f32 * 0.85;
    if init_w > est_win_w {
        init_w = est_win_w;
        init_h = init_w / aspect as f32;
    }

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
