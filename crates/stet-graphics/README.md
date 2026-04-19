# stet-graphics

[![crates.io](https://img.shields.io/crates/v/stet-graphics.svg)](https://crates.io/crates/stet-graphics)
[![docs.rs](https://img.shields.io/docsrs/stet-graphics)](https://docs.rs/stet-graphics)

The graphics foundation for the stet PostScript and PDF rendering stack —
colours, the display list, ICC profile management, and the mesh / patch
shading parsers.

Only depends on [`stet-fonts`](https://crates.io/crates/stet-fonts), so it's
usable on its own if any of these subsystems is what you actually want:

- ICC colour management (CMYK↔sRGB, image bulk conversion, BPC) via
  [`moxcms`](https://crates.io/crates/moxcms).
- Parsers for PDF Type 4–7 shading data (Gouraud mesh, Coons / tensor
  patch).
- The `DisplayList` / `DisplayElement` types — both the stet PostScript
  interpreter and `stet-pdf-reader` emit into this format, so downstream
  consumers (custom output devices, PDF rewriters, diff tools) use these
  types to stay compatible.

If you just want to render PS or PDF, use the
[`stet`](https://crates.io/crates/stet) facade crate instead.

## What's inside

| Module | Purpose |
|--------|---------|
| `color` | `DeviceColor` (sRGB + lossless native CMYK), `LineCap`, `LineJoin`, `FillRule`, `DashPattern`, and `CIEBasedA/ABC/DEF/DEFG` parameter types |
| `device` | `FillParams`, `StrokeParams`, `ImageParams`, `ClipParams`, `AxialShadingParams`, `RadialShadingParams`, `MeshShadingParams`, `PatchShadingParams`, `PatternFillParams`, `TextParams`, `TransferState`, `HalftoneState`, `BgUcrState`, plus `PageSink` / `PageSinkFactory` traits |
| `display_list` | `DisplayList`, `DisplayElement`, `GroupParams`, `GroupColorSpace`, `SoftMaskParams`, `MaskRaster` — the flat painter-order intermediate representation every stet output device consumes |
| `icc` | `IccCache`, `BpcMode`, `ProfileHash`, `find_system_cmyk_profile_bytes()` — profile registration, SHA-256 deduplication, cached f64 and 8-bit transforms, black-point compensation |
| `mesh_shading` | Binary decoders for PDF shading types 4 (free-form mesh), 5 (lattice-form mesh), 6 (Coons patches), 7 (tensor patches) |

See the [Display List Reference](https://github.com/AndyCappDev/stet/blob/main/docs/DISPLAY-LIST.md)
for complete element and parameter documentation.

## Standalone use

```toml
[dependencies]
stet-graphics = "0.1"
```

### ICC colour management

```rust
use stet_graphics::icc::IccCache;

let profile = std::fs::read("FOGRA39.icc")?;
let mut cache = IccCache::new();
cache.load_cmyk_profile_bytes(&profile);

// CMYK → sRGB in [0, 1]
if let Some((r, g, b)) = cache.convert_cmyk(0.0, 0.81, 1.0, 0.0) {
    println!("{r:.3} {g:.3} {b:.3}");
}

// Bulk image conversion (8-bit) — fast path for rasterization.
// `src` holds CMYK bytes; `dst` receives 8-bit sRGB.
// cache.convert_image_8bit(&src_cmyk, &mut dst_rgb, ...);
# Ok::<(), Box<dyn std::error::Error>>(())
```

Black-point compensation is on by default; opt out with
`IccCache::new_with_options(IccCacheOptions { bpc_mode: BpcMode::Off, .. })`.

### Parse a PDF Type 6 (Coons patch) shading

```rust
use stet_graphics::mesh_shading::parse_type6_patches;

// `data` is the decoded binary stream from a PDF shading dictionary.
// The bits-per-* arguments come from the shading dict's
// /BitsPerCoordinate, /BitsPerComponent, and /BitsPerFlag entries; the
// `decode` slice holds the flattened /Decode array (x range, y range,
// then one range per colour component).
let patches = parse_type6_patches(
    &data,
    /* bits_per_coordinate = */ 16,
    /* bits_per_component  = */ 8,
    /* bits_per_flag       = */ 2,
    /* decode              = */ &[0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0],
    /* n_components        = */ 3,
);

for patch in &patches {
    // patch.points — 12 or 16 control points (Coons uses 12)
    // patch.colors — 4 corner `DeviceColor` values
}
# Ok::<(), ()>(())
```

### Consume a display list

The `DisplayList` type is shared between stet and `stet-pdf-reader`, so a
custom output format can iterate over elements without caring which
source produced them:

```rust
use stet_graphics::display_list::{DisplayList, DisplayElement};

fn summarize(list: &DisplayList) {
    let (mut fills, mut strokes, mut images) = (0, 0, 0);
    for elem in list.elements() {
        match elem {
            DisplayElement::Fill { .. }   => fills   += 1,
            DisplayElement::Stroke { .. } => strokes += 1,
            DisplayElement::Image { .. }  => images  += 1,
            _ => {}
        }
    }
    println!("{fills} fills, {strokes} strokes, {images} images");
}
```

## License

Apache-2.0 OR MIT
