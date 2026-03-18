# Phase B: Content Stream Interpreter and Graphics Rendering

## Overview

Phase B interprets PDF content streams into DisplayList elements that the existing SkiaDevice can render. The content stream interpreter maps ~60 PDF operators to the same DisplayElement variants the PS interpreter produces — zero new rendering code.

## Prerequisites

### Dependency Change

`stet-pdf-reader` must now depend on `stet-core` (for Matrix, PsPath, DeviceColor, DisplayList, DisplayElement, etc.) and `stet-render` (for SkiaDevice rendering).

**License note**: stet-core is currently AGPL-3.0. The PDF-READER-PLAN.md calls for it to become Apache-2.0/MIT. That license change is a separate task.

## New Module Structure

```
crates/stet-pdf-reader/src/
├── (existing Phase A files)
├── content/
│   ├── mod.rs              # ContentInterpreter, operand stack, token loop
│   ├── graphics_state.rs    # PdfGraphicsState, ColorSpaceRef, BlendMode
│   └── color_space.rs       # Color space resolution from PDF resources
├── resources/
│   ├── mod.rs               # Resource resolver helpers
│   ├── ext_gstate.rs        # ExtGState dict → graphics state mutations
│   ├── image.rs             # XObject images + inline images (BI/ID/EI)
│   ├── function.rs          # PDF Function Types 0/2/3/4 evaluator
│   ├── shading.rs           # Shading dict → DisplayElement (Types 1-7)
│   └── pattern.rs           # Tiling patterns (Type 1) + shading patterns (Type 2)
```

## Implementation Steps

### Step 1: PDF Graphics State (`content/graphics_state.rs`)

Self-contained graphics state (no VM references like the PS interpreter's).

```rust
pub struct PdfGraphicsState {
    pub ctm: Matrix,
    pub fill_color: DeviceColor,
    pub stroke_color: DeviceColor,
    pub line_width: f64,
    pub line_cap: LineCap,
    pub line_join: LineJoin,
    pub miter_limit: f64,
    pub dash_pattern: DashPattern,
    pub rendering_intent: u8,
    pub stroke_adjust: bool,
    pub overprint: bool,
    pub overprint_stroke: bool,
    pub flatness: f64,
    pub fill_color_space: ColorSpaceRef,
    pub stroke_color_space: ColorSpaceRef,
    pub pending_clip: Option<(PsPath, FillRule)>,
    pub fill_alpha: f64,       // ExtGState ca
    pub stroke_alpha: f64,     // ExtGState CA
    pub blend_mode: u8,        // 0=Normal, 1–15 per PDF spec (applied in E1)
    // Text state (record only in Phase B, render in Phase C):
    pub text_matrix: Matrix,
    pub text_line_matrix: Matrix,
    pub font_size: f64,
    pub char_spacing: f64,
    pub word_spacing: f64,
    pub text_leading: f64,
    pub text_rise: f64,
    pub text_rendering_mode: i32,
    pub text_font_name: Vec<u8>,
}

pub enum ColorSpaceRef {
    DeviceGray,
    DeviceRGB,
    DeviceCMYK,
    Named(Vec<u8>),
}
```

**Defaults** (PDF spec Table 52): CTM = identity (set per page), colors = black, line_width = 1.0, line_cap = Butt, line_join = Miter, miter_limit = 10.0, dash = solid, fill_alpha/stroke_alpha = 1.0.

### Step 2: Content Stream Interpreter Framework (`content/mod.rs`)

```rust
pub struct ContentInterpreter<'a> {
    resolver: &'a Resolver<'a>,
    resources: PdfDict,          // current resource scope (page or form)
    gstate_stack: Vec<PdfGraphicsState>,
    gstate: PdfGraphicsState,
    current_path: PsPath,
    current_point: Option<(f64, f64)>,
    subpath_start: Option<(f64, f64)>,
    operand_stack: Vec<Operand>,
    display_list: DisplayList,
    in_text: bool,
    depth: u32,                  // Form XObject recursion guard
}

pub enum Operand {
    Int(i64),
    Real(f64),
    Name(Vec<u8>),
    Str(Vec<u8>),
    Array(Vec<Operand>),
    Dict(Vec<(Vec<u8>, Operand)>),
    Bool(bool),
    Null,
}
```

Token loop: reuse existing `Lexer` from Phase A. Numbers/strings/names/arrays/dicts → operand stack. Keywords → operator dispatch.

### Step 3: Operator Dispatch Table

```rust
fn dispatch_operator(&mut self, op: &[u8]) -> Result<(), PdfError> {
    match op {
        // Graphics state
        b"q"  => self.op_gsave(),
        b"Q"  => self.op_grestore(),
        b"cm" => self.op_concat_matrix(),
        b"w"  => self.op_set_line_width(),
        b"J"  => self.op_set_line_cap(),
        b"j"  => self.op_set_line_join(),
        b"M"  => self.op_set_miter_limit(),
        b"d"  => self.op_set_dash(),
        b"ri" => self.op_set_rendering_intent(),
        b"i"  => self.op_set_flatness(),
        b"gs" => self.op_set_ext_gstate(),

        // Path construction
        b"m"  => self.op_moveto(),
        b"l"  => self.op_lineto(),
        b"c"  => self.op_curveto(),
        b"v"  => self.op_curveto_v(),
        b"y"  => self.op_curveto_y(),
        b"h"  => self.op_closepath(),
        b"re" => self.op_rectangle(),

        // Path painting
        b"S"  => self.op_stroke(),
        b"s"  => self.op_close_stroke(),
        b"f" | b"F" => self.op_fill(),
        b"f*" => self.op_eofill(),
        b"B"  => self.op_fill_stroke(),
        b"B*" => self.op_eofill_stroke(),
        b"b"  => self.op_close_fill_stroke(),
        b"b*" => self.op_close_eofill_stroke(),
        b"n"  => self.op_end_path(),

        // Clipping
        b"W"  => self.op_clip(),
        b"W*" => self.op_eoclip(),

        // Color
        b"CS" => self.op_set_stroke_colorspace(),
        b"cs" => self.op_set_fill_colorspace(),
        b"SC" | b"SCN" => self.op_set_stroke_color(),
        b"sc" | b"scn" => self.op_set_fill_color(),
        b"G"  => self.op_set_stroke_gray(),
        b"g"  => self.op_set_fill_gray(),
        b"RG" => self.op_set_stroke_rgb(),
        b"rg" => self.op_set_fill_rgb(),
        b"K"  => self.op_set_stroke_cmyk(),
        b"k"  => self.op_set_fill_cmyk(),

        // Text (stub — record state only, render in Phase C)
        b"BT" => self.op_begin_text(),
        b"ET" => self.op_end_text(),
        b"Tf" => self.op_set_font(),
        b"Tc" => self.op_set_char_spacing(),
        b"Tw" => self.op_set_word_spacing(),
        b"TL" => self.op_set_text_leading(),
        b"Tr" => self.op_set_text_rendering_mode(),
        b"Ts" => self.op_set_text_rise(),
        b"Td" => self.op_text_move(),
        b"TD" => self.op_text_move_set_leading(),
        b"Tm" => self.op_set_text_matrix(),
        b"T*" => self.op_text_next_line(),
        b"Tj" | b"TJ" | b"'" | b"\"" => {
            self.operand_stack.clear(); // Skip text rendering
            Ok(())
        }

        // XObject / Image
        b"Do" => self.op_do_xobject(),
        b"BI" => self.op_begin_inline_image(),

        // Shading
        b"sh" => self.op_paint_shading(),

        // Marked content (no-op)
        b"BMC" | b"BDC" | b"EMC" | b"MP" | b"DP" => {
            self.operand_stack.clear();
            Ok(())
        }

        // Compatibility (no-op)
        b"BX" | b"EX" => Ok(()),

        _ => { self.operand_stack.clear(); Ok(()) }
    }
}
```

### Step 4: Path Construction Operators

Paths stored in device space (CTM applied at construction time, identity CTM passed to DisplayElement — same convention as PS interpreter).

- `m x y` → MoveTo(ctm.transform(x,y))
- `l x y` → LineTo(ctm.transform(x,y))
- `c x1 y1 x2 y2 x3 y3` → CurveTo with all points transformed
- `v x2 y2 x3 y3` → CurveTo with current point as first control point
- `y x1 y1 x3 y3` → CurveTo with endpoint as second control point
- `h` → ClosePath, restore current_point to subpath_start
- `re x y w h` → MoveTo + 3 LineTo + ClosePath (rectangle shorthand)

### Step 5: Path Painting Operators

Each paint operator:
1. Takes ownership of `current_path` via `std::mem::take`.
2. Builds `FillParams` / `StrokeParams` from current graphics state.
3. Pushes `DisplayElement::Fill` and/or `DisplayElement::Stroke`.
4. Applies pending clip (if W/W* was called before this operator).
5. Clears `current_point`.

**FillParams construction**:
```rust
fn make_fill_params(&self, fill_rule: FillRule) -> FillParams {
    FillParams {
        color: self.gstate.fill_color.clone(),
        fill_rule,
        ctm: Matrix::identity(),
        is_text_glyph: false,
        overprint: self.gstate.overprint,
        spot_color: None,
        rendering_intent: self.gstate.rendering_intent,
        transfer: TransferState::default(),
        halftone: HalftoneState::default(),
        bg_ucr: BgUcrState::default(),
        alpha: self.gstate.fill_alpha,
        blend_mode: self.gstate.blend_mode,
    }
}
```

**StrokeParams construction**: line_width and dash pattern scaled by CTM scale factor (`sqrt(a² + b²)`), same as PS interpreter.

**Operator → DisplayElement mapping**:
| Operator | Action |
|----------|--------|
| `S` | Stroke |
| `s` | ClosePath + Stroke |
| `f` / `F` | Fill (NonZero winding) |
| `f*` | Fill (EvenOdd) |
| `B` | Fill + Stroke |
| `B*` | EoFill + Stroke |
| `b` | Close + Fill + Stroke |
| `b*` | Close + EoFill + Stroke |
| `n` | Discard path (clip-only) |

### Step 6: Clipping Operators

`W` / `W*` set `gstate.pending_clip` = clone of current path + fill rule. The clip is emitted at the next paint operator (after the painted element), per PDF spec. `n` (end path) is the typical paint operator used with clip-only paths.

### Step 7: Color Operators

**Simple device colors** (Phase B core):
- `G`/`g` → gray (1 component)
- `RG`/`rg` → RGB (3 components)
- `K`/`k` → CMYK (4 components)

**Color space operators** (`CS`/`cs` + `SC`/`SCN`/`sc`/`scn`):
- `CS`/`cs` set the current color space from Resources `/ColorSpace` dict
- `SC`/`SCN`/`sc`/`scn` set color components in the current space

### Step 8: Color Space Resolution (`content/color_space.rs`)

```rust
pub enum ResolvedColorSpace {
    DeviceGray,
    DeviceRGB,
    DeviceCMYK,
    CalGray { white_point: [f64; 3], gamma: f64 },
    CalRGB { white_point: [f64; 3], gamma: [f64; 3], matrix: Option<[f64; 9]> },
    ICCBased { n: u32, profile_data: Vec<u8> },
    Indexed { base: Box<ResolvedColorSpace>, hival: u32, lookup: Vec<u8> },
    Separation { name: Vec<u8>, alt: Box<ResolvedColorSpace>, tint_fn: PdfFunction },
    DeviceN { names: Vec<Vec<u8>>, alt: Box<ResolvedColorSpace>, tint_fn: PdfFunction },
    Lab { white_point: [f64; 3], range: [f64; 4] },
    Pattern,
}

pub fn resolve_color_space(
    cs_obj: &PdfObj,
    resources: &PdfDict,
    resolver: &Resolver,
) -> Result<ResolvedColorSpace, PdfError>;
```

For Phase B, focus on DeviceGray/RGB/CMYK and ICCBased. CalGray/CalRGB can use approximate conversion. Indexed/Separation/DeviceN use the PDF function evaluator.

### Step 9: ExtGState Application (`resources/ext_gstate.rs`)

The `gs` operator applies an ExtGState dict to the current graphics state:

```rust
pub fn apply_ext_gstate(
    gstate: &mut PdfGraphicsState,
    gs_dict: &PdfDict,
    resolver: &Resolver,
) -> Result<(), PdfError>;
```

Applied keys: `LW` (line width), `LC` (line cap), `LJ` (line join), `ML` (miter limit), `D` (dash), `RI` (rendering intent), `OP`/`op` (overprint), `OPM`, `FL` (flatness), `SA` (stroke adjust), `ca` (fill alpha), `CA` (stroke alpha), `BM` (blend mode — applied to Paint in E1), `SMask` (soft mask — store for Phase E3), `Font` (sets text font+size).

### Step 10: PDF Function Evaluator (`resources/function.rs`)

Critical for shadings, Separation/DeviceN tint transforms, and ICC alternates.

```rust
pub enum PdfFunction {
    Sampled { domain, range, size, bps, encode, decode, samples },
    Exponential { domain, c0, c1, n },
    Stitching { domain, functions, bounds, encode },
    Calculator { domain, range, tokens },
}

impl PdfFunction {
    pub fn from_obj(obj: &PdfObj, resolver: &Resolver) -> Result<Self, PdfError>;
    pub fn evaluate(&self, inputs: &[f64], outputs: &mut [f64]) -> Result<(), PdfError>;
}
```

- **Type 0 (sampled)**: Multilinear interpolation with encode/decode maps. Reuse logic from stet-core's `evaluate_ps_function` for FunctionType 0.
- **Type 2 (exponential)**: `result[i] = C0[i] + x^N * (C1[i] - C0[i])`.
- **Type 3 (stitching)**: Piecewise — find subdomain, evaluate sub-function.
- **Type 4 (calculator)**: Mini stack machine with ~39 operators (add, sub, mul, div, neg, abs, sqrt, sin, cos, atan, exp, ln, log, etc. + comparison + logic + stack ops + if/ifelse). ~100-150 lines.

### Step 11: Image Handling (`resources/image.rs`)

**XObject images** (via `Do`):
1. Read Width, Height, BitsPerComponent, ColorSpace from image dict.
2. Decompress stream data.
3. Apply Decode array.
4. Build image matrix: `[width 0 0 -height 0 height]` maps unit square → pixels.
5. Transform through CTM for device coords.
6. Emit `DisplayElement::Image`.

**Inline images** (`BI ... ID ... EI`):
1. `BI` reads key-value pairs (abbreviated: `/W`→Width, `/H`→Height, `/BPC`→BitsPerComponent, `/CS`→ColorSpace, `/F`→Filter).
2. `ID` marks start of binary data.
3. Scan for `EI` preceded by whitespace to find end.
4. Decode and emit same as XObject image.

### Step 12: Form XObjects

Recursive content stream interpretation:
1. Check recursion depth (limit 20).
2. Get form's content stream and Resources.
3. `q` (save state).
4. Apply form Matrix to CTM.
5. Clip to BBox.
6. Interpret form's content stream with form's Resources.
7. `Q` (restore state).

Resources stack: form's own Resources override page Resources during interpretation.

### Step 13: Shading Operators (`resources/shading.rs`)

The `sh` operator paints a shading directly. Shading patterns (via pattern fill) also use this.

| Type | Method | DisplayElement |
|------|--------|----------------|
| 1 (function-based) | Rasterize 256×256 via function eval | Image |
| 2 (axial) | Sample function → 64 ColorStops | AxialShading |
| 3 (radial) | Sample function → 64 ColorStops | RadialShading |
| 4/5 (triangle mesh) | Parse binary data → triangles | MeshShading |
| 6/7 (Coons/tensor) | Parse patches → subdivide → triangles | PatchShading |

The PDF function evaluator (Step 10) replaces the PS `exec_sync` calls the PS interpreter uses. The `mesh_shading.rs` BitReader in stet-core can be reused for Types 4-7 binary parsing.

### Step 14: Tiling Patterns (`resources/pattern.rs`)

1. Parse pattern dict: BBox, XStep, YStep, PaintType, Matrix.
2. Interpret tile content stream → sub-DisplayList.
3. Emit `DisplayElement::PatternFill` with tile DisplayList + pattern matrix.

PaintType 1 (colored): tile has its own colors. PaintType 2 (uncolored): tile uses the current fill color.

### Step 15: Page Setup CTM

PDF default coords: origin lower-left, Y-up. SkiaDevice expects device-space (origin top-left, Y-down). Initial CTM:

```rust
fn compute_page_ctm(page: &PageInfo, dpi: f64) -> Matrix {
    let [llx, lly, urx, ury] = page.crop_box;
    let h = (ury - lly).abs();
    let scale = dpi / 72.0;
    // Scale + Y-flip + CropBox offset
    let mut ctm = Matrix::new(scale, 0.0, 0.0, -scale, -llx * scale, ury * scale);
    // Apply page rotation if needed
    // ...
    ctm
}
```

### Step 16: Public API Additions

```rust
impl<'a> PdfDocument<'a> {
    /// Render page to DisplayList.
    pub fn render_page(&self, page: usize, dpi: f64) -> Result<DisplayList, PdfError>;

    /// Render page to RGBA pixel buffer.
    pub fn render_page_to_rgba(
        &self, page: usize, dpi: f64,
    ) -> Result<(Vec<u8>, u32, u32), PdfError>;
}
```

## Implementation Order

1. `content/graphics_state.rs` — PdfGraphicsState with defaults
2. `content/mod.rs` — Interpreter skeleton: operand stack, token loop, dispatch
3. Graphics state operators: q, Q, cm, w, J, j, M, d, ri, i
4. Path construction: m, l, c, v, y, h, re
5. Path painting: S, s, f, f*, B, B*, b, b*, n + pending clip
6. Clipping: W, W*
7. Simple color: G, g, RG, rg, K, k
8. `resources/ext_gstate.rs` — gs operator
9. `content/color_space.rs` — CS, cs, SC, SCN, sc, scn + color space resolution
10. `resources/function.rs` — PDF function evaluator (Types 0/2/3/4)
11. `resources/image.rs` — XObject images + inline images
12. Form XObjects — Do with /Subtype /Form
13. `resources/shading.rs` — sh operator, all 7 types
14. `resources/pattern.rs` — tiling patterns
15. Page CTM + public API: render_page(), render_page_to_rgba()
16. Tests

## Test Strategy

| Category | Approach |
|----------|----------|
| Path ops | Synthesized content streams, verify DisplayList elements |
| Color ops | Set gray/rgb/cmyk, verify DeviceColor in FillParams |
| ExtGState | Synthesized gs dict, verify state mutations |
| Images | Render pages with images from sample PDFs, pixel check |
| Form XObjects | Nested forms, verify recursion + resource scoping |
| Full pages | Render javaplatform.pdf pages, compare against GhostScript |
| Edge cases | Empty pages, deep nesting, unknown operators |

## Challenges

1. **Resource reference resolution**: Must deref through Resolver during Form XObject interpretation without borrow conflicts. Store resources as owned `PdfDict`.
2. **Form XObject resource stacking**: Form's Resources override page's. Need to swap/restore during recursive interpretation.
3. **Inline image `EI` boundary**: Binary data makes finding `EI` ambiguous. Scan for whitespace + `EI` + whitespace/EOF.
4. **CTM multiplication order**: PDF `cm` does CTM' = CTM_old × M_new (column-vector convention). Use `ctm.multiply(&m)`.
5. **mesh_shading.rs reuse**: BitReader and mesh parsers in stet-core may need pub visibility for PDF reader to reuse.

## Verification

```bash
cargo build -p stet-pdf-reader
cargo test -p stet-pdf-reader

# Render a page to PNG and visually inspect
cargo run -p stet-pdf-reader -- render samples/javaplatform.pdf 1 300

# Compare against GhostScript:
gs -dNOPAUSE -dBATCH -sDEVICE=png16m -r300 -sOutputFile=gs_page1.png samples/javaplatform.pdf
```
