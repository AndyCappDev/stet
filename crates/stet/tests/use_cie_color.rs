// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! With `UseCIEColor` on, a device colour space is painted through its
//! `Default*` ColorSpace resource (PLRM, "Remapping Device Colors to CIE").
//!
//! The remapping is internal, so `currentcolorspace` still reports the
//! device space and no PostScript operator can see it; only what is painted
//! shows it. A `DefaultGray` whose `MatrixA` is all zeros sends every gray to
//! black, so white painted in DeviceGray comes out black only if the remap is
//! in force — Ghostscript paints it black.

#![cfg(feature = "render")]

use stet::Interpreter;

/// The pixel at (10, 10) of the first page, rendered at 72 dpi.
fn pixel_10_10(job: &str) -> [u8; 3] {
    let pages = Interpreter::new().render(job.as_bytes(), 72.0).unwrap();
    let page = &pages[0];
    let i = ((10 * page.width + 10) * 4) as usize;
    [page.rgba[i], page.rgba[i + 1], page.rgba[i + 2]]
}

/// White in DeviceGray, with `use_cie_color` as the page device's setting
/// and a `DefaultGray` that maps every gray to black.
fn white_gray_fill(use_cie_color: bool) -> String {
    format!(
        "%!PS\n<< /PageSize [20 20] >> setpagedevice\n\
         /DefaultGray [/CIEBasedA << /WhitePoint [0.9505 1 1.089] /MatrixA [0 0 0] >>]\n\
         /ColorSpace defineresource pop\n\
         << /UseCIEColor {use_cie_color} >> setpagedevice\n\
         [/DeviceGray] setcolorspace 1 setcolor 0 0 20 20 rectfill showpage\n"
    )
}

#[test]
fn setcolor_in_a_device_space_paints_through_the_default_space() {
    assert_eq!(pixel_10_10(&white_gray_fill(true)), [0, 0, 0]);
}

#[test]
fn without_use_cie_color_the_device_space_is_painted_as_is() {
    assert_eq!(pixel_10_10(&white_gray_fill(false)), [255, 255, 255]);
}

/// A page filled after `paint` with UseCIEColor on, a `DefaultGray` that
/// maps every gray to black, and a `DefaultRGB` with its R and B rows
/// swapped, which paints red as blue.
fn remapped_fill(paint: &str) -> String {
    format!(
        "%!PS\n<< /PageSize [20 20] >> setpagedevice\n\
         /DefaultGray [/CIEBasedA << /WhitePoint [0.9505 1 1.089] /MatrixA [0 0 0] >>]\n\
         /ColorSpace defineresource pop\n\
         /DefaultRGB [/CIEBasedABC << /WhitePoint [0.9505 1 1.089]\n\
           /MatrixABC [0.1805 0.0722 0.9505 0.3576 0.7152 0.1192 0.4124 0.2126 0.0193] >>]\n\
         /ColorSpace defineresource pop\n\
         << /UseCIEColor true >> setpagedevice\n\
         {paint} 0 0 20 20 rectfill showpage\n"
    )
}

/// `setgray`, `setrgbcolor` and `sethsbcolor` select their device space
/// implicitly, so they are remapped just as `setcolorspace` is. They used to
/// set the device colour directly, painting white where Ghostscript paints
/// black and red where it paints blue.
#[test]
fn the_colour_operators_paint_through_the_default_space() {
    assert_eq!(pixel_10_10(&remapped_fill("1 setgray")), [0, 0, 0]);
    assert_eq!(
        pixel_10_10(&remapped_fill("1 0 0 setrgbcolor")),
        [0, 0, 255]
    );
    assert_eq!(
        pixel_10_10(&remapped_fill("0 1 1 sethsbcolor")),
        [0, 0, 255]
    );
    assert_eq!(
        pixel_10_10(&remapped_fill("[/DeviceRGB] setcolorspace 1 0 0 setcolor")),
        [0, 0, 255]
    );
}
