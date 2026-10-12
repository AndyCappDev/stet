// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A fill whose path encloses no area is drawn as a thin line.
//!
//! A path that runs out along a line and back along it encloses nothing,
//! and the rasterizer paints no pixel for it. PostScript and PDF paint the
//! pixels such a path passes through, and files rely on it: PDFs rule
//! tables with rectangles of no height, and PostScript programs fill a
//! bare `moveto lineto`.

use stet_fonts::geometry::{Matrix, PathSegment, PsPath};
use stet_graphics::color::{DeviceColor, FillRule};
use stet_graphics::device::{BgUcrState, FillParams, HalftoneState, TransferState};
use stet_graphics::display_list::{DisplayElement, DisplayList};
use stet_render::render_to_rgba;

const W: u32 = 200;
const H: u32 = 100;

/// A page with `segments` filled in black, as the darkness of each pixel:
/// 0 for bare paper, 255 for black.
fn filled(segments: Vec<PathSegment>, is_text_glyph: bool, dpi: f64) -> Vec<u8> {
    let mut list = DisplayList::new();
    list.push(DisplayElement::Fill {
        path: PsPath { segments },
        params: FillParams {
            color: DeviceColor::from_gray(0.0),
            fill_rule: FillRule::NonZeroWinding,
            ctm: Matrix::identity(),
            is_text_glyph,
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
    render_to_rgba(&list, W, H, dpi, None, false)
        .as_chunks::<4>()
        .0
        .iter()
        .map(|px| 255 - px[0])
        .collect()
}

/// The ink in each column of the page, a fully black pixel counting 255.
fn column_ink(page: &[u8]) -> Vec<u32> {
    (0..W)
        .map(|x| (0..H).map(|y| u32::from(page[(y * W + x) as usize])).sum())
        .collect()
}

/// A line from (10, 20) to (190, 80), out and back.
fn sloped_line() -> Vec<PathSegment> {
    vec![
        PathSegment::MoveTo(10.0, 20.0),
        PathSegment::LineTo(190.0, 80.0),
        PathSegment::ClosePath,
    ]
}

/// Every column the path crosses is painted, and with one pixel's worth of
/// ink at 300 dpi: a line a pixel wide, drawn once and not once each way.
#[test]
fn a_line_out_and_back_is_drawn_once() {
    let ink = column_ink(&filled(sloped_line(), false, 300.0));
    for (x, &column) in ink.iter().enumerate().take(188).skip(12) {
        // A pixel wide across a slope of one in three.
        assert!(
            (240..=300).contains(&column),
            "column {x} has {column} of ink"
        );
    }
    assert_eq!(ink[5], 0);
    assert_eq!(ink[195], 0);
}

/// A PDF table rule: `10 50 180 0 re f`.
#[test]
fn a_rectangle_of_no_height_is_a_rule() {
    let page = filled(
        vec![
            PathSegment::MoveTo(10.0, 50.0),
            PathSegment::LineTo(190.0, 50.0),
            PathSegment::LineTo(190.0, 50.0),
            PathSegment::LineTo(10.0, 50.0),
            PathSegment::ClosePath,
        ],
        false,
        300.0,
    );
    let ink = column_ink(&page);
    for (x, &column) in ink.iter().enumerate().take(188).skip(12) {
        assert_eq!(column, 255, "column {x}");
    }
}

/// A curve out, and the same curve back.
#[test]
fn a_curve_out_and_back_is_drawn() {
    let page = filled(
        vec![
            PathSegment::MoveTo(10.0, 20.0),
            PathSegment::CurveTo {
                x1: 60.0,
                y1: 90.0,
                x2: 140.0,
                y2: 90.0,
                x3: 190.0,
                y3: 20.0,
            },
            PathSegment::CurveTo {
                x1: 140.0,
                y1: 90.0,
                x2: 60.0,
                y2: 90.0,
                x3: 10.0,
                y3: 20.0,
            },
            PathSegment::ClosePath,
        ],
        false,
        300.0,
    );
    let ink = column_ink(&page);
    for (x, &column) in ink.iter().enumerate().take(188).skip(12) {
        // Unbroken from end to end, whatever the slope.
        assert!(column >= 128, "column {x} has {column} of ink");
    }
    // The curve tops out near y = 72; nothing fills the space under it.
    assert_eq!(page[(40 * W + 100) as usize], 0);
}

/// A font's outlines carry stray contours of no area that were never meant
/// to show.
#[test]
fn a_glyph_of_no_area_paints_nothing() {
    let page = filled(sloped_line(), true, 300.0);
    assert!(page.iter().all(|&ink| ink == 0));
}

/// A shape with area is filled as it always was, and a subpath of no area
/// beside it is drawn too.
#[test]
fn a_shape_keeps_its_fill_beside_a_line() {
    let mut segments = vec![
        PathSegment::MoveTo(20.0, 20.0),
        PathSegment::LineTo(60.0, 20.0),
        PathSegment::LineTo(60.0, 60.0),
        PathSegment::LineTo(20.0, 60.0),
        PathSegment::ClosePath,
    ];
    segments.extend([
        PathSegment::MoveTo(100.0, 20.0),
        PathSegment::LineTo(180.0, 60.0),
        PathSegment::ClosePath,
    ]);
    let page = filled(segments, false, 300.0);
    assert_eq!(page[(40 * W + 40) as usize], 255, "inside the square");
    let ink = column_ink(&page);
    assert!(ink[140] >= 240, "the line: {}", ink[140]);
    assert_eq!(ink[80], 0, "between the two");
}
