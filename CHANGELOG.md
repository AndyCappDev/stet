# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/AndyCappDev/stet/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/AndyCappDev/stet/releases/tag/v0.1.0
