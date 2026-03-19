# stet

A production-grade PostScript Level 3 interpreter written in Rust.

## Use as a Library

Add the `stet` crate to render PostScript in your own application:

```toml
[dependencies]
stet = "0.1"
```

```rust
let mut interp = stet::Interpreter::new();
let pages = interp.render(include_bytes!("document.ps"), 300.0)?;
// pages[0].rgba — RGBA pixel data, 4 bytes/pixel
```

Three output modes:
- **`render()`** — RGBA pixels (default)
- **`render_to_pdf()`** — PDF document bytes (default)
- **`render_to_display_list()`** — display lists for custom rendering

See the [`stet` crate documentation](crates/stet/README.md) for the full API.

## Use as a CLI

```bash
cargo install stet-cli
```

```bash
# Render PostScript to PNG
stet --device png document.ps

# Render to PDF
stet --device pdf document.ps

# Interactive REPL
stet

# Interactive viewer (with viewer feature)
stet document.ps
```

### CLI Options

```
stet [OPTIONS] [FILES...]

Options:
  --device <DEVICE>    Output device: png, pdf, viewer, null (default: viewer)
  --dpi <DPI>          Resolution in dots per inch
  --pages <RANGE>      Page range (e.g., 1, 1-5, 2,4,6)
  --no-icc             Disable ICC color management
  --no-aa              Disable anti-aliasing
  --overprint          Enable overprint simulation
  --profile <FILE>     Use a specific ICC output profile
```

## Crate Architecture

```
stet              — Batteries-included library API (facade)
stet-core         — Interpreter infrastructure: types, VM, tokenizer
stet-ops          — ~268 PostScript operator implementations
stet-engine       — Execution engine (eval loop)
stet-fonts        — Font parsing: Type 1, CFF/Type 2, TrueType
stet-graphics     — Display list, color types, ICC color management
stet-render       — tiny-skia rendering backend, PNG output
stet-pdf          — PDF output device (PS → PDF)
stet-pdf-reader   — PDF input parser (PDF → display lists)
stet-viewer       — Interactive egui/winit viewer
stet-cli          — Command-line interface
```

## Building

```bash
cargo build                    # Build all crates
cargo test                     # Run all tests
cargo run -- file.ps           # Run a PostScript file
cargo run                      # Interactive REPL
```

### WASM Viewer

```bash
cd web && ./build.sh           # Build WASM module
python3 serve.py               # Serve at localhost:8000
```

## License

Apache-2.0 OR MIT
