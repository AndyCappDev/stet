// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! CLI entry point: file input or interactive REPL.

use std::io::Write;
use std::path::PathBuf;

use stet_core::context::Context;
use stet_core::eps::{read_eps_bounding_box, strip_dos_eps_header};
use stet_engine::eval::parse_and_exec;
use stet_ops::build_system_dict;
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
    let mut dpi: f64 = 300.0;
    let mut threads: Option<usize> = None;
    let mut file_args: Vec<&str> = Vec::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--dpi" => {
                if i + 1 < args.len() {
                    dpi = args[i + 1].parse().unwrap_or_else(|_| {
                        eprintln!("Error: invalid DPI value '{}'", args[i + 1]);
                        std::process::exit(1);
                    });
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
            _ => {}
        }
        file_args.push(&args[i]);
        i += 1;
    }

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

    let mut ctx = Context::new();
    ctx.exec_sync_fn = Some(stet_engine::eval::exec_sync);
    build_system_dict(&mut ctx);

    // Discover the resources/ directory and set paths
    let resource_path = find_resource_path();
    if let Some(ref rp) = resource_path {
        ctx.resource_base_path = Some(rp.clone());
        // Also set font_resource_path for backward compatibility
        let font_path = PathBuf::from(rp).join("Font");
        if font_path.is_dir() {
            ctx.font_resource_path = Some(font_path.to_string_lossy().to_string());
        }
    }

    // Run init scripts to bootstrap the resource system.
    run_init_scripts(&mut ctx);

    // Register device factory (before setpagedevice)
    ctx.device_factory = Some(Box::new(|w, h| Box::new(SkiaDevice::new(w, h))));

    if !file_args.is_empty() {
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
            ctx.output_path = Some(format!("{}.png", output_base));

            let source = match std::fs::read(filename) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Error: cannot read '{}': {}", filename, e);
                    std::process::exit(1);
                }
            };

            // Strip DOS EPS binary header if present
            let ps_data = strip_dos_eps_header(&source);
            let is_eps = filename_lower.ends_with(".eps") || filename_lower.ends_with(".epsf");

            let job_start = std::time::Instant::now();

            let exec_result = if is_eps {
                run_eps_file(&mut ctx, dpi, ps_data)
            } else {
                // Regular PS file — run as-is (DOS header still stripped)
                if job_idx == 0 {
                    install_png_device(&mut ctx, dpi);
                }
                parse_and_exec(&mut ctx, ps_data)
            };

            // Wait for any pipelined background render to complete before timing
            if let Some(ref mut device) = ctx.device
                && let Err(e) = device.finish()
            {
                eprintln!("render error: {}", e);
            }

            let job_duration = job_start.elapsed();

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
                        stet_core::error::PsError::Stop => {
                            // .error already populated $error; call handleerror
                            let _ = parse_and_exec(&mut ctx, b"{ handleerror } stopped pop");
                            eprintln!("Job {} FAILED: {}", job_idx + 1, display_name);
                        }
                        _ => {
                            eprintln!("Error: {}", e);
                            eprintln!("Job {} FAILED: {}", job_idx + 1, display_name);
                        }
                    }
                }
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
        print_stack(&ctx);
        eprintln!("\nexecution stack");
        print_exec_stack(&ctx);
    } else {
        // Interactive REPL
        let stdin = std::io::stdin();
        let mut line = String::new();
        loop {
            eprint!("PS> ");
            std::io::stderr().flush().ok();
            line.clear();
            match stdin.read_line(&mut line) {
                Ok(0) => break, // EOF
                Ok(_) => {
                    if let Err(e) = parse_and_exec(&mut ctx, line.as_bytes()) {
                        match e {
                            stet_core::error::PsError::Quit => break,
                            stet_core::error::PsError::Stop => {
                                let _ = parse_and_exec(&mut ctx, b"{ handleerror } stopped pop");
                            }
                            _ => eprintln!("Error: {}", e),
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Error reading input: {}", e);
                    break;
                }
            }
        }
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

/// Install the PNG device via `setpagedevice`.
///
/// Loads the device dict from `resources/OutputDevice/png.ps`, overrides
/// HWResolution with the requested DPI, and calls `setpagedevice`.
fn install_png_device(ctx: &mut Context, dpi: f64) {
    let setup = format!(
        "/png /OutputDevice findresource dup /HWResolution [{0} {0}] put setpagedevice",
        dpi
    );
    if let Err(e) = parse_and_exec(ctx, setup.as_bytes()) {
        // Fallback: set up device manually if resource system isn't available
        eprintln!(
            "Warning: setpagedevice via resource failed ({}), using fallback",
            e
        );
        install_png_device_fallback(ctx, dpi);
    }
}

/// Fallback device setup when the resource system isn't available.
fn install_png_device_fallback(ctx: &mut Context, dpi: f64) {
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

/// Run an EPS file with BoundingBox page sizing and automatic `showpage`.
fn run_eps_file(
    ctx: &mut Context,
    dpi: f64,
    ps_data: &[u8],
) -> Result<(), stet_core::error::PsError> {
    if let Some((llx, lly, urx, ury)) = read_eps_bounding_box(ps_data) {
        let w = urx - llx;
        let h = ury - lly;
        if w > 0.0 && h > 0.0 {
            // Install device with EPS bounding box dimensions
            install_png_device_with_size(ctx, dpi, w, h);
            // Translate origin if bbox doesn't start at (0,0)
            let wrapper = format!("gsave {} {} translate", -llx, -lly);
            parse_and_exec(ctx, wrapper.as_bytes())?;
            parse_and_exec(ctx, ps_data)?;
            parse_and_exec(ctx, b"grestore showpage")?;
            return Ok(());
        }
    }
    // No valid bbox — use default page size, add showpage
    install_png_device(ctx, dpi);
    parse_and_exec(ctx, ps_data)?;
    parse_and_exec(ctx, b"showpage")
}

/// Install the PNG device with a custom page size (for EPS bounding boxes).
fn install_png_device_with_size(ctx: &mut Context, dpi: f64, width: f64, height: f64) {
    let setup = format!(
        "/png /OutputDevice findresource dup /HWResolution [{0} {0}] put \
         dup /PageSize [{1} {2}] put setpagedevice",
        dpi, width, height
    );
    if let Err(e) = parse_and_exec(ctx, setup.as_bytes()) {
        eprintln!(
            "Warning: setpagedevice with size failed ({}), using fallback",
            e
        );
        install_png_device_fallback(ctx, dpi);
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
