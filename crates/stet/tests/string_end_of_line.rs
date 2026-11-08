// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A string literal that spans lines of a CRLF file holds one newline per line end (PLRM 3.2.2), as in
//! Ghostscript. Programs that walk a string character by character (the "tiny raytracer" sample
//! turns every character below `$` into `]`) went wrong on a file saved with CRLF.

use stet::Interpreter;

fn colour_of(src: &[u8]) -> [u8; 3] {
    let mut interp = Interpreter::builder().suppress_output().build();
    let pages = interp.render(src, 18.0).expect("renders");
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
fn a_crlf_in_a_string_is_one_newline() {
    let src = b"%!PS\r\n/s (a\r\nb\rc) def\r\n\
        s length 5 eq s 1 get 10 eq and s 3 get 10 eq and { 0 0 1 } { 1 0 0 } ifelse setrgbcolor\r\n\
        0 0 100 100 rectfill showpage\r\n";
    assert_eq!(colour_of(src), [0, 0, 255]);
}
