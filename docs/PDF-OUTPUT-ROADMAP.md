# PDF Output Roadmap — Print-Ready & Round-Trip Testable

## Context

stet's PDF output device (`stet-pdf`) produces valid PDF 1.7 files with correct paths,
text, images, gradients, and clipping. However, several PostScript features are
dropped or simplified during PDF emission. This matters for two reasons:

1. **Round-trip testing**: When stet adds PDF input (see `PDF-ROADMAP.md`), the test
   loop is PS → PDF → pixels vs PS → pixels. Any feature the PDF output drops will
   cause pixel diffs that are PDF output bugs, not PDF input bugs.

2. **Print production**: Commercial print workflows require Separation/DeviceN
   color spaces, overprint control, trim/bleed boxes, and PDF/X conformance.

This roadmap tracks what's missing and prioritizes by impact on round-trip fidelity.

---

## Current State

### Working

| Feature | Status | Notes |
|---------|--------|-------|
| DeviceGray/RGB/CMYK colors | ✓ | CMYK preserved via `native_cmyk` on DeviceColor |
| ICCBased images | ✓ | Profile bytes embedded as ICC stream |
| Indexed images | ✓ | Base space + lookup table |
| Imagemasks | ✓ | 1-bit stencil with fill color |
| SMask (alpha) | ✓ | PreconvertedRGBA → RGB + SMask |
| Font embedding | ✓ | Type 1 (subset), CFF/Type 2, TrueType (glyf subset) |
| Standard 14 fonts | ✓ | Not embedded, referenced by name |
| Text batching + kerning | ✓ | TJ arrays with computed kern values |
| Paths (fill/stroke/clip) | ✓ | All segment types, fill rules, stroke params |
| Axial/Radial shadings | ✓ | Type 2/3 with Extend flags, sampled function, native color spaces |
| Mesh/Patch shadings | ✓ | Types 4/6/7 with encoded vertex data, native color spaces |
| Native shading color spaces | ✓ | DeviceGray/RGB/CMYK, ICCBased, CalRGB/CalGray preserved |
| Flate compression | ✓ | All streams deflated |
| Multi-page output | ✓ | Pages tree with per-page resources |

### Not Working / Missing

| Feature | Current Behavior | Impact |
|---------|-----------------|--------|
| Separation/DeviceN colors | Pre-converted to RGB at op time | **HIGH** — round-trip + print |
| Separation/DeviceN images | ✓ Native samples in display list | Raster ✓, PDF falls back to alt space (1.3) |
| Color key mask (Type 4) | ✓ `/Mask` array emitted | — |
| Pattern fills | Gray placeholder (0.7) | **HIGH** — round-trip |
| CIE shadings (complex decode) | Falls back to DeviceRGB | **LOW** — needs ICC profile construction (3.4) |
| Overprint settings | ✓ ExtGState with OP/op/OPM | — |
| ExtGState dict | ✓ Deduplicated, emitted per page | — |
| Transfer functions | Stored in gstate, not emitted | **LOW** — press calibration |
| Halftone screens | Stored in gstate, not emitted | **LOW** — screening |
| Black generation / UCR | Stored in gstate, not emitted | **LOW** — CMYK separation |
| TrimBox / BleedBox | Only MediaBox | **MEDIUM** — print finishing |
| Document metadata | ✓ Info dict (Producer, Title, CreationDate) | — |
| PDF/X conformance | No OutputIntent | **LOW** — certification |
| ToUnicode CMap | ASCII 0x20–0x7E only | **LOW** — searchability |
| CID font CMap | Not embedded | **LOW** — CJK text extraction |

---

## Phase 1: Round-Trip Critical (Visible Pixel Diffs)

These features, when missing from PDF output, will cause pixel differences in the
PS → PDF → pixels round-trip test. Fix these before PDF input validation.

### 1.1 Color Key Masking in PDF XObjects ✅
- [x] Write `/Mask` array in image XObject dict from `color_key_mask` field
- [x] Handle both exact-match (ncomp values → expanded to range pairs) and range (2×ncomp values) forms
- [x] Added `num_components()` to `PdfColorSpace`
- [x] Identity-decode Type 4 images preserve native color space + mask_color for PDF
- [x] Non-identity-decode Type 4 images use PreconvertedRGBA fallback (mask applied at op time)
- **Files**: `crates/stet-pdf/src/pdf_device.rs`, `crates/stet-pdf/src/image_ops.rs`, `crates/stet-ops/src/image_ops.rs`

### 1.2 Separation/DeviceN in Display List ✅
- [x] `TintLookupTable` struct: pre-sampled tint transform (256 for 1D, 17-33 for N-D)
- [x] `ImageColorSpace::Separation` variant with name, alt space, `Arc<TintLookupTable>`
- [x] `ImageColorSpace::DeviceN` variant with names, alt space, `Arc<TintLookupTable>`
- [x] `capture_image_color_space()` samples tint transforms via `exec_sync` at capture time
- [x] `TintLookupTable::lookup_1d()` (linear interpolation) and `lookup_nd()` (multilinear)
- [x] Fixed ncomp bug: Separation images use 1 sample/pixel, DeviceN uses `num_colorants` (was incorrectly using `num_alt_components`)
- [x] Added `name: Vec<u8>` to `ColorSpace::Separation`, `names: Vec<Vec<u8>>` to `ColorSpace::DeviceN`
- [x] Colorant names extracted during `setcolorspace` parsing
- [x] DeviceN capped at 8 colorants for sampling; >8 falls back to pre-conversion
- **Files**: `crates/stet-core/src/device.rs`, `crates/stet-core/src/graphics_state.rs`,
  `crates/stet-ops/src/image_ops.rs`, `crates/stet-ops/src/color_ops.rs`

### 1.3 Separation/DeviceN in PDF Output
- [x] `PdfColorSpace::Separation` and `PdfColorSpace::DeviceN` variants (with tint table + names)
- [x] Raw native samples pass through to PDF (1 byte/pixel Separation, N bytes/pixel DeviceN)
- [ ] Emit `/Separation` color space array: `[/Separation /Name /AlternateSpace tintTransform]`
- [ ] Emit `/DeviceN` color space array with colorant names + tint transform
- [ ] Tint transform as Type 0 (sampled) PDF function — avoids PS procedure dependency
- [ ] Emit Separation/DeviceN for fill/stroke colors (not just images)
- [ ] Currently falls back to alt space name (e.g., DeviceCMYK) in `build_pdf_colorspace()`
- **Files**: `crates/stet-pdf/src/image_ops.rs`, `crates/stet-pdf/src/content_stream.rs`,
  `crates/stet-pdf/src/pdf_device.rs`
- **Effort**: Medium — data structures in place, need PDF function object emission

### 1.4 Separation/DeviceN in Raster Rendering ✅
- [x] `samples_to_rgba` in skia_device.rs handles Separation/DeviceN via `TintLookupTable`
- [x] `alt_comps_to_rgb()` helper converts alt-space f32 values (Gray/RGB/CMYK) to RGB bytes
- [x] Separation: 1D linear interpolation per pixel
- [x] DeviceN: N-D multilinear interpolation per pixel
- **Files**: `crates/stet-render/src/skia_device.rs`

### 1.5 Pattern Fills
- [ ] Emit tiling patterns as PDF Pattern XObjects (Type 1)
- [ ] Pattern has own content stream (from pattern's cached display list)
- [ ] Add `/Pattern` to page Resources dict
- [ ] Emit `cs /Pattern cs` + `scn /P0 scn` in content stream
- [ ] Render pattern fills in raster device (tile pattern display list across fill area)
- **Files**: `crates/stet-pdf/src/pdf_device.rs`, `crates/stet-pdf/src/content_stream.rs`,
  `crates/stet-render/src/skia_device.rs`, `crates/stet-core/src/device.rs`
- **Effort**: Large — pattern rendering is architecturally complex

### 1.6 Native Color Space Shadings ✅
- [x] `ShadingColorSpace` enum: DeviceGray/RGB/CMYK, ICCBased, CalRGB, CalGray
- [x] Raw component values carried through display list on ColorStop, ShadingVertex, ShadingPatch
- [x] `color_space: ShadingColorSpace` on all 4 shading param structs
- [x] WhitePoint stored in CieAbcParams/CieAParams for CalRGB/CalGray emission
- [x] Raw ICC profile bytes stored in IccCache for PDF embedding
- [x] `capture_shading_color_space()` maps PS ColorSpace → ShadingColorSpace at shfill time
- [x] `detect_gamma()` identifies power-curve decode tables for CalRGB/CalGray (complex → DeviceRGB fallback)
- [x] Separation/DeviceN shadings resolve to alt space (after tint conversion)
- [x] PDF emitter: `shading_color_space_to_pdf()` emits native CS dicts + N-component samples
- [x] Raster renderer: unchanged (uses DeviceColor.r/g/b)
- **Verified**: hospital.eps CMYK shadings → DeviceCMYK in PDF; 10-ch8.ps/16-ch14.ps DeviceN→RGB correct
- **Files**: `crates/stet-core/src/device.rs`, `crates/stet-core/src/graphics_state.rs`,
  `crates/stet-core/src/icc.rs`, `crates/stet-core/src/mesh_shading.rs`,
  `crates/stet-ops/src/shading_ops.rs`, `crates/stet-ops/src/color_ops.rs`,
  `crates/stet-pdf/src/shading_ops.rs`

---

## Phase 2: Print Production Quality

These improve PDF output for real print workflows. Not strictly needed for round-trip
pixel testing (since stet's raster renderer also ignores most of these), but important
for producing usable PDF files.

### 2.1 ExtGState Support ✅
- [x] `ExtGStateDict` struct in content_stream.rs with arbitrary key-value entries
- [x] Deduplicated ExtGState dicts — identical parameter combinations share one resource
- [x] `emit_overprint()` emits `/GSn gs` operator when overprint state changes
- [x] GState tracks current `overprint` to suppress redundant emissions
- [x] Build `/ExtGState` resource dict on page referencing per-page ExtGState objects
- [x] Q/q restore resets overprint tracking (re-emits `gs` as needed)
- **Files**: `crates/stet-pdf/src/content_stream.rs`, `crates/stet-pdf/src/pdf_device.rs`

### 2.2 Overprint Settings ✅
- [x] Added `overprint: bool` field to `FillParams` and `StrokeParams`
- [x] Captured `ctx.gstate.overprint` at all FillParams/StrokeParams construction sites
  (paint_ops.rs ~6 sites, show_ops.rs ~2 sites, halftone_ops.rs ~2 sites)
- [x] Emit `/OP true` (stroke) + `/op true` (fill) + `/OPM 1` (nonzero overprint mode) in ExtGState dict
- [x] Overprint-off emits `/OP false` + `/op false` (no OPM entry)
- **Files**: `crates/stet-core/src/device.rs`, `crates/stet-ops/src/paint_ops.rs`,
  `crates/stet-ops/src/show_ops.rs`, `crates/stet-ops/src/halftone_ops.rs`,
  `crates/stet-pdf/src/content_stream.rs`, `crates/stet-pdf/src/pdf_device.rs`

### 2.3 TrimBox / BleedBox ✅
- [x] `set_trim_box()` method on `OutputDevice` trait (default no-op) and `PdfDevice`
- [x] EPS `%%BoundingBox` automatically sets trim box on PDF device
- [x] Emit `/TrimBox` array in page dict alongside `/MediaBox`
- [ ] Accept trim/bleed dimensions from pagedevice or CLI — future
- [ ] BleedBox support — future
- **Files**: `crates/stet-core/src/device.rs`, `crates/stet-pdf/src/pdf_device.rs`, `crates/stet-cli/src/main.rs`

### 2.4 Document Metadata ✅
- [x] Emit Info dict with `/Producer (stet)`, `/CreationDate` (UTC), `/Title` (from filename)
- [x] Info dict referenced from trailer via `/Info` entry
- [ ] Optionally accept metadata from DSC comments (`%%Title`, `%%Creator`) — future
- **Files**: `crates/stet-pdf/src/pdf_device.rs`, `crates/stet-pdf/src/pdf_writer.rs`

---

## Phase 3: Compliance & Polish

### 3.1 PDF/X-3 OutputIntent
- [ ] Embed ICC output profile (sRGB or system CMYK) as OutputIntent stream
- [ ] Add `/OutputIntents` array to catalog
- [ ] Set `/GTS_PDFX_Version` in Info dict
- **Effort**: Medium

### 3.2 Full ToUnicode CMap
- [ ] Map glyph names → Unicode via Adobe Glyph List
- [ ] Emit proper `bfchar` / `bfrange` entries for all used glyphs
- [ ] Handle CID fonts with CID→Unicode mapping
- **Effort**: Medium

### 3.3 Color Rendering Intent
- [ ] Capture rendering intent from `setrenderingintent`
- [ ] Emit `/ri` operator in content stream
- **Effort**: Small

### 3.4 CIE → ICC Profile Construction
- [ ] Build ICC profile binary from CIEBasedABC/A params with complex decode tables
- [ ] Currently falls back to DeviceRGB when decode tables don't match simple gamma
- **Effort**: Large — ICC binary format construction

### 3.5 Type 1 Shading Native Color Rasterization
- [ ] Rasterize function-based shadings to N-component pixel data instead of RGB
- [ ] Currently pre-rasterized to 256×256 RGB image — visual quality loss is minimal
- **Effort**: Medium — requires N-component pixel buffer

---

## Phase 4: Do When Needed

These features are stored in the graphics state but don't affect rendering in stet
or most PDF viewers. They only matter for specific print customers or press
calibration workflows. Implement on demand rather than speculatively.

### 4.1 Transfer Functions
- [ ] Sample transfer function procedures to lookup tables at capture time
- [ ] Emit as Type 0 (sampled) PDF functions in ExtGState `/TR` or `/TR2`
- [ ] Requires ExtGState support (2.1)
- **Files**: `crates/stet-core/src/device.rs`, `crates/stet-pdf/src/pdf_device.rs`
- **Effort**: Medium

### 4.2 Halftone Screens
- [ ] Capture halftone parameters (frequency, angle, spot function)
- [ ] Emit `/HT` dict in ExtGState (Type 1 halftone with spot function)
- [ ] Spot function as Type 4 (PostScript calculator) PDF function
- [ ] Requires ExtGState support (2.1)
- **Files**: `crates/stet-core/src/device.rs`, `crates/stet-pdf/src/pdf_device.rs`
- **Effort**: Medium

### 4.3 Black Generation / UCR
- [ ] Sample BG/UCR procedures to lookup tables
- [ ] Emit as PDF functions in ExtGState `/BG2` and `/UCR2`
- **Effort**: Small (once ExtGState exists)

---

## Implementation Order

Work these sequentially, top to bottom. Quick wins first, then the big
Separation/DeviceN block, then patterns last (most architecturally complex).
Phase 4 items are deferred — implement only when a specific need arises.

```
 #   Item                                 Effort    Notes
───  ─────────────────────────────────────  ────────  ──────────────────────────────
 ✅  1.1  Color key mask emission          done
 ✅  2.4  Document metadata                done
 ✅  2.3  TrimBox / BleedBox               done
 ✅  1.6  Native color space shadings      done      Gray/RGB/CMYK/ICCBased/CalRGB/CalGray
 ✅  2.1  ExtGState framework              done      dedup'd dicts, gs operator, overprint tracking
 ✅  2.2  Overprint settings               done      OP/op/OPM in ExtGState, captured at all paint sites
 ✅  1.2  Separation/DeviceN display list  done      TintLookupTable, ncomp fix, colorant names
 ✅  1.4  Separation/DeviceN raster        done      lookup_1d/lookup_nd interpolation
 8.  1.3  Separation/DeviceN PDF output    medium    Type 0 sampled function emission remaining
 9.  1.5  Pattern fills                    large     most complex, do last in Phase 1
11.  3.1  PDF/X-3 OutputIntent             medium    when print compliance needed
12.  3.2  Full ToUnicode CMap              medium    when text extraction needed
13.  3.3  Color rendering intent           small     when needed

Phase 4 (on demand only):
 -   4.1  Transfer functions               medium    press calibration
 -   4.2  Halftone screens                 medium    offset litho screening
 -   4.3  Black generation / UCR           small     CMYK separation control
```

---

## Key Files Reference

| File | Role |
|------|------|
| `crates/stet-core/src/device.rs` | ImageColorSpace, DisplayElement, shading param structs |
| `crates/stet-core/src/display_list.rs` | DisplayList structure |
| `crates/stet-core/src/graphics_state.rs` | GraphicsState fields (overprint, transfer, halftone, pattern) |
| `crates/stet-ops/src/image_ops.rs` | Image capture, decode, color space conversion |
| `crates/stet-ops/src/shading_ops.rs` | shfill operator, color sampling |
| `crates/stet-ops/src/halftone_ops.rs` | Halftone/transfer/pattern operators |
| `crates/stet-ops/src/color_ops.rs` | Separation/DeviceN parsing, tint transforms |
| `crates/stet-render/src/skia_device.rs` | Raster rendering, samples_to_rgba |
| `crates/stet-pdf/src/pdf_device.rs` | PDF page structure, Resources dict, XObject emission |
| `crates/stet-pdf/src/content_stream.rs` | PDF operators, color emission |
| `crates/stet-pdf/src/image_ops.rs` | Image XObject creation |
| `crates/stet-pdf/src/shading_ops.rs` | PDF shading dict generation |
| `crates/stet-pdf/src/font_embedder.rs` | Font subsetting and embedding |

---

## Validation

### Round-trip pixel test
```bash
# Render PS directly to PNG
stet --device png --dpi 300 sample.ps        # → sample-0001.png

# Render PS to PDF, then PDF to PNG (once PDF input exists)
stet --device pdf sample.ps                  # → sample.pdf
stet --device png --dpi 300 sample.pdf       # → sample-0001.png

# Compare
magick compare -metric RMSE direct.png roundtrip.png null: 2>&1
```

### Print validation
```bash
# Preflight check (requires external tool)
pdfcpu validate sample.pdf

# Check color spaces
mutool info sample.pdf           # lists fonts, images, color spaces

# Visual proof
gs -dBATCH -dNOPAUSE -sDEVICE=png16m -r300 -o gs_%03d.png sample.pdf
```

### Test files for specific features
| Feature | Test file |
|---------|-----------|
| Separation/DeviceN | `samples/hospital.eps` (CMYK), DeviceN test files |
| Color key mask | `samples/image-qa.ps` (Type 4 images) |
| Patterns | Pattern test files from PostForge |
| Native shading CS | `samples/hospital.eps` (CMYK→DeviceCMYK), `samples/10-ch8.ps` (DeviceN→RGB), `samples/16-ch14.ps` (DeviceN→RGB) |
| Overprint | AGM EPS files with overprint flags |
| Transfer functions | Files with `settransfer` / `setcolortransfer` |
