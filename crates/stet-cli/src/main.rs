// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! CLI entry point: file input or interactive REPL.

use std::io::Write;
use std::path::PathBuf;

use stet_core::context::Context;
use stet_core::eps::{content_is_epsf, read_eps_bounding_box, strip_dos_eps_header};
use stet_engine::eval::{parse_and_exec, parse_and_exec_file};
use stet_graphics::icc::{BpcMode, IccCacheOptions};
use stet_ops::build_system_dict;
use stet_pdf::PdfDevice;
use stet_pdf_reader::PdfDocument;
use stet_render::SkiaDevice;

/// CLI-level ICC configuration: aggregates `--no-icc`, `--output-profile`,
/// `--cmyk-profile`, and `--bpc` into a single value passed through the
/// rendering modes. Cheap to clone.
#[derive(Clone, Default)]
struct IccCliConfig {
    no_icc: bool,
    output_profile_path: Option<String>,
    cmyk_profile_path: Option<String>,
    bpc_mode: BpcMode,
    /// When true, prefer the PDF's embedded `/OutputIntents[].DestOutputProfile`
    /// over the system-default CMYK profile (unless `--cmyk-profile` is also
    /// set, which always wins). Off by default because it changes the sRGB
    /// output for every CMYK pixel and can expose CMYK-math drift that the
    /// GS default profile happens to mask.
    use_output_intent: bool,
}

impl IccCliConfig {
    /// Resolved source CMYK profile path: `--cmyk-profile` wins over
    /// `--output-profile` when both are given. Used as the "source CMYK"
    /// override; `--output-profile` continues to control PDF embedding bytes
    /// independently.
    fn source_cmyk_path(&self) -> Option<&str> {
        self.cmyk_profile_path
            .as_deref()
            .or(self.output_profile_path.as_deref())
    }
}

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
    // winit's Wayland backend (0.30+) doesn't support drag-and-drop.
    // Force X11 backend (via XWayland) by hiding WAYLAND_DISPLAY so winit
    // falls back to X11 where XDnD file drops work.
    #[cfg(target_os = "linux")]
    if std::env::var("WAYLAND_DISPLAY").is_ok() && std::env::var("DISPLAY").is_ok() {
        // SAFETY: called at program start before any other threads exist.
        unsafe { std::env::remove_var("WAYLAND_DISPLAY") };
    }

    let args: Vec<String> = std::env::args().collect();

    // Parse flags
    let mut dpi: Option<f64> = None;
    let mut threads: Option<usize> = None;
    let mut device_name: Option<String> = None;
    let mut no_icc = false;
    let mut no_aa = false;
    let mut output_profile_path: Option<String> = None;
    let mut cmyk_profile_path: Option<String> = None;
    let mut bpc_mode = BpcMode::Auto;
    let mut bpc_explicit = false;
    // Default: honour the PDF's declared OutputIntent as the CMYK→sRGB
    // source profile. Matches Acrobat's behaviour for PDF/X files and
    // eliminates profile-approximation artefacts on GWG swatches (e.g.
    // the near-invisible Hue/Sat/Color X on 16.2). `--no-output-intent`
    // reverts to the system CMYK profile for comparison renders.
    let mut use_output_intent = true;
    let mut pages_spec: Option<String> = None;
    let mut password: Option<String> = None;
    let mut target_width: Option<u32> = None;
    let mut target_height: Option<u32> = None;
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
            "--use-output-intent" => {
                // Kept for backward compat; OutputIntent is on by default now.
                use_output_intent = true;
                i += 1;
                continue;
            }
            "--no-output-intent" => {
                use_output_intent = false;
                i += 1;
                continue;
            }
            "--no-aa" => {
                no_aa = true;
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
            "--cmyk-profile" => {
                if i + 1 < args.len() {
                    cmyk_profile_path = Some(args[i + 1].clone());
                    i += 2;
                    continue;
                } else {
                    eprintln!("Error: --cmyk-profile requires a path");
                    std::process::exit(1);
                }
            }
            "--bpc" => {
                if i + 1 < args.len() {
                    bpc_mode = match args[i + 1].as_str() {
                        "on" => BpcMode::On,
                        "off" => BpcMode::Off,
                        "auto" => BpcMode::Auto,
                        other => {
                            eprintln!(
                                "Error: --bpc must be one of: on, off, auto (got '{}')",
                                other
                            );
                            std::process::exit(1);
                        }
                    };
                    bpc_explicit = true;
                    i += 2;
                    continue;
                } else {
                    eprintln!("Error: --bpc requires a value (on|off|auto)");
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
            "--password" => {
                if i + 1 < args.len() {
                    password = Some(args[i + 1].clone());
                    i += 2;
                    continue;
                } else {
                    eprintln!("Error: --password requires a value");
                    std::process::exit(1);
                }
            }
            "--width" => {
                if i + 1 < args.len() {
                    target_width = Some(args[i + 1].parse().unwrap_or_else(|_| {
                        eprintln!("Error: invalid --width value '{}'", args[i + 1]);
                        std::process::exit(1);
                    }));
                    if target_width == Some(0) {
                        eprintln!("Error: --width must be at least 1");
                        std::process::exit(1);
                    }
                    i += 2;
                    continue;
                } else {
                    eprintln!("Error: --width requires a pixel value");
                    std::process::exit(1);
                }
            }
            "--height" => {
                if i + 1 < args.len() {
                    target_height = Some(args[i + 1].parse().unwrap_or_else(|_| {
                        eprintln!("Error: invalid --height value '{}'", args[i + 1]);
                        std::process::exit(1);
                    }));
                    if target_height == Some(0) {
                        eprintln!("Error: --height must be at least 1");
                        std::process::exit(1);
                    }
                    i += 2;
                    continue;
                } else {
                    eprintln!("Error: --height requires a pixel value");
                    std::process::exit(1);
                }
            }
            _ => {}
        }
        file_args.push(args[i].clone());
        i += 1;
    }

    if no_icc && cmyk_profile_path.is_some() {
        eprintln!("Error: --cmyk-profile cannot be combined with --no-icc");
        std::process::exit(1);
    }
    if no_icc && bpc_explicit {
        eprintln!("Error: --bpc cannot be combined with --no-icc");
        std::process::exit(1);
    }
    if (target_width.is_some() || target_height.is_some()) && dpi.is_some() {
        eprintln!("Error: --width/--height cannot be combined with --dpi");
        std::process::exit(1);
    }

    let _ = bpc_explicit; // already consumed by the conflict check above
    let icc_cfg = IccCliConfig {
        no_icc,
        output_profile_path,
        cmyk_profile_path,
        bpc_mode,
        use_output_intent,
    };

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

    // Configure rayon thread pool. Viewer uses 75% of cores (no PNG bottleneck);
    // other modes cap at 8 (sequential PNG writing limits additional core benefit).
    // --threads overrides either default.
    let default_pool_size = if device == "viewer" {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(8);
        (cpus * 3 / 4).max(1)
    } else {
        8
    };
    let pool_size = threads.unwrap_or(default_pool_size);
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

    if (target_width.is_some() || target_height.is_some())
        && !matches!(device.as_str(), "png" | "viewport-png")
    {
        eprintln!(
            "Error: --width/--height is only supported for --device png (got '{}')",
            device
        );
        std::process::exit(1);
    }

    match device.as_str() {
        "png" => {
            run_png_mode(
                dpi,
                file_args,
                &icc_cfg,
                no_aa,
                page_filter,
                false,
                password.as_deref(),
                target_width,
                target_height,
            );
        }
        "viewport-png" => {
            // Audit path: renders through the viewport pipeline instead of
            // the banded page pipeline. Used by the visual test runner to
            // exercise the viewer's render path against the same baselines.
            run_png_mode(
                dpi,
                file_args,
                &icc_cfg,
                no_aa,
                page_filter,
                true,
                password.as_deref(),
                target_width,
                target_height,
            );
        }
        "pdf" => {
            // `--device pdf` produces PDF from PostScript input — no PDF
            // input path here, so --password does not apply.
            let _ = &password;
            run_pdf_mode(dpi, file_args, &icc_cfg, no_aa, page_filter);
        }
        "null" => {
            run_null_mode(dpi, file_args, &icc_cfg, no_aa, page_filter);
        }
        #[cfg(feature = "viewer")]
        "viewer" => run_viewer_mode(dpi, file_args, &icc_cfg, no_aa, page_filter, password),
        #[cfg(not(feature = "viewer"))]
        "viewer" => {
            eprintln!("Error: viewer not available (built without 'viewer' feature)");
            std::process::exit(1);
        }
        other => {
            eprintln!("Error: unknown device '{}'", other);
            eprintln!("Available devices: png, viewport-png, pdf, null, viewer");
            std::process::exit(1);
        }
    }
}

/// Run in PNG output mode. When `use_viewport` is true, rendering is routed
/// through the viewport pipeline (same code path the interactive viewer
/// uses) instead of the banded full-page pipeline — this is the audit mode
/// behind `--device viewport-png`.
#[allow(clippy::too_many_arguments)]
fn run_png_mode(
    dpi_override: Option<f64>,
    file_args: Vec<String>,
    icc_cfg: &IccCliConfig,
    no_aa: bool,
    page_filter: Option<std::collections::HashSet<i32>>,
    use_viewport: bool,
    password: Option<&str>,
    target_width: Option<u32>,
    target_height: Option<u32>,
) {
    // Check if all files are PDFs — use fast path (no PS interpreter needed)
    if !file_args.is_empty() && file_args.iter().all(|f| is_pdf_file(f)) {
        let dpi = dpi_override.unwrap_or(300.0);
        run_pdf_input_png(
            dpi,
            &file_args,
            &page_filter,
            no_aa,
            use_viewport,
            icc_cfg,
            password,
            target_width,
            target_height,
        );
        return;
    }

    if target_width.is_some() || target_height.is_some() {
        eprintln!(
            "Error: --width/--height is not yet supported for PostScript input — use --dpi for now"
        );
        std::process::exit(1);
    }

    let mut ctx = create_context(icc_cfg);
    ctx.page_filter = page_filter;

    // Register device factory (before setpagedevice)
    let cmyk_bytes = ctx.icc_cache.system_cmyk_bytes().cloned();
    ctx.device_factory = Some(Box::new(move |w, h| {
        let mut dev = SkiaDevice::new(w, h);
        if let Some(ref bytes) = cmyk_bytes {
            dev.set_system_cmyk_bytes(bytes.clone());
        }
        dev.set_no_aa(no_aa);
        dev.set_use_viewport_path(use_viewport);
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
    icc_cfg: &IccCliConfig,
    _no_aa: bool,
    page_filter: Option<std::collections::HashSet<i32>>,
) {
    let mut ctx = create_context(icc_cfg);
    ctx.page_filter = page_filter;
    let dpi_val = dpi_override.unwrap_or(300.0);

    ctx.device_factory = Some(Box::new(move |w, h| {
        Box::new(PdfDevice::new(w, h, dpi_val))
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
    icc_cfg: &IccCliConfig,
    _no_aa: bool,
    page_filter: Option<std::collections::HashSet<i32>>,
) {
    use stet_core::device::NullDevice;

    let mut ctx = create_context(icc_cfg);
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
    icc_cfg: &IccCliConfig,
    no_aa: bool,
    page_filter: Option<std::collections::HashSet<i32>>,
    cli_password: Option<String>,
) {
    use stet_core::device::NullDevice;

    // Get CMYK profile bytes for ICC-aware viewer rendering.
    // --cmyk-profile takes precedence over --output-profile when both are set.
    let system_cmyk_bytes = if !icc_cfg.no_icc {
        if let Some(path) = icc_cfg.source_cmyk_path() {
            std::fs::read(path).ok().map(std::sync::Arc::new)
        } else {
            stet_graphics::icc::find_system_cmyk_profile_bytes()
        }
    } else {
        None
    };

    let (
        interp_end,
        viewer_end,
        dl_sender,
        advance_rx,
        file_drop_rx,
        interrupt_flag,
        password_response_rx,
    ) = stet_viewer::create_channels();
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
    // A clone of `page_sender` also goes to the interpreter thread so it
    // can post `PasswordRequired` prompts for encrypted PDFs directly.
    let page_sender = interp_end.page_sender;
    let page_sender_for_interp = page_sender.clone();
    let dl_receiver = interp_end.dl_receiver;
    std::thread::spawn(move || {
        let mut page_num = 1u32;
        while let Ok((dl, dpi, w, h, cmyk_bytes)) = dl_receiver.recv() {
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
                cmyk_bytes,
            }));
            page_num += 1;
        }
        // dl_sender dropped (interpreter done) → loop ends → page_sender drops
        // → viewer sees Disconnected
    });

    // Spawn interpreter thread
    let _screen_info_receiver = interp_end.screen_info_receiver;
    let icc_cfg_thread = icc_cfg.clone();
    let interrupt_flag_thread = interrupt_flag.clone();
    let password_response_rx_thread = password_response_rx;
    let page_sender_thread = page_sender_for_interp;
    let cli_password_thread = cli_password;
    std::thread::spawn(move || {
        let mut ctx = create_context(&icc_cfg_thread);
        ctx.page_filter = page_filter;
        ctx.interrupt_flag = Some(interrupt_flag_thread.clone());

        // Set display_list_sender for incremental delivery at each showpage
        ctx.display_list_sender = Some(dl_sender);

        // NullDevice: no-op rendering — display list capture is the output
        ctx.device_factory = Some(Box::new(|w, h| Box::new(NullDevice::new(w, h))));

        if file_args.is_empty() {
            // REPL mode with viewer: install a default device, run the REPL,
            // and send display lists to the viewer as showpage is called.
            install_device(&mut ctx, dpi_override, "png");
            run_repl(&mut ctx);

            // REPL done — signal JobDone, then accept dropped files
            if let Some(ref sender) = ctx.display_list_sender {
                let _ = sender.send((
                    stet_graphics::display_list::DisplayList::new(),
                    -1.0,
                    0,
                    0,
                    None,
                ));
            }
        } else {
            // Process initial CLI files: PDF files go direct, PS/EPS through interpreter
            let ps_files: Vec<String> = file_args
                .iter()
                .filter(|f| !is_pdf_file(f))
                .cloned()
                .collect();
            let pdf_files: Vec<String> = file_args
                .iter()
                .filter(|f| is_pdf_file(f))
                .cloned()
                .collect();

            // Render PDF files first (no interpreter needed)
            for (i, path) in pdf_files.iter().enumerate() {
                if i > 0 || !ps_files.is_empty() {
                    if let Some(ref sender) = ctx.display_list_sender {
                        let _ = sender.send((
                            stet_graphics::display_list::DisplayList::new(),
                            0.0,
                            0,
                            0,
                            None,
                        ));
                    }
                }
                if let Some(ref sender) = ctx.display_list_sender {
                    render_dropped_pdf(
                        path,
                        dpi_override,
                        sender,
                        &ctx.icc_cache,
                        icc_cfg_thread.use_output_intent,
                        &interrupt_flag_thread,
                        Some(&page_sender_thread),
                        Some(&password_response_rx_thread),
                        cli_password_thread.as_deref(),
                    );
                }
            }

            // Render PS/EPS files through interpreter
            if !ps_files.is_empty() {
                if !pdf_files.is_empty() {
                    if let Some(ref sender) = ctx.display_list_sender {
                        let _ = sender.send((
                            stet_graphics::display_list::DisplayList::new(),
                            0.0,
                            0,
                            0,
                            None,
                        ));
                    }
                }
                run_file_jobs_viewer(&mut ctx, dpi_override, &ps_files, advance_rx);
            }

            // CLI files done — send final JobDone
            if let Some(ref sender) = ctx.display_list_sender {
                let _ = sender.send((
                    stet_graphics::display_list::DisplayList::new(),
                    -1.0,
                    0,
                    0,
                    None,
                ));
            }
        }

        // Wait for dropped files (works for both REPL and file-based paths)
        // Use the explicit --dpi override if given; otherwise let the viewer
        // OutputDevice resource supply its default (300 DPI).  Don't inherit
        // from the post-restore page device — that reverts to 72 and would
        // override the resource's own HWResolution.
        let established_dpi = dpi_override;

        while let Ok(mut path) = file_drop_rx.recv() {
            loop {
                // Drain any additional paths queued while we were blocked or
                // parsing — only the most recent drop matters.
                while let Ok(newer) = file_drop_rx.try_recv() {
                    path = newer;
                }

                // Clear the interrupt flag now that we've picked up the
                // latest path. Any *further* drops during parsing will set
                // it again and abort the job.
                interrupt_flag_thread.store(false, std::sync::atomic::Ordering::Relaxed);

                let sender = match ctx.display_list_sender {
                    Some(ref s) => s.clone(),
                    None => return,
                };

                // Signal new job so viewer clears old pages
                let _ = sender.send((
                    stet_graphics::display_list::DisplayList::new(),
                    0.0,
                    0,
                    0,
                    None,
                ));

                if is_pdf_file(&path) {
                    render_dropped_pdf(
                        &path,
                        established_dpi,
                        &sender,
                        &ctx.icc_cache,
                        icc_cfg_thread.use_output_intent,
                        &interrupt_flag_thread,
                        Some(&page_sender_thread),
                        Some(&password_response_rx_thread),
                        None,
                    );
                } else {
                    run_file_jobs(
                        &mut ctx,
                        established_dpi,
                        &[path.clone()],
                        "viewer",
                        None,
                        None,
                    );
                }

                // Signal job done
                let _ = sender.send((
                    stet_graphics::display_list::DisplayList::new(),
                    -1.0,
                    0,
                    0,
                    None,
                ));

                // If the job was interrupted by a new drop, the next path is
                // already (or about to be) in the channel — loop back and
                // grab it without blocking on recv().
                if interrupt_flag_thread.load(std::sync::atomic::Ordering::Relaxed) {
                    match file_drop_rx.try_recv() {
                        Ok(next) => {
                            path = next;
                            continue;
                        }
                        Err(_) => {
                            // Flag set but channel hadn't delivered yet — race
                            // is rare; fall through to outer recv().
                            break;
                        }
                    }
                }
                break;
            }
        }
        // file_drop_sender dropped (viewer closed) → loop ends → ctx drops
    });

    // Wait for the first page before creating the viewer window.
    // If the interpreter finishes without producing any pages (e.g. unit tests,
    // nulldevice), skip the viewer entirely — no window flash.
    let page_rx = viewer_end.page_receiver;
    let screen_info_sender = viewer_end.screen_info_sender;
    let advance_sender = viewer_end.advance_sender;

    // Block until the first real page, a password prompt, or disconnect.
    // A `PasswordRequired` is enough reason to open the viewer — the modal
    // must be drawn before the user can respond.
    let mut first_event: Option<stet_viewer::ViewerMsg> = None;
    loop {
        match page_rx.recv() {
            Ok(msg @ stet_viewer::ViewerMsg::Page(_)) => {
                first_event = Some(msg);
                break;
            }
            Ok(msg @ stet_viewer::ViewerMsg::PasswordRequired { .. }) => {
                first_event = Some(msg);
                break;
            }
            Ok(stet_viewer::ViewerMsg::JobDone) => {
                // All CLI files processed without producing pages — no viewer needed
                break;
            }
            Ok(_) => {
                // NewJob and other control messages — keep waiting
                continue;
            }
            Err(_) => {
                // Interpreter done without producing any pages — no viewer needed
                break;
            }
        }
    }

    if let Some(first) = first_event {
        // Forward first event + remaining messages through a new channel
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
            file_drop_sender: viewer_end.file_drop_sender,
            interrupt_flag: viewer_end.interrupt_flag,
            password_response_sender: viewer_end.password_response_sender,
        };
        stet_viewer::run_viewer(
            new_viewer_end,
            dpi_override,
            first_file.as_deref(),
            first_page_size,
            system_cmyk_bytes,
            no_aa,
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

/// Build an [`IccCache`](stet_graphics::icc::IccCache) from the CLI config.
///
/// Resolution rules:
/// - `--no-icc` ⇒ empty cache, BPC mode forced to `Off`.
/// - `--cmyk-profile <path>` overrides `--output-profile` for the source CMYK
///   profile. The path is read and validated as a 4-component CMYK ICC.
/// - `--output-profile <path>` (without `--cmyk-profile`) preserves prior
///   behavior: bytes serve as both the source CMYK profile and the embedded
///   PDF output profile. Validated as a generic ICC (`acsp` magic) only.
/// - Otherwise the system CMYK profile is searched in the standard locations.
fn build_icc_cache(icc_cfg: &IccCliConfig) -> stet_graphics::icc::IccCache {
    use stet_graphics::icc::IccCache;

    if icc_cfg.no_icc {
        return IccCache::new_with_options(IccCacheOptions {
            bpc_mode: BpcMode::Off,
            source_cmyk_profile: None,
        });
    }

    if let Some(path) = icc_cfg.cmyk_profile_path.as_deref() {
        let bytes = std::fs::read(path).unwrap_or_else(|e| {
            eprintln!("Error: cannot read --cmyk-profile '{}': {}", path, e);
            std::process::exit(1);
        });
        validate_cmyk_icc(&bytes, path);
        eprintln!("[ICC] Loaded source CMYK profile: {}", path);
        return IccCache::new_with_options(IccCacheOptions {
            bpc_mode: icc_cfg.bpc_mode,
            source_cmyk_profile: Some(bytes),
        });
    }

    if let Some(path) = icc_cfg.output_profile_path.as_deref() {
        let bytes = std::fs::read(path).unwrap_or_else(|e| {
            eprintln!("Error: cannot read output profile '{}': {}", path, e);
            std::process::exit(1);
        });
        if bytes.len() < 40 || &bytes[36..40] != b"acsp" {
            eprintln!("Error: '{}' is not a valid ICC profile", path);
            std::process::exit(1);
        }
        eprintln!("[ICC] Loaded output profile: {}", path);
        return IccCache::new_with_options(IccCacheOptions {
            bpc_mode: icc_cfg.bpc_mode,
            source_cmyk_profile: Some(bytes),
        });
    }

    let mut cache = IccCache::new_with_options(IccCacheOptions {
        bpc_mode: icc_cfg.bpc_mode,
        source_cmyk_profile: None,
    });
    cache.search_system_cmyk_profile();
    cache
}

/// Validate that an ICC profile byte slice is a 4-component CMYK profile.
/// Exits the process on failure.
fn validate_cmyk_icc(bytes: &[u8], path: &str) {
    if bytes.len() < 40 || &bytes[36..40] != b"acsp" {
        eprintln!("Error: '{}' is not a valid ICC profile", path);
        std::process::exit(1);
    }
    // ICC header: data color space at offset 16..20.
    if &bytes[16..20] != b"CMYK" {
        let cs = String::from_utf8_lossy(&bytes[16..20]);
        eprintln!(
            "Error: --cmyk-profile '{}' has data color space '{}'; expected CMYK",
            path,
            cs.trim()
        );
        std::process::exit(1);
    }
}

/// Create and initialize a Context with the resource system.
fn create_context(icc_cfg: &IccCliConfig) -> Context {
    let mut ctx = Context::new();
    // Replace the default IccCache with one configured per the CLI options.
    // This is where `--bpc` lands; commits 2-3 of docs/PLAN-BPC.md will turn
    // the stored mode into actual conversion-time behavior.
    ctx.icc_cache = build_icc_cache(icc_cfg);
    ctx.exec_sync_fn = Some(stet_engine::eval::exec_sync);
    build_system_dict(&mut ctx);

    // Register the embedded resource tree (init scripts, encodings, fonts,
    // CMaps, ICC profile) into the virtual filesystem. This is what the init
    // scripts and findresource lookups consume — without it, the CLI would
    // have to rely on an external resources/ directory.
    stet::embedded_resources::register_all(&mut ctx.files);
    ctx.font_resource_path = Some("Font".to_string());

    // If a filesystem resources/ tree is adjacent to the binary (development
    // builds, explicit install layouts), expose it so users can override or
    // extend the embedded set with their own fonts/resources.
    if let Some(rp) = find_resource_path() {
        ctx.resource_base_path = Some(rp.clone());
        let font_path = PathBuf::from(&rp).join("Font");
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
    use stet_graphics::display_list::DisplayList;

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
            let _ = sender.send((DisplayList::new(), 0.0, 0, 0, None));
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
                let _ = sender.send((DisplayList::new(), -1.0, 0, 0, None));
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

    // 2. Record job start save depth (for startjob condition 3)
    ctx.job_start_save_depth = ctx.save_stack.depth();

    // 3. Clear execution state
    ctx.o_stack.clear();
    ctx.e_stack.clear();
    ctx.loops.clear();

    // 4. Reset d_stack to base (systemdict, globaldict, userdict)
    ctx.d_stack.truncate(3);

    // 5. Reset graphics state
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
    // Check $error/newerror to distinguish real errors from clean exits
    // (quit sets newerror=false before calling stop).
    let job_result = match &exec_result {
        Err(PsError::Stop) => {
            if is_newerror_set(ctx) {
                let _ = parse_and_exec(ctx, b"{ handleerror } stopped pop");
                exec_result
            } else {
                // Clean stop (e.g. quit) — not an error
                Ok(())
            }
        }
        _ => exec_result,
    };

    // --- Job cleanup (always runs, like a finally-block) ---

    // 1. Flush device BEFORE restore (restore reverts gstate.page_device)
    if let Some(mut dev) = ctx.device.take() {
        if let Err(e) = dev.finish_with_context(ctx) {
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

/// Check if `$error/newerror` is true (indicates a real error, not a clean quit).
fn is_newerror_set(ctx: &Context) -> bool {
    use stet_core::dict::DictKey;
    use stet_core::object::PsValue;
    let newerror_id = ctx
        .names
        .find(b"newerror")
        .unwrap_or(stet_core::object::NameId(0));
    match ctx.dicts.get(ctx.dollar_error, &DictKey::Name(newerror_id)) {
        Some(obj) => matches!(obj.value, PsValue::Bool(true)),
        None => true, // If we can't check, assume error
    }
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
    // - Set systemdict to read-only
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
    use stet_fonts::geometry::Matrix;

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

/// Print the operand stack contents to stderr, each object formatted
/// as `==` would render it, wrapped in `[…]` with comma separators.
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

/// Print the execution stack contents to stderr in the same `[…]` form
/// as `print_stack`.
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

/// Render a dropped PDF file and send its pages through the display list channel.
///
/// When the PDF is password-protected, posts a `ViewerMsg::PasswordRequired`
/// via `page_sender` and blocks on `password_response_rx` for the user's
/// reply; loops until the PDF opens, the user cancels, or the interrupt
/// flag fires. The caller-supplied `initial_password` (from `--password`)
/// is tried first; if absent or wrong, the prompt is used. `page_sender`
/// and `password_response_rx` may be `None` for headless callers (no
/// viewer) — in that case only `initial_password` is tried and failure
/// is reported to stderr.
#[allow(clippy::too_many_arguments)]
fn render_dropped_pdf(
    path: &str,
    dpi_override: Option<f64>,
    dl_sender: &std::sync::mpsc::Sender<stet_viewer::DisplayListMsg>,
    icc_cache: &stet_graphics::icc::IccCache,
    use_output_intent: bool,
    interrupt_flag: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    page_sender: Option<&std::sync::mpsc::Sender<stet_viewer::ViewerMsg>>,
    password_response_rx: Option<&std::sync::mpsc::Receiver<Option<String>>>,
    initial_password: Option<&str>,
) {
    let dpi = dpi_override.unwrap_or(150.0);

    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Error: cannot read '{}': {}", path, e);
            return;
        }
    };

    let mut password_attempt: Option<String> = initial_password.map(|s| s.to_string());
    let mut any_password_tried = false;
    let mut doc = loop {
        let result = match password_attempt.as_deref() {
            None => PdfDocument::from_bytes_with_icc(&data, icc_cache.clone()),
            Some(pw) => {
                PdfDocument::from_bytes_with_password(&data, icc_cache.clone(), pw.as_bytes())
            }
        };
        match result {
            Ok(d) => break d,
            Err(stet_pdf_reader::PdfError::PasswordRequired) => {
                let (Some(tx), Some(rx)) = (page_sender, password_response_rx) else {
                    eprintln!(
                        "Error: '{}' is password-protected (use --password or drop onto viewer)",
                        path
                    );
                    return;
                };
                if tx
                    .send(stet_viewer::ViewerMsg::PasswordRequired {
                        filename: path.to_string(),
                        retry: any_password_tried,
                    })
                    .is_err()
                {
                    return;
                }
                match rx.recv() {
                    Ok(Some(pw)) => {
                        password_attempt = Some(pw);
                        any_password_tried = true;
                        if interrupt_flag.load(std::sync::atomic::Ordering::Relaxed) {
                            return;
                        }
                        continue;
                    }
                    _ => return,
                }
            }
            Err(e) => {
                eprintln!("Error: cannot parse '{}': {}", path, e);
                return;
            }
        }
    };
    if use_output_intent && doc.apply_output_intent_as_default_cmyk() {
        eprintln!("[ICC] Using PDF OutputIntent profile for {}", path);
    }
    // Snapshot the effective CMYK bytes (post-OI-apply) so the viewer's
    // render-time ICC cache matches the one used to bake the display list.
    let effective_cmyk_bytes = doc.icc_cache().system_cmyk_bytes().cloned();

    let page_count = doc.page_count();
    eprintln!("PDF: {} ({} pages)", path, page_count);

    let start = std::time::Instant::now();
    for page in 0..page_count {
        if interrupt_flag.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        match doc.render_page(page, dpi) {
            Ok(display_list) => {
                let (w, h) = doc.page_size(page).unwrap_or((612.0, 792.0));
                let scale = dpi / 72.0;
                let pixel_w = (w * scale).round() as u32;
                let pixel_h = (h * scale).round() as u32;
                let _ = dl_sender.send((
                    display_list,
                    dpi,
                    pixel_w,
                    pixel_h,
                    effective_cmyk_bytes.clone(),
                ));
            }
            Err(e) => {
                eprintln!("  Page {}: render error: {}", page + 1, e);
            }
        }
    }
    eprintln!(
        "PDF interpret time: {:.3} seconds",
        start.elapsed().as_secs_f64()
    );
}

/// Render PDF files to PNG output.
/// Render a single PDF page to RGBA with configurable anti-aliasing.
/// When `use_viewport` is true, rendering is routed through the viewport
/// pipeline so visual tests can audit that path against the same baseline.
/// Compute aspect-preserving fit dimensions.
///
/// Given the source page's point dimensions and optional target pixel
/// dimensions (at least one of which must be `Some`), returns
/// `(output_width_px, output_height_px, effective_dpi)`. The output
/// aspect ratio matches the input; when both targets are given, the
/// smaller of the two scale factors wins (the page fits *inside* the
/// target box).
fn compute_fit_dims(
    page_w_pt: f64,
    page_h_pt: f64,
    target_w: Option<u32>,
    target_h: Option<u32>,
) -> (u32, u32, f64) {
    let scale = match (target_w, target_h) {
        (Some(w), Some(h)) => {
            let sx = w as f64 / page_w_pt;
            let sy = h as f64 / page_h_pt;
            sx.min(sy)
        }
        (Some(w), None) => w as f64 / page_w_pt,
        (None, Some(h)) => h as f64 / page_h_pt,
        (None, None) => {
            // Caller contract: at least one must be Some. This branch is
            // unreachable in correct CLI flow.
            return (page_w_pt.round() as u32, page_h_pt.round() as u32, 72.0);
        }
    };
    let dpi = scale * 72.0;
    let out_w = ((page_w_pt * scale).round().max(1.0)) as u32;
    let out_h = ((page_h_pt * scale).round().max(1.0)) as u32;
    (out_w, out_h, dpi)
}

#[allow(clippy::too_many_arguments)]
fn render_pdf_page_to_rgba(
    doc: &PdfDocument,
    page: usize,
    dpi: f64,
    no_aa: bool,
    use_viewport: bool,
    target_width: Option<u32>,
    target_height: Option<u32>,
) -> Result<(Vec<u8>, u32, u32), stet_pdf_reader::PdfError> {
    let (page_w, page_h) = doc.page_size(page)?;
    let (pixel_w, pixel_h, effective_dpi) = if target_width.is_some() || target_height.is_some() {
        compute_fit_dims(page_w, page_h, target_width, target_height)
    } else {
        let scale = dpi / 72.0;
        (
            (page_w * scale).round() as u32,
            (page_h * scale).round() as u32,
            dpi,
        )
    };
    let display_list = doc.render_page(page, effective_dpi)?;
    let rgba = if use_viewport {
        stet_render::render_to_rgba_viewport(
            &display_list,
            pixel_w,
            pixel_h,
            effective_dpi,
            Some(doc.icc_cache()),
            no_aa,
        )
    } else {
        stet_render::render_to_rgba(
            &display_list,
            pixel_w,
            pixel_h,
            effective_dpi,
            Some(doc.icc_cache()),
            no_aa,
        )
    };
    Ok((rgba, pixel_w, pixel_h))
}

#[allow(clippy::too_many_arguments)]
fn run_pdf_input_png(
    dpi: f64,
    file_args: &[String],
    page_filter: &Option<std::collections::HashSet<i32>>,
    no_aa: bool,
    use_viewport: bool,
    icc_cfg: &IccCliConfig,
    password: Option<&str>,
    target_width: Option<u32>,
    target_height: Option<u32>,
) {
    let icc_cache = build_icc_cache(icc_cfg);

    for filename in file_args {
        let data = std::fs::read(filename).unwrap_or_else(|e| {
            eprintln!("Error: cannot read '{}': {}", filename, e);
            std::process::exit(1);
        });

        let open_result = match password {
            Some(pw) => {
                PdfDocument::from_bytes_with_password(&data, icc_cache.clone(), pw.as_bytes())
            }
            None => PdfDocument::from_bytes_with_icc(&data, icc_cache.clone()),
        };
        let mut doc = open_result.unwrap_or_else(|e| {
            match e {
                stet_pdf_reader::PdfError::PasswordRequired => eprintln!(
                    "Error: '{}' is password-protected (use --password)",
                    filename
                ),
                _ => eprintln!("Error: cannot parse '{}': {}", filename, e),
            }
            std::process::exit(1);
        });
        // Opt-in: when `--use-output-intent` is set and the user didn't pin a
        // source CMYK profile via `--cmyk-profile`/`--output-profile`, prefer
        // the PDF's own `/OutputIntents[].DestOutputProfile`. Gated because
        // changing the CMYK→sRGB profile shifts every pixel and can expose
        // small CMYK-math drift that the system-default profile happens to
        // mask (e.g. GWG overprint swatches on PDFX-ready_Output-Test).
        if icc_cfg.use_output_intent
            && icc_cfg.source_cmyk_path().is_none()
            && doc.apply_output_intent_as_default_cmyk()
        {
            eprintln!("[ICC] Using PDF OutputIntent profile for {}", filename);
        }

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

            match render_pdf_page_to_rgba(
                &doc,
                page,
                dpi,
                no_aa,
                use_viewport,
                target_width,
                target_height,
            ) {
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
    encoder.set_compression(png::Compression::Default);
    encoder.set_adaptive_filter(png::AdaptiveFilterType::Adaptive);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(rgba).unwrap();
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

#[cfg(test)]
mod tests {
    use super::compute_fit_dims;

    const LETTER_W: f64 = 612.0;
    const LETTER_H: f64 = 792.0;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn fit_letter_portrait_into_256_square_is_height_constrained() {
        // Portrait Letter into 256x256 box: height 792pt is the long side,
        // so height caps at 256px; width scales proportionally.
        let (w, h, dpi) = compute_fit_dims(LETTER_W, LETTER_H, Some(256), Some(256));
        assert_eq!(h, 256);
        assert_eq!(w, 198); // 612 * 256/792 = 197.8...
        assert!(close(dpi, 256.0 * 72.0 / 792.0));
    }

    #[test]
    fn fit_letter_landscape_into_256_square_is_width_constrained() {
        let (w, h, dpi) = compute_fit_dims(LETTER_H, LETTER_W, Some(256), Some(256));
        assert_eq!(w, 256);
        assert_eq!(h, 198);
        assert!(close(dpi, 256.0 * 72.0 / 792.0));
    }

    #[test]
    fn width_only_preserves_aspect() {
        let (w, h, dpi) = compute_fit_dims(LETTER_W, LETTER_H, Some(512), None);
        assert_eq!(w, 512);
        assert_eq!(h, 663); // 792 * 512/612 = 662.588 → rounds to 663
        assert!(close(dpi, 512.0 * 72.0 / 612.0));
    }

    #[test]
    fn height_only_preserves_aspect() {
        let (w, h, dpi) = compute_fit_dims(LETTER_W, LETTER_H, None, Some(512));
        assert_eq!(h, 512);
        assert_eq!(w, 396); // 612 * 512/792 = 395.6...
        assert!(close(dpi, 512.0 * 72.0 / 792.0));
    }

    #[test]
    fn fit_preserves_minimum_dimension_of_one() {
        // A very tall skinny page fit into a box where the width would
        // round to zero should still produce at least 1x1.
        let (w, h, _dpi) = compute_fit_dims(1.0, 10000.0, Some(32), Some(32));
        assert!(w >= 1);
        assert!(h >= 1);
    }

    #[test]
    fn thumbnailer_128_bucket_letter_portrait() {
        // XDG "normal" bucket — matches what file manager thumbnailers request.
        let (w, h, dpi) = compute_fit_dims(LETTER_W, LETTER_H, Some(128), Some(128));
        assert_eq!(w, 99);
        assert_eq!(h, 128);
        assert!(close(dpi, 128.0 * 72.0 / 792.0)); // ~11.6
    }
}
