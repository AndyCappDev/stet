// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `InterpreterBuilder::page_background` — pages from `render` left clear
//! instead of on white paper, for artwork placed over other content.

#![cfg(feature = "render")]

use stet::{Interpreter, PageBackground, RenderedPage};

/// Two 100×100 pt pages, each with a red square in the lower-left quarter.
const TWO_PAGES: &[u8] = b"%!PS-Adobe-3.0\n\
    << /PageSize [100 100] >> setpagedevice\n\
    1 0 0 setrgbcolor 0 0 50 50 rectfill showpage\n\
    1 0 0 setrgbcolor 0 0 50 50 rectfill showpage\n";

/// The pixel at (x, y), counted from the top-left.
fn pixel(page: &RenderedPage, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * page.width + x) * 4) as usize;
    page.rgba[i..i + 4].try_into().unwrap()
}

#[test]
fn white_paper_by_default() {
    let pages = Interpreter::builder()
        .no_icc()
        .build()
        .render(TWO_PAGES, 72.0)
        .unwrap();
    assert_eq!(pages.len(), 2);
    for page in &pages {
        assert_eq!(pixel(page, 75, 25), [255, 255, 255, 255]);
        assert_eq!(pixel(page, 25, 75), [255, 0, 0, 255]);
    }
}

#[test]
fn transparent_leaves_every_page_clear() {
    let pages = Interpreter::builder()
        .no_icc()
        .page_background(PageBackground::Transparent)
        .build()
        .render(TWO_PAGES, 72.0)
        .unwrap();
    assert_eq!(pages.len(), 2);
    for (n, page) in pages.iter().enumerate() {
        assert_eq!(
            pixel(page, 75, 25),
            [0, 0, 0, 0],
            "page {} unpainted",
            n + 1
        );
        assert_eq!(pixel(page, 25, 75), [255, 0, 0, 255], "page {} mark", n + 1);
    }
}

/// An anti-aliased edge is partly covered: straight alpha keeps its colour
/// at full strength and lowers only alpha, where premultiplied pixels would
/// darken it.
#[test]
fn transparent_edges_are_straight_alpha() {
    let ps = b"%!PS-Adobe-3.0\n\
        << /PageSize [100 100] >> setpagedevice\n\
        1 0 0 setrgbcolor 0 0 50.5 50 rectfill showpage\n";
    let pages = Interpreter::builder()
        .no_icc()
        .page_background(PageBackground::Transparent)
        .build()
        .render(ps, 72.0)
        .unwrap();
    let [r, g, b, a] = pixel(&pages[0], 50, 75);
    assert!(
        a > 0 && a < 255,
        "edge pixel should be partly covered, alpha {a}"
    );
    assert_eq!((r, g, b), (255, 0, 0));
}
