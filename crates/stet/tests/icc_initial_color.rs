// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! An ICCBased space with a profile starts with every component at 0,
//! clipped to its `/Range`.
//!
//! PLRM `setcolorspace` gives an ICCBased space that initial colour, so a
//! CMYK profile starts with no ink, white. stet started it at `0 0 0 1`, the
//! initial colour of DeviceCMYK, and painted black. A space whose profile
//! cannot be used stands for the device space, as in Ghostscript, and keeps
//! that space's black.

#![cfg(feature = "render")]

use stet::Interpreter;

/// The pixel at (10, 10) of the first page, rendered at 72 dpi.
fn pixel_10_10(job: &str) -> [u8; 3] {
    let pages = Interpreter::new().render(job.as_bytes(), 72.0).unwrap();
    let page = &pages[0];
    let i = ((10 * page.width + 10) * 4) as usize;
    [page.rgba[i], page.rgba[i + 1], page.rgba[i + 2]]
}

/// A page filled straight after `[/ICCBased dict] setcolorspace`.
fn fill_in_icc(dict_extra: &str, with_profile: bool) -> String {
    let source = if with_profile {
        let profile = concat!(env!("CARGO_MANIFEST_DIR"), "/resources/default_cmyk.icc");
        format!("/DataSource ({profile}) (r) file")
    } else {
        String::new()
    };
    format!(
        "%!PS\n<< /PageSize [20 20] >> setpagedevice\n\
         [/ICCBased << /N 4 {source} {dict_extra} >>] setcolorspace\n\
         0 0 20 20 rectfill showpage\n"
    )
}

#[test]
fn cmyk_profile_starts_with_no_ink() {
    assert_eq!(pixel_10_10(&fill_in_icc("", true)), [255, 255, 255]);
}

#[test]
fn initial_component_is_clipped_to_range() {
    // Cyan starts at 0.2, the bottom of its range: Ghostscript paints 199 234 251.
    let [r, g, b] = pixel_10_10(&fill_in_icc("/Range [0.2 1 0 1 0 1 0 1]", true));
    assert!(r < 220 && g > 220 && b > 240, "{r} {g} {b}");
}

#[test]
fn space_without_a_profile_starts_black_like_devicecmyk() {
    let [r, g, b] = pixel_10_10(&fill_in_icc("", false));
    assert!(r < 60 && g < 60 && b < 60, "{r} {g} {b}");
}
