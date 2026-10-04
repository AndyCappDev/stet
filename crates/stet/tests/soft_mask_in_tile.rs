// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A soft mask inside a pattern tile renders wherever the tile lands.
//!
//! A tile's elements are moved into device space by their CTM alone, their
//! paths left in pattern space, and the mask raster was bounded by the raw
//! paths — so it was built where the tile is in pattern space, not where it
//! is drawn, and masked everything out. A tile whose device origin is not at
//! x = 0 took another route and rendered; one at x = 0 rendered nothing.
#![cfg(feature = "render")]

use stet::Interpreter;

/// A pattern of 100-unit cells, one per 120, whose PaintProc runs `cell`,
/// made with its origin `dx` along, filling most of a 300-point page.
fn job(cell: &str, dx: f64) -> String {
    format!(
        "%!PS\n<< /PageSize [300 300] >> setpagedevice\n\
         << /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 100 100] \
         /XStep 120 /YStep 120 /PaintProc {{ pop {cell} }} >> \
         [1 0 0 1 {dx} 0] makepattern setpattern 10 10 280 280 rectfill showpage\n"
    )
}

fn render(ps: &str) -> Vec<u8> {
    Interpreter::new()
        .render(ps.as_bytes(), 72.0)
        .unwrap()
        .remove(0)
        .rgba
}

/// Pixels that are not white.
fn ink(rgba: &[u8]) -> usize {
    rgba.chunks(4).filter(|p| p[..3] != [255, 255, 255]).count()
}

/// The cell renders the same with the pattern's origin at x = 0 as one
/// whole step along, where the tiling is identical, and draws something.
fn renders_at_origin(cell: &str) {
    let at_zero = render(&job(cell, 0.0));
    let one_step = render(&job(cell, 120.0));
    assert!(ink(&one_step) > 1000, "the cell draws nothing");
    let differing = at_zero
        .chunks(4)
        .zip(one_step.chunks(4))
        .filter(|(a, b)| a.iter().zip(b.iter()).any(|(x, y)| x.abs_diff(*y) > 8))
        .count();
    assert!(
        differing < 50,
        "{differing} pixels differ: ink {} at x = 0, {} one step along",
        ink(&at_zero),
        ink(&one_step)
    );
}

#[test]
fn an_alpha_mask_in_a_tile_renders_at_the_origin() {
    renders_at_origin(
        "<< /Subtype /Alpha /BBox [0 0 100 100] >> beginsoftmask \
         0 setgray 0 0 50 100 rectfill endsoftmask \
         0 0.6 0 setrgbcolor 0 0 100 100 rectfill clearsoftmask",
    );
}

#[test]
fn a_luminosity_mask_in_a_tile_renders_at_the_origin() {
    renders_at_origin(
        "<< /Subtype /Luminosity /BBox [0 0 100 100] >> beginsoftmask \
         1 setgray 0 0 50 100 rectfill endsoftmask \
         0 0.6 0 setrgbcolor 0 0 100 100 rectfill clearsoftmask",
    );
}

/// A mask drawn by a stroke alone is bounded through its CTM too.
#[test]
fn a_stroked_mask_in_a_tile_renders_at_the_origin() {
    renders_at_origin(
        "<< /Subtype /Alpha /BBox [0 0 100 100] >> beginsoftmask \
         0 setgray 10 setlinewidth 10 10 moveto 90 90 lineto stroke endsoftmask \
         0 0 1 setrgbcolor 0 0 100 100 rectfill clearsoftmask",
    );
}

/// The same through the PDF reader: a hand-written PDF whose pattern
/// cell sets a soft mask, its `/Matrix` putting the cell's origin at
/// `dx`.
#[test]
fn a_soft_mask_in_a_pdf_tile_renders_at_the_origin() {
    let pdf = |dx: i32| -> Vec<u8> {
        let mask = "1 g 0 0 50 100 re f";
        let cell = "q /GS0 gs 0 0.6 0 rg 0 0 100 100 re f Q";
        let page = "/Pattern cs /P0 scn 10 10 280 280 re f";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] \
             /Resources << /Pattern << /P0 5 0 R >> >> /Contents 4 0 R >>"
                .to_string(),
            format!("<< /Length {} >>\nstream\n{page}\nendstream", page.len()),
            format!(
                "<< /Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 \
                 /BBox [0 0 100 100] /XStep 120 /YStep 120 /Matrix [1 0 0 1 {dx} 0] \
                 /Resources << /ExtGState << /GS0 << /SMask << /Type /Mask \
                 /S /Luminosity /G 6 0 R >> >> >> >> /Length {} >>\n\
                 stream\n{cell}\nendstream",
                cell.len()
            ),
            format!(
                "<< /Type /XObject /Subtype /Form /BBox [0 0 100 100] \
                 /Group << /S /Transparency /CS /DeviceGray >> /Length {} >>\n\
                 stream\n{mask}\nendstream",
                mask.len()
            ),
        ];
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
        for o in offsets {
            out.extend(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    };
    let render = |bytes: &[u8]| -> Vec<u8> {
        let doc = stet_pdf_reader::PdfDocument::from_bytes(bytes).unwrap();
        let list = doc.render_page(0, 72.0).unwrap();
        stet::render_to_rgba(&list, 300, 300, 72.0, None, false)
    };
    let at_zero = render(&pdf(0));
    let one_step = render(&pdf(120));
    assert!(ink(&one_step) > 1000, "the cell draws nothing");
    assert!(
        ink(&at_zero).abs_diff(ink(&one_step)) < 50,
        "ink {} at x = 0, {} one step along",
        ink(&at_zero),
        ink(&one_step)
    );
}
