// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A shading drawn many times on a page is sampled into colour stops once
//! and the stops reused. Each use must still get the stops of its own
//! shading, under the rendering intent in force where it is drawn.

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

/// An axial shading from `c0` to `c1`, across the page.
fn axial(c0: &str, c1: &str) -> String {
    format!(
        "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 100 0] \
         /Function << /FunctionType 2 /Domain [0 1] /C0 [{c0}] /C1 [{c1}] /N 1 >> >>"
    )
}

/// A radial shading from `c0` to `c1`.
fn radial(c0: &str, c1: &str) -> String {
    format!(
        "<< /ShadingType 3 /ColorSpace /DeviceRGB /Coords [50 50 0 50 50 40] \
         /Function << /FunctionType 2 /Domain [0 1] /C0 [{c0}] /C1 [{c1}] /N 1 >> >>"
    )
}

/// A page with `/Red` (object 5, red to yellow), `/Blue` (object 6, blue to
/// white) and `/Ring` (object 7, radial) as shading resources, plus `/Here`
/// written directly in the resource dictionary (green to black).
fn page(content: &str) -> Vec<u8> {
    pdf_from(&[
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R \
             /Resources << /Shading << /Red 5 0 R /Blue 6 0 R /Ring 7 0 R /Here {} >> >> >>",
            axial("0 1 0", "0 0 0")
        ),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
        axial("1 0 0", "1 1 0"),
        axial("0 0 1", "1 1 1"),
        radial("1 0 1", "0 1 1"),
    ])
}

/// The first and last stop colours of every shading in the list, in
/// drawing order, as `(kind, [r g b] first, [r g b] last, stops)`.
fn shadings(pdf: &[u8]) -> Vec<(&'static str, [f64; 3], [f64; 3], usize)> {
    fn walk(list: &DisplayList, out: &mut Vec<(&'static str, [f64; 3], [f64; 3], usize)>) {
        for element in list.elements() {
            let (kind, stops) = match element {
                DisplayElement::AxialShading { params } => ("axial", &params.color_stops),
                DisplayElement::RadialShading { params } => ("radial", &params.color_stops),
                DisplayElement::Group { elements, .. } => {
                    walk(elements, out);
                    continue;
                }
                _ => continue,
            };
            let rgb = |i: usize| {
                let c = &stops[i].color;
                [c.r, c.g, c.b].map(|v| (v * 100.0).round() / 100.0)
            };
            out.push((kind, rgb(0), rgb(stops.len() - 1), stops.len()));
        }
    }
    let doc = PdfDocument::from_bytes(pdf).expect("fixture parses");
    let mut out = Vec::new();
    walk(&doc.render_page(0, 72.0).expect("page renders"), &mut out);
    out
}

const RED: (&str, [f64; 3], [f64; 3]) = ("axial", [1.0, 0.0, 0.0], [1.0, 1.0, 0.0]);
const BLUE: (&str, [f64; 3], [f64; 3]) = ("axial", [0.0, 0.0, 1.0], [1.0, 1.0, 1.0]);
const RING: (&str, [f64; 3], [f64; 3]) = ("radial", [1.0, 0.0, 1.0], [0.0, 1.0, 1.0]);
const HERE: (&str, [f64; 3], [f64; 3]) = ("axial", [0.0, 1.0, 0.0], [0.0, 0.0, 0.0]);

#[test]
fn each_use_of_a_shading_gets_that_shadings_stops() {
    let drawn = shadings(&page(
        "/Red sh /Blue sh /Red sh /Ring sh /Here sh /Blue sh /Ring sh /Here sh /Red sh",
    ));
    let colours: Vec<_> = drawn.iter().map(|&(k, a, b, _)| (k, a, b)).collect();
    assert_eq!(colours, [RED, BLUE, RED, RING, HERE, BLUE, RING, HERE, RED]);
    // A repeat has as many stops as the first use: it is the same sampling.
    assert_eq!(drawn[0].3, drawn[2].3);
    assert_eq!(drawn[0].3, drawn[8].3);
    assert_eq!(drawn[3].3, drawn[6].3);
}

/// Each use owns its stops: where it is drawn does not come from the cache.
#[test]
fn a_reused_shading_is_still_drawn_where_each_use_puts_it() {
    let doc_pdf = page("q 1 0 0 1 0 0 cm /Red sh Q q 2 0 0 2 10 20 cm /Red sh Q");
    let doc = PdfDocument::from_bytes(&doc_pdf).unwrap();
    let list = doc.render_page(0, 72.0).unwrap();
    let ctms: Vec<_> = list
        .elements()
        .iter()
        .filter_map(|e| match e {
            DisplayElement::AxialShading { params } => Some(format!("{:?}", params.ctm)),
            _ => None,
        })
        .collect();
    assert_eq!(ctms.len(), 2);
    assert_ne!(ctms[0], ctms[1]);
}

/// The stops are colours after conversion, and the rendering intent is part
/// of that conversion, so a shading drawn under two intents is sampled for
/// each. With device RGB the two come out the same; what matters here is
/// that changing the intent between uses neither fails nor mixes shadings.
#[test]
fn a_shading_drawn_under_two_intents_is_right_under_both() {
    let drawn = shadings(&page(
        "/Perceptual ri /Red sh /Saturation ri /Red sh /Blue sh /Perceptual ri /Blue sh /Red sh",
    ));
    let colours: Vec<_> = drawn.iter().map(|&(k, a, b, _)| (k, a, b)).collect();
    assert_eq!(colours, [RED, RED, BLUE, BLUE, RED]);
}
