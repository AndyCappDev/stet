// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Debug helper: dump per-element bboxes from both pipelines.
//!
//! Usage: cargo run --release --example dump_bboxes -- <pdf> [page_1based] [dpi]

use stet_pdf_reader::PdfDocument;
use stet_render::debug_bbox_comparison;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <pdf> [page_1based] [dpi]", args[0]);
        std::process::exit(1);
    }
    let pdf_path = &args[1];
    let page_1based: usize = args.get(2).map(|s| s.parse().unwrap()).unwrap_or(1);
    let dpi: f64 = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(150.0);

    let data = std::fs::read(pdf_path).unwrap();
    let doc = PdfDocument::from_bytes(&data).unwrap();
    let page = page_1based - 1;
    let list = doc.render_page(page, dpi).unwrap();

    println!(
        "# Display list for {} page {} at {} DPI",
        pdf_path, page_1based, dpi
    );
    println!("# Elements: {}", list.elements().len());
    println!();
    for line in debug_bbox_comparison(&list, dpi) {
        println!("{}", line);
    }
}
