// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Ghostscript's transparency extensions as Ghostscript's own example uses them
//! (`examples/transparency_example.ps`): the pdf14 device filter brackets, and
//! `paramdict llx lly urx ury .begintransparencygroup`.

use stet::Interpreter;

#[test]
fn ghostscripts_group_form_and_device_filter_render() {
    let src = b"%!PS\n\
        0 .pushpdf14devicefilter\n\
        0 0 1 setrgbcolor 0 0 100 100 rectfill\n\
        << >> 0 0 100 100 .begintransparencygroup\n\
        .5 .setfillconstantalpha 1 0 0 setrgbcolor 0 0 100 100 rectfill\n\
        .endtransparencygroup\n\
        .poppdf14devicefilter\n\
        showpage\n";
    let mut interp = Interpreter::builder().suppress_output().build();
    let pages = interp.render(src, 72.0).expect("renders");
    assert_eq!(pages.len(), 1);
    let w = pages[0].width as usize;
    let at = ((pages[0].height as usize - 50) * w + 50) * 4;
    let px = &pages[0].rgba[at..at + 3];
    // Half red over blue.
    assert!(
        (120..=135).contains(&px[0]) && px[1] < 10 && (120..=135).contains(&px[2]),
        "{px:?}"
    );
}

#[test]
fn the_dict_form_still_works() {
    let src = b"%!PS\n\
        0 0 1 setrgbcolor 0 0 100 100 rectfill\n\
        << /BBox [0 0 100 100] >> .begintransparencygroup\n\
        .5 .setfillconstantalpha 1 0 0 setrgbcolor 0 0 100 100 rectfill\n\
        .endtransparencygroup showpage\n";
    let mut interp = Interpreter::builder().suppress_output().build();
    let pages = interp.render(src, 72.0).expect("renders");
    let w = pages[0].width as usize;
    let at = ((pages[0].height as usize - 50) * w + 50) * 4;
    let px = &pages[0].rgba[at..at + 3];
    assert!(
        (120..=135).contains(&px[0]) && px[1] < 10 && (120..=135).contains(&px[2]),
        "{px:?}"
    );
}
