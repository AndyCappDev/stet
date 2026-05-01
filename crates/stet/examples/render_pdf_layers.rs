// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Demonstrate stet's PDF Optional Content (layer) API.
//!
//! Opens a PDF, enumerates its Optional Content Groups (OCGs), and renders
//! each page three ways:
//!
//! 1. **default**       — every layer at its document default visibility.
//! 2. **print intent**  — `RenderIntent::Print` honours `/AS` automatic-state
//!                        rules so layers tagged "print only" come on, "view
//!                        only" come off.
//! 3. **toggle first**  — same as default, but the first layer is forcibly
//!                        hidden via a custom `LayerSet`. Drops the
//!                        first-listed OCG out of the render.
//!
//! Run with: `cargo run --example render_pdf_layers -- input.pdf`
//!
//! Produces `layers_default_NNN.png`, `layers_print_NNN.png`, and
//! `layers_toggle_NNN.png` per page in the current directory. PDFs without
//! any layers print "(no Optional Content Groups in this document)" and
//! exit without rendering — the API works on every PDF, but there's
//! nothing to demonstrate without OCGs.

use std::fs::{self, File};
use std::io::BufWriter;

use stet_pdf_reader::{PdfDocument, RenderIntent, layers};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: cargo run --example render_pdf_layers -- <input.pdf>")?;
    let data = fs::read(&path)?;

    let doc = PdfDocument::from_bytes(&data)?;
    let page_count = doc.page_count();
    let layer_list = doc.layers();

    println!("{}: {} page(s), {} layer(s)", path, page_count, layer_list.len());

    if layer_list.is_empty() {
        println!("(no Optional Content Groups in this document)");
        return Ok(());
    }

    // Print each layer's metadata so the caller knows what they're toggling.
    for layer in layer_list {
        println!(
            "  ocg {:>4}  intent={:?}  default_visible={}  locked={}  name={:?}",
            layer.ocg_id, layer.intent, layer.default_visible, layer.locked, layer.name
        );
    }

    // First layer's id — this is what the "toggle" pass hides.
    let first_id = layer_list[0].ocg_id;
    println!(
        "\nWill toggle layer {} ({:?}) OFF in the third render pass.",
        first_id, layer_list[0].name
    );

    for page in 0..page_count {
        // Pass 1 — default layer state.
        let default_set = layers::layer_set_from_document(&doc);
        write_png(
            &doc,
            page,
            150.0,
            &default_set,
            &format!("layers_default_{:03}.png", page + 1),
        )?;

        // Pass 2 — print intent. Honours `/AS` rules so layers tagged
        // "print only" come on and layers tagged "view only" come off.
        let print_set = doc.layer_set_for(RenderIntent::Print);
        write_png(
            &doc,
            page,
            150.0,
            &print_set,
            &format!("layers_print_{:03}.png", page + 1),
        )?;

        // Pass 3 — same as default but first layer forcibly hidden.
        let mut toggled = default_set;
        toggled.set(first_id, false);
        write_png(
            &doc,
            page,
            150.0,
            &toggled,
            &format!("layers_toggle_{:03}.png", page + 1),
        )?;
    }

    Ok(())
}

fn write_png(
    doc: &PdfDocument<'_>,
    page: usize,
    dpi: f64,
    layer_set: &stet_pdf_reader::LayerSet,
    out_path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let (rgba, w, h) = doc.render_page_to_rgba_with_layers(page, dpi, layer_set)?;
    let file = File::create(out_path)?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&rgba)?;
    println!("  wrote {} ({}x{})", out_path, w, h);
    Ok(())
}
