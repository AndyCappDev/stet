# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0] — 2026-05-01

This release lands a substantial expansion of the `stet-pdf-reader`
structural API, the PDF imaging-extension operators (transparency,
soft masks, optional content), and the `pdfmark` PostScript-to-PDF
authoring bridge. Several public match-surface enums are now
`#[non_exhaustive]` to lock in additive evolution — the breaking
changes are deliberate and documented per-crate below.

### ⚠ Breaking changes

This is a **breaking release**. Cargo treats the `0.1 → 0.2` bump as
incompatible (per the SemVer rules for `0.x`), so existing users
pinned at `stet = "0.1"` won't be auto-upgraded.

The breaking surface is concentrated in two places:

1. **`#[non_exhaustive]` markers** were added to ~40 public
   match-surface enums across `stet-graphics`, `stet-core`, and
   `stet-pdf-reader`. Any downstream `match` over `DisplayElement`,
   `PsError`, `Destination`, `AnnotationKind`, the various pdfmark
   record enums, etc. now requires a `_ => { ... }` wildcard arm.
   See the "Changed — public API breaking changes" subsection below
   for the complete list.

2. **`stet-pdf` no longer emits PDF/X-3 OutputIntents.** PDF output
   is now plain PDF 1.7. `PdfDevice::set_output_profile()` is
   `#[deprecated]` as a no-op; existing call sites compile but stop
   producing the (previously broken) PDF/X-3 conformance label.

For a typical downstream renderer that pattern-matches on
`DisplayElement`, the migration is one wildcard arm per `match`
site:

```diff
 match element {
     DisplayElement::Fill { .. } => { /* … */ }
     DisplayElement::Stroke { .. } => { /* … */ }
     DisplayElement::Image { .. } => { /* … */ }
+    _ => { /* fall through; new variants in 0.2.x are additive */ }
 }
```

The `#[non_exhaustive]` ratchet is intentional: it makes future
variant additions non-breaking, so 0.2.x → 0.3.x will be smaller.

### Added — `stet-pdf-reader` structural API

A read-only structural-content API for PDF inspection and tooling.
Every accessor parses lazily on first call and caches its result.

- `metadata()` — `/Info` dict (title, author, dates, …) and the
  catalog's `/Metadata` XMP stream.
- `viewer_preferences()` — page layout, page mode, print preferences,
  and reading direction hints.
- `outline()` — bookmark tree as `OutlineItem`s with
  destination/action resolution.
- `destinations()`, `resolve_named_destination(name)` — named
  destination table merged from `/Catalog /Dests` (legacy) and the
  `/Names /Dests` name tree.
- `page_annotations(page)` — typed `Annotation` list with
  destination/action resolution.
- `form()`, `form_fields()` — AcroForm field tree (text, choice,
  button, signature) with widget cross-references.
- `page_boxes(page)` — MediaBox / CropBox / BleedBox / TrimBox /
  ArtBox.
- `embedded_files()`, `embedded_file_bytes(name)` — `/EmbeddedFiles`
  name-tree walker.
- `layers()`, `layer(ocg_id)`, `configurations()`,
  `default_configuration()`, `layer_tree()`, `layer_set_for(intent)`
  — Optional Content Group (OCG) metadata, hierarchy, render-intent
  rules, and a runtime `LayerSet` for visibility overrides.
- `parse_warnings()` — diagnostic sink for non-fatal parse issues
  (broken outlines, bad name trees, malformed `/VE` expressions, …).
- New `stet inspect <file.pdf>` CLI subcommand surfaces the structural
  API at the command line.

See `docs/PDF-READER-API.md` and `docs/PDF-LAYERS.md` for full
references.

### Added — PDF imaging extensions

Display-list-level support for the PDF transparency and optional-content
imaging models, layered on top of the PostScript interpreter.

- **Alpha and blend modes**: `setblendmode`, `setfillalpha`,
  `setstrokealpha`, `setalphaisshape`. All 16 PDF blend modes.
- **Transparency groups**: `begintransparencygroup` /
  `endtransparencygroup` with `Knockout`, `Isolated`, and group
  colour space (`DeviceGray` / `DeviceRGB` / `DeviceCMYK` / ICC).
- **Soft masks**: `begintransparencymaskgroup` /
  `endtransparencymaskgroup` with `Alpha` and `Luminosity` subtypes,
  transfer functions, and backdrop-colour handling.
- **Optional Content (OCG)**: `setocg` / `endocg` operators wrap
  display-list content in `OcgGroup` elements with
  `OcgVisibility::Single` / `Membership` / `Expression` predicates.
  `LayerSet` (in `stet-graphics`) is the consumer's per-render override
  map; `render_page_to_rgba_with_layers` honours it.
- **Filters**: `JBIG2Decode` and `JPXDecode` for embedded image
  streams.

See `docs/PDF-EXTENSIONS.md` for the full reference and
`docs/PDF-LAYERS.md` for the runtime layer-visibility model.

### Added — `pdfmark` PostScript-to-PDF authoring

`pdfmark` operator dispatch in `stet-ops` (gated behind
`register_pdf_authoring_ops` so it's only visible to systemdict on the
PDF output path) plus matching emitters in `stet-pdf`. Five phases of
authoring support:

- `/DOCINFO` — document info dictionary (title, author, subject,
  keywords, creator, producer, dates, trapped).
- `/OUT` — outline (bookmark) tree authoring with destination /
  action targets.
- `/ANN` — Link, Text, FreeText annotations.
- `/DEST`, `/PAGE`, `/PAGES` — named destinations and per-page-box
  overrides.
- `/VIEWERPREFERENCES`, `/Metadata` — viewer preferences and
  document-level XMP metadata.
- `/Widget` and `/FORM` — AcroForm widget annotations and field-tree
  emission.
- `/EMBED`, JavaScript / Named actions, page-level `/AA` triggers.

See `docs/PDFMARK-AUTHORING.md` for the full reference.

### Added — colour management

- **Hand-rolled colorimetric A2B1 CLUT sampler**
  (`stet-graphics::icc::perceptual`). moxcms 0.8's `create_transform`
  pipeline over-saturates CMYK→sRGB output relative to lcms2 / Acrobat
  / Ghostscript on midtone colours; this module bypasses it for v2
  `lut16Type` CMYK profiles and matches lcms2 RelCol output to ±1 RGB
  level on a 17⁴ sweep against ISO Coated v2 300% (ECI). Out-of-gamut
  colours clip to the sRGB boundary (matching lcms2 / GS) so pure
  process primaries remain saturated. BPC is calibrated against the
  sampler's own (1, 1, 1, 1) output so K-heavy CMYK lands at the
  correct darkness. Profiles whose tables are mAB / mft1 fall back
  to the moxcms-driven bake.
- Soft-mask CMYK-domain blend gate widened to accept Group-wrapped
  flat CMYK fills (GWG 16.11 "Gradient Feather"). The GWG 16.10
  outer-glow protection still rejects on the inner Fill's blend-mode
  check.

### Changed — public API breaking changes

These match-surface enums are now `#[non_exhaustive]` so adding
variants is non-breaking for any consumer that includes a `_ =>` arm.
Existing consumers must add wildcard arms (or update their match
expressions) to keep building.

- `stet-graphics`: `DisplayElement`, `ImageColorSpace`,
  `ShadingColorSpace`, `SpotColorSpace`, `LineCap`, `LineJoin`,
  `FillRule`.
- `stet-core`: `PsError`, `FilterKind`, `RleState`.
- `stet-core::pdfmark`: `PdfMarkRecord`, `AnnotationSubtype`,
  `AnnotationTarget`, `OutlineDestination`, `OutlineAction`,
  `GoToTarget`, `ViewSpec`, `FieldType`, `FieldValue`, `DocDate`,
  `TrappedState`, `TzSign`, `LinkHighlight`, `TextAnnotationIcon`,
  `PageOverrideScope`.
- `stet-pdf-reader`: `PdfError`, `Destination`, `ViewSpec`, `Action`,
  `AnnotationDate`, `AnnotationKind`, `AnnotationColor`,
  `AnnotationKindData`, `FieldKind`, `ButtonType`, `FieldValue`,
  `TrappedFlag`, `PageLayout`, `PageMode`, `ReadingDirection`,
  `PrintScaling`, `Duplex`, `AfRelationship`, `ParsePhase`,
  `LocationHint`, `Severity`, `RenderIntent`, `LayerIntent`,
  `UsageState`, `PageElementSubtype`, `LayerTreeNode`, `BaseState`,
  `ListMode`, `AutoStateEvent`.

Param **structs** (`FillParams`, `StrokeParams`, `ImageParams`, the
pdfmark record structs, `Annotation`, `FormField`, `Layer`, etc.) are
**not** marked `#[non_exhaustive]` — adding fields lands additively
and consumers should pattern-match with `..` for forward
compatibility.

A `scripts/check-non-exhaustive.sh` audit runs in the local pre-push
hook; new public enums in the listed files must either carry the
marker or be allow-listed with a one-line justification. See the
"Stable extension points" section of CLAUDE.md and the per-doc
"Stability" sections of `docs/DISPLAY-LIST.md`,
`docs/PDF-READER-API.md`, and `docs/PDFMARK-AUTHORING.md`.

### Changed — other

- **`stet-pdf`**: removed the PDF/X-3 OutputIntent emission. The writer
  was emitting soft-mask transparency (prohibited by PDF/X-3) while
  labelling output as `PDF/X-3:2003` — a conformance conflict any
  preflight tool would flag. PDF output is now plain PDF 1.7 with no
  PDF/X conformance claim. A correct PDF/X-4 implementation is
  planned.
- **`stet-pdf`**: `PdfDevice::set_output_profile()` is `#[deprecated]`
  as a no-op. Retained for forward API compatibility with the planned
  PDF/X-4 work.
- **`stet-cli`**: `--width` / `--height` flags for PDF input override
  the page's MediaBox at render time.

### Added — documentation

- `docs/PDF-READER-API.md` — full reference for the structural API.
- `docs/PDF-LAYERS.md` — full reference for the OCG / layer API.
- `docs/PDF-EXTENSIONS.md` — full reference for the imaging extension
  operators and the JBIG2 / JPX filters.
- `docs/PDFMARK-AUTHORING.md` — full reference for the pdfmark
  authoring bridge.
- New **Rendering Correctness** section in the root README covering
  seam-free rendering on adjacent clipped regions and full overprint
  simulation.

## [0.1.0] — 2026-04-18

Initial public release.

### PostScript interpreter

- Level 3 interpreter with ~320 operators covering stack, math, type,
  dict, array, string, control, file, graphics state, path construction,
  painting, clipping, colour, font, show, image, halftone/transfer,
  pattern/device, resource, and param categories.
- Arena + entity-indirection memory model with full save/restore (COW).
- Dual VM (local/global) with unified stores and `vm_alloc_mode`.
- Name interning via `NameTable`; dict version cache for O(1) name
  resolution on the hot path.
- Full Type 1, CFF/Type 2, Type 3, TrueType, and Type 42 (CID) font
  support with URW substitutions for the 35 standard PostScript fonts.
- Eexec, ASCIIHex, ASCII85, RLE, Flate, LZW, DCT, SubFile, and their
  encode counterparts as streaming filters.
- CIE-based colour spaces (A/ABC/DEF/DEFG) and ICC-based via moxcms.
- Smooth shading types 1–7 (function, axial, radial, triangle meshes,
  Coons/tensor patches) with native PS function evaluation.

### PDF reader (`stet-pdf-reader`)

- Self-contained PDF parser: xref (including xref streams), decryption
  (RC4/AES), object-stream decompression, page tree, resource
  resolution.
- Content-stream interpreter producing the same `DisplayList` type the
  PostScript interpreter produces — **no dependency on `stet-core`**.
- Transparency groups, soft masks (alpha & luminosity), tiling
  patterns, shadings 1–7.
- All standard stream filters including Flate (with PNG predictors),
  LZW, DCT (two backends), CCITT, JBIG2, JPEG 2000, ASCII85, ASCIIHex.
- Optional Content Groups (OCG) captured in the display list for future
  layer toggling.
- PDF OutputIntent profile honoured by default for PDF/X documents.
- CJK CMap loading (poppler-data / `STET_CMAP_DIR`).

### Rendering (`stet-render`)

- tiny-skia–based rasterizer (vendored as `stet-tiny-skia`) producing
  RGBA output.
- Banded rendering sized to L2 cache; rayon-parallel band processing.
- Clip fast path with rect detection, mask caching, and spare mask
  recycling.
- Viewport rendering: render any rectangular region of a display list
  at any zoom without re-interpreting the source.
- ICC-aware CMYK path with black-point compensation and per-pixel
  consistency checks for transparency-group blending.
- Overprint simulation (OPM 0/1), including strict OPM-1 "preserve
  zero components" semantics.
- Hairline and stroke-adjust handling for thin lines.

### PDF output (`stet-pdf`)

- Display list → PDF with embedded fonts (Type 1, TrueType, CFF),
  image compression, shadings, and transparency groups.
- Preserves native CMYK and spot colour spaces (Separation, DeviceN)
  without lossy RGB round-tripping.
- Pre-sampled transfer, halftone, and black-generation/UCR tables
  carried per paint element.
- Print-workflow quality output suitable for pre-press.

### Viewer & frontends

- `stet-viewer`: egui desktop viewer with pan/zoom, minimap,
  multi-page navigation, and drag-and-drop.
- On-demand viewport rendering: zoom/pan without re-interpretation.
- WASM frontend (`stet-wasm`, excluded from the main workspace):
  browser-side PDF viewer with viewport rendering and SIMD-enabled
  tiny-skia.

### Public library API (`stet` facade)

- `Interpreter::new()` / `Interpreter::builder()` for batteries-included
  PostScript rendering.
- `render()` → RGBA pages, `render_to_display_list()` → display lists,
  `render_to_pdf()` → PDF bytes, `exec()` → side-effects only.
- All 53 resources (fonts, encodings, CMaps, ICC profile) embedded in
  the binary via `include_bytes!`.
- Example programs: `render_ps`, `render_pdf`, `display_list`.

### Workspace

- 13 crates under Apache-2.0 OR MIT, plus two vendored tiny-skia forks
  (`stet-tiny-skia`, `stet-tiny-skia-path`) under BSD-3-Clause.
- `stet-pdf-reader` is intentionally independent of `stet-core` — it
  can be used as a standalone PDF parser/renderer without pulling in
  the PostScript VM.

[0.2.0]: https://github.com/AndyCappDev/stet/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/AndyCappDev/stet/releases/tag/v0.1.0
