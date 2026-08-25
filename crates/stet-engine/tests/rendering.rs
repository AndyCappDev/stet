// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Integration tests for Phase 3 graphics rendering.
//!
//! Each test executes a PostScript program with a SkiaDevice attached,
//! then inspects the resulting PNG output to verify correct rendering.

use stet_core::context::Context;
use stet_core::geometry::Matrix;
use stet_render::SkiaDevice;

/// Create a rendering context with a SkiaDevice and default CTM.
fn render_ctx(width: u32, height: u32) -> Context {
    let device = SkiaDevice::new(width, height);
    let mut ctx = Context::new();
    stet_ops::build_system_dict(&mut ctx);
    ctx.exec_sync_fn = Some(stet_engine::eval::exec_sync);
    ctx.device = Some(Box::new(device));
    ctx.page_width = width;
    ctx.page_height = height;
    // Default CTM: Y-flip so PS origin is at bottom-left
    let ctm = Matrix {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: -1.0,
        tx: 0.0,
        ty: height as f64,
    };
    ctx.gstate.ctm = ctm;
    ctx.gstate.default_ctm = ctm;
    // Set font resource path for font tests
    let font_dir = std::path::Path::new("resources/Font");
    if font_dir.is_dir() {
        ctx.font_resource_path = Some(
            font_dir
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .to_string(),
        );
    } else {
        // Fallback: search from workspace root
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let candidate = workspace_root.join("resources").join("Font");
        if candidate.is_dir() {
            ctx.font_resource_path = Some(candidate.to_string_lossy().to_string());
        }
    }
    ctx
}

/// Atomic counter for unique temp file names across parallel tests.
static TEST_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Run PostScript source, call showpage via PS, return PNG bytes.
fn render_to_png(source: &[u8], width: u32, height: u32) -> Vec<u8> {
    let id = TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp_path =
        std::env::temp_dir().join(format!("stet_test_{}_{}.png", std::process::id(), id));
    let path_str = tmp_path.to_str().unwrap().to_string();

    let mut ctx = render_ctx(width, height);
    ctx.output_path = Some(path_str.clone());

    // Append showpage if source doesn't contain it
    let mut full_source = source.to_vec();
    if !source.windows(8).any(|w| w == b"showpage") {
        full_source.extend_from_slice(b"\nshowpage\n");
    }

    stet_engine::eval::parse_and_exec(&mut ctx, &full_source).expect("PS execution failed");

    let png_data = std::fs::read(&path_str).expect("read output PNG");
    std::fs::remove_file(&path_str).ok();
    png_data
}

/// Verify PNG header and extract dimensions.
fn verify_png(data: &[u8]) -> (u32, u32) {
    assert!(data.len() > 24, "PNG too small: {} bytes", data.len());
    assert_eq!(&data[..8], b"\x89PNG\r\n\x1a\n", "Not a valid PNG");
    let width = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
    let height = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
    (width, height)
}

// --- Integration Tests ---

/// Test 1: Rectangle fill produces a valid PNG with rendered content.
#[test]
fn test_rectangle_fill() {
    let png = render_to_png(
        b"1 0 0 setrgbcolor
          100 100 moveto 200 100 lineto 200 200 lineto 100 200 lineto
          closepath fill",
        300,
        300,
    );
    let (w, h) = verify_png(&png);
    assert_eq!(w, 300);
    assert_eq!(h, 300);
    assert!(png.len() > 200, "PNG should have rendered content");
}

/// Test 2: Stroke with line width produces visible pixels.
#[test]
fn test_stroke_with_linewidth() {
    let png = render_to_png(
        b"0 0 1 setrgbcolor
          5 setlinewidth
          50 50 moveto 250 50 lineto
          stroke",
        300,
        100,
    );
    let (w, h) = verify_png(&png);
    assert_eq!(w, 300);
    assert_eq!(h, 100);
    assert!(png.len() > 200);
}

/// Test 3: gsave/grestore preserves and restores graphics state.
#[test]
fn test_gsave_grestore() {
    let png = render_to_png(
        b"1 0 0 setrgbcolor
          gsave
            0 0 1 setrgbcolor
            10 10 moveto 50 10 lineto 50 50 lineto 10 50 lineto closepath fill
          grestore
          60 10 moveto 100 10 lineto 100 50 lineto 60 50 lineto closepath fill",
        110,
        60,
    );
    let (w, h) = verify_png(&png);
    assert_eq!(w, 110);
    assert_eq!(h, 60);
    assert!(png.len() > 200);
}

/// Test 4: Translate moves drawing position correctly.
#[test]
fn test_translate() {
    let png = render_to_png(
        b"100 100 translate
          1 0 0 setrgbcolor
          0 0 moveto 50 0 lineto 50 50 lineto 0 50 lineto
          closepath fill",
        200,
        200,
    );
    let (w, h) = verify_png(&png);
    assert_eq!(w, 200);
    assert_eq!(h, 200);
    assert!(png.len() > 200);
}

/// Test 5: Scale transform magnifies drawing.
#[test]
fn test_scale() {
    let png = render_to_png(
        b"2 2 scale
          1 0 0 setrgbcolor
          10 10 moveto 50 10 lineto 50 50 lineto 10 50 lineto
          closepath fill",
        200,
        200,
    );
    let (w, h) = verify_png(&png);
    assert_eq!(w, 200);
    assert_eq!(h, 200);
    assert!(png.len() > 200);
}

/// Test 6: Clipping restricts fill area.
#[test]
fn test_clipping() {
    let png = render_to_png(
        b"newpath 50 50 moveto 150 50 lineto 150 150 lineto 50 150 lineto closepath clip
          1 0 0 setrgbcolor
          0 0 moveto 200 0 lineto 200 200 lineto 0 200 lineto closepath fill",
        200,
        200,
    );
    let (w, h) = verify_png(&png);
    assert_eq!(w, 200);
    assert_eq!(h, 200);
    assert!(png.len() > 200);
}

/// Test 7: tiger.ps produces a valid PNG with substantial content.
#[test]
fn test_tiger_ps() {
    let tiger_path = std::env::temp_dir().join("tiger.ps");
    if !tiger_path.exists() {
        eprintln!(
            "Skipping tiger.ps test — file not found at {}",
            tiger_path.display()
        );
        return;
    }

    let source = std::fs::read(&tiger_path).expect("read tiger.ps");
    let tmp_path = std::env::temp_dir().join(format!("stet_tiger_test_{}.png", std::process::id()));
    let path_str = tmp_path.to_str().unwrap().to_string();

    let mut ctx = render_ctx(612, 792);
    ctx.output_path = Some(path_str.clone());

    stet_engine::eval::parse_and_exec(&mut ctx, &source).expect("tiger.ps execution failed");

    let png_data = std::fs::read(&path_str).expect("read tiger output PNG");
    let (w, h) = verify_png(&png_data);
    assert_eq!(w, 612);
    assert_eq!(h, 792);
    // Tiger produces a substantial PNG (>10KB)
    assert!(
        png_data.len() > 10_000,
        "Tiger PNG should be substantial, got {} bytes",
        png_data.len()
    );
    std::fs::remove_file(&path_str).ok();
}

/// Test 8: CMYK color conversion.
#[test]
fn test_cmyk_color() {
    let png = render_to_png(
        b"0 0 0 1 setcmykcolor
          10 10 moveto 50 10 lineto 50 50 lineto 10 50 lineto closepath fill
          1 0 0 0 setcmykcolor
          60 10 moveto 100 10 lineto 100 50 lineto 60 50 lineto closepath fill",
        110,
        60,
    );
    let (w, h) = verify_png(&png);
    assert_eq!(w, 110);
    assert_eq!(h, 60);
    assert!(png.len() > 200);
}

/// Test 9: Arc drawing.
#[test]
fn test_arc() {
    let png = render_to_png(
        b"1 0 0 setrgbcolor
          2 setlinewidth
          100 100 50 0 360 arc
          stroke",
        200,
        200,
    );
    let (w, h) = verify_png(&png);
    assert_eq!(w, 200);
    assert_eq!(h, 200);
    assert!(png.len() > 200);
}

/// Test 10: Dash pattern rendering.
#[test]
fn test_dash_pattern() {
    let png = render_to_png(
        b"0 0 0 setrgbcolor
          2 setlinewidth
          [10 5] 0 setdash
          50 50 moveto 250 50 lineto
          stroke",
        300,
        100,
    );
    let (w, h) = verify_png(&png);
    assert_eq!(w, 300);
    assert_eq!(h, 100);
    assert!(png.len() > 200);
}

// --- Phase 4: Font & Text Rendering Tests ---

/// Helper to check if font resources are available.
fn fonts_available() -> bool {
    let ctx = render_ctx(100, 100);
    ctx.font_resource_path.is_some()
}

/// Test 11: Simple show renders text (non-empty PNG with content).
#[test]
fn test_show_simple() {
    if !fonts_available() {
        eprintln!("Skipping font test — resources/Font not found");
        return;
    }
    let png = render_to_png(
        b"/Helvetica findfont 24 scalefont setfont
          72 700 moveto
          (Hello, World!) show",
        612,
        792,
    );
    let (w, h) = verify_png(&png);
    assert_eq!(w, 612);
    assert_eq!(h, 792);
    // Text should produce visible content beyond a blank page
    assert!(
        png.len() > 300,
        "show should render visible text, got {} bytes",
        png.len()
    );
}

/// Test 12: stringwidth returns positive width values via PS execution.
#[test]
fn test_stringwidth_via_ps() {
    if !fonts_available() {
        eprintln!("Skipping font test — resources/Font not found");
        return;
    }
    let mut ctx = render_ctx(612, 792);
    // Capture output to check width values
    let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let output_clone = output.clone();
    ctx.stdout = Box::new(OutputCapture(output_clone));

    let source = b"/Helvetica findfont 12 scalefont setfont
                   (Hello) stringwidth
                   2 copy
                   20 string cvs print ( ) print
                   20 string cvs print
                   ";
    stet_engine::eval::parse_and_exec(&mut ctx, source).expect("PS execution failed");

    let bytes = output.lock().unwrap().clone();
    let text = String::from_utf8_lossy(&bytes);
    // Should have printed two numbers: wy (likely 0) and wx (positive)
    let parts: Vec<&str> = text.trim().split_whitespace().collect();
    assert_eq!(parts.len(), 2, "expected 2 values, got: {:?}", parts);
    // First printed is wy (on top), second is wx
    let wy: f64 = parts[0].parse().unwrap_or(999.0);
    let wx: f64 = parts[1].parse().unwrap_or(0.0);
    assert!(wx > 0.0, "wx should be positive, got {}", wx);
    assert!(wy.abs() < 0.01, "wy should be ~0, got {}", wy);
}

/// Test 13: Multiple fonts in same document.
#[test]
fn test_multiple_fonts() {
    if !fonts_available() {
        eprintln!("Skipping font test — resources/Font not found");
        return;
    }
    let png = render_to_png(
        b"/Helvetica findfont 20 scalefont setfont
          72 700 moveto (Helvetica) show
          /Times-Roman findfont 20 scalefont setfont
          72 670 moveto (Times) show
          /Courier findfont 20 scalefont setfont
          72 640 moveto (Courier) show",
        612,
        792,
    );
    let (w, h) = verify_png(&png);
    assert_eq!(w, 612);
    assert_eq!(h, 792);
    assert!(png.len() > 300);
}

/// Test 14: charpath + stroke produces outlined text.
#[test]
fn test_charpath_stroke() {
    if !fonts_available() {
        eprintln!("Skipping font test — resources/Font not found");
        return;
    }
    let png = render_to_png(
        b"/Helvetica findfont 48 scalefont setfont
          72 400 moveto
          (Outlined) true charpath
          2 setlinewidth stroke",
        612,
        792,
    );
    let (w, h) = verify_png(&png);
    assert_eq!(w, 612);
    assert_eq!(h, 792);
    assert!(png.len() > 300);
}

/// Test 15: selectfont convenience operator.
#[test]
fn test_selectfont() {
    if !fonts_available() {
        eprintln!("Skipping font test — resources/Font not found");
        return;
    }
    let png = render_to_png(
        b"/Helvetica 30 selectfont
          72 400 moveto (selectfont works) show",
        612,
        792,
    );
    let (w, h) = verify_png(&png);
    assert_eq!(w, 612);
    assert_eq!(h, 792);
    assert!(png.len() > 300);
}

/// Test 16: tiger.ps continues to render correctly after Phase 4 changes.
#[test]
fn test_tiger_ps_regression() {
    let tiger_path = std::env::temp_dir().join("tiger.ps");
    if !tiger_path.exists() {
        eprintln!("Skipping tiger.ps regression test — file not found");
        return;
    }

    let source = std::fs::read(&tiger_path).expect("read tiger.ps");
    let tmp_path =
        std::env::temp_dir().join(format!("stet_tiger_phase4_{}.png", std::process::id()));
    let path_str = tmp_path.to_str().unwrap().to_string();

    let mut ctx = render_ctx(612, 792);
    ctx.output_path = Some(path_str.clone());

    stet_engine::eval::parse_and_exec(&mut ctx, &source).expect("tiger.ps execution failed");

    let png_data = std::fs::read(&path_str).expect("read tiger output PNG");
    let (w, h) = verify_png(&png_data);
    assert_eq!(w, 612);
    assert_eq!(h, 792);
    assert!(
        png_data.len() > 10_000,
        "Tiger PNG should be substantial, got {} bytes",
        png_data.len()
    );
    std::fs::remove_file(&path_str).ok();
}

// --- Phase 5: Filters, Images & Advanced Color Tests ---

/// Test 17: turkey-imagemask.ps renders a turkey bitmap.
#[test]
fn test_turkey_imagemask() {
    let turkey_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("ps_samples")
        .join("turkey-imagemask.ps");
    if !turkey_path.exists() {
        eprintln!("Skipping turkey-imagemask.ps test — file not found");
        return;
    }

    let source = std::fs::read(&turkey_path).expect("read turkey-imagemask.ps");
    let png = render_to_png(&source, 200, 200);
    let (w, h) = verify_png(&png);
    assert_eq!(w, 200);
    assert_eq!(h, 200);
    assert!(
        png.len() > 200,
        "turkey PNG should have rendered content, got {} bytes",
        png.len()
    );
}

/// Test 18: Inline 8-bit grayscale image renders correctly.
#[test]
fn test_inline_grayscale_image() {
    // 4x4 grayscale image: alternating black and white pixels
    let png = render_to_png(
        b"100 100 translate 100 100 scale
          4 4 8 [4 0 0 -4 0 4]
          <00FF00FF FF00FF00 00FF00FF FF00FF00>
          image",
        300,
        300,
    );
    let (w, h) = verify_png(&png);
    assert_eq!(w, 300);
    assert_eq!(h, 300);
    assert!(
        png.len() > 200,
        "grayscale image PNG should have content, got {} bytes",
        png.len()
    );
}

/// Test 19: Inline 1-bit imagemask with hex data.
#[test]
fn test_imagemask_inline() {
    // 8x8 checkerboard mask
    let png = render_to_png(
        b"100 100 translate 100 100 scale
          1 0 0 setrgbcolor
          8 8 true [8 0 0 -8 0 8]
          <AA55AA55 AA55AA55>
          imagemask",
        300,
        300,
    );
    let (w, h) = verify_png(&png);
    assert_eq!(w, 300);
    assert_eq!(h, 300);
    assert!(png.len() > 200);
}

/// Test 20: RGB colorimage renders correctly.
#[test]
fn test_colorimage_rgb() {
    // 2x2 RGB image: red, green, blue, white
    let png = render_to_png(
        b"100 100 translate 200 200 scale
          2 2 8 [2 0 0 -2 0 2]
          <FF0000 00FF00 0000FF FFFFFF>
          false 3 colorimage",
        400,
        400,
    );
    let (w, h) = verify_png(&png);
    assert_eq!(w, 400);
    assert_eq!(h, 400);
    assert!(png.len() > 200);
}

/// Test 21: ASCIIHexDecode filter decodes hex data correctly.
#[test]
fn test_asciihex_filter() {
    let mut ctx = render_ctx(100, 100);
    let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let output_clone = output.clone();
    ctx.stdout = Box::new(OutputCapture(output_clone));

    let source = b"(48656C6C6F>) /ASCIIHexDecode filter
                   5 string readstring pop print";
    stet_engine::eval::parse_and_exec(&mut ctx, source).expect("PS execution failed");

    let bytes = output.lock().unwrap().clone();
    assert_eq!(&bytes, b"Hello", "ASCIIHexDecode should decode to 'Hello'");
}

/// Test 22: ASCII85Decode filter decodes data correctly.
#[test]
fn test_ascii85_filter() {
    let mut ctx = render_ctx(100, 100);
    let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let output_clone = output.clone();
    ctx.stdout = Box::new(OutputCapture(output_clone));

    // "Hello" in ASCII85 is 87cURD]j
    let source = b"(87cURD]j~>) /ASCII85Decode filter
                   5 string readstring pop print";
    stet_engine::eval::parse_and_exec(&mut ctx, source).expect("PS execution failed");

    let bytes = output.lock().unwrap().clone();
    assert_eq!(&bytes, b"Hello", "ASCII85Decode should decode to 'Hello'");
}

/// Test 23: Halftone stubs don't crash and consume operands correctly.
#[test]
fn test_halftone_stubs() {
    let mut ctx = render_ctx(100, 100);

    // Run various halftone/transfer operators — they should not error
    let source = b"60 45 {} setscreen
                   currentscreen pop pop pop
                   {} settransfer
                   currenttransfer pop
                   {} {} {} {} setcolortransfer
                   currentcolortransfer pop pop pop pop
                   {} setblackgeneration
                   currentblackgeneration pop
                   {} setundercolorremoval
                   currentundercolorremoval pop";
    stet_engine::eval::parse_and_exec(&mut ctx, source).expect("halftone stubs should not error");
    assert!(
        ctx.o_stack.is_empty(),
        "all halftone results should be consumed"
    );
}

/// Test 24: setpagedevice / currentpagedevice work.
#[test]
fn test_pagedevice_ops() {
    let mut ctx = render_ctx(612, 792);

    // Set up a page device with PageSize and HWResolution
    let source =
        b"<< /PageSize [612 792] /HWResolution [72 72] /.IsPageDevice true >> setpagedevice
                   currentpagedevice pop";
    ctx.device_factory = Some(Box::new(|w, h| {
        Box::new(stet_render::SkiaDevice::new(w, h))
    }));
    stet_engine::eval::parse_and_exec(&mut ctx, source).expect("pagedevice should not error");
    // Note: without init scripts, setpagedevice's Install procedure may leave
    // residual items on the stack. We only verify that page_device was set.
    assert!(ctx.gstate.page_device.is_some());
}

/// Test 25: FlateDecode filter decompresses data correctly.
#[test]
fn test_flate_filter() {
    let mut ctx = render_ctx(100, 100);
    let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let output_clone = output.clone();
    ctx.stdout = Box::new(OutputCapture(output_clone));

    // Compress "Hello World!" with flate2 and embed as hex
    let input = b"Hello World!";
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut encoder, input).unwrap();
    let compressed = encoder.finish().unwrap();

    // Build hex string of compressed data
    let hex: String = compressed.iter().map(|b| format!("{:02X}", b)).collect();

    let source = format!(
        "(<{}>) /ASCIIHexDecode filter /FlateDecode filter\n\
         {} string readstring pop print",
        hex,
        input.len()
    );
    stet_engine::eval::parse_and_exec(&mut ctx, source.as_bytes())
        .expect("FlateDecode filter should work");

    let bytes = output.lock().unwrap().clone();
    assert_eq!(
        &bytes, input,
        "FlateDecode should decompress to original data"
    );
}

/// Re-installing the same CIE colour space must not cost arena memory per
/// installation.
///
/// `setcolorspace` samples each decode procedure 256 times per channel. The
/// procedures `pdftops` emits for an ICCBased space contain an inline array
/// literal, so `[` / `]` are executable and every one of those evaluations
/// builds a fresh array in the array arena — which is never reclaimed without
/// a `restore`. A file that re-installs the space on each of its pages (the
/// normal shape of `pdftops` output) therefore grew without bound: a 55 KB
/// corpus file reached over 3 GB, and running the corpus concurrently
/// exhausted host memory.
///
/// The memo in `Context::cie_decode_cache` collapses the repeats, so arena
/// growth after the first installation must be negligible.
#[test]
fn cie_decode_tables_are_memoised_across_reinstalls() {
    let mut ctx = render_ctx(64, 64);

    // A DecodeABC whose procedures each build a 64-element array literal and
    // index it — the construction that made this unbounded.
    let table: String = (0..64)
        .map(|i| (i * 4).to_string())
        .collect::<Vec<_>>()
        .join(" ");
    let space = format!(
        "/cs [ /CIEBasedABC << \
           /DecodeABC [ \
             {{ dup 0 lt {{ pop 0 }} if [ {table} ] exch 63 mul cvi get 256 div }} \
             {{ dup 0 lt {{ pop 0 }} if [ {table} ] exch 63 mul cvi get 256 div }} \
             {{ dup 0 lt {{ pop 0 }} if [ {table} ] exch 63 mul cvi get 256 div }} \
           ] \
           /RangeABC [0 1 0 1 0 1] \
           /MatrixABC [1 0 0 0 1 0 0 0 1] \
           /WhitePoint [0.9505 1.0 1.089] \
         >> ] def\n"
    );
    stet_engine::eval::parse_and_exec(&mut ctx, space.as_bytes())
        .expect("colour space definition should execute");

    let install = b"cs setcolorspace 0.2 0.4 0.6 setcolor\n";

    stet_engine::eval::parse_and_exec(&mut ctx, install).expect("first install");
    let after_first = ctx.arrays.allocated_objects();
    let cache_after_first = ctx.cie_decode_cache.len();
    assert!(
        cache_after_first > 0,
        "the first install should populate the decode-table memo"
    );

    // Re-install many times, as a multi-page document would.
    const REINSTALLS: usize = 40;
    for _ in 0..REINSTALLS {
        stet_engine::eval::parse_and_exec(&mut ctx, install).expect("re-install");
    }
    let after_repeats = ctx.arrays.allocated_objects();

    assert_eq!(
        ctx.cie_decode_cache.len(),
        cache_after_first,
        "re-installing an unchanged space must not add cache entries"
    );

    // Uncached, each re-install re-ran 3 procedures x 256 samples, every one
    // building a 64-element array: ~49k objects per install. Allow generous
    // slack for the interpreter's own per-execution allocations while still
    // failing decisively if the sampling is happening again.
    let growth = after_repeats - after_first;
    let per_install_uncached = 3 * 256 * 64;
    assert!(
        growth < per_install_uncached,
        "arena grew {growth} objects over {REINSTALLS} re-installs \
         (>= {per_install_uncached}, one uncached install) — decode tables \
         are being resampled instead of reused"
    );
}

/// A Type 3 font whose `A` glyph fills a triangle and whose `B` glyph strokes
/// the same three points. `resources/Font` is not needed — the glyph
/// procedures paint directly.
const TYPE3_CHARPATH_FONT: &[u8] = b"
    /T3 8 dict def
    T3 begin
      /FontType 3 def
      /FontMatrix [0.001 0 0 0.001 0 0] def
      /FontBBox [0 0 1000 1000] def
      /Encoding 256 array def
      0 1 255 { Encoding exch /.notdef put } for
      Encoding 65 /filled put
      Encoding 66 /stroked put
      /CharProcs 3 dict def
      CharProcs begin
        /.notdef { 0 0 setcharwidth } bind def
        /filled { 1000 0 setcharwidth
                  100 200 moveto 400 200 lineto 400 700 lineto fill } bind def
        /stroked { 1000 0 setcharwidth 40 setlinewidth
                   100 200 moveto 400 200 lineto 400 700 lineto stroke } bind def
      end
      /BuildGlyph { exch /CharProcs get exch
                    2 copy known not { pop /.notdef } if get exec } bind def
      /BuildChar { 1 index /Encoding get exch get
                   1 index /BuildGlyph get exec } bind def
    end
    /T3 T3 definefont pop
    /T3 findfont 100 scalefont setfont
";

/// `charpath` on a Type 3 font builds the path without marking the page.
///
/// The glyph procedure runs for real — that is the only way to learn its
/// outline — so the check that matters is that nothing it painted survives on
/// the display list. Ghostscript renders a blank page for the same input.
#[test]
fn test_charpath_type3_paints_nothing() {
    let mut ctx = render_ctx(200, 200);
    let mut src = TYPE3_CHARPATH_FONT.to_vec();
    src.extend_from_slice(b"newpath 0 0 moveto (AB) false charpath\n");
    stet_engine::eval::parse_and_exec(&mut ctx, &src).expect("PS execution failed");

    assert!(
        ctx.display_list.is_empty(),
        "a Type 3 charpath must leave no marks, found {} display elements",
        ctx.display_list.len()
    );
    assert!(
        !ctx.gstate.path.segments.is_empty(),
        "a Type 3 charpath must append the glyph outlines to the current path"
    );

    // Both glyphs cover the same 100..400 x 200..700 glyph-space box, at
    // FontMatrix 0.001 and 100pt that is 10..40 x 20..70 in user space, and
    // the second glyph sits 100 units to the right. The Y flip in render_ctx
    // puts device Y at 200 - user Y.
    let (mut min_x, mut min_y) = (f64::MAX, f64::MAX);
    let (mut max_x, mut max_y) = (f64::MIN, f64::MIN);
    for seg in &ctx.gstate.path.segments {
        let pts: Vec<(f64, f64)> = match *seg {
            stet_core::graphics_state::PathSegment::MoveTo(x, y) => vec![(x, y)],
            stet_core::graphics_state::PathSegment::LineTo(x, y) => vec![(x, y)],
            stet_core::graphics_state::PathSegment::CurveTo {
                x1,
                y1,
                x2,
                y2,
                x3,
                y3,
            } => vec![(x1, y1), (x2, y2), (x3, y3)],
            _ => vec![],
        };
        for (x, y) in pts {
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
    }
    assert!((min_x - 10.0).abs() < 0.01, "min x was {min_x}");
    assert!((max_x - 140.0).abs() < 0.01, "max x was {max_x}");
    assert!((min_y - 130.0).abs() < 0.01, "min y was {min_y}");
    assert!((max_y - 180.0).abs() < 0.01, "max y was {max_y}");
}

/// A Type 3 font supplying **only** `BuildGlyph`, with no `BuildChar`.
///
/// PLRM 5.7 makes `BuildChar` required only "for LanguageLevel 1 or if
/// BuildGlyph is absent", so this font is well-formed. `BuildGlyph` receives
/// the character *name* from `Encoding`, not the character code — the glyph
/// procedure here records which name it was handed so tests can check it.
const TYPE3_BUILDGLYPH_ONLY_FONT: &[u8] = b"
    /T3G 8 dict def
    /SeenNames 8 array def
    /SeenIdx 0 def
    T3G begin
      /FontType 3 def
      /FontMatrix [0.001 0 0 0.001 0 0] def
      /FontBBox [0 0 1000 1000] def
      /Encoding 256 array def
      0 1 255 { Encoding exch /.notdef put } for
      Encoding 65 /square put
      Encoding 66 /bar put
      /BuildGlyph {
        exch pop
        dup SeenNames SeenIdx 3 -1 roll put
        /SeenIdx SeenIdx 1 add def
        600 0 setcharwidth
        dup /square eq { 100 100 moveto 400 0 rlineto 0 400 rlineto
                         -400 0 rlineto closepath fill } if
        /bar eq { 100 100 moveto 200 0 rlineto 0 700 rlineto
                  -200 0 rlineto closepath fill } if
      } bind def
    end
    /T3G T3G definefont pop
    /T3G findfont 100 scalefont setfont
";

/// A `show` on a Type 3 font that has only `BuildGlyph` must render, not
/// raise `invalidfont`.
///
/// Regression: `render_show_type3` used to require `BuildChar` unconditionally
/// and push the character code, so a BuildGlyph-only font — which Ghostscript
/// renders fine — failed outright.
#[test]
fn test_type3_buildglyph_without_buildchar_shows() {
    let mut ctx = render_ctx(200, 200);
    let mut src = TYPE3_BUILDGLYPH_ONLY_FONT.to_vec();
    src.extend_from_slice(b"0 0 moveto (AB) show\n");
    stet_engine::eval::parse_and_exec(&mut ctx, &src)
        .expect("a BuildGlyph-only Type 3 font must render");

    assert!(
        !ctx.display_list.is_empty(),
        "both glyphs should have painted"
    );
}

/// `BuildGlyph` receives the character *name* from `Encoding`, not the code.
///
/// This is the part that makes BuildGlyph worth having: the demo font that
/// exposed the bug dispatches on `/leaf` / `/bud` / `/berry`, which only works
/// if the name reaches the procedure.
#[test]
fn test_type3_buildglyph_receives_encoded_name() {
    let mut ctx = render_ctx(200, 200);
    let mut src = TYPE3_BUILDGLYPH_ONLY_FONT.to_vec();
    // Code 65 -> /square, 66 -> /bar, 67 -> /.notdef (unmapped).
    src.extend_from_slice(b"0 0 moveto (ABC) show\n");
    src.extend_from_slice(b"0 1 SeenIdx 1 sub { SeenNames exch get } for\n");
    stet_engine::eval::parse_and_exec(&mut ctx, &src).expect("PS execution failed");

    let names: Vec<String> = (0..ctx.o_stack.len())
        .rev()
        .map(|i| {
            let obj = ctx.o_stack.peek(i).expect("stack entry");
            match obj.value {
                stet_core::object::PsValue::Name(id) => {
                    String::from_utf8_lossy(ctx.names.get_bytes(id)).into_owned()
                }
                other => panic!("expected a name on the stack, got {other:?}"),
            }
        })
        .collect();

    assert_eq!(
        names,
        vec!["square", "bar", ".notdef"],
        "BuildGlyph must be handed the Encoding name for each code, \
         with unmapped codes falling back to /.notdef"
    );
}

/// `stringwidth` on a Type 3 font runs the build procedure for its width and
/// paints nothing.
///
/// Regression: `measure_string_width` had no Type 3 branch, so it fell through
/// to the Type 1 path, looked for `CharStrings`, and raised `invalidfont`.
#[test]
fn test_type3_stringwidth_measures_without_painting() {
    let mut ctx = render_ctx(200, 200);
    let mut src = TYPE3_BUILDGLYPH_ONLY_FONT.to_vec();
    src.extend_from_slice(b"(AB) stringwidth\n");
    stet_engine::eval::parse_and_exec(&mut ctx, &src).expect("Type 3 stringwidth must work");

    assert!(
        ctx.display_list.is_empty(),
        "stringwidth must not mark the page, found {} elements",
        ctx.display_list.len()
    );

    let wy = ctx.o_stack.peek(0).expect("wy").as_f64().expect("wy real");
    let wx = ctx.o_stack.peek(1).expect("wx").as_f64().expect("wx real");
    // setcharwidth 600 per glyph, FontMatrix 0.001, 100pt => 60pt each.
    assert!((wx - 120.0).abs() < 0.01, "wx was {wx}, expected 120");
    assert!(wy.abs() < 0.01, "wy was {wy}, expected 0");
}

/// `glyphshow` on a Type 3 font with `BuildGlyph` hands it the name directly,
/// bypassing `Encoding`.
///
/// The bypass is the point of `glyphshow` (PLRM: "glyphshow bypasses the
/// current font's Encoding array; it can access any character in the font,
/// whether or not that character's name is present in the encoding vector"),
/// so `/unencoded` — which no code maps to — must still reach BuildGlyph.
#[test]
fn test_type3_glyphshow_buildglyph_bypasses_encoding() {
    let mut ctx = render_ctx(200, 200);
    let mut src = TYPE3_BUILDGLYPH_ONLY_FONT.to_vec();
    src.extend_from_slice(b"0 0 moveto /square glyphshow /unencoded glyphshow\n");
    src.extend_from_slice(b"0 1 SeenIdx 1 sub { SeenNames exch get } for\n");
    stet_engine::eval::parse_and_exec(&mut ctx, &src).expect("Type 3 glyphshow must work");

    let names: Vec<String> = (0..ctx.o_stack.len())
        .rev()
        .map(|i| match ctx.o_stack.peek(i).expect("stack entry").value {
            stet_core::object::PsValue::Name(id) => {
                String::from_utf8_lossy(ctx.names.get_bytes(id)).into_owned()
            }
            other => panic!("expected a name, got {other:?}"),
        })
        .collect();

    assert_eq!(
        names,
        vec!["square", "unencoded"],
        "glyphshow must pass the name straight through, including one that is \
         not in Encoding"
    );
}

/// A Type 3 font with only `BuildChar`, for the `glyphshow` reverse-lookup
/// path. Code 66 is `/bar`; every other code is `/.notdef`.
const TYPE3_BUILDCHAR_ONLY_FONT: &[u8] = b"
    /T3C 8 dict def
    /SeenCodes 8 array def
    /SeenIdx 0 def
    T3C begin
      /FontType 3 def
      /FontMatrix [0.001 0 0 0.001 0 0] def
      /FontBBox [0 0 1000 1000] def
      /Encoding 256 array def
      0 1 255 { Encoding exch /.notdef put } for
      Encoding 66 /bar put
      /BuildChar {
        exch pop
        dup SeenCodes SeenIdx 3 -1 roll put
        /SeenIdx SeenIdx 1 add def
        600 0 setcharwidth
        pop 100 100 200 700 rectfill
      } bind def
    end
    /T3C T3C definefont pop
    /T3C findfont 100 scalefont setfont
";

/// `glyphshow` on a BuildChar-only Type 3 font reverse-searches `Encoding` for
/// the name and passes the resulting code.
///
/// PLRM: "If there is no BuildGlyph procedure, but only a BuildChar procedure,
/// glyphshow searches the font's Encoding array for an occurrence of name. If
/// it finds one, it pushes the font dictionary and the array index". An
/// unencoded name falls back to searching for `/.notdef`, which this font has
/// at code 0. Ghostscript produces the same two codes.
#[test]
fn test_type3_glyphshow_buildchar_reverse_encoding_lookup() {
    let mut ctx = render_ctx(200, 200);
    let mut src = TYPE3_BUILDCHAR_ONLY_FONT.to_vec();
    src.extend_from_slice(b"0 0 moveto /bar glyphshow /nosuchglyph glyphshow\n");
    src.extend_from_slice(b"0 1 SeenIdx 1 sub { SeenCodes exch get } for\n");
    stet_engine::eval::parse_and_exec(&mut ctx, &src).expect("Type 3 glyphshow must work");

    let codes: Vec<i32> = (0..ctx.o_stack.len())
        .rev()
        .map(|i| {
            ctx.o_stack
                .peek(i)
                .expect("stack entry")
                .as_i32()
                .expect("integer code")
        })
        .collect();

    assert_eq!(
        codes,
        vec![66, 0],
        "/bar is encoded at 66; /nosuchglyph falls back to the /.notdef search, \
         which finds code 0"
    );
}

/// With neither the name nor `/.notdef` in `Encoding`, `glyphshow` on a
/// BuildChar-only Type 3 font raises `invalidfont`.
///
/// PLRM: "If .notdef is not present either, an invalidfont error occurs."
/// Ghostscript raises the same error for the same font.
#[test]
fn test_type3_glyphshow_without_notdef_is_invalidfont() {
    let mut ctx = render_ctx(200, 200);
    let src = b"
        /FN 8 dict def
        FN begin
          /FontType 3 def
          /FontMatrix [0.001 0 0 0.001 0 0] def
          /FontBBox [0 0 1000 1000] def
          /Encoding 256 array def
          0 1 255 { Encoding exch /filler put } for
          /BuildChar { pop pop 600 0 setcharwidth } bind def
        end
        /FN FN definefont pop
        /FN findfont 100 scalefont setfont
        0 0 moveto /nosuchglyph
    ";
    stet_engine::eval::parse_and_exec(&mut ctx, src).expect("font setup should succeed");

    // Invoke the operator directly: this bare context has no init scripts, so
    // an error raised through the eval loop surfaces only as `Stop` with no
    // populated $error to inspect. Calling it here gets the typed error.
    let err = stet_ops::show_ops::op_glyphshow(&mut ctx)
        .expect_err("glyphshow must fail when neither the name nor /.notdef is encoded");
    assert!(
        matches!(err, stet_core::error::PsError::InvalidFont),
        "expected invalidfont, got {err:?}"
    );
}

/// `clippath` yields the page itself, wherever the program has moved its
/// coordinate system.
///
/// The default clip is a fixed region of the device. Deriving it with the
/// *current* CTM made `translate` drag the clip rectangle along, so the
/// `clippath fill` idiom for painting a background filled an offset region
/// and left part of the page bare. `gstate.path` is device space, so the
/// invariant is that it spans the whole page no matter the CTM.
#[test]
fn clippath_is_the_whole_page_regardless_of_ctm() {
    for prologue in [
        &b""[..],
        &b"50 30 translate"[..],
        &b"2 2 scale"[..],
        &b"50 30 translate 2 3 scale 15 rotate"[..],
    ] {
        let mut ctx = render_ctx(200, 200);
        let mut src = prologue.to_vec();
        src.extend_from_slice(b" clippath\n");
        stet_engine::eval::parse_and_exec(&mut ctx, &src).expect("PS execution failed");

        let (mut min_x, mut min_y) = (f64::MAX, f64::MAX);
        let (mut max_x, mut max_y) = (f64::MIN, f64::MIN);
        for seg in &ctx.gstate.path.segments {
            let pts: Vec<(f64, f64)> = match *seg {
                stet_core::graphics_state::PathSegment::MoveTo(x, y)
                | stet_core::graphics_state::PathSegment::LineTo(x, y) => vec![(x, y)],
                _ => vec![],
            };
            for (x, y) in pts {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
        let ctm_desc = String::from_utf8_lossy(prologue).to_string();
        assert!(
            (min_x).abs() < 0.01 && (min_y).abs() < 0.01,
            "clip origin moved to ({min_x}, {min_y}) after `{ctm_desc}`"
        );
        assert!(
            (max_x - 200.0).abs() < 0.01 && (max_y - 200.0).abs() < 0.01,
            "clip extent became ({max_x}, {max_y}) after `{ctm_desc}`"
        );
    }
}

/// `clippath pathbbox` reports the page in *current user space*.
///
/// Ghostscript returns [-50 -30 150 170] for a 200x200 page after
/// `50 30 translate`; the values are what a program uses to size a
/// background, so they have to track the CTM even though the region does not.
#[test]
fn clippath_pathbbox_is_in_current_user_space() {
    let mut ctx = render_ctx(200, 200);
    stet_engine::eval::parse_and_exec(&mut ctx, b"50 30 translate clippath pathbbox\n")
        .expect("PS execution failed");

    let vals: Vec<f64> = (0..4)
        .rev()
        .map(|i| ctx.o_stack.peek(i).expect("bbox value").as_f64().unwrap())
        .collect();
    let expected = [-50.0, -30.0, 150.0, 170.0];
    for (got, want) in vals.iter().zip(expected.iter()) {
        assert!(
            (got - want).abs() < 0.01,
            "pathbbox was {vals:?}, expected {expected:?}"
        );
    }
}

/// The bug as it appeared: `clippath fill` after a `translate` must cover the
/// whole page, not an offset slice of it.
#[test]
fn clippath_fill_covers_the_page_after_translate() {
    let mut ctx = render_ctx(200, 200);
    stet_engine::eval::parse_and_exec(&mut ctx, b"0.8 setgray 50 30 translate clippath fill\n")
        .expect("PS execution failed");

    let (mut min_x, mut min_y) = (f64::MAX, f64::MAX);
    let (mut max_x, mut max_y) = (f64::MIN, f64::MIN);
    let mut found = false;
    for elem in ctx.display_list.elements_from(0) {
        if let stet_core::display_list::DisplayElement::Fill { path, .. } = elem {
            found = true;
            for seg in &path.segments {
                let pts: Vec<(f64, f64)> = match *seg {
                    stet_core::graphics_state::PathSegment::MoveTo(x, y)
                    | stet_core::graphics_state::PathSegment::LineTo(x, y) => vec![(x, y)],
                    _ => vec![],
                };
                for (x, y) in pts {
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_x = max_x.max(x);
                    max_y = max_y.max(y);
                }
            }
        }
    }
    assert!(found, "expected a Fill element on the display list");
    assert!(
        min_x.abs() < 0.01 && min_y.abs() < 0.01,
        "background fill starts at ({min_x}, {min_y}), leaving the page edge bare"
    );
    assert!(
        (max_x - 200.0).abs() < 0.01 && (max_y - 200.0).abs() < 0.01,
        "background fill ends at ({max_x}, {max_y}), short of the page"
    );
}

/// Helper: Write adapter that captures bytes to a shared Vec.
struct OutputCapture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for OutputCapture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
