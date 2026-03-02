import init, { create_interpreter, render, set_page_callback, clear_page_callback } from './pkg/stet_wasm.js';

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
            const { dpi, filename } = e.data;
            const data = new Uint8Array(e.data.buffer);
            const start = performance.now();

            // Stream pages to main thread as they render
            set_page_callback(function(index, width, height, rgba) {
                // rgba is a Uint8Array copied from WASM memory — transfer its buffer
                const buf = rgba.slice().buffer;
                self.postMessage(
                    { type: 'page', index, width, height, rgba: buf },
                    [buf]
                );
            });

            const result = render(interpreter, data, dpi, filename);

            clear_page_callback();

            const elapsed = performance.now() - start;
            const pageCount = result.page_count;
            result.free();

            self.postMessage({ type: 'done', pageCount, elapsed });
        } catch (err) {
            clear_page_callback();
            self.postMessage({ type: 'error', message: '' + err });
        }
    }
};
