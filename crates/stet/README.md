# stet

[![crates.io](https://img.shields.io/crates/v/stet.svg)](https://crates.io/crates/stet)
[![docs.rs](https://img.shields.io/docsrs/stet)](https://docs.rs/stet)

A PostScript Level 3 interpreter and PDF rendering engine written in Rust.

**stet** renders PostScript, EPS, and PDF files to RGBA pixels, PDF output, or
display lists. All resources (35 fonts, init scripts, encodings, ICC profiles)
are embedded in the binary — no external files needed.

## Rendering Correctness

stet handles two rendering issues that most PDF/PostScript renderers get
wrong: no seams on adjacent clipped regions (binary clip coverage, not
anti-aliased), and full overprint simulation with CMYK blend math, knockout
groups, and spot channel preservation. See the [repository
README](https://github.com/AndyCappDev/stet#rendering-correctness) for
detail — it matters for prepress and proofing workflows.

## Quick Start

```toml
[dependencies]
stet = "0.2"
```

### PostScript / EPS

```rust
use stet::Interpreter;

fn main() -> Result<(), stet::StetError> {
    let mut interp = Interpreter::new();
    let pages = interp.render(include_bytes!("test.ps"), 300.0)?;

    for (i, page) in pages.iter().enumerate() {
        println!("Page {}: {}x{} pixels", i + 1, page.width, page.height);
        // page.rgba contains RGBA pixel data (4 bytes per pixel, row-major)
    }
    Ok(())
}
```

### PDF

PDF reading lives in the companion [`stet-pdf-reader`](https://crates.io/crates/stet-pdf-reader)
crate and is deliberately **independent of `stet`** — it does not depend on the
PostScript interpreter at all, so PDF-only users don't pay for the VM:

```toml
[dependencies]
stet-pdf-reader = "0.2"
```

```rust
use stet_pdf_reader::PdfDocument;

let data = std::fs::read("document.pdf")?;
let doc = PdfDocument::from_bytes(&data)?;

for page in 0..doc.page_count() {
    // DisplayList — same type the PS interpreter produces.
    let display_list = doc.render_page(page, 300.0)?;

    // …or go straight to RGBA with the `render` feature (default):
    let (rgba, w, h) = doc.render_page_to_rgba(page, 300.0)?;
}
```

Because both `stet` and `stet-pdf-reader` produce the same
`DisplayList` type, every downstream consumer (rasterizer, PDF writer,
custom output device) works with both sources interchangeably.

See [`crates/stet/examples/`](https://github.com/AndyCappDev/stet/tree/main/crates/stet/examples)
for runnable `render_ps`, `render_pdf`, and `display_list` programs.

## Output Modes

| Method | Returns | Feature |
|--------|---------|---------|
| `render()` | RGBA pixels + display list | `render` (default) |
| `render_to_display_list()` | Display lists only | always available |
| `render_to_pdf()` | PDF document bytes | `pdf-output` (default) |
| `exec()` | Nothing (side effects only) | always available |

### Render to RGBA

```rust
let mut interp = stet::Interpreter::new();
let pages = interp.render(ps_data, 300.0)?;
// pages[0].rgba: Vec<u8>  — RGBA, 4 bytes/pixel, row-major
// pages[0].width: u32     — pixels
// pages[0].height: u32    — pixels
// pages[0].display_list   — for viewport rendering
```

### Render to PDF

```rust
let mut interp = stet::Interpreter::new();
let pdf_bytes = interp.render_to_pdf(ps_data, 300.0)?;
std::fs::write("output.pdf", &pdf_bytes)?;
```

### Display List Only

For custom rendering pipelines or viewport rendering:

```rust
let mut interp = stet::Interpreter::new();
let pages = interp.render_to_display_list(ps_data, 300.0)?;

// Render a viewport region later
let prepared = stet::prepare_display_list(&pages[0].display_list);
let rgba = stet::render_region_prepared(
    &pages[0].display_list,
    &prepared,
    vp_x, vp_y, vp_w, vp_h,  // viewport in device pixels
    pixel_w, pixel_h,          // output dimensions
    pages[0].dpi,
    None,   // ICC cache
    None,   // image cache
    false,  // anti-aliasing
);
```

### Execute Without Rendering

```rust
let mut interp = stet::Interpreter::new();
interp.exec(b"1 2 add ==")?;  // prints "3" to stdout
```

## Choosing a DPI

The `dpi` parameter controls the coordinate space of the display list, not
just the output pixel count. PostScript paths are transformed to device
coordinates at the specified DPI during interpretation. For best quality,
use a DPI that matches your final output:

```rust
// Standard output (matches built-in device defaults)
let pages = interp.render(ps_data, 300.0)?;

// Lower resolution for faster screen viewing
let pages = interp.render(ps_data, 150.0)?;

// High-resolution proofing
let pages = interp.render(ps_data, 600.0)?;
```

## Configuration

```rust
let mut interp = stet::Interpreter::builder()
    .no_icc()             // disable ICC color management
    .suppress_output()    // silence PS print/==/= operators
    .build();
```

## Features

Both features are enabled by default:

| Feature | Adds | Extra dependency |
|---------|------|-----------------|
| `render` | `render()` — RGBA pixel output | `stet-render` (tiny-skia) |
| `pdf-output` | `render_to_pdf()` — PDF output | `stet-pdf` |

To use only display lists (smallest dependency footprint):

```toml
[dependencies]
stet = { version = "0.2", default-features = false }
```

## Power User: Direct Context Access

For advanced use cases, you can access the underlying PostScript interpreter
context directly:

```rust
let mut interp = stet::Interpreter::new();
let ctx = interp.context();
stet::ps_exec(ctx, b"/greeting (Hello, PostScript!) def greeting print")?;
```

## Crate Architecture

`stet` is a facade over a multi-crate workspace:

| Crate | Role |
|-------|------|
| `stet` | Batteries-included library API (this crate) |
| `stet-core` | Interpreter infrastructure: types, VM, tokenizer |
| `stet-ops` | ~376 PostScript operator implementations |
| `stet-engine` | Execution engine (eval loop) |
| `stet-fonts` | Font parsing (Type 1, CFF, TrueType) |
| `stet-graphics` | Display list, color types, ICC |
| `stet-render` | `stet-tiny-skia` rendering backend |
| `stet-pdf` | PDF output device (display list → PDF) |
| `stet-pdf-reader` | PDF input parser (PDF → display list), independent of `stet-core` |
| `stet-viewer` | Interactive egui desktop viewer |
| `stet-cli` | Command-line `stet` binary |
| `stet-tiny-skia` / `-path` | Vendored tiny-skia fork (BSD-3-Clause) |

## License

Apache-2.0 OR MIT
