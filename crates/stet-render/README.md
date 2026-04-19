# stet-render

Software rasterizer for stet display lists. Turns a
[`DisplayList`](https://docs.rs/stet-graphics/latest/stet_graphics/display_list/struct.DisplayList.html)
— whether it came from the stet PostScript interpreter or from
[`stet-pdf-reader`](https://crates.io/crates/stet-pdf-reader) — into
RGBA pixels.

Built on
[`stet-tiny-skia`](https://crates.io/crates/stet-tiny-skia), a modified
fork of [tiny-skia](https://github.com/RazrFalcon/tiny-skia) with
higher-quality analytical antialiasing for PostScript-grade hairlines.
Rendering is multi-threaded and banded (sized to fit L2 cache), with
mask caching, clip fast paths, and ICC-aware CMYK handling.

Because `DisplayList` is the neutral meeting point between stet's PS
interpreter and `stet-pdf-reader`, **the same renderer handles both PS
and PDF sources** — there is no separate PDF rasterizer. The only
difference a consumer sees is where the display list came from.

This is a low-level crate. Most users should use the
[`stet`](https://crates.io/crates/stet) facade (for PS / EPS) or
[`stet-pdf-reader`](https://crates.io/crates/stet-pdf-reader) (for PDF),
both of which drive `stet-render` for you.

## What's inside

| API | Purpose |
|-----|---------|
| [`render_to_rgba`] | One-shot: `DisplayList` → `Vec<u8>` RGBA. Automatic banded parallelism. |
| [`prepare_display_list`] + [`render_region_prepared`] | Viewport rendering — pre-compute bounding boxes once, then rasterize any rectangular region at any zoom without re-interpretation. Powers the interactive viewer and the WASM frontend. |
| [`render_region_prepared_parallel`] / `_cancellable` / `_with_progress` | Parallel viewport variants with progress callbacks and cancellation tokens for interactive UIs. |
| [`ImageCache`] | Pre-converted RGBA image cache, amortizing decode + ICC conversion across repeated viewport renders of the same page. |
| [`build_icc_cache_for_list`] | Sweep a `DisplayList` and populate an [`IccCache`](https://docs.rs/stet-graphics/latest/stet_graphics/icc/struct.IccCache.html) with every embedded ICC profile it references. |
| [`SkiaDevice`] | `OutputDevice` implementation — plug it into a `Context` for streaming per-page PS/PDF rendering. |
| [`PngSinkFactory`] | `PageSinkFactory` that streams banded output straight to a PNG file. |

[`render_to_rgba`]: https://docs.rs/stet-render/latest/stet_render/fn.render_to_rgba.html
[`prepare_display_list`]: https://docs.rs/stet-render/latest/stet_render/fn.prepare_display_list.html
[`render_region_prepared`]: https://docs.rs/stet-render/latest/stet_render/fn.render_region_prepared.html
[`render_region_prepared_parallel`]: https://docs.rs/stet-render/latest/stet_render/fn.render_region_prepared_parallel.html
[`ImageCache`]: https://docs.rs/stet-render/latest/stet_render/struct.ImageCache.html
[`build_icc_cache_for_list`]: https://docs.rs/stet-render/latest/stet_render/fn.build_icc_cache_for_list.html
[`SkiaDevice`]: https://docs.rs/stet-render/latest/stet_render/struct.SkiaDevice.html
[`PngSinkFactory`]: https://docs.rs/stet-render/latest/stet_render/struct.PngSinkFactory.html

## Features

| Feature | Default | Description |
|---------|---------|------------|
| `parallel` | yes | Multi-threaded banded rendering via `rayon`. Disable for a single-threaded build. |

## Usage

### Render a PDF page to RGBA

```rust
use stet_pdf_reader::PdfDocument;
use stet_render::render_to_rgba;

let data = std::fs::read("document.pdf")?;
let doc = PdfDocument::from_bytes(&data)?;
let display_list = doc.render_page(0, 150.0)?;
let (w, h) = {
    let (pw, ph) = doc.page_size(0)?;
    let s = 150.0 / 72.0;
    ((pw * s).round() as u32, (ph * s).round() as u32)
};

let rgba = render_to_rgba(&display_list, w, h, 150.0, Some(doc.icc_cache()), false);
# Ok::<(), Box<dyn std::error::Error>>(())
```

(For a one-call shortcut, `PdfDocument::render_page_to_rgba` wraps this
and the `w`/`h` derivation.)

### Render a PostScript-produced display list

```rust
use stet::Interpreter;
use stet_render::render_to_rgba;

let mut interp = Interpreter::new();
let pages = interp.render_to_display_list(ps_data, 300.0)?;

let p = &pages[0];
let rgba = render_to_rgba(&p.display_list, p.width, p.height, p.dpi, None, false);
# Ok::<(), Box<dyn std::error::Error>>(())
```

### Viewport rendering (zoom / pan without re-interpreting)

```rust
use stet_render::{prepare_display_list, render_region_prepared};

let prepared = prepare_display_list(&display_list);

let rgba = render_region_prepared(
    &display_list,
    &prepared,
    vp_x, vp_y, vp_w, vp_h,   // viewport in device pixels
    pixel_w, pixel_h,           // output dimensions
    dpi,
    None,   // ICC cache
    None,   // ImageCache
    false,  // no_aa
);
```

## License

Apache-2.0 OR MIT
