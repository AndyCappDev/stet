// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! egui viewer application — displays rendered PostScript pages.

use std::sync::mpsc::{Receiver, Sender, SyncSender};

use egui::{ColorImage, TextureHandle, TextureOptions, Vec2};

use crate::{PageImage, ScreenInfo, ViewerEnd};

/// Interactive viewer for rendered PostScript pages.
pub struct ViewerApp {
    page_receiver: Option<Receiver<PageImage>>,
    continue_sender: Option<Sender<()>>,
    screen_info_sender: Option<SyncSender<ScreenInfo>>,
    dpi_override: Option<f64>,
    /// All received pages (for back-navigation).
    pages: Vec<StoredPage>,
    /// Index into `pages` of the currently displayed page.
    current_page: usize,
    /// Zoom level (1.0 = fit to window).
    zoom: f32,
    /// Maximum zoom (where effective_scale = 1.0, i.e., native DPI).
    max_zoom: f32,
    /// Pan offset in screen pixels.
    pan_offset: Vec2,
    /// Whether the user is currently dragging to pan.
    dragging: bool,
    /// Last drag position.
    last_drag_pos: Option<egui::Pos2>,
    /// Whether the interpreter has finished sending pages.
    interpreter_done: bool,
    /// Whether we've sent a continue signal for the current page
    /// (to avoid double-sending).
    continue_sent: bool,
    /// Whether we've already sent screen info to the interpreter.
    screen_info_sent: bool,
    /// Whether the window has been resized to match the first page.
    window_sized: bool,
    /// Pending window size for centering (set by size_window_to_page).
    pending_center: Option<Vec2>,
    /// Set by Q/Escape handler; processed at the top of next update().
    quit_requested: bool,
}

/// A page image with its egui texture.
struct StoredPage {
    width: u32,
    height: u32,
    texture: Option<TextureHandle>,
    rgba_data: Vec<u8>,
    page_num: u32,
}

impl ViewerApp {
    pub fn new(viewer_end: ViewerEnd, dpi_override: Option<f64>) -> Self {
        Self {
            page_receiver: Some(viewer_end.page_receiver),
            continue_sender: Some(viewer_end.continue_sender),
            screen_info_sender: Some(viewer_end.screen_info_sender),
            dpi_override,
            pages: Vec::new(),
            current_page: 0,
            zoom: 1.0,
            max_zoom: 1.0,
            pan_offset: Vec2::ZERO,
            dragging: false,
            last_drag_pos: None,
            interpreter_done: false,
            continue_sent: false,
            screen_info_sent: false,
            window_sized: false,
            pending_center: None,
            quit_requested: false,
        }
    }

    /// Send screen info to the interpreter for DPI calculation.
    ///
    /// If --dpi was specified, sends the override directly. Otherwise sends
    /// the available pixel height (monitor_h * 0.85) so the interpreter can
    /// calculate DPI based on the actual page height.
    fn send_screen_info(&mut self, ctx: &egui::Context) {
        if self.screen_info_sent {
            return;
        }

        let info = if let Some(override_dpi) = self.dpi_override {
            ScreenInfo::DpiOverride(override_dpi)
        } else {
            let monitor_size = ctx.input(|i| i.viewport().monitor_size);
            let Some(monitor) = monitor_size else {
                return; // try again next frame
            };

            let ppp = ctx.input(|i| {
                i.viewport().native_pixels_per_point.unwrap_or(1.0)
            }) as f64;
            let available_h = monitor.y as f64 * ppp * 0.85;
            ScreenInfo::AvailableHeight(available_h)
        };

        if let Some(sender) = self.screen_info_sender.take() {
            let _ = sender.send(info);
        }
        self.screen_info_sent = true;
    }

    /// Resize the window to fit the rendered image.
    ///
    /// For auto-DPI the image is already sized to ~85% of screen height, so
    /// the window matches 1:1.  For high --dpi overrides the image may be
    /// larger than the screen — cap the window at 85% of monitor dimensions
    /// and let fit_scale shrink the image to fit.
    fn size_window_to_page(&mut self, ctx: &egui::Context) {
        if self.window_sized || self.pages.is_empty() {
            return;
        }
        self.window_sized = true;

        let page = &self.pages[0];
        let img_w = page.width as f32;
        let img_h = page.height as f32;
        if img_w <= 0.0 || img_h <= 0.0 {
            return;
        }

        // Image dimensions are in physical pixels; convert to logical points.
        let ppp = ctx.input(|i| i.viewport().native_pixels_per_point.unwrap_or(1.0));
        let logical_w = img_w / ppp;
        let logical_h = img_h / ppp;

        // Status bar overhead so the central panel fits the image.
        let panel_overhead = ctx.screen_rect().height() - ctx.available_rect().height();

        // Cap window to 85% of monitor dimensions.
        let (max_w, max_h) = ctx.input(|i| {
            if let Some(monitor) = i.viewport().monitor_size {
                (monitor.x * 0.85, monitor.y * 0.85)
            } else {
                (1024.0, 768.0)
            }
        });

        // Cap the image content area to screen limits, then add overhead.
        let mut win_w = logical_w;
        let mut win_h = logical_h;
        if win_w > max_w || win_h > max_h {
            let scale = (max_w / win_w).min(max_h / win_h);
            win_w *= scale;
            win_h *= scale;
        }
        win_h += panel_overhead;

        let win_w = win_w.max(400.0);
        let win_h = win_h.max(300.0);

        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(win_w, win_h)));

        // Store the window size for deferred centering.
        self.pending_center = Some(Vec2::new(win_w, win_h));
    }

    /// Check for newly arrived pages (non-blocking).
    fn poll_pages(&mut self, ctx: &egui::Context) {
        use std::sync::mpsc::TryRecvError;
        let had_pages = !self.pages.is_empty();
        let Some(receiver) = &self.page_receiver else {
            return;
        };
        loop {
            match receiver.try_recv() {
                Ok(page) => {
                    self.pages.push(StoredPage {
                        width: page.width,
                        height: page.height,
                        texture: None,
                        rgba_data: page.rgba_data,
                        page_num: page.page_num,
                    });
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.interpreter_done = true;
                    break;
                }
            }
        }

        if !had_pages && !self.pages.is_empty() {
            // First page arrived — show it and size the window
            self.current_page = 0;
            self.reset_view();
            self.size_window_to_page(ctx);
        } else if self.continue_sent && self.current_page + 1 < self.pages.len() {
            // User requested next page and it has arrived — advance
            self.current_page += 1;
            self.reset_view();
            self.continue_sent = false;
        } else if self.continue_sent && self.interpreter_done {
            // User requested next page but interpreter is done — no more pages, quit
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// Reset zoom and pan to defaults.
    fn reset_view(&mut self) {
        self.zoom = 1.0;
        self.pan_offset = Vec2::ZERO;
    }

    /// Advance to the next page, signaling the interpreter if needed.
    fn next_page(&mut self) {
        if self.current_page + 1 < self.pages.len() {
            // Already have the next page cached
            self.current_page += 1;
            self.reset_view();
        } else if !self.interpreter_done && !self.continue_sent {
            // Signal interpreter to render the next page
            let send_ok = self
                .continue_sender
                .as_ref()
                .map_or(false, |s| s.send(()).is_ok());
            if !send_ok {
                self.interpreter_done = true;
            }
            self.continue_sent = true;
        } else if self.interpreter_done {
            // No more pages — quit
            self.continue_sent = true; // triggers close in poll_pages
        }
    }

    /// Go to the previous page.
    fn prev_page(&mut self) {
        if self.current_page > 0 {
            self.current_page -= 1;
            self.reset_view();
        }
    }

    /// Calculate the fit-to-window scale factor.
    fn fit_scale(&self, available: Vec2, page: &StoredPage) -> f32 {
        let img_w = page.width as f32;
        let img_h = page.height as f32;
        if img_w <= 0.0 || img_h <= 0.0 {
            return 1.0;
        }
        let sx = available.x / img_w;
        let sy = available.y / img_h;
        sx.min(sy)
    }

    /// Ensure a page's texture is loaded.
    fn ensure_texture(&mut self, ctx: &egui::Context, page_idx: usize) {
        if self.pages[page_idx].texture.is_some() {
            return;
        }
        let page = &self.pages[page_idx];
        let image = ColorImage::from_rgba_unmultiplied(
            [page.width as usize, page.height as usize],
            &page.rgba_data,
        );
        let texture = ctx.load_texture(
            format!("page_{}", page.page_num),
            image,
            TextureOptions::LINEAR,
        );
        self.pages[page_idx].texture = Some(texture);
    }
}

impl eframe::App for ViewerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Handle quit: drop channels to unblock the interpreter, then close.
        // Done here (not inside ctx.input()) so the close command is processed
        // at the top level of update() rather than inside a locked input callback.
        if self.quit_requested {
            // Drop channels — this unblocks the interpreter wherever it's waiting
            // (page_sender.send() gets Err, continue_receiver.recv() gets Err).
            self.page_receiver = None;
            self.continue_sender = None;
            self.screen_info_sender = None;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        // Status bar — rendered first so send_dpi can measure the actual panel
        // overhead via ctx.available_rect() instead of using a hardcoded constant.
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if self.pages.is_empty() {
                    ui.label("Waiting for page...");
                } else {
                    let total = if self.interpreter_done {
                        format!("{}", self.pages.len())
                    } else {
                        format!("{}+", self.pages.len())
                    };
                    ui.label(format!(
                        "Page {} of {} | Zoom: {:.0}%",
                        self.current_page + 1,
                        total,
                        self.zoom * 100.0,
                    ));
                    ui.separator();
                    ui.label("Space/Right: next | Left: prev | +/-: zoom | 0: fit | Q: quit");
                }
            });
        });

        // Send screen info to interpreter (deferred until monitor size is available)
        self.send_screen_info(ctx);

        // Poll for new pages
        self.poll_pages(ctx);

        // Center the window (deferred from size_window_to_page so the resize
        // has taken effect and outer_rect reflects actual window dimensions).
        // On X11, center window after resize (Wayland ignores this but
        // centers the initial window automatically via the compositor).
        if let Some(win_size) = self.pending_center.take()
            && let Some(monitor) = ctx.input(|i| i.viewport().monitor_size)
        {
            let pos_x = (monitor.x - win_size.x) / 2.0;
            let pos_y = (monitor.y - win_size.y) / 2.0;
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
                pos_x.max(0.0),
                pos_y.max(0.0),
            )));
        }

        // Handle keyboard input
        ctx.input(|i| {
            // Quit
            if i.key_pressed(egui::Key::Q) || i.key_pressed(egui::Key::Escape) {
                self.quit_requested = true;
                return;
            }

            // Zoom (capped at native DPI: effective_scale = fit * zoom <= 1.0)
            let max_z = self.max_zoom;
            if i.key_pressed(egui::Key::Equals) || i.key_pressed(egui::Key::Plus) {
                self.zoom = (self.zoom * 1.25).min(max_z);
            }
            if i.key_pressed(egui::Key::Minus) {
                self.zoom = (self.zoom / 1.25).max(0.1);
            }
            if i.key_pressed(egui::Key::Num0) {
                self.reset_view();
            }

            // Mouse wheel zoom (zoom toward cursor).
            // egui's smooth_scroll_delta: ±40 pts per mouse wheel notch (native
            // line_scroll_speed=40), continuous small values for trackpads.
            // Map 40 pts → 1.25× to match keyboard +/- step size.
            let scroll = i.smooth_scroll_delta.y;
            if scroll.abs() > 0.5 {
                let factor = 1.25_f32.powf(scroll / 40.0);
                let new_zoom = (self.zoom * factor).clamp(0.1, max_z);
                if let Some(pos) = i.pointer.latest_pos() {
                    // Zoom toward cursor position
                    let center = pos.to_vec2();
                    self.pan_offset =
                        center - (center - self.pan_offset) * (new_zoom / self.zoom);
                }
                self.zoom = new_zoom;
            }

            // Page navigation
            if i.key_pressed(egui::Key::Space)
                || i.key_pressed(egui::Key::ArrowRight)
                || i.key_pressed(egui::Key::Enter)
            {
                self.next_page();
            }
            if i.key_pressed(egui::Key::ArrowLeft) || i.key_pressed(egui::Key::Backspace) {
                self.prev_page();
            }

            // Pan with arrow keys (only when zoomed)
            if self.zoom > 1.01 {
                if i.key_pressed(egui::Key::ArrowUp) {
                    self.pan_offset.y += 50.0;
                }
                if i.key_pressed(egui::Key::ArrowDown) {
                    self.pan_offset.y -= 50.0;
                }
            }
        });

        // Handle mouse drag for panning
        ctx.input(|i| {
            if i.pointer.primary_pressed() {
                self.dragging = true;
                self.last_drag_pos = i.pointer.latest_pos();
            }
            if i.pointer.primary_released() {
                self.dragging = false;
                self.last_drag_pos = None;
            }
            if self.dragging
                && let Some(current_pos) = i.pointer.latest_pos()
            {
                if let Some(last_pos) = self.last_drag_pos {
                    let delta = current_pos - last_pos;
                    self.pan_offset += delta;
                }
                self.last_drag_pos = Some(current_pos);
            }

            // Double-click to reset
            if i.pointer
                .button_double_clicked(egui::PointerButton::Primary)
            {
                self.reset_view();
            }
        });

        // Main content area (no inner margins — image centering is handled manually)
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(ctx.style().visuals.panel_fill))
            .show(ctx, |ui| {
            if self.pages.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.label("Waiting for first page to render...");
                });
                return;
            }

            // Ensure texture is loaded for current page
            let page_idx = self.current_page;
            self.ensure_texture(ctx, page_idx);

            let page = &self.pages[page_idx];
            if let Some(ref texture) = page.texture {
                let available = ui.available_size();
                let fit = self.fit_scale(available, page);

                // Update max zoom: effective_scale = fit * zoom <= 1.0
                self.max_zoom = if fit > 0.0 { (1.0 / fit).max(1.0) } else { 1.0 };
                self.zoom = self.zoom.min(self.max_zoom);

                let effective_scale = fit * self.zoom;

                let img_size = Vec2::new(
                    page.width as f32 * effective_scale,
                    page.height as f32 * effective_scale,
                );

                // Center the image when it fits in the window
                let center_offset = Vec2::new(
                    ((available.x - img_size.x) / 2.0).max(0.0),
                    ((available.y - img_size.y) / 2.0).max(0.0),
                );

                let offset = center_offset + self.pan_offset;
                let rect = egui::Rect::from_min_size(ui.min_rect().min + offset, img_size);

                // Checkerboard background (shows through transparent areas)
                ui.painter()
                    .rect_filled(rect, 0.0, egui::Color32::from_gray(200));

                // Draw the page image
                ui.painter().image(
                    texture.id(),
                    rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
        });

        // Request periodic repaints to check for new pages
        if !self.interpreter_done {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}
