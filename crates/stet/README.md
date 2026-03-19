# stet

A PostScript Level 3 interpreter written in Rust.

**stet** renders PostScript and EPS files to RGBA pixels, PDF, or display lists.
All resources (35 fonts, init scripts, encodings, ICC profiles) are embedded in
the binary — no external files needed.

## Quick Start

```toml
[dependencies]
stet = "0.1"
```

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
stet = { version = "0.1", default-features = false }
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
| `stet-ops` | ~268 PostScript operator implementations |
| `stet-engine` | Execution engine (eval loop) |
| `stet-fonts` | Font parsing (Type 1, CFF, TrueType) |
| `stet-graphics` | Display list, color types, ICC |
| `stet-render` | tiny-skia rendering backend |
| `stet-pdf` | PDF output device |

## License

Apache-2.0 OR MIT
