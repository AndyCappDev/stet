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
| Tiling patterns | ✓ | Type 1 Pattern XObjects, colored + uncolored, affine transforms |

### Not Working / Missing

| Feature | Current Behavior | Impact |
|---------|-----------------|--------|
| Separation/DeviceN colors | ✓ Native spot colors in PDF | — |
| Separation/DeviceN images | ✓ Native samples + tint functions in PDF | — |
| Color key mask (Type 4) | ✓ `/Mask` array emitted | — |
| Pattern fills | ✓ Type 1 tiling patterns as PDF Pattern XObjects | — |
| CIE shadings (complex decode) | Falls back to DeviceRGB | **LOW** — needs ICC profile construction (3.4) |
| Overprint settings | ✓ ExtGState with OP/op/OPM | — |
| ExtGState dict | ✓ Deduplicated, emitted per page | — |
| Transfer functions | Stored in gstate, not emitted | **LOW** — press calibration |
| Halftone screens | Stored in gstate, not emitted | **LOW** — screening |
| Black generation / UCR | Stored in gstate, not emitted | **LOW** — CMYK separation |
| TrimBox / BleedBox | Only MediaBox | **MEDIUM** — print finishing |
| Document metadata | ✓ Info dict (Producer, Title, CreationDate, GTS_PDFXVersion) | — |
| PDF/X-3 OutputIntent | ✓ `--output-profile` embeds ICC + OutputIntent | — |
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

### 1.3 Separation/DeviceN in PDF Output ✅
- [x] `PdfColorSpace::Separation` and `PdfColorSpace::DeviceN` variants (with tint table + names)
- [x] Raw native samples pass through to PDF (1 byte/pixel Separation, N bytes/pixel DeviceN)
- [x] Emit `/Separation` color space array: `[/Separation /Name /AlternateSpace tintTransform]`
- [x] Emit `/DeviceN` color space array with colorant names + tint transform
- [x] Tint transform as Type 0 (sampled) PDF function — avoids PS procedure dependency
- [x] Emit Separation/DeviceN for fill/stroke colors (not just images)
- [x] `build_tint_function()` — converts TintLookupTable to PDF Type 0 function stream
- [x] `build_spot_colorspace()` — builds Separation/DeviceN arrays from SpotColorSpace
- [x] `build_pdf_colorspace()` emits proper Separation/DeviceN arrays (was falling back to alt space)
- [x] `SpotColor`/`SpotColorSpace` structs on FillParams/StrokeParams/TextParams
- [x] `tint_values` + `cached_tint_table` on GraphicsState, captured at setcolor/setcolorspace time
- [x] `capture_spot_color()` helper at all paint sites (paint_ops, show_ops, halftone_ops)
- [x] Content stream: `cs`/`CS` + `scn`/`SCN` operators for spot color fill/stroke
- [x] ColorSpace resource dict on page for Separation/DeviceN resources
- [x] Color space deduplication by colorant name(s) — same spot color shares one resource
- [x] Fixed Type 0 function sample ordering: PDF requires dim0-fastest, our table stores dim0-slowest
- **Files**: `crates/stet-core/src/device.rs`, `crates/stet-core/src/graphics_state.rs`,
  `crates/stet-ops/src/color_ops.rs`, `crates/stet-ops/src/paint_ops.rs`,
  `crates/stet-ops/src/show_ops.rs`, `crates/stet-ops/src/halftone_ops.rs`,
  `crates/stet-ops/src/image_ops.rs`, `crates/stet-pdf/src/pdf_device.rs`,
  `crates/stet-pdf/src/content_stream.rs`

### 1.4 Separation/DeviceN in Raster Rendering ✅
- [x] `samples_to_rgba` in skia_device.rs handles Separation/DeviceN via `TintLookupTable`
- [x] `alt_comps_to_rgb()` helper converts alt-space f32 values (Gray/RGB/CMYK) to RGB bytes
- [x] Separation: 1D linear interpolation per pixel
- [x] DeviceN: N-D multilinear interpolation per pixel
- **Files**: `crates/stet-render/src/skia_device.rs`

### 1.5 Pattern Fills ✅
- [x] Emit tiling patterns as PDF Pattern XObjects (Type 1) with `/PatternType 1`
- [x] Pattern has own content stream generated from tile's cached display list (`build_tile_content_stream`)
- [x] Add `/Pattern` to page Resources dict with per-pattern entries (`/P0`, `/P1`, ...)
- [x] Emit `/Pattern cs /Pn scn` for colored patterns (PaintType 1)
- [x] Emit `/CSPn cs <components> /Pn scn` for uncolored patterns (PaintType 2) with `[/Pattern /DeviceXxx]` color space
- [x] Pattern matrix composition: `initial_cm.concat(pattern_matrix)` maps pattern space → PDF page coordinates
- [x] Pattern dedup by `pattern_id` (unique per `makepattern` call) — same pattern reused shares one XObject
- [x] Tile resources: images, shadings, fonts, ExtGState, color spaces embedded in pattern's `/Resources` dict
- [x] BBox expanded by 0.5 units to eliminate hairline seam artifacts in PDF viewers
- [x] Raster renderer: full affine transform support (rotation, scale, shear) via 2D step vectors
- [x] Raster renderer: non-anti-aliased tile fills eliminate seam artifacts between adjacent tiles
- [x] `op_rectfill` routed through `push_fill_element` for pattern fill support
- [x] Text batch color state reset after `flush_text_batch` prevents stale color space tracking
- [x] Test file: `samples/pattern_test.ps` — 12 tests covering colored, crosshatch, circles, scaled, rotated, eofill, rectfill, multiple patterns, complex PaintProc, dedup, gsave/grestore, clipped
- **Files**: `crates/stet-pdf/src/pdf_device.rs`, `crates/stet-pdf/src/content_stream.rs`,
  `crates/stet-render/src/skia_device.rs`, `crates/stet-core/src/device.rs`,
  `crates/stet-ops/src/paint_ops.rs`

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

### 3.1 PDF/X-3 OutputIntent + Output Profile Override ✅
- [x] `--output-profile <path>` CLI flag specifies ICC output profile
- [x] User profile substitutes system CMYK for all DeviceCMYK color conversion (viewer, PNG, PDF)
- [x] ICC profile embedded as flate-compressed stream with `/N` component count
- [x] OutputIntent dict: `/Type /OutputIntent`, `/S /GTS_PDFX`, `/OutputConditionIdentifier`, `/Info`, `/DestOutputProfile`
- [x] `/OutputIntents` array added to catalog
- [x] `/GTS_PDFXVersion (PDF/X-3:2003)` added to Info dict
- [x] ICC header parsing: color space signature → N, `desc`/`mluc` tag → description string
- [x] `set_system_cmyk()` on IccCache for explicit profile override
- [x] `--no-icc` suppresses both ICC conversion and OutputIntent (wins if both specified)
- [x] No flag = auto-detect system CMYK profile (current behavior), no OutputIntent
- **Files**: `crates/stet-core/src/icc.rs`, `crates/stet-pdf/src/pdf_device.rs`, `crates/stet-cli/src/main.rs`

### 3.2 Full ToUnicode CMap ✅
- [x] Map glyph names → Unicode via Adobe Glyph List (encoding-aware Path A in font_embedder.rs)
- [x] Emit proper `bfchar` entries for all used glyphs
- [x] Handle CID fonts with CID→Unicode mapping (build_cid_tounicode in font_embedder.rs)
- [x] Fallback path: `build_tounicode_for_fallback()` extracts encoding from font dict when full embedding fails
- [x] AGL table expanded with Greek letters (α–ω, Α–Ω) and math symbols (∞, ≠, ≤, ≥, ≈, ∑, ∏, √, ∂, ∫)
- **Files**: `crates/stet-pdf/src/unicode_mapping.rs`, `crates/stet-pdf/src/font_embedder.rs`, `crates/stet-pdf/src/pdf_device.rs`

### 3.3 Color Rendering Intent ✅
- [x] `setrenderingintent` / `currentrenderingintent` operators
- [x] Rendering intent propagated through display list (FillParams, StrokeParams, TextParams)
- [x] Emit `/ri` operator in PDF content stream when intent changes
- **Files**: `graphics_state.rs`, `device.rs`, `halftone_ops.rs`, `paint_ops.rs`, `show_ops.rs`, `content_stream.rs`

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

### 4.1 Transfer Functions ✅
- [x] Sample transfer function procedures to lookup tables at capture time
- [x] Emit as Type 0 (sampled) PDF functions in ExtGState `/TR2`
- [x] Requires ExtGState support (2.1)
- **Files**: `crates/stet-core/src/device.rs`, `crates/stet-core/src/graphics_state.rs`, `crates/stet-ops/src/halftone_ops.rs`, `crates/stet-ops/src/paint_ops.rs`, `crates/stet-ops/src/show_ops.rs`, `crates/stet-pdf/src/content_stream.rs`, `crates/stet-pdf/src/pdf_device.rs`
- **Effort**: Medium

### 4.2 Halftone Screens ✅
- [x] Capture halftone parameters (frequency, angle, spot function)
- [x] Emit `/HT` dict in ExtGState (Type 1 halftone with spot function)
- [x] Spot function as Type 4 (PostScript calculator) PDF function
- [x] Type 0 sampled 2D fallback when Type 4 decompilation fails
- [x] Type 5 composite halftone for setcolorscreen (R/G/B/Default)
- [x] sethalftone Type 1 dict support
- **Files**: `crates/stet-core/src/device.rs`, `crates/stet-core/src/graphics_state.rs`, `crates/stet-ops/src/halftone_ops.rs`, `crates/stet-ops/src/paint_ops.rs`, `crates/stet-ops/src/show_ops.rs`, `crates/stet-pdf/src/content_stream.rs`, `crates/stet-pdf/src/pdf_device.rs`
- **Effort**: Medium

### 4.3 Black Generation / UCR ✅
- [x] Sample BG/UCR procedures to 256-entry lookup tables via exec_sync
- [x] Emit as PDF Type 0 functions in ExtGState `/BG2` and `/UCR2`
- [x] UCR uses signed range [-1,1] with proper encoding
- **Files**: `crates/stet-core/src/device.rs`, `crates/stet-core/src/graphics_state.rs`, `crates/stet-ops/src/halftone_ops.rs`, `crates/stet-ops/src/paint_ops.rs`, `crates/stet-ops/src/show_ops.rs`, `crates/stet-pdf/src/content_stream.rs`, `crates/stet-pdf/src/pdf_device.rs`
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
 ✅  1.3  Separation/DeviceN PDF output    done      Type 0 functions, cs/scn operators, ColorSpace resources
 ✅  1.5  Pattern fills                    done      Type 1 tiling patterns, colored + uncolored, full affine transform
 ✅  3.1  PDF/X-3 OutputIntent             done      --output-profile embeds ICC + OutputIntent + GTS_PDFXVersion
 ✅  3.2  Full ToUnicode CMap              done      encoding-aware fallback + Greek/math AGL entries
 ✅  3.3  Color rendering intent           done      setrenderingintent + /ri in content stream

Phase 4 (on demand only):
 ✅  4.1  Transfer functions               done      settransfer/setcolortransfer → /TR2 Type 0 functions
 ✅  4.2  Halftone screens                 done      setscreen/setcolorscreen/sethalftone → /HT Type 1/5
 ✅  4.3  Black generation / UCR           done      setblackgeneration/setundercolorremoval → /BG2 /UCR2
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
| Separation/DeviceN | `samples/hospital.eps` (CMYK images), `samples/10-ch8.ps` (DeviceN fill/stroke) |
| Color key mask | `samples/image-qa.ps` (Type 4 images) |
| Patterns | `samples/pattern_test.ps` (12 pattern tests), `samples/javaplatform.ps`, `samples/cf-route.ps` |
| Native shading CS | `samples/hospital.eps` (CMYK→DeviceCMYK), `samples/10-ch8.ps` (DeviceN→RGB), `samples/16-ch14.ps` (DeviceN→RGB) |
| Overprint | AGM EPS files with overprint flags |
| PDF/X-3 OutputIntent | `stet --device pdf --output-profile /usr/share/color/icc/colord/FOGRA39L_coated.icc samples/hospital.eps` |
| Transfer functions | Files with `settransfer` / `setcolortransfer` |
