<h1 align="center">stet</h1>

<p align="center">A modern, open-source PostScript and PDF rendering engine written in pure Rust.</p>

<p align="center">
  <img src="https://img.shields.io/badge/Version-0.1.0-blue" alt="Version 0.1.0">
  <img src="https://img.shields.io/badge/License-Apache--2.0_OR_MIT-green" alt="License Apache-2.0 OR MIT">
  <img src="https://img.shields.io/badge/Rust-1.85+-orange" alt="Rust 1.85+">
</p>

## About

stet interprets PostScript (Level 3) and parses PDF files, rendering both
to PNG images, PDF documents, or display lists through a unified pipeline.
It can also render them in an interactive desktop viewer or a browser-based
WASM viewer.

The PostScript interpreter and PDF reader are independent — use either or
both — but they produce the same display list type, so every output device
and rendering path works with both sources.

### Try it online

<!-- The URL below follows GitHub's project-Pages convention for
     AndyCappDev/stet. Pages deployment itself is still pending — see
     docs/PUBLISH-TODO.md. -->
A browser-based WASM build lives at
**[andycappdev.github.io/stet](https://andycappdev.github.io/stet/)** —
drop a PS, EPS, or PDF on the page and stet renders it client-side.

The online build is a **capability sampler, not a production viewer**. It
exists so you can check stet's rendering quality on your own files before
installing anything. In particular:

- **No system fonts.** A browser WASM sandbox can't reach the OS font
  directories, so font coverage is limited to the 35 URW fonts embedded
  in the binary plus whatever the source PDF embeds. Documents that
  expect a specific unembedded font will fall back to a URW substitute.
- **Fixed zoom stops** (fit, 75, 150, 300, 600 DPI) rather than
  arbitrary zoom — re-rasterizing at every scroll was too slow in
  single-threaded WASM to feel responsive.
- **Single-threaded WASM.** No rayon parallelism. Rendering is ~2× slower
  than native stet.

For production work, use the native crates below.

### Display List Architecture

Unlike rendering engines that interpret and rasterize in a single pass,
stet decouples the two: interpreters produce an intermediate **display
list**, and rendering is a separate step that consumes it. This enables
viewport rendering at arbitrary zoom without re-interpretation, pipelined
multi-page rendering, trivial cancellation between render bands, multiple
output formats from a single interpretation pass, and display list caching
for repeated renders at different resolutions. See the
[Architecture Guide](docs/ARCHITECTURE.md) for details.

## Features

**PostScript Interpreter**
- Full PostScript Level 3 with ~320 operators
- Type 1, CFF/Type 2, TrueType, CID, and Type 3 font rendering
- All 7 shading types (axial, radial, Gouraud mesh, Coons/tensor patch)
- CIE color spaces (CIEBasedABC, CIEBasedA, CIEBasedDEF, CIEBasedDEFG)
- ICC color management with system CMYK profile auto-detection
- Filters: ASCII85, ASCIIHex, Flate, LZW, RunLength, DCT (JPEG), eexec, SubFile
- Resource system with embedded fonts (35 URW equivalents of the standard PS fonts)
- Interactive REPL with `executive`

**PDF Reader**
- PDF 1.0–2.0 parsing with cross-reference tables and streams
- Encryption: RC4, AES-128, AES-256
- Filters: Flate, LZW, ASCII85, ASCIIHex, RunLength, DCT, JPXDecode (JPEG 2000), CCITTFax, JBIG2
- All PDF color spaces including ICCBased, Separation, DeviceN, Indexed
- Transparency groups (isolated, knockout), soft masks, blend modes
- Font rendering: Type 1, TrueType, CFF, CID with CMap/encoding support
- Annotations (form fields, stamps)
- No dependency on the PostScript interpreter — usable standalone

**Rendering & Output**
- RGBA rasterization via [`stet-tiny-skia`](https://crates.io/crates/stet-tiny-skia) (banded, multi-threaded)
- PDF output with native CMYK, spot colors, ICC profiles, transfer functions, halftone screens, overprint, and font embedding
- PNG file output
- Viewport rendering: render any region at any zoom from a stored display list
- Interactive desktop viewer (egui) with zoom, pan, minimap, drag-and-drop
- WASM viewer for browser-based rendering
- Display list as a public API for building custom output devices

**Print Production**
- Native CMYK color preservation (no lossy RGB round-trip)
- Separation and DeviceN (spot color) support with tint transforms
- Overprint and overprint mode (OPM) for both rasterizer simulation and PDF output
- Transfer functions, halftone screens, black generation, and undercolor removal carried per display element
- Rendering intent preservation
- PDF/X-3 OutputIntent with ICC output profile embedding
- Trim box support

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
| `render` | yes | RGBA pixel output via `stet-render` (`stet-tiny-skia`) |
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
| `--threads <N>` | Worker-thread count (default: 75 % of cores in viewer mode, 8 otherwise) |
| `--no-icc` | Disable ICC color management entirely |
| `--no-aa` | Disable anti-aliasing |
| `--output-profile <FILE>` | Generic ICC output profile (also used as source CMYK when `--cmyk-profile` is absent) |
| `--cmyk-profile <FILE>` | Pin the source CMYK ICC profile for CMYK→sRGB conversion |
| `--no-output-intent` | Ignore the PDF's embedded OutputIntent (default: honoured) |
| `--bpc <on\|off\|auto>` | Black-point compensation (default: `auto`, currently equivalent to `on`) |

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

| Crate | Role |
|-------|------|
| `stet` | Batteries-included library API (facade) |
| `stet-core` | Interpreter infrastructure: types, VM, tokenizer |
| `stet-ops` | ~320 PostScript operator implementations |
| `stet-engine` | Execution engine (eval loop) |
| `stet-fonts` | Font parsing: Type 1, CFF/Type 2, TrueType |
| `stet-graphics` | Display list, color types, ICC color management |
| `stet-render` | Rasterization backend, PNG output |
| `stet-tiny-skia` | Modified [tiny-skia](https://github.com/RazrFalcon/tiny-skia) fork with stet-specific optimizations (BSD-3-Clause) |
| `stet-tiny-skia-path` | Companion path/stroker crate for `stet-tiny-skia` (BSD-3-Clause) |
| `stet-pdf` | PDF output device (PS → PDF) |
| `stet-pdf-reader` | PDF input parser (PDF → display lists) |
| `stet-viewer` | Interactive egui/winit desktop viewer |
| `stet-cli` | Command-line interface and REPL |
| `stet-wasm` | WebAssembly bindings for browser rendering |

See the [Architecture Guide](docs/ARCHITECTURE.md) for a detailed explanation
of how these crates work together.

## Building from Source

```bash
cargo build                    # Build all crates
cargo test                     # Run all tests (759 passing)
cargo run -- file.ps           # Run a PostScript file
cargo run                      # Interactive REPL
cargo clippy                   # Lint
```

### WASM Viewer

```bash
cd web && ./build.sh           # Build WASM module
python3 serve.py               # Serve at localhost:8000
```

## Contributing

`cargo test` runs with no extra setup. The PDF visual-regression
harness (`./pdf_visual_test.sh`) needs a local corpus of test PDFs,
which the project doesn't ship (most are third-party). To reproduce
PDF-rendering bugs or check for regressions across a large corpus:

```bash
# 1. Fetch public test corpora into pdf_samples/ (clones with
#    sparse-checkout so only the PDFs are pulled).
./scripts/fetch_test_pdfs.sh            # all corpora
./scripts/fetch_test_pdfs.sh --list     # see what's available

# 2. Generate your local baseline on a known-good commit
#    (typically `main` before your changes).
./pdf_visual_test.sh --baseline

# 3. Switch to your feature branch and compare.
./pdf_visual_test.sh
```

Any PDFs you already have at the top level of `pdf_samples/` keep
working; the fetcher drops new corpora into their own subdirs
(e.g. `pdf_samples/pdfjs/`) and the visual-test harness walks the
tree so both flat and subdir layouts are picked up. Corpus
subdirectories are gitignored — nothing third-party lands in a
commit.

## Acknowledgements

- **[hayro](https://github.com/LaurenzV/hayro)** — PDF renderer by
  Laurenz Stampfl. `stet-pdf-reader` uses the project's
  [`hayro-jpeg2000`](https://crates.io/crates/hayro-jpeg2000),
  [`hayro-jbig2`](https://crates.io/crates/hayro-jbig2), and
  [`hayro-ccitt`](https://crates.io/crates/hayro-ccitt) crates for
  JPEG 2000, JBIG2, and CCITT-Fax stream decoding — the PDF filters
  that have no other pure-Rust implementation. Big thanks to the hayro
  project for factoring these out as reusable crates.
- **[tiny-skia](https://github.com/RazrFalcon/tiny-skia)** by
  [Yevhenii Reizner](https://github.com/RazrFalcon) — a Skia subset
  ported to Rust. `stet-tiny-skia` / `stet-tiny-skia-path` are modified
  forks (see each crate's README for the specific changes).
- **[moxcms](https://crates.io/crates/moxcms)** — pure-Rust ICC colour
  management. `stet-graphics` uses it for CMYK↔sRGB conversion, image
  bulk transforms, and black-point compensation.

## License

Apache-2.0 OR MIT
