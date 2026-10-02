// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Image samples deeper than 8 bits, and colour-key masks, in the PDF reader.
//!
//! The display list carries 8-bit samples: a 16-bit sample becomes the
//! nearest 8-bit value, `v / 257` rounded, not its high byte. A colour-key
//! `/Mask` names samples as the file encodes them, at their own depth and
//! before `/Decode` (ISO 32000-1 §8.9.6.4). Where every sample the display
//! list carries is its encoded value expanded to 8 bits, the key is handed
//! to the renderer to test on them; otherwise the reader tests the encoded
//! samples and clears the masked pixels of the converted image.

use stet_graphics::device::ImageColorSpace;
use stet_graphics::display_list::DisplayElement;
use stet_pdf_reader::PdfDocument;

/// A one-page document painting the image XObject `image` (its dictionary
/// entries, without `/Type`, `/Subtype` or the stream keys) whose decoded
/// stream is `data`, plus any further objects it refers to as `4 0 R` on.
fn document(image: &str, data: &[u8], extra: &[String]) -> Vec<u8> {
    document_with(image, data, extra, "", "", "")
}

/// [`document`], with more page `resources`, content `before` the image,
/// and more `catalog` entries.
fn document_with(
    image: &str,
    data: &[u8],
    extra: &[String],
    resources: &str,
    before: &str,
    catalog: &str,
) -> Vec<u8> {
    let hex: String = data.iter().map(|b| format!("{b:02X}")).collect();
    let content = format!("{before} q 20 0 0 20 0 0 cm /Im0 Do Q");
    let mut objects = vec![
        format!("<< /Type /Catalog /Pages 2 0 R {catalog} >>"),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 20 20] \
             /Resources << /XObject << /Im0 {} 0 R >> {resources} >> /Contents {} 0 R >>",
            4 + extra.len(),
            5 + extra.len()
        ),
    ];
    objects.extend(extra.iter().cloned());
    objects.push(format!(
        "<< /Type /XObject /Subtype /Image {image} /Filter /ASCIIHexDecode \
         /Length {} >>\nstream\n{hex}>\nendstream",
        hex.len() + 1
    ));
    objects.push(format!(
        "<< /Length {} >>\nstream\n{content}\nendstream",
        content.len()
    ));

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

/// The page's display list.
fn elements(pdf: &[u8]) -> Vec<DisplayElement> {
    let doc = PdfDocument::from_bytes(pdf).unwrap();
    doc.render_page(0, 72.0).unwrap().elements().to_vec()
}

/// The painted image's samples, colour space and colour key, and its soft
/// mask's alpha if it has one.
struct Painted {
    samples: Vec<u8>,
    cs: ImageColorSpace,
    key: Option<Vec<u8>>,
    soft_mask: Option<Vec<u8>>,
}

fn painted(pdf: &[u8]) -> Painted {
    let elements = elements(pdf);
    let [element] = elements.as_slice() else {
        panic!("one element expected, got {elements:?}");
    };
    let only_image = |e: &DisplayElement| match e {
        DisplayElement::Image {
            sample_data,
            params,
        } => Painted {
            samples: sample_data.to_vec(),
            cs: params.color_space.clone(),
            key: params.mask_color.clone(),
            soft_mask: None,
        },
        other => panic!("an image expected, got {other:?}"),
    };
    match element {
        DisplayElement::SoftMasked { mask, content, .. } => Painted {
            soft_mask: Some(only_image(&mask.elements()[0]).samples),
            ..only_image(&content.elements()[0])
        },
        e => only_image(e),
    }
}

/// Which pixels the page leaves unpainted, as the renderer decides: by the
/// colour key on the samples it is given, or by the alpha of samples
/// converted already.
fn masked(pdf: &[u8]) -> Vec<bool> {
    let Painted {
        samples, cs, key, ..
    } = painted(pdf);
    if let ImageColorSpace::PreconvertedRGBA = cs {
        assert_eq!(key, None);
        return samples
            .as_chunks::<4>()
            .0
            .iter()
            .map(|px| px[3] == 0)
            .collect();
    }
    let n = cs.num_components() as usize;
    samples
        .chunks_exact(n)
        .map(|px| {
            key.as_ref().is_some_and(|key| {
                assert_eq!(key.len(), 2 * n, "a range per component");
                px.iter()
                    .enumerate()
                    .all(|(c, &s)| key[2 * c] <= s && s <= key[2 * c + 1])
            })
        })
        .collect()
}

/// A 2×1 DeviceGray image of `bpc` bits whose samples are `data`, with
/// the colour key `mask` and any other entries `more`.
fn gray(bpc: u32, data: &[u8], mask: &str, more: &str) -> Vec<u8> {
    document(
        &format!(
            "/Width 2 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent {bpc} \
             /Mask {mask} {more}"
        ),
        data,
        &[],
    )
}

// ------------------------------------------------------------- rounding

/// 0x10FF is 16.93 levels: 17 to the nearest, 16 by its high byte.
#[test]
fn sixteen_bit_samples_are_rounded() {
    let pdf = document(
        "/Width 4 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 16",
        &[0x10, 0xFF, 0x81, 0x00, 0x81, 0x01, 0xFF, 0xFF],
        &[],
    );
    assert_eq!(painted(&pdf).samples, [0x11, 0x80, 0x81, 0xFF]);
}

#[test]
fn sixteen_bit_soft_masks_are_rounded() {
    let smask = "<< /Type /XObject /Subtype /Image /Width 2 /Height 1 /ColorSpace /DeviceGray \
                 /BitsPerComponent 16 /Filter /ASCIIHexDecode /Length 9 >>\nstream\n10FF8101>\nendstream";
    let pdf = document(
        "/Width 2 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /SMask 4 0 R",
        &[0, 0],
        &[smask.to_string()],
    );
    assert_eq!(painted(&pdf).soft_mask.unwrap(), [0x11, 0x81]);
}

/// A palette index is never scaled.
#[test]
fn indexed_samples_are_not_scaled() {
    let pdf = document(
        "/Width 2 /Height 1 /ColorSpace [/Indexed /DeviceGray 15 <000102030405060708090A0B0C0D0E0F>] \
         /BitsPerComponent 4",
        &[0x3F],
        &[],
    );
    let Painted { samples, cs, .. } = painted(&pdf);
    assert!(matches!(cs, ImageColorSpace::Indexed { .. }));
    assert_eq!(samples, [3, 15]);
}

// ---------------------------------------------------------- colour keys

/// The key is at the image's own depth: `[15 15]` is a 4-bit sample's
/// maximum, `[1 1]` a 1-bit one's, not the 8-bit values they expand to.
#[test]
fn colour_keys_name_samples_at_their_depth() {
    for (bpc, data, mask) in [
        (1, &[0b1000_0000][..], "[1 1]"),
        (2, &[0b1101_0000], "[3 3]"),
        (4, &[0xF0], "[15 15]"),
        (8, &[200, 50], "[200 200]"),
        (16, &[0x80, 0x40, 0x10, 0x00], "[32768 33023]"),
    ] {
        assert_eq!(
            masked(&gray(bpc, data, mask, "")),
            [true, false],
            "{bpc} bits"
        );
    }
}

/// A 16-bit key is not cut to 8 bits: `33023 as u8` is 255, which made
/// `[32768 33023]` take in every sample.
#[test]
fn sixteen_bit_colour_keys_keep_their_range() {
    let pdf = gray(16, &[0x80, 0x40, 0x1F, 0xFF], "[32768 33023]", "");
    assert_eq!(masked(&pdf), [true, false]);
    // Both 128 at 8 bits; only the first is in the key.
    let pdf = gray(16, &[0x80, 0xFF, 0x81, 0x00], "[32768 33023]", "");
    assert_eq!(masked(&pdf), [true, false]);
}

/// The key names samples before `/Decode`.
#[test]
fn colour_keys_name_samples_before_decode() {
    let pdf = gray(8, &[200, 55], "[200 200]", "/Decode [1 0]");
    assert_eq!(masked(&pdf), [true, false]);
    let pdf = gray(4, &[0xF0], "[15 15]", "/Decode [1 0]");
    assert_eq!(masked(&pdf), [true, false]);
}

#[test]
fn colour_keys_on_indexed_images_name_indices() {
    let pdf = document(
        "/Width 2 /Height 1 /ColorSpace [/Indexed /DeviceRGB 3 <FF000000FF000000FFFFFFFF>] \
         /BitsPerComponent 2 /Mask [3 3]",
        &[0b1101_0000],
        &[],
    );
    assert_eq!(masked(&pdf), [true, false]);
}

/// Every component must be in range for a pixel to be masked. An 8-bit
/// key stays the renderer's to test, on the samples as they are.
#[test]
fn colour_keys_need_every_component() {
    let pdf = document(
        "/Width 3 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8 \
         /Mask [10 20 0 0 100 255]",
        &[15, 0, 200, 15, 1, 200, 25, 0, 200],
        &[],
    );
    assert_eq!(masked(&pdf), [true, false, false]);
    let Painted {
        samples, cs, key, ..
    } = painted(&pdf);
    assert!(matches!(cs, ImageColorSpace::DeviceRGB));
    assert_eq!(samples, [15, 0, 200, 15, 1, 200, 25, 0, 200]);
    assert_eq!(key.unwrap(), [10, 20, 0, 0, 100, 255]);
}

/// Up to 8 bits the key reaches the renderer reduced as the samples are.
#[test]
fn shallow_colour_keys_are_expanded_with_their_samples() {
    let Painted { samples, key, .. } = painted(&gray(4, &[0xF0], "[15 15]", ""));
    assert_eq!(samples, [255, 0]);
    assert_eq!(key.unwrap(), [255, 255]);
}

/// A multi-input DeviceN image is converted while the page is read; its
/// key is still tested on the encoded samples.
#[test]
fn colour_keys_on_converted_images() {
    let tint = "<< /FunctionType 4 /Domain [0 1 0 1] /Range [0 1] /Length 13 >>\n\
                stream\n{add 2 div}\nendstream";
    let pdf = document(
        "/Width 2 /Height 1 /ColorSpace [/DeviceN [/A /B] /DeviceGray 4 0 R] \
         /BitsPerComponent 8 /Mask [0 0 0 0]",
        &[0, 0, 255, 255],
        &[tint.to_string()],
    );
    let Painted { samples, cs, .. } = painted(&pdf);
    assert!(matches!(cs, ImageColorSpace::PreconvertedRGBA));
    assert_eq!(samples, [0, 0, 0, 0, 255, 255, 255, 255]);
}

/// A transfer function applies to the colour the renderer paints, not to
/// the samples, so a key under one stays a key on the samples as encoded.
#[test]
fn colour_keys_under_a_transfer_function() {
    let pdf = document_with(
        "/Width 2 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8 \
         /Mask [200 200 200 200 200 200]",
        &[200, 200, 200, 55, 55, 55],
        &[],
        "/ExtGState << /GS0 << /TR << /FunctionType 2 /Domain [0 1] \
         /C0 [1] /C1 [0] /N 1 >> >> >>",
        "/GS0 gs",
        "",
    );
    let Painted { samples, cs, .. } = painted(&pdf);
    assert!(matches!(cs, ImageColorSpace::DeviceRGB));
    assert_eq!(samples, [200, 200, 200, 55, 55, 55]);
    assert_eq!(masked(&pdf), [true, false]);
}

/// Under a CMYK output intent a DeviceGray image is painted as K, which
/// changes its samples, so the reader applies the key.
#[test]
fn colour_keys_on_gray_painted_as_k() {
    let profile = include_bytes!("../../stet-graphics/tests/data/cmyk_intent/inklimit.icc");
    let hex: String = profile.iter().map(|b| format!("{b:02X}")).collect();
    let profile = format!(
        "<< /N 4 /Filter /ASCIIHexDecode /Length {} >>\nstream\n{hex}>\nendstream",
        hex.len() + 1
    );
    let pdf = document_with(
        "/Width 2 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Mask [200 200]",
        &[200, 55],
        &[profile],
        "",
        "",
        "/OutputIntents [<< /Type /OutputIntent /S /GTS_PDFX \
         /OutputConditionIdentifier (Test) /DestOutputProfile 4 0 R >>]",
    );
    let Painted { cs, .. } = painted(&pdf);
    assert!(matches!(cs, ImageColorSpace::PreconvertedRGBA));
    assert_eq!(masked(&pdf), [true, false]);
}

/// A range past the depth is clipped to it, not wrapped.
#[test]
fn colour_key_ranges_are_clipped_to_the_depth() {
    let pdf = gray(8, &[0, 255], "[0 300]", "");
    assert_eq!(masked(&pdf), [true, true]);
    let pdf = gray(8, &[44, 255], "[300 300]", "");
    assert_eq!(masked(&pdf), [false, false]);
}

/// A key that masks nothing leaves the image as it was.
#[test]
fn colour_keys_that_mask_nothing_are_dropped() {
    let Painted { cs, key, .. } = painted(&gray(8, &[1, 2], "[200 200]", ""));
    assert!(matches!(cs, ImageColorSpace::DeviceGray));
    assert_eq!(key, None);
    let Painted { cs, key, .. } = painted(&gray(16, &[0, 1, 0, 2], "[200 200]", ""));
    assert!(matches!(cs, ImageColorSpace::DeviceGray));
    assert_eq!(key, None);
}

#[test]
fn colour_keys_may_be_indirect() {
    let pdf = document(
        "/Width 2 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Mask 4 0 R",
        &[200, 50],
        &["[200 200]".to_string()],
    );
    assert_eq!(masked(&pdf), [true, false]);
}

/// An `/SMask` overrides `/Mask` (ISO 32000-1 Table 89).
#[test]
fn soft_masks_override_colour_keys() {
    let smask = "<< /Type /XObject /Subtype /Image /Width 2 /Height 1 /ColorSpace /DeviceGray \
                 /BitsPerComponent 8 /Filter /ASCIIHexDecode /Length 5 >>\nstream\n8040>\nendstream";
    let pdf = document(
        "/Width 2 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 \
         /Mask [200 200] /SMask 4 0 R",
        &[200, 50],
        &[smask.to_string()],
    );
    let Painted { key, soft_mask, .. } = painted(&pdf);
    assert_eq!(soft_mask.unwrap(), [0x80, 0x40]);
    assert_eq!(key, None);
}
