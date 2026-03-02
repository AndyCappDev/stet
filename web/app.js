console.log('stet-web v6');

let pages = [];
let pageCount = 0;
let currentPage = 0;
let currentFileName = '';
let currentPsData = null;
let workerReady = false;

const $ = id => document.getElementById(id);
const dropZone = $('drop-zone');
const fileInput = $('file-input');
const canvasContainer = $('canvas-container');
const canvas = $('canvas');
const loading = $('loading');
const statusEl = $('status');
const filenameEl = $('filename');
const dpiSelect = $('dpi-select');
const pageNav = $('page-nav');
const pageInfo = $('page-info');
const prevBtn = $('prev-page');
const nextBtn = $('next-page');

// --- Web Worker setup ---

const worker = new Worker('./worker.js', { type: 'module' });

let renderStart = 0;

worker.onmessage = function(e) {
    const msg = e.data;

    if (msg.type === 'ready') {
        workerReady = true;
        statusEl.textContent = 'Ready \u2014 drop a PostScript or EPS file';

    } else if (msg.type === 'page') {
        // Page streamed from worker during rendering
        const rgba = new Uint8Array(msg.rgba);
        pages[msg.index] = {
            width: msg.width,
            height: msg.height,
            rgba: rgba
        };
        pageCount = msg.index + 1;

        if (msg.index === 0) {
            // First page — display immediately
            currentPage = 0;
            displayPage(0);
            loading.classList.add('hidden');
            canvasContainer.classList.remove('hidden');
        }
        updatePageNav();

    } else if (msg.type === 'done') {
        const elapsed = msg.elapsed;
        pageCount = msg.pageCount;
        loading.classList.add('hidden');
        canvasContainer.classList.remove('hidden');

        if (pageCount === 0) {
            statusEl.textContent = 'No pages rendered';
            dropZone.classList.remove('hidden');
            canvasContainer.classList.add('hidden');
            return;
        }

        if (pageCount > 1) {
            pageNav.classList.remove('hidden');
        } else {
            pageNav.classList.add('hidden');
        }
        updatePageNav();

        const dpi = parseInt(dpiSelect.value, 10);
        const p = pages[0];
        if (p) {
            const ptW = Math.round(p.width * 72 / dpi);
            const ptH = Math.round(p.height * 72 / dpi);
            statusEl.textContent =
                `Rendered in ${(elapsed / 1000).toFixed(3)}s \u00b7 ` +
                `${ptW}\u00d7${ptH} pt \u00b7 ${dpi} DPI \u00b7 ` +
                `${pageCount} page${pageCount > 1 ? 's' : ''}`;
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
        renderPS(currentPsData);
    };
    reader.readAsArrayBuffer(file);
}

function renderPS(data) {
    const dpi = parseInt(dpiSelect.value, 10);
    loading.classList.remove('hidden');
    dropZone.classList.add('hidden');
    canvasContainer.classList.add('hidden');
    pageNav.classList.add('hidden');
    statusEl.textContent = 'Rendering...';

    // Reset page state
    pages = [];
    pageCount = 0;
    currentPage = 0;

    // Send data to worker (transfer the buffer for zero-copy)
    const buf = data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength);
    worker.postMessage(
        { type: 'render', buffer: buf, dpi, filename: currentFileName },
        [buf]
    );
}

// --- Page display ---

function displayPage(index) {
    const page = pages[index];
    if (!page) return;
    canvas.width = page.width;
    canvas.height = page.height;
    const ctx = canvas.getContext('2d');
    const imageData = new ImageData(
        new Uint8ClampedArray(page.rgba.buffer),
        page.width, page.height
    );
    ctx.putImageData(imageData, 0, 0);
}

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

// --- Navigation ---

prevBtn.addEventListener('click', () => {
    if (currentPage > 0) {
        currentPage--;
        displayPage(currentPage);
        updatePageNav();
    }
});

nextBtn.addEventListener('click', () => {
    if (currentPage < pageCount - 1) {
        currentPage++;
        displayPage(currentPage);
        updatePageNav();
    }
});

// DPI change re-renders
dpiSelect.addEventListener('change', () => {
    if (currentPsData) renderPS(currentPsData);
});

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

// Keyboard navigation
document.addEventListener('keydown', e => {
    if (pageCount === 0) return;
    if (e.key === 'ArrowLeft' && currentPage > 0) {
        currentPage--;
        displayPage(currentPage);
        updatePageNav();
    } else if (e.key === 'ArrowRight' && currentPage < pageCount - 1) {
        currentPage++;
        displayPage(currentPage);
        updatePageNav();
    }
});
