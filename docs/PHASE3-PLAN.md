# xforge Phase 3: Graphics Foundation — Implementation Plan

## Context

Phase 2 is complete: ~111 operators, 161 tests, zero clippy warnings. The interpreter has VM persistence (save/restore with COW), file I/O, and error dispatch via errordict. Phase 3 adds graphics rendering — path construction, matrix transforms, color, painting, and clipping — using tiny-skia as the rasterizer behind a swappable trait.

**Done when**: `cargo run -- tiger.ps` produces a valid PNG of the PostScript tiger.

---

## Architecture: Key Decisions

### 1. Trait-Based Device Abstraction

A `RasterDevice` trait in xforge-core provides the abstraction boundary. tiny-skia is the default implementation in a new `xforge-render` crate. Operators never see tiny-skia — they call trait methods. This lets us swap in cairo or any other backend later.

### 2. Immediate-Mode Rendering (No Display List)

For Phase 3 (PNG only), painting operators invoke the device immediately. No display list intermediate representation. `fill` → `device.fill_path()`, `stroke` → `device.stroke_path()`, `showpage` → `device.show_page()`. A display list can be layered in Phase 7 when multiple output devices are needed.

### 3. Paths in User Space

Unlike PostForge (which transforms to device space at construction time), xforge stores paths in **user space**. The CTM is captured at paint time and passed to tiny-skia as its `Transform` parameter. This is cleaner because:
- Matches tiny-skia's API naturally (transform parameter exists for this purpose)
- `currentpoint` returns user-space coordinates directly — no inverse CTM needed
- `pathbbox` returns user-space bounds directly
- Anisotropic strokes work correctly (tiny-skia applies transform to path and stroke width)

### 4. Graphics State as Rust Struct

`GraphicsState` is a plain Rust struct (not a PostScript object), cloned for `gsave`/`grestore`. The gsave stack is `Vec<GraphicsState>` on Context — separate from PostScript `save`/`restore`.

### 5. New Crate: xforge-render

The device trait goes in xforge-core (so operators can reference it). The tiny-skia implementation goes in a new `xforge-render` crate. xforge-cli depends on xforge-render to create the device.

### 6. Page Setup

Hardcoded for Phase 3: 612×792 points (US Letter), 72 DPI. Default CTM: `[1, 0, 0, -1, 0, 792]` (Y-flip: PS bottom-left-up → device top-left-down).

---

## Implementation Steps (13 steps, always compiling)

### Step 1: Graphics State Module
**New file**: `crates/xforge-core/src/graphics_state.rs`

Core data structures:

- `Matrix` — affine transform `[a, b, c, d, tx, ty]` with `multiply`, `invert`, `transform_point`, `transform_delta`, `translate`, `scale`, `rotate` constructors
- `PathSegment` — enum: `MoveTo(f64, f64)`, `LineTo(f64, f64)`, `CurveTo(x1,y1,x2,y2,x3,y3)`, `ClosePath`
- `PsPath` — `{ segments: Vec<PathSegment> }` with `new`, `is_empty`, `clear`
- `LineCap` — Butt(0), Round(1), Square(2)
- `LineJoin` — Miter(0), Round(1), Bevel(2)
- `FillRule` — NonZeroWinding, EvenOdd
- `DeviceColor` — `{ r, g, b: f64 }` with `from_gray`, `from_rgb`, `from_cmyk`, `from_hsb`
- `ColorSpace` — DeviceGray, DeviceRGB, DeviceCMYK
- `DashPattern` — `{ array: Vec<f64>, offset: f64 }`
- `GraphicsState` — all of the above plus `current_point`, `line_width`, `miter_limit`, `flatness`, `stroke_adjust`, `clip_path`

**Modify**: `crates/xforge-core/src/context.rs` — add `gstate: GraphicsState`, `gstate_stack: Vec<GraphicsState>`, `device: Option<Box<dyn RasterDevice>>`, `page_width/height: u32`, `output_path: Option<String>`

~12 unit tests (matrix math, color conversion, defaults).

### Step 2: Device Trait
**New file**: `crates/xforge-core/src/device.rs`

```rust
pub trait RasterDevice {
    fn fill_path(&mut self, path: &PsPath, params: &FillParams);
    fn stroke_path(&mut self, path: &PsPath, params: &StrokeParams);
    fn clip_path(&mut self, path: &PsPath, params: &ClipParams);
    fn init_clip(&mut self);
    fn erase_page(&mut self);
    fn show_page(&mut self, output_path: &str) -> Result<(), String>;
    fn page_size(&self) -> (u32, u32);
}
```

Plus `FillParams`, `StrokeParams`, `ClipParams` structs bundling the parameters each method needs. No tests needed — trait definition only.

### Step 3: Matrix Operators (16)
**New file**: `crates/xforge-ops/src/matrix_ops.rs`

`matrix`, `identmatrix`, `currentmatrix`, `setmatrix`, `defaultmatrix`, `initmatrix`, `translate`, `scale`, `rotate`, `concat`, `concatmatrix`, `invertmatrix`, `transform`, `itransform`, `dtransform`, `idtransform`

Helpers: `read_matrix_from_array(ctx, entity, start)` → Matrix, `write_matrix_to_array(ctx, entity, start, m)`. `translate`/`scale`/`rotate` have two forms: if top of stack is array, return matrix in array; otherwise modify CTM.

~20 tests.

### Step 4: Path Construction Operators (13)
**New file**: `crates/xforge-ops/src/path_ops.rs`

`newpath`, `currentpoint`, `moveto`, `rmoveto`, `lineto`, `rlineto`, `curveto`, `rcurveto`, `closepath`, `arc`, `arcn`, `arcto`, `arct`

Arc-to-bezier conversion ported from PostForge's `_acuteArcToBezier` — approximate circular arcs with cubic beziers in segments ≤90°.

~18 tests.

### Step 5: Color Operators (12)
**New file**: `crates/xforge-ops/src/color_ops.rs`

`setgray`, `currentgray`, `setrgbcolor`, `currentrgbcolor`, `setcmykcolor`, `currentcmykcolor`, `sethsbcolor`, `currenthsbcolor`, `setcolorspace`, `currentcolorspace`, `setcolor`, `currentcolor`

All colors convert to RGB internally via `DeviceColor`. CMYK→RGB: `r=1-(c+k)`, clamped. HSB→RGB: standard algorithm. Back-conversion for `currentcmykcolor` etc.

~10 tests.

### Step 6: Graphics State Operators (18)
**New file**: `crates/xforge-ops/src/graphics_state_ops.rs`

`gsave`, `grestore`, `grestoreall`, `setlinewidth`, `currentlinewidth`, `setlinecap`, `currentlinecap`, `setlinejoin`, `currentlinejoin`, `setmiterlimit`, `currentmiterlimit`, `setdash`, `currentdash`, `setflat`, `currentflat`, `setstrokeadjust`, `currentstrokeadjust`, `initgraphics`

`gsave`: clone `gstate` onto `gstate_stack`. `grestore`: pop and replace, then reset device clip and re-apply restored clip path.

~14 tests.

### Step 7: Painting Operators (7)
**New file**: `crates/xforge-ops/src/paint_ops.rs`

`fill`, `eofill`, `stroke`, `rectfill`, `rectstroke`, `erasepage`, `showpage`

`fill`: close open subpaths, build `FillParams` from gstate (color, fill rule, CTM), call `device.fill_path()`, then `newpath`. `stroke`: build `StrokeParams` (color, line width, cap, join, miter, dash, CTM), call `device.stroke_path()`, then `newpath`. `showpage`: call `device.show_page()`, erase, reset graphics state.

~8 tests.

### Step 8: Clipping Operators (7)
**New file**: `crates/xforge-ops/src/clip_ops.rs`

`clip`, `eoclip`, `clippath`, `initclip`, `rectclip`, `clipsave`, `cliprestore`

`clip`: copy current path to gstate clip_path, call `device.clip_path()`. Path is NOT cleared (unlike fill/stroke). `clipsave`/`cliprestore` use a clip stack on GraphicsState.

~6 tests.

### Step 9: Path Query Operators (5)
**New file**: `crates/xforge-ops/src/path_query_ops.rs`

`pathbbox`, `flattenpath`, `reversepath`, `strokepath`, `pathforall`

`pathbbox`: min/max of all path points in user space (conservative control-point hull for curves). `flattenpath`: de Casteljau recursive subdivision of curves to line segments. `pathforall`: iterate segments, execute procs (uses e_stack loop pattern). `strokepath`/`reversepath`: simplified or deferred.

~8 tests.

### Step 10: tiny-skia Renderer
**New crate**: `crates/xforge-render/`

`SkiaDevice` implements `RasterDevice`:
- `Pixmap` for pixel buffer
- `Mask` for clipping
- `build_skia_path()`: convert `PsPath` → tiny-skia `Path` via `PathBuilder`
- `to_transform()`: convert `Matrix` → tiny-skia `Transform`
- `fill_path()`: `pixmap.fill_path(path, paint, fill_rule, transform, mask)`
- `stroke_path()`: `pixmap.stroke_path(path, paint, stroke, transform, mask)`
- `clip_path()`: `Mask::fill_path` or `Mask::intersect_path`
- `show_page()`: `pixmap.save_png(path)`

All internal math stays f64. Convert to f32 only at the tiny-skia boundary.

~6 tests.

### Step 11: Operator Registration
**Modify**: `crates/xforge-ops/src/lib.rs`

Register all ~78 new operators. Add 7 new `pub mod` declarations.

### Step 12: Wire into CLI
**Modify**: `crates/xforge-cli/src/main.rs` and `Cargo.toml`

Create `SkiaDevice(612, 792)`, set default CTM `[1, 0, 0, -1, 0, 792]`, wire device into Context. Output path derived from input filename (`.ps` → `.png`).

### Step 13: Integration Tests
Tests executing PostScript programs that verify rendering:
1. Simple rectangle fill — verify non-white pixels at expected location
2. Stroke with line width
3. gsave/grestore state preservation
4. Matrix transforms (translate, scale)
5. Clipping
6. **tiger.ps** — execute full file, verify valid PNG output

~6 integration tests.

---

## tiger.ps Requirements

tiger.ps defines an `Adobe_Illustrator_1.2d1` ProcSet with abbreviated operators:
- `c`/`v`/`y` → curveto variants, `l` → lineto, `m` → moveto (all using `transform`/`itransform` for grid snapping)
- `f`/`F`/`s`/`S`/`b`/`B` → fill/stroke variants using `gsave`/`grestore`
- `g`/`G`/`k`/`K` → gray/CMYK color via `setgray`/`setrgbcolor`
- `d`/`i`/`j`/`J`/`M`/`w` → dash/flat/join/cap/miter/width

Also uses: `clippath fill` (fill clip with background), `translate`, `scale`, `where` (checks for `setcmybcolor` — falls back to manual CMYK→RGB, which calls `setrgbcolor`).

**No images, no gradients, no text, no shading** — purely paths, fills, strokes, and colors.

---

## Files Summary

**New files (12):**
- `crates/xforge-core/src/graphics_state.rs` — Matrix, PsPath, GraphicsState, DeviceColor
- `crates/xforge-core/src/device.rs` — RasterDevice trait
- `crates/xforge-ops/src/matrix_ops.rs` — 16 operators
- `crates/xforge-ops/src/path_ops.rs` — 13 operators
- `crates/xforge-ops/src/color_ops.rs` — 12 operators
- `crates/xforge-ops/src/graphics_state_ops.rs` — 18 operators
- `crates/xforge-ops/src/paint_ops.rs` — 7 operators
- `crates/xforge-ops/src/clip_ops.rs` — 7 operators
- `crates/xforge-ops/src/path_query_ops.rs` — 5 operators
- `crates/xforge-render/Cargo.toml` + `src/lib.rs` — new crate
- `crates/xforge-render/src/skia_device.rs` — tiny-skia device

**Modified files (6):**
- `Cargo.toml` — add xforge-render to workspace
- `crates/xforge-core/src/lib.rs` — add `graphics_state`, `device` modules
- `crates/xforge-core/src/context.rs` — add gstate, gstate_stack, device, page dims
- `crates/xforge-ops/src/lib.rs` — register ~78 operators, 7 new modules
- `crates/xforge-cli/Cargo.toml` — add xforge-render dependency
- `crates/xforge-cli/src/main.rs` — create device, set CTM, wire output

---

## Operator Count

Phase 2: ~111 → Phase 3: ~189 (+78 new)

| Category | New | Operators |
|----------|-----|-----------|
| Matrix | 16 | matrix, identmatrix, currentmatrix, setmatrix, defaultmatrix, initmatrix, translate, scale, rotate, concat, concatmatrix, invertmatrix, transform, itransform, dtransform, idtransform |
| Path | 13 | newpath, currentpoint, moveto, rmoveto, lineto, rlineto, curveto, rcurveto, closepath, arc, arcn, arcto, arct |
| Color | 12 | setgray, currentgray, setrgbcolor, currentrgbcolor, setcmykcolor, currentcmykcolor, sethsbcolor, currenthsbcolor, setcolorspace, currentcolorspace, setcolor, currentcolor |
| Graphics State | 18 | gsave, grestore, grestoreall, setlinewidth, currentlinewidth, setlinecap, currentlinecap, setlinejoin, currentlinejoin, setmiterlimit, currentmiterlimit, setdash, currentdash, setflat, currentflat, setstrokeadjust, currentstrokeadjust, initgraphics |
| Painting | 7 | fill, eofill, stroke, rectfill, rectstroke, erasepage, showpage |
| Clipping | 7 | clip, eoclip, clippath, initclip, rectclip, clipsave, cliprestore |
| Path Query | 5 | pathbbox, flattenpath, reversepath, strokepath, pathforall |

---

## Test Target

~108 new tests → ~269 total

---

## Done-When Criteria

1. `cargo build` + `cargo test` + `cargo clippy` — zero warnings across all 5 crates
2. `cargo run -- tiger.ps` produces a valid PNG recognizable as the PostScript tiger
3. PostScript programs using gsave/grestore, translate/scale/rotate, fill/stroke, clip, setlinewidth/setdash, setgray/setrgbcolor execute correctly
4. Device trait enables future backend swaps without changing operator code

---

## Risk Mitigations

| Risk | Mitigation |
|------|------------|
| f32 precision at tiny-skia boundary | All internal math stays f64; convert to f32 only when calling tiny-skia |
| gsave/grestore clip restoration | On grestore, reset device clip to None, re-apply restored clip path |
| Arc-to-bezier accuracy | Port PostForge's proven `_acuteArcToBezier` (≤90° segments) |
| Missing `setcmybcolor` | tiger.ps checks with `where`, falls back to manual CMYK→RGB via `setrgbcolor` |
| Anisotropic strokes | Handled naturally by passing user-space paths + CTM to tiny-skia |
