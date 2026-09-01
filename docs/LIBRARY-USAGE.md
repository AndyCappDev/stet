# Library Usage

Calling stet from Rust rather than from the command line. For the CLI, the
install options, and what stet is, see the [README](../README.md).


```toml
[dependencies]
stet = "0.8"
```

> **Upgrading?** Cargo will not auto-bump across these pre-1.0 minors, each
> of which is a compatibility boundary. Two breaking surfaces so far:
> **0.5.0** widened `PsValue::Int` from `i32` to `i64` (so `match` arms bind
> an `i64`, and `as_i32()` now returns `None` out of range instead of always
> `Some`), and **0.2.0** added `#[non_exhaustive]` to ~40 public
> match-surface enums (`DisplayElement`, `PsError`, `Destination`, the
> pdfmark records, …), so `match` sites need a `_ => { ... }` wildcard arm.
> See [CHANGELOG.md](../CHANGELOG.md) for the details of each.

```rust
let mut interp = stet::Interpreter::new();
let pages = interp.render(include_bytes!("document.ps"), 300.0)?;
// pages[0].rgba  — RGBA pixel data (4 bytes/pixel, row-major)
// pages[0].width — pixel width at 300 DPI
```

The `stet` crate embeds all required resources (35 fonts, init scripts,
encodings, ICC color profiles) so there are no external files to ship.

## Output Formats

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

## Diagnostics

An empty page list is not necessarily an error. The usual cause is a program
that painted marks and then ended without calling `showpage` — the page is
discarded and `render()` returns `Ok(vec![])`, which reads like a legitimate
result. `warnings()` distinguishes the two:

```rust
let pages = interp.render(ps_data, 300.0)?;
if pages.is_empty() {
    for w in interp.warnings() {
        eprintln!("warning: {}\n         {}", w, w.hint());
    }
}
```

Warnings describe the most recent render call and are cleared at the start of
the next one. Programs that install `nulldevice` are exempt — that is the
PLRM-sanctioned way to ask for no output, so unemitted marks are expected
there. The CLI prints the same diagnostic to stderr.

## Viewport Rendering

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

## PDF Reader

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

For layer-aware rendering, build a `LayerSet` and pass it through:

```rust
use stet_pdf_reader::{PdfDocument, RenderIntent, layers};

let doc = PdfDocument::from_bytes(&pdf_data)?;

// Hide print-only watermarks for an interactive view.
let view_set = doc.layer_set_for(RenderIntent::View);
let (rgba, w, h) = doc.render_page_to_rgba_with_layers(0, 150.0, &view_set)?;

// Or build a custom override set and toggle one layer.
let mut custom = layers::layer_set_from_document(&doc);
custom.set(/* ocg_id */ 42, false);
let (rgba, w, h) = doc.render_page_to_rgba_with_layers(0, 150.0, &custom)?;
```

Full layer reference: [`PDF-LAYERS.md`](PDF-LAYERS.md).
Runnable example: `cargo run --example render_pdf_layers -- some.pdf`
(see [`crates/stet/examples/render_pdf_layers.rs`](../crates/stet/examples/render_pdf_layers.rs)).

## Custom Output Devices

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

See the [Architecture Guide](ARCHITECTURE.md) for how the crates fit
together, and the [Display List Reference](DISPLAY-LIST.md) for
complete element documentation.

## Feature Flags

| Feature | Default | Description |
|---------|---------|------------|
| `render` | yes | RGBA pixel output via `stet-render` (`stet-tiny-skia`) |
| `pdf-output` | yes | PDF output via `stet-pdf` |

For the smallest dependency footprint (display lists only):

```toml
[dependencies]
stet = { version = "0.8", default-features = false }
```

## Configuration

```rust
let mut interp = stet::Interpreter::builder()
    .no_icc()             // disable ICC color management
    .suppress_output()    // silence PS print/==/= operators
    .build();
```
