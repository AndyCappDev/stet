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

/// The colour at the centre of a 100 x 100 pt page.
fn centre(src: &[u8]) -> [u8; 3] {
    let mut interp = Interpreter::builder().suppress_output().build();
    let pages = interp.render(src, 72.0).expect("renders");
    assert_eq!(pages.len(), 1);
    let w = pages[0].width as usize;
    let at = (50 * w + 50) * 4;
    [
        pages[0].rgba[at],
        pages[0].rgba[at + 1],
        pages[0].rgba[at + 2],
    ]
}

/// A `.begintransparencygroup` that fails records nothing: the outer group still composites at
/// the opacity current when it began (solid red), not the failed begin's 0.3.
#[test]
fn a_failed_begin_records_nothing() {
    let src = b"%!PS\n<< /PageSize [100 100] >> setpagedevice\n\
        0 0 1 setrgbcolor 0 0 100 100 rectfill\n\
        1 .setfillconstantalpha\n\
        << >> 0 0 100 100 .begintransparencygroup\n\
          .3 .setfillconstantalpha\n\
          { << >> 0 0 (x) 100 .begintransparencygroup } stopped pop clear\n\
          1 .setfillconstantalpha\n\
          1 0 0 setrgbcolor 0 0 100 100 rectfill\n\
        .endtransparencygroup\n\
        showpage\n";
    assert_eq!(centre(src), [255, 0, 0]);
}

/// The opacity a group composites with belongs to the group: a plain `endtransparencygroup` that
/// closes a group opened with Ghostscript's form keeps the opacity current at its begin, and a
/// plain group nested inside still reads its own at end.
#[test]
fn the_opacity_at_begin_stays_with_the_group() {
    let src = b"%!PS\n<< /PageSize [100 100] >> setpagedevice\n\
        0 0 1 setrgbcolor 0 0 100 100 rectfill\n\
        << >> 0 0 100 100 .begintransparencygroup\n\
          .5 .setfillconstantalpha\n\
          << >> begintransparencygroup\n\
            1 .setfillconstantalpha 1 0 0 setrgbcolor 0 0 100 100 rectfill\n\
          endtransparencygroup\n\
        endtransparencygroup\n\
        showpage\n";
    // The inner group composites at 1 (its end), the outer at 1 (its begin): solid red.
    assert_eq!(centre(src), [255, 0, 0]);
}

/// Ghostscript's form checks its operands before it opens anything.
#[test]
fn ghostscripts_form_checks_its_operands() {
    let src = b"%!PS\n<< /PageSize [100 100] >> setpagedevice\n\
        { /x 0 0 100 100 .begintransparencygroup } stopped not { 1 0 div } if\n\
        $error /errorname get /typecheck ne { 1 0 div } if clear\n\
        { 0 100 .begintransparencygroup } stopped not { 1 0 div } if\n\
        $error /errorname get /stackunderflow ne { 1 0 div } if clear\n\
        { << >> 0 0 /x 100 .begintransparencygroup } stopped not { 1 0 div } if\n\
        $error /errorname get /typecheck ne { 1 0 div } if clear\n\
        { endtransparencygroup } stopped not { 1 0 div } if clear\n\
        0 0 1 setrgbcolor 0 0 100 100 rectfill showpage\n";
    assert_eq!(centre(src), [0, 0, 255]);
}
