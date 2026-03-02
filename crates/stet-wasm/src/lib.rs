// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! WebAssembly bindings for the stet PostScript interpreter.
//!
//! Provides `Interpreter` (a fully initialized PostScript context with embedded
//! resources) and `render()` (renders PostScript/EPS data to RGBA pages).

mod embedded_resources;
mod memory_sink;

use std::sync::{Arc, Mutex};

use wasm_bindgen::prelude::*;

use stet_core::context::Context;
use stet_core::device::OutputDevice;
use stet_core::eps::read_eps_bounding_box;
use stet_core::error::PsError;
use stet_engine::eval::parse_and_exec;
use stet_render::SkiaDevice;

use memory_sink::{MemorySinkFactory, PageData, set_page_ready_callback};

/// A fully initialized PostScript interpreter context.
///
/// Created once via `create_interpreter()`, reused across `render()` calls.
#[wasm_bindgen]
pub struct Interpreter {
    ctx: Context,
}

/// Result of rendering a PostScript file: one or more RGBA pages.
#[wasm_bindgen]
pub struct RenderResult {
    pages: Vec<PageData>,
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

#[wasm_bindgen]
impl RenderResult {
    /// Number of rendered pages.
    #[wasm_bindgen(getter)]
    pub fn page_count(&self) -> u32 {
        self.pages.len() as u32
    }

    /// Get a specific page by index.
    pub fn get_page(&mut self, index: u32) -> Option<Page> {
        let i = index as usize;
        if i < self.pages.len() {
            let page = &mut self.pages[i];
            Some(Page {
                width: page.width,
                height: page.height,
                rgba: std::mem::take(&mut page.rgba),
            })
        } else {
            None
        }
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
    log("stet: creating context...");
    let mut ctx = Context::new();

    log("stet: wiring exec_sync...");
    ctx.exec_sync_fn = Some(stet_engine::eval::exec_sync);

    log("stet: building system dict...");
    stet_ops::build_system_dict(&mut ctx);

    log("stet: registering embedded resources...");
    embedded_resources::register_embedded_resources(&mut ctx.files);

    ctx.font_resource_path = Some("Font".to_string());
    ctx.stdout = Box::new(NullWriter);

    ctx.device_factory = Some(Box::new(|w, h| {
        Box::new(SkiaDevice::new(w, h)) as Box<dyn OutputDevice>
    }));

    log("stet: running init scripts...");
    run_embedded_init_scripts(&mut ctx);

    log("stet: interpreter ready");
    Interpreter { ctx }
}

/// Register a JS callback that fires after each page is rendered.
///
/// The callback receives (index, width, height, rgbaUint8Array).
/// Used by the Web Worker to stream pages to the main thread during rendering.
#[wasm_bindgen]
pub fn set_page_callback(callback: &js_sys::Function) {
    let callback = callback.clone();
    set_page_ready_callback(Some(Box::new(move |index, width, height, rgba| {
        let arr = js_sys::Uint8Array::from(rgba);
        let args = js_sys::Array::new();
        args.push(&JsValue::from(index));
        args.push(&JsValue::from(width));
        args.push(&JsValue::from(height));
        args.push(&arr.into());
        let _ = callback.apply(&JsValue::NULL, &args);
    })));
}

/// Clear the page callback.
#[wasm_bindgen]
pub fn clear_page_callback() {
    set_page_ready_callback(None);
}

/// Render PostScript or EPS data at the specified DPI.
///
/// Returns a `RenderResult` containing one or more rendered pages.
/// The interpreter state is reset after rendering so it can be reused.
#[wasm_bindgen]
pub fn render(interp: &mut Interpreter, ps_data: &[u8], dpi: f64, filename: &str) -> Result<RenderResult, JsValue> {
    log(&format!("stet: render() called — {} bytes, dpi={}, file={}", ps_data.len(), dpi, filename));
    let ctx = &mut interp.ctx;

    // Set up shared page collection for the memory sink
    let (_sink_factory, pages_ref) = MemorySinkFactory::new();
    // _sink_factory is unused — device_factory creates sinks via from_shared(pages_ref)

    // Strip DOS EPS header and check for EPS bounding box
    let ps_data = stet_core::eps::strip_dos_eps_header(ps_data);

    // Use EPS mode only when the file extension is .eps or .epsf (matching CLI behavior)
    let filename_lower = filename.to_ascii_lowercase();
    let is_eps = filename_lower.ends_with(".eps") || filename_lower.ends_with(".epsf");

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

        // Set up device_factory with MemorySinkFactory, then use setpagedevice
        let pages_for_factory = pages_ref.clone();
        ctx.device_factory = Some(Box::new(move |w, h| {
            let factory = MemorySinkFactory::from_shared(pages_for_factory.clone());
            Box::new(SkiaDevice::with_sink_factory(w, h, Box::new(factory)))
                as Box<dyn OutputDevice>
        }));

        install_device_via_setpagedevice(ctx, dpi, w, h)
            .map_err(|e| JsValue::from_str(&format!("Device setup error: {}", e)))?;

        let save_obj = ctx.vm_save();
        let save_id = match save_obj.value {
            stet_core::object::PsValue::Save(stet_core::object::SaveLevel(id)) => id,
            _ => unreachable!(),
        };
        let wrapper = format!("gsave {} {} translate", -llx, -lly);
        parse_and_exec(ctx, wrapper.as_bytes())
            .map_err(|e| JsValue::from_str(&format!("PS error (translate): {}", e)))?;

        log(&format!(
            "stet: EPS before exec — display_list={}",
            ctx.display_list.elements().len()
        ));
        parse_and_exec(ctx, ps_data)
            .map_err(|e| JsValue::from_str(&format!("PS error (exec): {}", e)))?;
        log(&format!(
            "stet: EPS after exec — display_list={} device={}",
            ctx.display_list.elements().len(),
            ctx.device.is_some()
        ));

        // grestore to undo our translate; only call showpage if the EPS didn't already
        let need_showpage = pages_ref.lock().map(|g| g.is_empty()).unwrap_or(true);
        if need_showpage {
            parse_and_exec(ctx, b"grestore showpage")
                .map_err(|e| JsValue::from_str(&format!("PS error (showpage): {}", e)))?;
        } else {
            parse_and_exec(ctx, b"grestore")
                .map_err(|e| JsValue::from_str(&format!("PS error (grestore): {}", e)))?;
        }

        finish_device(ctx);
        let pages = extract_pages(&pages_ref);
        let _ = ctx.vm_restore(save_id);
        reset_context(ctx);
        return Ok(RenderResult { pages });
    }

    // Non-EPS or no valid bounding box: standard page rendering
    // Set up device_factory with MemorySinkFactory, then use setpagedevice
    // (matching CLI behavior for proper EndPage/BeginPage continuation on multi-page files)
    let pages_for_factory = pages_ref.clone();
    ctx.device_factory = Some(Box::new(move |w, h| {
        let factory = MemorySinkFactory::from_shared(pages_for_factory.clone());
        Box::new(SkiaDevice::with_sink_factory(w, h, Box::new(factory))) as Box<dyn OutputDevice>
    }));

    install_device_via_setpagedevice(ctx, dpi, 612.0, 792.0)
        .map_err(|e| JsValue::from_str(&format!("Device setup error: {}", e)))?;

    // Wrap execution in save/restore to isolate VM changes between renders
    let save_obj = ctx.vm_save();
    let save_id = match save_obj.value {
        stet_core::object::PsValue::Save(stet_core::object::SaveLevel(id)) => id,
        _ => unreachable!(),
    };
    match parse_and_exec(ctx, ps_data) {
        Ok(()) => {}
        Err(PsError::Quit) => {
            log("stet: after exec — Quit");
        }
        Err(e) => {
            log(&format!(
                "stet: render error: {} | o_stack={} e_stack={} d_stack={}",
                e,
                ctx.o_stack.len(),
                ctx.e_stack.len(),
                ctx.d_stack.len()
            ));
            // Dump top of e_stack for debugging
            for i in 0..ctx.e_stack.len().min(10) {
                if let Ok(obj) = ctx.e_stack.peek(i) {
                    log(&format!("  e_stack[{}]: {:?}", i, obj.value));
                }
            }
            // Try to salvage any rendered pages before reporting error
            finish_device(ctx);
            let pages = extract_pages(&pages_ref);
            let _ = ctx.vm_restore(save_id);
            reset_context(ctx);
            if pages.is_empty() {
                return Err(JsValue::from_str(&format!("PS error: {}", e)));
            }
            return Ok(RenderResult { pages });
        }
    }

    finish_device(ctx);
    let pages = extract_pages(&pages_ref);
    let _ = ctx.vm_restore(save_id);
    reset_context(ctx);
    Ok(RenderResult { pages })
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
    if let Some(ref mut device) = ctx.device {
        let _ = device.finish();
    }
}

/// Extract collected pages from the shared page buffer.
fn extract_pages(pages_ref: &Arc<Mutex<Vec<PageData>>>) -> Vec<PageData> {
    match pages_ref.lock() {
        Ok(mut guard) => std::mem::take(&mut *guard),
        Err(e) => std::mem::take(&mut *e.into_inner()),
    }
}

/// Reset interpreter state for the next render call.
fn reset_context(ctx: &mut Context) {
    ctx.device = None;
    ctx.output_path = None;
    ctx.display_list.clear();
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
