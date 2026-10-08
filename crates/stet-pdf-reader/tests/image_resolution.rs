// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `ImageResolution`: a display list keeps every image at its stored
//! resolution unless the caller says the list is for one rasterisation —
//! and `render_page_to_rgba`, which rasterises at once, says so itself.
//!
//! The fixture is a 512 x 384 JPEG 2000 photo-like image with six
//! resolution levels (512, 256, 128, 64, 32, 16 wide), written by OpenJPEG
//! through Pillow. Pages are 512 x 384 pt, so at 72 dpi a full-page
//! placement draws it one image sample to the pixel.

#![cfg(all(feature = "render", feature = "jpx"))]

use stet_graphics::display_list::DisplayElement;
use stet_graphics::layer_set::LayerSet;
use stet_pdf_reader::{ImageResolution, PageBackground, PdfDocument};

const PHOTO: &[u8] = include_bytes!("data/jpx/photo-512x384.jp2");
const STORED: (u32, u32) = (512, 384);

fn stream(dict: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!("<< {dict} /Length {} >>\nstream\n", body.len()).into_bytes();
    out.extend_from_slice(body);
    out.extend_from_slice(b"\nendstream");
    out
}

/// A 512 x 384 pt page running `content`, with the photo as `/Im`
/// (object 5, its dictionary extended by `image_extra`), object 6 as
/// `/Im`'s companion if `extra` gives one, and `resources` added to the
/// page's.
fn page(content: &str, image_extra: &str, resources: &str, extra: Option<Vec<u8>>) -> Vec<u8> {
    let mut objects: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 512 384] /Contents 4 0 R \
             /Resources << /XObject << /Im 5 0 R >> {resources} >> >>"
        )
        .into_bytes(),
        stream("", content.as_bytes()),
        stream(
            &format!(
                "/Type /XObject /Subtype /Image /Width 512 /Height 384 \
                 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /JPXDecode {image_extra}"
            ),
            PHOTO,
        ),
    ];
    objects.extend(extra);
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

/// The photo over the whole page.
const FULL_PAGE: &str = "q 512 0 0 384 0 0 cm /Im Do Q";

/// The sizes of the images in a page's display list, in drawing order,
/// looking inside groups and soft masks.
fn image_sizes(pdf: &[u8], dpi: f64, resolution: ImageResolution) -> Vec<(u32, u32)> {
    fn walk(elements: &[DisplayElement], out: &mut Vec<(u32, u32)>) {
        for element in elements {
            match element {
                DisplayElement::Image { params, .. } => out.push((params.width, params.height)),
                DisplayElement::Group { elements, .. } => walk(elements.elements(), out),
                DisplayElement::OcgGroup { elements, .. } => walk(elements.elements(), out),
                DisplayElement::PatternFill { params } => walk(params.tile.elements(), out),
                DisplayElement::SoftMasked { mask, content, .. } => {
                    walk(mask.elements(), out);
                    walk(content.elements(), out);
                }
                _ => {}
            }
        }
    }
    let mut doc = PdfDocument::from_bytes(pdf).expect("fixture parses");
    doc.set_image_resolution(resolution);
    assert_eq!(doc.image_resolution(), resolution);
    let list = doc.render_page(0, dpi).expect("page renders");
    let mut sizes = Vec::new();
    walk(list.elements(), &mut sizes);
    sizes
}

/// The page rasterised from a list that keeps images at full resolution.
fn rasterised_from_full(pdf: &[u8], dpi: f64) -> (Vec<u8>, u32, u32) {
    let doc = PdfDocument::from_bytes(pdf).expect("fixture parses");
    assert_eq!(doc.image_resolution(), ImageResolution::Full);
    let (w, h) = doc.page_size(0).unwrap();
    let (w, h) = (
        (w * dpi / 72.0).round() as u32,
        (h * dpi / 72.0).round() as u32,
    );
    let list = doc.render_page(0, dpi).unwrap();
    let rgba = stet_render::render_to_rgba_with_background(
        &list,
        w,
        h,
        dpi,
        Some(doc.icc_cache()),
        false,
        &LayerSet::new(),
        PageBackground::White,
    );
    (rgba, w, h)
}

/// The largest and the mean difference between two renders, over the
/// colour channels.
fn difference(a: &[u8], b: &[u8]) -> (u8, f64) {
    assert_eq!(a.len(), b.len());
    let (mut worst, mut total) = (0u8, 0u64);
    for (pa, pb) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0) {
        for c in 0..3 {
            let d = pa[c].abs_diff(pb[c]);
            worst = worst.max(d);
            total += u64::from(d);
        }
    }
    (worst, total as f64 / (a.len() / 4 * 3) as f64)
}

#[test]
fn a_display_list_keeps_full_resolution_whatever_it_is_built_for() {
    let pdf = page(FULL_PAGE, "", "", None);
    // 9 dpi draws the photo 64 pixels wide; the list still holds all 512.
    for dpi in [9.0, 18.0, 72.0, 300.0] {
        assert_eq!(image_sizes(&pdf, dpi, ImageResolution::Full), [STORED]);
    }
}

#[test]
fn for_one_rasterisation_an_image_is_decoded_at_the_level_that_covers_it() {
    let pdf = page(FULL_PAGE, "", "", None);
    let rendered = |dpi| image_sizes(&pdf, dpi, ImageResolution::Rendered);
    // Drawn 512 or more wide: all of it.
    assert_eq!(rendered(300.0), [STORED]);
    assert_eq!(rendered(72.0), [STORED]);
    // Drawn 300 wide: the half-size level would fall short.
    assert_eq!(rendered(72.0 * 300.0 / 512.0), [STORED]);
    // Drawn exactly half or a quarter: exactly that level.
    assert_eq!(rendered(36.0), [(256, 192)]);
    assert_eq!(rendered(18.0), [(128, 96)]);
    // Drawn 100 wide: the 128 level covers it, the 64 level does not.
    assert_eq!(rendered(72.0 * 100.0 / 512.0), [(128, 96)]);
    // Drawn an eighth, 64 wide: the 64 level would fit, but its picture
    // sits 3 stored samples off, which is 3/8 of a pixel here. The 128
    // level is 1 sample off, an eighth of a pixel.
    assert_eq!(rendered(9.0), [(128, 96)]);
    // Drawn 40 wide: 3 samples is now under a quarter of a pixel.
    assert_eq!(rendered(72.0 * 40.0 / 512.0), [(64, 48)]);
    // Drawn 16 wide, a thirty-second.
    assert_eq!(rendered(72.0 * 16.0 / 512.0), [(32, 24)]);
}

/// The reduced decode is a different route to the same picture: the
/// wavelet's own smaller image, where the full route averages blocks of
/// the full-size one. The two filters differ, and the smaller image may sit
/// up to a quarter of a pixel off, so the renders are not identical — and
/// on this fixture, hard-edged shapes drawn only 64 to 256 pixels wide,
/// that shows as much as it ever will. They must still be the same picture.
#[test]
fn render_page_to_rgba_decodes_for_its_size_and_draws_the_same_picture() {
    let pdf = page(FULL_PAGE, "", "", None);
    let doc = PdfDocument::from_bytes(&pdf).unwrap();
    for dpi in [36.0, 18.0, 9.0] {
        let (full, w, h) = rasterised_from_full(&pdf, dpi);
        let (reduced, rw, rh) = doc.render_page_to_rgba(0, dpi).unwrap();
        assert_eq!((rw, rh), (w, h));
        let (worst, mean) = difference(&full, &reduced);
        assert!(mean < 4.0, "{dpi} dpi: mean difference {mean}");
        assert!(worst > 0, "{dpi} dpi: identical, so nothing was reduced");
    }
    // Where nothing can be saved, nothing changes.
    let (full, ..) = rasterised_from_full(&pdf, 72.0);
    let (same, ..) = doc.render_page_to_rgba(0, 72.0).unwrap();
    assert_eq!(full, same);
}

/// The setting is the document's and is not disturbed by a call that
/// overrides it.
#[test]
fn render_page_to_rgba_leaves_the_setting_alone() {
    let pdf = page(FULL_PAGE, "", "", None);
    let doc = PdfDocument::from_bytes(&pdf).unwrap();
    doc.render_page_to_rgba(0, 9.0).unwrap();
    assert_eq!(doc.image_resolution(), ImageResolution::Full);
    assert_eq!(image_sizes(&pdf, 9.0, ImageResolution::Full), [STORED]);
}

/// An image decoded small for one placement is no use to a larger one on
/// the same page, in either order.
#[test]
fn each_placement_gets_the_resolution_it_needs() {
    let small_then_large = "q 64 0 0 48 0 0 cm /Im Do Q q 512 0 0 384 0 0 cm /Im Do Q";
    let large_then_small = "q 512 0 0 384 0 0 cm /Im Do Q q 64 0 0 48 0 0 cm /Im Do Q";
    let sizes = |content| {
        image_sizes(
            &page(content, "", "", None),
            72.0,
            ImageResolution::Rendered,
        )
    };
    assert_eq!(sizes(small_then_large), [(128, 96), STORED]);
    // The full decode is already in hand, and serves the small one too.
    assert_eq!(sizes(large_then_small), [STORED, STORED]);
    // Small, then one the same decode covers: decoded once.
    assert_eq!(
        sizes("q 64 0 0 48 0 0 cm /Im Do Q q 120 0 0 90 100 100 cm /Im Do Q"),
        [(128, 96), (128, 96)]
    );
    // Small, then larger but still under full size: decoded again, larger.
    assert_eq!(
        sizes("q 64 0 0 48 0 0 cm /Im Do Q q 200 0 0 150 100 100 cm /Im Do Q"),
        [(128, 96), (256, 192)]
    );
}

/// The size an image is drawn at is the length of the CTM's axes, so a
/// rotated placement is measured along its own sides.
#[test]
fn a_rotated_image_is_measured_along_its_own_sides() {
    // 128 x 96 on the page, turned through 90 degrees.
    let turned = "q 0 128 -96 0 300 100 cm /Im Do Q";
    assert_eq!(
        image_sizes(&page(turned, "", "", None), 72.0, ImageResolution::Rendered),
        [(128, 96)]
    );
}

/// A soft mask comes down with its image: averaged, so that alpha stays
/// alpha, rather than the image being enlarged back to meet it.
#[test]
fn a_soft_masked_image_is_reduced_with_its_mask() {
    // Opaque on the left half, clear on the right.
    let mut alpha = Vec::with_capacity(512 * 384);
    for _ in 0..384 {
        alpha.extend(std::iter::repeat_n(255u8, 256));
        alpha.extend(std::iter::repeat_n(0u8, 256));
    }
    let mask = stream(
        "/Type /XObject /Subtype /Image /Width 512 /Height 384 \
         /ColorSpace /DeviceGray /BitsPerComponent 8",
        &alpha,
    );
    let pdf = page(FULL_PAGE, "/SMask 6 0 R", "", Some(mask));
    // The image and its mask are both in the list, and both came down.
    let sizes = image_sizes(&pdf, 18.0, ImageResolution::Rendered);
    assert!(
        !sizes.is_empty() && sizes.iter().all(|&s| s == (128, 96)),
        "{sizes:?}"
    );
    let sizes = image_sizes(&pdf, 18.0, ImageResolution::Full);
    assert!(
        !sizes.is_empty() && sizes.iter().all(|&s| s == STORED),
        "{sizes:?}"
    );

    let doc = PdfDocument::from_bytes(&pdf).unwrap();
    let (full, w, h) = rasterised_from_full(&pdf, 18.0);
    let (reduced, ..) = doc.render_page_to_rgba(0, 18.0).unwrap();
    let (_, mean) = difference(&full, &reduced);
    assert!(mean < 4.0, "mean difference {mean}");
    // The right half is the white page showing through in both.
    let at = |rgba: &[u8], x: u32, y: u32| {
        let i = ((y * w + x) * 4) as usize;
        [rgba[i], rgba[i + 1], rgba[i + 2]]
    };
    assert_eq!(at(&reduced, w * 3 / 4, h / 2), [255, 255, 255]);
    assert_ne!(at(&reduced, w / 4, h / 8), [255, 255, 255]);
}

/// A colour key picks out exact sample values, and a reduced decode has
/// none of the originals: such an image is decoded in full.
#[test]
fn a_colour_keyed_image_is_left_at_full_resolution() {
    let pdf = page(FULL_PAGE, "/Mask [250 255 250 255 250 255]", "", None);
    assert_eq!(image_sizes(&pdf, 9.0, ImageResolution::Rendered), [STORED]);
}

/// A pattern cell is drawn at whatever scale each use of the pattern
/// gives it, which the image inside cannot see: it keeps full resolution.
#[test]
fn an_image_in_a_tiling_pattern_is_left_at_full_resolution() {
    let cell = "q 64 0 0 48 0 0 cm /Im Do Q";
    let pattern = stream(
        "/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 64 48] \
         /XStep 64 /YStep 48 /Resources << /XObject << /Im 5 0 R >> >>",
        cell.as_bytes(),
    );
    let pdf = page(
        "/Pattern cs /P scn 0 0 512 384 re f",
        "",
        "/Pattern << /P 6 0 R >>",
        Some(pattern),
    );
    let sizes = image_sizes(&pdf, 72.0, ImageResolution::Rendered);
    assert!(!sizes.is_empty(), "the pattern's image is in the list");
    assert!(sizes.iter().all(|&s| s == STORED), "{sizes:?}");
}

// --- The application's ceiling on image size --------------------------------

/// Render `pdf` with a ceiling of `limit` samples; the sizes of the images
/// drawn, and the content warnings.
fn with_limit(
    pdf: &[u8],
    dpi: f64,
    resolution: ImageResolution,
    limit: Option<u64>,
) -> (Vec<(u32, u32)>, Vec<String>) {
    fn walk(elements: &[DisplayElement], out: &mut Vec<(u32, u32)>) {
        for element in elements {
            match element {
                DisplayElement::Image { params, .. } => out.push((params.width, params.height)),
                DisplayElement::Group { elements, .. } => walk(elements.elements(), out),
                DisplayElement::SoftMasked { mask, content, .. } => {
                    walk(mask.elements(), out);
                    walk(content.elements(), out);
                }
                _ => {}
            }
        }
    }
    let mut doc = PdfDocument::from_bytes(pdf).unwrap();
    doc.set_image_resolution(resolution);
    doc.set_max_image_pixels(limit);
    assert_eq!(doc.max_image_pixels(), limit);
    let list = doc.render_page(0, dpi).unwrap();
    let mut sizes = Vec::new();
    walk(list.elements(), &mut sizes);
    let warnings = doc
        .parse_warnings()
        .iter()
        .map(|w| w.message.clone())
        .collect();
    (sizes, warnings)
}

/// The photo is 196,608 samples. Under a ceiling below that it is left
/// out, and the page says why; with none, or one above, it is drawn.
#[test]
fn an_image_over_the_ceiling_is_left_out_with_a_warning() {
    let pdf = page(FULL_PAGE, "", "", None);

    let (sizes, warnings) = with_limit(&pdf, 72.0, ImageResolution::Full, Some(100_000));
    assert!(sizes.is_empty(), "{sizes:?}");
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].contains("512 x 384") && warnings[0].contains("limit of 100000"),
        "{warnings:?}"
    );

    for limit in [None, Some(196_608), Some(1_000_000)] {
        let (sizes, warnings) = with_limit(&pdf, 72.0, ImageResolution::Full, limit);
        assert_eq!(sizes, [STORED], "{limit:?}");
        assert!(warnings.is_empty(), "{limit:?}: {warnings:?}");
    }
}

/// What counts is the size decoded. Drawn at a quarter of its size and
/// decoded for that, the photo is 128 x 96 and passes a ceiling its stored
/// size does not.
#[test]
fn a_reduced_decode_is_measured_at_its_reduced_size() {
    let pdf = page("q 128 0 0 96 0 0 cm /Im Do Q", "", "", None);

    let (sizes, warnings) = with_limit(&pdf, 72.0, ImageResolution::Rendered, Some(100_000));
    assert_eq!(sizes, [(128, 96)]);
    assert!(warnings.is_empty(), "{warnings:?}");

    // The same page at the stored resolution is over it.
    let (sizes, warnings) = with_limit(&pdf, 72.0, ImageResolution::Full, Some(100_000));
    assert!(sizes.is_empty(), "{sizes:?}");
    assert_eq!(warnings.len(), 1, "{warnings:?}");
}

/// A ceiling above the built-in limit is the built-in limit, which is to
/// say none.
#[test]
fn a_ceiling_above_the_built_in_limit_is_no_ceiling() {
    let pdf = page(FULL_PAGE, "", "", None);
    let mut doc = PdfDocument::from_bytes(&pdf).unwrap();
    assert_eq!(doc.max_image_pixels(), None);
    doc.set_max_image_pixels(Some(u64::MAX));
    assert_eq!(doc.max_image_pixels(), None);
    doc.set_max_image_pixels(Some(1_000));
    assert_eq!(doc.max_image_pixels(), Some(1_000));
    doc.set_max_image_pixels(None);
    assert_eq!(doc.max_image_pixels(), None);
}

/// An image that is not JPEG 2000 is as large as its dictionary says, and
/// so is a soft mask: one over the ceiling takes its image with it, since
/// the image without its mask would be the wrong picture.
#[test]
fn the_ceiling_covers_other_images_and_soft_masks() {
    // Object 6: a 400 x 400 grey image, run-length coded (0x81 0x00 is "two
    // zeros"; 80,000 of them make 160,000 samples).
    let grey = stream(
        "/Type /XObject /Subtype /Image /Width 400 /Height 400 \
         /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /RunLengthDecode",
        &[0x81, 0x00].repeat(80_000),
    );
    let plain = page(
        "q 100 0 0 100 0 0 cm /G Do Q",
        "",
        "/XObject << /G 6 0 R >>",
        Some(grey.clone()),
    );
    let (sizes, warnings) = with_limit(&plain, 72.0, ImageResolution::Full, Some(100_000));
    assert!(sizes.is_empty(), "{sizes:?}");
    assert!(
        warnings.iter().any(|w| w.contains("400 x 400")),
        "{warnings:?}"
    );
    let (sizes, _) = with_limit(&plain, 72.0, ImageResolution::Full, None);
    assert_eq!(sizes, [(400, 400)]);

    // The photo, drawn small enough to be decoded at 128 x 96, with that
    // 400 x 400 image as its soft mask.
    let masked = page(
        "q 128 0 0 96 0 0 cm /Im Do Q",
        "/SMask 6 0 R",
        "",
        Some(grey),
    );
    let (sizes, warnings) = with_limit(&masked, 72.0, ImageResolution::Rendered, Some(100_000));
    assert!(sizes.is_empty(), "{sizes:?}");
    assert!(
        warnings.iter().any(|w| w.contains("400 x 400")),
        "{warnings:?}"
    );
    let (sizes, _) = with_limit(&masked, 72.0, ImageResolution::Rendered, Some(200_000));
    // The image and its mask, both at the reduced size.
    assert_eq!(sizes, [(128, 96), (128, 96)]);
}
