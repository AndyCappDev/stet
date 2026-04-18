// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Interpret PostScript and walk the resulting display list without
//! rasterizing. Demonstrates the `render_to_display_list` API — the
//! foundation for writing custom output formats (SVG, TIFF, etc.).
//!
//! Run with: `cargo run --example display_list`

use stet::DisplayElement;

const PS_SOURCE: &[u8] = br#"%!PS-Adobe-3.0
%%BoundingBox: 0 0 612 792
/Helvetica findfont 24 scalefont setfont
72 720 moveto (example) show

100 100 moveto 300 100 lineto 300 300 lineto 100 300 lineto closepath
0.8 0.2 0.2 setrgbcolor fill

0 setgray 2 setlinewidth
400 400 60 0 360 arc stroke

showpage
"#;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut interp = stet::Interpreter::new();
    let pages = interp.render_to_display_list(PS_SOURCE, 150.0)?;

    for (i, page) in pages.iter().enumerate() {
        println!(
            "page {}: {}x{} @ {} DPI, {} element(s)",
            i + 1,
            page.width,
            page.height,
            page.dpi,
            page.display_list.len()
        );

        let mut fills = 0usize;
        let mut strokes = 0usize;
        let mut clips = 0usize;
        let mut images = 0usize;
        let mut texts = 0usize;
        let mut other = 0usize;

        for elem in page.display_list.elements() {
            match elem {
                DisplayElement::Fill { .. } => fills += 1,
                DisplayElement::Stroke { .. } => strokes += 1,
                DisplayElement::Clip { .. } | DisplayElement::InitClip => clips += 1,
                DisplayElement::Image { .. } => images += 1,
                DisplayElement::Text { .. } => texts += 1,
                _ => other += 1,
            }
        }

        println!(
            "  fills={}  strokes={}  clips={}  images={}  texts={}  other={}",
            fills, strokes, clips, images, texts, other
        );
    }

    Ok(())
}
