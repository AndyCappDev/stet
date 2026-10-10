// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Which tiles of a tiling pattern are drawn, and where.
//!
//! A tile's cell is its `/BBox`, which need not sit at the tile's origin.
//! The renderer chose tiles by their origins — those within a step of the
//! area being filled — so a cell several steps from its origin was drawn
//! where nobody was looking, and the fill came out blank.

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

/// A 100 × `height` page whose pattern `/P0` is object 5.
fn page(height: usize, content: &str, pattern: String) -> Vec<u8> {
    pdf_from(&[
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 {height}] /Contents 4 0 R \
             /Resources << /Pattern << /P0 5 0 R >> >> >>"
        ),
        stream("", content),
        pattern,
    ])
}

/// A solid blue cell `cell`, repeated every 10 units both ways.
fn blue_cell(cell: [i32; 4]) -> String {
    let [x0, y0, x1, y1] = cell;
    stream(
        &format!(
            "/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 /BBox [{x0} {y0} {x1} {y1}] \
             /XStep 10 /YStep 10 /Resources << >>"
        ),
        &format!("0 0 1 rg {x0} {y0} {} {} re f", x1 - x0, y1 - y0),
    )
}

/// Render at 72 dpi and return a sampler in PDF user space.
fn render(pdf: Vec<u8>, height: usize) -> impl Fn(usize, usize) -> [u8; 3] {
    let doc = PdfDocument::from_owned(pdf).expect("document opens");
    let (rgba, w, h) = doc.render_page_to_rgba(0, 72.0).expect("page renders");
    assert_eq!((w as usize, h as usize), (100, height));
    assert!(
        doc.parse_warnings().is_empty(),
        "{:?}",
        doc.parse_warnings()
    );
    move |x, y| {
        let i = ((height - 1 - y) * 100 + x) * 4;
        [rgba[i], rgba[i + 1], rgba[i + 2]]
    }
}

const WHITE: [u8; 3] = [255, 255, 255];
const BLUE: [u8; 3] = [0, 0, 255];

const FILL_PAGE: &str = "/Pattern cs /P0 scn 0 0 100 100 re f";

/// The cell is four steps up and right of its tile's origin. The tiles that
/// paint the page's bottom-left corner have origins four steps outside it.
#[test]
fn a_cell_away_from_its_tiles_origin_still_fills_the_shape() {
    let at = render(page(100, FILL_PAGE, blue_cell([40, 40, 50, 50])), 100);
    for (x, y) in [(5, 5), (15, 35), (35, 15), (45, 45), (75, 95), (95, 5)] {
        assert_eq!(at(x, y), BLUE, "({x},{y})");
    }
}

/// The same, with the cell down and left of the origin.
#[test]
fn a_cell_behind_its_tiles_origin_still_fills_the_shape() {
    let at = render(page(100, FILL_PAGE, blue_cell([-40, -40, -30, -30])), 100);
    for (x, y) in [(5, 5), (55, 55), (75, 95), (95, 65), (95, 95)] {
        assert_eq!(at(x, y), BLUE, "({x},{y})");
    }
}

/// A fill is drawn in the rows it reaches and not the whole surface, on a
/// page tall enough to be rendered in several bands; it must still land
/// where the shape is, and nowhere else.
#[test]
fn a_small_shape_on_a_tall_page_is_filled_where_it_is() {
    let content = "/Pattern cs /P0 scn 20 130 30 20 re f 60 380 20 250 re f";
    let at = render(page(700, content, blue_cell([0, 0, 10, 10])), 700);
    for (x, y) in [
        (21, 131),
        (35, 140),
        (49, 149),
        (61, 381),
        (70, 500),
        (79, 629),
    ] {
        assert_eq!(at(x, y), BLUE, "({x},{y}) is inside a shape");
    }
    for (x, y) in [
        (18, 140),
        (52, 140),
        (35, 127),
        (35, 152),
        (70, 377),
        (70, 632),
        (57, 500),
    ] {
        assert_eq!(at(x, y), WHITE, "({x},{y}) is outside both");
    }
}
