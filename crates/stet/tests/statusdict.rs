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

/// Run `body`, then paint and show one page. Each check in `body` divides by
/// zero when it fails, so a passing program renders exactly one page.
fn passes(body: &str) -> Vec<stet::RenderedPage> {
    let src =
        format!("%!PS-Adobe-3.0\n{body}\n0 0 1 setrgbcolor 20 20 100 100 rectfill showpage\n");
    let mut interp = Interpreter::builder().suppress_output().build();
    let pages = interp.render(src.as_bytes(), 72.0).expect("renders");
    assert_eq!(pages.len(), 1);
    pages
}

/// Choosing a tray chooses the paper in it, as in Ghostscript: each tray
/// selector sets the page size of the procedure it is named after.
#[test]
fn a_tray_selector_sets_its_page_size() {
    for (tray, w, h) in [
        ("lettertray", 612, 792),
        ("legaltray", 612, 1008),
        ("11x17tray", 792, 1224),
        ("ledgertray", 1224, 792),
        ("a3tray", 842, 1191),
        ("a4tray", 595, 842),
        ("a5tray", 420, 595),
        ("b4tray", 709, 1001),
        ("b5tray", 499, 709),
    ] {
        let pages = passes(&format!(
            "statusdict begin {tray} end\n\
             currentpagedevice /PageSize get aload pop {h} ne exch {w} ne or {{ 0 0 div }} if"
        ));
        assert_eq!((pages[0].width, pages[0].height), (w, h), "{tray}");
    }
}

/// A prolog that defines its own tray procedures (FrameMaker's do) does not
/// redirect statusdict's: the page size comes from userdict.
#[test]
fn a_tray_selector_ignores_the_programs_dictionary() {
    passes(
        "<< /a4 { 0 0 div } /a4tray { 0 0 div } >> begin\n\
         statusdict /a4tray get exec\n\
         end\n\
         currentpagedevice /PageSize get aload pop 842 ne exch 595 ne or { 0 0 div } if",
    );
}

/// Tray, duplex and tumble settings are recorded, and their queries answer
/// what was set last.
#[test]
fn settings_are_answered_by_their_queries() {
    passes(
        "statusdict /papertray get 1 ne { 0 0 div } if\n\
         statusdict /duplexmode get { 0 0 div } if\n\
         statusdict /tumble get { 0 0 div } if\n\
         statusdict begin 3 setpapertray true setduplexmode true settumble end\n\
         statusdict begin papertray 3 ne { 0 0 div } if\n\
         duplexmode not { 0 0 div } if tumble not { 0 0 div } if end",
    );
}
