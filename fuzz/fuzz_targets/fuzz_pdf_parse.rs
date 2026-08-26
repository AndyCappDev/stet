// Open and render arbitrary bytes as a PDF.
//
// This is the widest target: `PdfDocument::from_bytes` runs the lexer, the
// xref parser and its multi-stage rebuild, the object resolver, and the
// filter chain, and `render_page` then runs the content-stream interpreter,
// the font parsers, the shading and function evaluators, and the image
// decoders. Nearly every byte of untrusted input stet handles flows through
// here.
//
// The target deliberately does no error checking: any `Err` is a *success*.
// What it is looking for is a panic, an abort (stack overflow or a failed
// allocation), or a hang.
#![no_main]

use libfuzzer_sys::fuzz_target;
use stet_pdf_reader::PdfDocument;

fuzz_target!(|data: &[u8]| {
    let Ok(doc) = PdfDocument::from_bytes(data) else {
        return;
    };

    // Cap the page count. A crafted file can legitimately declare a great many
    // pages, and rendering all of them turns every input into a timeout, which
    // buries real findings in noise.
    for page in 0..doc.page_count().min(4) {
        let _ = doc.page_size(page);
        let _ = doc.page_contents(page);
        let _ = doc.render_page(page, 72.0);
    }

    // The structural API is a separate surface from rendering: it walks the
    // outline, name, and field trees, which have their own cycle and depth
    // guards, and none of it is reached by `render_page`.
    let _ = doc.metadata();
    let _ = doc.outline();
    let _ = doc.destinations();
    let _ = doc.form();
    let _ = doc.embedded_files();
    let _ = doc.layers();
    let _ = doc.viewer_preferences();
    for page in 0..doc.page_count().min(4) {
        let _ = doc.page_annotations(page);
        let _ = doc.page_boxes(page);
    }
});
