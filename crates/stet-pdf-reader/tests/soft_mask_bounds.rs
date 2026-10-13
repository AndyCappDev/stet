// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A soft mask's content is drawn only where the mask can show it, and a
//! group with nothing to paint is not drawn at all.
//!
//! InDesign gives a feathered or glowing object a form whose `/BBox` is far
//! larger than the page, and leaves behind groups that hold only a clip. A
//! page of a hundred such objects drew a hundred gradients the size of the
//! page, each to show a small part, and composited dozens of empty
//! page-sized groups.

#![cfg(feature = "render")]

use stet_graphics::layer_set::LayerSet;
use stet_pdf_reader::{PageBackground, PdfDocument};

fn stream(dict: &str, body: &str) -> String {
    format!(
        "<< {dict} /Length {} >>\nstream\n{body}\nendstream",
        body.len()
    )
}

const HUGE: &str = "[-30000 -30000 30000 30000]";

/// A 100 × 100 pt page running `content`, with `/GS` a luminosity soft mask
/// that is white over `mask_rect` and black elsewhere, `/Sh` a shading red
/// at the left of the page and blue at the right, and `/Empty` a
/// transparency group holding only a clip. Every form's `/BBox` is far
/// larger than the page.
fn page(content: &str, mask_rect: &str) -> Vec<u8> {
    let group = "/Group << /S /Transparency /CS /DeviceRGB /I true >>";
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R \
         /Resources << /ExtGState << /GS 5 0 R >> /Shading << /Sh 7 0 R >> \
         /XObject << /Empty 8 0 R >> >> >>"
            .to_string(),
        stream("", content),
        "<< /Type /ExtGState /SMask << /Type /Mask /S /Luminosity /G 6 0 R >> >>".to_string(),
        stream(
            &format!("/Type /XObject /Subtype /Form /BBox {HUGE} {group}"),
            &format!("1 g {mask_rect} re f"),
        ),
        "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 100 0] /Extend [true true] \
         /Function << /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1 >> >>"
            .to_string(),
        stream(
            &format!("/Type /XObject /Subtype /Form /BBox {HUGE} {group}"),
            "0 0 100 100 re W n",
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

/// The page at 72 dpi, as RGB at `(x, y)` from the top left.
fn render(pdf: &[u8]) -> impl Fn(usize, usize) -> [u8; 3] + use<> {
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
    move |x, y| {
        let at = (y * 100 + x) * 4;
        [rgba[at], rgba[at + 1], rgba[at + 2]]
    }
}

/// The shading shows through the mask where the mask is, in the colours it
/// has there, and nowhere else.
#[test]
fn masked_content_keeps_its_place_on_the_page() {
    // The mask is 20 pt square, 60 pt along and 30 pt up: rows 50 to 70
    // from the top.
    let at = render(&page("/GS gs /Sh sh", "60 30 20 20"));
    for (x, want_red) in [(62, 96), (70, 76), (78, 55)] {
        let [r, g, b] = at(x, 60);
        assert!(
            r.abs_diff(want_red) <= 4 && g <= 4 && b.abs_diff(255 - want_red) <= 4,
            "{x} along: {r} {g} {b}"
        );
    }
    for (x, y) in [(57, 60), (83, 60), (70, 47), (70, 73), (10, 10), (95, 95)] {
        assert_eq!(at(x, y), [255, 255, 255], "outside the mask at {x},{y}");
    }
}

/// A group holding only a clip leaves the page as it was.
#[test]
fn a_group_with_nothing_to_paint_changes_nothing() {
    let plain = render(&page("0 0 1 rg 20 20 60 60 re f", "0 0 1 1"));
    let with_empty = render(&page(
        "0 0 1 rg 20 20 60 60 re f /Empty Do q 0.5 0 0 0.5 0 0 cm /Empty Do Q",
        "0 0 1 1",
    ));
    for y in 0..100 {
        for x in 0..100 {
            assert_eq!(plain(x, y), with_empty(x, y), "at {x},{y}");
        }
    }
}
