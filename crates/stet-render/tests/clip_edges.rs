// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The edges of clipped shapes: smooth where nothing is beside them, and
//! closed where two clipped regions meet.
//!
//! A clip with a free edge covers the pixels along it in part. Two clips
//! that share an edge each cut it hard, so that painting one and then the
//! other leaves none of the page showing between them.

use stet_fonts::geometry::{Matrix, PathSegment, PsPath};
use stet_graphics::color::{DeviceColor, FillRule};
use stet_graphics::device::{ClipParams, FillParams};
use stet_graphics::display_list::{DisplayElement, DisplayList};
use stet_render::render_to_rgba;

const W: u32 = 200;
const H: u32 = 200;

fn polygon(points: &[(f64, f64)]) -> PsPath {
    let mut segments = vec![PathSegment::MoveTo(points[0].0, points[0].1)];
    segments.extend(points[1..].iter().map(|p| PathSegment::LineTo(p.0, p.1)));
    segments.push(PathSegment::ClosePath);
    PsPath { segments }
}

fn clip(path: PsPath, fill_rule: FillRule) -> DisplayElement {
    DisplayElement::Clip {
        path,
        params: ClipParams {
            fill_rule,
            ctm: Matrix::default(),
            stroke_params: None,
        },
    }
}

/// Black over the whole page, to be seen through whatever clip is set.
fn flood() -> DisplayElement {
    DisplayElement::Fill {
        path: polygon(&[
            (0.0, 0.0),
            (W as f64, 0.0),
            (W as f64, H as f64),
            (0.0, H as f64),
        ]),
        params: FillParams {
            color: DeviceColor::from_gray(0.0),
            fill_rule: FillRule::NonZeroWinding,
            ..FillParams::default()
        },
    }
}

/// Each region painted black through its own clip, one after another, on
/// white paper. Returns the red channel of every pixel.
fn paint(regions: Vec<Vec<DisplayElement>>, no_aa: bool) -> Vec<u8> {
    let mut list = DisplayList::new();
    for clips in regions {
        list.push(DisplayElement::InitClip);
        for c in clips {
            list.push(c);
        }
        list.push(flood());
    }
    render_to_rgba(&list, W, H, 72.0, None, no_aa)
        .as_chunks::<4>()
        .0
        .iter()
        .map(|px| px[0])
        .collect()
}

fn at(page: &[u8], x: u32, y: u32) -> u8 {
    page[(y * W + x) as usize]
}

fn grays(page: &[u8]) -> usize {
    page.iter().filter(|&&v| v != 0 && v != 255).count()
}

// Slanted, so that no edge runs along the pixel grid.
fn left_tile() -> PsPath {
    polygon(&[(20.0, 20.0), (100.0, 24.0), (104.0, 180.0), (16.0, 176.0)])
}
fn right_tile() -> PsPath {
    polygon(&[(100.0, 24.0), (180.0, 20.0), (184.0, 176.0), (104.0, 180.0)])
}

const NZ: FillRule = FillRule::NonZeroWinding;

#[test]
fn a_clip_with_nothing_beside_it_has_a_smooth_edge() {
    let page = paint(vec![vec![clip(left_tile(), NZ)]], false);
    assert_eq!(at(&page, 60, 100), 0, "inside");
    assert_eq!(at(&page, 150, 100), 255, "outside");
    assert!(
        grays(&page) > 200,
        "an edge covered in part: {}",
        grays(&page)
    );
}

#[test]
fn with_anti_aliasing_off_a_clip_is_hard() {
    let page = paint(vec![vec![clip(left_tile(), NZ)]], true);
    assert_eq!(grays(&page), 0);
}

/// Along the edge two tiles share there is nothing but black; their other
/// edges are smooth.
#[test]
fn two_clips_that_meet_leave_no_paper_between_them() {
    let page = paint(
        vec![vec![clip(left_tile(), NZ)], vec![clip(right_tile(), NZ)]],
        false,
    );
    // The shared edge runs from (100, 24) to (104, 180).
    for y in 30..175 {
        for x in 95..110 {
            assert_eq!(at(&page, x, y), 0, "({x}, {y})");
        }
    }
    assert!(grays(&page) > 300, "the outer edges: {}", grays(&page));
}

/// The same with a curve: a disc, and around it a ring whose hole is that
/// circle drawn from other arcs.
#[test]
fn a_disc_and_the_ring_around_it_leave_no_paper_between_them() {
    let (cx, cy, r) = (100.0, 100.0, 50.0);
    let circle = |n: usize, turn: f64, sign: f64| {
        let k = 4.0 / 3.0 * (std::f64::consts::PI / (2.0 * n as f64)).tan();
        let angle = |i: usize| sign * (std::f64::consts::TAU * (i as f64 + turn) / n as f64);
        let on = |t: f64| (cx + r * t.cos(), cy + r * t.sin());
        let mut segments = vec![PathSegment::MoveTo(on(angle(0)).0, on(angle(0)).1)];
        for i in 0..n {
            let (t0, t1) = (angle(i), angle(i + 1));
            let (p0, p1) = (on(t0), on(t1));
            segments.push(PathSegment::CurveTo {
                x1: p0.0 - sign * k * r * t0.sin(),
                y1: p0.1 + sign * k * r * t0.cos(),
                x2: p1.0 + sign * k * r * t1.sin(),
                y2: p1.1 - sign * k * r * t1.cos(),
                x3: p1.0,
                y3: p1.1,
            });
        }
        segments.push(PathSegment::ClosePath);
        segments
    };
    let disc = PsPath {
        segments: circle(4, 0.0, 1.0),
    };
    let mut ring = polygon(&[(10.0, 12.0), (190.0, 10.0), (188.0, 190.0), (12.0, 188.0)]).segments;
    ring.extend(circle(7, 0.3, -1.0));
    let ring = PsPath { segments: ring };

    let regions = || {
        vec![
            vec![clip(disc.clone(), NZ)],
            vec![clip(ring.clone(), FillRule::EvenOdd)],
        ]
    };
    let page = paint(regions(), false);
    let hard = paint(regions(), true);
    // The two circles are not the same curve to the last digit, and a
    // pixel whose centre falls between them belongs to neither clip. That
    // is so with every edge hard, and smoothing must not add to it: round
    // the hole the page is as it is with anti-aliasing off.
    let mut bare = 0;
    for y in 20..180 {
        for x in 20..180 {
            assert_eq!(at(&page, x, y), at(&hard, x, y), "({x}, {y})");
            bare += usize::from(at(&page, x, y) != 0);
        }
    }
    assert!(bare < 8, "{bare} pixels between the circles");
    assert!(
        grays(&page) > 300,
        "the ring's outer edge: {}",
        grays(&page)
    );
}

/// A path set as the clip twice lets through what it lets through once:
/// an edge pixel is covered as far as the path covers it, not that far
/// twice over.
#[test]
fn a_clip_set_twice_is_the_clip_set_once() {
    let once = paint(vec![vec![clip(left_tile(), NZ)]], false);
    let twice = paint(
        vec![vec![clip(left_tile(), NZ), clip(left_tile(), NZ)]],
        false,
    );
    assert_eq!(once, twice);
}

/// A shape inside the box that bounds it, the box set first: the box is a
/// rectangle and hard, the shape's own edge smooth, and where they run
/// together the shape's coverage stands.
#[test]
fn a_clip_inside_another_keeps_its_smooth_edge() {
    let alone = paint(vec![vec![clip(left_tile(), NZ)]], false);
    let outer = polygon(&[(10.0, 10.0), (120.0, 14.0), (116.0, 190.0), (8.0, 186.0)]);
    let nested = paint(vec![vec![clip(outer, NZ), clip(left_tile(), NZ)]], false);
    assert_eq!(alone, nested);
}
