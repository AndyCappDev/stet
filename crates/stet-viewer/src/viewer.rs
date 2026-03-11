// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! egui viewer application — renders PostScript pages on demand from display lists.

use std::sync::mpsc::{Receiver, SyncSender, TryRecvError};

use egui::{ColorImage, TextureHandle, TextureOptions, Vec2};
use stet_core::display_list::DisplayList;
use stet_render::PreparedDisplayList;

use crate::{ScreenInfo, ViewerEnd, ViewerMsg};

/// Interactive viewer for PostScript pages with viewport-based rendering.
pub struct ViewerApp {
    page_receiver: Option<Receiver<ViewerMsg>>,
    screen_info_sender: Option<SyncSender<ScreenInfo>>,
    advance_sender: Option<SyncSender<()>>,
    dpi_override: Option<f64>,
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
    job_done: bool,
    /// Whether we've already sent screen info to the interpreter.
    screen_info_sent: bool,
    /// Whether the window has been resized to match the first page.
    window_sized: bool,
    /// Deferred window position after resize (computed from old center point).
    pending_position: Option<egui::Pos2>,
    /// Set by Q/Escape handler; processed at the top of next update().
    quit_requested: bool,
    /// When a DPI preset is active, store its exact value for display
    /// (avoids f32 round-trip precision loss in zoom * fit * ref_dpi).
    dpi_preset: Option<f64>,
    /// Viewport rendering needs refresh (zoom/pan/page changed).
    render_dirty: bool,
    /// Last available size used for rendering (detects window resize).
    last_available: Vec2,
    /// Central panel available size (for status bar DPI display).
    central_available: Vec2,
    /// Minimap state (cached thumbnail for current page).
    minimap: Option<MinimapState>,
    /// Whether the user is dragging the minimap viewport rectangle.
    minimap_dragging: bool,
    /// System CMYK ICC profile bytes (for ICC-aware rendering).
    system_cmyk_bytes: Option<std::sync::Arc<Vec<u8>>>,
}

/// A page stored as a resolution-independent display list.
struct StoredPage {
    display_list: DisplayList,
    /// Precomputed bboxes/epochs for fast viewport rendering.
    prepared: PreparedDisplayList,
    /// Device pixel dimensions at reference DPI.
    width: u32,
    height: u32,
    /// Reference DPI from the interpreter.
    dpi: f64,
    page_num: u32,
    /// Cached viewport render (reused if viewport unchanged).
    cached_render: Option<CachedRender>,
    /// ICC cache built from this page's display list profiles.
    icc_cache: stet_core::icc::IccCache,
}

/// Cached result of a viewport render.
struct CachedRender {
    texture: TextureHandle,
    vp_x: f64,
    vp_y: f64,
    vp_w: f64,
    vp_h: f64,
    pixel_w: u32,
    pixel_h: u32,
}

/// Minimap thumbnail state.
struct MinimapState {
    texture: TextureHandle,
    page_index: usize,
}

/// DPI preset values accessible via number keys.
const DPI_PRESETS: [(egui::Key, f64, &str); 7] = [
    (egui::Key::Num1, 150.0, "150"),
    (egui::Key::Num2, 300.0, "300"),
    (egui::Key::Num3, 600.0, "600"),
    (egui::Key::Num4, 1200.0, "1200"),
    (egui::Key::Num5, 2400.0, "2400"),
    (egui::Key::Num6, 4800.0, "4800"),
    (egui::Key::Num7, 9600.0, "9600"),
];

/// Minimap dimensions (logical pixels).
const MINIMAP_MAX_W: f32 = 160.0;
const MINIMAP_MAX_H: f32 = 200.0;
const MINIMAP_MARGIN: f32 = 12.0;

impl ViewerApp {
    pub fn new(
        viewer_end: ViewerEnd,
        dpi_override: Option<f64>,
        system_cmyk_bytes: Option<std::sync::Arc<Vec<u8>>>,
    ) -> Self {
        Self {
            page_receiver: Some(viewer_end.page_receiver),
            screen_info_sender: Some(viewer_end.screen_info_sender),
            advance_sender: Some(viewer_end.advance_sender),
            dpi_override,
            pages: Vec::new(),
            current_page: 0,
            zoom: 1.0,
            pan_offset: Vec2::ZERO,
            dragging: false,
            last_drag_pos: None,
            interpreter_done: false,
            job_done: false,
            screen_info_sent: false,
            window_sized: false,
            pending_position: None,
            dpi_preset: None,
            quit_requested: false,
            render_dirty: true,
            last_available: Vec2::ZERO,
            central_available: Vec2::ZERO,
            minimap: None,
            minimap_dragging: false,
            system_cmyk_bytes,
        }
    }

    /// Send screen info to the interpreter for DPI calculation.
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

            let ppp = ctx.input(|i| i.viewport().native_pixels_per_point.unwrap_or(1.0)) as f64;
            let available_h = monitor.y as f64 * ppp * 0.85;
            ScreenInfo::AvailableHeight(available_h)
        };

        if let Some(sender) = self.screen_info_sender.take() {
            let _ = sender.send(info);
        }
        self.screen_info_sent = true;
    }

    /// Resize the window to match the page aspect ratio.
    ///
    /// On the first call (initial window), always resizes since the compositor
    /// already placed the window correctly via `centered: true`.
    ///
    /// On subsequent calls (new jobs), only resizes if `outer_rect` is available
    /// (X11/Mac/Windows) so we can re-center. On Wayland, `outer_rect` is
    /// unavailable and repositioning is impossible, so we skip the resize and
    /// let zoom-to-fit handle the content.
    fn size_window_to_page(&mut self, ctx: &egui::Context) {
        if self.window_sized || self.pages.is_empty() {
            return;
        }

        let page = &self.pages[0];
        if page.width == 0 || page.height == 0 || page.dpi <= 0.0 {
            self.window_sized = true;
            return;
        }

        // Check if we can reposition (X11/Mac/Windows provide outer_rect;
        // Wayland does not). On the very first call outer_rect may be None
        // even on X11 (window just created), which is fine — the initial
        // window size was already set correctly by run_viewer.
        let outer = ctx.input(|i| i.viewport().outer_rect);
        let is_first_sizing = !self.window_sized;
        if !is_first_sizing && outer.is_none() {
            // Subsequent job on Wayland — can't reposition, skip resize
            self.window_sized = true;
            return;
        }
        self.window_sized = true;

        // Recover page dimensions in PostScript points from device pixels + DPI
        let page_pts_w = page.width as f32 * 72.0 / page.dpi as f32;
        let page_pts_h = page.height as f32 * 72.0 / page.dpi as f32;
        let aspect = page_pts_w / page_pts_h;

        // Status bar overhead so the central panel fits the image.
        let panel_overhead = ctx.screen_rect().height() - ctx.available_rect().height();

        // Target: 85% of monitor height for the content area
        let (max_w, max_h) = ctx.input(|i| {
            if let Some(monitor) = i.viewport().monitor_size {
                (monitor.x * 0.85, monitor.y * 0.85)
            } else {
                (1024.0, 768.0)
            }
        });

        // Size to fill 85% of screen height, then cap width
        let mut win_h = max_h;
        let mut win_w = win_h * aspect;
        if win_w > max_w {
            win_w = max_w;
            win_h = win_w / aspect;
        }
        win_h += panel_overhead;

        let win_w = win_w.max(400.0);
        let win_h = win_h.max(300.0);

        // On X11/Mac/Windows: compute new position to keep window's center
        // point fixed on the same monitor after resize.
        if let Some(outer) = outer {
            let cx = outer.min.x + outer.width() / 2.0;
            let cy = outer.min.y + outer.height() / 2.0;
            self.pending_position = Some(egui::pos2(
                (cx - win_w / 2.0).max(0.0),
                (cy - win_h / 2.0).max(0.0),
            ));
        }

        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(win_w, win_h)));
    }

    /// Check for newly arrived pages (non-blocking).
    /// Limits intake to a few pages per frame so the first frame renders quickly.
    fn poll_pages(&mut self, ctx: &egui::Context) {
        let had_pages = !self.pages.is_empty();
        let mut pages_cleared = false;
        let Some(receiver) = &self.page_receiver else {
            return;
        };
        // Accept at most a few pages per frame to avoid blocking the first paint.
        // Control messages (NewJob, JobDone) don't count toward the limit.
        let mut pages_this_frame = 0;
        const MAX_PAGES_PER_FRAME: usize = 4;
        loop {
            if pages_this_frame >= MAX_PAGES_PER_FRAME {
                // More pages may be waiting — request another repaint to drain them.
                ctx.request_repaint();
                break;
            }
            match receiver.try_recv() {
                Ok(ViewerMsg::Page(page)) => {
                    let prepared = stet_render::prepare_display_list(&page.display_list);
                    let icc_cache = stet_render::build_icc_cache_for_list(
                        &page.display_list,
                        self.system_cmyk_bytes.as_ref(),
                    );
                    self.pages.push(StoredPage {
                        display_list: page.display_list,
                        prepared,
                        width: page.width,
                        height: page.height,
                        dpi: page.dpi,
                        page_num: page.page_num,
                        cached_render: None,
                        icc_cache,
                    });
                    pages_this_frame += 1;
                }
                Ok(ViewerMsg::NewJob) => {
                    // New job starting — clear accumulated pages
                    self.pages.clear();
                    self.current_page = 0;
                    self.job_done = false;
                    self.render_dirty = true;
                    self.minimap = None;
                    self.window_sized = false;
                    pages_cleared = true;
                }
                Ok(ViewerMsg::JobDone) => {
                    self.job_done = true;
                    // Zero-page job — auto-advance to next job
                    if self.pages.is_empty() {
                        if let Some(ref sender) = self.advance_sender {
                            let _ = sender.send(());
                        }
                        self.job_done = false;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.interpreter_done = true;
                    break;
                }
            }
        }

        if (!had_pages || pages_cleared) && !self.pages.is_empty() {
            // First page of a new job — show it and resize the window
            self.current_page = 0;
            self.reset_view();
            self.size_window_to_page(ctx);
        }

        // Interpreter finished without producing any pages — auto-quit
        if self.interpreter_done && self.pages.is_empty() {
            self.quit_requested = true;
        }
    }

    /// Reset zoom and pan to defaults.
    fn reset_view(&mut self) {
        self.zoom = 1.0;
        self.pan_offset = Vec2::ZERO;
        self.dpi_preset = None;
        self.render_dirty = true;
        self.minimap = None;
    }

    /// Advance to the next page.
    fn next_page(&mut self) {
        if self.current_page + 1 < self.pages.len() {
            self.current_page += 1;
            self.reset_view();
        } else if self.job_done && !self.interpreter_done {
            // Last page of current job, more jobs pending — advance
            if let Some(ref sender) = self.advance_sender {
                let _ = sender.send(());
            }
            self.job_done = false;
        } else if self.interpreter_done {
            // No more pages, no more jobs — quit
            self.quit_requested = true;
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
    ///
    /// Returns the scale that maps reference-DPI device pixels to available
    /// screen pixels (logical × ppp).
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

    /// Calculate effective DPI for the current zoom level.
    fn effective_dpi(&self, available: Vec2, page: &StoredPage) -> f64 {
        let fit = self.fit_scale(available, page);
        page.dpi * fit as f64 * self.zoom as f64
    }

    /// Adjust pan offset so the point under `cursor` stays fixed when zoom changes.
    ///
    /// Accounts for the auto-centering offset that applies when the image is
    /// smaller than the window, which the naive `pan = cursor - (cursor - pan) * ratio`
    /// formula misses.
    fn zoom_toward_cursor(&mut self, cursor: egui::Pos2, new_zoom: f32) {
        if self.pages.is_empty() || self.central_available == Vec2::ZERO {
            self.zoom = new_zoom;
            return;
        }
        let page = &self.pages[self.current_page];
        let fit = self.fit_scale(self.central_available, page);
        let avail = self.central_available;

        // Old centering offset
        let old_scale = fit * self.zoom;
        let old_cx = ((avail.x - page.width as f32 * old_scale) / 2.0).max(0.0);
        let old_cy = ((avail.y - page.height as f32 * old_scale) / 2.0).max(0.0);

        // New centering offset
        let new_scale = fit * new_zoom;
        let new_cx = ((avail.x - page.width as f32 * new_scale) / 2.0).max(0.0);
        let new_cy = ((avail.y - page.height as f32 * new_scale) / 2.0).max(0.0);

        // The image point under the cursor (in device pixels):
        //   img_pt = (cursor - center - pan) / scale
        // We want the same img_pt under cursor at the new zoom:
        //   cursor - new_center - new_pan = img_pt * new_scale
        // So: new_pan = cursor - new_center - (cursor - old_center - old_pan) * ratio
        let ratio = new_scale / old_scale;
        let c = cursor.to_vec2();
        self.pan_offset.x = c.x * (1.0 - ratio) - new_cx + (old_cx + self.pan_offset.x) * ratio;
        self.pan_offset.y = c.y * (1.0 - ratio) - new_cy + (old_cy + self.pan_offset.y) * ratio;
        self.zoom = new_zoom;

        // At fit-to-window, auto-centering handles positioning — reset pan
        if new_zoom <= 1.0 {
            self.pan_offset = Vec2::ZERO;
        }
    }

    /// Render the visible viewport of the current page and return/update the texture.
    fn render_viewport(&mut self, ctx: &egui::Context, available: Vec2, ppp: f32) {
        if self.pages.is_empty() {
            return;
        }
        let page_idx = self.current_page;
        let page = &self.pages[page_idx];

        let fit = self.fit_scale(available, page);
        let effective_scale = fit * self.zoom;

        // Image dimensions in screen pixels at current zoom
        let img_w = page.width as f32 * effective_scale;
        let img_h = page.height as f32 * effective_scale;

        // Center offset
        let center_x = ((available.x - img_w) / 2.0).max(0.0);
        let center_y = ((available.y - img_h) / 2.0).max(0.0);

        // Viewport in screen coordinates (what's visible in the window)
        let screen_vp_x = -(center_x + self.pan_offset.x);
        let screen_vp_y = -(center_y + self.pan_offset.y);
        let screen_vp_w = available.x;
        let screen_vp_h = available.y;

        // Clamp viewport to image bounds
        let clamp_x = screen_vp_x.max(0.0).min((img_w - screen_vp_w).max(0.0));
        let clamp_y = screen_vp_y.max(0.0).min((img_h - screen_vp_h).max(0.0));
        let clamp_w = screen_vp_w.min(img_w - clamp_x).min(img_w);
        let clamp_h = screen_vp_h.min(img_h - clamp_y).min(img_h);

        if clamp_w <= 0.0 || clamp_h <= 0.0 {
            return;
        }

        // Convert screen viewport to reference-DPI device coordinates
        let vp_x = (clamp_x / effective_scale) as f64;
        let vp_y = (clamp_y / effective_scale) as f64;
        let vp_w = (clamp_w / effective_scale) as f64;
        let vp_h = (clamp_h / effective_scale) as f64;

        // Output pixel dimensions (physical pixels)
        let pixel_w = (clamp_w * ppp).round() as u32;
        let pixel_h = (clamp_h * ppp).round() as u32;

        if pixel_w == 0 || pixel_h == 0 {
            return;
        }

        // Check if cached render is still valid
        if let Some(ref cached) = self.pages[page_idx].cached_render
            && (cached.vp_x - vp_x).abs() < 0.01
            && (cached.vp_y - vp_y).abs() < 0.01
            && (cached.vp_w - vp_w).abs() < 0.01
            && (cached.vp_h - vp_h).abs() < 0.01
            && cached.pixel_w == pixel_w
            && cached.pixel_h == pixel_h
        {
            return; // cache hit
        }

        // Render the viewport region using precomputed metadata
        let rgba = stet_render::render_region_prepared(
            &page.display_list,
            &page.prepared,
            vp_x,
            vp_y,
            vp_w,
            vp_h,
            pixel_w,
            pixel_h,
            page.dpi,
            Some(&page.icc_cache),
        );

        let image = ColorImage::from_rgba_unmultiplied([pixel_w as usize, pixel_h as usize], &rgba);
        let texture = ctx.load_texture(
            format!("viewport_p{}", page.page_num),
            image,
            TextureOptions::LINEAR,
        );

        self.pages[page_idx].cached_render = Some(CachedRender {
            texture,
            vp_x,
            vp_y,
            vp_w,
            vp_h,
            pixel_w,
            pixel_h,
        });

        self.render_dirty = false;
    }

    /// Render or update the minimap thumbnail for the current page.
    fn ensure_minimap(&mut self, ctx: &egui::Context) {
        if self.pages.is_empty() {
            return;
        }
        let page_idx = self.current_page;

        // Check if we already have a valid minimap for this page
        if let Some(ref mm) = self.minimap
            && mm.page_index == page_idx
        {
            return;
        }

        let page = &self.pages[page_idx];
        let pw = page.width as f32;
        let ph = page.height as f32;
        if pw <= 0.0 || ph <= 0.0 {
            return;
        }

        // Calculate minimap pixel dimensions (maintain aspect ratio)
        let scale = (MINIMAP_MAX_W / pw).min(MINIMAP_MAX_H / ph);
        let mm_w = (pw * scale).round() as u32;
        let mm_h = (ph * scale).round() as u32;
        if mm_w == 0 || mm_h == 0 {
            return;
        }

        let rgba = stet_render::render_region_prepared(
            &page.display_list,
            &page.prepared,
            0.0,
            0.0,
            pw as f64,
            ph as f64,
            mm_w,
            mm_h,
            page.dpi,
            Some(&page.icc_cache),
        );

        let image = ColorImage::from_rgba_unmultiplied([mm_w as usize, mm_h as usize], &rgba);
        let texture = ctx.load_texture(
            format!("minimap_p{}", page.page_num),
            image,
            TextureOptions::LINEAR,
        );

        self.minimap = Some(MinimapState {
            texture,
            page_index: page_idx,
        });
    }

    /// Draw the minimap overlay when zoomed in.
    fn draw_minimap(
        &mut self,
        ui: &mut egui::Ui,
        available: Vec2,
        page_width: u32,
        page_height: u32,
        page_dpi: f64,
    ) {
        let img_w_raw = page_width as f32;
        let img_h_raw = page_height as f32;
        if img_w_raw <= 0.0 || img_h_raw <= 0.0 {
            return;
        }
        let fit = (available.x / img_w_raw).min(available.y / img_h_raw);
        let _ = page_dpi; // kept for future use
        let effective_scale = fit * self.zoom;
        let img_w = img_w_raw * effective_scale;
        let img_h = img_h_raw * effective_scale;

        // Only show minimap when zoomed in past the window size
        if img_w <= available.x + 1.0 && img_h <= available.y + 1.0 {
            return;
        }

        let Some(ref mm) = self.minimap else { return };

        let tex_size = mm.texture.size_vec2();
        let mm_w = tex_size.x;
        let mm_h = tex_size.y;

        // Position in bottom-right corner
        let panel_rect = ui.min_rect();
        let mm_x = panel_rect.max.x - mm_w - MINIMAP_MARGIN;
        let mm_y = panel_rect.max.y - mm_h - MINIMAP_MARGIN;
        let mm_rect = egui::Rect::from_min_size(egui::pos2(mm_x, mm_y), egui::vec2(mm_w, mm_h));

        let painter = ui.painter();

        // Semi-transparent background
        painter.rect_filled(
            mm_rect.expand(2.0),
            4.0,
            egui::Color32::from_black_alpha(160),
        );

        // Draw thumbnail
        painter.image(
            mm.texture.id(),
            mm_rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );

        // Calculate viewport indicator rectangle
        let center_x = ((available.x - img_w) / 2.0).max(0.0);
        let center_y = ((available.y - img_h) / 2.0).max(0.0);
        let vp_left = -(center_x + self.pan_offset.x);
        let vp_top = -(center_y + self.pan_offset.y);

        // Normalize to [0, 1] in image space
        let norm_x = (vp_left / img_w).clamp(0.0, 1.0);
        let norm_y = (vp_top / img_h).clamp(0.0, 1.0);
        let norm_w = (available.x / img_w).clamp(0.0, 1.0 - norm_x);
        let norm_h = (available.y / img_h).clamp(0.0, 1.0 - norm_y);

        // Map to minimap coordinates
        let vp_rect = egui::Rect::from_min_size(
            egui::pos2(mm_x + norm_x * mm_w, mm_y + norm_y * mm_h),
            egui::vec2(norm_w * mm_w, norm_h * mm_h),
        );

        // Draw viewport indicator
        painter.rect_stroke(
            vp_rect,
            0.0,
            egui::Stroke::new(1.5, egui::Color32::from_rgb(60, 140, 255)),
            egui::StrokeKind::Outside,
        );
        painter.rect_filled(vp_rect, 0.0, egui::Color32::from_white_alpha(40));

        // Handle minimap interaction (click/drag to pan)
        let minimap_response = ui.interact(
            mm_rect,
            ui.id().with("minimap"),
            egui::Sense::click_and_drag(),
        );

        if (minimap_response.dragged() || minimap_response.clicked())
            && let Some(pos) = minimap_response.interact_pointer_pos()
        {
            // Convert click position to normalized image coordinates
            let click_norm_x = ((pos.x - mm_x) / mm_w).clamp(0.0, 1.0);
            let click_norm_y = ((pos.y - mm_y) / mm_h).clamp(0.0, 1.0);

            // Center the viewport on the clicked point
            let target_vp_center_x = click_norm_x * img_w;
            let target_vp_center_y = click_norm_y * img_h;

            self.pan_offset.x = -(target_vp_center_x - available.x / 2.0) + center_x;
            self.pan_offset.y = -(target_vp_center_y - available.y / 2.0) + center_y;
            self.render_dirty = true;
            self.minimap_dragging = true;
        }
        if minimap_response.drag_stopped() {
            self.minimap_dragging = false;
        }
    }
}

impl eframe::App for ViewerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Handle quit
        if self.quit_requested {
            self.page_receiver = None;
            self.screen_info_sender = None;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        let ppp = ctx.input(|i| i.viewport().native_pixels_per_point.unwrap_or(1.0));

        // Status bar
        let status_font = egui::FontId::proportional(16.0);
        egui::TopBottomPanel::bottom("status")
            .min_height(32.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if self.pages.is_empty() {
                        ui.label(
                            egui::RichText::new("Waiting for page...").font(status_font.clone()),
                        );
                    } else {
                        let total = if self.interpreter_done {
                            format!("{}", self.pages.len())
                        } else {
                            format!("{}+", self.pages.len())
                        };
                        let page = &self.pages[self.current_page];
                        let eff_dpi = self
                            .dpi_preset
                            .unwrap_or_else(|| self.effective_dpi(self.central_available, page));
                        let fit = self.fit_scale(self.central_available, page);
                        let zoom_pct = fit * self.zoom * 100.0;
                        ui.label(
                            egui::RichText::new(format!(
                                "Page {} of {} | {:.0} DPI | Zoom: {:.0}%",
                                self.current_page + 1,
                                total,
                                eff_dpi,
                                zoom_pct,
                            ))
                            .font(status_font.clone()),
                        );
                        ui.separator();
                        ui.label(egui::RichText::new(
                        "Space/Right: next | Left: prev | +/-: zoom | 0: fit | 1-7: DPI | Q: quit"
                    ).font(status_font));
                    }
                });
            });

        // Send screen info to interpreter
        self.send_screen_info(ctx);

        // Poll for new pages
        self.poll_pages(ctx);

        // Apply deferred window position (keeps center fixed after resize)
        if let Some(pos) = self.pending_position.take() {
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
        }

        // Handle keyboard input
        ctx.input(|i| {
            // Quit
            if i.key_pressed(egui::Key::Q) || i.key_pressed(egui::Key::Escape) {
                self.quit_requested = true;
                return;
            }

            // Zoom (no max cap — display lists render at any zoom)
            if i.key_pressed(egui::Key::Equals) || i.key_pressed(egui::Key::Plus) {
                let new_zoom = (self.zoom * 1.25).min(1000.0);
                if let Some(pos) = i.pointer.latest_pos() {
                    self.zoom_toward_cursor(pos, new_zoom);
                } else {
                    self.zoom = new_zoom;
                }
                self.dpi_preset = None;
                self.render_dirty = true;
            }
            if i.key_pressed(egui::Key::Minus) {
                let new_zoom = (self.zoom / 1.25).max(1.0);
                if let Some(pos) = i.pointer.latest_pos() {
                    self.zoom_toward_cursor(pos, new_zoom);
                } else {
                    self.zoom = new_zoom;
                }
                self.dpi_preset = None;
                self.render_dirty = true;
            }
            if i.key_pressed(egui::Key::Num0) {
                self.reset_view();
            }

            // DPI presets as zoom shortcuts
            if !self.pages.is_empty() {
                let page = &self.pages[self.current_page];
                let fit = self.fit_scale(self.central_available, page);
                let ref_dpi = page.dpi as f32;

                for &(key, target_dpi, _) in &DPI_PRESETS {
                    if i.key_pressed(key) {
                        let new_zoom = (target_dpi as f32 / (ref_dpi * fit)).max(1.0);
                        if let Some(pos) = i.pointer.latest_pos() {
                            self.zoom_toward_cursor(pos, new_zoom);
                        } else {
                            self.zoom = new_zoom;
                        }
                        self.dpi_preset = Some(target_dpi);
                        self.render_dirty = true;
                        self.minimap = None;
                    }
                }
            }

            // Mouse wheel zoom (zoom toward cursor)
            let scroll = i.smooth_scroll_delta.y;
            if scroll.abs() > 0.5 {
                let factor = 1.25_f32.powf(scroll / 40.0);
                let new_zoom = (self.zoom * factor).clamp(1.0, 1000.0);
                if let Some(pos) = i.pointer.latest_pos() {
                    self.zoom_toward_cursor(pos, new_zoom);
                } else {
                    self.zoom = new_zoom;
                }
                self.dpi_preset = None;
                self.render_dirty = true;
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
                    self.render_dirty = true;
                }
                if i.key_pressed(egui::Key::ArrowDown) {
                    self.pan_offset.y -= 50.0;
                    self.render_dirty = true;
                }
            }
        });

        // Handle mouse drag for panning (skip if minimap is being dragged)
        if !self.minimap_dragging {
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
                        if delta.length_sq() > 0.0 {
                            self.pan_offset += delta;
                            self.render_dirty = true;
                        }
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
        } else {
            // Still track release even during minimap drag.
            // Clear main dragging state so it doesn't resume panning.
            ctx.input(|i| {
                if i.pointer.primary_released() {
                    self.minimap_dragging = false;
                    self.dragging = false;
                    self.last_drag_pos = None;
                }
            });
        }

        // Main content area
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(ctx.style().visuals.panel_fill))
            .show(ctx, |ui| {
                if self.pages.is_empty() {
                    ui.centered_and_justified(|ui| {
                        ui.label("Waiting for first page to render...");
                    });
                    return;
                }

                let available = ui.available_size();
                self.central_available = available;
                let page_idx = self.current_page;

                // Detect window resize — re-render if available area changed
                if (available - self.last_available).length_sq() > 1.0 {
                    self.render_dirty = true;
                    self.last_available = available;
                }

                // Render viewport if dirty
                if self.render_dirty {
                    self.render_viewport(ctx, available, ppp);
                }

                // Draw the rendered viewport
                let page = &self.pages[page_idx];
                let fit = self.fit_scale(available, page);
                let effective_scale = fit * self.zoom;
                let img_w = page.width as f32 * effective_scale;
                let img_h = page.height as f32 * effective_scale;

                if let Some(ref cached) = page.cached_render {
                    // Center the full image area in the available space
                    let center_x = ((available.x - img_w) / 2.0).max(0.0);
                    let center_y = ((available.y - img_h) / 2.0).max(0.0);

                    // Map cached viewport (in device coords) back to current screen
                    // coords using the current effective_scale. This stays correct
                    // even if the window resized since the render was cached.
                    let tex_x = cached.vp_x as f32 * effective_scale;
                    let tex_y = cached.vp_y as f32 * effective_scale;
                    let tex_w = cached.vp_w as f32 * effective_scale;
                    let tex_h = cached.vp_h as f32 * effective_scale;

                    let origin = ui.min_rect().min;
                    let img_origin = origin
                        + egui::vec2(center_x + self.pan_offset.x, center_y + self.pan_offset.y);

                    // Checkerboard background for the full image area
                    let full_rect = egui::Rect::from_min_size(img_origin, egui::vec2(img_w, img_h));
                    ui.painter()
                        .rect_filled(full_rect, 0.0, egui::Color32::from_gray(200));

                    // Draw the viewport texture, sized by the viewport's device
                    // coords mapped to screen space (egui scales the texture to fit)
                    let tex_rect = egui::Rect::from_min_size(
                        img_origin + egui::vec2(tex_x, tex_y),
                        egui::vec2(tex_w, tex_h),
                    );
                    ui.painter().image(
                        cached.texture.id(),
                        tex_rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                }

                // Minimap overlay (when zoomed in)
                self.ensure_minimap(ctx);
                let (pw, ph, pdpi) = {
                    let p = &self.pages[page_idx];
                    (p.width, p.height, p.dpi)
                };
                self.draw_minimap(ui, available, pw, ph, pdpi);
            });

        // Request periodic repaints to check for new pages
        if !self.interpreter_done {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}
