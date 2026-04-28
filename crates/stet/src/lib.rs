// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Batteries-included PostScript Level 3 interpreter.
//!
//! This crate provides a simple, high-level API for rendering PostScript and
//! EPS files. All resources (35 fonts, init scripts, encodings, ICC profiles)
//! are embedded in the binary — no external `resources/` directory needed.
//!
//! # Quick Start
//!
//! ```no_run
//! let mut interp = stet::Interpreter::new();
//! let pages = interp.render(b"%!PS\n100 100 moveto 200 200 lineto stroke showpage", 300.0).unwrap();
//! // pages[0].rgba — RGBA pixel data
//! // pages[0].width, pages[0].height — dimensions in pixels
//! ```
//!
//! # Output Modes
//!
//! | Method | Returns | Feature |
//! |--------|---------|---------|
//! | [`Interpreter::render`] | RGBA pixels + display list | `render` (default) |
//! | [`Interpreter::render_to_display_list`] | Display lists only | always available |
//! | [`Interpreter::render_to_pdf`] | PDF bytes | `pdf-output` (default) |
//! | [`Interpreter::exec`] | Nothing (side effects only) | always available |
//!
//! # Configuration
//!
//! ```no_run
//! let mut interp = stet::Interpreter::builder()
//!     .no_icc()             // disable ICC color management
//!     .suppress_output()    // silence PS print/==/= operators
//!     .build();
//! ```
//!
//! # Feature Flags
//!
//! Both features are enabled by default. Use `default-features = false` for
//! a minimal build that only supports display list output.
//!
//! | Feature | Adds | Extra dependency |
//! |---------|------|-----------------|
//! | `render` | `render()` — RGBA pixel output | `stet-render` (tiny-skia) |
//! | `pdf-output` | `render_to_pdf()` — PDF output | `stet-pdf` |

pub mod embedded_resources;
mod init;

use std::sync::{Arc, Mutex};

use stet_core::context::Context;
use stet_core::device::OutputDevice;
use stet_core::eps::{content_is_epsf, read_eps_bounding_box, strip_dos_eps_header};
use stet_core::error::PsError;
use stet_core::object::PsValue;
use stet_engine::eval::parse_and_exec;
use stet_graphics::display_list::DisplayList;

// Re-exports for power users
pub use stet_core::context::Context as PsContext;
pub use stet_engine::eval::parse_and_exec as ps_exec;
pub use stet_graphics::display_list::{DisplayElement, DisplayList as PsDisplayList};
pub use stet_graphics::icc::IccCache;

#[cfg(feature = "render")]
pub use stet_render::{
    ImageCache, PreparedDisplayList, build_icc_cache_for_list, prepare_display_list,
    render_region_prepared, render_to_rgba, viewport_band_count,
};

/// Error type for interpreter operations.
#[derive(Debug)]
pub enum StetError {
    /// PostScript execution error.
    PostScript(String),
    /// Initialization error.
    Init(String),
}

impl std::fmt::Display for StetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StetError::PostScript(msg) => write!(f, "PostScript error: {}", msg),
            StetError::Init(msg) => write!(f, "initialization error: {}", msg),
        }
    }
}

impl std::error::Error for StetError {}

/// A rendered page with display list and optional RGBA pixel data.
pub struct RenderedPage {
    /// The display list for this page (for custom/viewport rendering).
    pub display_list: DisplayList,
    /// Page width in pixels at the rendered DPI.
    pub width: u32,
    /// Page height in pixels at the rendered DPI.
    pub height: u32,
    /// DPI this page was rendered at.
    pub dpi: f64,
    /// RGBA pixel data (4 bytes per pixel, row-major).
    /// Present only when rendered via [`Interpreter::render`].
    #[cfg(feature = "render")]
    pub rgba: Vec<u8>,
}

/// A fully initialized PostScript interpreter.
///
/// Create via [`Interpreter::new`] for defaults or [`Interpreter::builder`]
/// for custom configuration. Reusable across multiple `render` calls.
pub struct Interpreter {
    ctx: Context,
    use_icc: bool,
}

/// Builder for configuring an [`Interpreter`] before creation.
pub struct InterpreterBuilder {
    use_icc: bool,
    suppress_output: bool,
}

impl Interpreter {
    /// Create a fully initialized interpreter with default settings.
    ///
    /// Includes embedded fonts, ICC color management, and all standard
    /// PostScript resources. Ready to render immediately.
    pub fn new() -> Self {
        Self::builder().build()
    }

    /// Create a builder for custom configuration.
    pub fn builder() -> InterpreterBuilder {
        InterpreterBuilder {
            use_icc: true,
            suppress_output: false,
        }
    }

    /// Render PostScript or EPS data to RGBA pages.
    ///
    /// `dpi` is the resolution in dots per inch (e.g., `300.0`). It is `f64`
    /// because PostScript defines HWResolution as a pair of reals; integer
    /// values like `300.0` work for typical use.
    ///
    /// Each `showpage` in the PostScript program produces one [`RenderedPage`]
    /// with RGBA pixel data and the display list. EPS files are detected
    /// automatically and rendered using their bounding box.
    ///
    /// The interpreter state is isolated via save/restore between calls.
    #[cfg(feature = "render")]
    pub fn render(&mut self, ps_data: &[u8], dpi: f64) -> Result<Vec<RenderedPage>, StetError> {
        let dl_pages = self.render_to_display_list(ps_data, dpi)?;

        let icc_cache = if self.use_icc {
            let mut cache = IccCache::new();
            cache.load_cmyk_profile_bytes(embedded_resources::DEFAULT_CMYK_ICC);
            Some(cache)
        } else {
            None
        };

        let mut pages = Vec::with_capacity(dl_pages.len());
        for p in dl_pages {
            let rgba = stet_render::render_to_rgba(
                &p.display_list,
                p.width,
                p.height,
                p.dpi,
                icc_cache.as_ref(),
                false,
            );
            pages.push(RenderedPage {
                display_list: p.display_list,
                width: p.width,
                height: p.height,
                dpi: p.dpi,
                rgba,
            });
        }
        Ok(pages)
    }

    /// Render PostScript or EPS data to display lists only (no pixel rendering).
    ///
    /// `dpi` is the resolution in dots per inch (e.g., `300.0`). It is `f64`
    /// because PostScript defines HWResolution as a pair of reals; integer
    /// values like `300.0` work for typical use.
    ///
    /// Returns one entry per `showpage`. Use this when you want to do your
    /// own rendering (e.g., viewport rendering) or just inspect the display list.
    pub fn render_to_display_list(
        &mut self,
        ps_data: &[u8],
        dpi: f64,
    ) -> Result<Vec<DisplayListPage>, StetError> {
        let ps_data = strip_dos_eps_header(ps_data);
        let is_eps = content_is_epsf(ps_data);

        if is_eps {
            if let Some((llx, lly, urx, ury)) = read_eps_bounding_box(ps_data) {
                let w = urx - llx;
                let h = ury - lly;
                if w > 0.0 && h > 0.0 {
                    return self.render_eps(ps_data, dpi, llx, lly, w, h);
                }
            }
        }

        self.render_ps(ps_data, dpi, 612.0, 792.0)
    }

    /// Render PostScript data to a PDF document.
    ///
    /// `dpi` is the resolution in dots per inch (e.g., `300.0`). It is `f64`
    /// because PostScript defines HWResolution as a pair of reals; integer
    /// values like `300.0` work for typical use.
    ///
    /// Returns the PDF file contents as bytes.
    #[cfg(feature = "pdf-output")]
    pub fn render_to_pdf(&mut self, ps_data: &[u8], dpi: f64) -> Result<Vec<u8>, StetError> {
        let ps_data = strip_dos_eps_header(ps_data);
        let is_eps = content_is_epsf(ps_data);

        let (page_w, page_h) = if is_eps {
            if let Some((llx, lly, urx, ury)) = read_eps_bounding_box(ps_data) {
                (urx - llx, ury - lly)
            } else {
                (612.0, 792.0)
            }
        } else {
            (612.0, 792.0)
        };

        // Set up PdfDevice as the device factory
        let dpi_val = dpi;
        self.ctx.device_factory = Some(Box::new(move |w, h| {
            Box::new(stet_pdf::PdfDevice::new(w, h, dpi_val)) as Box<dyn OutputDevice>
        }));

        self.ctx.output_path = Some("output.pdf".to_string());
        install_device(&mut self.ctx, dpi, page_w, page_h)?;

        // PDF output: register pdfmark + distiller params so prologues that
        // branch on `systemdict /pdfmark known` see Distiller-equivalent
        // semantics. The screen/viewer path leaves these undefined so the
        // same prologue takes its CMYK→ICC branch instead.
        stet_ops::register_pdf_authoring_ops(&mut self.ctx);

        // Save/restore isolation
        let save_obj = self.ctx.vm_save();
        let save_id = extract_save_id(&save_obj);

        let exec_result = if is_eps {
            if let Some((llx, lly, _, _)) = read_eps_bounding_box(ps_data) {
                self.exec_eps(ps_data, llx, lly)
            } else {
                parse_and_exec(&mut self.ctx, ps_data).map_err(ps_err)
            }
        } else {
            parse_and_exec(&mut self.ctx, ps_data).map_err(ps_err)
        };

        // Finish device and extract PDF bytes
        let pdf_bytes = if let Some(mut dev) = self.ctx.device.take() {
            let _ = dev.finish_with_context(&self.ctx);
            // Get the PDF bytes from the device
            let bytes = dev
                .as_any()
                .downcast_ref::<stet_pdf::PdfDevice>()
                .and_then(|pdf_dev| pdf_dev.take_pdf_bytes_with_context(&self.ctx))
                .unwrap_or_default();
            self.ctx.device = Some(dev);
            bytes
        } else {
            Vec::new()
        };

        let _ = self.ctx.vm_restore(save_id);
        reset_context(&mut self.ctx);

        exec_result?;
        Ok(pdf_bytes)
    }

    /// Execute PostScript without rendering (null device).
    ///
    /// Useful for running test suites or scripts that don't produce pages.
    pub fn exec(&mut self, ps_data: &[u8]) -> Result<(), StetError> {
        let save_obj = self.ctx.vm_save();
        let save_id = extract_save_id(&save_obj);

        let result = parse_and_exec(&mut self.ctx, ps_data);

        let _ = self.ctx.vm_restore(save_id);
        reset_context(&mut self.ctx);

        match result {
            Ok(()) | Err(PsError::Quit) => Ok(()),
            Err(e) => Err(StetError::PostScript(e.to_string())),
        }
    }

    /// Access the underlying Context for power-user operations.
    pub fn context(&mut self) -> &mut Context {
        &mut self.ctx
    }

    // --- Internal rendering helpers ---

    fn render_ps(
        &mut self,
        ps_data: &[u8],
        dpi: f64,
        page_w_pt: f64,
        page_h_pt: f64,
    ) -> Result<Vec<DisplayListPage>, StetError> {
        self.ctx.capture_display_lists = Some(Vec::new());

        #[cfg(feature = "render")]
        let pages_ref = {
            let (pages_ref, _) = shared_page_tracker();
            self.setup_capture_device_factory(pages_ref.clone());
            pages_ref
        };
        #[cfg(not(feature = "render"))]
        {
            self.setup_capture_device_factory(Arc::new(Mutex::new(Vec::<()>::new())));
        }

        self.ctx.output_path = Some("stet_output".to_string());
        install_device(&mut self.ctx, dpi, page_w_pt, page_h_pt)?;

        let save_obj = self.ctx.vm_save();
        let save_id = extract_save_id(&save_obj);

        let exec_result = match parse_and_exec(&mut self.ctx, ps_data) {
            Ok(()) => Ok(()),
            Err(PsError::Quit) => Ok(()),
            Err(e) => Err(StetError::PostScript(e.to_string())),
        };

        finish_device(&mut self.ctx);

        #[cfg(feature = "render")]
        let result = {
            let pages = take_pages(&pages_ref);
            collect_display_lists(&mut self.ctx, &pages, dpi)
        };
        #[cfg(not(feature = "render"))]
        let result = collect_display_lists_simple(&mut self.ctx, dpi);

        let _ = self.ctx.vm_restore(save_id);
        reset_context(&mut self.ctx);

        exec_result?;
        Ok(result)
    }

    fn render_eps(
        &mut self,
        ps_data: &[u8],
        dpi: f64,
        llx: f64,
        lly: f64,
        w: f64,
        h: f64,
    ) -> Result<Vec<DisplayListPage>, StetError> {
        self.ctx.capture_display_lists = Some(Vec::new());

        #[cfg(feature = "render")]
        let pages_ref = {
            let (pages_ref, _) = shared_page_tracker();
            self.setup_capture_device_factory(pages_ref.clone());
            pages_ref
        };
        #[cfg(not(feature = "render"))]
        {
            self.setup_capture_device_factory(Arc::new(Mutex::new(Vec::<()>::new())));
        }

        self.ctx.output_path = Some("stet_output".to_string());
        install_device(&mut self.ctx, dpi, w, h)?;

        let save_obj = self.ctx.vm_save();
        let save_id = extract_save_id(&save_obj);

        let wrapper = format!("gsave {} {} translate", -llx, -lly);
        parse_and_exec(&mut self.ctx, wrapper.as_bytes()).map_err(ps_err)?;
        let _ = parse_and_exec(&mut self.ctx, ps_data);

        // Call showpage if the EPS didn't already
        #[cfg(feature = "render")]
        let need_showpage = pages_ref.lock().map(|g| g.is_empty()).unwrap_or(true);
        #[cfg(not(feature = "render"))]
        let need_showpage = self
            .ctx
            .capture_display_lists
            .as_ref()
            .map_or(true, |v| v.is_empty());

        if need_showpage {
            let _ = parse_and_exec(&mut self.ctx, b"grestore showpage");
        } else {
            let _ = parse_and_exec(&mut self.ctx, b"grestore");
        }

        finish_device(&mut self.ctx);

        #[cfg(feature = "render")]
        let result = {
            let pages = take_pages(&pages_ref);
            collect_display_lists(&mut self.ctx, &pages, dpi)
        };
        #[cfg(not(feature = "render"))]
        let result = collect_display_lists_simple(&mut self.ctx, dpi);

        let _ = self.ctx.vm_restore(save_id);
        reset_context(&mut self.ctx);

        Ok(result)
    }

    #[cfg(feature = "pdf-output")]
    fn exec_eps(&mut self, ps_data: &[u8], llx: f64, lly: f64) -> Result<(), StetError> {
        let wrapper = format!("gsave {} {} translate", -llx, -lly);
        parse_and_exec(&mut self.ctx, wrapper.as_bytes()).map_err(ps_err)?;
        let _ = parse_and_exec(&mut self.ctx, ps_data);
        let _ = parse_and_exec(&mut self.ctx, b"grestore showpage");
        Ok(())
    }

    #[cfg(feature = "render")]
    fn setup_capture_device_factory(&mut self, pages_ref: Arc<Mutex<Vec<PageDims>>>) {
        let use_icc = self.use_icc;
        self.ctx.device_factory = Some(Box::new(move |w, h| {
            let factory = NullSinkFactory(pages_ref.clone());
            let mut dev = stet_render::SkiaDevice::with_sink_factory(w, h, Box::new(factory));
            if use_icc {
                dev.set_system_cmyk_bytes(Arc::new(embedded_resources::DEFAULT_CMYK_ICC.to_vec()));
            }
            Box::new(dev) as Box<dyn OutputDevice>
        }));
    }

    #[cfg(not(feature = "render"))]
    fn setup_capture_device_factory<T>(&mut self, _pages_ref: Arc<Mutex<Vec<T>>>) {
        self.ctx.device_factory = Some(Box::new(|w, h| {
            Box::new(stet_core::device::NullDevice::new(w, h))
        }));
    }
}

impl Default for Interpreter {
    fn default() -> Self {
        Self::new()
    }
}

impl InterpreterBuilder {
    /// Disable ICC color management.
    pub fn no_icc(mut self) -> Self {
        self.use_icc = false;
        self
    }

    /// Suppress PostScript stdout (`print`, `=`, `==` operators).
    pub fn suppress_output(mut self) -> Self {
        self.suppress_output = true;
        self
    }

    /// Build the interpreter.
    pub fn build(self) -> Interpreter {
        let ctx = init::create_initialized_context(self.use_icc, self.suppress_output)
            .expect("interpreter initialization failed");
        Interpreter {
            ctx,
            use_icc: self.use_icc,
        }
    }
}

// --- Page dimension tracking (NullSink) ---

/// Recorded page dimensions from the null sink.
#[cfg(feature = "render")]
struct PageDims {
    width: u32,
    height: u32,
}

/// A PageSinkFactory that discards pixels but records page dimensions.
#[cfg(feature = "render")]
struct NullSinkFactory(Arc<Mutex<Vec<PageDims>>>);

#[cfg(feature = "render")]
unsafe impl Send for NullSinkFactory {}
#[cfg(feature = "render")]
unsafe impl Sync for NullSinkFactory {}

#[cfg(feature = "render")]
impl stet_graphics::device::PageSinkFactory for NullSinkFactory {
    fn create_sink(
        &self,
        _output_path: &str,
    ) -> Result<Box<dyn stet_graphics::device::PageSink>, String> {
        // We don't know the pixel dimensions here — they'll be set by begin_page.
        // Use 0x0 as placeholders; the actual dimensions come from the device.
        let pages = self.0.clone();
        Ok(Box::new(NullSink {
            width: 0,
            height: 0,
            pages,
        }))
    }
}

#[cfg(feature = "render")]
struct NullSink {
    width: u32,
    height: u32,
    pages: Arc<Mutex<Vec<PageDims>>>,
}

#[cfg(feature = "render")]
unsafe impl Send for NullSink {}

#[cfg(feature = "render")]
impl stet_graphics::device::PageSink for NullSink {
    fn begin_page(&mut self, width: u32, height: u32) -> Result<(), String> {
        self.width = width;
        self.height = height;
        Ok(())
    }
    fn write_rows(&mut self, _rows: &[u8], _num_rows: u32) -> Result<(), String> {
        Ok(())
    }
    fn end_page(&mut self) -> Result<(), String> {
        if let Ok(mut guard) = self.pages.lock() {
            guard.push(PageDims {
                width: self.width,
                height: self.height,
            });
        }
        Ok(())
    }
}

/// A page's display list with dimensions (before RGBA rendering).
pub struct DisplayListPage {
    /// The display list for this page.
    pub display_list: DisplayList,
    /// Page width in pixels at the rendered DPI.
    pub width: u32,
    /// Page height in pixels at the rendered DPI.
    pub height: u32,
    /// DPI this page was rendered at.
    pub dpi: f64,
}

// --- Helpers ---

#[cfg(feature = "render")]
fn shared_page_tracker() -> (Arc<Mutex<Vec<PageDims>>>, ()) {
    (Arc::new(Mutex::new(Vec::new())), ())
}

#[cfg(feature = "render")]
fn take_pages(pages_ref: &Arc<Mutex<Vec<PageDims>>>) -> Vec<PageDims> {
    match pages_ref.lock() {
        Ok(mut guard) => std::mem::take(&mut *guard),
        Err(e) => std::mem::take(&mut *e.into_inner()),
    }
}

fn install_device(
    ctx: &mut Context,
    dpi: f64,
    width_pt: f64,
    height_pt: f64,
) -> Result<(), StetError> {
    let setup = format!(
        "<< /PageSize [{w} {h}] /HWResolution [{dpi} {dpi}] \
         /.IsPageDevice true \
         /Install {{ /DeviceRGB setcolorspace }} bind \
         /BeginPage {{pop}} bind \
         /EndPage {{ \
             dup 0 eq {{ pop pop true }} {{ \
                 1 eq {{ pop true }} {{ pop false }} ifelse \
             }} ifelse \
         }} bind \
         /PageCount 0 \
        >> setpagedevice",
        w = width_pt,
        h = height_pt,
        dpi = dpi
    );
    // The caller-supplied DPI is an explicit library-level request, not
    // an unsolicited change from within the PS program. Open the gate
    // around setpagedevice so merge_request_dict accepts HWResolution.
    let saved = ctx.allow_ps_resolution;
    ctx.allow_ps_resolution = true;
    let result = parse_and_exec(ctx, setup.as_bytes()).map_err(ps_err);
    ctx.allow_ps_resolution = saved;
    result
}

fn finish_device(ctx: &mut Context) {
    if let Some(mut dev) = ctx.device.take() {
        let _ = dev.finish_with_context(ctx);
        ctx.device = Some(dev);
    }
}

#[cfg(feature = "render")]
fn collect_display_lists(
    ctx: &mut Context,
    pages: &[PageDims],
    default_dpi: f64,
) -> Vec<DisplayListPage> {
    let captured = ctx.capture_display_lists.take().unwrap_or_default();
    captured
        .into_iter()
        .enumerate()
        .map(|(i, (dl, dpi))| {
            let dpi = if dpi > 0.0 { dpi } else { default_dpi };
            if i < pages.len() {
                DisplayListPage {
                    display_list: dl,
                    width: pages[i].width,
                    height: pages[i].height,
                    dpi,
                }
            } else {
                DisplayListPage {
                    display_list: dl,
                    width: (612.0 * dpi / 72.0) as u32,
                    height: (792.0 * dpi / 72.0) as u32,
                    dpi,
                }
            }
        })
        .collect()
}

/// Collect display lists without page dimension tracking (no-render fallback).
#[cfg(not(feature = "render"))]
fn collect_display_lists_simple(ctx: &mut Context, default_dpi: f64) -> Vec<DisplayListPage> {
    let captured = ctx.capture_display_lists.take().unwrap_or_default();
    captured
        .into_iter()
        .map(|(dl, dpi)| {
            let dpi = if dpi > 0.0 { dpi } else { default_dpi };
            DisplayListPage {
                display_list: dl,
                width: (612.0 * dpi / 72.0) as u32,
                height: (792.0 * dpi / 72.0) as u32,
                dpi,
            }
        })
        .collect()
}

fn reset_context(ctx: &mut Context) {
    ctx.device = None;
    ctx.output_path = None;
    ctx.display_list.clear();
    ctx.capture_display_lists = None;
    ctx.o_stack.clear();
    ctx.e_stack.clear();
    ctx.gstate = stet_core::graphics_state::GraphicsState::new();
    ctx.gstate_stack.clear();
    ctx.d_stack.truncate(3);
    ctx.save_stack = stet_core::save_stack::SaveStack::new();
    ctx.in_error_handler = false;
}

fn extract_save_id(save_obj: &stet_core::object::PsObject) -> u32 {
    match save_obj.value {
        PsValue::Save(stet_core::object::SaveLevel(id)) => id,
        _ => unreachable!(),
    }
}

fn ps_err(e: PsError) -> StetError {
    StetError::PostScript(e.to_string())
}
