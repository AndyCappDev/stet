// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A `DCTDecode` filter's data ends at the JPEG's end-of-image marker, and
//! what follows is left in the source.
//!
//! The filter read its whole source before decoding, so a JPEG read
//! straight from `currentfile`, as an EPS can inline one, swallowed the
//! rest of the program: the page after it was never drawn or output.
//! Ghostscript stops at the marker.

#![cfg(feature = "render")]

use stet::Interpreter;

/// A 16 x 8 JPEG of RGB (200, 40, 40), made with Pillow.
const SOLID: &str = "ffd8ffe000104a46494600010100000100010000ffdb004300030202030202030303030403030405080505040405\
    0a070706080c0a0c0c0b0a0b0b0d0e12100d0e110e0b0b1016101113141515150c0f171816141812141514ffdb00\
    430103040405040509050509140d0b0d141414141414141414141414141414141414141414141414141414141414\
    1414141414141414141414141414141414141414ffc00011080008001003012200021101031101ffc4001f000001\
    0501010101010100000000000000000102030405060708090a0bffc400b510000201030302040305050404000001\
    7d01020300041105122131410613516107227114328191a1082342b1c11552d1f02433627282090a161718191a25\
    262728292a3435363738393a434445464748494a535455565758595a636465666768696a737475767778797a8384\
    85868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9cad2d3d4d5\
    d6d7d8d9dae1e2e3e4e5e6e7e8e9eaf1f2f3f4f5f6f7f8f9faffc4001f0100030101010101010101010000000000\
    000102030405060708090a0bffc400b5110002010204040304070504040001027700010203110405213106124151\
    0761711322328108144291a1b1c109233352f0156272d10a162434e125f11718191a262728292a35363738393a43\
    4445464748494a535455565758595a636465666768696a737475767778797a82838485868788898a929394959697\
    98999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9cad2d3d4d5d6d7d8d9dae2e3e4e5e6e7e8\
    e9eaf2f3f4f5f6f7f8f9faffda000c03010002110311003f00f14a28a2bf373fb54fffd9";

/// A 32 x 32 JPEG of noise with a restart marker after every MCU, made with
/// Pillow: its scan data holds stuffed `FF 00` bytes and `FF D0`-`FF D7`.
const NOISY: &str = "ffd8ffe000104a46494600010100000100010000ffdb004300030202030202030303030403030405080505040405\
    0a070706080c0a0c0c0b0a0b0b0d0e12100d0e110e0b0b1016101113141515150c0f171816141812141514ffdb00\
    430103040405040509050509140d0b0d141414141414141414141414141414141414141414141414141414141414\
    1414141414141414141414141414141414141414ffc00011080020002003012200021101031101ffc4001f000001\
    0501010101010100000000000000000102030405060708090a0bffc400b510000201030302040305050404000001\
    7d01020300041105122131410613516107227114328191a1082342b1c11552d1f02433627282090a161718191a25\
    262728292a3435363738393a434445464748494a535455565758595a636465666768696a737475767778797a8384\
    85868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9cad2d3d4d5\
    d6d7d8d9dae1e2e3e4e5e6e7e8e9eaf1f2f3f4f5f6f7f8f9faffc4001f0100030101010101010101010000000000\
    000102030405060708090a0bffc400b5110002010204040304070504040001027700010203110405213106124151\
    0761711322328108144291a1b1c109233352f0156272d10a162434e125f11718191a262728292a35363738393a43\
    4445464748494a535455565758595a636465666768696a737475767778797a82838485868788898a929394959697\
    98999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9cad2d3d4d5d6d7d8d9dae2e3e4e5e6e7e8\
    e9eaf2f3f4f5f6f7f8f9faffdd00040001ffda000c03010002110311003f00c7d6a2d0eca3bdb7d66db4cf0bdf3d\
    adc2cd1a10f25c640b9790b5be14483ed38215271134319dc0484d743368a9ae5b6a0d1bcbf0fa53637da4463c3f\
    a9a4a81e79239a34568866691f7c76e225608cf12aaae1101c85baf114ba9c771a8e9b6b75fd9b3dddbcd73a640b\
    6d18737722c70406149ca23ccedf3e00fb45aa42bb026e7bf7fe1abb934c1a6cbe1a975a818c56ff00630803cea2\
    497c98ada37c6e9230cd1e602c0886370c439324d5ab3aa9c54af55a69c95ad7937aa574dca316972b8c9bba49a5\
    cd7da2e3ad3854f753e6bf3492e57a2f8db51492b36db6da82d79b4fffd0a29a26b56d66fe18893ec93dc15c6b12\
    4ff688ad5253146ac18c72ee68b31400091956dae1d0b0f318a90ea1a9e9de24f0ade3f88ec759beb1ba4beb882e\
    e7850c5e632b30f52b145ba695e3708a239599fe50f2de82cecedbc49e219758bfd12743a64a61d39e79e38efb6a\
    1588cb12324b2299a490b0649498cb3c4a859f342c741f1178e34adb6fa7f87b51d534ad3a08ef2ed56596eded20\
    16e2399ed1e39944e444176a911cbcb3602263a2ae228d4a5ed6ab6f757b7325a72f2b94f5719b95a6e2d5a56d1b\
    6d9d5cb2aaaa622a416eadbc926d251717694ef6834edaed17cd1e78cbffd1a3f16bc53a4bf86f44b59b50b2b5bb\
    d1f7adb457174f2c76b69e5289156da248a362606b81b595e2091b468a392ba97ba85ba6a1a68d3e59ed5b499eee\
    fa6b88755b6940b6b9920530cd0bdd95118221802812a990622db248af54f50d747f62d8dd5d4f6fa9dbea16521b\
    816b74d7426bc6fb3c701244bcb4891b18a247490e5880af1c60d3b5f1e2e8ba0c9fd8b730ea3a258c31345672dd\
    a5ec12db92247fb399144fcce5a1cf94c5558be23712a2ef4e841d1a54e8a69424e52f7ecacefa38b8c39a3acacf\
    964aedbd79ac63470ee855e4c1c9ca6d7bcad049ea9b92f89a8a6df2c5a4b97de56ba4ff00ffd2afa44b178ab558\
    45d5bf8774e175773cf3dc5c4b0a2dac40ec25a4119f3183411a2446431c917da00472331af8abc2812d3c437778\
    134cb6b7f383cba74168b0dedd219db72a3a202f25bdac6e4c9e5a6d676006f41253d2609f548f59934dd1eec5ed\
    c406e64d4750b68120b4da9e723c4a8f3bbbf9cd748c4be50ae368910b47a7a4eb3aa5c788f50d2923d4edb4b749\
    614b5d2e1331d32272e92cc096f3d0a1c8da5434b240a225431a495ad2a58ca152588a114a2b95c5f35afcaeef95\
    fbd7495d5a565cb69bb5d5f2c57bb53db45e8a4e50d9eb1778bd52e449def16a52b29737d993ffd9";

fn bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

/// A page with the JPEG drawn over the left half, read from `currentfile`
/// through `filters`, then a red fill over the right half: the number of
/// pages, and the page as RGBA.
fn render(w: u32, h: u32, filters: &str, data: &[u8]) -> (usize, Vec<u8>) {
    let mut ps = format!(
        "%!PS\n<< /PageSize [100 100] >> setpagedevice\n\
         gsave /DeviceRGB setcolorspace 50 100 scale\n\
         << /ImageType 1 /Width {w} /Height {h} /BitsPerComponent 8 /Decode [0 1 0 1 0 1]\n\
         /ImageMatrix [{w} 0 0 {h} 0 0] /DataSource currentfile {filters} /DCTDecode filter >>\n\
         image\n"
    )
    .into_bytes();
    ps.extend_from_slice(data);
    ps.extend_from_slice(b"\ngrestore 1 0 0 setrgbcolor 50 0 50 100 rectfill showpage\n");
    let mut interp = Interpreter::builder().suppress_output().build();
    let pages = interp.render(&ps, 72.0).expect("renders");
    let rgba = pages.first().map(|p| p.rgba.clone()).unwrap_or_default();
    (pages.len(), rgba)
}

fn pixel(rgba: &[u8], x: usize, y: usize) -> [u8; 3] {
    let i = (y * 100 + x) * 4;
    [rgba[i], rgba[i + 1], rgba[i + 2]]
}

fn close(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter().zip(b).all(|(&p, q)| p.abs_diff(q) <= 4)
}

#[test]
fn a_binary_jpeg_on_currentfile_leaves_the_rest_of_the_program() {
    let (pages, rgba) = render(16, 8, "", &bytes(SOLID));
    assert_eq!(pages, 1, "the page after the image is output");
    assert!(
        close(pixel(&rgba, 25, 50), [200, 40, 40]),
        "{:?}",
        pixel(&rgba, 25, 50)
    );
    assert_eq!(
        pixel(&rgba, 75, 50),
        [255, 0, 0],
        "the fill after the image"
    );
}

#[test]
fn an_end_marker_inside_a_segment_does_not_end_the_jpeg() {
    // An APP15 segment holding FF D9, as an embedded thumbnail would, just
    // after the start-of-image marker.
    let mut jpeg = bytes(SOLID);
    jpeg.splice(2..2, [0xFF, 0xEF, 0x00, 0x06, b'x', 0xFF, 0xD9, b'y']);
    let (pages, rgba) = render(16, 8, "", &jpeg);
    assert_eq!(pages, 1);
    assert!(
        close(pixel(&rgba, 25, 50), [200, 40, 40]),
        "{:?}",
        pixel(&rgba, 25, 50)
    );
    assert_eq!(pixel(&rgba, 75, 50), [255, 0, 0]);
}

#[test]
fn restart_markers_and_stuffed_bytes_are_scan_data() {
    let jpeg = bytes(NOISY);
    let (pages, binary) = render(32, 32, "", &jpeg);
    assert_eq!(pages, 1);
    assert_eq!(pixel(&binary, 75, 50), [255, 0, 0]);
    // The same JPEG through ASCIIHexDecode, whose own end of data bounds it.
    let hex: String = jpeg.iter().map(|b| format!("{b:02x}")).collect();
    let (_, wrapped) = render(
        32,
        32,
        "/ASCIIHexDecode filter",
        format!("{hex}>").as_bytes(),
    );
    for y in (2..98).step_by(3) {
        for x in (2..48).step_by(3) {
            assert_eq!(pixel(&binary, x, y), pixel(&wrapped, x, y), "at {x},{y}");
        }
    }
}
