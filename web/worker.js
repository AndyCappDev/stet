import init, {
    create_interpreter,
    render,
    render_pdf,
    render_viewport,
    viewport_band_params,
    render_viewport_band,
    page_count,
    page_dimensions,
    reference_dpi,
} from './pkg/stet_wasm.js';

let interpreter = null;

// Queue for incoming viewport requests so we can cancel mid-render.
// Between bands, we check if a newer request has arrived.
let pendingViewport = null;

self.onmessage = async function(e) {
    const { type } = e.data;

    if (type === 'init') {
        try {
            await init({ module_or_path: e.data.wasmUrl });
            interpreter = create_interpreter();
            self.postMessage({ type: 'ready' });
        } catch (err) {
            self.postMessage({ type: 'error', message: 'Init failed: ' + err });
        }

    } else if (type === 'render') {
        if (!interpreter) {
            self.postMessage({ type: 'error', message: 'Interpreter not ready' });
            return;
        }
        try {
            const { filename } = e.data;
            const data = new Uint8Array(e.data.buffer);
            const start = performance.now();

            // Detect PDF vs PostScript and use appropriate renderer
            const dpi = 300;
            const isPdf = filename.toLowerCase().endsWith('.pdf');
            const numPages = isPdf
                ? render_pdf(interpreter, data, dpi)
                : render(interpreter, data, dpi, filename);
            const elapsed = performance.now() - start;

            // Collect page dimensions and per-page DPI
            const pages = [];
            for (let i = 0; i < numPages; i++) {
                const dims = page_dimensions(interpreter, i);
                pages.push({ width: dims[0], height: dims[1], dpi: dims[2] });
            }

            self.postMessage({
                type: 'interpreted',
                pageCount: numPages,
                pages,
                referenceDpi: reference_dpi(interpreter),
                elapsed,
            });
        } catch (err) {
            self.postMessage({ type: 'error', message: '' + err });
        }

    } else if (type === 'viewport') {
        // Store the latest request; processViewport will pick it up
        pendingViewport = e.data;
        // If we're not currently inside a banded render, process immediately
        processViewport();
    }
};

let rendering = false;

async function processViewport() {
    if (rendering) return;  // band loop will pick up pendingViewport
    if (!interpreter) {
        self.postMessage({ type: 'error', message: 'Interpreter not ready' });
        return;
    }

    while (pendingViewport) {
        const req = pendingViewport;
        pendingViewport = null;
        rendering = true;

        try {
            const { pageIndex, vpX, vpY, vpW, vpH, pixelW, pixelH, requestId } = req;
            const params = viewport_band_params(pixelW, pixelH);
            const numBands = params[0];
            const bandH = params[1];

            if (numBands <= 1) {
                // Small render — single pass (no progress needed)
                const page = render_viewport(
                    interpreter, pageIndex,
                    vpX, vpY, vpW, vpH,
                    pixelW, pixelH
                );
                const rgba = page.rgba;
                const width = page.width;
                const height = page.height;
                page.free();

                self.postMessage(
                    { type: 'viewport', requestId, width, height, rgba: rgba.buffer },
                    [rgba.buffer]
                );
            } else {
                // Banded render with progress and cancel support
                const fullRgba = new Uint8Array(pixelW * pixelH * 4);
                let cancelled = false;

                for (let b = 0; b < numBands; b++) {
                    // Check for a newer request between bands
                    // Yield to the event loop so queued messages can be delivered
                    await new Promise(r => setTimeout(r, 0));
                    if (pendingViewport) {
                        cancelled = true;
                        break;
                    }

                    const band = render_viewport_band(
                        interpreter, pageIndex,
                        vpX, vpY, vpW, vpH,
                        pixelW, pixelH,
                        b, bandH, numBands
                    );
                    const bandRgba = band.rgba;
                    fullRgba.set(bandRgba, b * bandH * pixelW * 4);
                    band.free();

                    self.postMessage({
                        type: 'render_progress',
                        requestId,
                        percent: (b + 1) / numBands,
                    });
                }

                if (!cancelled) {
                    self.postMessage(
                        { type: 'viewport', requestId, width: pixelW, height: pixelH,
                          rgba: fullRgba.buffer },
                        [fullRgba.buffer]
                    );
                }
            }
        } catch (err) {
            self.postMessage({
                type: 'viewport_error',
                requestId: req.requestId,
                message: '' + err,
            });
        }

        rendering = false;
        // Loop back to check if a newer request arrived during rendering
    }
}
