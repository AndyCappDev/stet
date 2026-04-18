// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Render a short PostScript snippet to PNG using the `stet` facade.
//!
//! Run with: `cargo run --example render_ps`
//! Produces `render_ps_out_1.png` in the current directory.

use std::fs::File;
use std::io::BufWriter;

const PS_SOURCE: &[u8] = br#"%!PS-Adobe-3.0
%%BoundingBox: 0 0 612 792
/Helvetica findfont 36 scalefont setfont
72 720 moveto (Hello from stet!) show

0.20 0.45 0.85 setrgbcolor
100 300 moveto 500 300 lineto 500 600 lineto 100 600 lineto closepath fill

0 setgray 4 setlinewidth
100 300 moveto 500 300 lineto 500 600 lineto 100 600 lineto closepath stroke

showpage
"#;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut interp = stet::Interpreter::new();
    let pages = interp.render(PS_SOURCE, 150.0)?;

    for (i, page) in pages.iter().enumerate() {
        let path = format!("render_ps_out_{}.png", i + 1);
        let file = File::create(&path)?;
        let mut encoder = png::Encoder::new(BufWriter::new(file), page.width, page.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(&page.rgba)?;
        println!("wrote {} ({}x{})", path, page.width, page.height);
    }

    Ok(())
}
