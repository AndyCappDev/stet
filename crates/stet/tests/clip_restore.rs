// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The clip is the intersection of every region set since `initclip`, and
//! it is that whole intersection that `grestore`, `restore`, `setgstate`
//! and `cliprestore` bring back.
//!
//! Only the most recent region was kept, so once a nested `gsave … clip …
//! grestore` had run, everything clipped before the last `clip` was let
//! through, and an `eoclip` came back as a non-zero winding clip.

use stet::Interpreter;

const SIZE: usize = 200;

fn render(body: &str) -> Vec<u8> {
    let program =
        format!("%!PS\n<< /PageSize [{SIZE} {SIZE}] >> setpagedevice\n{body}\nshowpage\n");
    let mut pages = Interpreter::new()
        .render(program.as_bytes(), 72.0)
        .expect("program runs");
    assert_eq!(pages.len(), 1);
    pages.remove(0).rgba
}

/// The pixel at a point given in PostScript's default user space.
fn at(rgba: &[u8], x: usize, y: usize) -> [u8; 3] {
    let i = ((SIZE - 1 - y) * SIZE + x) * 4;
    [rgba[i], rgba[i + 1], rgba[i + 2]]
}

const WHITE: [u8; 3] = [255, 255, 255];
const RED: [u8; 3] = [255, 0, 0];
const PAINT: &str = "1 0 0 setrgbcolor 0 0 200 200 rectfill";

/// Two overlapping discs, centred (70,100) and (130,100): the clip is the
/// lens they share. (40,100) is in the first only, (160,100) in the second
/// only.
const LENS: &str = "newpath 70 100 60 0 360 arc clip newpath 130 100 60 0 360 arc clip newpath";

fn assert_lens(rgba: &[u8], what: &str) {
    assert_eq!(at(rgba, 100, 100), RED, "{what}: inside both");
    assert_eq!(at(rgba, 40, 100), WHITE, "{what}: inside the first only");
    assert_eq!(at(rgba, 160, 100), WHITE, "{what}: inside the second only");
}

#[test]
fn without_a_restore_the_clips_intersect() {
    assert_lens(&render(&format!("{LENS} {PAINT}")), "plain");
}

#[test]
fn grestore_brings_back_every_region() {
    let rgba = render(&format!("{LENS} gsave 0 0 20 20 rectclip grestore {PAINT}"));
    assert_lens(&rgba, "grestore");
}

#[test]
fn restore_brings_back_every_region() {
    let rgba = render(&format!("{LENS} save 0 0 20 20 rectclip restore {PAINT}"));
    assert_lens(&rgba, "restore");
}

#[test]
fn setgstate_brings_back_every_region() {
    let rgba = render(&format!(
        "{LENS} /G gstate def initclip 0 0 20 20 rectclip G setgstate {PAINT}"
    ));
    assert_lens(&rgba, "setgstate");
}

#[test]
fn cliprestore_brings_back_every_region() {
    let rgba = render(&format!(
        "{LENS} clipsave 0 0 20 20 rectclip cliprestore {PAINT}"
    ));
    assert_lens(&rgba, "cliprestore");
}

#[test]
fn an_even_odd_clip_comes_back_even_odd() {
    // A ring: the inner disc is outside the clip.
    let rgba = render(&format!(
        "newpath 100 100 80 0 360 arc 100 100 40 0 360 arc eoclip newpath\n\
         gsave 0 0 20 20 rectclip grestore {PAINT}"
    ));
    assert_eq!(at(&rgba, 100, 100), WHITE);
    assert_eq!(at(&rgba, 100, 160), RED);
}

#[test]
fn nested_rectangles_come_back_as_what_they_share() {
    let rgba = render(&format!(
        "0 0 120 200 rectclip 0 0 200 120 rectclip 60 60 140 140 rectclip\n\
         gsave 0 0 20 20 rectclip grestore {PAINT}"
    ));
    assert_eq!(at(&rgba, 90, 90), RED);
    assert_eq!(at(&rgba, 150, 90), WHITE);
    assert_eq!(at(&rgba, 90, 150), WHITE);
    assert_eq!(at(&rgba, 30, 30), WHITE);
}

#[test]
fn clippath_of_nested_rectangles_is_what_they_share() {
    let rgba = render(
        "0 0 120 200 rectclip 60 0 140 200 rectclip clippath pathbbox\n\
         /ury exch def /urx exch def /lly exch def /llx exch def initclip\n\
         llx 60 eq urx 120 eq and lly 0 eq and ury 200 eq and\n\
         { 1 0 0 setrgbcolor 0 0 200 200 rectfill } if",
    );
    assert_eq!(at(&rgba, 100, 100), RED);
}

#[test]
fn rectangles_that_share_nothing_clip_everything() {
    let rgba = render(&format!(
        "0 0 50 50 rectclip 100 100 50 50 rectclip gsave 0 0 20 20 rectclip grestore {PAINT}"
    ));
    for (x, y) in [(25, 25), (125, 125), (75, 75)] {
        assert_eq!(at(&rgba, x, y), WHITE);
    }
}

#[test]
fn initclip_forgets_every_region() {
    let rgba = render(&format!(
        "{LENS} initclip gsave 0 0 20 20 rectclip grestore {PAINT}"
    ));
    assert_eq!(at(&rgba, 10, 190), RED);
}

#[test]
fn a_long_run_of_clips_is_restored_and_dropped() {
    // Rotated, so no two of them fold into one rectangle.
    let rgba = render(&format!(
        "gsave 100 100 translate 20000 {{ 0.01 rotate -90 -90 180 180 rectclip }} repeat\n\
         gsave 0 0 1 1 rectclip grestore grestore {PAINT}"
    ));
    assert_eq!(at(&rgba, 100, 100), RED);
}
