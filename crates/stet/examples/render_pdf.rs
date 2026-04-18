// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Render every page of a PDF to PNG using `stet-pdf-reader`.
//!
//! The `stet-pdf-reader` crate does not depend on `stet-core` or the
//! PostScript VM — this example demonstrates using stet purely as a PDF
//! renderer.
//!
//! Run with: `cargo run --example render_pdf -- input.pdf`
//! Produces `render_pdf_out_001.png`, `render_pdf_out_002.png`, ... in the
//! current directory.

use std::fs::{self, File};
use std::io::BufWriter;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: cargo run --example render_pdf -- <input.pdf>")?;
    let data = fs::read(&path)?;

    let doc = stet_pdf_reader::PdfDocument::from_bytes(&data)?;
    let page_count = doc.page_count();
    println!("{}: {} page(s)", path, page_count);

    for page in 0..page_count {
        let (rgba, w, h) = doc.render_page_to_rgba(page, 150.0)?;

        let out = format!("render_pdf_out_{:03}.png", page + 1);
        let file = File::create(&out)?;
        let mut encoder = png::Encoder::new(BufWriter::new(file), w, h);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(&rgba)?;

        println!("  wrote {} ({}x{})", out, w, h);
    }

    Ok(())
}
