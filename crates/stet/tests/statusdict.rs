// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Level 1 printer controls that print drivers call through statusdict.

use stet::Interpreter;

#[test]
fn print_driver_tray_and_duplex_calls_are_accepted() {
    let src = b"%!PS-Adobe-3.0\n\
        0 statusdict /setpapertray get exec\n\
        statusdict begin a4tray true setduplexmode false settumble papertray pop duplexmode pop tumble pop end\n\
        statusdict /manualfeed true put\n\
        0 0 1 setrgbcolor 20 20 100 100 rectfill showpage\n";
    let mut interp = Interpreter::builder().suppress_output().build();
    let pages = interp.render(src, 18.0).expect("renders");
    assert_eq!(pages.len(), 1);
}
