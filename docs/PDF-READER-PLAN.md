# stet-pdf-reader — Architecture Plan

## Crate Identity

- **License**: Apache-2.0 OR MIT (same as stet-core, stet-render)
- **Dependencies**: stet-core (types, font parsers, ICC), stet-render (SkiaDevice, display list replay)
- **No dependency on**: stet-ops, stet-engine (PS interpreter not needed)

## Module Structure

```
crates/stet-pdf-reader/src/
├── lib.rs              # Public API: PdfDocument, render_page(), render_page_to_rgba()
├── error.rs            # PdfError enum
├── lexer.rs            # PDF tokenizer (simpler than PS — no procedures)
├── objects.rs          # PDF object model (PdfObj enum, PdfDict, indirect references)
├── xref.rs             # Cross-reference table parsing
├── resolver.rs         # Indirect reference resolution, stream decompression, decryption
├── page_tree.rs        # Page tree traversal, MediaBox/CropBox/Rotate inheritance
├── crypto.rs           # RC4 / AES-128 / AES-256 decryption (standard security handler)
├── filters.rs          # Stream decompression (Flate, LZW, ASCII85, ASCIIHex, RunLength, DCT)
├── content/
│   ├── mod.rs          # ContentInterpreter: content stream → DisplayList (~60 operators)
│   ├── color_space.rs  # All color spaces: Device*, Cal*, Lab, ICCBased, Indexed, Separation, DeviceN
│   ├── font.rs         # Font resolution: Type1, TrueType, CFF, Type0 (CID), Type3, substitution
│   ├── cmap.rs         # CMap parser for composite font CID decoding
│   └── graphics_state.rs # PdfGraphicsState (CTM, color, text state, blend mode, opacity)
└── resources/
    ├── mod.rs          # Resource dict resolution
    ├── function.rs     # PDF functions (Type 0 sampled, Type 2 exponential, Type 3 stitching, Type 4 calculator)
    ├── image.rs        # XObject/inline images: decode, color convert → DisplayElement::Image
    └── shading.rs      # All 7 shading types → DisplayList elements
```

## Reuse from Existing Crates

| Existing Code | Used For |
|---|---|
| `stet-core::type1_parser` | Type 1 font parsing + charstring interpretation |
| `stet-core::truetype` | TrueType glyf/hmtx/cmap parsing |
| `stet-core::cff_parser` + `type2_charstring` | CFF font parsing + Type 2 charstrings |
| `stet-core::icc` | ICC profile loading + color transforms |
| `stet-core::device` | DisplayList, DisplayElement, all Params types |
| `stet-core::path` | PsPath for glyph outlines |
| `stet-core::matrix` | Matrix transforms |
| `stet-render::skia_device` | SkiaDevice for rasterization (banded, clip-optimized) |
| `stet-render::png_sink` | PNG output |
| Filter decoders (flate, lzw, ascii85, etc.) | Stream decompression |

## What's New (PDF-specific)

### 1. PDF Object Model

Distinct from PS objects. PDF has indirect references (`5 0 R`), streams with dict metadata,
no executable arrays. A lightweight `PdfObj` enum, not `PsValue`.

### 2. Xref Parsing

Classic xref tables + xref streams (PDF 1.5+). Object streams. Linearized PDF support
(optional, Phase E).

### 3. Content Stream Interpreter

~60 operators mapped to DisplayList elements. No eval loop, no exec stack, no procedures —
just a flat operator stream.

| Category | Operators |
|---|---|
| Graphics state | `q Q cm w J j M d ri i gs` |
| Path construction | `m l c v y h re` |
| Path painting | `S s f f* B B* b b* n` |
| Clipping | `W W*` |
| Color | `CS cs SC SCN sc scn G g RG rg K k` |
| Text state | `BT ET Tf Ts Tc Tw TL Td TD Tm T* Tj TJ ' "` |
| Image/XObject | `BI ID EI Do` |
| Marked content | `BMC BDC EMC` |

### 4. PDF Function Evaluator

Type 4 calculator functions need a mini stack machine (not the full PS interpreter — just
~39 arithmetic/logic ops on a float stack). Types 0/2/3 are pure math (interpolation,
exponential, stitching).

### 5. Transparency

The big feature PDF has that PS doesn't. Transparency groups, soft masks, 12 blend modes.
This is where hayro is weak and where stet can differentiate.

- **Blend modes**: Normal, Multiply, Screen, Overlay, Darken, Lighten, ColorDodge, ColorBurn,
  HardLight, SoftLight, Difference, Exclusion (+ HSL modes)
- **Soft masks**: Alpha and luminosity masks via SMask in ExtGState
- **Transparency groups**: Isolated and knockout group semantics
- **Group compositing**: Render group to offscreen buffer, composite with blend mode + opacity

### 6. Encryption

Standard security handler (passwords → encryption keys → RC4/AES per-object decryption).
Support for PDF 1.4–2.0 encryption revisions.

## Public API

```rust
pub struct PdfDocument { /* parsed xref, page tree, resources */ }

impl PdfDocument {
    /// Parse a PDF from bytes (zero-copy where possible).
    pub fn from_bytes(data: &[u8]) -> Result<Self, PdfError>;

    /// Parse with password for encrypted PDFs.
    pub fn from_bytes_with_password(data: &[u8], password: &str) -> Result<Self, PdfError>;

    /// Number of pages.
    pub fn page_count(&self) -> usize;

    /// Page dimensions in points.
    pub fn page_size(&self, page: usize) -> (f64, f64);

    /// Render page to DisplayList (for viewport rendering, WASM).
    pub fn render_page(&self, page: usize) -> Result<DisplayList, PdfError>;

    /// Render page directly to RGBA buffer.
    pub fn render_page_to_rgba(&self, page: usize, dpi: f64) -> Result<Vec<u8>, PdfError>;
}
```

## Implementation Phases

### Phase A: Parser (Foundation) — COMPLETE

- PDF lexer (tokenizer for PDF syntax) ✓
- PDF object model (`PdfObj` enum with indirect references) ✓
- Xref table and xref stream parsing ✓
- Object and object stream resolution ✓
- Page tree traversal with inherited attributes (MediaBox, CropBox, Rotate, Resources) ✓
- Stream decompression (reuse existing filter decoders) ✓
- Basic validation and error recovery ✓

### Phase B: Graphics (Core Rendering) — COMPLETE

- Content stream interpreter framework (operator dispatch) ✓
- Graphics state stack (`q`/`Q`) with clip restore ✓
- Path construction and painting operators → DisplayList ✓
- Clipping paths ✓
- Color spaces: DeviceRGB, DeviceCMYK, DeviceGray, ICCBased, Indexed, Separation ✓
- Images (XObject and inline): decode, color convert, emit as DisplayElement::Image ✓
- Shadings: Types 2/3 (axial/radial) ✓
- Form XObjects (recursive content stream interpretation, depth limit 20) ✓
- ExtGState: line width, dash, join, cap, overprint, rendering intent, opacity ✓
- PDF Function evaluator (Types 0/2/3/4) ✓
- Page CTM: scale(dpi/72) + Y-flip + CropBox offset + rotation ✓

**Deferred to Phase D** (now complete):
- Tiling patterns, Shading Types 1/4-7, CalRGB/CalGray/Lab/DeviceN color spaces

### Phase C: Text (Text Display) — COMPLETE

- Font resource resolution (font descriptor, widths, encoding) ✓
- Type 1 font loading (reuse stet-core parser, PFB stripping) ✓
- TrueType font loading (reuse stet-core parser, cmap Format 0/4/6) ✓
- CFF/OpenType font loading (reuse stet-core parser) ✓
- Font routing by FontFile3/FontFile2/FontFile presence (not /Subtype) ✓
- Encoding resolution: WinAnsi, MacRoman, Standard, /Differences overlay ✓
- Text positioning operators (BT/ET, Tm, Td, TD, T*) ✓
- String rendering (Tj, TJ, ', ") with character/word spacing ✓
- Font cache per ContentInterpreter (keyed by resource name) ✓
- Rendering mode 0 (fill) only ✓

**Deferred to Phase D** (now complete):
- Font substitution, composite fonts (Type 0/CID), Type 3 fonts, text rendering modes 1-7
- ToUnicode CMap parsing (text extraction, not rendering) — still TODO

### Phase D: Advanced Features (Completeness) — COMPLETE

- **D1: Separation/DeviceN tint functions + CIE color spaces** ✓
  - Separation: tint function evaluation via PdfFunction, conversion through alternate space ✓
  - DeviceN: names + alt space + tint function (same pattern as Separation) ✓
  - CalGray: gamma + white point → CieAParams → from_cie_a() pipeline ✓
  - CalRGB: gamma + matrix + white point → CieAbcParams → from_cie_abc() pipeline ✓
  - Lab: L*a*b* → XYZ → sRGB (CIE standard formulas) ✓
  - Separation/DeviceN images: TintLookupTable for image decode pipeline ✓
- **D2: Font substitution** for non-embedded fonts ✓
  - Adobe→URW name mapping via FONT_SUBSTITUTIONS table ✓
  - Loads bundled .t1 fonts from resources/Font/ ✓
  - Preserves PDF /Widths for layout correctness ✓
- **D3: Text rendering modes 1-7** ✓
  - Mode 0: fill, Mode 1: stroke, Mode 2: fill+stroke, Mode 3: invisible ✓
  - Modes 4-7: same as 0-3 but accumulate glyph clip path, applied at ET ✓
- **D4: Shading types 1, 4-7** ✓
  - Type 1 (function-based): 256×256 RGBA rasterization → Image ✓
  - Types 4-5 (mesh): BitReader + stet_core::mesh_shading parsers → MeshShading ✓
  - Types 6-7 (patches): stet_core::mesh_shading parsers → PatchShading ✓
- **D5: Composite fonts** (Type 0 + CIDFont + CMap) ✓
  - CMap parser (codespacerange, cidchar, cidrange, bfchar, bfrange) ✓
  - CIDFont Type 0 (CFF) and Type 2 (TrueType) with CIDToGIDMap ✓
  - Multi-byte text decoding in show_text ✓
- **D6: Type 3 fonts** ✓
  - CharProcs content stream per glyph, recursive interpreter call ✓
  - d0/d1 operators ✓
- **D7: Tiling patterns** ✓
  - Tiling pattern content stream → sub-DisplayList → PatternFill ✓
  - Shading patterns (Type 2): embedded shading emission ✓
- **D8: Transparency (Part 1)** ✓
  - Blend mode parsing from BM in ExtGState ✓
  - Alpha/opacity (ca/CA) propagation to FillParams/StrokeParams ✓
  - tiny-skia Paint opacity application ✓
- **D9: Encryption** ✓
  - Standard security handler (empty password auto-open) ✓
  - RC4 (V=1,2), AES-128-CBC (V=4), AES-256-CBC (V=5) ✓
  - Per-object key derivation, string/stream decryption in resolver ✓

**Known limitations**:
- ICCBased: profile data is parsed but ignored — falls back to device space by component count
  (1→DeviceGray, 3→DeviceRGB, 4→DeviceCMYK). The PS interpreter has full ICC support via
  `stet-core::icc` + `moxcms`; wire it into the PDF reader's color pipeline in Phase E.
- CalGray/CalRGB: CIE parameters are parsed but rendering uses approximate identity conversion
  (no actual CIE→sRGB transform applied). The PS interpreter handles CIE properly via
  `CieAParams`/`CieAbcParams`.

**Not done (moved to Phase E)**:
- ICCBased color management (extract profile data, apply transforms via IccCache)
- CalGray/CalRGB accurate CIE rendering
- Transparency groups (isolated/knockout), soft masks (D8 Part 2)
- Annotations (Link, Widget)
- Optional content (layers) — basic visibility toggling
- Cross-reference streams (PDF 1.5+)

### Phase E: Transparency & Remaining Features

- **ICCBased color management**: Extract embedded ICC profile data from ICCBased color space
  dicts, register with `IccCache`, apply profile transforms for fill/stroke colors and images.
  Reuse `stet-core::icc` + `moxcms` (already working in the PS interpreter).
- **CalGray/CalRGB accurate rendering**: Apply CIE gamma/matrix/white point transforms instead
  of passing colors through as-is. Reuse `CieAParams`/`CieAbcParams` pipelines from stet-core.
- Transparency group rendering to offscreen buffers
- Isolated and knockout group semantics
- All 12 blend modes (pixel-level compositing in tiny-skia)
- Soft mask (alpha and luminosity) application
- Nested transparency groups
- Color space conversion within groups
- Performance optimization (avoid offscreen buffer when group is trivial)
- Cross-reference streams (PDF 1.5+)
- Annotations (Link, Widget appearance streams)
- Optional content (layers) — basic visibility toggling

## Licensing Split

```
Apache-2.0/MIT (library crates, embeddable):
  stet-core
  stet-render
  stet-pdf-reader
  stet-pdf

AGPL-3.0 (PS interpreter, CLI tools):
  stet-ops
  stet-engine
  stet-cli
  stet-viewer
```

This lets SaaS/commercial users embed the PDF renderer without AGPL obligations, while
the PS interpreter remains copyleft. The `stet-cli` binary can optionally include
`stet-pdf-reader` behind a cargo feature for a unified tool.

## Architectural Advantage vs hayro

hayro builds its own rendering pipeline (path rasterization, compositing). stet-pdf-reader
outputs `DisplayList` → feeds into the existing `SkiaDevice` which already has:

- Banded rendering with L2 cache optimization
- Optimized clip mask caching (rect fast path, cache-on-second-sight, spare mask recycling)
- Pipelined multi-page rendering via rayon
- WASM viewport rendering support
- ICC color management via moxcms

The content stream interpreter maps PDF operators to the same DisplayList elements the PS
interpreter already emits. This means all rendering optimizations apply automatically.

Phase E (transparency) is the main new rendering work — everything else is "parse PDF →
emit the same DisplayList elements the PS interpreter already produces."

## Key Design Decisions

1. **Zero-copy parsing where possible** — `PdfObj` borrows from input `&[u8]` for strings/streams
   during parsing, copies only when needed (decryption, decompression).

2. **Lazy object resolution** — Don't parse all objects upfront. Resolve indirect references
   on demand as pages are rendered.

3. **DisplayList as intermediate representation** — Same IR as the PS interpreter. One rendering
   pipeline serves both input formats.

4. **No PS interpreter dependency** — Type 4 calculator functions get their own mini evaluator
   (~100 lines). No need to pull in the full PS exec engine.

5. **Cargo feature for PS interpreter** — `stet-cli` can optionally depend on both `stet-pdf-reader`
   and `stet-engine` behind features. When both are enabled, the CLI auto-detects file type
   (%!PS vs %PDF) and dispatches accordingly.
