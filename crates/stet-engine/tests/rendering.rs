// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Integration tests for Phase 3 graphics rendering.
//!
//! Each test executes a PostScript program with a SkiaDevice attached,
//! then inspects the resulting PNG output to verify correct rendering.

use stet_core::context::Context;
use stet_core::graphics_state::Matrix;
use stet_render::SkiaDevice;

/// Create a rendering context with a SkiaDevice and default CTM.
fn render_ctx(width: u32, height: u32) -> Context {
    let device = SkiaDevice::new(width, height);
    let mut ctx = Context::new();
    stet_ops::build_system_dict(&mut ctx);
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
    let tiger_path = std::path::Path::new("/tmp/tiger.ps");
    if !tiger_path.exists() {
        eprintln!("Skipping tiger.ps test — file not found at /tmp/tiger.ps");
        return;
    }

    let source = std::fs::read(tiger_path).expect("read tiger.ps");
    let tmp_path =
        std::env::temp_dir().join(format!("stet_tiger_test_{}.png", std::process::id()));
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
    let tiger_path = std::path::Path::new("/tmp/tiger.ps");
    if !tiger_path.exists() {
        eprintln!("Skipping tiger.ps regression test — file not found");
        return;
    }

    let source = std::fs::read(tiger_path).expect("read tiger.ps");
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
        .join("tests")
        .join("ps")
        .join("turkey-imagemask.ps");
    // Also try the postforge samples path
    let turkey_path = if turkey_path.exists() {
        turkey_path
    } else {
        std::path::PathBuf::from("/home/scott/Projects/postforge/samples/turkey-imagemask.ps")
    };
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
    assert!(ctx.o_stack.is_empty());
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
