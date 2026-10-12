// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Where two edges of a path lie on the same line, the pixels along it get
//! the coverage of the shape and no more.
//!
//! The fill rule decides what is inside from how many edges have been
//! crossed, so it has to be applied to the edges in order. Adding up what
//! each edge covers of a pixel and applying the rule to the total goes
//! wrong wherever two edges share a pixel: two edges 0.7 of the way across
//! add up to 1.4, which the even-odd rule reads as 0.6 where nothing is
//! inside, and the non-zero rule as 1.0 where 0.7 is.

use stet_fonts::geometry::{Matrix, PathSegment, PsPath};
use stet_graphics::color::{DeviceColor, FillRule};
use stet_graphics::device::{BgUcrState, FillParams, HalftoneState, TransferState};
use stet_graphics::display_list::{DisplayElement, DisplayList};
use stet_render::render_to_rgba;

const W: u32 = 80;
const H: u32 = 80;

/// A page with `segments` filled in black, as the darkness of each pixel:
/// 0 for bare paper, 255 for black.
fn filled(segments: Vec<PathSegment>, fill_rule: FillRule) -> Vec<u8> {
    let mut list = DisplayList::new();
    list.push(DisplayElement::Fill {
        path: PsPath { segments },
        params: FillParams {
            color: DeviceColor::from_gray(0.0),
            fill_rule,
            ctm: Matrix::identity(),
            is_text_glyph: false,
            overprint: false,
            overprint_mode: 0,
            opm_paired: false,
            painted_channels: 0,
            is_device_cmyk: false,
            spot_color: None,
            icc_color: None,
            rendering_intent: 0,
            transfer: TransferState::default(),
            halftone: HalftoneState::default(),
            bg_ucr: BgUcrState::default(),
            alpha: 1.0,
            blend_mode: 0,
            alpha_is_shape: false,
        },
    });
    render_to_rgba(&list, W, H, 72.0, None, false)
        .as_chunks::<4>()
        .0
        .iter()
        .map(|px| 255 - px[0])
        .collect()
}

/// A square with its corner at (`at`, `at`), drawn the same way round
/// whatever its size.
fn square(at: f64, size: f64) -> [PathSegment; 5] {
    [
        PathSegment::MoveTo(at, at),
        PathSegment::LineTo(at + size, at),
        PathSegment::LineTo(at + size, at + size),
        PathSegment::LineTo(at, at + size),
        PathSegment::ClosePath,
    ]
}

/// A square of 40 with a square of 20 in the same corner, the corner 0.3
/// of the way into a pixel so that the shared edges cross their pixels.
fn two_squares(fill_rule: FillRule) -> Vec<u8> {
    let segments = square(20.3, 40.0)
        .into_iter()
        .chain(square(20.3, 20.0))
        .collect();
    filled(segments, fill_rule)
}

/// The darkness of the large square's edge pixel: 0.7 of it is covered,
/// which is 178 across the pixel and, at four rows to the pixel, 191 down.
const EDGE: std::ops::RangeInclusive<u8> = 170..=195;

fn at(page: &[u8], x: u32, y: u32) -> u8 {
    page[(y * W + x) as usize]
}

/// The small square is a hole under the even-odd rule, and the edges it
/// shares with the large one leave nothing behind along them.
#[test]
fn a_hole_sharing_an_edge_leaves_no_line() {
    let page = two_squares(FillRule::EvenOdd);
    for along in 21..40 {
        assert_eq!(at(&page, 20, along), 0, "left edge at row {along}");
        assert_eq!(at(&page, along, 20), 0, "top edge at column {along}");
    }
    // Past the hole the same edges are the edge of the shape.
    for along in 42..60 {
        assert!(EDGE.contains(&at(&page, 20, along)));
        assert!(EDGE.contains(&at(&page, along, 20)));
    }
    assert_eq!(at(&page, 30, 30), 0);
    assert_eq!(at(&page, 50, 50), 255);
}

/// Under the non-zero rule the small square adds nothing, and the edges it
/// shares are as dark as the rest of the large square's edge: 0.7 of a
/// pixel, not all of it.
#[test]
fn a_shape_laid_over_itself_is_no_heavier_at_its_edge() {
    let page = two_squares(FillRule::NonZeroWinding);
    for along in 21..60 {
        let left = at(&page, 20, along);
        let top = at(&page, along, 20);
        assert!(EDGE.contains(&left), "left edge at row {along}: {left}");
        assert!(EDGE.contains(&top), "top edge at column {along}: {top}");
    }
    assert_eq!(at(&page, 30, 30), 255);
}
