// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A transparency group without a `/BBox`, opened with no clip set, is
//! bounded by the page.
//!
//! The default took the page size in points as its device-space bounds, so
//! above 72 dpi the group was clipped to the device's top-left corner — at
//! 300 dpi, 24 % of the page in each direction — and drew nothing beyond it,
//! in every output.

use stet::{DisplayElement, Interpreter};

/// A 300 × 200 page, its group opened after a `translate` (the page does
/// not move with the user's coordinates), filling a square in the corner
/// furthest from the device origin.
const JOB: &str = "%!PS
<< /PageSize [300 200] >> setpagedevice
50 50 translate
<< >> begintransparencygroup
0 0 1 setrgbcolor 200 -40 40 40 rectfill
endtransparencygroup
showpage
";

#[test]
fn the_group_is_bounded_by_the_page_in_device_space() {
    for dpi in [72.0, 300.0] {
        let pages = Interpreter::new()
            .render_to_display_list(JOB.as_bytes(), dpi)
            .unwrap();
        let page = &pages[0];
        let bbox = page
            .display_list
            .elements()
            .iter()
            .find_map(|e| match e {
                DisplayElement::Group { params, .. } => Some(params.bbox),
                _ => None,
            })
            .expect("a group");
        let (w, h) = (300.0 * dpi / 72.0, 200.0 * dpi / 72.0);
        for (got, want) in bbox.iter().zip([0.0, 0.0, w, h]) {
            assert!((got - want).abs() < 1.0, "{dpi} dpi: bbox {bbox:?}");
        }
    }
}

#[cfg(feature = "render")]
#[test]
fn the_group_draws_across_the_page_at_300_dpi() {
    let pages = Interpreter::new().render(JOB.as_bytes(), 300.0).unwrap();
    let page = &pages[0];
    assert_eq!((page.width, page.height), (1250, 833));
    let pixel = |x: u32, y: u32| {
        let i = ((y * page.width + x) * 4) as usize;
        [page.rgba[i], page.rgba[i + 1], page.rgba[i + 2]]
    };
    // The square spans 250–290 × 10–50 pt; its centre, 270 × 30 pt, is
    // device (1125, 708).
    assert_eq!(pixel(1125, 708), [0, 0, 255]);
}
