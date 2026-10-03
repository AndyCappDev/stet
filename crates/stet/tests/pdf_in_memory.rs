// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `render_to_pdf` returns the document and touches nothing else.
//!
//! It used to finish the job as the command line does — writing the PDF to
//! `output.pdf` in the working directory, overwriting any file of that name,
//! and announcing it on stderr — then build the document a second time for
//! the bytes it returned, which were titled "output" after that file.
//!
//! This test changes the process's working directory, so it lives in a test
//! binary of its own.

use stet::Interpreter;
use stet_pdf_reader::PdfDocument;

#[test]
fn render_to_pdf_writes_no_file_and_invents_no_title() {
    let dir = std::env::temp_dir().join(format!("stet-pdf-in-memory-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_current_dir(&dir).unwrap();

    let pdf = Interpreter::new()
        .render_to_pdf(b"%!PS\n0 0 100 100 rectfill showpage\n", 72.0)
        .unwrap();
    let left: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    std::env::set_current_dir(std::env::temp_dir()).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(left.is_empty(), "files left behind: {left:?}");

    let doc = PdfDocument::from_bytes(&pdf).unwrap();
    assert_eq!(doc.page_count(), 1);
    assert_eq!(doc.metadata().title, None);

    // A title the job asks for is still written.
    let pdf = Interpreter::new()
        .render_to_pdf(
            b"%!PS\n[ /Title (Asked for) /DOCINFO pdfmark showpage\n",
            72.0,
        )
        .unwrap();
    let doc = PdfDocument::from_bytes(&pdf).unwrap();
    assert_eq!(doc.metadata().title.as_deref(), Some("Asked for"));
}
