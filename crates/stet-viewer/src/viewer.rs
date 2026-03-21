// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! egui viewer application — renders PostScript pages on demand from display lists.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError};
use std::time::{Duration, Instant};

use egui::{ColorImage, TextureHandle, TextureOptions, Vec2};
use stet_graphics::display_list::DisplayList;
use stet_render::{ImageCache, PreparedDisplayList};

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
    /// Whether the window needs resizing for the current page.
    needs_resize: bool,
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
    /// Disable anti-aliasing for all rendering.
    no_aa: bool,
    /// Previous page's texture, shown during page transitions to avoid black flash.
    transition_texture: Option<TextureHandle>,
    /// Timestamp of most recent viewport change (for debouncing).
    render_requested_at: Option<Instant>,
    /// Receiver for async viewport render results.
    render_receiver: Option<std::sync::mpsc::Receiver<AsyncRenderResult>>,
    /// Viewport params of the in-flight async render (to detect stale results).
    inflight_params: Option<ViewportParams>,
    /// Receiver for full-page background render.
    fullpage_receiver: Option<std::sync::mpsc::Receiver<FullPageRenderResult>>,
    /// Scale of in-flight full-page render (to detect stale results on zoom).
    fullpage_inflight_scale: Option<(f32, f32)>,
    /// When the full-page render was spawned (for delayed indicator).
    fullpage_started: Option<Instant>,
    /// Cancellation flag for in-flight renders. Set on zoom/pan change.
    render_cancel: Arc<std::sync::atomic::AtomicBool>,
    /// Channel to send dropped file paths to the interpreter.
    file_drop_sender: Option<std::sync::mpsc::Sender<String>>,
}

/// Result from a full-page background render.
struct FullPageRenderResult {
    rgba: Vec<u8>,
    pixel_w: u32,
    pixel_h: u32,
    effective_scale: f32,
    ppp: f32,
    page_idx: usize,
}

/// Debounce delay before spawning a viewport render.
const RENDER_DEBOUNCE: Duration = Duration::from_millis(30);

/// Parameters identifying a viewport render (for cache/stale checks).
#[derive(Clone, PartialEq)]
struct ViewportParams {
    page_idx: usize,
    vp_x: f64,
    vp_y: f64,
    vp_w: f64,
    vp_h: f64,
    pixel_w: u32,
    pixel_h: u32,
}

/// Result from an async viewport render.
struct AsyncRenderResult {
    rgba: Vec<u8>,
    params: ViewportParams,
}

/// A page stored as a resolution-independent display list.
struct StoredPage {
    display_list: Arc<DisplayList>,
    /// Precomputed bboxes/epochs for fast viewport rendering.
    prepared: Arc<PreparedDisplayList>,
    /// Device pixel dimensions at reference DPI.
    width: u32,
    height: u32,
    /// Reference DPI from the interpreter.
    dpi: f64,
    page_num: u32,
    /// Cached viewport render (reused if viewport unchanged).
    cached_render: Option<CachedRender>,
    /// ICC cache built from this page's display list profiles.
    icc_cache: Arc<stet_graphics::icc::IccCache>,
    /// Pre-converted RGBA image cache for fast viewport rendering.
    image_cache: Arc<ImageCache>,
    /// Full-page RGBA buffer at screen resolution for instant panning.
    full_page: Option<FullPageBuffer>,
}

/// Pre-rendered full-page buffer at a specific zoom level.
/// Panning blits from this buffer instead of re-rendering.
struct FullPageBuffer {
    rgba: Vec<u8>,
    pixel_w: u32,
    pixel_h: u32,
    /// The effective_scale (fit * zoom) this was rendered at.
    effective_scale: f32,
    /// Pixels per point (HiDPI factor) this was rendered at.
    ppp: f32,
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
        no_aa: bool,
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
            needs_resize: false,
            pending_position: None,
            dpi_preset: None,
            quit_requested: false,
            render_dirty: true,
            last_available: Vec2::ZERO,
            central_available: Vec2::ZERO,
            minimap: None,
            minimap_dragging: false,
            system_cmyk_bytes,
            no_aa,
            transition_texture: None,
            render_requested_at: None,
            render_receiver: None,
            inflight_params: None,
            fullpage_receiver: None,
            fullpage_inflight_scale: None,
            fullpage_started: None,
            render_cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            file_drop_sender: Some(viewer_end.file_drop_sender),
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
    /// Resize the window to fit the current page's aspect ratio.
    ///
    /// Content area fills 85% of the monitor's limiting dimension
    /// (height or width, whichever constrains the page's aspect ratio),
    /// with the window width matching the content width exactly.
    fn size_window_to_page(&mut self, ctx: &egui::Context) {
        if self.pages.is_empty() {
            return;
        }

        let page = &self.pages[self.current_page];
        if page.width == 0 || page.height == 0 || page.dpi <= 0.0 {
            return;
        }

        // Recover page dimensions in PostScript points from device pixels + DPI
        let page_pts_w = page.width as f32 * 72.0 / page.dpi as f32;
        let page_pts_h = page.height as f32 * 72.0 / page.dpi as f32;
        let aspect = page_pts_w / page_pts_h;

        // Panel overhead so the central panel fits the image exactly.
        let panel_overhead_h = ctx.screen_rect().height() - ctx.available_rect().height();
        let panel_overhead_w = ctx.screen_rect().width() - ctx.available_rect().width();

        // Target: 85% of monitor size for the window
        let (max_w, max_h) = ctx.input(|i| {
            if let Some(monitor) = i.viewport().monitor_size {
                (monitor.x * 0.85, monitor.y * 0.85)
            } else {
                (1024.0, 768.0)
            }
        });

        // Size content to fill 85% of screen, respecting aspect ratio
        let max_content_h = max_h - panel_overhead_h;
        let max_content_w = max_w - panel_overhead_w;
        let mut content_w = max_content_h * aspect;
        let mut content_h = max_content_h;
        if content_w > max_content_w {
            content_w = max_content_w;
            content_h = content_w / aspect;
        }
        let win_w = (content_w + panel_overhead_w).max(400.0);
        let win_h = (content_h + panel_overhead_h).max(300.0);

        // Center window on the monitor.
        let monitor = ctx.input(|i| i.viewport().monitor_size);
        if let Some(mon) = monitor {
            self.pending_position = Some(egui::pos2(
                ((mon.x - win_w) / 2.0).max(0.0),
                ((mon.y - win_h) / 2.0).max(0.0),
            ));
        }

        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(win_w, win_h)));
    }

    /// Check for newly arrived pages (non-blocking).
    /// Limits intake to a few pages per frame so the first frame renders quickly.
    fn poll_pages(&mut self, ctx: &egui::Context) {
        let had_pages = !self.pages.is_empty();
        let mut pages_cleared = false;
        let Some(receiver) = self.page_receiver.take() else {
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
                    let image_cache = ImageCache::build(&page.display_list, Some(&icc_cache));
                    self.pages.push(StoredPage {
                        display_list: Arc::new(page.display_list),
                        prepared: Arc::new(prepared),
                        width: page.width,
                        height: page.height,
                        dpi: page.dpi,
                        page_num: page.page_num,
                        cached_render: None,
                        icc_cache: Arc::new(icc_cache),
                        image_cache: Arc::new(image_cache),
                        full_page: None,
                    });
                    pages_this_frame += 1;
                }
                Ok(ViewerMsg::NewJob) => {
                    // New job starting — clear accumulated pages
                    self.pages.clear();
                    self.current_page = 0;
                    self.job_done = false;
                    self.render_dirty = true;
                    self.request_render();
                    self.minimap = None;
                    self.window_sized = false;
                    pages_cleared = true;
                }
                Ok(ViewerMsg::JobDone) => {
                    self.job_done = true;
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

        // Put receiver back
        self.page_receiver = Some(receiver);
    }

    /// Save the current page's texture for transition display.
    fn save_transition_texture(&mut self) {
        if let Some(page) = self.pages.get(self.current_page) {
            if let Some(ref cached) = page.cached_render {
                self.transition_texture = Some(cached.texture.clone());
            }
        }
    }

    /// Reset zoom and pan to defaults.
    fn reset_view(&mut self) {
        self.zoom = 1.0;
        self.pan_offset = Vec2::ZERO;
        self.dpi_preset = None;
        self.render_dirty = true;
        self.request_render();
        self.minimap = None;
    }

    /// Check if two pages have the same dimensions (in PostScript points).
    fn pages_same_size(&self, a: usize, b: usize) -> bool {
        if let (Some(pa), Some(pb)) = (self.pages.get(a), self.pages.get(b)) {
            let wa = (pa.width as f64 * 72.0 / pa.dpi).round() as u32;
            let ha = (pa.height as f64 * 72.0 / pa.dpi).round() as u32;
            let wb = (pb.width as f64 * 72.0 / pb.dpi).round() as u32;
            let hb = (pb.height as f64 * 72.0 / pb.dpi).round() as u32;
            wa == wb && ha == hb
        } else {
            false
        }
    }

    /// Advance to the next page.
    fn next_page(&mut self) {
        if self.current_page + 1 < self.pages.len() {
            let prev = self.current_page;
            self.save_transition_texture();
            self.current_page += 1;
            if !self.pages_same_size(prev, self.current_page) {
                self.needs_resize = true;
            }
            self.reset_view();
        } else if self.interpreter_done {
            // No more pages, no more jobs — quit
            self.quit_requested = true;
        }
        // else: on last page but interpreter still alive (accepting drops) — do nothing
    }

    /// Go to the previous page.
    fn prev_page(&mut self) {
        if self.current_page > 0 {
            let prev = self.current_page;
            self.save_transition_texture();
            self.current_page -= 1;
            if !self.pages_same_size(prev, self.current_page) {
                self.needs_resize = true;
            }
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
    /// Compute the current viewport parameters for the active page.
    fn compute_viewport_params(&self, available: Vec2, ppp: f32) -> Option<(ViewportParams, f64)> {
        if self.pages.is_empty() {
            return None;
        }
        let page_idx = self.current_page;
        let page = &self.pages[page_idx];

        let fit = self.fit_scale(available, page);
        let effective_scale = fit * self.zoom;

        let img_w = page.width as f32 * effective_scale;
        let img_h = page.height as f32 * effective_scale;

        let center_x = ((available.x - img_w) / 2.0).max(0.0);
        let center_y = ((available.y - img_h) / 2.0).max(0.0);

        let screen_vp_x = -(center_x + self.pan_offset.x);
        let screen_vp_y = -(center_y + self.pan_offset.y);
        let screen_vp_w = available.x;
        let screen_vp_h = available.y;

        let clamp_x = screen_vp_x.max(0.0).min((img_w - screen_vp_w).max(0.0));
        let clamp_y = screen_vp_y.max(0.0).min((img_h - screen_vp_h).max(0.0));
        let clamp_w = screen_vp_w.min(img_w - clamp_x).min(img_w);
        let clamp_h = screen_vp_h.min(img_h - clamp_y).min(img_h);

        if clamp_w <= 0.0 || clamp_h <= 0.0 {
            return None;
        }

        let vp_x = (clamp_x / effective_scale) as f64;
        let vp_y = (clamp_y / effective_scale) as f64;
        let vp_w = (clamp_w / effective_scale) as f64;
        let vp_h = (clamp_h / effective_scale) as f64;

        let pixel_w = (clamp_w * ppp).round() as u32;
        let pixel_h = (clamp_h * ppp).round() as u32;

        if pixel_w == 0 || pixel_h == 0 {
            return None;
        }

        Some((
            ViewportParams {
                page_idx,
                vp_x,
                vp_y,
                vp_w,
                vp_h,
                pixel_w,
                pixel_h,
            },
            page.dpi,
        ))
    }

    /// Check if the cached render matches the given viewport params.
    fn cache_matches(&self, vp: &ViewportParams) -> bool {
        if let Some(ref cached) = self.pages[vp.page_idx].cached_render {
            (cached.vp_x - vp.vp_x).abs() < 0.01
                && (cached.vp_y - vp.vp_y).abs() < 0.01
                && (cached.vp_w - vp.vp_w).abs() < 0.01
                && (cached.vp_h - vp.vp_h).abs() < 0.01
                && cached.pixel_w == vp.pixel_w
                && cached.pixel_h == vp.pixel_h
        } else {
            false
        }
    }

    /// Request an async viewport render. Debounces rapid changes.
    /// Cancels any stale in-flight renders — zoom/pan changed, results would be wrong.
    fn request_render(&mut self) {
        self.render_requested_at = Some(Instant::now());
        // Signal cancellation to any in-flight render threads
        self.render_cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
        // Create a fresh cancel flag for the next render
        self.render_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        // Drop stale receivers
        self.render_receiver = None;
        self.inflight_params = None;
        self.fullpage_receiver = None;
        self.fullpage_inflight_scale = None;
        self.fullpage_started = None;
    }

    /// Try to blit the current viewport from the full-page buffer.
    /// Returns true if successful (no rendering needed).
    fn try_blit_from_fullpage(
        &mut self,
        ctx: &egui::Context,
        vp: &ViewportParams,
        available: Vec2,
        ppp: f32,
    ) -> bool {
        let page = &self.pages[vp.page_idx];
        let fit = self.fit_scale(available, page);
        let effective_scale = fit * self.zoom;

        let Some(ref fp) = page.full_page else {
            return false;
        };
        // Check that the full-page buffer matches current zoom/DPI
        if (fp.effective_scale - effective_scale).abs() > 0.001 || (fp.ppp - ppp).abs() > 0.001 {
            return false;
        }

        // Compute source rect in the full-page buffer
        let src_x = (vp.vp_x * fp.pixel_w as f64 / page.width as f64).round() as i32;
        let src_y = (vp.vp_y * fp.pixel_h as f64 / page.height as f64).round() as i32;
        let src_w = vp.pixel_w as i32;
        let src_h = vp.pixel_h as i32;

        // Clamp to buffer bounds
        let x0 = src_x.max(0) as u32;
        let y0 = src_y.max(0) as u32;
        let x1 = (src_x + src_w).min(fp.pixel_w as i32).max(0) as u32;
        let y1 = (src_y + src_h).min(fp.pixel_h as i32).max(0) as u32;
        let copy_w = x1.saturating_sub(x0);
        let copy_h = y1.saturating_sub(y0);

        if copy_w == 0 || copy_h == 0 {
            return false;
        }

        // Blit rows from the full-page buffer
        let mut rgba = vec![255u8; vp.pixel_w as usize * vp.pixel_h as usize * 4];
        let dst_x_off = (x0 as i32 - src_x) as usize;
        let dst_y_off = (y0 as i32 - src_y) as usize;
        let dst_stride = vp.pixel_w as usize * 4;
        let src_stride = fp.pixel_w as usize * 4;

        for row in 0..copy_h as usize {
            let src_start = (y0 as usize + row) * src_stride + x0 as usize * 4;
            let dst_start = (dst_y_off + row) * dst_stride + dst_x_off * 4;
            let len = copy_w as usize * 4;
            if src_start + len <= fp.rgba.len() && dst_start + len <= rgba.len() {
                rgba[dst_start..dst_start + len]
                    .copy_from_slice(&fp.rgba[src_start..src_start + len]);
            }
        }

        let image = ColorImage::from_rgba_unmultiplied(
            [vp.pixel_w as usize, vp.pixel_h as usize],
            &rgba,
        );
        let texture = ctx.load_texture(
            format!("viewport_p{}", page.page_num),
            image,
            TextureOptions::LINEAR,
        );
        self.pages[vp.page_idx].cached_render = Some(CachedRender {
            texture,
            vp_x: vp.vp_x,
            vp_y: vp.vp_y,
            vp_w: vp.vp_w,
            vp_h: vp.vp_h,
            pixel_w: vp.pixel_w,
            pixel_h: vp.pixel_h,
        });
        self.render_dirty = false;
        self.render_requested_at = None;
        true
    }

    /// Spawn a full-page background render if one isn't already in flight.
    /// Only spawns when the viewport is stable (not dirty, no render in-flight).
    fn spawn_fullpage_render(&mut self, ctx: &egui::Context, available: Vec2, ppp: f32) {
        if self.pages.is_empty()
            || self.fullpage_receiver.is_some()
            || self.render_dirty
            || self.render_receiver.is_some()
        {
            return;
        }
        let page_idx = self.current_page;
        let page = &self.pages[page_idx];
        let fit = self.fit_scale(available, page);
        let effective_scale = fit * self.zoom;

        // Already have a valid buffer?
        if let Some(ref fp) = page.full_page {
            if (fp.effective_scale - effective_scale).abs() < 0.001
                && (fp.ppp - ppp).abs() < 0.001
            {
                return;
            }
        }

        // Already rendering at this scale?
        if let Some((s, p)) = self.fullpage_inflight_scale {
            if (s - effective_scale).abs() < 0.001 && (p - ppp).abs() < 0.001 {
                return;
            }
        }

        // Full-page pixel dimensions at screen resolution.
        // Cap total pixels to avoid multi-GB allocations at extreme zoom.
        // The viewport render handles detail at high zoom; the full-page
        // buffer is only useful for scroll/blit at moderate zoom levels.
        let fp_pixel_w = (page.width as f32 * effective_scale * ppp).round() as u32;
        let fp_pixel_h = (page.height as f32 * effective_scale * ppp).round() as u32;
        if fp_pixel_w == 0 || fp_pixel_h == 0 {
            return;
        }
        const MAX_FULLPAGE_PIXELS: u64 = 64 * 1024 * 1024;
        if (fp_pixel_w as u64) * (fp_pixel_h as u64) > MAX_FULLPAGE_PIXELS {
            return;
        }

        let dl = Arc::clone(&page.display_list);
        let prep = Arc::clone(&page.prepared);
        let icc = Arc::clone(&page.icc_cache);
        let img_cache = Arc::clone(&page.image_cache);
        let dpi = page.dpi;
        let dev_w = page.width as f64;
        let dev_h = page.height as f64;
        let no_aa = self.no_aa;

        let cancel = Arc::clone(&self.render_cancel);

        let (tx, rx) = std::sync::mpsc::channel();
        self.fullpage_receiver = Some(rx);
        self.fullpage_inflight_scale = Some((effective_scale, ppp));
        self.fullpage_started = Some(Instant::now());

        let egui_ctx = ctx.clone();
        // Use std::thread (not rayon) so it doesn't compete with viewport renders
        std::thread::spawn(move || {
            let result = stet_render::render_region_prepared_parallel_cancellable(
                &dl, &prep, 0.0, 0.0, dev_w, dev_h, fp_pixel_w, fp_pixel_h, dpi,
                Some(&icc), Some(&img_cache), no_aa, &cancel,
            );
            let Some(rgba) = result else {
                egui_ctx.request_repaint();
                return;
            };
            let _ = tx.send(FullPageRenderResult {
                rgba,
                pixel_w: fp_pixel_w,
                pixel_h: fp_pixel_h,
                effective_scale,
                ppp,
                page_idx,
            });
            egui_ctx.request_repaint();
        });
    }

    /// Poll for async render results, blit from full-page buffer, or spawn renders.
    fn process_async_render(&mut self, ctx: &egui::Context, available: Vec2, ppp: f32) {
        // 1. Poll completed full-page background render
        if let Some(ref rx) = self.fullpage_receiver {
            match rx.try_recv() {
                Ok(result) => {
                    if result.page_idx < self.pages.len() {
                        self.pages[result.page_idx].full_page = Some(FullPageBuffer {
                            rgba: result.rgba,
                            pixel_w: result.pixel_w,
                            pixel_h: result.pixel_h,
                            effective_scale: result.effective_scale,
                            ppp: result.ppp,
                        });
                    }
                    self.fullpage_receiver = None;
                    self.fullpage_inflight_scale = None;
                    self.fullpage_started = None;
                }
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint();
                }
                Err(TryRecvError::Disconnected) => {
                    self.fullpage_receiver = None;
                    self.fullpage_inflight_scale = None;
                    self.fullpage_started = None;
                }
            }
        }

        // 2. Poll completed viewport render
        if let Some(ref rx) = self.render_receiver {
            match rx.try_recv() {
                Ok(result) => {
                    let page_idx = result.params.page_idx;
                    if page_idx < self.pages.len() {
                        let image = ColorImage::from_rgba_unmultiplied(
                            [
                                result.params.pixel_w as usize,
                                result.params.pixel_h as usize,
                            ],
                            &result.rgba,
                        );
                        let texture = ctx.load_texture(
                            format!("viewport_p{}", self.pages[page_idx].page_num),
                            image,
                            TextureOptions::LINEAR,
                        );
                        self.pages[page_idx].cached_render = Some(CachedRender {
                            texture,
                            vp_x: result.params.vp_x,
                            vp_y: result.params.vp_y,
                            vp_w: result.params.vp_w,
                            vp_h: result.params.vp_h,
                            pixel_w: result.params.pixel_w,
                            pixel_h: result.params.pixel_h,
                        });
                    }
                    self.render_receiver = None;
                    self.inflight_params = None;
                    // Viewport render done — spawn full-page render in background
                    self.spawn_fullpage_render(ctx, available, ppp);
                    // If viewport moved since we spawned, handle it
                    if let Some((vp, _)) = self.compute_viewport_params(available, ppp) {
                        if !self.cache_matches(&vp) {
                            self.render_dirty = true;
                            self.request_render();
                        } else {
                            self.render_dirty = false;
                        }
                    }
                }
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint();
                }
                Err(TryRecvError::Disconnected) => {
                    self.render_receiver = None;
                    self.inflight_params = None;
                }
            }
        }

        // 3. Handle dirty viewport
        if !self.render_dirty {
            // Not dirty, but maybe we need a full-page render
            self.spawn_fullpage_render(ctx, available, ppp);
            return;
        }

        // Ensure we have a debounce timestamp — if render_dirty is set but
        // render_requested_at was cleared (e.g. by a prior spawn), reset it.
        if self.render_requested_at.is_none() {
            self.render_requested_at = Some(Instant::now());
        }
        let requested_at = self.render_requested_at.unwrap();

        let Some((vp, dpi)) = self.compute_viewport_params(available, ppp) else {
            return;
        };

        // Already cached?
        if self.cache_matches(&vp) {
            self.render_dirty = false;
            self.render_requested_at = None;
            self.spawn_fullpage_render(ctx, available, ppp);
            return;
        }

        // Try instant blit from full-page buffer (covers panning at same zoom)
        if self.try_blit_from_fullpage(ctx, &vp, available, ppp) {
            return;
        }

        // Debounce: wait until input has settled before spawning a render
        if requested_at.elapsed() < RENDER_DEBOUNCE {
            ctx.request_repaint_after(RENDER_DEBOUNCE - requested_at.elapsed());
            return;
        }

        // Already rendering the same viewport?
        if let Some(ref inflight) = self.inflight_params
            && *inflight == vp
        {
            return;
        }

        // Don't pile up viewport renders
        if self.render_receiver.is_some() {
            return;
        }

        // Invalidate full-page buffer if zoom changed
        {
            let fit = self.fit_scale(available, &self.pages[vp.page_idx]);
            let effective_scale = fit * self.zoom;
            if let Some(ref fp) = self.pages[vp.page_idx].full_page {
                if (fp.effective_scale - effective_scale).abs() > 0.001
                    || (fp.ppp - ppp).abs() > 0.001
                {
                    self.pages[vp.page_idx].full_page = None;
                }
            }
        }

        // Spawn async viewport render (cancellable)
        let page = &self.pages[vp.page_idx];
        let dl = Arc::clone(&page.display_list);
        let prep = Arc::clone(&page.prepared);
        let icc = Arc::clone(&page.icc_cache);
        let img_cache = Arc::clone(&page.image_cache);
        let no_aa = self.no_aa;
        let vp_clone = vp.clone();
        let cancel = Arc::clone(&self.render_cancel);

        let (tx, rx) = std::sync::mpsc::channel();
        self.render_receiver = Some(rx);
        self.inflight_params = Some(vp);
        self.render_requested_at = None;

        let egui_ctx = ctx.clone();
        rayon::spawn(move || {
            let result = stet_render::render_region_prepared_parallel_cancellable(
                &dl, &prep, vp_clone.vp_x, vp_clone.vp_y, vp_clone.vp_w, vp_clone.vp_h,
                vp_clone.pixel_w, vp_clone.pixel_h, dpi, Some(&icc), Some(&img_cache), no_aa,
                &cancel,
            );
            let Some(rgba) = result else {
                // Cancelled — don't send result
                egui_ctx.request_repaint();
                return;
            };
            let _ = tx.send(AsyncRenderResult {
                rgba,
                params: vp_clone,
            });
            egui_ctx.request_repaint();
        });

        ctx.request_repaint();
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

        let rgba = stet_render::render_region_prepared_parallel(
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
            Some(&page.image_cache),
            self.no_aa,
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
            self.request_render();
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
            // Drop all channels so interpreter/relay threads unblock and exit
            self.page_receiver = None;
            self.screen_info_sender = None;
            self.advance_sender = None;
            self.file_drop_sender = None;
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
                        let total = if self.interpreter_done || self.job_done {
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

        // Handle file drops — send to interpreter for processing
        let dropped: Vec<_> = ctx.input(|i| {
            i.raw.dropped_files
                .iter()
                .filter_map(|f| {
                    f.path.as_ref().map(|p| p.to_string_lossy().to_string())
                })
                .collect()
        });
        if !dropped.is_empty() {
            // Update window title to the dropped file name
            if let Some(last) = dropped.last() {
                let base = std::path::Path::new(last)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| last.clone());
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!("stet — {}", base)));
            }
            if let Some(ref sender) = self.file_drop_sender {
                for path in dropped {
                    let _ = sender.send(path);
                }
            }
            // Reset interpreter_done so we accept new pages
            self.interpreter_done = false;
        }

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
                self.request_render();
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
                self.request_render();
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
                        self.request_render();
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
                self.request_render();
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
                    self.request_render();
                }
                if i.key_pressed(egui::Key::ArrowDown) {
                    self.pan_offset.y -= 50.0;
                    self.render_dirty = true;
                    self.request_render();
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
                            self.request_render();
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
                        let msg = if self.job_done {
                            "No pages rendered. Drop a file to open it."
                        } else {
                            "Waiting for first page to render..."
                        };
                        ui.label(msg);
                    });
                    return;
                }

                // Resize window to fit current page if needed
                if self.needs_resize {
                    self.needs_resize = false;
                    self.size_window_to_page(ctx);
                }

                let available = ui.available_size();
                self.central_available = available;
                let page_idx = self.current_page;

                // Detect window resize — re-render if available area changed
                if (available - self.last_available).length_sq() > 1.0 {
                    self.render_dirty = true;
                    self.request_render();
                    self.last_available = available;
                }

                // Process async rendering (poll results, spawn new renders)
                self.process_async_render(ctx, available, ppp);

                // Draw the rendered viewport
                let page = &self.pages[page_idx];
                let fit = self.fit_scale(available, page);
                let effective_scale = fit * self.zoom;
                let img_w = page.width as f32 * effective_scale;
                let img_h = page.height as f32 * effective_scale;

                if let Some(ref cached) = page.cached_render {
                    // New page has a rendered viewport — clear transition texture
                    self.transition_texture = None;

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
                } else if let Some(ref tex) = self.transition_texture {
                    // New page not yet rendered — show previous page's texture
                    // centered at fit-to-window size to avoid black flash
                    let center_x = ((available.x - img_w) / 2.0).max(0.0);
                    let center_y = ((available.y - img_h) / 2.0).max(0.0);
                    let origin = ui.min_rect().min;
                    let img_origin = origin + egui::vec2(center_x, center_y);
                    let fill_rect =
                        egui::Rect::from_min_size(img_origin, egui::vec2(img_w, img_h));
                    ui.painter().image(
                        tex.id(),
                        fill_rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                }

                // Pulsing indicator while rendering in background
                // (only shown after 1 second to avoid flashing on fast renders)
                let rendering_in_background = self.fullpage_receiver.is_some()
                    || self.render_receiver.is_some();
                let render_start = self.fullpage_started.or(self.render_requested_at);
                if rendering_in_background
                    && render_start.is_some_and(|t| t.elapsed() >= Duration::from_secs(1))
                {
                    let t = ctx.input(|i| i.time) as f32;
                    let on = (t % 1.0) < 0.5;
                    let alpha = if on { 255u8 } else { 0u8 };
                    let bar_h = 6.0;
                    let origin = ui.min_rect().min;
                    let bar_rect = egui::Rect::from_min_size(
                        egui::pos2(origin.x, origin.y + available.y - bar_h),
                        egui::vec2(available.x, bar_h),
                    );
                    ui.painter().rect_filled(
                        bar_rect,
                        0.0,
                        egui::Color32::from_rgba_unmultiplied(255, 50, 50, alpha),
                    );
                    ctx.request_repaint_after(Duration::from_millis(50));
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
