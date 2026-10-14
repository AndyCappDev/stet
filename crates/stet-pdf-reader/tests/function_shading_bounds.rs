// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A function-based shading is sampled into an image before rendering,
//! one sample per device pixel. Only the part that can show is sampled:
//! the domain cut to the page and to the clip. A domain far larger than
//! what shows — a colour wheel cut to a ring — otherwise costs a function
//! evaluation for every pixel of a square that is mostly never seen.

use stet_graphics::display_list::{DisplayElement, DisplayList};
use stet_pdf_reader::PdfDocument;

fn pdf_from(objects: &[String]) -> Vec<u8> {
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

fn stream(dict: &str, body: &str) -> String {
    format!(
        "<< {dict} /Length {} >>\nstream\n{body}\nendstream",
        body.len()
    )
}

/// A 100 x 100 pt page running `content`, with `/Wheel` (object 5) a
/// function-based shading whose red is x and green is y across its
/// domain, which spans 400 pt from (-150, -150), and `/Cell` (object 7) a
/// tiling pattern whose cell draws `/Wheel`.
fn page(content: &str) -> Vec<u8> {
    pdf_from(&[
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R \
         /Resources << /Shading << /Wheel 5 0 R >> /Pattern << /Cell 7 0 R >> >> >>"
            .into(),
        stream("", content),
        "<< /ShadingType 1 /ColorSpace /DeviceRGB /Domain [0 1 0 1] \
         /Matrix [400 0 0 400 -150 -150] /Function 6 0 R >>"
            .into(),
        stream(
            "/FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1]",
            "{ 0 }",
        ),
        stream(
            "/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 \
             /BBox [0 0 1000 1000] /XStep 1000 /YStep 1000 \
             /Resources << /Shading << /Wheel 5 0 R >> >>",
            "/Wheel sh",
        ),
    ])
}

/// The size of every image in the list, tiling cells included.
fn images(pdf: &[u8]) -> Vec<(u32, u32)> {
    fn walk(list: &DisplayList, out: &mut Vec<(u32, u32)>) {
        for element in list.elements() {
            match element {
                DisplayElement::Image { params, .. } => out.push((params.width, params.height)),
                DisplayElement::PatternFill { params } => walk(&params.tile, out),
                DisplayElement::Group { elements, .. } => walk(elements, out),
                _ => {}
            }
        }
    }
    let doc = PdfDocument::from_bytes(pdf).expect("fixture parses");
    let mut out = Vec::new();
    walk(&doc.render_page(0, 72.0).expect("page renders"), &mut out);
    out
}

/// Two samples are kept past each edge for interpolation, and a grid line
/// may fall either side of a pixel boundary.
const MARGIN: u32 = 2 * 2 + 1;

#[test]
fn an_unclipped_shading_is_sampled_only_over_the_page() {
    let drawn = images(&page("/Wheel sh"));
    assert_eq!(drawn.len(), 1);
    let (w, h) = drawn[0];
    assert!(w <= 100 + MARGIN && h <= 100 + MARGIN, "{w} x {h}");
}

#[test]
fn a_clipped_shading_is_sampled_only_inside_the_clip() {
    let drawn = images(&page("q 40 30 10 20 re W n /Wheel sh Q"));
    assert_eq!(drawn.len(), 1);
    let (w, h) = drawn[0];
    assert!((10..=10 + MARGIN).contains(&w), "width {w}");
    assert!((20..=20 + MARGIN).contains(&h), "height {h}");
}

#[test]
fn every_clip_in_force_cuts_the_samples() {
    let drawn = images(&page(
        "q 0 0 50 100 re W n q 0 0 100 30 re W n /Wheel sh Q Q",
    ));
    assert_eq!(drawn.len(), 1);
    let (w, h) = drawn[0];
    assert!(w <= 50 + MARGIN && h <= 30 + MARGIN, "{w} x {h}");
}

#[test]
fn a_clip_with_nothing_inside_samples_nothing() {
    assert!(images(&page("q 10 10 0 0 re W n /Wheel sh Q")).is_empty());
    // A clip path that only moves.
    assert!(images(&page("q 10 10 m W n /Wheel sh Q")).is_empty());
    // Off the page.
    assert!(images(&page("q 300 300 10 10 re W n /Wheel sh Q")).is_empty());
}

/// A tiling cell is drawn in pattern space, where the page's bounds mean
/// nothing: the cell's shading is sampled whole.
#[test]
fn a_shading_in_a_tiling_cell_is_sampled_whole() {
    let drawn = images(&page("/Pattern cs /Cell scn 0 0 100 100 re f"));
    assert_eq!(drawn, [(400, 400)]);
}

/// What shows is what showed when the whole domain was sampled.
#[cfg(feature = "render")]
#[test]
fn the_pixels_inside_the_clip_are_unchanged() {
    let render = |content: &str| {
        let pdf = page(content);
        let doc = PdfDocument::from_bytes(&pdf).expect("fixture parses");
        doc.render_page_to_rgba(0, 72.0).expect("page renders")
    };
    let (whole, w, _) = render("/Wheel sh");
    let (clipped, ..) = render("q 40 30 10 20 re W n /Wheel sh Q");
    // Device rows run down from the top of the page.
    for y in 50..70 {
        for x in 40..50 {
            let i = (y * w as usize + x) * 4;
            assert_eq!(whole[i..i + 4], clipped[i..i + 4], "pixel ({x}, {y})");
        }
    }
}
