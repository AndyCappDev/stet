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

/// Three pages in the shape of pdf.js's `boundingBox_invalid.pdf`, where the
/// first used to reach the PNG writer as a 0 x 0 image and panic there.
fn pages_with_boxes(boxes: &[&str]) -> Vec<u8> {
    let kids: Vec<String> = (0..boxes.len()).map(|i| format!("{} 0 R", i + 3)).collect();
    let mut objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>",
            kids.join(" "),
            boxes.len()
        )
        .into_bytes(),
    ];
    for b in boxes {
        objects.push(format!("<< /Type /Page /Parent 2 0 R {b} /Resources << >> >>").into_bytes());
    }
    pdf_from(&objects)
}

#[test]
fn a_media_box_with_no_area_becomes_us_letter() {
    let pdf = pages_with_boxes(&[
        "/MediaBox [0 0 0 0] /CropBox [0 0 0 0]",
        "/MediaBox [0 0 0 500]",
        "/MediaBox [0 0 0 0] /CropBox [100 100 300 400]",
        "/MediaBox [0 0 600 800]",
    ]);
    let mut doc = PdfDocument::from_bytes(&pdf).unwrap();
    assert_eq!(doc.page_size(0).unwrap(), (612.0, 792.0));
    assert_eq!(doc.page_size(1).unwrap(), (612.0, 792.0));
    // A usable CropBox is still honoured, inside the substituted MediaBox.
    assert_eq!(doc.page_area_rect(2).unwrap(), [100.0, 100.0, 300.0, 400.0]);
    assert_eq!(doc.page_size(3).unwrap(), (600.0, 800.0));

    let (rgba, w, h) = doc.render_page_to_rgba(0, 72.0).unwrap();
    assert_eq!((w, h, rgba.len()), (612, 792, 612 * 792 * 4));

    doc.set_page_area(PageArea::MediaBox);
    assert_eq!(doc.page_size(0).unwrap(), (612.0, 792.0));

    // One warning per replaced box: the MediaBox of pages 1 to 3 and the
    // CropBox of page 1. The sound page has none.
    let warned: Vec<usize> = doc
        .parse_warnings()
        .iter()
        .map(|w| match w.phase {
            stet_pdf_reader::ParsePhase::PageBoxes { page } => page,
            ref other => panic!("unexpected warning phase {other:?}"),
        })
        .collect();
    assert_eq!(warned, [0, 0, 1, 2]);
}

#[test]
fn a_crop_box_with_nothing_inside_the_media_box_is_the_media_box() {
    let pdf = pages_with_boxes(&[
        // Wholly outside, as in the pdf.js file.
        "/MediaBox [0 0 800 600] /CropBox [600 800 1000 1000]",
        // A line, not an area.
        "/MediaBox [0 0 800 600] /CropBox [10 10 10 500]",
        // Not finite once parsed.
        "/MediaBox [0 0 800 600] /CropBox [0 0 1e999 600]",
    ]);
    let doc = PdfDocument::from_bytes(&pdf).unwrap();
    for page in 0..2 {
        assert_eq!(doc.page_size(page).unwrap(), (800.0, 600.0), "page {page}");
        assert_eq!(
            doc.page_area_rect(page).unwrap(),
            [0.0, 0.0, 800.0, 600.0],
            "page {page}"
        );
    }
    // Whatever the reader makes of the third, it has a size that renders.
    let (w, h) = doc.page_size(2).unwrap();
    assert!(w > 0.0 && h > 0.0 && w.is_finite() && h.is_finite());
    assert!(doc.parse_warnings().len() >= 2);
}

/// Where `area` lies in a render of `base` at `scale` pixels per point, at
/// `rotate`: `device_rect` for any resolution, rounding each edge to the
/// nearest pixel (it is on the grid to within float error when this is used).
fn device_rect_at(
    base: [f64; 4],
    area: [f64; 4],
    rotate: i32,
    scale: f64,
) -> (usize, usize, usize, usize) {
    let [bx0, by0, bx1, by1] = base;
    let [ax0, ay0, ax1, ay1] = area;
    let (x0, x1, y0, y1) = match rotate {
        0 => (ax0 - bx0, ax1 - bx0, by1 - ay1, by1 - ay0),
        90 => (ay0 - by0, ay1 - by0, ax0 - bx0, ax1 - bx0),
        180 => (bx1 - ax1, bx1 - ax0, ay0 - by0, ay1 - by0),
        270 => (by1 - ay1, by1 - ay0, bx1 - ax1, bx1 - ax0),
        _ => unreachable!(),
    };
    let px = |v: f64| (v * scale).round() as usize;
    (px(x0), px(y0), px(x1) - px(x0), px(y1) - px(y0))
}

fn worst_difference(a: &[u8], b: &[u8]) -> u8 {
    a.iter()
        .zip(b)
        .map(|(x, y)| x.abs_diff(*y))
        .max()
        .unwrap_or(0)
}

/// An area whose edges fall between device pixels antialiases differently
/// from the page; the same area widened onto the page's pixel grid renders
/// the pixels a crop-box render has there, at every rotation.
#[test]
fn an_area_on_the_pixel_grid_renders_the_pages_pixels() {
    let dpi = 150.0;
    let scale = dpi / 72.0;
    let area = [100.3, 50.7, 250.6, 150.2];
    for rotate in [0, 90, 180, 270] {
        let pdf = fixture(rotate);
        let mut doc = PdfDocument::from_bytes(&pdf).unwrap();
        let (page, page_w, _) = doc.render_page_to_rgba(0, dpi).unwrap();

        doc.set_page_area(PageArea::Rect(area));
        let snapped = doc.page_area_rect_on_pixel_grid(0, dpi).unwrap();
        for i in 0..2 {
            assert!(
                snapped[i] <= area[i] && snapped[i + 2] >= area[i + 2],
                "/Rotate {rotate}: {snapped:?} does not cover {area:?}"
            );
            assert!(
                snapped[i + 2] - snapped[i] - (area[i + 2] - area[i]) < 2.0 / scale,
                "/Rotate {rotate}: widened by more than a pixel per side"
            );
        }

        doc.set_page_area(PageArea::Rect(snapped));
        let (rgba, w, h) = doc.render_page_to_rgba(0, dpi).unwrap();
        let at = device_rect_at(CROP, snapped, rotate, scale);
        assert_eq!(
            (w as usize, h as usize),
            (at.2, at.3),
            "/Rotate {rotate}: size"
        );
        // Within two levels: the f32 rounding `an_area_renders_the_pixels_it_covers_on_the_page`
        // describes, over the larger translations a 150-dpi render makes (at /Rotate 180, 6 of
        // 262,504 values differ by 2). Off the grid, edges differ by hundreds of levels.
        let worst = worst_difference(&rgba, &crop(&page, page_w, at));
        assert!(
            worst <= 2,
            "/Rotate {rotate}: a pixel differs from the page's by {worst} levels"
        );
    }
}

/// Snapping keeps an area's reach beyond the crop box. The grid is the crop
/// box's, but it does not end there: the bleed box on the grid is still the
/// bleed box, a pixel wider at most, and the page's pixels are where the
/// page has them. Clipped to the crop box instead, `--box bleed --box-snap`
/// rendered the crop box.
#[test]
fn an_area_beyond_the_crop_box_keeps_its_reach_on_the_grid() {
    let dpi = 150.0;
    let scale = dpi / 72.0;
    let probe = [100.3, 50.7, 250.6, 150.2];
    for rotate in [0, 90, 180, 270] {
        let pdf = fixture(rotate);
        let mut doc = PdfDocument::from_bytes(&pdf).unwrap();
        let (page, page_w, _) = doc.render_page_to_rgba(0, dpi).unwrap();
        doc.set_page_area(PageArea::Rect(probe));
        let probe = doc.page_area_rect_on_pixel_grid(0, dpi).unwrap();

        doc.set_page_area(PageArea::BleedBox);
        let snapped = doc.page_area_rect_on_pixel_grid(0, dpi).unwrap();
        for i in 0..2 {
            assert!(
                snapped[i] <= BLEED[i] && snapped[i + 2] >= BLEED[i + 2],
                "/Rotate {rotate}: {snapped:?} does not cover the bleed box"
            );
            assert!(
                snapped[i] >= MEDIA[i] && snapped[i + 2] <= MEDIA[i + 2],
                "/Rotate {rotate}: {snapped:?} leaves the MediaBox"
            );
        }

        doc.set_page_area(PageArea::Rect(snapped));
        let (rgba, w, _) = doc.render_page_to_rgba(0, dpi).unwrap();
        let worst = worst_difference(
            &crop(&rgba, w, device_rect_at(snapped, probe, rotate, scale)),
            &crop(&page, page_w, device_rect_at(CROP, probe, rotate, scale)),
        );
        assert!(
            worst <= 2,
            "/Rotate {rotate}: a pixel differs from the page's by {worst} levels"
        );
    }

    // The MediaBox bounds every area, so on the grid it is itself.
    let pdf = fixture(0);
    let mut doc = PdfDocument::from_bytes(&pdf).unwrap();
    doc.set_page_area(PageArea::MediaBox);
    assert_eq!(doc.page_area_rect_on_pixel_grid(0, dpi).unwrap(), MEDIA);
}

/// The check above is not vacuous: without the grid, the same area's pixels
/// differ from the page's by far more than f32 rounding.
#[test]
fn an_area_off_the_pixel_grid_does_not_render_the_pages_pixels() {
    let dpi = 150.0;
    let scale = dpi / 72.0;
    let pdf = fixture(0);
    let mut doc = PdfDocument::from_bytes(&pdf).unwrap();
    let (page, page_w, _) = doc.render_page_to_rgba(0, dpi).unwrap();
    let area = [100.3, 50.7, 250.6, 150.2];
    doc.set_page_area(PageArea::Rect(area));
    let (rgba, w, h) = doc.render_page_to_rgba(0, dpi).unwrap();
    // The nearest whole-pixel placement of the area on the page.
    let (x, y, _, _) = device_rect_at(CROP, area, 0, scale);
    let worst = worst_difference(&rgba, &crop(&page, page_w, (x, y, w as usize, h as usize)));
    assert!(
        worst > 16,
        "an off-grid area matched the page to within {worst} levels"
    );
}
