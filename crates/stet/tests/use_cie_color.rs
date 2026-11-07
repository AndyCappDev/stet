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
