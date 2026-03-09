# stet-pdf-reader — Architecture Plan

## Crate Identity

- **License**: Apache-2.0 OR MIT (same as stet-core, stet-render)
- **Dependencies**: stet-core (types, font parsers, ICC), stet-render (SkiaDevice, display list replay)
- **No dependency on**: stet-ops, stet-engine (PS interpreter not needed)

## Module Structure

```
crates/stet-pdf-reader/src/
├── lib.rs              # Public API: PdfDocument, render_page()
├── parser/
│   ├── mod.rs
│   ├── xref.rs         # Cross-reference table/stream parsing
│   ├── objects.rs      # PDF object model (bool, int, real, string, name, array, dict, stream)
│   ├── lexer.rs        # PDF tokenizer (simpler than PS — no procedures)
│   └── encrypt.rs      # RC4 / AES decryption (standard security handler)
├── content/
│   ├── mod.rs
│   ├── interpreter.rs  # Content stream → DisplayList (~60 operators)
│   └── operators.rs    # Operator dispatch table
├── resources/
│   ├── mod.rs
│   ├── color_space.rs  # DeviceRGB/CMYK/Gray, CalRGB/Gray, ICCBased, Lab, Indexed, Separation, DeviceN
│   ├── image.rs        # Inline/XObject images, decode filters, SMask
│   ├── pattern.rs      # Tiling (Type 1) and shading (Type 2) patterns
│   ├── shading.rs      # Types 1-7, reuse shading_ops sampling logic
│   ├── ext_gstate.rs   # ExtGState dict → graphics state updates
│   └── function.rs     # Type 0/2/3/4 PDF functions (eval without PS interpreter)
├── font/
│   ├── mod.rs
│   ├── type1.rs        # Type 1 fonts (reuse stet-core parser)
│   ├── truetype.rs     # TrueType/OpenType (reuse stet-core parser)
│   ├── cff.rs          # CFF/CIDFont Type 0 (reuse stet-core parser)
│   ├── type3.rs        # Type 3 fonts (content stream per glyph)
│   ├── encoding.rs     # Encoding/ToUnicode/CMap resolution
│   └── metrics.rs      # Widths, font descriptors, missing glyph fallback
├── transparency.rs     # Transparency groups, SMask, blend modes
└── page_tree.rs        # Page tree traversal, MediaBox/CropBox inheritance
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

### Phase A: Parser (Foundation)

- PDF lexer (tokenizer for PDF syntax)
- PDF object model (`PdfObj` enum with indirect references)
- Xref table and xref stream parsing
- Object and object stream resolution
- Page tree traversal with inherited attributes (MediaBox, CropBox, Rotate, Resources)
- Stream decompression (reuse existing filter decoders)
- Basic validation and error recovery

### Phase B: Graphics (Core Rendering)

- Content stream interpreter framework (operator dispatch)
- Graphics state stack (`q`/`Q`)
- Path construction and painting operators → DisplayList
- Clipping paths
- Color spaces: DeviceRGB, DeviceCMYK, DeviceGray, CalRGB, CalGray, ICCBased, Indexed, Separation, DeviceN, Lab
- Images (XObject and inline): decode, color convert, emit as DisplayElement::Image
- Patterns: tiling (Type 1) and shading (Type 2)
- Shadings: Types 1–7 (reuse existing sampling/mesh code)
- Form XObjects (recursive content stream interpretation)
- ExtGState: line width, dash, join, cap, overprint, rendering intent

### Phase C: Text (Text Display)

- Font resource resolution (font descriptor, widths, encoding)
- Type 1 font loading (reuse stet-core parser, extract glyphs → paths)
- TrueType font loading (reuse stet-core parser, cmap → glyph mapping)
- CFF/OpenType font loading (reuse stet-core parser)
- Text positioning operators (Tm, Td, TD, T*)
- String rendering (Tj, TJ) with character/word spacing
- ToUnicode CMap parsing (for text extraction, not rendering)
- Composite fonts (Type 0) with CIDFont descendants
- CMap-based CID decoding

### Phase D: Advanced Features (Completeness)

- Type 3 fonts (content stream per glyph — recursive interpreter call)
- Encryption (standard security handler, RC4/AES)
- ExtGState: blend mode, opacity (ca/CA), soft mask
- Annotations (at minimum: Link, Widget for form fields)
- Optional content (layers) — basic visibility toggling

### Phase E: Transparency (Differentiator)

- Transparency group rendering to offscreen buffers
- Isolated and knockout group semantics
- All 12 blend modes (pixel-level compositing)
- Soft mask (alpha and luminosity) application
- Nested transparency groups
- Color space conversion within groups
- Performance optimization (avoid offscreen buffer when group is trivial)

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
