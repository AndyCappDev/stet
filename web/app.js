console.log('stet-web v12');

// --- State ---
let workerReady = false;
let currentFileName = '';
let currentPsData = null;

// Per-document state
let pageCount = 0;
let pageDims = [];     // [{width, height}] in device pixels at reference DPI
let referenceDpi = 150;
let currentPage = 0;

// Zoom/pan state
let zoom = 1.0;        // 1.0 = fit-to-viewport
let panX = 0;          // pan offset in CSS pixels
let panY = 0;
let fitScale = 1;      // computed scale for fit mode
let fitMode = true;

// Render request tracking
let nextRequestId = 0;
let pendingRequestId = null;
let pendingThumbnailId = null;
let renderDebounceTimer = null;

const $ = id => document.getElementById(id);
const dropZone = $('drop-zone');
const fileInput = $('file-input');
const canvasContainer = $('canvas-container');
const canvas = $('canvas');
const loading = $('loading');
const statusEl = $('status');
const filenameEl = $('filename');
const pageNav = $('page-nav');
const pageInfo = $('page-info');
const prevBtn = $('prev-page');
const nextBtn = $('next-page');
const zoomInBtn = $('zoom-in');
const zoomOutBtn = $('zoom-out');
const zoomLevelEl = $('zoom-level');
const minimap = $('minimap');
const minimapCanvas = $('minimap-canvas');
const minimapViewport = $('minimap-viewport');
const progressContainer = $('progress-container');
const progressBar = $('progress-bar');

// --- Web Worker setup ---

const worker = new Worker('./worker.js', { type: 'module' });

worker.onmessage = function(e) {
    const msg = e.data;

    if (msg.type === 'ready') {
        workerReady = true;
        statusEl.textContent = 'Ready \u2014 drop a PostScript, EPS, or PDF file';

    } else if (msg.type === 'interpreted') {
        // PS interpretation complete — display lists captured
        pageCount = msg.pageCount;
        pageDims = msg.pages;
        referenceDpi = msg.referenceDpi;

        loading.classList.add('hidden');

        if (pageCount === 0) {
            statusEl.textContent = 'No pages rendered';
            dropZone.classList.remove('hidden');
            canvasContainer.classList.add('hidden');
            return;
        }

        currentPage = 0;
        fitMode = true;

        canvasContainer.classList.remove('hidden');
        canvasContainer.classList.remove('zoomed');
        canvasContainer.scrollLeft = 0;
        canvasContainer.scrollTop = 0;
        minimap.classList.add('hidden');

        if (pageCount > 1) {
            pageNav.classList.remove('hidden');
        } else {
            pageNav.classList.add('hidden');
        }
        updatePageNav();

        // Compute page size in points for status
        const p = pageDims[0];
        const ptW = Math.round(p.width * 72 / p.dpi);
        const ptH = Math.round(p.height * 72 / p.dpi);
        statusEl.textContent =
            `Interpreted in ${(msg.elapsed / 1000).toFixed(3)}s \u00b7 ` +
            `${ptW}\u00d7${ptH} pt \u00b7 ` +
            `${pageCount} page${pageCount > 1 ? 's' : ''}`;

        // Render the first page at fit-to-viewport resolution
        requestViewportRender();

    } else if (msg.type === 'render_progress') {
        if (msg.requestId === pendingRequestId) {
            progressContainer.classList.remove('hidden');
            progressBar.style.width = (msg.percent * 100) + '%';
        }

    } else if (msg.type === 'viewport') {
        if (msg.requestId === pendingThumbnailId) {
            // Minimap thumbnail render complete
            pendingThumbnailId = null;
            const rgba = new Uint8Array(msg.rgba);
            minimapThumbnail = document.createElement('canvas');
            minimapThumbnail.width = msg.width;
            minimapThumbnail.height = msg.height;
            const tctx = minimapThumbnail.getContext('2d');
            const imageData = new ImageData(
                new Uint8ClampedArray(rgba.buffer),
                msg.width, msg.height
            );
            tctx.putImageData(imageData, 0, 0);
            drawMinimapThumbnail();
            return;
        }

        // Viewport render complete — only apply if it matches current request
        if (msg.requestId !== pendingRequestId) return;
        pendingRequestId = null;
        progressContainer.classList.add('hidden');

        const rgba = new Uint8Array(msg.rgba);
        canvas.width = msg.width;
        canvas.height = msg.height;
        const ctx2d = canvas.getContext('2d');
        const imageData = new ImageData(
            new Uint8ClampedArray(rgba.buffer),
            msg.width, msg.height
        );
        ctx2d.putImageData(imageData, 0, 0);

        applyCanvasLayout();

        // Request a minimap thumbnail if we don't have one for this page
        if (!minimapThumbnail) {
            requestMinimapThumbnail();
        }
        updateMinimap();

    } else if (msg.type === 'viewport_error') {
        if (msg.requestId === pendingRequestId) {
            pendingRequestId = null;
            progressContainer.classList.add('hidden');
            console.error('Viewport render error:', msg.message);
        }

    } else if (msg.type === 'error') {
        statusEl.textContent = 'Error: ' + msg.message;
        console.error('Worker error:', msg.message);
        loading.classList.add('hidden');
        dropZone.classList.remove('hidden');
    }
};

// Boot: tell worker to load WASM and init interpreter
statusEl.textContent = 'Initializing interpreter...';
worker.postMessage({
    type: 'init',
    wasmUrl: './pkg/stet_wasm_bg.wasm'
});

// --- File handling ---

function handleFile(file) {
    if (!workerReady) {
        statusEl.textContent = 'Interpreter not ready';
        return;
    }
    currentFileName = file.name;
    filenameEl.textContent = file.name;
    const reader = new FileReader();
    reader.onload = e => {
        currentPsData = new Uint8Array(e.target.result);
        renderFile(currentPsData);
    };
    reader.readAsArrayBuffer(file);
}

function renderFile(data) {
    loading.classList.remove('hidden');
    dropZone.classList.add('hidden');
    canvasContainer.classList.add('hidden');
    pageNav.classList.add('hidden');
    const isPdf = currentFileName.toLowerCase().endsWith('.pdf');
    statusEl.textContent = isPdf ? 'Parsing PDF...' : 'Interpreting...';

    // Reset state
    pageCount = 0;
    pageDims = [];
    currentPage = 0;
    fitMode = true;
    zoom = 1.0;
    panX = 0;
    panY = 0;
    minimapThumbnail = null;

    // Send data to worker (transfer the buffer for zero-copy)
    const buf = data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength);
    worker.postMessage(
        { type: 'render', buffer: buf, filename: currentFileName },
        [buf]
    );
}

// --- Viewport rendering ---

function computeFitScale() {
    const page = pageDims[currentPage];
    if (!page) return 1;
    const dpr = window.devicePixelRatio || 1;
    const cw = canvasContainer.clientWidth - 48;
    const ch = canvasContainer.clientHeight - 48;
    // page.width/height are in reference-DPI pixels (= physical display pixels
    // at effectiveScale=1). Divide by dpr to get CSS pixel dimensions.
    return Math.min(cw / (page.width / dpr), ch / (page.height / dpr), 1);
}

function requestViewportRender() {
    if (pageCount === 0) return;
    const page = pageDims[currentPage];
    if (!page) return;

    const dpr = window.devicePixelRatio || 1;
    fitScale = computeFitScale();
    const effectiveScale = fitMode ? fitScale : zoom;

    // Always render the full page — panning is handled by native CSS scrolling
    // with no re-rendering needed.
    const pixelW = Math.round(page.width * effectiveScale);
    const pixelH = Math.round(page.height * effectiveScale);

    if (pixelW <= 0 || pixelH <= 0) return;

    // Cancel any pending debounce
    if (renderDebounceTimer) {
        clearTimeout(renderDebounceTimer);
        renderDebounceTimer = null;
    }

    const requestId = nextRequestId++;
    pendingRequestId = requestId;

    progressBar.style.width = '0%';
    progressContainer.classList.remove('hidden');

    worker.postMessage({
        type: 'viewport',
        pageIndex: currentPage,
        vpX: 0, vpY: 0,
        vpW: page.width, vpH: page.height,
        pixelW, pixelH,
        requestId,
    });
}

function requestViewportRenderDebounced() {
    if (renderDebounceTimer) clearTimeout(renderDebounceTimer);
    renderDebounceTimer = setTimeout(() => {
        renderDebounceTimer = null;
        requestViewportRender();
    }, 150);
}

// --- Canvas layout ---

function applyCanvasLayout() {
    const page = pageDims[currentPage];
    if (!page) return;

    const dpr = window.devicePixelRatio || 1;
    const effectiveScale = fitMode ? fitScale : zoom;
    // CSS size = pixel size / dpr (full page already rendered at pixel resolution)
    const cssW = (page.width * effectiveScale) / dpr;
    const cssH = (page.height * effectiveScale) / dpr;
    const containerW = canvasContainer.clientWidth;
    const containerH = canvasContainer.clientHeight;
    const isZoomed = !fitMode && (cssW > containerW || cssH > containerH);

    // Remove old spacer if present — canvas IS the full page now
    const spacer = document.getElementById('scroll-spacer');
    if (spacer) spacer.remove();

    if (isZoomed) {
        canvasContainer.classList.add('zoomed');
    } else {
        canvasContainer.classList.remove('zoomed');
    }

    canvas.style.width = cssW + 'px';
    canvas.style.height = cssH + 'px';
    canvas.style.position = '';
    canvas.style.top = '';
    canvas.style.left = '';

    updateZoomLabel();
}

function currentPageDpi() {
    const page = pageDims[currentPage];
    return page ? page.dpi : referenceDpi;
}

function updateZoomLabel() {
    const dpr = window.devicePixelRatio || 1;
    const effectiveScale = fitMode ? fitScale : zoom;
    // Actual render DPI: effectiveScale scales ref-DPI pixels to physical
    // display pixels, so the effective DPI seen on screen is ref_dpi * scale.
    const renderDpi = Math.round(currentPageDpi() * effectiveScale);
    zoomLevelEl.textContent = renderDpi + ' dpi';
}

// --- Page navigation ---

function updatePageNav() {
    if (pageCount <= 1) {
        pageNav.classList.add('hidden');
        return;
    }
    pageNav.classList.remove('hidden');
    pageInfo.textContent = `${currentPage + 1} / ${pageCount}`;
    prevBtn.disabled = currentPage === 0;
    nextBtn.disabled = currentPage === pageCount - 1;
}

function goToPage(index) {
    if (index < 0 || index >= pageCount || index === currentPage) return;
    currentPage = index;
    minimapThumbnail = null;
    updatePageNav();
    // Request a fit-mode thumbnail for the minimap, then re-render at current zoom
    if (!fitMode) {
        requestMinimapThumbnail();
    }
    requestViewportRender();
}

// --- Zoom ---

// Zoom steps as DPI values: 150, 300, 600, 1200, 2400, 4800, 9600
// Stored as scale factors relative to reference DPI (300)
const ZOOM_STEPS = [0.5, 1, 2, 4];

// Get the device-space point at the center of the visible area
function getViewCenter() {
    const page = pageDims[currentPage];
    if (!page) return { x: 0, y: 0 };
    const dpr = window.devicePixelRatio || 1;
    const oldScale = fitMode ? fitScale : zoom;
    // CSS scroll → ref-DPI device coords: multiply by dpr/effectiveScale
    const cx = (canvasContainer.scrollLeft + canvasContainer.clientWidth / 2) * dpr / oldScale;
    const cy = (canvasContainer.scrollTop + canvasContainer.clientHeight / 2) * dpr / oldScale;
    return { x: cx, y: cy };
}

// Scroll so the given device-space point is at the center of the visible area
function scrollToCenter(center) {
    const dpr = window.devicePixelRatio || 1;
    const newScale = fitMode ? fitScale : zoom;
    // Ref-DPI device coords → CSS scroll: multiply by effectiveScale/dpr
    canvasContainer.scrollLeft = center.x * newScale / dpr - canvasContainer.clientWidth / 2;
    canvasContainer.scrollTop = center.y * newScale / dpr - canvasContainer.clientHeight / 2;
}

function zoomIn() {
    if (pageCount === 0) return;
    const center = getViewCenter();
    const current = fitMode ? fitScale : zoom;
    const next = ZOOM_STEPS.find(s => s > current + 0.01);
    if (next !== undefined) {
        zoom = next;
        fitMode = false;
        applyCanvasLayout();
        scrollToCenter(center);
        updateMinimap();
        requestViewportRenderDebounced();
    }
}

function zoomOut() {
    if (pageCount === 0) return;
    const center = getViewCenter();
    const current = fitMode ? fitScale : zoom;
    let prev;
    for (let i = ZOOM_STEPS.length - 1; i >= 0; i--) {
        if (ZOOM_STEPS[i] < current - 0.01) {
            prev = ZOOM_STEPS[i];
            break;
        }
    }
    if (prev !== undefined) {
        zoom = prev;
        fitMode = false;
        applyCanvasLayout();
        scrollToCenter(center);
        updateMinimap();
        requestViewportRenderDebounced();
    }
}

function resetZoom() {
    fitMode = true;
    minimap.classList.add('hidden');
    requestViewportRender();
}

// --- Event handlers ---

zoomInBtn.addEventListener('click', zoomIn);
zoomOutBtn.addEventListener('click', zoomOut);
zoomLevelEl.addEventListener('click', resetZoom);
$('zoom-fit').addEventListener('click', resetZoom);

// Mouse wheel zoom on the canvas area
canvasContainer.addEventListener('wheel', e => {
    if (pageCount === 0) return;
    e.preventDefault();
    if (e.deltaY < 0) zoomIn();
    else if (e.deltaY > 0) zoomOut();
}, { passive: false });

// Scroll → update minimap position (no re-render needed, full page is pre-rendered)
canvasContainer.addEventListener('scroll', () => {
    if (pageCount === 0 || fitMode) return;
    updateMinimap();
});

// Click-drag panning when zoomed
let isPanning = false;
let panStartX = 0;
let panStartY = 0;
let scrollStartX = 0;
let scrollStartY = 0;

canvasContainer.addEventListener('mousedown', e => {
    if (fitMode || pageCount === 0 || e.button !== 0) return;
    isPanning = true;
    panStartX = e.clientX;
    panStartY = e.clientY;
    scrollStartX = canvasContainer.scrollLeft;
    scrollStartY = canvasContainer.scrollTop;
    canvasContainer.classList.add('panning');
    e.preventDefault();
});

window.addEventListener('mousemove', e => {
    if (!isPanning) return;
    canvasContainer.scrollLeft = scrollStartX - (e.clientX - panStartX);
    canvasContainer.scrollTop = scrollStartY - (e.clientY - panStartY);
});

window.addEventListener('mouseup', () => {
    if (!isPanning) return;
    isPanning = false;
    canvasContainer.classList.remove('panning');
});

// Page navigation
prevBtn.addEventListener('click', () => goToPage(currentPage - 1));
nextBtn.addEventListener('click', () => goToPage(currentPage + 1));

// File input
dropZone.addEventListener('click', () => fileInput.click());
fileInput.addEventListener('change', e => {
    if (e.target.files.length > 0) handleFile(e.target.files[0]);
});

// Drag and drop on the whole page
document.addEventListener('dragover', e => {
    e.preventDefault();
    if (!canvasContainer.classList.contains('hidden')) {
        dropZone.classList.remove('hidden');
        canvasContainer.classList.add('hidden');
    }
    dropZone.classList.add('drag-over');
});

document.addEventListener('dragleave', e => {
    if (e.relatedTarget === null) {
        dropZone.classList.remove('drag-over');
        if (pageCount > 0) {
            dropZone.classList.add('hidden');
            canvasContainer.classList.remove('hidden');
        }
    }
});

document.addEventListener('drop', e => {
    e.preventDefault();
    dropZone.classList.remove('drag-over');
    if (e.dataTransfer.files.length > 0) handleFile(e.dataTransfer.files[0]);
});

// Keyboard navigation and zoom
document.addEventListener('keydown', e => {
    if (pageCount === 0) return;
    if (e.key === 'ArrowLeft' && currentPage > 0) {
        goToPage(currentPage - 1);
    } else if (e.key === 'ArrowRight' && currentPage < pageCount - 1) {
        goToPage(currentPage + 1);
    } else if (e.key === '+' || e.key === '=') {
        e.preventDefault();
        zoomIn();
    } else if (e.key === '-') {
        e.preventDefault();
        zoomOut();
    } else if (e.key === '0') {
        e.preventDefault();
        resetZoom();
    }
});

// Resize handler — re-render at new fit scale
window.addEventListener('resize', () => {
    if (pageCount === 0) return;
    if (fitMode) {
        requestViewportRenderDebounced();
    }
});

// --- Minimap ---

let minimapScale = 1;   // ratio: minimap CSS pixels per device-space pixel

function updateMinimap() {
    const page = pageDims[currentPage];
    if (!page) return;

    const dpr = window.devicePixelRatio || 1;
    const effectiveScale = fitMode ? fitScale : zoom;
    const fullW = (page.width / dpr) * effectiveScale;
    const fullH = (page.height / dpr) * effectiveScale;
    const containerW = canvasContainer.clientWidth;
    const containerH = canvasContainer.clientHeight;
    const isZoomed = !fitMode && (fullW > containerW || fullH > containerH);

    if (!isZoomed) {
        minimap.classList.add('hidden');
        return;
    }

    minimap.classList.remove('hidden');

    // Size minimap canvas to fit in 200×260 box, maintaining aspect ratio
    const maxW = 200, maxH = 260;
    minimapScale = Math.min(maxW / fullW, maxH / fullH);
    const mw = Math.round(fullW * minimapScale);
    const mh = Math.round(fullH * minimapScale);
    minimapCanvas.style.width = mw + 'px';
    minimapCanvas.style.height = mh + 'px';

    // Position the viewport rectangle (no canvas redraw needed)
    const scrollX = canvasContainer.scrollLeft;
    const scrollY = canvasContainer.scrollTop;
    const vpLeft = scrollX * minimapScale;
    const vpTop = scrollY * minimapScale;
    const vpW = Math.min(containerW, fullW) * minimapScale;
    const vpH = Math.min(containerH, fullH) * minimapScale;

    // +4px for minimap padding
    minimapViewport.style.left = (vpLeft + 4) + 'px';
    minimapViewport.style.top = (vpTop + 4) + 'px';
    minimapViewport.style.width = vpW + 'px';
    minimapViewport.style.height = vpH + 'px';
}

/// Draw the thumbnail image onto the minimap canvas (called once when thumbnail arrives).
function drawMinimapThumbnail() {
    if (!minimapThumbnail) return;
    const page = pageDims[currentPage];
    if (!page) return;

    const dpr = window.devicePixelRatio || 1;
    const effectiveScale = fitMode ? fitScale : zoom;
    const fullW = (page.width / dpr) * effectiveScale;
    const fullH = (page.height / dpr) * effectiveScale;

    const maxW = 200, maxH = 260;
    const scale = Math.min(maxW / fullW, maxH / fullH);
    const mw = Math.round(fullW * scale);
    const mh = Math.round(fullH * scale);

    minimapCanvas.width = mw;
    minimapCanvas.height = mh;
    const mctx = minimapCanvas.getContext('2d');
    mctx.fillStyle = '#fff';
    mctx.fillRect(0, 0, mw, mh);
    mctx.drawImage(minimapThumbnail, 0, 0, mw, mh);
}

// Cache a thumbnail of the page for the minimap
let minimapThumbnail = null;

function requestMinimapThumbnail() {
    const page = pageDims[currentPage];
    if (!page) return;
    // Render full page at minimap resolution (~200px wide)
    const maxW = 200, maxH = 260;
    const thumbScale = Math.min(maxW / page.width, maxH / page.height, 1);
    const pixelW = Math.round(page.width * thumbScale);
    const pixelH = Math.round(page.height * thumbScale);
    if (pixelW <= 0 || pixelH <= 0) return;

    const requestId = nextRequestId++;
    pendingThumbnailId = requestId;

    worker.postMessage({
        type: 'viewport',
        pageIndex: currentPage,
        vpX: 0, vpY: 0,
        vpW: page.width, vpH: page.height,
        pixelW, pixelH,
        requestId,
    });
}

// Minimap drag interaction
let isMinimapDragging = false;

minimapViewport.addEventListener('mousedown', e => {
    isMinimapDragging = true;
    e.preventDefault();
    e.stopPropagation();
});

window.addEventListener('mousemove', e => {
    if (!isMinimapDragging) return;
    const rect = minimapCanvas.getBoundingClientRect();
    const page = pageDims[currentPage];
    if (!page) return;

    const dpr = window.devicePixelRatio || 1;
    const effectiveScale = fitMode ? fitScale : zoom;
    const fullW = (page.width / dpr) * effectiveScale;
    const fullH = (page.height / dpr) * effectiveScale;
    const containerW = canvasContainer.clientWidth;
    const containerH = canvasContainer.clientHeight;

    // Mouse position relative to minimap canvas center of viewport
    const vpW = Math.min(containerW, fullW) * minimapScale;
    const vpH = Math.min(containerH, fullH) * minimapScale;
    const mx = e.clientX - rect.left - vpW / 2;
    const my = e.clientY - rect.top - vpH / 2;

    // Convert minimap coords to scroll position
    canvasContainer.scrollLeft = mx / minimapScale;
    canvasContainer.scrollTop = my / minimapScale;
});

window.addEventListener('mouseup', () => {
    isMinimapDragging = false;
});

// Click on minimap background to jump
minimapCanvas.addEventListener('mousedown', e => {
    const rect = minimapCanvas.getBoundingClientRect();
    const page = pageDims[currentPage];
    if (!page) return;

    const dpr = window.devicePixelRatio || 1;
    const effectiveScale = fitMode ? fitScale : zoom;
    const fullW = (page.width / dpr) * effectiveScale;
    const fullH = (page.height / dpr) * effectiveScale;
    const containerW = canvasContainer.clientWidth;
    const containerH = canvasContainer.clientHeight;

    const vpW = Math.min(containerW, fullW) * minimapScale;
    const vpH = Math.min(containerH, fullH) * minimapScale;
    const mx = e.clientX - rect.left - vpW / 2;
    const my = e.clientY - rect.top - vpH / 2;

    canvasContainer.scrollLeft = mx / minimapScale;
    canvasContainer.scrollTop = my / minimapScale;

    // Start dragging immediately
    isMinimapDragging = true;
    e.preventDefault();
    e.stopPropagation();
});
