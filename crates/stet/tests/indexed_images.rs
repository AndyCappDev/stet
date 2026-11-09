// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! An Indexed space may have up to 4096 entries, and an image in one takes
//! each sample through its `Decode` to an index (PLRM 3e §4.8.4, §4.10.5).
//!
//! `setcolorspace` refused a palette past 256 entries with `rangecheck`,
//! though the PLRM allows `hival` up to 4095 for 12-bit images. An image
//! ignored its `Decode`, and a 12- or 16-bit sample was cut to its high 8
//! bits, so index 5 of a 16-bit image drew entry 0.

#![cfg(feature = "render")]

use stet::Interpreter;

/// Entry `i` of a 4096-entry RGB palette.
fn entry(i: usize) -> [u8; 3] {
    [
        (i & 0xFF) as u8,
        ((i * 7) & 0xFF) as u8,
        ((i >> 4) & 0xFF) as u8,
    ]
}

/// `[/Indexed base 4095 <…>]` over `entry`.
fn big_palette(base: &str, comps: impl Fn(usize) -> Vec<u8>) -> String {
    let hex: String = (0..4096)
        .flat_map(comps)
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("[/Indexed {base} 4095 <{hex}>]")
}

/// A row of 100-point cells, 50 high, at 72 dpi; the RGB at each cell's
/// centre.
fn cells(body: &str, n: usize) -> Vec<[u8; 3]> {
    let w = 100 * n;
    let ps = format!("%!PS\n<< /PageSize [{w} 50] >> setpagedevice\n{body}\nshowpage\n");
    let mut interp = Interpreter::builder().suppress_output().build();
    let pages = interp.render(ps.as_bytes(), 72.0).expect("renders");
    let rgba = &pages[0].rgba;
    (0..n)
        .map(|k| {
            let i = (25 * w + k * 100 + 50) * 4;
            [rgba[i], rgba[i + 1], rgba[i + 2]]
        })
        .collect()
}

/// A one-row image of `samples`, one cell each.
fn image(samples: &[usize], bps: u32, decode: &str) -> String {
    let bits: String = samples
        .iter()
        .map(|&s| format!("{s:0width$b}", width = bps as usize))
        .collect();
    let hex: String = bits
        .as_bytes()
        .chunks(8)
        .map(|c| {
            let byte = std::str::from_utf8(c).unwrap();
            format!(
                "{:02x}",
                u8::from_str_radix(&format!("{byte:0<8}"), 2).unwrap()
            )
        })
        .collect();
    let n = samples.len();
    format!(
        "gsave {w} 50 scale << /ImageType 1 /Width {n} /Height 1 /BitsPerComponent {bps} \
         /Decode {decode} /ImageMatrix [{n} 0 0 1 0 0] /DataSource <{hex}> >> image grestore",
        w = n * 100
    )
}

const INDICES: [usize; 6] = [0, 5, 300, 1000, 2049, 4095];

#[test]
fn a_palette_of_4096_entries_sets_each_colour() {
    let body = format!(
        "{} setcolorspace\n{}",
        big_palette("/DeviceRGB", |i| entry(i).to_vec()),
        INDICES
            .iter()
            .enumerate()
            .map(|(k, i)| format!("{i} setcolor {} 0 100 50 rectfill", k * 100))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let expected: Vec<_> = INDICES.iter().map(|&i| entry(i)).collect();
    assert_eq!(cells(&body, 6), expected);
}

#[test]
fn a_16_bit_image_takes_every_index() {
    let body = format!(
        "{} setcolorspace {}",
        big_palette("/DeviceRGB", |i| entry(i).to_vec()),
        image(&INDICES, 16, "[0 65535]")
    );
    let expected: Vec<_> = INDICES.iter().map(|&i| entry(i)).collect();
    assert_eq!(cells(&body, 6), expected);
}

#[test]
fn a_12_bit_image_takes_every_index() {
    let body = format!(
        "{} setcolorspace {}",
        big_palette("/DeviceRGB", |i| entry(i).to_vec()),
        image(&INDICES, 12, "[0 4095]")
    );
    let expected: Vec<_> = INDICES.iter().map(|&i| entry(i)).collect();
    assert_eq!(cells(&body, 6), expected);
}

/// Red, green, blue, yellow, cyan, magenta.
const SIX: &str = "[/Indexed /DeviceRGB 5 <FF0000 00FF00 0000FF FFFF00 00FFFF FF00FF>]";

#[test]
fn a_16_bit_index_into_a_small_palette_is_not_cut_to_its_high_byte() {
    let body = format!(
        "{SIX} setcolorspace {}",
        image(&[0, 1, 2, 3, 4, 5], 16, "[0 65535]")
    );
    assert_eq!(
        cells(&body, 6),
        [
            [255, 0, 0],
            [0, 255, 0],
            [0, 0, 255],
            [255, 255, 0],
            [0, 255, 255],
            [255, 0, 255]
        ]
    );
}

#[test]
fn the_decode_maps_samples_to_indices() {
    // [5 0] reverses 2-bit samples 0, 1, 2, 3 onto indices 5, 3.33, 1.67, 0,
    // rounded as `setcolor` rounds an index: 5, 3, 2, 0.
    let body = format!("{SIX} setcolorspace {}", image(&[0, 1, 2, 3], 2, "[5 0]"));
    assert_eq!(
        cells(&body, 4),
        [[255, 0, 255], [255, 255, 0], [0, 0, 255], [255, 0, 0]]
    );
}

#[test]
fn a_large_palette_over_a_spot_colour_is_drawn_through_its_tint_transform() {
    let sep = "[/Separation /Spot /DeviceCMYK {dup 0.1 mul exch dup 0.8 mul exch dup 0.9 mul exch 0 mul}]";
    // Entry i is tint i / 4095 to the nearest byte; 4095 is the full tint.
    let palette = big_palette(sep, |i| vec![(i * 255 / 4095) as u8]);
    let body = format!(
        "{sep} setcolorspace 1 setcolor 0 0 100 50 rectfill\n\
         {palette} setcolorspace 4095 setcolor 100 0 100 50 rectfill\n\
         gsave 200 0 translate {}",
        image(&[4095], 16, "[0 65535]").replacen("gsave ", "", 1)
    );
    let got = cells(&body, 3);
    assert!(got[0] != [255, 255, 255], "the tint is drawn: {got:?}");
    // The image goes through the sampled tint table, the fills through the
    // transform itself.
    let close = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).all(|(&p, q)| p.abs_diff(q) <= 2);
    assert!(close(got[1], got[0]), "fill {got:?}");
    assert!(close(got[2], got[0]), "image {got:?}");
}
