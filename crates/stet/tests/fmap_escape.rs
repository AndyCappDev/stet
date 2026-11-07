// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Type 0 fonts with Type 1 descendants and the PLRM's character mappings (5.10.1). Intergraph's
//! map plots show text in escape-mapped fonts (`FMapType 3`, `EscChar 255`) through `xyshow`: an
//! escape and its font number are two bytes but no character, so `<ff00 34>` takes one
//! displacement pair.

use stet::Interpreter;

const COMPOSITE: &str = "%!PS\n\
    /Big /Helvetica findfont [10 0 0 10 0 0] makefont def\n\
    12 dict begin /FontName /Comp def /FontType 0 def /WMode 0 def /FontMatrix matrix def\n\
    /Encoding [0 1] def /FMapType 3 def /EscChar 255 def\n\
    /FDepVector [/Helvetica findfont Big] def FontName currentdict end definefont pop\n\
    /Comp findfont 20 scalefont setfont\n";

/// The colour of the pixel `x` points right and `y` points up from the bottom-left corner.
fn pixel(body: &str, x: usize, y: usize) -> [u8; 3] {
    let src = format!("{COMPOSITE}{body}\nshowpage\n");
    let mut interp = Interpreter::builder().suppress_output().build();
    let pages = interp.render(src.as_bytes(), 72.0).expect("renders");
    assert_eq!(pages.len(), 1);
    let (w, h) = (pages[0].width as usize, pages[0].height as usize);
    let at = ((h - 1 - y) * w + x) * 4;
    [
        pages[0].rgba[at],
        pages[0].rgba[at + 1],
        pages[0].rgba[at + 2],
    ]
}

#[test]
fn xyshow_takes_one_pair_per_character_not_per_byte() {
    // Two characters after an escape: four numbers. The current point ends 30 + 40 points on.
    let body = "10 10 moveto <ff00 4142> [30 0 40 0] xyshow currentpoint pop 80 eq \
        { 0 0 1 } { 1 0 0 } ifelse setrgbcolor 0 300 20 20 rectfill";
    assert_eq!(pixel(body, 5, 305), [0, 0, 255]);
}

#[test]
fn an_escape_selects_the_font_for_the_rest_of_the_string() {
    // Font 1 is Helvetica ten times larger: an I 144 points high, its stem 18-38 points right of x 100.
    let body = "100 100 moveto <ff01 49> show";
    assert_eq!(pixel(body, 128, 200), [0, 0, 0], "a tall I from font 1");
    let body = "100 100 moveto <49> show";
    assert_eq!(
        pixel(body, 128, 200),
        [255, 255, 255],
        "font 0 by default: a small I"
    );
}
