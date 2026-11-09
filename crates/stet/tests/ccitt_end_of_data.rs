// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A `CCITTFaxDecode` filter's data ends with the image, and what follows
//! is left in the source.
//!
//! The filter read its whole source before decoding, so a fax image read
//! straight from `currentfile` swallowed the rest of the program: the page
//! after it was never drawn or output. Ghostscript stops at the image's
//! end-of-block code.

#![cfg(feature = "render")]

use stet::Interpreter;

/// A 32 x 16 image, left half black, as Ghostscript's `CCITTFaxEncode`
/// writes it with `EndOfBlock` true.
const G4: &str = "26a0bfffffffffffe0020020";
/// The same image in Group 3 one-dimensional coding (`K 0`), ending in six
/// EOLs.
const G3: &str = "3505ea3505ea3505ea3505ea3505ea3505ea3505ea3505ea3505ea3505ea3505ea\
                  3505ea3505ea3505ea3505ea3505ea001001001001001001";
/// The same image in Group 3 two-dimensional coding (`K 2`) with EOLs,
/// ending in six EOLs each followed by a tag bit.
const G3_2D: &str = "0019a82f5000b800cd417a8005c0066a0bd4002e0033505ea00170019a82f5000b80\
                     0cd417a8005c0066a0bd4002e0033505ea0017001800c006003001800c";

fn bytes(hex: &str) -> Vec<u8> {
    let hex: String = hex.split_whitespace().collect();
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

/// A page with the image over the left half, read raw from `currentfile`
/// with `params`, then a red fill over the right half: the number of
/// pages, and the colours at the image's left and right and in the fill.
fn render(params: &str, data: &[u8]) -> (usize, [[u8; 3]; 3]) {
    let mut ps = format!(
        "%!PS\n<< /PageSize [100 100] >> setpagedevice\n\
         gsave 50 100 scale /DeviceGray setcolorspace\n\
         << /ImageType 1 /Width 32 /Height 16 /BitsPerComponent 1 /Decode [0 1]\n\
         /ImageMatrix [32 0 0 -16 0 16] /DataSource currentfile\n\
         << {params} /Columns 32 /Rows 16 >> /CCITTFaxDecode filter >>\nimage\n"
    )
    .into_bytes();
    ps.extend_from_slice(data);
    ps.extend_from_slice(b"\ngrestore 1 0 0 setrgbcolor 50 0 50 100 rectfill showpage\n");
    let mut interp = Interpreter::builder().suppress_output().build();
    let pages = interp.render(&ps, 72.0).expect("renders");
    let Some(page) = pages.first() else {
        return (0, [[0; 3]; 3]);
    };
    let px = |x: usize| {
        let i = (50 * 100 + x) * 4;
        [page.rgba[i], page.rgba[i + 1], page.rgba[i + 2]]
    };
    (pages.len(), [px(10), px(40), px(75)])
}

const BLACK: [u8; 3] = [0, 0, 0];
const WHITE: [u8; 3] = [255, 255, 255];
const RED: [u8; 3] = [255, 0, 0];

#[test]
fn a_group_4_image_on_currentfile_leaves_the_rest_of_the_program() {
    assert_eq!(render("/K -1", &bytes(G4)), (1, [BLACK, WHITE, RED]));
}

#[test]
fn a_group_3_image_on_currentfile_leaves_the_rest_of_the_program() {
    assert_eq!(render("/K 0", &bytes(G3)), (1, [BLACK, WHITE, RED]));
}

/// hayro-ccitt reads only the first EOL of a two-dimensional image's
/// end-of-block code; the rest must not be left for the scanner.
#[test]
fn a_two_dimensional_group_3_image_takes_its_whole_end_of_block_code() {
    assert_eq!(
        render("/K 2 /EndOfLine true", &bytes(G3_2D)),
        (1, [BLACK, WHITE, RED])
    );
}
