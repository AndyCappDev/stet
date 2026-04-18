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
/// (DisplayList, dpi, page_width, page_height, effective CMYK profile bytes).
///
/// The 5th element carries the CMYK ICC profile that was *effectively* used
/// to build the display list (e.g. a PDF's OutputIntent when
/// `--use-output-intent` is active). The viewer uses these bytes to build its
/// render-time ICC cache so runtime overprint math stays consistent with the
/// baked RGB values in the display list. `None` means "use the CLI-level
/// default" (typically the system CMYK profile).
pub type DisplayListMsg = (
    DisplayList,
    f64,
    u32,
    u32,
    Option<std::sync::Arc<Vec<u8>>>,
);

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
    /// CMYK ICC profile bytes that were used when building this page's display
    /// list, when different from the CLI-level default. The viewer uses these
    /// per-page bytes so overprint math at render time matches the baked RGB.
    pub cmyk_bytes: Option<std::sync::Arc<Vec<u8>>>,
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
    /// Sends dropped file paths to the interpreter for processing.
    pub file_drop_sender: mpsc::Sender<String>,
}

/// Create matched channel pairs for interpreter <-> viewer communication.
///
/// Returns `(InterpreterEnd, ViewerEnd, dl_sender, advance_receiver, file_drop_receiver)`.
/// - `dl_sender` should be set on `Context.display_list_sender`.
/// - `advance_receiver` is used by the interpreter to wait between jobs.
/// - `file_drop_receiver` receives file paths dropped onto the viewer window.
pub fn create_channels() -> (
    InterpreterEnd,
    ViewerEnd,
    mpsc::Sender<DisplayListMsg>,
    mpsc::Receiver<()>,
    mpsc::Receiver<String>,
) {
    // Display list pipe: unbounded (interpreter never blocks at showpage)
    let (dl_tx, dl_rx) = mpsc::channel();
    // Page pipe: unbounded (display lists are lightweight metadata)
    let (page_tx, page_rx) = mpsc::channel();
    // Screen info: bounded (single message)
    let (info_tx, info_rx) = mpsc::sync_channel(1);
    // Job advance: bounded (interpreter blocks until viewer signals)
    let (advance_tx, advance_rx) = mpsc::sync_channel(0);
    // File drop: unbounded (viewer sends dropped file paths to interpreter)
    let (file_drop_tx, file_drop_rx) = mpsc::channel();

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
            file_drop_sender: file_drop_tx,
        },
        dl_tx,
        advance_rx,
        file_drop_rx,
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
    run_viewer_inner(
        viewer_end,
        dpi_override,
        filename,
        page_size,
        system_cmyk_bytes,
        no_aa,
    )
}

/// Inner implementation of `run_viewer`.
fn run_viewer_inner(
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
    // Estimate status bar at ~32 logical pixels; content area fills 85% of
    // monitor height minus that overhead.
    let (page_w, page_h) = page_size.unwrap_or((DEFAULT_PAGE_W, DEFAULT_PAGE_H));
    let aspect = page_w / page_h;
    let status_bar_est = 32.0_f32;
    let est_mon_h = 1440.0_f32;
    let est_mon_w = 2560.0_f32;
    let max_content_h = est_mon_h * 0.85 - status_bar_est;
    let max_content_w = est_mon_w * 0.85;
    let mut content_h = max_content_h;
    let mut content_w = content_h * aspect as f32;
    if content_w > max_content_w {
        content_w = max_content_w;
        content_h = content_w / aspect as f32;
    }
    let init_w = content_w;
    let init_h = content_h + status_bar_est;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(&title)
            .with_inner_size([init_w, init_h])
            .with_drag_and_drop(true),
        centered: true,
        persist_window: false,
        ..Default::default()
    };
    eframe::run_native("stet", options, Box::new(|_cc| Ok(Box::new(app))))
        .expect("Failed to start viewer");
}
