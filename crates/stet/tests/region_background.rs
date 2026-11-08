// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `stet_render::render_region_with_background`: with a transparent page the
//! unpainted part of a region stays clear, for a consumer that places it over
//! other content; with white paper it is what `render_region` gives.

use stet::Interpreter;
use stet_render::{PageBackground, render_region, render_region_with_background};

fn region(list: &stet::PsDisplayList, bg: PageBackground) -> Vec<u8> {
    render_region_with_background(
        list, 0.0, 0.0, 100.0, 100.0, 100, 100, 72.0, None, None, false, bg,
    )
}

#[test]
fn unpainted_pixels_stay_transparent() {
    // A blue rectangle over the left half of a 100 x 100 pt page, at 72 dpi, its right edge half
    // way across pixel column 50.
    let ps = b"%!PS\n<< /PageSize [100 100] >> setpagedevice\n0 0 1 setrgbcolor 0 0 50.5 50 rectfill showpage\n";
    let pages = Interpreter::builder()
        .suppress_output()
        .build()
        .render_to_display_list(ps, 72.0)
        .unwrap();
    let list = &pages[0].display_list;

    let px = |buf: &[u8], x: usize, y: usize| buf[(y * 100 + x) * 4..][..4].to_vec();
    let clear = region(list, PageBackground::Transparent);
    let white = region(list, PageBackground::White);

    // Rows run top down: (10, 90) is inside the rectangle, (90, 10) outside.
    assert_eq!(
        px(&clear, 10, 90),
        vec![0, 0, 255, 255],
        "painted: opaque blue"
    );
    assert_eq!(
        px(&clear, 90, 10),
        vec![0, 0, 0, 0],
        "unpainted: transparent"
    );

    // The half-covered column is straight alpha: still pure blue, half opaque (premultiplied it
    // would be half as bright).
    let edge = px(&clear, 50, 90);
    assert!((100..=155).contains(&edge[3]), "partly covered: {edge:?}");
    assert_eq!(
        &edge[..3],
        &[0, 0, 255],
        "straight alpha keeps the colour: {edge:?}"
    );

    // White paper is what render_region gives.
    assert_eq!(px(&white, 90, 10), vec![255, 255, 255, 255], "white paper");
    assert_eq!(
        white,
        render_region(
            list, 0.0, 0.0, 100.0, 100.0, 100, 100, 72.0, None, None, false
        )
    );
}

#[test]
fn an_empty_region_is_clear_or_white() {
    let pages = Interpreter::builder()
        .suppress_output()
        .build()
        .render_to_display_list(b"%!PS\n0 0 50 50 rectfill showpage\n", 72.0)
        .unwrap();
    let list = &pages[0].display_list;
    let empty = |bg| {
        render_region_with_background(list, 0.0, 0.0, 0.0, 10.0, 4, 4, 72.0, None, None, false, bg)
    };
    assert!(empty(PageBackground::Transparent).iter().all(|&b| b == 0));
    assert!(empty(PageBackground::White).iter().all(|&b| b == 0xFF));
}
