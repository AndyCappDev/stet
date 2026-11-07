// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `stet_graphics::jpeg::decode` returns a CMYK JPEG's samples as stored.
//!
//! The fixture is an 8×8 CMYK JPEG written by Ghostscript's `DCTEncode` from
//! the samples C 51, M 102, Y 153, K 0 (0.2 0.4 0.6 0), with Adobe's APP14
//! marker. Ghostscript's `DCTDecode` reads it back as those values, and so
//! must stet's — the PostScript interpreter and the PDF reader both decode
//! through this function. `jpeg_decoder` alone returns them inverted, which
//! turned a CMYK page's white paper black.

#[test]
fn cmyk_jpeg_decodes_to_the_samples_it_stores() {
    let data = include_bytes!("data/jpeg/cmyk_adobe_gs.jpg");
    let pixels = stet_graphics::jpeg::decode(data).unwrap();
    assert_eq!(pixels.len(), 8 * 8 * 4);
    for px in pixels.as_chunks::<4>().0 {
        for (got, want) in px.iter().zip([51u8, 102, 153, 0]) {
            assert!(got.abs_diff(want) <= 4, "{px:?}");
        }
    }
}

#[test]
fn not_a_jpeg_is_an_error() {
    assert!(stet_graphics::jpeg::decode(b"not a jpeg").is_err());
}
