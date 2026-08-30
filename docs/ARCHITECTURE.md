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

- **Layer toggling.** Display-list elements are tagged with their PDF
  Optional Content Group (OCG) via `DisplayElement::OcgGroup`, and a
  per-render `LayerSet` (in `stet-graphics`) overrides visibility at
  render time — no re-interpretation required. See
  [`docs/PDF-LAYERS.md`](PDF-LAYERS.md) for the runtime visibility model.

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

3. **Operators** (`stet-ops`): 331 native Rust functions that manipulate
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

### Local and global VM (PLRM 3.7.2)

Composite objects live in one of two arenas, selected by the
`currentglobal` allocation mode and recorded in the tag bit of every
`EntityId`. PLRM 3.7.2 forbids a composite in **global** VM from
referencing a composite in **local** VM, because `restore` may release
the local object and leave the global one dangling.

Two mechanisms uphold this:

1. **Operator enforcement.** `put`, `def`, `store`, `astore`,
   `putinterval`, `copy`, and the `[`…`]` / `<<`…`>>` constructors each
   raise `invalidaccess` when a PostScript program attempts such a
   store. These checks consult the `EntityId` tag rather than
   `ObjFlags`, because an object retrieved via `get` or `currentdict`
   can have lost its flag bit. `unit_tests/vm_global_local_tests.ps`
   covers the full PLRM 3.7.2 table.

2. **The VM audit** (`stet_core::vm_audit`). Operator checks only cover
   stores a *program* makes; the interpreter's own Rust code writes into
   dictionaries and arrays directly, and `ArrayStore::get_mut` hands out
   `&mut [PsObject]`, so those writes pass no checkpoint. `audit_global_vm`
   sweeps both global entity tables and reports every global→local
   reference regardless of how it got there. Run it via
   `cargo run --release --example audit_vm -- [--show-permanent] file.ps`.

Not every global→local reference is a defect. PLRM 3.7.5 explicitly
exempts `systemdict`, which holds the local `userdict`, `errordict`,
`$error`, `statusdict`, and `FontDirectory`. What makes those safe is
lifetime, not identity: they are built during bootstrap, before any
`save`, so no `restore` can reclaim them. The audit classifies on that
basis — `Violation::target_reclaimable` — and
`audit_global_vm_unsafe_only` returns just the references that could
actually dangle. That set being empty is what allowed `restore` to start
releasing local VM; see `crates/stet-cli/tests/vm_audit.rs`.

#### What `restore` reclaims

`save` records the high-water marks of each local store (`VmMarks`), and
`restore` truncates the string/array/dict payloads and their entity
tables back to them, so the memory a save level allocated becomes
available again. It also rewinds `gstate_store` and purges the
entity-keyed caches (`glyph_caches`, `form_cache`), since an `EntityId`
becomes reusable the moment a table shrinks. Global VM is untouched —
save/restore never affects it.

Truncation is sound because `check_invalidrestore` refuses any `restore`
that would strand a reachable composite created after the save, and
because `cow_copy` leaves surviving data at its original offset, below
the mark, parking the discarded mutated copy above it.
[`Context::vm_restore`] backs that argument with a debug-build
assertion — see [`vm_audit::audit_dangling_refs`].

This is **not** garbage collection. `vmreclaim` remains a no-op, and a
program that allocates in a loop with no `save` in scope still grows
without bound. In practice that covers most Ghostscript-generated
PostScript, so reclamation-on-restore is a correctness fix and a bound on
save/restore loops rather than a general memory improvement.

#### Allocating from Rust

Every entity carries three stamps: `save_level`, `global`, and
`created_after_save`. The last one is what `check_invalidrestore` reads
to decide whether a surviving reference points at a composite the
`restore` is about to release.

Operator code must therefore allocate through the VM-aware helpers in
`stet_ops::vm_ops`, which stamp all three from the live `Context`:

| helper | VM |
|---|---|
| `alloc_dict`, `alloc_array`, `alloc_array_from`, `alloc_string`, `alloc_string_empty` | follows the ambient `currentglobal` |
| `alloc_dict_in`, `alloc_array_in`, `alloc_array_from_in`, `alloc_string_in` | explicitly chosen |

Use the ambient form when the object is handed back to the program, and
the `_in` form when it goes straight into a container whose VM is already
fixed — allocating per `currentglobal` and then storing into a global
container is the PLRM 3.7.2 violation above, committed by the
interpreter rather than by the program.

The raw store methods `allocate_at_level_zero` /
`allocate_from_at_level_zero` hardcode all three stamps to zero. An
entity stamped that way claims to predate every outstanding `save` while
still sitting above that save's high-water mark, so `restore` releases it
and `check_invalidrestore` never sees it. Only `Context::new` may use
them, since during bootstrap the claim is true.
`scripts/check-level-zero-alloc.sh` enforces that, and is wired into CI
and `.git/hooks/pre-push`.

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
(including the Type 3 BuildChar/BuildGlyph capture window) all resolve to the
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

### pdfmark authoring buffer

The `pdfmark` operator (Adobe / GhostScript convention) feeds a
producer/consumer pipeline that's independent of the paint pipeline.
PostScript code issues `[ … /TYPETAG pdfmark` calls during
interpretation; the dispatcher in `stet-ops::pdfmark_ops` parses each
call into a [`stet_graphics::document_structure::StructuralRecord`] and
pushes it onto `Context::doc_structure`. The `PdfDevice` reads that
buffer at end-of-job (in `build_info_dict()` and the outline / annotation
/ names / forms / attachments writers) and merges every record into the
output PDF's catalog, info dictionary, page tree, and so on.
`Context::doc_structure` is the same `DocumentStructure` type
`stet-pdf-reader` populates when parsing a PDF, so PDF→PDF round-tripping
goes through the same writer codepath as PostScript → PDF — there is no
separate bridge. Non-PDF output devices simply never read the buffer,
which keeps structural data a true no-op for screen / viewer rendering.

The buffer is **document-global**: `save` / `restore` do not roll it
back, because pdfmark records are catalog-scoped facts about the PDF
being produced rather than VM-level state.

The pdfmark operators (`pdfmark`, `currentdistillerparams`,
`setdistillerparams`) are not registered by the default
`stet_ops::build_system_dict` — only the PDF rendering paths
(`stet::Interpreter::render_to_pdf` and the CLI's `run_pdf_mode`) call
the separate `register_pdf_authoring_ops`. PostScript prologues that
branch on `systemdict /pdfmark known` therefore see Distiller-equivalent
semantics on the PDF output path and pre-Distiller semantics on the
screen / viewer / WASM paths — which matters for prologues like
FrameMaker 5.0's that switch between CMYK→`setcmykcolor` (designed for
print and stet's CMYK→ICC→sRGB conversion) and direct RGB
→`setrgbcolor` (designed for Distiller). See `docs/PDFMARK-AUTHORING.md`
for the operator reference.

Type-tag dispatch is layered: the operator scans the operand stack
to a `[` mark and reads the type-tag, then the corresponding handler
(`parse_docinfo`, `parse_outline`, `parse_annotation`, …) walks the
payload as alternating `/Name value` pairs and emits one record. The
buffer's `current_page` counter — bumped by the `showpage` continuation
in `device_ops.rs` — lets page-scoped record handlers (`/ANN`,
forthcoming `/PAGE`) resolve to the page being assembled when the
producer omits an explicit `/Page` key.

Consumer side, the `PdfDevice` reads the buffer at end-of-job in
`build_pdf`. `/Outlines` is wired into `/Catalog` plus `/PageMode
/UseOutlines` (see `outline.rs`). Annotations need a different shape:
`/Annots` arrays attach to per-page dicts, which means each annotation
has to know its target page's indirect ref before the page dict is
written. The output path therefore pre-allocates one indirect-object
number per page up front, then both the annotation writer
(`annotations.rs::collect_per_page`) and the per-page builder
(`build_page`) consume that same `page_refs` slice — annotations
reference `page_refs[N-1]` for `/Page`/`/Dest` targets, and the page
dict's `/Annots` array references the annotation refs.

Named destinations (`/DEST` records) feed a single-leaf name tree in
`names.rs` referenced from `/Catalog /Names`. Page-box overrides
(`/PAGE` per-page, `/PAGES` document-wide) are layered into one
`EffectivePageOverride` per page by `compute_page_overrides`: per-page
records take precedence over document-wide defaults key-by-key, with
later records winning when the same key appears more than once at the
same scope. The result is a flat per-page table the page builder
consults when emitting `/CropBox` / `/BleedBox` / `/TrimBox` /
`/ArtBox` / `/Rotate`.

`/VIEWERPREFERENCES` records are similarly merged via
`collect_viewer_prefs` into one effective bag in `metadata.rs`. The
writer splits the bag in two: boolean entries (`/HideToolbar`,
`/FitWindow`, …) and name entries with allow-lists
(`/NonFullScreenPageMode`, `/Direction`) go into a single
`/ViewerPreferences` indirect object referenced from `/Catalog`, while
`/PageLayout` and `/PageMode` are validated against their respective
allow-lists and emitted as catalog-level entries proper. A producer-
supplied `/PageMode` wins over the `UseOutlines` default the writer
applies when `/OUT` records exist. `/Metadata` records emit one stream
object with `/Type /Metadata /Subtype /XML` referenced from
`/Catalog /Metadata`; the bytes are round-tripped verbatim and
written uncompressed (PDF spec requirement for grep-friendly
extraction).

File attachments (`/EMBED` records) and embedded JavaScript /
named-action passthrough are Phase 7 additions. `/EMBED` records
flow through `attachments.rs::build_embedded_files_leaf`, which emits
one flate-compressed `/EmbeddedFile` stream + one `/Filespec` dict
per record and groups them into a single-leaf `/EmbeddedFiles` name
tree. The names.rs writer splits in two: `build_dests_leaf` returns
the `/Dests` leaf ref, and `write_names_root` combines any
combination of `/Dests` + `/EmbeddedFiles` leaves into one
`/Catalog /Names` dict. JavaScript and named actions ride on
`OutlineAction::JavaScript(String)` / `OutlineAction::Named(String)`
variants — stet doesn't execute either, the bytes are round-tripped
verbatim. Page-level `/AA` (additional actions — `/O` open, `/C`
close) extends `/PAGE` and `/PAGES`: `compute_page_overrides`
layers per-page actions over document-wide defaults the same way it
layers page boxes. The shared `outline::encode_action` helper
emits the same on-the-wire shape for outline `/Action`, annotation
`/A`, and page `/AA` so the four action subtypes (URI, GoTo,
JavaScript, Named) stay in one place.

Form fields (`/Subtype /Widget` annotations and `/FORM` records) take
a different shape because the AcroForm needs the field tree to
predate any individual field object — `/Parent` and `/Kids` cross-
reference each other. `form_fields.rs::write_form` therefore owns
widget emission end-to-end: it splits widgets out of the standard
`/ANN` flow (the conventional annotation writer in `annotations.rs`
skips Widget records), pre-allocates one object number per widget,
groups widgets by canonical dotted `/T` to detect radio groups
(multiple widgets sharing a name), allocates additional refs for
every dotted-name prefix (implicit container parents), and emits
each object with the right merged-field-keys vs. radio-kid shape.
Single-widget leaves merge field keys (`/T`, `/FT`, `/V`, `/Ff`,
`/MaxLen`, …) into the annotation dict so the same object satisfies
both `/AcroForm /Fields` resolution and the page's `/Annots` array.
Radio kids omit field-level keys (those lift to a synthetic radio-
group parent). The shared `push_field_level_keys` helper is reused
between the leaf-merge path and the radio-parent path so encoding
stays consistent. `/AcroForm /Fields` lists exactly the root field
refs (top-level container parents and standalone single-widget root
fields); the writer also returns per-page widget refs that
`pdf_device.rs` merges into the existing `per_page_annots` arrays
before assembling page dicts. `/Catalog /AcroForm` is wired only
when at least one widget or a `/FORM` record exists.

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

The `stet` facade crate embeds all 55 files (4.6 MB) via `include_bytes!()`.
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
                     (stet-render needs it only for the `ps-device` feature)
     │
stet-ops             331 operator implementations (328 + 3 PDF-path)
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

**Output / rendering crates** may pull in `stet-core` for the `OutputDevice`
trait, but never pull in the interpreter (`stet-ops`, `stet-engine`):

| Crate | Internal deps | What it gives you |
|-------|---------------|-------------------|
| `stet-render` | stet-fonts, stet-graphics, stet-tiny-skia, and stet-core **only with the default `ps-device` feature** | `DisplayList` → RGBA (banded, viewport, ICC-aware) |
| `stet-pdf` | stet-fonts, stet-graphics, stet-core | `DisplayList` → PDF bytes |

`stet-render`'s dependency on `stet-core` exists solely so `SkiaDevice` can
implement `OutputDevice`, whose `finish_with_context` hands a device the live
interpreter `Context`. Rasterizing a `DisplayList` never needs it, so that
impl sits behind the `ps-device` feature and a consumer who only renders can
switch it off. `stet-pdf` genuinely needs the `Context` — it reads PostScript
font dictionaries through it to subset and embed fonts — so its dependency is
unconditional.

**Interpreter-only** crates that rarely make sense to depend on in
isolation: `stet-core` (PS VM types), `stet-ops` (operator
implementations), `stet-engine` (eval loop), `stet-viewer` (egui desktop
viewer), `stet-cli` (binary), `stet-wasm` (wasm-bindgen glue).

**Useful external combos:**

- **Pure PDF viewer / rasterizer:** `stet-pdf-reader` + `stet-render` —
  no PostScript VM involved. `stet-pdf-reader` takes `stet-render` with
  `default-features = false, features = ["parallel"]`, so `stet-core` is
  absent from the graph; depending on `stet-pdf-reader` alone gets you this
  automatically.
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
