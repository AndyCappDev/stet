// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `PdfDocument::set_page_area` — rendering a page box, or a rectangle, of a
//! PDF page as the page.
//!
//! The central check: an area whose corners fall on whole device pixels
//! renders the pixels it covers in a render of the whole MediaBox — to
//! within one level of f32 rounding — at every `/Rotate`. That holds only if the area's offset, size and
//! rotation all go through the same transform the page itself does.

#![cfg(feature = "render")]

use stet_pdf_reader::{PageArea, PdfDocument, PdfError};

const MEDIA: [f64; 4] = [0.0, 0.0, 300.0, 200.0];
const CROP: [f64; 4] = [10.0, 10.0, 290.0, 190.0];
const BLEED: [f64; 4] = [5.0, 5.0, 295.0, 195.0];
const ART: [f64; 4] = [40.0, 30.0, 160.0, 130.0];

/// Marks with fractional, antialiased edges that cross the art box, and a
/// strip in the bleed outside the crop box.
const CONTENT: &str = "1 0 0 rg 20 20 100 60 re f \
    0 0 1 rg 120.5 60.25 90.3 70.7 re f \
    0 0.6 0 rg 45 100 m 150 125 l 80 180 l f \
    1 0 1 rg 0 0 300 8 re f";

fn pdf_from(objects: &[Vec<u8>]) -> Vec<u8> {
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend(format!("{} 0 obj\n", i + 1).as_bytes());
        pdf.extend(body);
        pdf.extend(b"\nendobj\n");
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

fn rect(r: [f64; 4]) -> String {
    format!("[{} {} {} {}]", r[0], r[1], r[2], r[3])
}

/// One page with the boxes above and `/Rotate rotate`. The ArtBox is an
/// indirect array; there is no TrimBox.
fn fixture(rotate: i32) -> Vec<u8> {
    let page = format!(
        "<< /Type /Page /Parent 2 0 R /MediaBox {} /CropBox {} /BleedBox {} \
         /ArtBox 5 0 R /Rotate {rotate} /Contents 4 0 R /Resources << >> >>",
        rect(MEDIA),
        rect(CROP),
        rect(BLEED),
    );
    let mut contents = format!("<< /Length {} >>\nstream\n", CONTENT.len()).into_bytes();
    contents.extend(CONTENT.as_bytes());
    contents.extend(b"\nendstream");
    pdf_from(&[
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        page.into_bytes(),
        contents,
        rect(ART).into_bytes(),
    ])
}

/// Where `area` lies in a 72-dpi render whose page is `base`, at `rotate`:
/// `(x, y, width, height)` in device pixels. Worked out independently of the
/// reader, from what each `/Rotate` does to the page.
fn device_rect(base: [f64; 4], area: [f64; 4], rotate: i32) -> (usize, usize, usize, usize) {
    let [bx0, by0, bx1, by1] = base;
    let [ax0, ay0, ax1, ay1] = area;
    let (x0, x1, y0, y1) = match rotate {
        0 => (ax0 - bx0, ax1 - bx0, by1 - ay1, by1 - ay0),
        90 => (ay0 - by0, ay1 - by0, ax0 - bx0, ax1 - bx0),
        180 => (bx1 - ax1, bx1 - ax0, ay0 - by0, ay1 - by0),
        270 => (by1 - ay1, by1 - ay0, bx1 - ax1, bx1 - ax0),
        _ => unreachable!(),
    };
    (
        x0 as usize,
        y0 as usize,
        (x1 - x0) as usize,
        (y1 - y0) as usize,
    )
}

fn crop(rgba: &[u8], width: u32, (x, y, w, h): (usize, usize, usize, usize)) -> Vec<u8> {
    let stride = width as usize * 4;
    (y..y + h)
        .flat_map(|row| rgba[row * stride + x * 4..row * stride + (x + w) * 4].to_vec())
        .collect()
}

fn render(pdf: &[u8], area: PageArea) -> (Vec<u8>, u32, u32) {
    let mut doc = PdfDocument::from_bytes(pdf).unwrap();
    doc.set_page_area(area);
    doc.render_page_to_rgba(0, 72.0).unwrap()
}

#[test]
fn the_crop_box_is_the_default() {
    let pdf = fixture(0);
    let doc = PdfDocument::from_bytes(&pdf).unwrap();
    assert_eq!(doc.page_area(), PageArea::CropBox);
    assert_eq!(doc.page_area_rect(0).unwrap(), CROP);
    assert_eq!(doc.page_size(0).unwrap(), (280.0, 180.0));
    assert_eq!(
        doc.render_page_to_rgba(0, 72.0).unwrap(),
        render(&pdf, PageArea::CropBox),
    );
}

/// The area renders the pixels it covers in a render of the whole MediaBox,
/// at every rotation.
#[test]
fn an_area_renders_the_pixels_it_covers_on_the_page() {
    let areas = [
        (PageArea::CropBox, CROP),
        (PageArea::BleedBox, BLEED),
        (PageArea::ArtBox, ART),
        (
            PageArea::Rect([100.0, 50.0, 250.0, 150.0]),
            [100.0, 50.0, 250.0, 150.0],
        ),
    ];
    for rotate in [0, 90, 180, 270] {
        let pdf = fixture(rotate);
        let (page, page_w, _) = render(&pdf, PageArea::MediaBox);
        for (area, user_rect) in areas {
            let (rgba, w, h) = render(&pdf, area);
            let at = device_rect(MEDIA, user_rect, rotate);
            assert_eq!(
                (w as usize, h as usize),
                (at.2, at.3),
                "{area:?} at /Rotate {rotate}: size"
            );
            // Equal to within one level: the rasteriser works in f32, and a
            // fractional edge translated by a whole number of pixels rounds
            // differently (130.95 lands at 69.05 on the page, -0.95 in the
            // area), now and then tipping a coverage value over a 1/255
            // boundary. A misplaced area shifts whole edges, hundreds of
            // levels off.
            let expected = crop(&page, page_w, at);
            let worst = (0..rgba.len())
                .map(|i| rgba[i].abs_diff(expected[i]))
                .max()
                .unwrap_or(0);
            assert!(
                worst <= 1,
                "{area:?} at /Rotate {rotate}: a pixel differs from the page's by {worst} levels"
            );
        }
    }
}

#[test]
fn a_rotated_page_reports_the_area_size_rotated() {
    let pdf = fixture(90);
    let mut doc = PdfDocument::from_bytes(&pdf).unwrap();
    doc.set_page_area(PageArea::ArtBox);
    assert_eq!(doc.page_size(0).unwrap(), (100.0, 120.0));
}

#[test]
fn an_absent_box_is_the_crop_box() {
    let pdf = fixture(0);
    let mut doc = PdfDocument::from_bytes(&pdf).unwrap();
    doc.set_page_area(PageArea::TrimBox);
    assert_eq!(doc.page_area_rect(0).unwrap(), CROP);
}

/// The ArtBox in the fixture is an indirect object; it must still be found.
#[test]
fn an_indirect_box_is_read() {
    let pdf = fixture(0);
    let mut doc = PdfDocument::from_bytes(&pdf).unwrap();
    doc.set_page_area(PageArea::ArtBox);
    assert_eq!(doc.page_area_rect(0).unwrap(), ART);
}

/// A bleed lies outside the crop box by definition, so only the MediaBox
/// limits an area.
#[test]
fn areas_are_clipped_to_the_media_box_not_the_crop_box() {
    let pdf = fixture(0);
    let mut doc = PdfDocument::from_bytes(&pdf).unwrap();
    doc.set_page_area(PageArea::BleedBox);
    assert_eq!(doc.page_area_rect(0).unwrap(), BLEED);

    // Corners in either order, partly off the medium.
    doc.set_page_area(PageArea::Rect([150.0, 250.0, -50.0, 100.0]));
    assert_eq!(doc.page_area_rect(0).unwrap(), [0.0, 100.0, 150.0, 200.0]);
}

#[test]
fn an_area_off_the_page_is_an_error() {
    let pdf = fixture(0);
    let mut doc = PdfDocument::from_bytes(&pdf).unwrap();
    for area in [
        PageArea::Rect([400.0, 0.0, 500.0, 100.0]),
        PageArea::Rect([50.0, 50.0, 50.0, 100.0]),
        PageArea::Rect([f64::NAN, 0.0, 100.0, 100.0]),
    ] {
        doc.set_page_area(area);
        assert!(
            matches!(doc.page_size(0), Err(PdfError::EmptyPageArea { page: 0 })),
            "{area:?}: page_size"
        );
        assert!(
            matches!(
                doc.render_page(0, 72.0),
                Err(PdfError::EmptyPageArea { page: 0 })
            ),
            "{area:?}: render_page"
        );
    }
}
