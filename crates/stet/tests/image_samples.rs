// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PostScript image samples deeper than 8 bits, and ImageType 4 colour keys.
//!
//! The display list carries 8-bit samples: a 12- or 16-bit sample becomes
//! the nearest 8-bit value, not its high bits. `MaskColor` names samples as
//! encoded, at their own depth and before `Decode` (PLRM 4.10.6).

use stet::{DisplayElement, Interpreter};
use stet_graphics::device::ImageColorSpace;

/// The one image `body` paints: its samples, colour space and colour key.
fn image(body: &str) -> (Vec<u8>, ImageColorSpace, Option<Vec<u8>>) {
    let src = format!("%!PS-Adobe-3.0\n{body}\nshowpage\n");
    let mut pages = Interpreter::new()
        .render_to_display_list(src.as_bytes(), 72.0)
        .unwrap();
    assert_eq!(pages.len(), 1);
    let list = pages.remove(0).display_list;
    let images: Vec<_> = list
        .elements()
        .iter()
        .filter_map(|e| match e {
            DisplayElement::Image {
                sample_data,
                params,
            } => Some((
                sample_data.to_vec(),
                params.color_space.clone(),
                params.mask_color.clone(),
            )),
            _ => None,
        })
        .collect();
    let [image] = images.try_into().expect("one image");
    image
}

/// Which pixels of the image `body` paints are left unpainted, as the
/// renderer decides: by the colour key on the samples it is given, or by
/// the alpha of samples converted already.
fn masked(body: &str) -> Vec<bool> {
    let (samples, cs, key) = image(body);
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

/// A 2×1 ImageType 4 image in the current colour space, of `bpc` bits,
/// with `data` and the key `mask`, plus any other entries `more`.
fn keyed(bpc: u32, data: &str, mask: &str, more: &str) -> String {
    format!(
        "<< /ImageType 4 /Width 2 /Height 1 /BitsPerComponent {bpc} /MaskColor {mask}\n\
            /ImageMatrix [2 0 0 1 0 0] /DataSource <{data}> {more} >> image"
    )
}

fn gray_keyed(bpc: u32, data: &str, mask: &str, more: &str) -> String {
    format!("/DeviceGray setcolorspace {}", keyed(bpc, data, mask, more))
}

// ------------------------------------------------------------- rounding

/// 0x10FF is 16.93 levels: 17 to the nearest, 16 by its high byte.
#[test]
fn sixteen_bit_samples_are_rounded() {
    let (samples, ..) = image(
        "/DeviceGray setcolorspace\n\
         << /ImageType 1 /Width 4 /Height 1 /BitsPerComponent 16 /Decode [0 1]\n\
            /ImageMatrix [4 0 0 1 0 0] /DataSource <10FF 8100 8101 FFFF> >> image",
    );
    assert_eq!(samples, [0x11, 0x80, 0x81, 0xFF]);
}

/// 0x10F is 16.88 levels, 0x108 16.44.
#[test]
fn twelve_bit_samples_are_rounded() {
    let (samples, ..) = image(
        "/DeviceGray setcolorspace\n\
         << /ImageType 1 /Width 2 /Height 1 /BitsPerComponent 12 /Decode [0 1]\n\
            /ImageMatrix [2 0 0 1 0 0] /DataSource <10F108> >> image",
    );
    assert_eq!(samples, [17, 16]);
}

// ---------------------------------------------------------- colour keys

#[test]
fn colour_keys_name_samples_at_their_depth() {
    for (bpc, data, mask) in [
        (1, "80", "[1]"),
        (2, "D0", "[3]"),
        (4, "F0", "[15]"),
        (8, "C832", "[200]"),
        (12, "800801", "[2048 2048]"),
        (16, "80401000", "[32768 33023]"),
    ] {
        assert_eq!(
            masked(&gray_keyed(bpc, data, mask, "")),
            [true, false],
            "{bpc} bits"
        );
    }
}

/// 0x80FF and 0x8100 both round to 128, but only 0x80FF is in the key: a
/// key reduced to 8 bits would take both.
#[test]
fn sixteen_bit_colour_keys_are_not_reduced() {
    assert_eq!(
        masked(&gray_keyed(16, "80FF8100", "[32768 33023]", "")),
        [true, false]
    );
}

#[test]
fn colour_keys_name_samples_before_decode() {
    assert_eq!(
        masked(&gray_keyed(8, "C837", "[200]", "/Decode [1 0]")),
        [true, false]
    );
    assert_eq!(
        masked(&gray_keyed(4, "F0", "[15]", "/Decode [1 0]")),
        [true, false]
    );
}

/// An index is the sample: the key is not scaled to 8 bits for it.
#[test]
fn colour_keys_on_indexed_images_name_indices() {
    let body = format!(
        "[/Indexed /DeviceRGB 3 <FF000000FF000000FFFFFFFF>] setcolorspace {}",
        keyed(4, "31", "[3]", "/Decode [0 15]")
    );
    assert_eq!(masked(&body), [true, false]);
}

/// A value past the depth is clipped, not wrapped: `300 as u8` is 44.
#[test]
fn colour_keys_past_the_depth_match_nothing() {
    assert_eq!(masked(&gray_keyed(8, "2CFF", "[300]", "")), [false, false]);
    assert_eq!(masked(&gray_keyed(8, "00FF", "[0 300]", "")), [true, true]);
}

/// A space converted while painting takes the key the same way.
#[test]
fn colour_keys_on_converted_spaces() {
    let body = format!(
        "[/Separation /Spot /DeviceGray {{}}] setcolorspace {}",
        keyed(16, "80401000", "[32768 33023]", "")
    );
    assert_eq!(masked(&body), [true, false]);
}
