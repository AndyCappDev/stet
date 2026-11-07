// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `stet_render::render_region_transparent`: the unpainted part of a region
//! stays transparent, for a consumer that places it over other content.

use stet::Interpreter;
use stet_render::{render_region, render_region_transparent};

#[test]
fn unpainted_pixels_stay_transparent() {
    // A blue square in the lower-left quarter of a 100 x 100 pt page, at 72 dpi.
    let ps = b"%!PS\n<< /PageSize [100 100] >> setpagedevice\n0 0 1 setrgbcolor 0 0 50 50 rectfill showpage\n";
    let pages = Interpreter::builder()
        .suppress_output()
        .build()
        .render_to_display_list(ps, 72.0)
        .unwrap();
    let list = &pages[0].display_list;

    let px = |buf: &[u8], x: usize, y: usize| buf[(y * 100 + x) * 4..][..4].to_vec();
    let clear = render_region_transparent(list, 0.0, 0.0, 100.0, 100.0, 100, 100, 72.0, None, false);
    let white = render_region(list, 0.0, 0.0, 100.0, 100.0, 100, 100, 72.0, None, None, false);

    // Rows run top down: (10, 90) is inside the square, (90, 10) outside.
    assert_eq!(px(&clear, 10, 90), vec![0, 0, 255, 255], "painted: opaque blue");
    assert_eq!(px(&clear, 90, 10), vec![0, 0, 0, 0], "unpainted: transparent");
    assert_eq!(px(&white, 90, 10), vec![255, 255, 255, 255], "render_region keeps its white paper");
}
