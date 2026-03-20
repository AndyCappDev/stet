# stet

A production-grade PostScript Level 3 interpreter written in Rust.

stet interprets PostScript and EPS files, producing output as RGBA pixels,
PDF documents, or display lists. It also includes a standalone PDF reader
that converts PDF pages to display lists without the PostScript interpreter.

## Library Usage

```toml
[dependencies]
stet = "0.1"
```

```rust
let mut interp = stet::Interpreter::new();
let pages = interp.render(include_bytes!("document.ps"), 300.0)?;
// pages[0].rgba  — RGBA pixel data (4 bytes/pixel, row-major)
// pages[0].width — pixel width at 300 DPI
```

The `stet` crate embeds all required resources (35 fonts, init scripts,
encodings, ICC color profiles) so there are no external files to ship.

### Output Formats

The interpreter produces a **display list** for each page. The display list
is the central data structure — every output format is derived from it.

| Method | Output | Use case |
|--------|--------|----------|
| `render()` | RGBA pixels + display list | Rasterization, thumbnails, image export |
| `render_to_pdf()` | PDF document bytes | Print-quality vector output |
| `render_to_display_list()` | Display list only | Custom renderers, viewport rendering, analysis |
| `exec()` | Nothing | Test suites, scripting, data extraction |

```rust
// RGBA pixels at 300 DPI
let pages = interp.render(ps_data, 300.0)?;

// PDF output
let pdf_bytes = interp.render_to_pdf(ps_data, 300.0)?;
std::fs::write("output.pdf", &pdf_bytes)?;

// Display list for custom rendering
let pages = interp.render_to_display_list(ps_data, 300.0)?;
for page in &pages {
    for element in page.display_list.elements() {
        // Fill, Stroke, Image, Clip, Shading, Text, Group, ...
    }
}
```

### Viewport Rendering

Display lists support efficient viewport rendering — render any rectangular
region at any zoom level without re-interpreting the PostScript:

```rust
let pages = interp.render_to_display_list(ps_data, 150.0)?;
let prepared = stet::prepare_display_list(&pages[0].display_list);

// Render just the top-left quadrant at 2x zoom
let rgba = stet::render_region_prepared(
    &pages[0].display_list, &prepared,
    0.0, 0.0, 500.0, 500.0,     // viewport in device pixels
    1000, 1000,                   // output pixel dimensions
    150.0, None, None, false,
);
```

### PDF Reader

`stet-pdf-reader` is a separate crate that parses PDF files and converts
pages to display lists. It does **not** depend on the PostScript interpreter —
it can be used standalone for PDF rendering:

```rust
use stet_pdf_reader::PdfDocument;

let doc = PdfDocument::from_bytes(&pdf_data)?;
for page in 0..doc.page_count() {
    let display_list = doc.render_page(page, 300.0)?;
    // Same DisplayList type as the PS interpreter produces
}
```

The display lists from the PDF reader and PostScript interpreter are the
same type (`DisplayList`), so the same rendering pipeline handles both.

### Custom Output Devices

The interpreter communicates with output backends through the `OutputDevice`
trait and the `DisplayList`. You can create custom output formats by
consuming the display list directly:

```rust
let pages = interp.render_to_display_list(ps_data, 300.0)?;
for page in &pages {
    for element in page.display_list.elements() {
        match element {
            DisplayElement::Fill { path, params } => { /* vector fill */ }
            DisplayElement::Stroke { path, params } => { /* vector stroke */ }
            DisplayElement::Image { sample_data, params } => { /* raster image */ }
            DisplayElement::Text { params } => { /* text with font/position */ }
            DisplayElement::AxialShading { params } => { /* linear gradient */ }
            // Clip, InitClip, RadialShading, MeshShading, PatchShading,
            // PatternFill, Group, SoftMasked, ErasePage
            _ => {}
        }
    }
}
```

Display list elements include all the information needed to render: paths are
already transformed to device coordinates, colors are resolved, images contain
raw sample data, and fonts are referenced by entity ID with glyph paths available.

See the [Architecture Guide](docs/ARCHITECTURE.md) for how the crates fit
together, and the [Display List Reference](docs/DISPLAY-LIST.md) for
complete element documentation.

### Feature Flags

| Feature | Default | Description |
|---------|---------|------------|
| `render` | yes | RGBA pixel output via `stet-render` (tiny-skia) |
| `pdf-output` | yes | PDF output via `stet-pdf` |

For the smallest dependency footprint (display lists only):

```toml
[dependencies]
stet = { version = "0.1", default-features = false }
```

### Configuration

```rust
let mut interp = stet::Interpreter::builder()
    .no_icc()             // disable ICC color management
    .suppress_output()    // silence PS print/==/= operators
    .build();
```

## CLI Usage

```bash
cargo install stet-cli
```

```bash
stet --device png document.ps          # PostScript → PNG
stet --device pdf document.ps          # PostScript → PDF
stet --device png document.pdf         # PDF → PNG
stet document.ps                       # Interactive viewer
stet                                   # REPL (viewer opens on first showpage)
```

See the [Viewer Guide](docs/VIEWER-GUIDE.md) for keyboard/mouse controls,
zoom presets, minimap navigation, and drag-and-drop.

### Options

| Option | Description |
|--------|------------|
| `--device <TYPE>` | Output: `png`, `pdf`, `viewer` (default), `null` |
| `--dpi <DPI>` | Resolution (overrides device default; all built-in devices default to 300) |
| `--pages <RANGE>` | Page filter: `1`, `1-5`, `2,4,6` |
| `--no-icc` | Disable ICC color management |
| `--no-aa` | Disable anti-aliasing |
| `--overprint` | Enable overprint simulation |
| `--profile <FILE>` | Specify ICC output profile |

## Crate Overview

```
                         ┌──────────┐
                         │ stet-cli │  Binary: file I/O, REPL, arg parsing
                         └────┬─────┘
                              │
              ┌───────────────┼───────────────┐
              v               v               v
        ┌──────────┐   ┌───────────┐   ┌─────────────┐
        │   stet   │   │stet-viewer│   │stet-pdf-    │
        │ (facade) │   │  (egui)   │   │  reader     │
        └────┬─────┘   └─────┬─────┘   └──────┬──────┘
             │               │                 │
     ┌───────┼───────┐       │                 │
     v       v       v       v                 │
┌────────┐┌──────┐┌───────────┐                │
│stet-pdf││stet- ││stet-render│                │
│(output)││engine││(tiny-skia)│                │
└───┬────┘└──┬───┘└─────┬─────┘                │
    │        v          │                      │
    │   ┌─────────┐     │                      │
    │   │stet-ops │     │                      │
    │   └────┬────┘     │                      │
    │        v          │                      │
    ├──►┌─────────┐◄────┘                      │
    │   │stet-core│                            │
    │   └────┬────┘                            │
    │        v                                 │
    │   ┌────────────┐◄────────────────────────┘
    └──►│stet-       │
        │  graphics  │
        └─────┬──────┘
              v
        ┌───────────┐
        │stet-fonts │
        └───────────┘
```

See the [Architecture Guide](docs/ARCHITECTURE.md) for a detailed explanation
of how these crates work together.

## Building from Source

```bash
cargo build                    # Build all crates
cargo test                     # Run all tests (~720 tests)
cargo run -- file.ps           # Run a PostScript file
cargo run                      # Interactive REPL
cargo clippy                   # Lint
```

### WASM Viewer

```bash
cd web && ./build.sh           # Build WASM module
python3 serve.py               # Serve at localhost:8000
```

## License

Apache-2.0 OR MIT
