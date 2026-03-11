import init, {
    create_interpreter,
    render,
    render_pdf,
    render_viewport,
    page_count,
    page_dimensions,
    reference_dpi,
} from './pkg/stet_wasm.js';

let interpreter = null;

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
        if (!interpreter) {
            self.postMessage({ type: 'error', message: 'Interpreter not ready' });
            return;
        }
        try {
            const { pageIndex, vpX, vpY, vpW, vpH, pixelW, pixelH, requestId } = e.data;
            const page = render_viewport(
                interpreter, pageIndex,
                vpX, vpY, vpW, vpH,
                pixelW, pixelH
            );
            const rgba = page.rgba;
            const width = page.width;
            const height = page.height;
            page.free();

            // Transfer the buffer for zero-copy
            self.postMessage(
                { type: 'viewport', requestId, width, height, rgba: rgba.buffer },
                [rgba.buffer]
            );
        } catch (err) {
            self.postMessage({
                type: 'viewport_error',
                requestId: e.data.requestId,
                message: '' + err,
            });
        }
    }
};
