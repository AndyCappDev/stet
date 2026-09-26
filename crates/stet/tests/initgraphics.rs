// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `initgraphics` resets only the parameters PLRM 3e lists for it.
//!
//! The graphics-state side is covered by `unit_tests/`; this file holds what
//! only shows up once a page is rendered.
#![cfg(feature = "render")]

use stet::Interpreter;

/// `initgraphics` used to rebuild the whole graphics state, putting the
/// clip's change counter back to zero. `grestore` re-emits the device clip
/// only when the counter differs from the saved one, so a clip set after
/// `initgraphics` inside a `gsave` could carry the saved counter by
/// coincidence and outlive the `grestore`: here the fill escaped the
/// 50 × 50 clip into the inner one.
#[test]
fn grestore_undoes_a_clip_set_after_initgraphics() {
    let mut interp = Interpreter::new();
    let pages = interp
        .render(
            br#"%!PS
<< /PageSize [120 120] >> setpagedevice
0 0 50 50 rectclip
gsave initgraphics 10 10 100 100 rectclip grestore
1 0 0 setrgbcolor 0 0 120 120 rectfill
showpage
"#,
            72.0,
        )
        .unwrap();
    let page = &pages[0];
    let rgb = |x: u32, y: u32| {
        let i = (((page.height - 1 - y) * page.width + x) * 4) as usize;
        [page.rgba[i], page.rgba[i + 1], page.rgba[i + 2]]
    };
    assert_eq!(rgb(25, 25), [255, 0, 0], "inside the clip");
    assert_eq!(rgb(80, 80), [255, 255, 255], "outside the clip");
}
