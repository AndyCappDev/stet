// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Lines stroked side by side, each overlapping the last, cover the page
//! between them.
//!
//! That is how a blend was drawn before there were shadings: a few hundred
//! lines of slowly changing colour. Painted one after another with smooth
//! edges, each line is laid over a pixel as though the others might lie
//! anywhere in it, and a pixel two lines cover between them still shows a
//! little of the page. Where the lines are finer than the pixels every row
//! shows it, by an amount that drifts down the blend, and the blend is
//! striped.

use stet_fonts::geometry::{Matrix, PathSegment, PsPath};
use stet_graphics::color::{DashPattern, DeviceColor, LineCap, LineJoin};
use stet_graphics::device::{BgUcrState, HalftoneState, StrokeParams, TransferState};
use stet_graphics::display_list::{DisplayElement, DisplayList};
use stet_render::render_to_rgba;

const W: u32 = 120;
const H: u32 = 120;

/// A horizontal line from x = 20 to x = 100 at height `y`.
fn line(y: f64, width: f64, gray: f64, alpha: f64) -> DisplayElement {
    DisplayElement::Stroke {
        path: PsPath {
            segments: vec![PathSegment::MoveTo(20.0, y), PathSegment::LineTo(100.0, y)],
        },
        params: StrokeParams {
            color: DeviceColor::from_gray(gray),
            line_width: width,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            miter_limit: 10.0,
            dash_pattern: DashPattern::solid(),
            ctm: Matrix::identity(),
            stroke_adjust: false,
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
            alpha,
            blend_mode: 0,
            alpha_is_shape: false,
        },
    }
}

/// The page with `elements` painted on it, as the darkness of each pixel.
fn painted(elements: Vec<DisplayElement>) -> Vec<u8> {
    let mut list = DisplayList::new();
    for element in elements {
        list.push(element);
    }
    render_to_rgba(&list, W, H, 72.0, None, false)
        .as_chunks::<4>()
        .0
        .iter()
        .map(|px| 255 - px[0])
        .collect()
}

/// Black lines `width` thick, `step` apart, from y = 30 for 60 pixels.
fn band(width: f64, step: f64) -> Vec<DisplayElement> {
    (0..)
        .map(|i| 30.0 + step * f64::from(i))
        .take_while(|y| *y <= 90.0)
        .map(|y| line(y, width, 0.0, 1.0))
        .collect()
}

fn row(pixels: &[u8], y: u32) -> &[u8] {
    &pixels[(y * W + 30) as usize..(y * W + 90) as usize]
}

/// Lines finer than a pixel that overlap leave none of the page showing
/// between them, however they fall on the rows.
#[test]
fn overlapping_lines_finer_than_a_pixel_cover_the_page() {
    for (width, step) in [(0.63, 0.523), (0.8, 0.31), (0.9, 0.71)] {
        let pixels = painted(band(width, step));
        for y in 32..88 {
            assert!(
                row(&pixels, y).iter().all(|&dark| dark == 255),
                "lines {width} thick every {step}: row {y} is {:?}",
                &row(&pixels, y)[..4]
            );
        }
    }
}

/// Lines that only touch, edge to edge, leave next to none: what the
/// rasteriser gives two halves of a pixel does not always add up to all
/// of it, but the rows are alike.
#[test]
fn lines_that_only_touch_cover_the_page_evenly() {
    let pixels = painted(band(0.55, 0.55));
    for y in 32..88 {
        assert!(
            row(&pixels, y).iter().all(|&dark| dark >= 245),
            "row {y} is {:?}",
            &row(&pixels, y)[..4]
        );
    }
}

/// So do lines broader than a pixel, at the rows where one meets the next.
#[test]
fn overlapping_lines_broader_than_a_pixel_cover_the_page() {
    for (width, step) in [(1.75, 1.45), (3.3, 3.3), (5.2, 4.4), (2.5, 0.9)] {
        let pixels = painted(band(width, step));
        for y in 34..86 {
            assert!(
                row(&pixels, y).iter().all(|&dark| dark == 255),
                "lines {width} thick every {step}: row {y} is {:?}",
                &row(&pixels, y)[..4]
            );
        }
    }
}

/// The run keeps the edges a line alone has: its first and last rows are
/// covered in part, as far as the lines reach into them.
#[test]
fn a_run_keeps_smooth_edges() {
    // From 30.25 - 0.5 to 60.25 + 0.5 by lines one thick: the top row a
    // quarter covered, the bottom three quarters.
    let lines: Vec<_> = (0..=30)
        .map(|i| line(30.25 + f64::from(i), 1.0, 0.0, 1.0))
        .collect();
    let pixels = painted(lines);
    let top = row(&pixels, 29)[0];
    let bottom = row(&pixels, 60)[0];
    assert!((55..=72).contains(&top), "top row {top}");
    assert!((180..=200).contains(&bottom), "bottom row {bottom}");
    assert_eq!(row(&pixels, 45)[0], 255);
}

/// Lines that do not touch are each painted as a line alone is, and so
/// are lines that are not opaque, which do show what is under them.
#[test]
fn lines_apart_and_lines_that_are_not_opaque_are_painted_as_before() {
    let alone = |y: f64, alpha: f64| painted(vec![line(y, 0.63, 0.0, alpha)]);
    let darkest =
        |a: &[u8], b: &[u8]| -> Vec<u8> { a.iter().zip(b).map(|(a, b)| *a.max(b)).collect() };
    // Three pixels apart: no pixel is reached by both.
    let apart = painted(vec![line(40.3, 0.63, 0.0, 1.0), line(43.3, 0.63, 0.0, 1.0)]);
    assert_eq!(apart, darkest(&alone(40.3, 1.0), &alone(43.3, 1.0)));

    // Overlapping, but half transparent: each is laid over the other.
    let sheer = painted(vec![line(40.3, 0.63, 0.0, 0.5), line(40.8, 0.63, 0.0, 0.5)]);
    let laid_over: Vec<u8> = alone(40.3, 0.5)
        .iter()
        .zip(alone(40.8, 0.5))
        .map(|(&a, b)| {
            let (a, b) = (u32::from(a), u32::from(b));
            (a + b - (a * b + 127) / 255) as u8
        })
        .collect();
    for (i, (got, want)) in sheer.iter().zip(&laid_over).enumerate() {
        assert!(got.abs_diff(*want) <= 1, "pixel {i}: {got} for {want}");
    }
}

/// A blend drawn as lines falls steadily from one end to the other: no
/// row is lighter than the row before it by more than the colours are.
#[test]
fn a_blend_of_lines_has_no_stripes() {
    let lines: Vec<_> = (0..115)
        .map(|i| {
            line(
                30.0 + 0.523 * f64::from(i),
                0.63,
                0.2 + 0.006 * f64::from(i),
                1.0,
            )
        })
        .collect();
    let pixels = painted(lines);
    // The lines lighten down the rows.
    let column: Vec<u8> = (32..88).map(|y| row(&pixels, y)[10]).collect();
    for pair in column.windows(2) {
        assert!(
            pair[0] >= pair[1] && pair[0] - pair[1] <= 6,
            "{} then {} in {column:?}",
            pair[0],
            pair[1]
        );
    }
}

/// A white line against a black one is not a step of a blend: each is
/// painted as a line alone is, and the black keeps its edge.
#[test]
fn lines_of_unlike_colours_are_painted_as_before() {
    // Both finer than a pixel and sharing a row of pixels, each covering
    // part of it.
    let black = painted(vec![line(40.3, 0.6, 0.0, 1.0)]);
    let both = painted(vec![line(40.3, 0.6, 0.0, 1.0), line(40.75, 0.6, 1.0, 1.0)]);
    let white = painted(vec![line(40.75, 0.6, 0.0, 1.0)]);
    assert!(row(&black, 40)[0] > 100 && row(&white, 40)[0] > 100);
    // White laid over black at the white line's own coverage.
    for (i, ((&got, &black), &cover)) in both.iter().zip(&black).zip(&white).enumerate() {
        let want = u32::from(black) * (255 - u32::from(cover)) / 255;
        assert!(
            u32::from(got).abs_diff(want) <= 1,
            "pixel {i}: {got} for {want}"
        );
    }
}

/// Lines a fraction of their width apart cover about what their outline
/// covers, not what each would alone, added up. Lines this fine are spread
/// over the two rows nearest them by where their middle falls, so it is
/// the rows together that are measured.
#[test]
fn lines_nearly_on_top_of_each_other_cover_only_their_outline() {
    let covered = |count: i32| -> u32 {
        let lines: Vec<_> = (0..count)
            .map(|i| line(40.31 + 0.02 * f64::from(i), 0.6, 0.0, 1.0))
            .collect();
        let pixels = painted(lines);
        (38..43).map(|y| u32::from(row(&pixels, y)[5])).sum()
    };
    // One line 0.6 thick; five reach 0.68; twenty reach 0.98. A pixel
    // covered is 255.
    assert_eq!(covered(1), 153);
    assert!(
        (150..=215).contains(&covered(5)),
        "five cover {}",
        covered(5)
    );
    assert!(
        (230..=350).contains(&covered(20)),
        "twenty cover {}",
        covered(20)
    );
}
