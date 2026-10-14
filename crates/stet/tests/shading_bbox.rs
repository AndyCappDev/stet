// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A shading's `BBox` "is applied as a temporary clipping boundary when the
//! shading is painted, in addition to the current clipping path" (PLRM 3,
//! 4.9.3). A function-based shading (type 1) is sampled into an image over
//! its whole domain, and its `BBox` was dropped.

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
const BLUE: [u8; 3] = [0, 0, 255];

/// A red function-based shading whose domain covers the page, cut to
/// [`BOX`].
const SHADING: &str = "<< /ShadingType 1 /ColorSpace /DeviceRGB /Domain [0 1 0 1] \
                       /Matrix [200 0 0 200 0 0] /BBox [50 50 120 100] \
                       /Function << /FunctionType 0 /Domain [0 1 0 1] /Range [0 1 0 1 0 1] \
                       /Size [2 2] /BitsPerSample 8 /DataSource <FF0000FF0000FF0000FF0000> >> >>";

/// The shading's `BBox`, in default user space.
const BOX: [usize; 4] = [50, 50, 120, 100];

/// Red inside [`BOX`] and white outside, a pixel clear of its edge either
/// way.
fn assert_cut_to_box(rgba: &[u8], what: &str) {
    for y in 0..SIZE {
        for x in 0..SIZE {
            let inside =
                (BOX[0] + 1..BOX[2] - 1).contains(&x) && (BOX[1] + 1..BOX[3] - 1).contains(&y);
            let outside =
                !(BOX[0] - 1..=BOX[2]).contains(&x) || !(BOX[1] - 1..=BOX[3]).contains(&y);
            if inside {
                assert_eq!(at(rgba, x, y), RED, "{what}: ({x}, {y}) inside");
            } else if outside {
                assert_eq!(at(rgba, x, y), WHITE, "{what}: ({x}, {y}) outside");
            }
        }
    }
}

#[test]
fn shfill_is_cut_to_the_bbox() {
    assert_cut_to_box(&render(&format!("{SHADING} shfill")), "shfill");
}

#[test]
fn a_shading_pattern_is_cut_to_the_bbox() {
    let rgba = render(&format!(
        "<< /PatternType 2 /Shading {SHADING} >> matrix makepattern setpattern \
         0 0 {SIZE} {SIZE} rectfill"
    ));
    assert_cut_to_box(&rgba, "shading pattern");
}

/// The `BBox` clip ends with the shading: what is painted next is cut only
/// by the clip the program set.
#[test]
fn the_bbox_clip_ends_with_the_shading() {
    let rgba = render(&format!(
        "{SHADING} shfill 0 0 1 setrgbcolor 150 150 20 20 rectfill"
    ));
    assert_eq!(at(&rgba, 160, 160), BLUE);

    let rgba = render(&format!(
        "0 0 155 155 rectclip {SHADING} shfill 0 0 1 setrgbcolor 150 150 20 20 rectfill"
    ));
    assert_eq!(at(&rgba, 152, 152), BLUE);
    assert_eq!(at(&rgba, 160, 160), WHITE);
}
