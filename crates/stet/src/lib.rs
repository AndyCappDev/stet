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
//! For artwork placed over other content, leave unpainted areas clear
//! (straight-alpha RGBA) instead of white:
//!
//! ```no_run
//! # #[cfg(feature = "render")] {
//! let mut interp = stet::Interpreter::builder()
//!     .page_background(stet::PageBackground::Transparent)
//!     .build();
//! # }
//! ```
//!
//! # Diagnostics
//!
//! A render that returns no pages is not necessarily an error. The common
//! cause is a program that painted marks and then ended without calling
//! `showpage`: the page is discarded, and the render returns `Ok(vec![])` —
//! an empty page list that reads like a legitimate result.
//! [`Interpreter::warnings`] tells the two apart.
//!
//! ```no_run
//! let mut interp = stet::Interpreter::new();
//! let pages = interp.render(b"%!PS\n0 0 100 100 rectfill", 72.0).unwrap();
//! if pages.is_empty() {
//!     for w in interp.warnings() {
//!         eprintln!("warning: {}\n         {}", w, w.hint());
//!     }
//! }
//! ```
//!
//! See [`diagnostics`] for the warning types.
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

pub mod diagnostics;
pub mod embedded_resources;
mod init;

use std::sync::{Arc, Mutex};

use stet_core::context::Context;
#[cfg(any(feature = "render", feature = "pdf-output"))]
use stet_core::device::OutputDevice;
use stet_core::eps::{content_is_epsf, read_eps_bounding_box, strip_dos_eps_header};
use stet_core::error::PsError;
use stet_core::object::PsValue;
use stet_engine::eval::parse_and_exec;
use stet_graphics::display_list::DisplayList;

pub use diagnostics::{ExecWarning, ExecWarningKind};

// Re-exports for power users
pub use stet_core::context::Context as PsContext;
pub use stet_engine::eval::parse_and_exec as ps_exec;
pub use stet_graphics::device::{ShownGlyph, TextExtraction, TextRunParams, UnicodeSource};
pub use stet_graphics::display_list::{DisplayElement, DisplayList as PsDisplayList};
pub use stet_graphics::icc::IccCache;
pub use stet_graphics::layer_set::LayerSet;
pub use stet_graphics::rendering_intent::RenderingIntent;
pub use stet_graphics::text::{TextLine, TextWord, text_lines, text_runs};

#[cfg(feature = "render")]
pub use stet_render::{
    ImageCache, PageBackground, PreparedDisplayList, RegionRender, build_icc_cache_for_list,
    build_icc_cache_for_list_with_bpc, prepare_display_list, render_region_prepared,
    render_to_rgba, render_to_rgba_with_background, viewport_band_count,
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
    ///
    /// Opaque on white paper by default; straight (non-premultiplied)
    /// alpha when built with
    /// [`page_background(PageBackground::Transparent)`](InterpreterBuilder::page_background).
    #[cfg(feature = "render")]
    pub rgba: Vec<u8>,
}

/// A fully initialized PostScript interpreter.
///
/// Create via [`Interpreter::new`] for defaults or [`Interpreter::builder`]
/// for custom configuration. Reusable across multiple `render` calls.
pub struct Interpreter {
    ctx: Context,
    #[cfg(feature = "render")]
    page_background: PageBackground,
    warnings: Vec<ExecWarning>,
}

/// Builder for configuring an [`Interpreter`] before creation.
pub struct InterpreterBuilder {
    use_icc: bool,
    suppress_output: bool,
    text_extraction: TextExtraction,
    default_rendering_intent: RenderingIntent,
    #[cfg(feature = "render")]
    page_background: PageBackground,
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
            text_extraction: TextExtraction::Off,
            default_rendering_intent: RenderingIntent::RelativeColorimetric,
            #[cfg(feature = "render")]
            page_background: PageBackground::White,
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
    ///
    /// Colour is converted through the context's [`IccCache`]
    /// (`context().icc_cache`), with its CMYK profile and black-point
    /// compensation — the colours converted as the page renders, such as
    /// CMYK images and overprint, as well as those converted as it is built.
    #[cfg(feature = "render")]
    pub fn render(&mut self, ps_data: &[u8], dpi: f64) -> Result<Vec<RenderedPage>, StetError> {
        let dl_pages = self.render_to_display_list(ps_data, dpi)?;

        let mut pages = Vec::with_capacity(dl_pages.len());
        for p in dl_pages {
            // Images and overprint are converted as the page renders. The
            // cache that does it is set up like the context's, which built
            // the display list, or they would disagree with the fills
            // beside them once a caller reconfigures `context().icc_cache`.
            let bake = &self.ctx.icc_cache;
            let icc_cache = build_icc_cache_for_list_with_bpc(
                &p.display_list,
                bake.system_cmyk_bytes(),
                false,
                bake.bpc_mode(),
            );
            let rgba = stet_render::render_to_rgba_with_background(
                &p.display_list,
                p.width,
                p.height,
                p.dpi,
                Some(&icc_cache),
                false,
                &LayerSet::new(),
                self.page_background,
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
        self.warnings.clear();
        let ps_data = strip_dos_eps_header(ps_data);
        let is_eps = content_is_epsf(ps_data);

        if is_eps && let Some((llx, lly, urx, ury)) = read_eps_bounding_box(ps_data) {
            let w = urx - llx;
            let h = ury - lly;
            if w > 0.0 && h > 0.0 {
                return self.render_eps(ps_data, dpi, llx, lly, w, h);
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
        self.warnings.clear();
        self.ctx.null_device_used = false;
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
            Box::new(stet_pdf::PdfDevice::in_memory(w, h, dpi_val)) as Box<dyn OutputDevice>
        }));

        // Routes showpage to the device; the in-memory device writes no file
        // and ignores the name.
        self.ctx.output_path = Some("output.pdf".to_string());
        let save_id = self.begin_job(dpi, page_w, page_h)?;

        // PDF output: register pdfmark + distiller params so prologues that
        // branch on `systemdict /pdfmark known` see Distiller-equivalent
        // semantics. The screen/viewer path leaves these undefined so the
        // same prologue takes its CMYK→ICC branch instead.
        stet_ops::register_pdf_authoring_ops(&mut self.ctx);

        let exec_result = if is_eps {
            if let Some((llx, lly, _, _)) = read_eps_bounding_box(ps_data) {
                self.exec_eps(ps_data, llx, lly)
            } else {
                let result = parse_and_exec(&mut self.ctx, ps_data);
                job_result(&self.ctx, result)
            }
        } else {
            let result = parse_and_exec(&mut self.ctx, ps_data);
            job_result(&self.ctx, result)
        };
        let exec_result = exec_result.and_then(|()| self.end_page_device());

        self.record_end_of_job_warnings();

        // Build the document once, as bytes: the device writes no file
        let pdf_bytes = if let Some(dev) = self.ctx.device.take() {
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

        end_job(&mut self.ctx, save_id);
        // `systemdict` is global, so the restore left them in; a later
        // screen render must not see `pdfmark`.
        stet_ops::remove_pdf_authoring_ops(&mut self.ctx);

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
        let result = job_result(&self.ctx, result);

        end_job(&mut self.ctx, save_id);

        result
    }

    /// Non-fatal problems noticed during the most recent render call.
    ///
    /// Cleared at the start of each [`render`], [`render_to_display_list`],
    /// and [`render_to_pdf`] call, so this always describes the latest one.
    ///
    /// The case worth checking for is a program that paints marks and never
    /// calls `showpage`: the page is discarded, the render returns an empty
    /// page list, and without this there is nothing to distinguish that from
    /// a program that legitimately drew nothing.
    ///
    /// ```no_run
    /// let mut interp = stet::Interpreter::new();
    /// let pages = interp.render(b"%!PS\n0 0 100 100 rectfill", 72.0).unwrap();
    /// if pages.is_empty() {
    ///     for w in interp.warnings() {
    ///         eprintln!("warning: {}\n         {}", w, w.hint());
    ///     }
    /// }
    /// ```
    ///
    /// [`exec`] is not covered: it runs on a null device for side effects
    /// only, where pending marks are expected rather than a problem.
    ///
    /// [`render`]: Interpreter::render
    /// [`render_to_display_list`]: Interpreter::render_to_display_list
    /// [`render_to_pdf`]: Interpreter::render_to_pdf
    /// [`exec`]: Interpreter::exec
    pub fn warnings(&self) -> &[ExecWarning] {
        &self.warnings
    }

    /// Access the underlying Context for power-user operations.
    pub fn context(&mut self) -> &mut Context {
        &mut self.ctx
    }

    /// Record a dropped-final-page warning if the job left marks unpainted.
    ///
    /// Must be called before `finish_device`/`vm_restore` — the restore tears
    /// down the page device the check reads `PageCount` from.
    fn record_end_of_job_warnings(&mut self) {
        if let Some(w) = diagnostics::dropped_final_page(&self.ctx) {
            self.warnings.push(w);
        }
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
        self.ctx.null_device_used = false;

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
        let save_id = self.begin_job(dpi, page_w_pt, page_h_pt)?;

        let exec_result = parse_and_exec(&mut self.ctx, ps_data);
        let exec_result = job_result(&self.ctx, exec_result);
        let exec_result = exec_result.and_then(|()| self.end_page_device());

        self.record_end_of_job_warnings();
        finish_device(&mut self.ctx);

        #[cfg(feature = "render")]
        let result = {
            let pages = take_pages(&pages_ref);
            collect_display_lists(&mut self.ctx, &pages, dpi)
        };
        #[cfg(not(feature = "render"))]
        let result = collect_display_lists_simple(&mut self.ctx, dpi);

        end_job(&mut self.ctx, save_id);

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
        self.ctx.null_device_used = false;

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
        let save_id = self.begin_job(dpi, w, h)?;

        let exec_result = self.run_eps(ps_data, llx, lly);
        if exec_result.is_ok() {
            let _ = self.end_page_device();
        }

        // Runs after the implicit `showpage` above, so it fires only if the
        // EPS painted more after its own showpage — a real dropped page.
        self.record_end_of_job_warnings();
        finish_device(&mut self.ctx);

        #[cfg(feature = "render")]
        let result = {
            let pages = take_pages(&pages_ref);
            collect_display_lists(&mut self.ctx, &pages, dpi)
        };
        #[cfg(not(feature = "render"))]
        let result = collect_display_lists_simple(&mut self.ctx, dpi);

        end_job(&mut self.ctx, save_id);

        exec_result?;
        Ok(result)
    }

    #[cfg(feature = "pdf-output")]
    fn exec_eps(&mut self, ps_data: &[u8], llx: f64, lly: f64) -> Result<(), StetError> {
        self.run_eps(ps_data, llx, lly)
    }

    /// Run an EPS with its bounding box's corner at the origin, and end its
    /// page if it did not. An EPS need not call `showpage`; one that does
    /// must not get a second, blank page. The EPS's own errors are not the
    /// caller's: the page is rendered as far as it got.
    /// Start a job: save VM, then install the device inside the save, as the
    /// CLI does, so that the job's restore reclaims what `setpagedevice`
    /// allocates. Returns the save to pass to [`end_job`]; if the device
    /// cannot be installed, the job is ended here.
    fn begin_job(&mut self, dpi: f64, width_pt: f64, height_pt: f64) -> Result<u32, StetError> {
        let save_obj = self.ctx.vm_save();
        let save_id = extract_save_id(&save_obj);
        if let Err(e) = install_device(&mut self.ctx, dpi, width_pt, height_pt) {
            end_job(&mut self.ctx, save_id);
            return Err(e);
        }
        Ok(save_id)
    }

    fn run_eps(&mut self, ps_data: &[u8], llx: f64, lly: f64) -> Result<(), StetError> {
        let wrapper = format!("gsave {} {} translate", -llx, -lly);
        parse_and_exec(&mut self.ctx, wrapper.as_bytes()).map_err(ps_err)?;
        let showpages_before = self.ctx.showpage_count;
        let _ = parse_and_exec(&mut self.ctx, ps_data);
        if self.ctx.showpage_count == showpages_before {
            // `showpage` first, as the EPS would have called it: on the
            // page device it left current. One that called `setpagedevice`
            // is on a device of its own, which the `grestore` deactivates,
            // taking with it whatever was painted and not yet shown.
            let _ = parse_and_exec(&mut self.ctx, b"showpage grestore");
        } else {
            let _ = parse_and_exec(&mut self.ctx, b"grestore");
        }
        Ok(())
    }

    /// End of job deactivates the page device: its `EndPage` runs with
    /// reason 2 and may transmit a page no `showpage` ended (PLRM 3e
    /// §6.2.6). Call after a clean run, before the dropped-page check.
    fn end_page_device(&mut self) -> Result<(), StetError> {
        stet_ops::device_ops::deactivate_page_device(&mut self.ctx)
            .map(|_| ())
            .map_err(ps_err)
    }

    #[cfg(feature = "render")]
    fn setup_capture_device_factory(&mut self, pages_ref: Arc<Mutex<Vec<PageDims>>>) {
        self.ctx.device_factory = Some(Box::new(move |w, h| {
            Box::new(PageSizeRecorder {
                width: w,
                height: h,
                pages: pages_ref.clone(),
            }) as Box<dyn OutputDevice>
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

    /// Record the text each page shows, for extraction, at `level`.
    ///
    /// Display lists then also carry [`DisplayElement::TextRun`] elements:
    /// the text shown, in Unicode — a run for each stretch of text along
    /// one baseline in one font, with word breaks marked where words are
    /// set apart — and, at [`TextExtraction::Glyphs`], the device-space
    /// position of every glyph. They paint nothing, so rendering is
    /// unchanged. [`TextExtraction::Off`] by default, which keeps display
    /// lists free of them.
    ///
    /// A glyph's text comes from its name through the Adobe Glyph List;
    /// PostScript has no ToUnicode, so a font whose glyph names mean
    /// nothing gives empty text. Text drawn inside a Type 3 font's
    /// `BuildChar` / `BuildGlyph` or a pattern cell is not the document's
    /// and is not recorded.
    pub fn text_extraction(mut self, level: TextExtraction) -> Self {
        self.text_extraction = level;
        self
    }

    /// The rendering intent each page starts with, in place of
    /// RelativeColorimetric. A program's own `setrenderingintent` still
    /// changes it.
    ///
    /// The intent selects which of the CMYK profile's tables converts CMYK
    /// colour; most press profiles' perceptual tables differ from their
    /// colorimetric ones.
    pub fn default_rendering_intent(mut self, intent: RenderingIntent) -> Self {
        self.default_rendering_intent = intent;
        self
    }

    /// Leave the unpainted areas of pages from [`Interpreter::render`]
    /// transparent, or keep them on white paper (the default).
    ///
    /// With [`PageBackground::Transparent`], every pixel no mark covers is
    /// left at alpha 0 and [`RenderedPage::rgba`] is straight
    /// (non-premultiplied) RGBA — for artwork placed over other content,
    /// such as an EPS in a page layout. Display lists are unaffected.
    #[cfg(feature = "render")]
    pub fn page_background(mut self, background: PageBackground) -> Self {
        self.page_background = background;
        self
    }

    /// Build the interpreter.
    pub fn build(self) -> Interpreter {
        let mut ctx = init::create_initialized_context(self.use_icc, self.suppress_output)
            .expect("interpreter initialization failed");
        ctx.text_extraction = self.text_extraction;
        ctx.set_default_rendering_intent(self.default_rendering_intent);
        Interpreter {
            ctx,
            #[cfg(feature = "render")]
            page_background: self.page_background,
            warnings: Vec::new(),
        }
    }
}

// --- Page dimension tracking ---

/// Recorded page dimensions, one per page the job sends.
#[cfg(feature = "render")]
struct PageDims {
    width: u32,
    height: u32,
}

/// The device the facade's raster calls install: it records each page's
/// size and drops its display list. The facade renders the captured display
/// lists itself (`Context::capture_display_lists`), so a device that also
/// rasterised them — as a `SkiaDevice` into a discarding sink once did —
/// rendered every page twice.
#[cfg(feature = "render")]
struct PageSizeRecorder {
    width: u32,
    height: u32,
    pages: Arc<Mutex<Vec<PageDims>>>,
}

#[cfg(feature = "render")]
impl OutputDevice for PageSizeRecorder {
    fn fill_path(
        &mut self,
        _path: &stet_fonts::geometry::PsPath,
        _params: &stet_graphics::device::FillParams,
    ) {
    }
    fn stroke_path(
        &mut self,
        _path: &stet_fonts::geometry::PsPath,
        _params: &stet_graphics::device::StrokeParams,
    ) {
    }
    fn clip_path(
        &mut self,
        _path: &stet_fonts::geometry::PsPath,
        _params: &stet_graphics::device::ClipParams,
    ) {
    }
    fn init_clip(&mut self) {}
    fn erase_page(&mut self) {}
    fn show_page(&mut self, _output_path: &str) -> Result<(), String> {
        Ok(())
    }
    fn draw_image(&mut self, _sample_data: &[u8], _params: &stet_graphics::device::ImageParams) {}
    fn page_size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    fn replay_and_show(&mut self, _list: DisplayList, _output_path: &str) -> Result<(), String> {
        if let Ok(mut pages) = self.pages.lock() {
            pages.push(PageDims {
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

/// End a job: discard what it left behind, restore VM to the save taken
/// before it, and reset the context for the next one.
///
/// The order matters. Whatever the job left on the operand, execution and
/// dictionary stacks, in unfinished loops, and in the graphics state can
/// refer to objects it created — leftover operands and an unmatched `begin`
/// are common in real programs — and the restore releases those objects.
/// Discarding them first means no stack holds a released object, even for
/// the length of the restore. The CLI's job loop has always done it in this
/// order.
fn end_job(ctx: &mut Context, save_id: u32) {
    ctx.o_stack.clear();
    ctx.e_stack.clear();
    ctx.loops.clear();
    ctx.d_stack.truncate(3);
    ctx.gstate = ctx.initial_gstate();
    ctx.gstate_stack.clear();
    // The job's device has read its font copies; dropping them first spares
    // the restore from copying fonts no one will read.
    ctx.font_snapshots.clear();
    let _ = ctx.vm_restore(save_id);
    reset_context(ctx);
}

fn reset_context(ctx: &mut Context) {
    ctx.device = None;
    ctx.output_path = None;
    ctx.display_list.clear();
    ctx.capture_display_lists = None;
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

/// How a job ended, for the caller: `quit` ends it cleanly, not in error.
///
/// Read `newerror` before the job's `restore`, while `$error` still says
/// how the job stopped.
fn job_result(ctx: &Context, result: Result<(), PsError>) -> Result<(), StetError> {
    match result {
        Ok(()) | Err(PsError::Quit) => Ok(()),
        // PostScript `quit` clears `newerror` and calls `stop`.
        Err(PsError::Stop) if !ctx.newerror() => Ok(()),
        Err(e) => Err(ps_err(e)),
    }
}
