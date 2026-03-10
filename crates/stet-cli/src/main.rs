// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! CLI entry point: file input or interactive REPL.

use std::io::Write;
use std::path::PathBuf;

use stet_core::context::Context;
use stet_core::eps::{content_is_epsf, read_eps_bounding_box, strip_dos_eps_header};
use stet_engine::eval::{parse_and_exec, parse_and_exec_file};
use stet_ops::build_system_dict;
use stet_pdf::PdfDevice;
use stet_pdf_reader::PdfDocument;
use stet_render::SkiaDevice;

/// A `Write` implementation that writes to a shared `Vec<u8>` behind a mutex.
struct SharedWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Parse flags
    let mut dpi: Option<f64> = None;
    let mut threads: Option<usize> = None;
    let mut device_name: Option<String> = None;
    let mut no_icc = false;
    let mut output_profile_path: Option<String> = None;
    let mut pages_spec: Option<String> = None;
    let mut file_args: Vec<String> = Vec::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--dpi" => {
                if i + 1 < args.len() {
                    dpi = Some(args[i + 1].parse().unwrap_or_else(|_| {
                        eprintln!("Error: invalid DPI value '{}'", args[i + 1]);
                        std::process::exit(1);
                    }));
                    i += 2;
                    continue;
                } else {
                    eprintln!("Error: --dpi requires a value");
                    std::process::exit(1);
                }
            }
            "--threads" => {
                if i + 1 < args.len() {
                    let n: usize = args[i + 1].parse().unwrap_or_else(|_| {
                        eprintln!("Error: invalid thread count '{}'", args[i + 1]);
                        std::process::exit(1);
                    });
                    if n == 0 {
                        eprintln!("Error: --threads must be at least 1");
                        std::process::exit(1);
                    }
                    threads = Some(n);
                    i += 2;
                    continue;
                } else {
                    eprintln!("Error: --threads requires a value");
                    std::process::exit(1);
                }
            }
            "--device" => {
                if i + 1 < args.len() {
                    device_name = Some(args[i + 1].clone());
                    i += 2;
                    continue;
                } else {
                    eprintln!("Error: --device requires a value");
                    std::process::exit(1);
                }
            }
            "--no-icc" => {
                no_icc = true;
                i += 1;
                continue;
            }
            "--output-profile" => {
                if i + 1 < args.len() {
                    output_profile_path = Some(args[i + 1].clone());
                    i += 2;
                    continue;
                } else {
                    eprintln!("Error: --output-profile requires a path");
                    std::process::exit(1);
                }
            }
            "--pages" => {
                if i + 1 < args.len() {
                    pages_spec = Some(args[i + 1].clone());
                    i += 2;
                    continue;
                } else {
                    eprintln!("Error: --pages requires a value (e.g., 1-5, 3, 1-3,7,10-12)");
                    std::process::exit(1);
                }
            }
            _ => {}
        }
        file_args.push(args[i].clone());
        i += 1;
    }

    // Determine the output device
    let device = device_name.unwrap_or_else(|| {
        if file_args.is_empty() {
            // REPL mode — no rendering device needed
            "png".to_string()
        } else if cfg!(feature = "viewer") {
            "viewer".to_string()
        } else {
            "png".to_string()
        }
    });

    // Configure rayon thread pool — default cap at 8 (sequential PNG writing
    // bottleneck means additional cores yield no speedup), override with --threads.
    let pool_size = threads.unwrap_or(8);
    rayon::ThreadPoolBuilder::new()
        .num_threads(pool_size)
        .build_global()
        .unwrap_or_else(|e| {
            eprintln!("Error: failed to set thread count: {}", e);
            std::process::exit(1);
        });

    // Parse --pages spec into a filter set
    let page_filter = pages_spec.map(|spec| {
        parse_page_ranges(&spec).unwrap_or_else(|e| {
            eprintln!("Error: {}", e);
            eprintln!("Expected format: 1-5, 3, 1-3,7,10-12");
            std::process::exit(1);
        })
    });

    match device.as_str() {
        "png" => {
            run_png_mode(dpi, file_args, no_icc, output_profile_path, page_filter);
        }
        "pdf" => {
            run_pdf_mode(dpi, file_args, no_icc, output_profile_path, page_filter);
        }
        "null" => {
            run_null_mode(dpi, file_args, no_icc, output_profile_path, page_filter);
        }
        #[cfg(feature = "viewer")]
        "viewer" => run_viewer_mode(dpi, file_args, no_icc, output_profile_path, page_filter),
        #[cfg(not(feature = "viewer"))]
        "viewer" => {
            eprintln!("Error: viewer not available (built without 'viewer' feature)");
            std::process::exit(1);
        }
        other => {
            eprintln!("Error: unknown device '{}'", other);
            eprintln!("Available devices: png, pdf, null, viewer");
            std::process::exit(1);
        }
    }
}

/// Run in PNG output mode (existing behavior).
fn run_png_mode(
    dpi_override: Option<f64>,
    file_args: Vec<String>,
    no_icc: bool,
    output_profile_path: Option<String>,
    page_filter: Option<std::collections::HashSet<i32>>,
) {
    // Check if all files are PDFs — use fast path (no PS interpreter needed)
    if !file_args.is_empty() && file_args.iter().all(|f| is_pdf_file(f)) {
        let dpi = dpi_override.unwrap_or(300.0);
        run_pdf_input_png(dpi, &file_args, &page_filter);
        return;
    }

    let mut ctx = create_context(no_icc, output_profile_path.as_deref());
    ctx.page_filter = page_filter;

    // Register device factory (before setpagedevice)
    let cmyk_bytes = ctx.icc_cache.system_cmyk_bytes().cloned();
    ctx.device_factory = Some(Box::new(move |w, h| {
        let mut dev = SkiaDevice::new(w, h);
        if let Some(ref bytes) = cmyk_bytes {
            dev.set_system_cmyk_bytes(bytes.clone());
        }
        Box::new(dev)
    }));

    if !file_args.is_empty() {
        run_file_jobs(&mut ctx, dpi_override, &file_args, "png", None, None);
    } else {
        run_repl(&mut ctx);
    }
}

/// Run in PDF output mode — vector PDF output.
fn run_pdf_mode(
    dpi_override: Option<f64>,
    file_args: Vec<String>,
    no_icc: bool,
    output_profile_path: Option<String>,
    page_filter: Option<std::collections::HashSet<i32>>,
) {
    // Read profile bytes for PDF embedding (create_context already validated the path)
    let output_profile_bytes: Option<Vec<u8>> = if !no_icc {
        output_profile_path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
    } else {
        None
    };

    let mut ctx = create_context(no_icc, output_profile_path.as_deref());
    ctx.page_filter = page_filter;
    let dpi_val = dpi_override.unwrap_or(300.0);

    ctx.device_factory = Some(Box::new(move |w, h| {
        let mut dev = PdfDevice::new(w, h, dpi_val);
        if let Some(ref bytes) = output_profile_bytes {
            dev.set_output_profile(bytes.clone());
        }
        Box::new(dev)
    }));

    if !file_args.is_empty() {
        run_file_jobs(&mut ctx, dpi_override, &file_args, "pdf", None, None);
    } else {
        eprintln!("Error: PDF device requires input files");
        std::process::exit(1);
    }
}

/// Run in null device mode — no rendering output, no user interaction.
///
/// Useful for running test suites and scripts that don't produce pages.
fn run_null_mode(
    dpi_override: Option<f64>,
    file_args: Vec<String>,
    no_icc: bool,
    output_profile_path: Option<String>,
    page_filter: Option<std::collections::HashSet<i32>>,
) {
    use stet_core::device::NullDevice;

    let mut ctx = create_context(no_icc, output_profile_path.as_deref());
    ctx.page_filter = page_filter;
    ctx.device_factory = Some(Box::new(|w, h| Box::new(NullDevice::new(w, h))));

    if !file_args.is_empty() {
        run_file_jobs(&mut ctx, dpi_override, &file_args, "null", None, None);
    } else {
        run_repl(&mut ctx);
    }
}

/// Run in viewer mode — interpreter on background thread, viewer on main thread.
///
/// The interpreter uses NullDevice (no rendering) and sends display lists to
/// the viewer via channels. The viewer renders visible viewport regions on
/// demand using `render_region()`.
#[cfg(feature = "viewer")]
fn run_viewer_mode(
    dpi_override: Option<f64>,
    file_args: Vec<String>,
    no_icc: bool,
    output_profile_path: Option<String>,
    page_filter: Option<std::collections::HashSet<i32>>,
) {
    use stet_core::device::NullDevice;

    if file_args.is_empty() {
        // REPL mode — no viewer, just run interactively
        let mut ctx = create_context(no_icc, output_profile_path.as_deref());
        ctx.device_factory = Some(Box::new(|w, h| Box::new(SkiaDevice::new(w, h))));
        run_repl(&mut ctx);
        return;
    }

    // Get CMYK profile bytes for ICC-aware viewer rendering.
    let system_cmyk_bytes = if !no_icc {
        if let Some(ref path) = output_profile_path {
            std::fs::read(path).ok().map(std::sync::Arc::new)
        } else {
            stet_core::icc::find_system_cmyk_profile_bytes()
        }
    } else {
        None
    };

    // PDF files: use fast path (no PS interpreter needed)
    if file_args.iter().all(|f| is_pdf_file(f)) {
        run_pdf_input_viewer(dpi_override, &file_args, &page_filter, system_cmyk_bytes);
        return;
    }

    let (interp_end, viewer_end, dl_sender, advance_rx) = stet_viewer::create_channels();
    let first_file = file_args.first().cloned();

    // Determine page size for the first file so the window is created at the
    // correct aspect ratio. On Wayland the compositor centers the window at
    // creation time and ignores later repositioning, so getting this right
    // upfront is essential.
    let first_page_size = first_file.as_deref().and_then(|path| {
        let lower = path.to_lowercase();
        if lower.ends_with(".eps") || lower.ends_with(".epsf") {
            let data = std::fs::read(path).ok()?;
            let ps_data = strip_dos_eps_header(&data);
            let (llx, lly, urx, ury) = read_eps_bounding_box(ps_data)?;
            let w = urx - llx;
            let h = ury - lly;
            if w > 0.0 && h > 0.0 {
                Some((w, h))
            } else {
                None
            }
        } else {
            None // PS files use default US Letter
        }
    });

    // Spawn relay thread: converts raw display list tuples from Context's
    // sender into PageReady messages for the viewer. Runs concurrently with
    // interpretation so pages appear in the viewer as they're produced.
    let page_sender = interp_end.page_sender;
    let dl_receiver = interp_end.dl_receiver;
    std::thread::spawn(move || {
        let mut page_num = 1u32;
        while let Ok((dl, dpi, w, h)) = dl_receiver.recv() {
            // Sentinel: zero dimensions = control message
            if w == 0 && h == 0 {
                if dpi < 0.0 {
                    // JobDone sentinel
                    let _ = page_sender.send(stet_viewer::ViewerMsg::JobDone);
                } else {
                    // NewJob sentinel
                    let _ = page_sender.send(stet_viewer::ViewerMsg::NewJob);
                    page_num = 1;
                }
                continue;
            }
            let _ = page_sender.send(stet_viewer::ViewerMsg::Page(stet_viewer::PageReady {
                display_list: dl,
                width: w,
                height: h,
                dpi,
                page_num,
            }));
            page_num += 1;
        }
        // dl_sender dropped (interpreter done) → loop ends → page_sender drops
        // → viewer sees Disconnected
    });

    // Spawn interpreter thread
    let _screen_info_receiver = interp_end.screen_info_receiver;
    std::thread::spawn(move || {
        let mut ctx = create_context(no_icc, output_profile_path.as_deref());
        ctx.page_filter = page_filter;

        // Set display_list_sender for incremental delivery at each showpage
        ctx.display_list_sender = Some(dl_sender);

        // NullDevice: no-op rendering — display list capture is the output
        ctx.device_factory = Some(Box::new(|w, h| Box::new(NullDevice::new(w, h))));

        // DPI comes from viewer.ps HWResolution by default, --dpi overrides
        run_file_jobs_viewer(&mut ctx, dpi_override, &file_args, advance_rx);
        // ctx drops here → display_list_sender drops → relay thread ends
    });

    // Wait for the first page before creating the viewer window.
    // If the interpreter finishes without producing any pages (e.g. unit tests,
    // nulldevice), skip the viewer entirely — no window flash.
    let page_rx = viewer_end.page_receiver;
    let screen_info_sender = viewer_end.screen_info_sender;
    let advance_sender = viewer_end.advance_sender;

    // Block until the first real page or disconnect
    let mut first_page = None;
    loop {
        match page_rx.recv() {
            Ok(msg @ stet_viewer::ViewerMsg::Page(_)) => {
                first_page = Some(msg);
                break;
            }
            Ok(_) => {
                // NewJob/JobDone control messages — keep waiting for a real page
                continue;
            }
            Err(_) => {
                // Interpreter done without producing any pages — no viewer needed
                break;
            }
        }
    }

    if let Some(first) = first_page {
        // Forward first page + remaining messages through a new channel
        let (fwd_tx, fwd_rx) = std::sync::mpsc::channel();
        fwd_tx.send(first).ok();
        std::thread::spawn(move || {
            for msg in page_rx {
                if fwd_tx.send(msg).is_err() {
                    break;
                }
            }
        });
        let new_viewer_end = stet_viewer::ViewerEnd {
            page_receiver: fwd_rx,
            screen_info_sender,
            advance_sender,
        };
        stet_viewer::run_viewer(
            new_viewer_end,
            dpi_override,
            first_file.as_deref(),
            first_page_size,
            system_cmyk_bytes,
        );
    }
    std::process::exit(0);
}

/// Parse a page range specification into a set of page numbers (1-based).
///
/// Supports single pages (`3`), ranges (`1-5`), and comma-separated
/// combinations (`1-3,7,10-12`).
fn parse_page_ranges(spec: &str) -> Result<std::collections::HashSet<i32>, String> {
    let mut pages = std::collections::HashSet::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((start_s, end_s)) = part.split_once('-') {
            let start: i32 = start_s
                .trim()
                .parse()
                .map_err(|_| format!("Invalid page range: '{}'", part))?;
            let end: i32 = end_s
                .trim()
                .parse()
                .map_err(|_| format!("Invalid page range: '{}'", part))?;
            if start < 1 || end < 1 {
                return Err(format!("Page numbers must be positive: '{}'", part));
            }
            if start > end {
                return Err(format!("Invalid page range (start > end): '{}'", part));
            }
            pages.extend(start..=end);
        } else {
            let num: i32 = part
                .parse()
                .map_err(|_| format!("Invalid page number: '{}'", part))?;
            if num < 1 {
                return Err(format!("Page numbers must be positive: '{}'", part));
            }
            pages.insert(num);
        }
    }
    if pages.is_empty() {
        return Err("Empty page range specification".to_string());
    }
    Ok(pages)
}

/// Create and initialize a Context with the resource system.
fn create_context(no_icc: bool, output_profile_path: Option<&str>) -> Context {
    let mut ctx = Context::new();
    if !no_icc {
        if let Some(path) = output_profile_path {
            // User-specified profile replaces system CMYK
            let bytes = std::fs::read(path).unwrap_or_else(|e| {
                eprintln!("Error: cannot read output profile '{}': {}", path, e);
                std::process::exit(1);
            });
            if bytes.len() < 40 || &bytes[36..40] != b"acsp" {
                eprintln!("Error: '{}' is not a valid ICC profile", path);
                std::process::exit(1);
            }
            if let Some(hash) = ctx.icc_cache.register_profile(&bytes) {
                eprintln!("[ICC] Loaded output profile: {}", path);
                ctx.icc_cache.set_system_cmyk(&bytes, hash);
            } else {
                eprintln!("Error: failed to parse ICC profile '{}'", path);
                std::process::exit(1);
            }
        } else {
            ctx.icc_cache.search_system_cmyk_profile();
        }
    }
    ctx.exec_sync_fn = Some(stet_engine::eval::exec_sync);
    build_system_dict(&mut ctx);

    // Discover the resources/ directory and set paths
    let resource_path = find_resource_path();
    if let Some(ref rp) = resource_path {
        ctx.resource_base_path = Some(rp.clone());
        let font_path = PathBuf::from(rp).join("Font");
        if font_path.is_dir() {
            ctx.font_resource_path = Some(font_path.to_string_lossy().to_string());
        }
    }

    // Run init scripts to bootstrap the resource system.
    run_init_scripts(&mut ctx);

    ctx
}

/// Run file jobs in viewer mode with per-job save/restore isolation.
#[cfg(feature = "viewer")]
fn run_file_jobs_viewer(
    ctx: &mut Context,
    dpi_override: Option<f64>,
    file_args: &[String],
    advance_rx: std::sync::mpsc::Receiver<()>,
) {
    run_file_jobs(
        ctx,
        dpi_override,
        file_args,
        "viewer",
        None,
        Some(&advance_rx),
    );
}

/// Run PostScript file jobs with per-job save/restore isolation.
///
/// `advance_receiver`: if provided (viewer mode), the interpreter waits
/// between jobs for the viewer to signal advancement.
fn run_file_jobs(
    ctx: &mut Context,
    dpi_override: Option<f64>,
    file_args: &[String],
    device: &str,
    viewer_wait: Option<&std::sync::Arc<std::sync::atomic::AtomicU64>>,
    advance_receiver: Option<&std::sync::mpsc::Receiver<()>>,
) {
    use stet_core::display_list::DisplayList;

    let num_jobs = file_args.len();

    for (job_idx, filename) in file_args.iter().enumerate() {
        let display_name = std::path::Path::new(filename)
            .canonicalize()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| filename.to_string());

        eprintln!("\n{}", "=".repeat(60));
        eprintln!(
            "Processing Job {}/{}: {}",
            job_idx + 1,
            num_jobs,
            display_name
        );
        eprintln!("{}", "=".repeat(60));

        let filename_lower = filename.to_ascii_lowercase();

        // Derive output path: strip known extensions, add .png
        let output_base = filename
            .strip_suffix(".ps")
            .or_else(|| filename.strip_suffix(".PS"))
            .or_else(|| filename.strip_suffix(".eps"))
            .or_else(|| filename.strip_suffix(".EPS"))
            .or_else(|| filename.strip_suffix(".epsf"))
            .or_else(|| filename.strip_suffix(".EPSF"))
            .unwrap_or(filename);
        let ext = if device == "pdf" { "pdf" } else { "png" };
        ctx.output_path = Some(format!("{}.{}", output_base, ext));

        let source = match std::fs::read(filename) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Error: cannot read '{}': {}", filename, e);
                std::process::exit(1);
            }
        };

        // Strip DOS EPS binary header if present
        let ps_data = strip_dos_eps_header(&source);
        let is_eps = filename_lower.ends_with(".eps")
            || filename_lower.ends_with(".epsf")
            || content_is_epsf(ps_data);

        // Signal new job to viewer (clear previous job's pages)
        if job_idx > 0
            && let Some(ref sender) = ctx.display_list_sender
        {
            let _ = sender.send((DisplayList::new(), 0.0, 0, 0));
        }

        let job_start = std::time::Instant::now();
        let wait_before = viewer_wait
            .map(|w| w.load(std::sync::atomic::Ordering::Relaxed))
            .unwrap_or(0);

        let exec_result = execjob(ctx, dpi_override, ps_data, filename, device, is_eps);

        let wait_after = viewer_wait
            .map(|w| w.load(std::sync::atomic::Ordering::Relaxed))
            .unwrap_or(0);
        let viewer_wait_dur = std::time::Duration::from_nanos(wait_after - wait_before);
        let job_duration = job_start.elapsed() - viewer_wait_dur;

        match exec_result {
            Ok(()) => {
                eprintln!(
                    "\nJob execution time: {:.3} seconds",
                    job_duration.as_secs_f64()
                );
                eprintln!(
                    "Job {} completed successfully: {}",
                    job_idx + 1,
                    display_name
                );
            }
            Err(e) => {
                eprintln!(
                    "\nJob execution time: {:.3} seconds",
                    job_duration.as_secs_f64()
                );
                match e {
                    stet_core::error::PsError::Quit => {
                        eprintln!("Job {} completed (quit): {}", job_idx + 1, display_name);
                    }
                    _ => {
                        eprintln!("Job {} FAILED: {}", job_idx + 1, display_name);
                    }
                }
            }
        }

        // Signal job done and wait for viewer to advance (between jobs only)
        if let Some(adv_rx) = advance_receiver
            && job_idx + 1 < num_jobs
        {
            if let Some(ref sender) = ctx.display_list_sender {
                let _ = sender.send((DisplayList::new(), -1.0, 0, 0));
            }
            // Block until viewer signals advance (or disconnects)
            let _ = adv_rx.recv();
        }
    }

    // Final summary
    eprintln!("\n{}", "=".repeat(60));
    eprintln!(
        "Processed {} job{}",
        num_jobs,
        if num_jobs == 1 { "" } else { "s" }
    );
    eprintln!("{}", "=".repeat(60));

    // Dump final stacks
    eprintln!("\nFinal operand stack:");
    print_stack(ctx);
    eprintln!("\nexecution stack");
    print_exec_stack(ctx);
}

/// Run the interactive REPL via PostScript's executive procedure.
fn run_repl(ctx: &mut Context) {
    match parse_and_exec(ctx, b"{executive} stopped pop") {
        Ok(()) => {}
        Err(stet_core::error::PsError::Quit) => {}
        Err(stet_core::error::PsError::Stop) => {}
        Err(e) => eprintln!("Error: {}", e),
    }
}

/// Execute a single PostScript job with PLRM 3.7.7 save/restore isolation.
///
/// Each job runs bracketed by `save`/`restore` so that state changes
/// (userdict definitions, graphics state, local VM mutations) don't bleed
/// across files.
fn execjob(
    ctx: &mut Context,
    dpi_override: Option<f64>,
    ps_data: &[u8],
    filename: &str,
    device_name: &str,
    is_eps: bool,
) -> Result<(), stet_core::error::PsError> {
    use stet_core::error::PsError;
    use stet_core::object::PsValue;

    // --- Job start (PLRM 3.7.7 steps 1-3) ---

    // 1. Save VM state
    let save_obj = ctx.vm_save();
    let save_id = match save_obj.value {
        PsValue::Save(sl) => sl.0,
        _ => unreachable!(),
    };

    // 2. Clear execution state
    ctx.o_stack.clear();
    ctx.e_stack.clear();
    ctx.loops.clear();

    // 3. Reset d_stack to base (systemdict, globaldict, userdict)
    ctx.d_stack.truncate(3);

    // 4. Reset graphics state
    let _ = parse_and_exec(ctx, b"initgraphics");

    // 5. Local VM allocation mode
    ctx.vm_alloc_mode = false;

    // 6. Clear transient state
    ctx.display_list.clear();
    ctx.in_error_handler = false;
    ctx.current_operator = None;

    // 7. Install device for this job
    if is_eps {
        if let Some((llx, lly, urx, ury)) = read_eps_bounding_box(ps_data) {
            let w = urx - llx;
            let h = ury - lly;
            if w > 0.0 && h > 0.0 {
                install_device_with_size(ctx, dpi_override, w, h, device_name);
                // Set trim box for PDF output (BoundingBox defines the artwork area)
                if let Some(ref mut dev) = ctx.device {
                    dev.set_trim_box(0.0, 0.0, w, h);
                }
            } else {
                install_device(ctx, dpi_override, device_name);
            }
        } else {
            install_device(ctx, dpi_override, device_name);
        }
    } else {
        install_device(ctx, dpi_override, device_name);
    }

    // --- Job execution (step 4) ---
    let exec_result = if is_eps {
        (|| {
            if let Some((llx, lly, _urx, _ury)) = read_eps_bounding_box(ps_data) {
                if llx != 0.0 || lly != 0.0 {
                    let wrapper = format!("gsave {} {} translate", -llx, -lly);
                    parse_and_exec(ctx, wrapper.as_bytes())?;
                    parse_and_exec_file(ctx, ps_data, filename)?;
                    parse_and_exec(ctx, b"grestore showpage")
                } else {
                    parse_and_exec_file(ctx, ps_data, filename)?;
                    parse_and_exec(ctx, b"showpage")
                }
            } else {
                parse_and_exec_file(ctx, ps_data, filename)?;
                parse_and_exec(ctx, b"showpage")
            }
        })()
    } else {
        parse_and_exec_file(ctx, ps_data, filename)
    };

    // --- Error handling ---
    let job_result = match &exec_result {
        Err(PsError::Stop) => {
            let _ = parse_and_exec(ctx, b"{ handleerror } stopped pop");
            exec_result
        }
        _ => exec_result,
    };

    // --- Job cleanup (always runs, like PostForge's _cleanup_job finally) ---

    // 1. Flush device BEFORE restore (restore reverts gstate.page_device)
    if let Some(mut dev) = ctx.device.take() {
        if let Err(e) = dev.finish_with_context(&ctx) {
            eprintln!("render error: {}", e);
        }
        ctx.device = Some(dev);
    }

    // 2. Clear execution state
    ctx.o_stack.clear();
    ctx.e_stack.clear();
    ctx.loops.clear();
    ctx.d_stack.truncate(3);

    // 3. Restore VM (reverts local VM + graphics state)
    let _ = ctx.vm_restore(save_id);

    // 4. Clear display list (rendering state, not VM)
    ctx.display_list.clear();

    // 5. Reset transient error state
    ctx.in_error_handler = false;
    ctx.current_operator = None;

    job_result
}

/// Run init scripts to bootstrap the resource system, error handlers, and
/// encoding/font definitions. If init fails, warn but continue with
/// Rust-only mode (all Rust operators are still functional as fallbacks).
fn run_init_scripts(ctx: &mut Context) {
    // sysdict.ps expects systemdict as the ONLY dict on the stack —
    // it creates and pushes globaldict + userdict itself.
    // Save the original d_stack so we can restore it on failure.
    let saved_d_stack = ctx.d_stack.clone();
    ctx.d_stack.truncate(1); // keep only systemdict

    // Capture stdout to detect "Init failed" from the stopped handler
    let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let old_stdout = std::mem::replace(&mut ctx.stdout, Box::new(SharedWriter(captured.clone())));

    let init_script = b"{(resources/Init/sysdict.ps) run} stopped { (Init failed\\n) print } if";
    let exec_ok = match parse_and_exec(ctx, init_script) {
        Ok(()) => true,
        Err(e) => {
            match e {
                stet_core::error::PsError::Quit => {}
                _ => eprintln!("Warning: init script error: {}", e),
            }
            false
        }
    };

    // Restore original stdout and check if init failed
    ctx.stdout = old_stdout;
    let output = captured.lock().unwrap();
    let init_failed = !exec_ok || output.windows(11).any(|w| w == b"Init failed");
    if !output.is_empty() {
        // Forward any captured output
        use std::io::Write;
        let _ = ctx.stdout.write_all(&output);
    }
    drop(output);

    if !init_failed && ctx.d_stack.len() >= 3 {
        // Init succeeded — sync Context fields to PS-created dicts
        sync_context_after_init(ctx);
    } else {
        // Init failed or left d_stack in a bad state — restore original
        ctx.d_stack = saved_d_stack;
        ctx.o_stack.clear();
        ctx.e_stack.clear();
    }

    // Ensure sane state regardless of init success/failure:
    // - VM allocation mode should be local (false) after init
    // - End initialization phase — enable access checks
    // - Set systemdict to read-only (matches PostForge behavior)
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

/// Install the output device via `setpagedevice`.
///
/// If `dpi_override` is `Some`, overwrite the device's HWResolution.
/// Otherwise, use the HWResolution from the device's .ps resource file.
fn install_device(ctx: &mut Context, dpi_override: Option<f64>, device: &str) {
    if device == "null" {
        let _ = parse_and_exec(ctx, b"nulldevice");
        return;
    }
    let resource_name = match device {
        "viewer" => "viewer",
        "pdf" => "pdf",
        _ => "png",
    };
    // Copy the resource dict before modifying HWResolution — the original is
    // in global VM and must not be mutated (would bleed into subsequent jobs).
    let setup = if let Some(dpi) = dpi_override {
        format!(
            "/{0} /OutputDevice findresource dup length dict copy \
             dup /HWResolution [{1} {1}] put setpagedevice",
            resource_name, dpi
        )
    } else {
        format!(
            "/{} /OutputDevice findresource setpagedevice",
            resource_name
        )
    };
    // Temporarily allow HWResolution changes so the CLI's own DPI override
    // isn't blocked by the PS-program filter in merge_request_dict.
    let saved = ctx.allow_ps_resolution;
    ctx.allow_ps_resolution = true;
    let result = parse_and_exec(ctx, setup.as_bytes());
    ctx.allow_ps_resolution = saved;
    if let Err(e) = result {
        eprintln!(
            "Warning: setpagedevice via resource failed ({}), using fallback",
            e
        );
        install_device_fallback(ctx, dpi_override.unwrap_or(300.0));
    }
}

/// Fallback device setup when the resource system isn't available.
fn install_device_fallback(ctx: &mut Context, dpi: f64) {
    use stet_core::graphics_state::Matrix;

    let scale = dpi / 72.0;
    let dev_width = (612.0 * scale).round() as u32;
    let dev_height = (792.0 * scale).round() as u32;

    let device = SkiaDevice::new(dev_width, dev_height);
    ctx.device = Some(Box::new(device));

    let default_ctm = Matrix::new(scale, 0.0, 0.0, -scale, 0.0, dev_height as f64);
    ctx.gstate.ctm = default_ctm;
    ctx.gstate.default_ctm = default_ctm;
}

/// Install the device with a custom page size (for EPS bounding boxes).
fn install_device_with_size(
    ctx: &mut Context,
    dpi_override: Option<f64>,
    width: f64,
    height: f64,
    device: &str,
) {
    if device == "null" {
        let _ = parse_and_exec(ctx, b"nulldevice");
        return;
    }
    let resource_name = match device {
        "viewer" => "viewer",
        "pdf" => "pdf",
        _ => "png",
    };
    // Copy the resource dict before modifying PageSize — the original is in
    // global VM and must not be mutated (would bleed into subsequent jobs).
    let setup = if let Some(dpi) = dpi_override {
        format!(
            "/{0} /OutputDevice findresource dup length dict copy \
             dup /HWResolution [{1} {1}] put \
             dup /PageSize [{2} {3}] put setpagedevice",
            resource_name, dpi, width, height
        )
    } else {
        format!(
            "/{0} /OutputDevice findresource dup length dict copy \
             dup /PageSize [{1} {2}] put setpagedevice",
            resource_name, width, height
        )
    };
    // Temporarily allow HWResolution changes so the CLI's own DPI override
    // isn't blocked by the PS-program filter in merge_request_dict.
    let saved = ctx.allow_ps_resolution;
    ctx.allow_ps_resolution = true;
    let result = parse_and_exec(ctx, setup.as_bytes());
    ctx.allow_ps_resolution = saved;
    if let Err(e) = result {
        eprintln!(
            "Warning: setpagedevice with size failed ({}), using fallback",
            e
        );
        install_device_fallback(ctx, dpi_override.unwrap_or(300.0));
    }
}

/// Print the operand stack contents to stderr (PostForge format).
fn print_stack(ctx: &Context) {
    let slice = ctx.o_stack.as_slice();
    if slice.is_empty() {
        eprintln!("[]");
        return;
    }
    let mut buf = Vec::new();
    buf.push(b'[');
    for (i, obj) in slice.iter().enumerate() {
        if i > 0 {
            buf.extend_from_slice(b", ");
        }
        stet_ops::type_ops::write_obj_equal(ctx, obj, &mut buf);
    }
    buf.push(b']');
    std::io::stderr().write_all(&buf).ok();
    eprintln!();
}

/// Print the execution stack contents to stderr (PostForge format).
fn print_exec_stack(ctx: &Context) {
    let slice = ctx.e_stack.as_slice();
    if slice.is_empty() {
        eprintln!("[]");
        return;
    }
    let mut buf = Vec::new();
    buf.push(b'[');
    for (i, obj) in slice.iter().enumerate() {
        if i > 0 {
            buf.extend_from_slice(b", ");
        }
        stet_ops::type_ops::write_obj_equal(ctx, obj, &mut buf);
    }
    buf.push(b']');
    std::io::stderr().write_all(&buf).ok();
    eprintln!();
}

/// Check if a filename has a PDF extension.
fn is_pdf_file(filename: &str) -> bool {
    filename.to_ascii_lowercase().ends_with(".pdf")
}

/// Render PDF files to PNG output.
fn run_pdf_input_png(
    dpi: f64,
    file_args: &[String],
    page_filter: &Option<std::collections::HashSet<i32>>,
) {
    for filename in file_args {
        let data = std::fs::read(filename).unwrap_or_else(|e| {
            eprintln!("Error: cannot read '{}': {}", filename, e);
            std::process::exit(1);
        });

        let doc = PdfDocument::from_bytes(&data).unwrap_or_else(|e| {
            eprintln!("Error: cannot parse '{}': {}", filename, e);
            std::process::exit(1);
        });

        let output_base = filename
            .strip_suffix(".pdf")
            .or_else(|| filename.strip_suffix(".PDF"))
            .unwrap_or(filename);

        let start = std::time::Instant::now();
        let page_count = doc.page_count();
        eprintln!("\n{}", "=".repeat(60));
        eprintln!("Processing PDF: {} ({} pages)", filename, page_count);
        eprintln!("{}", "=".repeat(60));

        for page in 0..page_count {
            let page_1based = page as i32 + 1;
            if let Some(filter) = page_filter
                && !filter.contains(&page_1based)
            {
                continue;
            }

            match doc.render_page_to_rgba(page, dpi) {
                Ok((rgba, w, h)) => {
                    let out_path = if page_count == 1 {
                        format!("{}.png", output_base)
                    } else {
                        format!("{}-{:03}.png", output_base, page_1based)
                    };
                    write_png_file(&out_path, &rgba, w, h);
                    eprintln!("  Page {}: {}x{} → {}", page_1based, w, h, out_path);
                }
                Err(e) => {
                    eprintln!("  Page {}: render error: {}", page_1based, e);
                }
            }
        }

        eprintln!(
            "PDF render time: {:.3} seconds",
            start.elapsed().as_secs_f64()
        );
    }
}

/// Write RGBA data to a PNG file.
fn write_png_file(path: &str, rgba: &[u8], width: u32, height: u32) {
    let file = std::fs::File::create(path).unwrap_or_else(|e| {
        eprintln!("Error: cannot create '{}': {}", path, e);
        std::process::exit(1);
    });
    let w = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(w, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(rgba).unwrap();
}

/// Render PDF files to the viewer.
#[cfg(feature = "viewer")]
fn run_pdf_input_viewer(
    dpi_override: Option<f64>,
    file_args: &[String],
    page_filter: &Option<std::collections::HashSet<i32>>,
    system_cmyk_bytes: Option<std::sync::Arc<Vec<u8>>>,
) {
    let (interp_end, viewer_end, dl_sender, advance_rx) = stet_viewer::create_channels();

    let first_file = file_args.first().cloned();

    // Try to get first page size for proper window sizing
    let first_page_size = first_file.as_deref().and_then(|path| {
        let data = std::fs::read(path).ok()?;
        let doc = PdfDocument::from_bytes(&data).ok()?;
        doc.page_size(0).ok()
    });

    // Relay thread: convert display list tuples to ViewerMsg
    let page_sender = interp_end.page_sender;
    let dl_receiver = interp_end.dl_receiver;
    std::thread::spawn(move || {
        let mut page_num = 1u32;
        while let Ok((dl, dpi, w, h)) = dl_receiver.recv() {
            if w == 0 && h == 0 {
                if dpi < 0.0 {
                    let _ = page_sender.send(stet_viewer::ViewerMsg::JobDone);
                } else {
                    let _ = page_sender.send(stet_viewer::ViewerMsg::NewJob);
                    page_num = 1;
                }
                continue;
            }
            let _ = page_sender.send(stet_viewer::ViewerMsg::Page(stet_viewer::PageReady {
                display_list: dl,
                width: w,
                height: h,
                dpi,
                page_num,
            }));
            page_num += 1;
        }
    });

    // PDF rendering thread
    let file_args_owned: Vec<String> = file_args.to_vec();
    let page_filter_owned = page_filter.clone();
    let dpi_override_copy = dpi_override;
    std::thread::spawn(move || {
        let dpi = dpi_override_copy.unwrap_or(150.0);

        for (job_idx, filename) in file_args_owned.iter().enumerate() {
            if job_idx > 0 {
                // Signal new job
                let _ = dl_sender.send((stet_core::display_list::DisplayList::new(), 0.0, 0, 0));
            }

            let data = match std::fs::read(filename) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("Error: cannot read '{}': {}", filename, e);
                    continue;
                }
            };

            let doc = match PdfDocument::from_bytes(&data) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("Error: cannot parse '{}': {}", filename, e);
                    continue;
                }
            };

            let page_count = doc.page_count();
            eprintln!("PDF: {} ({} pages)", filename, page_count);

            for page in 0..page_count {
                let page_1based = page as i32 + 1;
                if let Some(ref filter) = page_filter_owned
                    && !filter.contains(&page_1based)
                {
                    continue;
                }

                match doc.render_page(page, dpi) {
                    Ok(display_list) => {
                        let (w, h) = doc.page_size(page).unwrap_or((612.0, 792.0));
                        let scale = dpi / 72.0;
                        let pixel_w = (w * scale).round() as u32;
                        let pixel_h = (h * scale).round() as u32;
                        let _ = dl_sender.send((display_list, dpi, pixel_w, pixel_h));
                    }
                    Err(e) => {
                        eprintln!("  Page {}: render error: {}", page_1based, e);
                    }
                }
            }

            // Signal job done and wait for advance between jobs
            if job_idx + 1 < file_args_owned.len() {
                let _ = dl_sender.send((stet_core::display_list::DisplayList::new(), -1.0, 0, 0));
                let _ = advance_rx.recv();
            }
        }
        // dl_sender drops → relay thread ends → page_sender drops → viewer sees disconnect
    });

    // Wait for first page before creating viewer window
    let page_rx = viewer_end.page_receiver;
    let screen_info_sender = viewer_end.screen_info_sender;
    let advance_sender = viewer_end.advance_sender;

    let mut first_page = None;
    loop {
        match page_rx.recv() {
            Ok(msg @ stet_viewer::ViewerMsg::Page(_)) => {
                first_page = Some(msg);
                break;
            }
            Ok(_) => continue,
            Err(_) => break,
        }
    }

    if let Some(first) = first_page {
        let (fwd_tx, fwd_rx) = std::sync::mpsc::channel();
        fwd_tx.send(first).ok();
        std::thread::spawn(move || {
            for msg in page_rx {
                if fwd_tx.send(msg).is_err() {
                    break;
                }
            }
        });
        let new_viewer_end = stet_viewer::ViewerEnd {
            page_receiver: fwd_rx,
            screen_info_sender,
            advance_sender,
        };
        stet_viewer::run_viewer(
            new_viewer_end,
            dpi_override,
            first_file.as_deref(),
            first_page_size,
            system_cmyk_bytes,
        );
    }
    std::process::exit(0);
}

/// Locate the `resources/` directory relative to the executable.
///
/// Walks up from the executable's directory (up to 5 levels) looking for a
/// `resources/` subdirectory. Does NOT search CWD — that would pick up
/// other projects' resources when running from their directories.
fn find_resource_path() -> Option<String> {
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent().map(PathBuf::from);
        for _ in 0..5 {
            if let Some(ref d) = dir {
                let candidate = d.join("resources");
                if candidate.is_dir() {
                    return Some(candidate.to_string_lossy().to_string());
                }
                dir = d.parent().map(PathBuf::from);
            }
        }
    }

    None
}
