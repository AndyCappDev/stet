// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! JPEG images kept as their JPEG data (`InterpreterBuilder::keep_jpeg_data`),
//! for a consumer that writes JPEG out as it is.

use std::sync::Arc;

use stet::{DisplayElement, Interpreter, PsDisplayList as DisplayList};
use stet_graphics::device::ImageColorSpace;

/// A 16 x 8 RGB JPEG with a JFIF marker.
const RGB: &str = "ffd8ffe000104a46494600010100000100010000ffdb0043000302020302020303030304030304050805050404050a070706080c0a0c0c0b0a0b0b0d0e12100d0e110e0b0b1016101113141515150c0f171816141812141514ffdb00430103040405040509050509140d0b0d1414141414141414141414141414141414141414141414141414141414141414141414141414141414141414141414141414ffc00011080008001003012200021101031101ffc4001f0000010501010101010100000000000000000102030405060708090a0bffc400b5100002010303020403050504040000017d01020300041105122131410613516107227114328191a1082342b1c11552d1f02433627282090a161718191a25262728292a3435363738393a434445464748494a535455565758595a636465666768696a737475767778797a838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9cad2d3d4d5d6d7d8d9dae1e2e3e4e5e6e7e8e9eaf1f2f3f4f5f6f7f8f9faffc4001f0100030101010101010101010000000000000102030405060708090a0bffc400b51100020102040403040705040400010277000102031104052131061241510761711322328108144291a1b1c109233352f0156272d10a162434e125f11718191a262728292a35363738393a434445464748494a535455565758595a636465666768696a737475767778797a82838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9cad2d3d4d5d6d7d8d9dae2e3e4e5e6e7e8e9eaf2f3f4f5f6f7f8f9faffda000c03010002110311003f00f2df09feca9f73fd0fff001daf66f09feca9f73fd0ff00f1da28af1b39e27ccf5fde1f23e1b718e71ee7ef7b7f5b9fffd9";
/// An 8 x 8 greyscale JPEG.
const GRAY: &str = "ffd8ffe000104a46494600010100000100010000ffdb0043000302020302020303030304030304050805050404050a070706080c0a0c0c0b0a0b0b0d0e12100d0e110e0b0b1016101113141515150c0f171816141812141514ffc0000b080008000801011100ffc4001f0000010501010101010100000000000000000102030405060708090a0bffc400b5100002010303020403050504040000017d01020300041105122131410613516107227114328191a1082342b1c11552d1f02433627282090a161718191a25262728292a3435363738393a434445464748494a535455565758595a636465666768696a737475767778797a838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9cad2d3d4d5d6d7d8d9dae1e2e3e4e5e6e7e8e9eaf1f2f3f4f5f6f7f8f9faffda0008010100003f00e53fe09b7ff30bff0080d7ffd9";

fn bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

fn list(body: &str, keep: bool) -> DisplayList {
    let src = format!("%!PS-Adobe-3.0\n{body}\nshowpage\n");
    let builder = Interpreter::builder();
    let builder = if keep {
        builder.keep_jpeg_data()
    } else {
        builder
    };
    let mut pages = builder
        .build()
        .render_to_display_list(src.as_bytes(), 72.0)
        .unwrap();
    assert_eq!(pages.len(), 1);
    pages.remove(0).display_list
}

/// The images in a list: samples, colour space, JPEG data.
#[allow(clippy::type_complexity)]
fn images(list: &DisplayList) -> Vec<(Arc<Vec<u8>>, ImageColorSpace, Option<Arc<Vec<u8>>>)> {
    list.elements()
        .iter()
        .filter_map(|e| match e {
            DisplayElement::Image {
                sample_data,
                params,
            } => Some((
                sample_data.clone(),
                params.color_space.clone(),
                params.jpeg.clone(),
            )),
            _ => None,
        })
        .collect()
}

fn fills(list: &DisplayList) -> usize {
    list.elements()
        .iter()
        .filter(|e| matches!(e, DisplayElement::Fill { .. }))
        .count()
}

/// A JPEG image read through `currentfile`, as EPS writers inline them,
/// with a fill after it so the file is read on from the right place.
fn inline(space: &str, w: u32, h: u32, decode: &str, hex: &str) -> String {
    format!(
        "/{space} setcolorspace 100 100 scale\n\
         << /ImageType 1 /Width {w} /Height {h} /BitsPerComponent 8 /Decode [{decode}]\n\
         /ImageMatrix [{w} 0 0 {h} 0 0] /DataSource currentfile /ASCIIHexDecode filter /DCTDecode filter >>\n\
         image\n{hex}>\n0 0 0.5 0.5 rectfill"
    )
}

#[test]
fn a_jpeg_image_is_kept_as_its_jpeg_data() {
    let body = inline("DeviceRGB", 16, 8, "0 1 0 1 0 1", RGB);
    let kept = list(&body, true);
    let [(samples, cs, jpeg)] = images(&kept).try_into().expect("one image");
    assert!(samples.is_empty());
    assert!(matches!(cs, ImageColorSpace::DeviceRGB));
    assert_eq!(jpeg.as_deref(), Some(&bytes(RGB)));
    assert_eq!(fills(&kept), 1, "the file is read on after the image");

    // Decoded, it is the image the interpreter paints without keeping JPEG.
    let mut decoded = kept.clone();
    decoded.decode_kept_jpeg();
    let [(samples, _, jpeg)] = images(&decoded).try_into().expect("one image");
    let [(plain, _, none)] = images(&list(&body, false)).try_into().expect("one image");
    assert_eq!(jpeg, None);
    assert_eq!(none, None);
    assert_eq!(samples.len(), 16 * 8 * 3);
    assert_eq!(samples, plain);
}

#[test]
fn a_greyscale_jpeg_is_kept() {
    let kept = list(&inline("DeviceGray", 8, 8, "0 1", GRAY), true);
    let [(samples, cs, jpeg)] = images(&kept).try_into().expect("one image");
    assert!(samples.is_empty() && jpeg.is_some());
    assert!(matches!(cs, ImageColorSpace::DeviceGray));
}

/// An inverted `Decode`, or a size other than the JPEG's, is not what a
/// JPEG reader would show: the samples are decoded as before.
#[test]
fn a_jpeg_drawn_otherwise_is_decoded() {
    for body in [
        inline("DeviceRGB", 16, 8, "1 0 1 0 1 0", RGB),
        inline("DeviceRGB", 8, 8, "0 1 0 1 0 1", RGB),
    ] {
        let kept = list(&body, true);
        let [(samples, _, jpeg)] = images(&kept).try_into().expect("one image");
        assert_eq!(jpeg, None);
        assert!(!samples.is_empty());
        assert_eq!(samples, images(&list(&body, false))[0].0);
        assert_eq!(fills(&kept), 1);
    }
}

#[test]
fn without_the_switch_nothing_changes() {
    let [(samples, _, jpeg)] = images(&list(
        &inline("DeviceRGB", 16, 8, "0 1 0 1 0 1", RGB),
        false,
    ))
    .try_into()
    .expect("one image");
    assert_eq!(jpeg, None);
    assert_eq!(samples.len(), 16 * 8 * 3);
}

/// `RGB` with a marker segment inserted after its SOI: `marker`, then `payload`.
fn with_segment(marker: u8, payload: &[u8]) -> String {
    let rgb = bytes(RGB);
    let len = (payload.len() + 2) as u16;
    let mut jpeg = rgb[..2].to_vec();
    jpeg.extend([0xFF, marker]);
    jpeg.extend(len.to_be_bytes());
    jpeg.extend(payload);
    jpeg.extend(&rgb[2..]);
    jpeg.iter().map(|b| format!("{b:02x}")).collect()
}

/// An Exif APP1 whose first IFD holds an `Orientation` of `value`, in `order`.
fn exif(order: &[u8; 2], value: u16) -> Vec<u8> {
    let big = order == b"MM";
    let u16b = |v: u16| {
        if big {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        }
    };
    let u32b = |v: u32| {
        if big {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        }
    };
    let mut seg = b"Exif\0\0".to_vec();
    seg.extend(order);
    seg.extend(u16b(42));
    seg.extend(u32b(8));
    seg.extend(u16b(1));
    seg.extend(u16b(0x0112));
    seg.extend(u16b(3));
    seg.extend(u32b(1));
    seg.extend(u16b(value));
    seg.extend([0, 0]);
    seg.extend(u32b(0));
    seg
}

/// Viewers rotate a JPEG by its Exif `Orientation` and colour-manage it with
/// its ICC profile, and `DCTDecode` does neither: such a JPEG is decoded as
/// before. An `Orientation` of 1 changes nothing and is kept.
#[test]
fn a_jpeg_viewers_show_otherwise_is_not_kept() {
    let plain = |hex: &str| stet_graphics::jpeg::plain(&bytes(hex), 16, 8, 3);
    assert!(plain(RGB));
    for order in [b"MM", b"II"] {
        assert!(plain(&with_segment(0xE1, &exif(order, 1))), "orientation 1");
        assert!(
            !plain(&with_segment(0xE1, &exif(order, 6))),
            "orientation 6"
        );
    }
    assert!(
        !plain(&with_segment(0xE1, b"Exif\0\0nonsense")),
        "unreadable Exif"
    );
    assert!(
        !plain(&with_segment(0xE2, b"ICC_PROFILE\0\x01\x01")),
        "ICC profile"
    );

    let body = inline(
        "DeviceRGB",
        16,
        8,
        "0 1 0 1 0 1",
        &with_segment(0xE1, &exif(b"MM", 6)),
    );
    let [(samples, _, jpeg)] = images(&list(&body, true)).try_into().expect("one image");
    assert_eq!(jpeg, None);
    assert_eq!(samples, images(&list(&body, false))[0].0);
}

/// stet's own renderer and PDF writer decode a kept image: the page is the
/// one painted without keeping JPEG, not a black image.
#[test]
fn a_kept_jpeg_renders_and_writes_as_without_the_switch() {
    let src = format!(
        "%!PS-Adobe-3.0\n{}\nshowpage\n",
        inline("DeviceRGB", 16, 8, "0 1 0 1 0 1", RGB)
    );
    let render = |keep: bool| {
        let builder = Interpreter::builder();
        let builder = if keep {
            builder.keep_jpeg_data()
        } else {
            builder
        };
        builder.build().render(src.as_bytes(), 72.0).unwrap()
    };
    let (kept, plain) = (render(true), render(false));
    assert_eq!(kept.len(), 1);
    // The image's centre is not black, and every pixel is as without the switch.
    let w = kept[0].width as usize;
    let at = ((kept[0].height as usize - 50) * w + 50) * 4;
    assert_ne!(&kept[0].rgba[at..at + 3], &[0, 0, 0]);
    assert!(kept[0].rgba == plain[0].rgba);

    let pdf = |keep: bool| {
        let builder = Interpreter::builder();
        let builder = if keep {
            builder.keep_jpeg_data()
        } else {
            builder
        };
        builder.build().render_to_pdf(src.as_bytes(), 72.0).unwrap()
    };
    assert!(undated(pdf(true)) == undated(pdf(false)), "the same PDF");
}

/// A PDF with its `/CreationDate` value left out: two runs can be a second
/// apart.
fn undated(pdf: Vec<u8>) -> Vec<u8> {
    let key = b"/CreationDate";
    let Some(at) = pdf.windows(key.len()).position(|w| w == key) else {
        return pdf;
    };
    let end = pdf[at..]
        .iter()
        .position(|&b| b == b')')
        .map_or(pdf.len(), |i| at + i + 1);
    [&pdf[..at], &pdf[end..]].concat()
}
