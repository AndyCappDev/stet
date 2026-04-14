# Architecture Guide

This document describes how stet's crates work together, the role of the
display list, how output devices are plugged in, and how to extend the
system with custom renderers.

## Overview

stet has two independent pipelines that produce the same output type:

```
PostScript source  ──►  PS Interpreter  ──►  DisplayList  ──►  Output
PDF file           ──►  PDF Reader      ──►  DisplayList  ──►  Output
```

The **display list** is the meeting point. Both pipelines produce a
`Vec<DisplayElement>` per page, and every output format — PNG, PDF,
viewport render, or custom — consumes display lists.

## Why a Display List?

Most rendering engines (including GhostScript and hayro) interpret and
render in a single pass: the interpreter calls directly into the
rasterizer, and once a page is drawn the source must be re-interpreted to
draw it again. stet takes a fundamentally different approach — the
interpreter produces an intermediate **display list**, and rendering is a
separate step that consumes it.

This decoupling is the foundation of stet's architecture, and it enables
capabilities that are difficult or impossible to retrofit onto a
direct-rendering pipeline:

- **Viewport rendering.** Render any rectangular region of a page at any
  zoom level without re-interpreting the source. The display list is data
  that can be queried, culled, and projected onto arbitrary viewports. This
  is what powers the desktop viewer's pan/zoom and the WASM viewer's
  on-demand tile rendering.

- **Multiple outputs from one interpretation.** A single interpretation
  pass produces a display list that can be rasterized to PNG, converted to
  PDF, displayed in a viewer, or consumed by a custom output device — all
  without re-parsing the source.

- **Pipelined multi-page rendering.** The interpreter can build page N+1's
  display list while the rasterizer is still rendering page N in the
  background. Display lists are self-contained values that can be handed
  off across threads.

- **Cancellation and streaming.** Banded rendering processes the display
  list in chunks. Cancellation between bands is trivial — check a flag
  and bail. There is no deeply-nested interpreter state to unwind.

- **Layer toggling.** The display list architecture makes it straightforward
  to tag elements with their PDF Optional Content Group (OCG) and filter by
  layer visibility at render time — no re-interpretation required. (Not yet
  implemented; the OCG parsing and BDC/EMC tracking infrastructure is in
  place.)

- **Caching.** Store the display list and re-render at different
  DPI, zoom, or viewport without re-parsing. The prepare/cache pipeline
  (bounding boxes, image conversion, ICC transforms) is computed once and
  reused across renders.

- **Custom output devices.** The display list is a public, documented data
  structure. Building a new output format (SVG, TIFF, accessibility tree,
  diffing tool) is a matter of iterating over elements — no need to hook
  into interpreter internals.

The tradeoff is that display lists use memory and replay has per-element
overhead. For pages with tens of thousands of elements, a direct-rendering
engine avoids that cost. But the design ceiling is higher: the display list
is data you can index, filter, cache, and parallelize over. A
direct-rendering pipeline is a black box that runs start-to-finish.

## The Display List

See also the [Display List Reference](DISPLAY-LIST.md) for complete field
documentation and code examples.

A `DisplayList` is a flat sequence of `DisplayElement` variants that
describe everything on a page in device coordinates:

| Element | Description |
|---------|------------|
| `Fill { path, params }` | Fill a path (color, fill rule, transform) |
| `Stroke { path, params }` | Stroke a path (color, line width, dash, join, cap) |
| `Image { sample_data, params }` | Raster image (raw samples, dimensions, color space, transform) |
| `Text { params }` | Text run (font, size, position, glyphs) — used by PDF device |
| `Clip { path, params }` | Intersect clip region with a path |
| `InitClip` | Reset clip to full page |
| `ErasePage` | Clear the page to white |
| `AxialShading { params }` | Linear gradient |
| `RadialShading { params }` | Radial gradient |
| `MeshShading { params }` | Gouraud triangle mesh (Types 4/5) |
| `PatchShading { params }` | Coons/tensor patch mesh (Types 6/7) |
| `PatternFill { params }` | Tiled pattern |
| `Group { elements, params }` | Transparency group (blend mode, alpha, isolated/knockout) |
| `SoftMasked { content, mask, params }` | Soft mask (luminosity or alpha mask) |

Paths are already transformed through the CTM to device coordinates at
construction time. Colors are resolved. Images contain raw sample data in
their native color space. This means a consumer of the display list does
not need to understand PostScript graphics state — everything is explicit.

**DPI matters at interpretation time.** Because the display list is in device
coordinates, the DPI chosen during interpretation determines the coordinate
space of the display list. For best results, set the reference DPI to
match the intended final output resolution. Rendering at a different DPI
later (e.g., zooming in the viewer) scales the device-space coordinates.

## Pipeline: PostScript Interpretation

```
                    ┌──────────────────────────────────────────────────┐
 PS source ──►  Tokenizer ──►  Eval Loop ──►  Operators ──►  Context   │
                    │              │               │            │      │
                    │          (e_stack)      (o_stack)     (display_  │
                    │                                        list)     │
                    └──────────────────────────────────────────┬───────┘
                                                               │
                                             showpage ──► DisplayList
```

1. **Tokenizer** (`stet-core`): Converts bytes to PostScript tokens
   (numbers, names, strings, operators, procedure bodies).

2. **Eval loop** (`stet-engine`): Processes the execution stack one object
   at a time. Executable names are looked up in the dictionary stack and
   dispatched; procedures are stepped through element by element.

3. **Operators** (`stet-ops`): ~268 native Rust functions that manipulate
   the operand stack, dictionary stack, graphics state, and display list.
   Path-building operators (`moveto`, `lineto`, `curveto`) construct paths
   on the graphics state. Painting operators (`fill`, `stroke`, `image`)
   append `DisplayElement` entries to the display list. `showpage` finalizes
   the page.

4. **Context** (`stet-core`): Central state — operand stack, execution stack,
   dictionary stack, graphics state stack, VM stores (strings, arrays, dicts),
   save/restore stack, and the current display list.

5. **showpage**: Takes the accumulated display list, passes it to the output
   device via `replay_and_show()`, and captures a clone for viewport
   rendering if display list capture is enabled.

## Pipeline: PDF Reading

```
PDF bytes ──►  Parser ──►  Resolver ──►  Content Interpreter ──►  DisplayList
                 │             │                  │
           (xref, objects)  (deref)      (PDF operators → elements)
```

The PDF reader (`stet-pdf-reader`) is completely independent of the PostScript
interpreter. It has no dependency on `stet-core` — only on `stet-fonts`
(for font parsing) and `stet-graphics` (for the `DisplayList` type).

1. **Parser**: Reads PDF cross-reference tables, decrypts if needed,
   decompresses object streams.

2. **Resolver**: Dereferences indirect object references, applies stream
   filters (Flate, LZW, DCT, JPX, CCITT, JBIG2, ASCII85, etc.).

3. **Content interpreter**: Walks the page content stream, interpreting
   PDF operators (`m`, `l`, `c`, `re`, `f`, `S`, `Do`, `Tj`, `TJ`, etc.)
   and building a `DisplayList`. Handles:
   - Path construction and painting
   - Color spaces (DeviceRGB, DeviceCMYK, ICCBased, Indexed, etc.)
   - Images (inline and XObject) with all filter types
   - Fonts (Type 1, TrueType, CFF, CID) with encoding and CMap resolution
   - Transparency groups and soft masks
   - Tiling patterns and shadings

Because both pipelines produce the same `DisplayList` type, every downstream
consumer (rasterizer, PDF writer, viewport renderer) works with both sources.

## Output Devices

### Built-in Devices

| Device | Crate | Description |
|--------|-------|------------|
| `SkiaDevice` | `stet-render` | Rasterizes to RGBA via tiny-skia. Banded rendering, clip caching, ICC color. |
| `PdfDevice` | `stet-pdf` | Converts display lists to PDF with font embedding, image compression, shadings. |
| `NullDevice` | `stet-core` | Discards all output. Used for test suites and display list capture. |

### The OutputDevice Trait

Output devices implement the `OutputDevice` trait from `stet-core`:

```rust
pub trait OutputDevice {
    fn fill_path(&mut self, path: &PsPath, params: &FillParams);
    fn stroke_path(&mut self, path: &PsPath, params: &StrokeParams);
    fn clip_path(&mut self, path: &PsPath, params: &ClipParams);
    fn init_clip(&mut self);
    fn erase_page(&mut self);
    fn show_page(&mut self, output_path: &str) -> Result<(), String>;
    fn draw_image(&mut self, sample_data: &[u8], params: &ImageParams);
    fn page_size(&self) -> (u32, u32);

    // Optional — default implementations provided:
    fn paint_axial_shading(&mut self, params: &AxialShadingParams) {}
    fn paint_radial_shading(&mut self, params: &RadialShadingParams) {}
    fn paint_mesh_shading(&mut self, params: &MeshShadingParams) {}
    fn paint_patch_shading(&mut self, params: &PatchShadingParams) {}
    fn paint_pattern_fill(&mut self, params: &PatternFillParams) {}
    fn replay_and_show(&mut self, list: DisplayList, path: &str) -> Result<(), String>;
    fn finish(&mut self) -> Result<(), String> { Ok(()) }
}
```

The trait has a default `replay_and_show()` that iterates over a display list
and dispatches each element to the appropriate method. Devices like `SkiaDevice`
override this with optimized banded rendering.

### Creating a Custom Output Device

To add a new output format (e.g., SVG, TIFF, or a streaming protocol), you
have two options:

**Option A: Consume the display list directly** (recommended for most cases)

Use `render_to_display_list()` and iterate over the elements yourself:

```rust
let mut interp = stet::Interpreter::new();
let pages = interp.render_to_display_list(ps_data, 300.0)?;

for page in &pages {
    let mut svg = SvgBuilder::new(page.width, page.height);
    for element in page.display_list.elements() {
        match element {
            DisplayElement::Fill { path, params } => svg.fill(path, params),
            DisplayElement::Stroke { path, params } => svg.stroke(path, params),
            // ... handle other element types
            _ => {}
        }
    }
    svg.save("output.svg")?;
}
```

This is the simplest approach and doesn't require implementing any traits.

**Option B: Implement OutputDevice** (for tight integration with the interpreter)

Implement the `OutputDevice` trait and wire it as the device factory on the
interpreter context. This gives you streaming per-page output during
interpretation:

```rust
struct MyDevice { /* ... */ }

impl OutputDevice for MyDevice {
    fn fill_path(&mut self, path: &PsPath, params: &FillParams) { /* ... */ }
    fn stroke_path(&mut self, path: &PsPath, params: &StrokeParams) { /* ... */ }
    // ... implement required methods
}

let mut interp = stet::Interpreter::new();
let ctx = interp.context();
ctx.device_factory = Some(Box::new(|w, h| {
    Box::new(MyDevice::new(w, h))
}));
```

## Rendering Pipeline (stet-render)

The rasterizer in `stet-render` uses a multi-stage pipeline:

```
DisplayList
    │
    ├──► prepare_display_list()     Pre-compute bounding boxes, clip epochs
    │         │
    │         ▼
    │    PreparedDisplayList
    │         │
    ├──► build_icc_cache_for_list()  Extract ICC profiles from images
    │         │
    │         ▼
    │    IccCache
    │         │
    ├──► ImageCache::build()         Pre-convert images to RGBA
    │         │
    │         ▼
    │    ImageCache
    │         │
    └──► render_region_prepared()    Rasterize viewport region
              │
              ▼
         Vec<u8> (RGBA pixels)
```

The prepare/cache steps are done once per page. Viewport rendering can then
be called repeatedly with different regions and zoom levels without
re-interpreting or re-preparing.

### Banded Rendering

For large pages, the rasterizer splits the output into horizontal bands
sized to fit in L2 cache. Each band is rendered independently, enabling:

- **Memory efficiency**: Only one band's pixel buffer is live at a time
- **Parallelism**: Bands are rendered in parallel via rayon (when the
  `parallel` feature is enabled)
- **Streaming output**: Bands can be written to a `PageSink` incrementally

## Resource System

The PostScript interpreter requires several resource files to function:

- **Init scripts** (4 files): Bootstrap the resource system, error handlers,
  font categories, and font name mappings
- **Fonts** (35 Type 1 files): URW equivalents of the standard PostScript fonts
- **Encodings** (3 files): StandardEncoding, ISOLatin1Encoding, SymbolEncoding
- **CMap** (2 files): Identity-H, Identity-V
- **ProcSet** (2 files): CIDInit, FontSetInit
- **ICC profile** (1 file): CC0-licensed CMYK → sRGB conversion profile

The `stet` facade crate embeds all 53 files (4.6 MB) via `include_bytes!()`.
The CLI discovers them relative to the executable. The WASM build embeds
them in a virtual filesystem.

### CJK CMap Files (PDF Reader)

PDFs using CJK fonts with predefined encodings (e.g. `GBK-EUC-H`,
`90ms-RKSJ-H`, `ETen-B5-H`, `KSCms-UHC-H`) require CMap files that map
character codes to CIDs. These are **not** embedded in the binary — they
are loaded from the filesystem at runtime.

Search order:

1. **`STET_CMAP_DIR`** environment variable — point to a directory
   containing CMap files (flat layout, e.g. `$STET_CMAP_DIR/GBK-EUC-H`)
2. **`~/.local/share/stet/CMap/`** — user-local conventional location
3. **System poppler-data** — `/usr/share/poppler/cMap/Adobe-*/` (Linux),
   Homebrew paths (macOS)
4. **System GhostScript** — `/var/lib/ghostscript/CMap/` etc.

**Setup by platform:**

- **Linux**: `sudo apt install poppler-data` (Debian/Ubuntu) or equivalent
- **macOS**: `brew install poppler-data`
- **Windows / other**: Download the
  [Adobe CMap resources](https://github.com/nicferrier/python-ghostscript/tree/master/ghostscript/CMap)
  and either set `STET_CMAP_DIR` or place them in `~/.local/share/stet/CMap/`

If a required CMap is not found, a warning is printed and CJK text in the
affected font will not render correctly.

## Crate Dependency Graph

```
stet-fonts           No dependencies (geometry, font parsing, encoding)
     │
stet-graphics        Color types, display list, ICC, mesh shading
     │
stet-core            PS types, Context, VM stores, tokenizer, OutputDevice trait
     │
stet-ops             ~268 operator implementations
     │
stet-engine          Eval loop, parse_and_exec, exec_sync
     │
stet-render          tiny-skia rasterizer, viewport rendering, PNG output
stet-pdf             PDF output device (display list → PDF)
stet-pdf-reader      PDF parser (PDF → display list) — independent of stet-core
stet-viewer          egui desktop viewer
stet (facade)        Batteries-included API with embedded resources
stet-cli             Binary entry point
stet-wasm            WebAssembly bindings
```

### Independence of stet-pdf-reader

The PDF reader depends only on `stet-fonts` and `stet-graphics`. It does not
use the PostScript VM, operator system, or eval loop. This means it can be
used in contexts where PostScript interpretation is not needed — for example,
a pure PDF viewer or a PDF-to-image converter.
