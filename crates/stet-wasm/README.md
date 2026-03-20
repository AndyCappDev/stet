# stet-wasm

WebAssembly bindings for the stet PostScript interpreter.

Renders PostScript, EPS, and PDF files in the browser with on-demand
viewport rendering at arbitrary zoom levels.

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
import init, { create_interpreter, render, render_viewport, page_dimensions } from './pkg/stet_wasm.js';

await init({ module_or_path: './pkg/stet_wasm_bg.wasm' });
const interp = create_interpreter();

// Interpret PostScript (captures display lists for viewport rendering)
const numPages = render(interp, psData, 300, 'test.ps');

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

All resources (fonts, init scripts, encodings, ICC profiles) are embedded
in the WASM binary via `include_bytes!()`. The interpreter runs in a Web
Worker; viewport rendering is done on demand at the requested zoom level
using pre-captured display lists.

## License

Apache-2.0 OR MIT
