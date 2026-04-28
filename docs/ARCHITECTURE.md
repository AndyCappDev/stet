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
| `Group { elements, params }` | Transparency group (blend mode, alpha, isolated/knockout, group CS) |
| `OcgGroup { elements, ocg_id, default_visible }` | PDF Optional Content Group (layer) |
| `SoftMasked { mask, content, params, mask_cache }` | Soft mask (luminosity or alpha mask) |

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

3. **Operators** (`stet-ops`): ~330 native Rust functions that manipulate
   the operand stack, dictionary stack, graphics state, and display list.
   Path-building operators (`moveto`, `lineto`, `curveto`) construct paths
   on the graphics state. Painting operators (`fill`, `stroke`, `image`)
   append `DisplayElement` entries to the display list. `showpage` finalizes
   the page. The PDF-imaging extension operators
   (`begintransparencygroup`/`beginsoftmask`/`beginoptionalcontent` and
   their close partners — see `docs/PDF-EXTENSIONS.md`) push capture
   frames onto `Context::group_stack`, and paint emits route through
   `Context::current_display_list_mut()` so they land in the innermost
   active scope rather than the page-level list.

4. **Context** (`stet-core`): Central state — operand stack, execution stack,
   dictionary stack, graphics state stack, VM stores (strings, arrays, dicts),
   save/restore stack, and the current display list.

5. **showpage**: Takes the accumulated display list, passes it to the output
   device via `replay_and_show()`, and captures a clone for viewport
   rendering if display list capture is enabled.

### Group stack (PDF-imaging extension scopes)

The PDF-imaging extension operators
(`begintransparencygroup` / `endtransparencygroup`,
`beginsoftmask` / `endsoftmask` / `clearsoftmask`,
`beginoptionalcontent` / `endoptionalcontent`) capture paint into
nested scopes via `Context::group_stack: Vec<GroupFrame>`. While
`group_stack` is non-empty, paint operators emit into the topmost
frame's `display_list` instead of the page-level
`Context::display_list`. The new `current_display_list_mut()` /
`current_display_list()` helpers route every paint emit site
uniformly — `paint_ops`, `clip_ops`, `graphics_state_ops` (clip
restoration after `gsave`/`grestore`/`initgraphics`), `image_ops`,
`shading_ops`, `halftone_ops` form replay, and `show_ops`
(including the Type 3 BuildChar capture window) all resolve to the
active list.

Each `GroupFrame` carries a `GroupKind` discriminating
transparency groups from soft-mask builders (`SoftMask`), implicit
masked-content scopes (`Masked`, opened by `endsoftmask`), and
optional-content (OCG) scopes. On close the frame emits a single
`DisplayElement::Group` / `DisplayElement::SoftMasked` /
`DisplayElement::OcgGroup` element into the next-innermost
target. Frames also snapshot `gstate_stack.len()` at open time so
`grestore`, `restore`, and the close operators refuse to unwind
across the boundary, and `showpage` / `copypage` raise `rangecheck`
while a frame is open. See `docs/PDF-EXTENSIONS.md` for the
operator reference.

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

### PDF Structural API (stet-pdf-reader)

In addition to producing display lists for rendering, `stet-pdf-reader`
exposes the document's structural content as typed Rust data — for
indexers, accessibility tools, link extractors, format converters, and
anything else that needs to *read* a PDF rather than display it.

Every accessor parses lazily on first call (most behind `OnceCell`,
per-page annotations behind a `Vec<OnceCell<...>>`), so a 1000-page
document with bookmarks the caller never asks for doesn't pay to
parse them.

| Accessor on `PdfDocument` | Returns | Source |
|---------------------------|---------|--------|
| `metadata()` | `&DocumentMetadata` | `/Info` dict + XMP `/Metadata` stream |
| `viewer_preferences()` | `&ViewerPreferences` | catalog `/ViewerPreferences` + `/PageLayout` + `/PageMode` |
| `outline()` | `&[OutlineItem]` | catalog `/Outlines` (recursive `/First` / `/Next`) |
| `destinations()` | `&HashMap<String, Destination>` | merged legacy `/Catalog /Dests` + `/Catalog /Names /Dests` name tree |
| `resolve_named_destination(name)` | `Option<Destination>` | shorthand for `destinations().get(name)` |
| `page_annotations(page)` | `&[Annotation]` | per-page `/Annots` array; per-page `OnceCell` cache |
| `form()` | `Option<&FormCatalog>` | catalog `/AcroForm` field tree |
| `page_boxes(page)` | `PageBoxes` (value) | inheritable boxes from `PageInfo`, page-local boxes from the page dict |
| `embedded_files()` | `&HashMap<String, EmbeddedFile>` | catalog `/Names /EmbeddedFiles` name tree |
| `embedded_file_bytes(name)` | `Result<Vec<u8>, PdfError>` | on-demand stream decode |
| `layers()` / `layer(ocg_id)` | `&[Layer]` / `Option<&Layer>` | catalog `/OCProperties /OCGs` |
| `configurations()` / `default_configuration()` / `configuration(idx)` / `layer_tree()` | `&[Configuration]` / `Option<&Configuration>` / `LayerTree` | `/OCProperties /D` + `/Configs`, including `/Order` parsing |
| `layer_set_for(intent)` | `LayerSet` | default config + `/AS` automatic-state rules for the intent |
| `parse_warnings()` | `Ref<'_, [ParseWarning]>` | warnings emitted by the structural parsers |

The implementation lives in sibling modules under
`crates/stet-pdf-reader/src/`: `metadata.rs`, `viewer_prefs.rs`,
`outline.rs`, `destination.rs`, `name_tree.rs` (generic name-tree walker
reusable for embedded files and named destinations), `annotations.rs`,
`form_fields.rs`, `page_boxes.rs`, `embedded_files.rs`, the
`layers/` module (`metadata.rs`, `configuration.rs`, `ocmd.rs`), plus
`diagnostics.rs` for the warning sink. Each module is independently
testable; cross-references are explicit (e.g., a terminal `FormField`
carries `widget_obj_nums: Vec<u32>` so a consumer can find the matching
widgets in `page_annotations()`).

Optional Content support spans crates: the display list (in
`stet-graphics`) carries each `OcgGroup`'s [`OcgVisibility`] predicate
(Single / Membership / Expression) plus a per-variant `default_visible`
fallback baked from the document's default configuration. The
`LayerSet` evaluator (also in `stet-graphics`) lets a consumer
override visibility per OCG without re-parsing the PDF; `stet-render`
holds an `Arc<LayerSet>` on `SkiaDevice` and consults it during
banded / viewport replay. `render_to_rgba_with_layers` and
`PdfDocument::render_page_to_rgba_with_layers` are the
LayerSet-aware entry points.

[`OcgVisibility`]: https://docs.rs/stet-graphics/latest/stet_graphics/display_list/enum.OcgVisibility.html

Walkers that recurse over potentially-cyclic PDF structures
(outline tree, name trees, form-field tree) all bound traversal with a
visited-set + depth cap; truncations push a `ParseWarning` so the
absence of data is never silent.

See [`docs/PDF-READER-API.md`](PDF-READER-API.md) for the public API
reference with examples per accessor, and
[`docs/PDF-LAYERS.md`](PDF-LAYERS.md) for the layer / OCG model
(types, `LayerSet` flow, OCMD semantics, `/VE` grammar,
intent-driven rendering).

## Output Devices

### Built-in Devices

| Device | Crate | Description |
|--------|-------|------------|
| `SkiaDevice` | `stet-render` | Rasterizes to RGBA via the vendored `stet-tiny-skia` fork. Banded rendering, clip caching, ICC color. |
| `PdfDevice` | `stet-pdf` | Converts display lists to PDF with font embedding, image compression, shadings. |
| `NullDevice` | `stet-core` | Discards all output. Used for test suites and display list capture. |

### The OutputDevice Trait

Output devices implement the `OutputDevice` trait from `stet-core`:

```rust
pub trait OutputDevice {
    // Required:
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
    fn set_trim_box(&mut self, llx: f64, lly: f64, urx: f64, ury: f64) {}
    fn replay_and_show(&mut self, list: DisplayList, path: &str) -> Result<(), String>;
    fn finish(&mut self) -> Result<(), String> { Ok(()) }
    fn finish_with_context(&mut self, ctx: &Context) -> Result<(), String> { self.finish() }
    fn as_any(&self) -> &dyn std::any::Any { &() }
}
```

The trait has a default `replay_and_show()` that iterates over a display list
and dispatches each element to the appropriate method. `Group` and
`SoftMasked` elements are no-ops in the default — devices that care about
transparency (currently only `SkiaDevice`) override `replay_and_show()`
with their own banded renderer. `OcgGroup` is unwrapped inline: clip ops
always apply so downstream clips are consistent, but paint ops are gated
on `default_visible`. `Text` elements are ignored by rasterizers and only
consumed by `PdfDevice`.

`set_trim_box` is only meaningful for PDF output; other devices ignore
it. `finish_with_context` gives devices a chance to run context-aware
finalization (e.g., PDF output needs access to the font directory) and
defaults to `finish()`. `as_any` is the downcast escape hatch for
consumers that need the concrete device (e.g., reading `PdfDevice`'s
in-memory bytes).

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
stet-tiny-skia-path  Vendored fork of tiny-skia-path (BSD-3-Clause)
stet-tiny-skia       Vendored fork of tiny-skia — rasterizer (BSD-3-Clause)
     │
stet-fonts           No dependencies (geometry, font parsing, encoding)
     │
stet-graphics        Color types, display list, ICC, mesh shading
     │
stet-core            PS types, Context, VM stores, tokenizer, OutputDevice trait
     │
stet-ops             ~320 operator implementations
     │
stet-engine          Eval loop, parse_and_exec, exec_sync
     │
stet-render          stet-tiny-skia rasterizer, viewport rendering, PNG output
stet-pdf             PDF output device (display list → PDF)
stet-pdf-reader      PDF parser (PDF → display list) — independent of stet-core
stet-viewer          egui desktop viewer
stet (facade)        Batteries-included API with embedded resources
stet-cli             Binary entry point
stet-wasm            WebAssembly bindings (excluded from the main workspace)
```

The two `stet-tiny-skia*` crates are vendored forks of the upstream
tiny-skia / tiny-skia-path crates. They carry their own BSD-3-Clause
licence (separate from the workspace's Apache-2.0 OR MIT) and are
modified for stet's specific rasterisation needs.

### Using Individual Crates

Not every stet crate is coupled to the PostScript interpreter. The
workspace is layered so you can pick up just the pieces you need.

**Zero PS-VM dependency — standalone building blocks:**

| Crate | Internal deps | What it gives you |
|-------|---------------|-------------------|
| `stet-tiny-skia-path` | none | Bezier path primitives (vendored, BSD-3) |
| `stet-tiny-skia` | stet-tiny-skia-path | Software rasterizer (vendored, BSD-3) |
| `stet-fonts` | none | Type 1 / CFF / TrueType parsing, `PsPath`, `Matrix`, AGL, encodings |
| `stet-graphics` | stet-fonts | `DisplayList`, `DeviceColor`, `IccCache`, mesh-shading parser |
| `stet-pdf-reader` | stet-fonts, stet-graphics | PDF → `DisplayList`; no PS interpreter involved |

**Output / rendering crates** pull in `stet-core` for the `OutputDevice`
trait, but do **not** pull in the interpreter (`stet-ops`, `stet-engine`):

| Crate | Internal deps | What it gives you |
|-------|---------------|-------------------|
| `stet-render` | stet-fonts, stet-graphics, stet-core, stet-tiny-skia | `DisplayList` → RGBA (banded, viewport, ICC-aware) |
| `stet-pdf` | stet-fonts, stet-graphics, stet-core | `DisplayList` → PDF bytes |

**Interpreter-only** crates that rarely make sense to depend on in
isolation: `stet-core` (PS VM types), `stet-ops` (operator
implementations), `stet-engine` (eval loop), `stet-viewer` (egui desktop
viewer), `stet-cli` (binary), `stet-wasm` (wasm-bindgen glue).

**Useful external combos:**

- **Pure PDF viewer / rasterizer:** `stet-pdf-reader` + `stet-render` —
  no PostScript VM involved.
- **PDF → PDF normaliser / rewriter:** `stet-pdf-reader` + `stet-pdf`.
- **Custom output format (SVG, TIFF, accessibility tree):**
  `stet-pdf-reader` + your own `DisplayElement` iterator — no rendering
  crate required at all.
- **Font-only workflows:** `stet-fonts` on its own.
- **Batteries-included:** the `stet` facade, which exposes PS
  interpretation, rendering, and PDF output behind feature flags.

The deliberate design choice worth highlighting: **`stet-pdf-reader` has
no dependency on `stet-core`**. The full PDF parser can be linked without
the PostScript VM, operator system, or eval loop — it produces the same
`DisplayList` type that the interpreter produces, and every downstream
consumer treats them identically.
