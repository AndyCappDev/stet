// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A transparency group inside another, on a page rendered in bands.
//!
//! A band passes over a page-level group that does not reach it, going by
//! the group's bounding box. A group inside another is not looked at by the
//! bands, so each band met every one of them, and one that did not reach
//! the band was rendered into an offscreen the size of the band and
//! composited back — which draws nothing, slowly. Such a group is now
//! passed over as well; these check that it is still drawn where it is.

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

const WIDTH: usize = 300;
const HEIGHT: usize = 600;

/// A 300 × 600 page — several bands at 72 dpi — whose content is one
/// page-sized transparency group. Inside it, at half opacity, are two
/// isolated groups painting solid red: one over `tall`, which crosses band
/// seams, and one over `small`, which sits inside a single band.
fn page(tall: [usize; 4], small: [usize; 4]) -> Vec<u8> {
    let inner = |[x0, y0, x1, y1]: [usize; 4]| {
        stream(
            &format!(
                "/Type /XObject /Subtype /Form /BBox [{x0} {y0} {x1} {y1}] \
                 /Group << /S /Transparency /I true >>"
            ),
            &format!("1 0 0 rg {x0} {y0} {} {} re f", x1 - x0, y1 - y0),
        )
    };
    pdf_from(&[
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {WIDTH} {HEIGHT}] /Contents 4 0 R \
             /Resources << /XObject << /Outer 5 0 R >> >> >>"
        ),
        stream("", "/Outer Do"),
        stream(
            &format!(
                "/Type /XObject /Subtype /Form /BBox [0 0 {WIDTH} {HEIGHT}] \
                 /Group << /S /Transparency >> \
                 /Resources << /XObject << /Tall 6 0 R /Small 7 0 R >> \
                 /ExtGState << /Half << /ca 0.5 /BM /Multiply >> >> >>"
            ),
            "/Half gs /Tall Do /Small Do",
        ),
        inner(tall),
        inner(small),
    ])
}

/// Render at 72 dpi and return a sampler in PDF user space.
fn render(pdf: Vec<u8>) -> impl Fn(usize, usize) -> [u8; 3] {
    let doc = PdfDocument::from_owned(pdf).expect("document opens");
    let (rgba, w, h) = doc.render_page_to_rgba(0, 72.0).expect("page renders");
    assert_eq!((w as usize, h as usize), (WIDTH, HEIGHT));
    assert!(
        doc.parse_warnings().is_empty(),
        "{:?}",
        doc.parse_warnings()
    );
    move |x, y| {
        let i = ((HEIGHT - 1 - y) * WIDTH + x) * 4;
        [rgba[i], rgba[i + 1], rgba[i + 2]]
    }
}

const WHITE: [u8; 3] = [255, 255, 255];

#[test]
fn a_nested_group_is_drawn_in_every_band_it_reaches_and_no_other() {
    let tall = [40, 60, 120, 540];
    let small = [200, 300, 260, 340];
    let at = render(page(tall, small));

    // Half-opaque red on white, the same down the whole of the tall group:
    // across each band seam as within a band.
    let painted = at(80, 300);
    assert!(
        painted[0] > 240 && (110..150).contains(&painted[1]) && (110..150).contains(&painted[2]),
        "{painted:?}"
    );
    for y in tall[1] + 1..tall[3] - 1 {
        assert_eq!(at(80, y), painted, "row {y} of the tall group");
    }
    for y in small[1] + 1..small[3] - 1 {
        assert_eq!(at(230, y), painted, "row {y} of the small group");
    }

    // Nothing outside either group, in the bands they reach or the others.
    for y in (0..HEIGHT).step_by(7) {
        for x in [10, 150, 290] {
            assert_eq!(at(x, y), WHITE, "({x}, {y})");
        }
    }
    for y in (0..tall[1] - 1).chain(tall[3] + 1..HEIGHT) {
        assert_eq!(at(80, y), WHITE, "(80, {y})");
    }
    for y in (0..small[1] - 1).chain(small[3] + 1..HEIGHT) {
        assert_eq!(at(230, y), WHITE, "(230, {y})");
    }
}

/// A group whose box lies off the page draws nothing and does not disturb
/// what is on it.
#[test]
fn a_nested_group_off_the_page_draws_nothing() {
    let at = render(page([40, 60, 120, 540], [400, 700, 460, 760]));
    assert!(at(80, 300)[0] > 240 && at(80, 300)[1] < 150);
    for y in (0..HEIGHT).step_by(5) {
        assert_eq!(at(230, y), WHITE, "(230, {y})");
    }
}
