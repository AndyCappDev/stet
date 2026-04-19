# stet-wasm

WebAssembly bindings for stet — renders PostScript, EPS, and PDF files
in the browser using on-demand viewport rasterization.

## Intended use — a capability sampler, not production

`stet-wasm` exists so visitors can try stet's rendering quality on their
own files without installing Rust, any crates, or the CLI. It's a
sampler: drop in a PS or PDF, look at the result, decide whether to
adopt the real crates for your own project.

It is **not** intended as a production browser PDF viewer:

- **No system fonts.** WASM can't reach the OS font directories, so
  font coverage is limited to the 35 URW fonts embedded in the binary
  plus whatever the source PDF embeds. Non-standard PostScript font
  references get substituted to the nearest URW equivalent; documents
  that expect a specific unembedded font will look wrong.
- **Fixed zoom stops, not arbitrary zoom.** The viewer offers **fit-to-
  window, 75, 150, 300, and 600 DPI**. Arbitrary zoom was removed —
  re-rasterizing the display list at an arbitrary scale every scroll /
  pinch was too slow in single-threaded WASM to feel responsive.
- **Single-threaded WASM.** No rayon parallelism, no SIMD beyond what
  `wasm32+simd128` gives us. Rendering is ~2× slower than native stet on
  the same file — fine for a sampler, not fine for heavy production
  viewing.

For anything real, use the native [`stet`](https://crates.io/crates/stet)
facade, [`stet-pdf-reader`](https://crates.io/crates/stet-pdf-reader),
or [`stet-cli`](https://crates.io/crates/stet-cli).

## Building

```bash
cd web
./build.sh                    # PS/EPS only
./build.sh pdf-reader         # PS/EPS + PDF support
```

Or manually:

```bash
wasm-pack build --target web --release crates/stet-wasm
```

## API

```javascript
import init, {
    create_interpreter,
    render,
    render_viewport,
    page_dimensions,
} from './pkg/stet_wasm.js';

await init({ module_or_path: './pkg/stet_wasm_bg.wasm' });
const interp = create_interpreter();

// Interpret PostScript (captures display lists for viewport rendering)
const numPages = render(interp, psData, 150, 'test.ps');

// Get page dimensions at reference DPI
const [width, height, dpi] = page_dimensions(interp, 0);

// Render a viewport region
const page = render_viewport(interp, 0, vpX, vpY, vpW, vpH, pixelW, pixelH);
// page.width, page.height, page.rgba
```

## Features

| Feature | Default | Description |
|---------|---------|------------|
| `pdf-reader` | no | PDF file parsing and rendering via `stet-pdf-reader` |

## Architecture

All resources (URW fonts, init scripts, encodings, ICC profiles) are
embedded in the WASM binary via `include_bytes!()`. The interpreter
runs in a Web Worker; viewport rendering is done on demand at the
requested zoom step using pre-captured display lists.

## License

Apache-2.0 OR MIT
