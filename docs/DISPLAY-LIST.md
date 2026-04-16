# Display List Reference

The display list is the central data structure in stet. Both the PostScript
interpreter and the PDF reader produce display lists, and every output
format — PNG, PDF, viewport render, or custom — consumes them.

A `DisplayList` is a flat `Vec<DisplayElement>` representing one page of
output. Elements are in painter's order (back to front). All coordinates
are in **device space** — paths have already been transformed through the
CTM at the DPI specified during interpretation.

## Element Types

### Fill

```rust
Fill { path: PsPath, params: FillParams }
```

A filled path. The path is in device coordinates (already transformed
through the CTM). `FillParams` carries:

| Field | Type | Description |
|-------|------|-------------|
| `color` | `DeviceColor` | Fill color (RGB + optional native CMYK) |
| `fill_rule` | `FillRule` | `NonZeroWinding` or `EvenOdd` |
| `ctm` | `Matrix` | CTM at paint time (identity for device-space paths) |
| `overprint` | `bool` | Overprint flag |
| `overprint_mode` | `i32` | OPM (0 or 1) |
| `painted_channels` | `u8` | CMYK channel bitmask for overprint |
| `is_device_cmyk` | `bool` | True when color space is DeviceCMYK/ICCBased(4) |
| `spot_color` | `Option<SpotColor>` | Separation/DeviceN color (if applicable) |
| `rendering_intent` | `u8` | 0=RelativeColorimetric, 1=Absolute, 2=Perceptual, 3=Saturation |
| `transfer` | `TransferState` | Pre-sampled transfer function tables |
| `halftone` | `HalftoneState` | Halftone screen parameters |
| `bg_ucr` | `BgUcrState` | Black generation / undercolor removal tables |
| `is_text_glyph` | `bool` | True when this fill is a glyph from a show operator |
| `alpha` | `f64` | Fill opacity (0.0–1.0) |
| `blend_mode` | `u8` | PDF blend mode |

### Stroke

```rust
Stroke { path: PsPath, params: StrokeParams }
```

A stroked path. `StrokeParams` carries everything `FillParams` has for
color/overprint/transfer, plus line style:

| Field | Type | Description |
|-------|------|-------------|
| `line_width` | `f64` | Stroke width in device units |
| `line_cap` | `LineCap` | `Butt`, `Round`, or `Square` |
| `line_join` | `LineJoin` | `Miter`, `Round`, or `Bevel` |
| `miter_limit` | `f64` | Miter join cutoff ratio |
| `dash_pattern` | `DashPattern` | Dash array + offset |
| `stroke_adjust` | `bool` | Snap thin strokes to pixel centers |

### Image

```rust
Image { sample_data: Vec<u8>, params: ImageParams }
```

A raster image. `sample_data` contains raw pixel samples in the **native
color space** — CMYK images are CMYK bytes, not pre-converted to RGB.

| Field | Type | Description |
|-------|------|-------------|
| `width` | `u32` | Image width in samples |
| `height` | `u32` | Image height in samples |
| `color_space` | `ImageColorSpace` | Native color space (see below) |
| `ctm` | `Matrix` | Image-to-device transform |
| `image_matrix` | `Matrix` | Sample-to-image transform |
| `interpolate` | `bool` | Bilinear interpolation hint |
| `mask_color` | `Option<Vec<u8>>` | Chroma key mask range |
| `alpha` | `f64` | Image opacity |
| `blend_mode` | `u8` | PDF blend mode |
| `overprint` | `bool` | Overprint flag |

#### Image Color Spaces

`ImageColorSpace` preserves the original color space from the PostScript
program or PDF:

| Variant | Description |
|---------|-------------|
| `DeviceGray` | 1 component per sample |
| `DeviceRGB` | 3 components per sample |
| `DeviceCMYK` | 4 components per sample |
| `ICCBased { n, profile_hash, profile_data }` | N components with embedded ICC profile bytes |
| `Indexed { base, hival, lookup }` | Palette-indexed color |
| `Separation { name, alt, tint_table }` | Named spot ink with tint transform |
| `DeviceN { names, alt, tint_table }` | Multi-ink with tint transform |
| `CIEBasedABC { params }` | CIE-based 3-component (CalRGB) |
| `CIEBasedA { params }` | CIE-based 1-component (CalGray) |
| `Lab { white_point, range }` | CIE L\*a\*b\* 3-component |
| `Mask { color, polarity }` | 1-bit stencil mask with paint color |
| `PreconvertedRGBA` | Already RGBA (from JPEG 2000 decoder) |

### Text

```rust
Text { params: TextParams }
```

A text run from a PostScript show operator. Used by the PDF output device
to emit actual PDF text operators (preserving searchability); ignored by
the rasterizer (which renders text via Fill elements with glyph outlines).

| Field | Type | Description |
|-------|------|-------------|
| `text` | `Vec<u8>` | Character bytes (or 2-byte CIDs for Type 0) |
| `start_x`, `start_y` | `f64` | Device-space position |
| `font_entity` | `u32` | Font dict entity ID |
| `font_name` | `Vec<u8>` | Font name bytes (e.g., `b"Times-Roman"`) |
| `font_type` | `i32` | 0 (composite), 1 (Type 1), 2 (CFF), 3 (Type 3), 42 (TrueType) |
| `font_size` | `f64` | Effective device-space font size |
| `color` | `DeviceColor` | Text color |
| `paint_type` | `i32` | 0 = filled, 2 = stroked |
| `spot_color` | `Option<SpotColor>` | Separation/DeviceN text color |

### Clip / InitClip

```rust
Clip { path: PsPath, params: ClipParams }
InitClip
```

`Clip` intersects the current clip region with a path. `InitClip` resets
the clip to the full page. These are control elements — they have no
visual output but affect all subsequent paint elements.

When `ClipParams.stroke_params` is `Some`, the clip path is a stroke
centerline rather than a fill path. The renderer expands it to a stroke
outline before rasterizing the clip mask. Used for shading pattern strokes.

### ErasePage

```rust
ErasePage
```

Fills the entire page with white. Typically emitted at the start of each
page by the `erasepage` operator.

### Shadings

```rust
AxialShading { params: AxialShadingParams }     // Type 2: linear gradient
RadialShading { params: RadialShadingParams }   // Type 3: radial gradient
MeshShading { params: MeshShadingParams }       // Types 4/5: triangle mesh
PatchShading { params: PatchShadingParams }     // Types 6/7: Coons/tensor patch
```

All shading types carry their native `ShadingColorSpace` (DeviceGray,
DeviceRGB, DeviceCMYK, or ICCBased with embedded profile data). Gradient
color stops are pre-sampled from PostScript functions. Mesh and patch data
includes per-vertex colors and coordinates in device space.

### PatternFill

```rust
PatternFill { params: PatternFillParams }
```

A path filled or stroked with a tiling pattern. Contains a pre-rendered
display list for a single tile, plus the tiling parameters (step size,
bounding box, pattern matrix). Supports both colored (PaintType 1) and
uncolored (PaintType 2) patterns.

When `stroke_params` is `Some`, the `path` is a user-space stroke centerline
and the renderer expands it to a fill outline using `PathStroker::stroke()`
with the full stroke parameters (width, cap, join, miter, dash). This is
used for pattern-stroked paths in PDF.

`overprint_mode` carries the PDF OPM value (0 or 1). When 1 and the tile
contains CMYK images, zero-CMYK pixels (0,0,0,0) are rendered with alpha=0
(transparent) instead of opaque white, implementing the "no ink = don't
paint" semantics required by the PDF overprint specification.

### Group (PDF only)

```rust
Group { elements: DisplayList, params: GroupParams }
```

A PDF transparency group. Children are rendered offscreen and composited
onto the parent with the specified blend mode and opacity. Supports
isolated and knockout semantics. Produced by the PDF reader; the
PostScript interpreter does not generate these.

### OcgGroup (PDF only)

```rust
OcgGroup { elements: DisplayList, ocg_id: u32, default_visible: bool }
```

A PDF Optional Content Group (layer). Children are rendered only when the
layer is visible. `ocg_id` is the PDF object number of the OCG (or OCMD)
dictionary. `default_visible` records whether the layer is ON in the
document's default configuration. Produced by the PDF reader for `/OC BDC`
marked content blocks and XObjects with `/OC` entries. The rasterizer
currently renders all children unconditionally; layer toggling will be
added in a future update.

### SoftMasked (PDF only)

```rust
SoftMasked { mask: DisplayList, content: DisplayList, params: SoftMaskParams }
```

Content masked by a soft mask form. The mask display list is rendered to
grayscale (luminosity or alpha), then multiplied with the content's alpha
channel. Produced by the PDF reader only.

## Path Representation

Paths in the display list use `PsPath`, a simple vector of `PathSegment`
values. All coordinates are in device space (already transformed through
the CTM at the reference DPI).

```rust
pub struct PsPath {
    pub segments: Vec<PathSegment>,
}

pub enum PathSegment {
    MoveTo(f64, f64),              // Start a new subpath
    LineTo(f64, f64),              // Straight line to point
    CurveTo {                      // Cubic Bezier curve
        x1: f64, y1: f64,         //   first control point
        x2: f64, y2: f64,         //   second control point
        x3: f64, y3: f64,         //   endpoint
    },
    ClosePath,                     // Close subpath (line back to last MoveTo)
}
```

A path is a sequence of subpaths. Each subpath starts with `MoveTo` and
consists of `LineTo` and `CurveTo` segments, optionally ending with
`ClosePath`. Quadratic curves (from TrueType fonts) are promoted to
cubics during construction.

Fill, Stroke, and Clip elements all carry a `PsPath`. The same path
type is used throughout — there is no distinction between fill paths and
stroke paths at the data level.

## Color Representation

`DeviceColor` is the universal color type in the display list:

```rust
pub struct DeviceColor {
    pub r: f64,           // sRGB red (or gray level)
    pub g: f64,           // sRGB green
    pub b: f64,           // sRGB blue
    pub native_cmyk: Option<(f64, f64, f64, f64)>,  // lossless CMYK roundtrip
}
```

Every color has an RGB representation (for rasterization). CMYK colors
additionally carry their native CMYK components in `native_cmyk` so the
PDF output device can emit `DeviceCMYK` without lossy RGB→CMYK conversion.

For Separation and DeviceN colors, the `spot_color` field on paint params
carries the ink name(s), tint value(s), and a pre-sampled tint transform
table. This lets the PDF device write Separation/DeviceN color spaces
while the rasterizer approximates them via the alternative color space.

## Print Production State

Each paint element (Fill, Stroke) carries the full print production state
at the time it was created:

- **Transfer functions** (`TransferState`): Pre-sampled 256-entry lookup
  tables for gray and/or per-channel (C/M/Y/K) transfer. Identity transfers
  are represented as `None`.

- **Halftone screens** (`HalftoneState`): Screen frequency, angle, and
  spot function for gray and/or per-channel halftoning.

- **Black generation / UCR** (`BgUcrState`): Pre-sampled tables for
  black generation and undercolor removal functions.

- **Overprint** (`overprint`, `overprint_mode`, `painted_channels`):
  Overprint flag, OPM mode, and a bitmask of which CMYK channels are
  painted. The rasterizer uses this for overprint simulation; the PDF
  device emits `/OP`/`/op`/`/OPM` ExtGState entries.

- **Rendering intent** (`rendering_intent`): Which ICC rendering intent
  applies (RelativeColorimetric, AbsoluteColorimetric, Perceptual,
  Saturation).

This data is carried per-element, not as a separate state stack, because
the display list is flat and may be rendered out of order (banded
rendering, viewport culling). Each element is self-contained.

## Coordinate System

All paths and coordinates in the display list are in **device space** at
the DPI specified during interpretation. The PostScript CTM (which maps
user space to device space) has already been applied.

- Origin is top-left (Y increases downward)
- Units are device pixels at the reference DPI
- The `ctm` field on paint params is typically identity for paths that
  have already been transformed, or carries a residual transform for
  images and shadings

For best fidelity, choose a reference DPI that matches your final output
resolution. See the [Viewer Guide](VIEWER-GUIDE.md#dpi-and-display-quality)
for details.

## Consuming Display Lists

### Iterate elements directly

```rust
for element in display_list.elements() {
    match element {
        DisplayElement::Fill { path, params } => { /* ... */ }
        DisplayElement::Stroke { path, params } => { /* ... */ }
        DisplayElement::Image { sample_data, params } => { /* ... */ }
        // ...
        _ => {}
    }
}
```

### Rasterize to RGBA

```rust
let rgba = stet_render::render_to_rgba(&list, width, height, dpi, None, false);
```

### Viewport rendering

```rust
let prepared = stet_render::prepare_display_list(&list);
let rgba = stet_render::render_region_prepared(
    &list, &prepared,
    vp_x, vp_y, vp_w, vp_h,
    pixel_w, pixel_h,
    dpi, None, None, false,
);
```

### Inspect structure

```rust
println!("{} elements", list.len());
for (i, elem) in list.elements().iter().enumerate() {
    match elem {
        DisplayElement::Fill { .. } => println!("[{}] Fill", i),
        DisplayElement::Stroke { .. } => println!("[{}] Stroke", i),
        DisplayElement::Image { params, .. } =>
            println!("[{}] Image {}x{} {:?}", i, params.width, params.height, params.color_space),
        DisplayElement::Text { params } =>
            println!("[{}] Text '{}' at ({:.0},{:.0})", i,
                String::from_utf8_lossy(&params.text), params.start_x, params.start_y),
        DisplayElement::Clip { .. } => println!("[{}] Clip", i),
        DisplayElement::InitClip => println!("[{}] InitClip", i),
        DisplayElement::ErasePage => println!("[{}] ErasePage", i),
        _ => println!("[{}] {:?}", i, std::mem::discriminant(elem)),
    }
}
```
