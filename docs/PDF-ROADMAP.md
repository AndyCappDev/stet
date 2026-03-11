# stet: Pixel Comparison + PDF Rendering Roadmap

## Context

stet's PostScript rendering pipeline is mature (phases 1-6 complete, ~268 operators, display list rendering, banded output, clip optimization, all 7 shading types, CFF/TrueType/Type 1/Type 3 fonts). Before adding PDF input support, the PS renderer should be validated pixel-for-pixel against GhostScript. This catches rendering bugs now — when the only variable is the renderer — rather than later when a PDF parser bug could be confused with a rendering bug.

PDF rendering is the path to commercial viability. stet already has ~80% of the imaging model PDF needs. The gap is a PDF file parser, content stream interpreter, and the PDF 1.4 transparency compositing model.

This document is a reference roadmap, not an immediate work plan. The PS interpreter still has significant work remaining (Phase 6b sample verification, Phase 7 PDF output, remaining PostForge test suites).

---

## Part 1: Pixel Comparison Against GhostScript (Do Now)

### Goal
Automated pixel-diff testing of stet's PS renders against GhostScript output, catching rendering bugs before adding PDF complexity.

### Approach

1. **Reference image generation**: Render sample files with GS at fixed DPI
   ```bash
   gs -dBATCH -dNOPAUSE -sDEVICE=png16m -r300 -o ref_%03d.png sample.ps
   ```

2. **stet rendering**: Render same files at same DPI
   ```bash
   stet --dpi 300 sample.ps
   ```

3. **Pixel comparison**: Use ImageMagick `compare` for per-pixel diff
   ```bash
   magick compare -metric AE stet_page1.png ref_001.png diff.png
   # AE = Absolute Error (count of differing pixels)
   # Also useful: RMSE, PSNR for "close enough" thresholds
   ```

4. **Test script**: A shell script or Rust integration test that:
   - Iterates over `samples/*.ps`
   - Renders with both GS and stet at 300 DPI
   - Computes pixel diff metrics
   - Reports pass/fail per file with configurable tolerance
   - Stores reference images in a `tests/reference/` directory (generated, gitignored)

### What to compare
- Start with the files already known to render: tiger.ps, cf-route.ps, javaplatform.ps, hospital.eps, mandelbrot.ps, eazybbs.ps, policy.ps, etc.
- Expand to all PostForge samples as they start working
- Use Poppler's `pdftoppm` as a second oracle when GS and stet disagree

### Tolerance
- Anti-aliasing differences are expected (tiny-skia vs GS's Freetype/AA)
- Font hinting differences are expected
- Use RMSE threshold rather than exact pixel match
- Flag files that exceed threshold for manual review

---

## Part 2: PDF Rendering — Architecture

### How it fits into stet

```
PS input  → stet-engine (PS interpreter) → DisplayList → stet-render → pixels
PDF input → stet-pdf (PDF interpreter)   → DisplayList → stet-render → pixels
```

Both front-ends produce the same DisplayList. The entire rendering pipeline (tiny-skia, banded rendering, clip optimization, viewport rendering, WASM viewer) is shared. stet-pdf is a new crate in the existing workspace, not a separate project.

### What stet already has (reusable for PDF)
- Path rendering (fill, stroke, clip) — stet-render
- Color spaces (DeviceGray/RGB/CMYK, CIE-based, Indexed) — stet-core, stet-ops
- Fonts (Type 1, TrueType/Type 42, CFF/Type 2, CID, Type 3) — stet-core, stet-ops
- Images (all bit depths, decode arrays, masks) — stet-ops
- Filters (Flate, LZW, DCT, ASCII85, ASCIIHex, RunLength, SubFile) — stet-core
- Smooth shading (all 7 types) — stet-ops, stet-render
- Matrix transforms — stet-core
- Display list + banded rendering — stet-core, stet-render

### What PDF rendering needs (new work)

**New crate: stet-pdf**

1. **PDF file parser** (largest new component)
   - Cross-reference table parsing (standard + cross-ref streams)
   - Object resolution (direct + indirect references)
   - Stream decompression (reuse existing filter infrastructure)
   - Incremental update handling
   - Linearization awareness (optional, for streaming)
   - Encryption/decryption (RC4, AES — needed for real-world PDFs)
   - Damaged file recovery (scan for `obj` keywords)

2. **Page tree traversal**
   - Resource inheritance from parent pages
   - MediaBox/CropBox/TrimBox resolution
   - Page rotation

3. **Content stream interpreter**
   - PDF operators → DisplayList elements
   - PDF operators are a subset of PS (no loops, no conditionals, no procedures)
   - Simpler than PS interpretation — just a flat sequence of drawing commands
   - Key operator groups: path (m/l/c/v/y/h/re), paint (f/f*/S/s/B/B*/n), color (g/G/rg/RG/k/K/cs/CS/sc/SC/scn/SCN), text (BT/ET/Tf/Td/TD/Tm/T*/Tj/TJ/'/"), image (BI/ID/EI + Do for XObjects), state (q/Q/cm/w/J/j/M/d/ri/i/gs)

4. **Resource resolution**
   - Font resources (mapping PDF font dicts → stet's font infrastructure)
   - XObject resources (Form XObjects = nested content streams, Image XObjects)
   - ExtGState resources (graphics state parameter dictionaries)
   - ColorSpace resources
   - Pattern resources
   - Shading resources

5. **PDF transparency model** (hardest part — see Part 3)

6. **Font differences from PS**
   - ToUnicode CMaps (for text extraction, not rendering)
   - Subsetted fonts (subset prefix stripping)
   - Type 0 (CID) font handling differences
   - Predefined CJK CMaps
   - Font descriptor metrics

---

## Part 3: PDF Transparency

This is the single hardest piece and deserves its own section.

### What PDF transparency adds over PostScript
PostScript has no transparency model. PDF 1.4 (2001) added:
- **Blend modes**: 12 modes (Multiply, Screen, Overlay, Darken, Lighten, ColorDodge, ColorBurn, HardLight, SoftLight, Difference, Exclusion) + 4 HSL modes (Hue, Saturation, Color, Luminosity)
- **Alpha/opacity**: Per-object constant alpha (CA/ca in ExtGState)
- **Soft masks**: Luminosity or alpha masks from arbitrary drawing sequences
- **Transparency groups**: Isolated and knockout groups that composite as a unit
- **Group color space**: Groups can have their own blending color space

### Implementation approach

1. **Blend mode math** (~1 week)
   - Per-pixel arithmetic, formulas directly from PDF spec (section 11.3.5)
   - Implement as functions operating on RGBA pixel buffers
   - tiny-skia handles Porter-Duff alpha compositing; blend modes layer on top
   - Test each mode with synthetic inputs where expected output is hand-computable

2. **Constant alpha** (~days)
   - Pre-multiply alpha into paint operations
   - Already partially supported (tiny-skia Paint has opacity)

3. **Transparency groups** (~2-3 weeks)
   - Render group contents to a temporary Pixmap (offscreen buffer)
   - Composite result back to parent using blend mode + alpha + soft mask
   - Isolated groups: initialize buffer to transparent, not parent backdrop
   - Knockout groups: each element composites against group backdrop, not accumulated result
   - Nested groups: recursive — same pattern at each level
   - stet already has the Pixmap infrastructure for this (banded rendering uses temporary pixmaps)

4. **Soft masks** (~1 week)
   - Render mask source (a transparency group) to temporary pixmap
   - Extract luminosity or alpha channel as a grayscale mask
   - Apply mask during compositing (multiply with alpha)

5. **Validation**
   - Synthetic test PDFs: known inputs → hand-computable outputs
   - Render with GS (`-sDEVICE=pngalpha`) and Poppler as dual oracles
   - Ghent Workgroup / PDF Association conformance test files
   - Real-world PDFs from InDesign, Illustrator (heavy transparency users)

### Why tiny-skia is sufficient
- tiny-skia provides: Pixmap buffers, alpha compositing, path rasterization, masking
- tiny-skia does NOT provide: PDF blend modes, transparency groups
- But blend modes are just per-pixel math on RGBA buffers — implement directly
- Transparency groups are just "render to offscreen pixmap, composite back" — already proven pattern in stet's banded renderer
- No GPU acceleration needed for correctness; performance comes from stet's existing optimizations (banding, culling, clip caching)

### Time estimate with AI assistance
- Blend modes: 1 week (spec math → Rust is straightforward)
- Constant alpha: 2-3 days
- Transparency groups: 2-3 weeks (the architectural part, not the math)
- Soft masks: 1 week
- Edge cases and real-world validation: 2-4 weeks
- **Total: ~2-3 months** for a correct, well-tested implementation

---

## Part 4: Phased Implementation Order

### Phase A: Pixel comparison infrastructure (do now, alongside PS work)
- Build GS-comparison test script
- Run against all working sample files
- Fix rendering discrepancies found

### Phase B: PDF parser (after PS Phase 7 PDF output)
- PDF output work teaches the file format from the write side
- Parser is the inverse — reading what you've learned to write
- Start with simple PDFs (single page, no transparency, no encryption)
- Incrementally add: multi-page, encryption, cross-ref streams

### Phase C: Content stream interpreter
- Map PDF operators to existing stet drawing primitives
- Form XObject support (nested content streams)
- Font resource resolution
- Validate against simple PDFs rendered by GS

### Phase D: Transparency
- Blend modes — all 16 PDF modes wired to tiny-skia ✓
- Constant alpha ✓
- Transparency groups (isolated + non-isolated, offscreen rendering) ✓
- Soft masks (luminosity + alpha, ExtGState /SMask, backdrop color) ✓
- Pixel-accurate output matching GhostScript on VDP Tech transparency test suite ✓
- Validate against GS and Poppler with transparency-heavy PDFs

### Phase E: Real-world hardening
- Encrypted PDFs
- Damaged/malformed PDF recovery
- CJK font support (predefined CMaps)
- Conformance test suites
- Performance optimization

---

## Pixel Comparison Tools Reference

```bash
# Render with GhostScript
gs -dBATCH -dNOPAUSE -sDEVICE=png16m -r300 -o gs_%03d.png input.ps

# Render with stet
stet --dpi 300 input.ps

# Pixel diff (count of differing pixels)
magick compare -metric AE stet.png gs_001.png diff.png

# RMSE (root mean square error, 0 = identical)
magick compare -metric RMSE stet.png gs_001.png null: 2>&1

# Visual diff (highlights differences in red)
magick compare stet.png gs_001.png -highlight-color red diff.png

# Poppler as second oracle
pdftoppm -r 300 -png input.pdf poppler
```
