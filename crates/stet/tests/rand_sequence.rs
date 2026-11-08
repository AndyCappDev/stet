// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `rand`, `srand` and `rrand` give the sequence Ghostscript gives (Park and Miller's minimal
//! standard, as Adobe documents it), so a program that draws with `rand` renders as it does there.
//! The expected values are Ghostscript 10's.

use stet::Interpreter;

/// Blue when every check in `checks` (each leaves a boolean) is true, red otherwise.
fn all_true(checks: &[&str]) -> [u8; 3] {
    let body: Vec<String> = checks.iter().map(|c| format!("{c} and")).collect();
    let src = format!(
        "%!PS\ntrue {} {{ 0 0 1 }} {{ 1 0 0 }} ifelse setrgbcolor 0 0 100 100 rectfill showpage\n",
        body.join(" ")
    );
    let mut interp = Interpreter::builder().suppress_output().build();
    let pages = interp.render(src.as_bytes(), 18.0).expect("renders");
    assert_eq!(pages.len(), 1);
    let w = pages[0].width as usize;
    let at = ((pages[0].height as usize - 5) * w + 5) * 4;
    [
        pages[0].rgba[at],
        pages[0].rgba[at + 1],
        pages[0].rgba[at + 2],
    ]
}

#[test]
fn the_unseeded_sequence_is_the_minimal_standard() {
    let checks = [
        "rrand 1 eq",
        "rand 16807 eq",
        "rand 282475249 eq",
        "rrand 282475249 eq",
    ];
    assert_eq!(all_true(&checks), [0, 0, 255]);
}

#[test]
fn seeds_are_brought_into_range() {
    let checks = [
        "0 srand rrand 1 eq",
        "-5 srand rrand 6 eq",
        "-5 srand rand 100842 eq",
        "2147483647 srand rrand 2147483646 eq",
        "2147483647 srand rand 2147466840 eq",
        "42 srand rand 705894 eq",
        "42 srand rand pop rand 1126542223 eq",
    ];
    assert_eq!(all_true(&checks), [0, 0, 255]);
}

#[test]
fn rrand_resumes_the_sequence() {
    let checks = ["7 srand rand pop rrand rand exch srand rand eq"];
    assert_eq!(all_true(&checks), [0, 0, 255]);
}
