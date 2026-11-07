// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A Separation or DeviceN space paints at tint 1.0 until `setcolor` says
//! otherwise.
//!
//! PLRM `setcolorspace` makes 1.0 the initial value of every tint, so a fill
//! straight after `setcolorspace` is the spot colour at full strength. stet
//! painted it black: running the tint transform was left to `setcolor`.

#![cfg(feature = "render")]

use stet::Interpreter;

/// A spot whose CMYK alternate at tint 1.0 is 0 0.91 0.76 0, a red; at tint
/// 0 it is white, so black can only come from not running the transform.
const SPOT: &str =
    "[/Separation (Spot) /DeviceCMYK {dup 0 mul exch dup .91 mul exch dup .76 mul exch 0 mul}]";

/// The pixel at (10, 10) of each page, rendered at 72 dpi.
fn pixel_10_10(job: &str) -> Vec<[u8; 3]> {
    Interpreter::new()
        .render(job.as_bytes(), 72.0)
        .unwrap()
        .iter()
        .map(|page| {
            let i = ((10 * page.width + 10) * 4) as usize;
            [page.rgba[i], page.rgba[i + 1], page.rgba[i + 2]]
        })
        .collect()
}

/// A page filled in the colour `setup` leaves current.
fn fill_after(setup: &str) -> String {
    format!(
        "%!PS\n<< /PageSize [20 20] >> setpagedevice\n{setup}\n\
         0 0 20 20 rectfill showpage\n"
    )
}

#[test]
fn separation_starts_at_full_tint() {
    let pages = pixel_10_10(&fill_after(&format!("{SPOT} setcolorspace")));
    let [r, g, b] = pages[0];
    // Ghostscript paints 239 62 66.
    assert!(r > 200 && g < 100 && b < 100, "{r} {g} {b}");
}

#[test]
fn separation_starts_where_one_setcolor_would_put_it() {
    let initial = pixel_10_10(&fill_after(&format!("{SPOT} setcolorspace")));
    let explicit = pixel_10_10(&fill_after(&format!("{SPOT} setcolorspace 1 setcolor")));
    assert_eq!(initial, explicit);
}

/// Two colorants, magenta and yellow, which at tints 1 1 make a red.
const MAGENTA_YELLOW: &str = "[/DeviceN [/Magenta /Yellow] /DeviceCMYK {0 3 1 roll 0}]";

#[test]
fn devicen_starts_at_full_tint_for_every_colorant() {
    let pages = pixel_10_10(&fill_after(&format!("{MAGENTA_YELLOW} setcolorspace")));
    let [r, g, b] = pages[0];
    assert!(r > 200 && g < 100 && b < 100, "{r} {g} {b}");
    let explicit = pixel_10_10(&fill_after(&format!(
        "{MAGENTA_YELLOW} setcolorspace 1 1 setcolor"
    )));
    assert_eq!(pages, explicit);
}

/// A tint transform that fails cannot fail `setcolorspace`: Ghostscript runs
/// it only when the colour is painted. The colour stays black, as before the
/// space had an initial colour, and the operand stack is left as it was.
#[test]
fn failing_tint_transform_leaves_black() {
    let job = "%!PS\n<< /PageSize [20 20] >> setpagedevice\n\
               7 [/Separation (Bad) /DeviceGray {pop undefinedthing}] setcolorspace\n\
               7 ne { (operand stack disturbed) = quit } if\n\
               0 0 20 20 rectfill showpage\n";
    assert_eq!(pixel_10_10(job), vec![[0, 0, 0]]);
}
