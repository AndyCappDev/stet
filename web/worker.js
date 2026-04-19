import init, {
    create_interpreter,
    render,
    open_pdf,
    render_pdf_page,
    render_viewport,
    viewport_band_params,
    render_viewport_band,
    page_count,
    page_dimensions,
    reference_dpi,
    step_ps_page,
    ps_stream_active,
} from './pkg/stet_wasm.js';

let interpreter = null;

// Queue for incoming viewport requests so we can cancel mid-render.
// Between bands, we check if a newer request has arrived.
let pendingViewport = null;

// Streaming-PS bookkeeping. A monotonically-increasing session token lets
// us recognise "we're in a newer render" mid-loop without cross-awaiting.
// When the token no longer matches `currentPsSession`, the loop bails.
let currentPsSession = 0;

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

            // A new render supersedes any streaming PS loop from the prior
            // file — bump the session token and the loop's guard will bail.
            currentPsSession++;
            const session = currentPsSession;

            // Detect PDF vs PostScript and use appropriate renderer
            const dpi = 300;
            const isPdf = filename.toLowerCase().endsWith('.pdf');
            // PDFs use lazy per-page rendering: open_pdf parses only the
            // xref + page tree here so the first viewport render can start
            // immediately; each page's content stream is interpreted on
            // demand by render_viewport / render_viewport_band.
            //
            // PostScript streams page-by-page: render() returns after the
            // first showpage, then driveStreamingPs keeps polling
            // step_ps_page in the background while the main thread can
            // already render page 1.
            const numPages = isPdf
                ? open_pdf(interpreter, data, dpi)
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

            // Kick off streaming for multi-page PS. The loop yields between
            // every page so viewport requests get serviced first.
            if (!isPdf && ps_stream_active(interpreter)) {
                driveStreamingPs(session, numPages);
            }
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

/// Wait until the worker has no viewport work queued or in progress, so
/// that user-driven navigation beats PS-streaming to the CPU. Bails early
/// if a new render supersedes this session.
async function waitForViewportIdle(session) {
    // Give the message queue a few ticks to deliver any in-flight
    // viewport message (setTimeout(0) yields to macrotasks; a single
    // yield delivers at most one message).
    for (let i = 0; i < 3; i++) {
        await new Promise(r => setTimeout(r, 0));
        if (session !== currentPsSession) return;
    }
    // If a viewport is queued or currently rendering, keep yielding
    // until it finishes. Polling interval here is a knob: too short
    // (< 10ms) wastes CPU; too long adds perceptible stall between
    // finished viewport and resumed PS stepping.
    while (rendering || pendingViewport) {
        await new Promise(r => setTimeout(r, 10));
        if (session !== currentPsSession) return;
    }
}

/// Drive step_ps_page until the PS program finishes (or a newer render
/// supersedes us). Between pages, yields to the event loop and waits for
/// any in-flight viewport render to complete so page-navigation clicks
/// feel responsive — a sync step_ps_page call blocks the worker entirely,
/// and each PS page can take hundreds of milliseconds on complex content.
async function driveStreamingPs(session, knownPages) {
    let count = knownPages;
    while (session === currentPsSession && ps_stream_active(interpreter)) {
        await waitForViewportIdle(session);
        if (session !== currentPsSession) return;
        if (!ps_stream_active(interpreter)) break;

        let newCount;
        try {
            newCount = step_ps_page(interpreter);
        } catch (err) {
            self.postMessage({ type: 'ps_stream_error', message: '' + err });
            return;
        }

        if (newCount > count) {
            // Gather the freshly-interpreted pages' dimensions and ship
            // them to the main thread so page nav updates in real time.
            const added = [];
            for (let i = count; i < newCount; i++) {
                const dims = page_dimensions(interpreter, i);
                added.push({ width: dims[0], height: dims[1], dpi: dims[2] });
            }
            self.postMessage({
                type: 'pages_appended',
                startIndex: count,
                pages: added,
                totalPages: newCount,
            });
            count = newCount;
        }
        // If count didn't advance, ps_stream_active will be false next
        // iteration (the program finished in this step); loop exits.
    }
    if (session === currentPsSession) {
        self.postMessage({ type: 'ps_stream_done', totalPages: count });
    }
}

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
