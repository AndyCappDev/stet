// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A pattern tile larger than the page is drawn only where it shows.
//!
//! A tile holding a shading, a group or a soft mask is drawn once into a
//! buffer of its own and stamped wherever it falls. PowerPoint writes a
//! gradient fill as one such tile several times the size of the page, and
//! every band of the page drew the whole of it: a slide took seconds and,
//! on many threads, gigabytes. The buffer also stopped at 8192 pixels, so
//! what a tile held beyond that was not drawn at all.

#![cfg(feature = "render")]

use stet_graphics::layer_set::LayerSet;
use stet_pdf_reader::{PageBackground, PdfDocument};

fn stream(dict: &str, body: &str) -> String {
    format!(
        "<< {dict} /Length {} >>\nstream\n{body}\nendstream",
        body.len()
    )
}

/// A 100 × 100 pt page filled with a tiling pattern whose tile is `bbox`,
/// repeats every `step` and is placed by `matrix`. The tile is an axial
/// shading, red where the tile's x is `from` and blue where it is `to`.
fn page(bbox: &str, step: f64, matrix: &str, from: f64, to: f64) -> Vec<u8> {
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R \
         /Resources << /Pattern << /P 5 0 R >> >> >>"
            .to_string(),
        stream("", "/Pattern cs /P scn 0 0 100 100 re f"),
        stream(
            &format!(
                "/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 \
                 /BBox {bbox} /XStep {step} /YStep {step} /Matrix {matrix} \
                 /Resources << /Shading << /Sh 6 0 R >> >>"
            ),
            "/Sh sh",
        ),
        format!(
            "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [{from} 0 {to} 0] \
             /Extend [true true] \
             /Function << /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1 >> >>"
        ),
    ];
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = pdf.len();
    pdf.extend(format!("xref\n0 {}\n0000000000 65535 f\r\n", objects.len() + 1).as_bytes());
    for off in offsets {
        pdf.extend(format!("{off:010} 00000 n\r\n").as_bytes());
    }
    pdf.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}

/// The red and blue of the pixel `x` across the middle row of the page at
/// 72 dpi.
fn red_and_blue(pdf: &[u8], x: usize) -> (u8, u8) {
    let doc = PdfDocument::from_bytes(pdf).expect("fixture parses");
    let list = doc.render_page(0, 72.0).unwrap();
    let rgba = stet_render::render_to_rgba_with_background(
        &list,
        100,
        100,
        72.0,
        Some(doc.icc_cache()),
        false,
        &LayerSet::new(),
        PageBackground::White,
    );
    let at = (50 * 100 + x) * 4;
    (rgba[at], rgba[at + 2])
}

/// The page shows the far corner of a tile 9,100 pixels across, past where
/// the tile's buffer used to end.
#[test]
fn a_tile_far_larger_than_the_page_is_drawn() {
    let pdf = page("[-9000 -9000 100 100]", 9100.0, "[1 0 0 1 0 0]", 0.0, 100.0);
    let (left, right) = (red_and_blue(&pdf, 5), red_and_blue(&pdf, 94));
    assert!(left.0 > 220 && left.1 < 35, "left {left:?}");
    assert!(right.0 < 35 && right.1 > 220, "right {right:?}");
}

/// Two tiles each larger than the page meet on it: the end of one, then
/// the start of the next.
#[test]
fn the_parts_of_two_large_tiles_meet_where_the_tiles_do() {
    let pdf = page("[0 0 150 150]", 150.0, "[1 0 0 1 -100 0]", 0.0, 150.0);
    // The first tile runs from -100 to 50 on the page, the second from 50.
    let (before, after) = (red_and_blue(&pdf, 48), red_and_blue(&pdf, 52));
    assert!(before.0 < 20 && before.1 > 235, "end of a tile {before:?}");
    assert!(after.0 > 235 && after.1 < 20, "start of a tile {after:?}");
    let within = red_and_blue(&pdf, 10);
    assert!(
        (58..=78).contains(&within.0) && (177..=197).contains(&within.1),
        "110 of 150 along a tile {within:?}"
    );
}
