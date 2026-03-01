// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! egui viewer application — displays rendered PostScript pages.

use std::sync::mpsc::{Receiver, Sender};

use egui::{ColorImage, TextureHandle, TextureOptions, Vec2};

use crate::{PageImage, ViewerEnd};

/// Interactive viewer for rendered PostScript pages.
pub struct ViewerApp {
    page_receiver: Receiver<PageImage>,
    continue_sender: Sender<()>,
    /// All received pages (for back-navigation).
    pages: Vec<StoredPage>,
    /// Index into `pages` of the currently displayed page.
    current_page: usize,
    /// Zoom level (1.0 = fit to window).
    zoom: f32,
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
    pub fn new(viewer_end: ViewerEnd) -> Self {
        Self {
            page_receiver: viewer_end.page_receiver,
            continue_sender: viewer_end.continue_sender,
            pages: Vec::new(),
            current_page: 0,
            zoom: 1.0,
            pan_offset: Vec2::ZERO,
            dragging: false,
            last_drag_pos: None,
            interpreter_done: false,
            continue_sent: false,
        }
    }

    /// Check for newly arrived pages (non-blocking).
    fn poll_pages(&mut self, ctx: &egui::Context) {
        use std::sync::mpsc::TryRecvError;
        let had_pages = !self.pages.is_empty();
        loop {
            match self.page_receiver.try_recv() {
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
            // First page arrived — show it
            self.current_page = 0;
            self.reset_view();
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
            if self.continue_sender.send(()).is_err() {
                self.interpreter_done = true;
            }
            self.continue_sent = true;
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
        // Poll for new pages
        self.poll_pages(ctx);

        // Handle keyboard input
        ctx.input(|i| {
            // Quit
            if i.key_pressed(egui::Key::Q) || i.key_pressed(egui::Key::Escape) {
                // Signal interpreter to unblock and let it exit
                let _ = self.continue_sender.send(());
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }

            // Zoom
            if i.key_pressed(egui::Key::Equals) || i.key_pressed(egui::Key::Plus) {
                self.zoom = (self.zoom * 1.25).min(10.0);
            }
            if i.key_pressed(egui::Key::Minus) {
                self.zoom = (self.zoom / 1.25).max(0.1);
            }
            if i.key_pressed(egui::Key::Num0) {
                self.reset_view();
            }

            // Mouse wheel zoom (zoom toward cursor)
            let scroll = i.smooth_scroll_delta.y;
            if scroll != 0.0 {
                let factor = if scroll > 0.0 { 1.1 } else { 1.0 / 1.1 };
                let new_zoom = (self.zoom * factor).clamp(0.1, 10.0);
                if let Some(pos) = i.pointer.latest_pos() {
                    // Zoom toward cursor position
                    let center = pos.to_vec2();
                    self.pan_offset = center - (center - self.pan_offset) * (new_zoom / self.zoom);
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

        // Status bar at the bottom
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

        // Main content area
        egui::CentralPanel::default().show(ctx, |ui| {
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
