// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! WebAssembly bindings for the stet PostScript interpreter.
//!
//! Provides `Interpreter` (a fully initialized PostScript context with embedded
//! resources) and `render()` (renders PostScript/EPS data to RGBA pages).

pub mod embedded_resources;
mod memory_sink;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use wasm_bindgen::prelude::*;

use stet_core::context::Context;
use stet_core::device::OutputDevice;
use stet_core::eps::{content_is_epsf, read_eps_bounding_box};
use stet_core::error::PsError;
use stet_core::object::{ObjFlags, PsObject, PsValue};
use stet_engine::eval::{eval, parse_and_exec};
use stet_graphics::display_list::DisplayList;
use stet_graphics::icc::IccCache;
use stet_render::{ImageCache, PreparedDisplayList, SkiaDevice};

use memory_sink::{NullSinkFactory, PageData, set_sink_callback};

/// Embedded GhostScript default CMYK ICC profile for CMYK→sRGB conversion.
const DEFAULT_CMYK_ICC: &[u8] = include_bytes!("default_cmyk.icc");

/// Page metadata stored alongside display lists for viewport rendering.
struct PageInfo {
    /// Page width in device-space pixels at this page's DPI.
    width: u32,
    /// Page height in device-space pixels at this page's DPI.
    height: u32,
    /// The DPI this page was rendered at (may differ from initial reference
    /// if the PS program called setpagedevice with a different HWResolution).
    dpi: f64,
}

/// Preserved state for streaming PostScript interpretation.
///
/// `render()` returns as soon as the first `showpage` is captured, leaving
/// the interpreter paused mid-program. `step_ps_page()` re-enters the eval
/// loop to drive the next page. This state holds everything the resumed
/// eval needs that isn't already on `Context` itself.
struct PsStreamState {
    /// VM save level captured before interpretation started; restored at
    /// end-of-program so per-file VM changes don't leak across renders.
    save_id: u32,
    /// Shared page-data buffer populated by the NullSinkFactory as each
    /// `end_page` fires. Drained (per step) to pair dimensions with the
    /// newly-captured display lists.
    pages_ref: Arc<Mutex<Vec<PageData>>>,
    /// Flag set by `take_display_list` after each page is captured, causing
    /// the eval loop to return `PsError::Quit`. Cleared at the start of
    /// every step before re-entering eval.
    interrupt_flag: Arc<AtomicBool>,
}

/// A fully initialized PostScript interpreter context.
///
/// Created once via `create_interpreter()`, reused across `render()` calls.
#[wasm_bindgen]
pub struct Interpreter {
    ctx: Context,
    /// Display lists captured during rendering, one per page.
    /// Retained for viewport re-rendering at arbitrary zoom levels.
    page_display_lists: Vec<DisplayList>,
    /// Pre-computed metadata for each display list, built lazily on first viewport render.
    page_prepared: Vec<Option<PreparedDisplayList>>,
    /// Per-page ICC cache, built lazily on first viewport render.
    page_icc: Vec<Option<IccCache>>,
    /// Per-page pre-converted RGBA image cache, built lazily on first viewport render.
    /// Only the viewed page has its cache populated to avoid OOM on large documents.
    page_image_cache: Vec<Option<ImageCache>>,
    /// Per-page dimensions at the reference DPI.
    page_info: Vec<PageInfo>,
    /// The DPI used during interpretation (reference DPI for display list coordinates).
    reference_dpi: f64,
    /// Embedded CMYK ICC profile bytes for ICC-aware viewport rendering.
    system_cmyk_bytes: Arc<Vec<u8>>,
    /// Owned PDF source bytes, kept so per-page rendering is lazy:
    /// `open_pdf` parses only xref + page tree; each page's content stream
    /// is interpreted on demand by `render_pdf_page` (or implicitly by the
    /// first `render_viewport`/`render_viewport_band` call for that page).
    pdf_bytes: Option<Vec<u8>>,
    /// ICC cache captured at `open_pdf` time; cloned into a new
    /// `PdfDocument` for each per-page render.
    pdf_icc_cache: Option<IccCache>,
    /// Active PostScript streaming session, if any. `render()` installs this
    /// and returns after the first `showpage`; `step_ps_page` uses it to
    /// resume eval for subsequent pages. Cleared when the program completes.
    ps_stream: Option<PsStreamState>,
}

/// A single rendered page with dimensions and RGBA pixel data.
#[wasm_bindgen]
pub struct Page {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

#[wasm_bindgen]
impl Page {
    /// Page width in pixels.
    #[wasm_bindgen(getter)]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Page height in pixels.
    #[wasm_bindgen(getter)]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// RGBA pixel data (4 bytes per pixel, row-major).
    #[wasm_bindgen(getter)]
    pub fn rgba(&self) -> Vec<u8> {
        self.rgba.clone()
    }
}

/// A Write implementation that discards all output (for WASM where there's no stdout).
struct NullWriter;

impl std::io::Write for NullWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Create a new PostScript interpreter with all embedded resources initialized.
///
/// This runs the init scripts (sysdict.ps, resourcecategories.ps, fontcategory.ps,
/// fontmapping.ps) so the interpreter is ready to render PostScript files.
fn log(msg: &str) {
    web_sys::console::log_1(&wasm_bindgen::JsValue::from_str(msg));
}

#[wasm_bindgen]
pub fn create_interpreter() -> Interpreter {
    // Forward Rust panics to console.error with file/line/message.
    // Required: WASM builds default to panic=abort, which surfaces to JS as
    // the opaque "RuntimeError: unreachable executed". This hook replaces that
    // with a readable panic message before the abort fires.
    std::panic::set_hook(Box::new(console_error_panic_hook::hook));

    log("stet: creating context...");
    let mut ctx = Context::new();

    log("stet: wiring exec_sync...");
    ctx.exec_sync_fn = Some(stet_engine::eval::exec_sync);
    ctx.allow_ps_resolution = true;

    log("stet: building system dict...");
    stet_ops::build_system_dict(&mut ctx);

    log("stet: registering embedded resources...");
    embedded_resources::register_embedded_resources(&mut ctx.files);

    ctx.font_resource_path = Some("Font".to_string());
    ctx.stdout = Box::new(NullWriter);

    log("stet: loading embedded CMYK ICC profile...");
    ctx.icc_cache.load_cmyk_profile_bytes(DEFAULT_CMYK_ICC);
    let cmyk_bytes: Arc<Vec<u8>> = Arc::new(DEFAULT_CMYK_ICC.to_vec());

    let cmyk_for_factory = cmyk_bytes.clone();
    ctx.device_factory = Some(Box::new(move |w, h| {
        let mut dev = SkiaDevice::new(w, h);
        dev.set_system_cmyk_bytes(cmyk_for_factory.clone());
        Box::new(dev) as Box<dyn OutputDevice>
    }));

    log("stet: running init scripts...");
    run_embedded_init_scripts(&mut ctx);

    log("stet: interpreter ready");
    Interpreter {
        ctx,
        page_display_lists: Vec::new(),
        page_prepared: Vec::new(),
        page_icc: Vec::new(),
        page_image_cache: Vec::new(),
        page_info: Vec::new(),
        reference_dpi: 150.0,
        system_cmyk_bytes: cmyk_bytes,
        pdf_bytes: None,
        pdf_icc_cache: None,
        ps_stream: None,
    }
}

/// Register a JS callback for streaming render events.
///
/// The callback receives (event, arg1, arg2, arg3, data):
///   event=0 (begin_page): arg1=index, arg2=width, arg3=height
///   event=1 (rows): data=Uint8Array of RGBA band pixels
///   event=2 (end_page): arg1=index
///
/// This streams bands directly to JS so WASM never holds a full page
/// in memory — critical at high DPI where a page can exceed 2 GB.
#[wasm_bindgen]
pub fn set_page_callback(callback: &js_sys::Function) {
    let callback = callback.clone();
    set_sink_callback(Some(Box::new(move |event, arg1, arg2, arg3, data| {
        let args = js_sys::Array::new();
        args.push(&JsValue::from(event));
        args.push(&JsValue::from(arg1));
        args.push(&JsValue::from(arg2));
        args.push(&JsValue::from(arg3));
        if !data.is_empty() {
            let arr = js_sys::Uint8Array::from(data);
            args.push(&arr.into());
        } else {
            args.push(&JsValue::NULL);
        }
        let _ = callback.apply(&JsValue::NULL, &args);
    })));
}

/// Clear the page callback.
#[wasm_bindgen]
pub fn clear_page_callback() {
    set_sink_callback(None);
}

/// Render PostScript or EPS data at the specified DPI.
///
/// Interprets the PostScript, renders an overview of each page, and retains
/// display lists for viewport re-rendering via `render_viewport()`.
/// The interpreter state is reset after rendering so it can be reused.
#[wasm_bindgen]
pub fn render(interp: &mut Interpreter, ps_data: &[u8], dpi: f64, filename: &str) -> Result<JsValue, JsValue> {
    log(&format!("stet: render() called — {} bytes, dpi={}, file={}", ps_data.len(), dpi, filename));

    // Clear previous display lists
    interp.page_display_lists.clear();
    interp.page_prepared.clear();
    interp.page_icc.clear();
    interp.page_image_cache.clear();
    interp.page_info.clear();
    interp.reference_dpi = dpi;
    // PS rendering doesn't use the lazy-PDF state
    interp.pdf_bytes = None;
    interp.pdf_icc_cache = None;
    abandon_ps_stream(interp);

    // Enable display list capture
    interp.ctx.capture_display_lists = Some(Vec::new());

    // Set up shared page collection — NullSinkFactory records dimensions only,
    // discarding rendered pixels since viewport rendering is done on demand.
    let (_sink_factory, pages_ref) = NullSinkFactory::new();

    // Strip DOS EPS header and check for EPS bounding box
    let ps_data = stet_core::eps::strip_dos_eps_header(ps_data);

    // Use EPS mode only when the file extension is .eps or .epsf (matching CLI behavior)
    let filename_lower = filename.to_ascii_lowercase();
    let is_eps = filename_lower.ends_with(".eps")
        || filename_lower.ends_with(".epsf")
        || content_is_epsf(ps_data);

    if is_eps
        && let Some((llx, lly, urx, ury)) = read_eps_bounding_box(ps_data)
        && (urx - llx) > 0.0
        && (ury - lly) > 0.0
    {
        let w = urx - llx;
        let h = ury - lly;
        log(&format!(
            "stet: EPS bbox=({},{},{},{}) size={}x{} dpi={}",
            llx, lly, urx, ury, w, h, dpi
        ));

        // Set up device_factory with NullSinkFactory — pixels are discarded
        let pages_for_factory = pages_ref.clone();
        let cmyk_for_factory = interp.system_cmyk_bytes.clone();
        interp.ctx.device_factory = Some(Box::new(move |w, h| {
            let factory = NullSinkFactory::from_shared(pages_for_factory.clone());
            let mut dev = SkiaDevice::with_sink_factory(w, h, Box::new(factory));
            dev.set_system_cmyk_bytes(cmyk_for_factory.clone());
            Box::new(dev) as Box<dyn OutputDevice>
        }));

        install_device_via_setpagedevice(&mut interp.ctx, dpi, w, h)
            .map_err(|e| JsValue::from_str(&format!("Device setup error: {}", e)))?;

        let save_obj = interp.ctx.vm_save();
        let save_id = match save_obj.value {
            stet_core::object::PsValue::Save(stet_core::object::SaveLevel(id)) => id,
            _ => unreachable!(),
        };
        let wrapper = format!("gsave {} {} translate", -llx, -lly);
        parse_and_exec(&mut interp.ctx, wrapper.as_bytes())
            .map_err(|e| JsValue::from_str(&format!("PS error (translate): {}", e)))?;

        parse_and_exec(&mut interp.ctx, ps_data)
            .map_err(|e| JsValue::from_str(&format!("PS error (exec): {}", e)))?;

        // grestore to undo our translate; only call showpage if the EPS didn't already
        let need_showpage = pages_ref.lock().map(|g| g.is_empty()).unwrap_or(true);
        if need_showpage {
            parse_and_exec(&mut interp.ctx, b"grestore showpage")
                .map_err(|e| JsValue::from_str(&format!("PS error (showpage): {}", e)))?;
        } else {
            parse_and_exec(&mut interp.ctx, b"grestore")
                .map_err(|e| JsValue::from_str(&format!("PS error (grestore): {}", e)))?;
        }

        finish_device(&mut interp.ctx);
        let pages = extract_pages(&pages_ref);
        collect_display_lists(interp, &pages);
        let _ = interp.ctx.vm_restore(save_id);
        reset_context(&mut interp.ctx);
        let page_count = interp.page_display_lists.len() as u32;
        return Ok(JsValue::from(page_count));
    }

    // Non-EPS or no valid bounding box: stream page-by-page.
    // Setup: device factory, setpagedevice, vm_save, push source on e_stack,
    // wire yield-after-showpage. Then drive the eval loop until the first
    // `showpage` fires (or the program ends). Pages 2..N are driven by
    // later `step_ps_page` calls — the worker is idle between them so it
    // can service viewport-render requests for page 1 while we queue up
    // page 2's interpretation.
    let pages_for_factory = pages_ref.clone();
    let cmyk_for_factory = interp.system_cmyk_bytes.clone();
    interp.ctx.device_factory = Some(Box::new(move |w, h| {
        let factory = NullSinkFactory::from_shared(pages_for_factory.clone());
        let mut dev = SkiaDevice::with_sink_factory(w, h, Box::new(factory));
        dev.set_system_cmyk_bytes(cmyk_for_factory.clone());
        Box::new(dev) as Box<dyn OutputDevice>
    }));

    install_device_via_setpagedevice(&mut interp.ctx, dpi, 612.0, 792.0)
        .map_err(|e| JsValue::from_str(&format!("Device setup error: {}", e)))?;

    let save_obj = interp.ctx.vm_save();
    let save_id = match save_obj.value {
        PsValue::Save(stet_core::object::SaveLevel(id)) => id,
        _ => unreachable!(),
    };

    let interrupt_flag = Arc::new(AtomicBool::new(false));
    interp.ctx.interrupt_flag = Some(interrupt_flag.clone());
    interp.ctx.yield_after_showpage = true;

    // Inline the setup side of parse_and_exec: install the source as a
    // StringSource in FileStore and push it as an executable File on the
    // exec stack. Future step_ps_page calls just re-enter eval — the File
    // stays on e_stack and the StringSource's cursor preserves position.
    let file_entity = interp.ctx.files.create_string_source(ps_data.to_vec());
    if let Err(e) = interp.ctx.e_stack.push(PsObject {
        value: PsValue::File(file_entity),
        flags: ObjFlags::executable_composite(),
    }) {
        return Err(JsValue::from_str(&format!("PS setup error: {}", e)));
    }

    interp.ps_stream = Some(PsStreamState {
        save_id,
        pages_ref,
        interrupt_flag,
    });

    match drive_ps_eval(interp) {
        Ok(page_count) => Ok(JsValue::from(page_count)),
        Err(e) => Err(e),
    }
}

/// Resume PS interpretation up to the next `showpage`, appending any new
/// pages to the interpreter's page tables. Returns the total page count so
/// far. Returns the same count when the program has already completed — JS
/// can poll this to learn when streaming is finished (the returned count
/// stops increasing and `ps_stream_active` reads false).
#[wasm_bindgen]
pub fn step_ps_page(interp: &mut Interpreter) -> Result<JsValue, JsValue> {
    if interp.ps_stream.is_none() {
        return Ok(JsValue::from(interp.page_display_lists.len() as u32));
    }
    match drive_ps_eval(interp) {
        Ok(page_count) => Ok(JsValue::from(page_count)),
        Err(e) => Err(e),
    }
}

/// True while a PS program has more pages to interpret. JS can stop its
/// step-loop as soon as this goes false.
#[wasm_bindgen]
pub fn ps_stream_active(interp: &Interpreter) -> bool {
    interp.ps_stream.is_some()
}

/// Run the eval loop until the next showpage-induced yield, end-of-program,
/// or error. Collects any newly-captured display lists into the interpreter
/// and finalises device/VM state when the program terminates.
///
/// Returns the total page count after this step. On a fatal PS error with
/// zero captured pages the function returns an `Err` describing it;
/// otherwise it logs the error and returns the pages captured so far (same
/// resilience the monolithic path used to provide).
fn drive_ps_eval(interp: &mut Interpreter) -> Result<u32, JsValue> {
    let state = interp
        .ps_stream
        .as_ref()
        .expect("drive_ps_eval called without an active PS stream");
    let interrupt_flag = state.interrupt_flag.clone();
    let pages_ref = state.pages_ref.clone();
    let save_id = state.save_id;

    // Clear flag before each step so the leftover `true` from the previous
    // showpage doesn't trip the Quit check on the first iteration.
    interrupt_flag.store(false, Ordering::Relaxed);

    let result = eval(&mut interp.ctx);
    let yielded_by_page = interrupt_flag.load(Ordering::Relaxed);

    // Drain any newly-captured pages regardless of how eval returned.
    collect_streaming_pages(interp, &pages_ref);

    match result {
        Err(PsError::Quit) if yielded_by_page => {
            // Ordinary page-boundary pause. Leave the PS stream active.
            Ok(interp.page_display_lists.len() as u32)
        }
        Ok(()) | Err(PsError::Quit) => {
            // Program ran to completion (or PS `quit` op fired).
            finalize_ps_stream(interp, save_id);
            Ok(interp.page_display_lists.len() as u32)
        }
        Err(e) => {
            log(&format!(
                "stet: render error: {} | o_stack={} e_stack={} d_stack={}",
                e,
                interp.ctx.o_stack.len(),
                interp.ctx.e_stack.len(),
                interp.ctx.d_stack.len()
            ));
            finalize_ps_stream(interp, save_id);
            let page_count = interp.page_display_lists.len() as u32;
            if page_count == 0 {
                return Err(JsValue::from_str(&format!("PS error: {}", e)));
            }
            Ok(page_count)
        }
    }
}

/// Move any newly-captured display lists from `ctx.capture_display_lists`
/// into the interpreter's page tables, pairing each with the matching
/// `PageData` dimensions from the shared sink buffer. Leaves capture
/// enabled (with a fresh empty vec) so the next step can collect more.
fn collect_streaming_pages(interp: &mut Interpreter, pages_ref: &Arc<Mutex<Vec<PageData>>>) {
    let captured = std::mem::replace(
        &mut interp.ctx.capture_display_lists,
        Some(Vec::new()),
    )
    .unwrap_or_default();
    let pages = extract_pages(pages_ref);
    for (i, (dl, dpi)) in captured.into_iter().enumerate() {
        interp.page_display_lists.push(dl);
        interp.page_prepared.push(None);
        interp.page_icc.push(None);
        interp.page_image_cache.push(None);
        if i < pages.len() {
            interp.page_info.push(PageInfo {
                width: pages[i].width,
                height: pages[i].height,
                dpi,
            });
        } else {
            interp.page_info.push(PageInfo {
                width: (612.0 * dpi / 72.0) as u32,
                height: (792.0 * dpi / 72.0) as u32,
                dpi,
            });
        }
    }
}

/// Finish a PS streaming session: flush the device, restore the VM, reset
/// per-render context, drop the streaming state. Idempotent.
fn finalize_ps_stream(interp: &mut Interpreter, save_id: u32) {
    finish_device(&mut interp.ctx);
    let _ = interp.ctx.vm_restore(save_id);
    reset_context(&mut interp.ctx);
    interp.ctx.interrupt_flag = None;
    interp.ctx.yield_after_showpage = false;
    interp.ctx.capture_display_lists = None;
    interp.ps_stream = None;
}

/// Tear down a streaming session without finalising (the VM is in an
/// undefined state, so skip vm_restore). Used when the next render is about
/// to overwrite everything anyway — e.g. a new file is dropped mid-stream.
fn abandon_ps_stream(interp: &mut Interpreter) {
    if interp.ps_stream.is_some() {
        reset_context(&mut interp.ctx);
        interp.ctx.interrupt_flag = None;
        interp.ctx.yield_after_showpage = false;
        interp.ctx.capture_display_lists = None;
        interp.ps_stream = None;
    }
}

/// Open a PDF file and parse its structure (xref, page tree, page sizes).
///
/// Does **not** interpret page content streams — those are built on demand by
/// `render_pdf_page` (or implicitly by the first `render_viewport` /
/// `render_viewport_band` call for that page). This keeps the initial call
/// fast on large documents: a 500-page PDF returns its page count and
/// per-page dimensions in milliseconds instead of seconds.
///
/// Returns the number of pages, or throws on parse error.
#[wasm_bindgen]
pub fn open_pdf(interp: &mut Interpreter, pdf_data: &[u8], dpi: f64) -> Result<JsValue, JsValue> {
    log(&format!("stet: open_pdf() called — {} bytes, dpi={}", pdf_data.len(), dpi));

    // Clear previous state
    interp.page_display_lists.clear();
    interp.page_prepared.clear();
    interp.page_icc.clear();
    interp.page_image_cache.clear();
    interp.page_info.clear();
    interp.reference_dpi = dpi;
    interp.pdf_bytes = None;
    interp.pdf_icc_cache = None;
    abandon_ps_stream(interp);

    let mut icc_cache = IccCache::new();
    icc_cache.load_cmyk_profile_bytes(DEFAULT_CMYK_ICC);

    // Parse structure only — page content streams are deferred.
    let doc = stet_pdf_reader::PdfDocument::from_bytes_with_icc(pdf_data, icc_cache.clone())
        .map_err(|e| JsValue::from_str(&format!("PDF parse error: {}", e)))?;

    let count = doc.page_count();
    let scale = dpi / 72.0;
    for page_idx in 0..count {
        let (page_w, page_h) = doc.page_size(page_idx).map_err(|e| {
            JsValue::from_str(&format!("PDF page {} error: {}", page_idx, e))
        })?;
        interp.page_display_lists.push(DisplayList::new());
        interp.page_prepared.push(None);
        interp.page_icc.push(None);
        interp.page_image_cache.push(None);
        interp.page_info.push(PageInfo {
            width: (page_w * scale).round() as u32,
            height: (page_h * scale).round() as u32,
            dpi,
        });
    }

    // Stash bytes + ICC cache so per-page renders can recreate the document.
    // PDF parse state is xref + page tree only; there's no mutable render
    // state to preserve, so re-parsing per page is cheap.
    interp.pdf_bytes = Some(pdf_data.to_vec());
    interp.pdf_icc_cache = Some(icc_cache);

    log(&format!("stet: open_pdf complete — {} pages (content streams deferred)", count));
    Ok(JsValue::from(count as u32))
}

/// Build the display list for a single PDF page.
///
/// Idempotent: if the page is already rendered, returns immediately.
/// Called implicitly by `render_viewport`/`render_viewport_band` on first
/// access, but exposed to JS so callers can prefetch future pages during
/// idle time.
#[wasm_bindgen]
pub fn render_pdf_page(interp: &mut Interpreter, page_index: u32) -> Result<(), JsValue> {
    let idx = page_index as usize;
    if idx >= interp.page_display_lists.len() {
        return Err(JsValue::from_str(&format!(
            "Page index {} out of range (have {} pages)",
            page_index,
            interp.page_display_lists.len()
        )));
    }
    ensure_pdf_page_rendered(interp, idx)
}

/// Internal: ensure `page_display_lists[idx]` is populated for a PDF-backed
/// interpreter. Returns `Err` only if PDF state is missing or re-parse fails.
/// Idempotent — checks `page_display_lists[idx]` for existing content first.
fn ensure_pdf_page_rendered(interp: &mut Interpreter, idx: usize) -> Result<(), JsValue> {
    // Already rendered (or PS-interpreted, which pre-populates with real content)
    if !interp.page_display_lists[idx].elements().is_empty() {
        return Ok(());
    }
    // No PDF open → nothing to lazy-render. This is expected for PS files;
    // an empty display list there means an empty page.
    let pdf_bytes = match interp.pdf_bytes.as_deref() {
        Some(b) => b,
        None => return Ok(()),
    };
    let icc_cache = interp
        .pdf_icc_cache
        .clone()
        .ok_or_else(|| JsValue::from_str("Missing ICC cache state"))?;

    let mut doc = stet_pdf_reader::PdfDocument::from_bytes_with_icc(pdf_bytes, icc_cache)
        .map_err(|e| JsValue::from_str(&format!("PDF parse error: {}", e)))?;
    doc.set_font_provider(embedded_resources::build_font_provider());

    let dpi = interp.reference_dpi;
    let dl = doc
        .render_page(idx, dpi)
        .map_err(|e| JsValue::from_str(&format!("PDF page {} render error: {}", idx, e)))?;

    log(&format!(
        "stet: rendered page {} — {} display elements",
        idx,
        dl.elements().len()
    ));
    interp.page_display_lists[idx] = dl;
    Ok(())
}

/// Get the number of pages available for viewport rendering.
#[wasm_bindgen]
pub fn page_count(interp: &Interpreter) -> u32 {
    interp.page_display_lists.len() as u32
}

/// Get page dimensions and DPI for a specific page.
/// Returns [width, height, dpi] or null if page index is out of range.
#[wasm_bindgen]
pub fn page_dimensions(interp: &Interpreter, page_index: u32) -> JsValue {
    let i = page_index as usize;
    if i < interp.page_info.len() {
        let info = &interp.page_info[i];
        let arr = js_sys::Array::new();
        arr.push(&JsValue::from(info.width));
        arr.push(&JsValue::from(info.height));
        arr.push(&JsValue::from(info.dpi));
        arr.into()
    } else {
        JsValue::NULL
    }
}

/// Get the initial reference DPI used during interpretation.
#[wasm_bindgen]
pub fn reference_dpi(interp: &Interpreter) -> f64 {
    interp.reference_dpi
}

/// Ensure the image cache for a given page is built, evicting all others to save memory.
///
/// Only one page's image cache is kept at a time — WASM memory is precious.
/// Uses explicit field access (not `&mut self`) to satisfy the borrow checker.
/// Ensure per-page caches (prepared, ICC, image) are built for the given page,
/// evicting all other pages' caches to keep WASM memory usage bounded.
macro_rules! ensure_page_caches {
    ($interp:expr, $page_idx:expr) => {
        if $interp.page_prepared[$page_idx].is_none()
            || $interp.page_image_cache[$page_idx].is_none()
        {
            // Evict all other pages' caches to reclaim memory
            for j in 0..$interp.page_image_cache.len() {
                if j != $page_idx {
                    $interp.page_prepared[j] = None;
                    $interp.page_icc[j] = None;
                    $interp.page_image_cache[j] = None;
                }
            }
            if $interp.page_prepared[$page_idx].is_none() {
                $interp.page_prepared[$page_idx] = Some(stet_render::prepare_display_list(
                    &$interp.page_display_lists[$page_idx],
                ));
            }
            if $interp.page_icc[$page_idx].is_none() {
                $interp.page_icc[$page_idx] = Some(stet_render::build_icc_cache_for_list(
                    &$interp.page_display_lists[$page_idx],
                    Some(&$interp.system_cmyk_bytes),
                ));
            }
            if $interp.page_image_cache[$page_idx].is_none() {
                $interp.page_image_cache[$page_idx] = Some(ImageCache::build(
                    &$interp.page_display_lists[$page_idx],
                    $interp.page_icc[$page_idx].as_ref(),
                ));
            }
        }
    };
}

/// Render a rectangular viewport region of a stored display list.
///
/// Arguments:
/// - `page_index`: Which page's display list to render
/// - `vp_x, vp_y, vp_w, vp_h`: Viewport rectangle in device-space pixels
///   (at the reference DPI used during interpretation)
/// - `pixel_w, pixel_h`: Output pixel dimensions
///
/// Returns a `Page` with the rendered RGBA data.
#[wasm_bindgen]
pub fn render_viewport(
    interp: &mut Interpreter,
    page_index: u32,
    vp_x: f64,
    vp_y: f64,
    vp_w: f64,
    vp_h: f64,
    pixel_w: u32,
    pixel_h: u32,
) -> Result<Page, JsValue> {
    let i = page_index as usize;
    if i >= interp.page_display_lists.len() {
        return Err(JsValue::from_str(&format!(
            "Page index {} out of range (have {} pages)",
            page_index,
            interp.page_display_lists.len()
        )));
    }

    ensure_pdf_page_rendered(interp, i)?;
    ensure_page_caches!(interp, i);

    let list = &interp.page_display_lists[i];
    let page_dpi = interp.page_info[i].dpi;
    let prepared = interp.page_prepared[i].as_ref().unwrap();
    let icc = interp.page_icc[i].as_ref();
    let image_cache = interp.page_image_cache[i].as_ref();
    let rgba = stet_render::render_region_prepared(
        list,
        prepared,
        vp_x,
        vp_y,
        vp_w,
        vp_h,
        pixel_w,
        pixel_h,
        page_dpi,
        icc,
        image_cache,
        false,
    );

    Ok(Page {
        width: pixel_w,
        height: pixel_h,
        rgba,
    })
}

/// Compute the number of bands and band height for viewport banding.
///
/// Returns a JS array `[num_bands, band_height]`.
#[wasm_bindgen]
pub fn viewport_band_params(pixel_w: u32, pixel_h: u32) -> js_sys::Array {
    let (num_bands, band_h) = stet_render::viewport_band_count(pixel_w, pixel_h);
    let arr = js_sys::Array::new();
    arr.push(&JsValue::from(num_bands));
    arr.push(&JsValue::from(band_h));
    arr
}

/// Render a single horizontal band of a viewport region.
///
/// This is the per-band counterpart to `render_viewport()`. The JS worker
/// loops over `band_idx` in `0..num_bands`, collecting RGBA strips.
#[wasm_bindgen]
#[allow(clippy::too_many_arguments)]
pub fn render_viewport_band(
    interp: &mut Interpreter,
    page_index: u32,
    vp_x: f64,
    vp_y: f64,
    vp_w: f64,
    vp_h: f64,
    pixel_w: u32,
    pixel_h: u32,
    band_idx: u32,
    band_h: u32,
    num_bands: u32,
) -> Result<Page, JsValue> {
    let i = page_index as usize;
    if i >= interp.page_display_lists.len() {
        return Err(JsValue::from_str(&format!(
            "Page index {} out of range (have {} pages)",
            page_index,
            interp.page_display_lists.len()
        )));
    }

    ensure_pdf_page_rendered(interp, i)?;
    ensure_page_caches!(interp, i);

    let list = &interp.page_display_lists[i];
    let page_dpi = interp.page_info[i].dpi;
    let prepared = interp.page_prepared[i].as_ref().unwrap();
    let icc = interp.page_icc[i].as_ref();
    let image_cache = interp.page_image_cache[i].as_ref();
    let rgba = stet_render::render_region_single_band(
        list,
        prepared,
        vp_x,
        vp_y,
        vp_w,
        vp_h,
        pixel_w,
        pixel_h,
        band_idx,
        band_h,
        num_bands,
        page_dpi,
        icc,
        image_cache,
        false,
    );

    let actual_h = if band_idx < num_bands - 1 {
        band_h
    } else {
        pixel_h - band_idx * band_h
    };

    Ok(Page {
        width: pixel_w,
        height: actual_h,
        rgba,
    })
}

/// Install a rendering device via setpagedevice (matching CLI behavior).
///
/// This creates a proper page device with EndPage/BeginPage procedures,
/// ensuring multi-page documents render correctly. The device_factory must
/// already be set before calling this.
fn install_device_via_setpagedevice(
    ctx: &mut Context,
    dpi: f64,
    width_pt: f64,
    height_pt: f64,
) -> Result<(), PsError> {
    ctx.output_path = Some("wasm_output".to_string());

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
    parse_and_exec(ctx, setup.as_bytes())
}

/// Call finish() on the device to flush any pending renders.
fn finish_device(ctx: &mut Context) {
    if let Some(mut dev) = ctx.device.take() {
        let _ = dev.finish_with_context(ctx);
        ctx.device = Some(dev);
    }
}

/// Extract collected pages from the shared page buffer.
fn extract_pages(pages_ref: &Arc<Mutex<Vec<PageData>>>) -> Vec<PageData> {
    match pages_ref.lock() {
        Ok(mut guard) => std::mem::take(&mut *guard),
        Err(e) => std::mem::take(&mut *e.into_inner()),
    }
}

/// Collect captured display lists and page info from Context into Interpreter.
fn collect_display_lists(interp: &mut Interpreter, pages: &[PageData]) {
    let captured = interp.ctx.capture_display_lists.take().unwrap_or_default();
    for (i, (dl, dpi)) in captured.into_iter().enumerate() {
        interp.page_display_lists.push(dl);
        interp.page_prepared.push(None);
        interp.page_icc.push(None);
        interp.page_image_cache.push(None);
        if i < pages.len() {
            interp.page_info.push(PageInfo {
                width: pages[i].width,
                height: pages[i].height,
                dpi,
            });
        } else {
            // Fallback: compute from captured DPI and default page size
            interp.page_info.push(PageInfo {
                width: (612.0 * dpi / 72.0) as u32,
                height: (792.0 * dpi / 72.0) as u32,
                dpi,
            });
        }
    }
    interp.ctx.capture_display_lists = None;
}

/// Reset interpreter state for the next render call.
fn reset_context(ctx: &mut Context) {
    ctx.device = None;
    ctx.output_path = None;
    ctx.display_list.clear();
    ctx.capture_display_lists = None;
    ctx.o_stack.clear();
    ctx.e_stack.clear();
    ctx.gstate = stet_core::graphics_state::GraphicsState::new();
    ctx.gstate_stack.clear();
    // Reset d_stack to the 3 standard dicts (systemdict, globaldict, userdict)
    ctx.d_stack.truncate(3);
    // Clear any pending save levels
    ctx.save_stack = stet_core::save_stack::SaveStack::new();
    // Reset error handling state
    ctx.in_error_handler = false;
}

/// Run embedded init scripts to bootstrap the PostScript resource system.
///
/// This replicates the logic from stet-cli's `run_init_scripts()` but uses
/// embedded byte data instead of reading from the filesystem.
fn run_embedded_init_scripts(ctx: &mut Context) {
    // sysdict.ps expects systemdict as the ONLY dict on the d_stack
    let saved_d_stack = ctx.d_stack.clone();
    ctx.d_stack.truncate(1);

    // Suppress stdout during init
    let old_stdout = std::mem::replace(&mut ctx.stdout, Box::new(NullWriter));

    ctx.initializing = true;
    ctx.vm_alloc_mode = true;

    // sysdict.ps uses `(resources/Init/X.ps) run` to load sub-scripts,
    // which will find the embedded files via the virtual filesystem.
    let init_script = b"{(resources/Init/sysdict.ps) run} stopped { } if";
    let exec_ok = match parse_and_exec(ctx, init_script) {
        Ok(()) => true,
        Err(PsError::Quit) => true,
        Err(e) => {
            log(&format!("stet: init script error: {}", e));
            false
        }
    };

    ctx.stdout = old_stdout;

    log(&format!(
        "stet: init exec_ok={}, d_stack.len={}",
        exec_ok,
        ctx.d_stack.len()
    ));

    if exec_ok && ctx.d_stack.len() >= 3 {
        sync_context_after_init(ctx);
        log("stet: init sync complete");
    } else {
        log("stet: init FAILED, restoring d_stack");
        ctx.d_stack = saved_d_stack;
        ctx.o_stack.clear();
        ctx.e_stack.clear();
    }

    ctx.vm_alloc_mode = false;
    ctx.initializing = false;
    ctx.dicts.set_access(
        ctx.systemdict,
        stet_core::object::ObjFlags::ACCESS_READ_ONLY,
    );
}

/// After init scripts run, update Context fields to match PS-created dicts.
fn sync_context_after_init(ctx: &mut Context) {
    use stet_core::dict::DictKey;
    use stet_core::object::PsValue;

    let sd = ctx.systemdict;
    let lookup = |ctx: &Context, name: &[u8]| -> Option<stet_core::object::EntityId> {
        let id = ctx.names.find(name)?;
        let obj = ctx.dicts.get(sd, &DictKey::Name(id))?;
        match obj.value {
            PsValue::Dict(e) => Some(e),
            _ => None,
        }
    };

    if let Some(e) = lookup(ctx, b"$error") {
        ctx.dollar_error = e;
    }
    if let Some(e) = lookup(ctx, b"errordict") {
        ctx.errordict = e;
    }
    if let Some(e) = lookup(ctx, b"FontDirectory") {
        ctx.font_directory = e;
    }
    if let Some(e) = lookup(ctx, b"userdict") {
        ctx.userdict = e;
    }
    if let Some(e) = lookup(ctx, b"globaldict") {
        ctx.globaldict = e;
    }
}
