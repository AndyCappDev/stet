// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A soft mask set with `gs` masks what the stream paints from then on,
//! and nothing else.
//!
//! The reader tracks that as a scope over its display list. A pattern's
//! cell and a Type 3 glyph procedure are interpreted into display lists of
//! their own, and the scope was left open across them: closing it there
//! wrapped the cell's elements, or none at all, and the fill it was meant
//! for went unmasked.

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

const FORM: &str = "/Type /XObject /Subtype /Form /BBox [0 0 100 100] \
                    /Group << /S /Transparency /CS /DeviceGray >>";

/// A 100 × 100 page. Objects 5 and 6 are luminosity mask forms, white over
/// the left half and over the bottom half; `/GSleft` and `/GSbottom` set
/// them. `extra` objects number from 7.
fn page(resources: &str, content: &str, extra: &[String]) -> Vec<u8> {
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R /Resources << \
             /ExtGState << /GSleft << /SMask << /Type /Mask /S /Luminosity /G 5 0 R >> >> >> \
             {resources} >> >>"
        ),
        stream("", content),
        stream(FORM, "0 g 0 0 100 100 re f 1 g 0 0 50 100 re f"),
        stream(FORM, "0 g 0 0 100 100 re f 1 g 0 0 100 50 re f"),
    ];
    objects.extend_from_slice(extra);
    pdf_from(&objects)
}

/// Render at 72 dpi and return a sampler in PDF user space.
fn render(pdf: Vec<u8>) -> impl Fn(usize, usize) -> [u8; 3] {
    let doc = PdfDocument::from_owned(pdf).expect("document opens");
    let (rgba, w, h) = doc.render_page_to_rgba(0, 72.0).expect("page renders");
    assert_eq!((w, h), (100, 100));
    assert!(
        doc.parse_warnings().is_empty(),
        "{:?}",
        doc.parse_warnings()
    );
    move |x, y| {
        let i = ((99 - y) * 100 + x) * 4;
        [rgba[i], rgba[i + 1], rgba[i + 2]]
    }
}

const WHITE: [u8; 3] = [255, 255, 255];
const RED: [u8; 3] = [255, 0, 0];
const BLUE: [u8; 3] = [0, 0, 255];

/// A cell of two fills, so that a mask closed inside the cell has
/// something to wrap.
fn tiling_pattern() -> String {
    stream(
        "/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 10 10] \
         /XStep 10 /YStep 10 /Resources << >>",
        "0 0 1 rg 0 0 5 10 re f 0 0 1 rg 5 0 5 10 re f",
    )
}

#[test]
fn a_tiling_pattern_fill_is_masked() {
    let at = render(page(
        "/Pattern << /P0 7 0 R >>",
        "q /GSleft gs /Pattern cs /P0 scn 0 0 100 100 re f Q",
        &[tiling_pattern()],
    ));
    for (x, y) in [(22, 25), (27, 75), (42, 50)] {
        assert_eq!(at(x, y), BLUE, "({x},{y}) is inside the mask");
    }
    for (x, y) in [(72, 25), (77, 75), (92, 50)] {
        assert_eq!(at(x, y), WHITE, "({x},{y}) is outside the mask");
    }
}

#[test]
fn a_tiling_pattern_stroke_is_masked() {
    let at = render(page(
        "/Pattern << /P0 7 0 R >>",
        "q /GSleft gs /Pattern CS /P0 SCN 20 w 0 50 m 100 50 l S Q",
        &[tiling_pattern()],
    ));
    assert_eq!(at(25, 50), BLUE);
    assert_eq!(at(75, 50), WHITE);
}

#[test]
fn the_mask_goes_on_past_a_pattern_fill() {
    // The pattern is used first; the plain fill after it is still masked.
    let at = render(page(
        "/Pattern << /P0 7 0 R >>",
        "q /GSleft gs /Pattern cs /P0 scn 0 0 100 50 re f 1 0 0 rg 0 50 100 50 re f Q",
        &[tiling_pattern()],
    ));
    assert_eq!(at(25, 25), BLUE);
    assert_eq!(at(75, 25), WHITE);
    assert_eq!(at(25, 75), RED);
    assert_eq!(at(75, 75), WHITE);
}

#[test]
fn a_pattern_cell_is_not_painted_through_the_page_mask() {
    // The pattern again once the mask is gone: the cell made under the
    // mask is whole.
    let at = render(page(
        "/Pattern << /P0 7 0 R >>",
        "q /GSleft gs /Pattern cs /P0 scn 0 0 100 50 re f Q \
         /Pattern cs /P0 scn 0 50 100 50 re f",
        &[tiling_pattern()],
    ));
    assert_eq!(at(25, 25), BLUE);
    assert_eq!(at(75, 25), WHITE);
    for x in [22, 27, 72, 77] {
        assert_eq!(at(x, 75), BLUE, "x = {x}");
    }
}

/// A Type 3 font whose one glyph, `a`, is a 1000-unit square drawn by
/// `charproc`, with `/GSbottom` among its resources.
fn type3_font(charproc_obj: usize) -> String {
    format!(
        "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
         /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << /a {charproc_obj} 0 R >> \
         /Encoding << /Type /Encoding /Differences [97 /a] >> \
         /FirstChar 97 /LastChar 97 /Widths [1000] \
         /Resources << /ExtGState << \
         /GSbottom << /SMask << /Type /Mask /S /Luminosity /G 6 0 R >> >> >> >> >>"
    )
}

#[test]
fn a_type3_glyph_is_masked_with_the_text_around_it() {
    let at = render(page(
        "/Font << /F1 7 0 R >>",
        "q /GSleft gs 0 0 1 rg BT /F1 100 Tf 0 0 Td (a) Tj ET Q",
        &[type3_font(8), stream("", "1000 0 d0 0 0 1000 1000 re f")],
    ));
    assert_eq!(at(25, 50), BLUE);
    assert_eq!(at(75, 50), WHITE);
}

#[test]
fn a_mask_set_inside_a_glyph_leaves_the_page_mask_in_force() {
    // The page's mask is open when the glyph procedure sets one of its
    // own. What was painted before the glyph stays masked by the page's.
    let at = render(page(
        "/Font << /F1 7 0 R >>",
        "q /GSleft gs 1 0 0 rg 0 50 100 50 re f \
         BT /F1 100 Tf 0 0 Td (a) Tj ET Q",
        &[
            type3_font(8),
            stream("", "1000 0 d0 /GSbottom gs 0 0 1000 400 re f"),
        ],
    ));
    assert_eq!(at(25, 75), RED);
    assert_eq!(at(75, 75), WHITE);
}
