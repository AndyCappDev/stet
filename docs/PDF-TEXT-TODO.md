# PDF Text Support — Remaining Work

Tracks progress on PDF text support after the initial BT/ET/Tf/Tm/Tj implementation.

## Status Key
- [ ] Not started
- [~] In progress
- [x] Done

## Items (priority order)

### 1. Context access for PDF font data
- [x] `finish_with_context(&mut self, ctx: &Context)` on OutputDevice trait
- [x] CLI and WASM call sites use `device.take()` pattern to satisfy borrow checker
- [x] PdfDevice uses Context to read font dicts at PDF build time

### 2. Widths arrays
- [x] Emit `/Widths`, `/FirstChar`, `/LastChar` in PDF font dicts
- [x] Extract glyph widths from Type 1 charstrings (hsbw/sbw opcodes)
- [x] Extract widths for full FirstChar..LastChar range (not just used chars)
- [x] Extract widths from Type 2 (CFF) charstrings via `build_type2_font`

### 3. Font descriptor
- [x] Emit `/FontDescriptor` with `/FontBBox`, `/Ascent`, `/Descent`, `/StemV`, `/Flags`

### 4. Full ToUnicode mapping
- [x] Encoding array -> glyph name -> AGL Unicode mapping via `unicode_mapping.rs`
- [x] `build_tounicode_map` in `font_embedder.rs` wired up

### 5. Encoding / Differences
- [x] Build `/Encoding` dict with `/Differences` array from PS font's actual encoding
- Fixes wrong glyphs (e.g., bullet showing as ¥) for fonts with custom encodings

### 6. Per-character text positioning
- [x] Plain `show`: one Text element for entire string (Widths handle spacing)
- [x] Adjusted shows (awidthshow/widthshow/ashow/xshow/yshow/xyshow): one Text per character with exact device-space position from `ctm.transform_point(cur_x, cur_y)`
- [ ] TJ array batching: merge consecutive same-font per-char Text elements into TJ arrays with kern values (optimization, not correctness)

### 7. Non-uniform text scaling
- [x] Encode CTM scaling/rotation into Tm matrix (was hardcoded `1 0 0 -1 tx ty`)
- [x] Always force Y-flip (d < 0) regardless of PS CTM sign convention (dvips uses d > 0)
- Handles narrow/stretched/rotated text (e.g., clipping.ps "Narrow Text")

### 8. Type 1 font embedding
- [x] Reconstruct Type 1 font binary (cleartext + eexec encrypted private dict + charstrings)
- [x] Subset to used glyphs + .notdef + seac dependencies
- [x] Emit as `/FontFile` stream in font descriptor with `/Length1`, `/Length2`, `/Length3`
- [x] Charstring encryption for re-encrypted subrs
- [x] Private dict hint values (BlueValues, StdHW, etc.) preserved in embedded font
- [x] PFB format wrapping (segment markers `\x80\x01`/`\x02`/`\x03`)
- [x] Always use standard FontMatrix `[0.001 0 0 0.001 0 0]` (font_entity may be scaled copy)
- [x] Standard 14 fonts skip embedding

### 9. CFF font embedding
- [x] Store raw CFF binary during parsing (`cff_ops.rs` / `system_font_loader.rs`) as `_CFFData` key
- [x] Embed as `/FontFile3` with `/Subtype /Type1C`
- [x] Extract widths from Type 2 charstrings (done in item 2)
- Subsetting deferred (embeds full CFF initially)

### 10. CID/TrueType font embedding
- [x] Extract TrueType data from sfnts array (reuses `truetype::concatenate_sfnts`)
- [x] Embed as `/FontFile2` with `/Length1`
- [x] Build `/W` array from hmtx table (scaled by 1000/unitsPerEm)
- [x] Identity-H CMap encoding
- [x] ToUnicode CMap for CID -> Unicode (2-byte codespace, cmap table reverse mapping)
- [x] CIDToGIDMap stream (cmap format 4 + format 12 parsing)
- [x] Type0 → CIDFontType2 → FontDescriptor → FontFile2 hierarchy
- [x] Implemented in `font_embedder.rs` (no separate cid_embedder.rs needed)

### 11. Standard 14 custom encoding check
- [ ] Detect Standard 14 fonts with custom Encoding arrays (must be embedded, not just referenced)
- Low priority edge case
