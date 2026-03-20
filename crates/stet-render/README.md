# stet-render

Rendering backend for the stet PostScript interpreter, built on
[`stet-tiny-skia`](https://crates.io/crates/stet-tiny-skia) (a modified
fork of [tiny-skia](https://github.com/RazrFalcon/tiny-skia) with
stet-specific optimizations).

This is a low-level crate. Most users should use the [`stet`](https://crates.io/crates/stet)
facade crate instead.

## Contents

- **`SkiaDevice`** — `OutputDevice` implementation using tiny-skia for rasterization
- **`render_to_rgba()`** — Render a display list to RGBA pixels (simplest API)
- **Viewport rendering** — Render arbitrary rectangular regions at any zoom level:
  - `prepare_display_list()` → `PreparedDisplayList` (pre-computed bounding boxes and epochs)
  - `render_region_prepared()` — Single-threaded viewport render
  - `render_region_prepared_parallel()` — Multi-threaded banded render
  - `render_region_prepared_parallel_cancellable()` — Cancellable parallel render
- **`ImageCache`** — Pre-converted RGBA image cache for repeated viewport renders
- **`build_icc_cache_for_list()`** — Extract ICC profiles from display list elements
- **`PngSinkFactory`** — `PageSinkFactory` for PNG file output

## Features

| Feature | Default | Description |
|---------|---------|------------|
| `parallel` | yes | Multi-threaded banded rendering via rayon |

## Usage

```rust
use stet_render::{render_to_rgba, prepare_display_list, render_region_prepared};

// Simple: display list → RGBA
let rgba = render_to_rgba(&display_list, width, height, dpi, None, false);

// Viewport: prepare once, render many regions
let prepared = prepare_display_list(&display_list);
let rgba = render_region_prepared(
    &display_list, &prepared,
    vp_x, vp_y, vp_w, vp_h,
    pixel_w, pixel_h,
    dpi, None, None, false,
);
```

## License

Apache-2.0 OR MIT
