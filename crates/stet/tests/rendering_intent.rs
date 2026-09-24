// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PostScript rendering intents reach the display list in the documented
//! encoding (`stet_graphics::rendering_intent`).
//!
//! The rasterizer once decoded that byte with the PDF reader's private
//! numbering, so a PostScript program's default RelativeColorimetric was
//! applied as Perceptual; and images carried a hardcoded intent regardless of
//! `setrenderingintent`.

use stet::{DisplayElement, Interpreter};
use stet_graphics::rendering_intent as ri;

/// Run `body` as a one-page program and return the intent byte of every
/// `Fill` and `Image` element, in paint order.
fn painted_intents(body: &str) -> Vec<(&'static str, u8)> {
    let src = format!("%!PS-Adobe-3.0\n{body}\nshowpage\n");
    let mut interp = Interpreter::new();
    let pages = interp.render_to_display_list(src.as_bytes(), 72.0).unwrap();
    assert_eq!(pages.len(), 1);
    pages[0]
        .display_list
        .elements()
        .iter()
        .filter_map(|e| match e {
            DisplayElement::Fill { params, .. } => Some(("fill", params.rendering_intent)),
            DisplayElement::Image { params, .. } => Some(("image", params.rendering_intent)),
            _ => None,
        })
        .collect()
}

#[test]
fn fills_carry_each_intent_in_the_documented_encoding() {
    let intents = painted_intents(
        "0 0 10 10 rectfill\n\
         /AbsoluteColorimetric setrenderingintent 0 0 10 10 rectfill\n\
         /Perceptual setrenderingintent 0 0 10 10 rectfill\n\
         /Saturation setrenderingintent 0 0 10 10 rectfill\n\
         /RelativeColorimetric setrenderingintent 0 0 10 10 rectfill",
    );
    assert_eq!(
        intents,
        [
            ("fill", ri::RELATIVE_COLORIMETRIC), // PLRM initial value
            ("fill", ri::ABSOLUTE_COLORIMETRIC),
            ("fill", ri::PERCEPTUAL),
            ("fill", ri::SATURATION),
            ("fill", ri::RELATIVE_COLORIMETRIC),
        ]
    );
}

#[test]
fn images_follow_setrenderingintent() {
    let intents = painted_intents(
        "10 10 scale\n\
         1 1 8 [1 0 0 1 0 0] {<ff0000>} false 3 colorimage\n\
         /Perceptual setrenderingintent\n\
         1 1 8 [1 0 0 1 0 0] {<ff0000>} false 3 colorimage",
    );
    assert_eq!(
        intents,
        [
            ("image", ri::RELATIVE_COLORIMETRIC),
            ("image", ri::PERCEPTUAL)
        ]
    );
}

#[test]
fn currentrenderingintent_round_trips() {
    // Feeding currentrenderingintent's result back to setrenderingintent
    // must leave the intent unchanged.
    let intents = painted_intents(
        "/Saturation setrenderingintent\n\
         currentrenderingintent setrenderingintent 0 0 10 10 rectfill",
    );
    assert_eq!(intents, [("fill", ri::SATURATION)]);
}
