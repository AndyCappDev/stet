// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A mesh whose triangles do not quite meet shows no specks of what is
//! under it.
//!
//! The patches of a gradient mesh are each cut into triangles of their own,
//! and where a long edge of one patch runs beside the short edges of several
//! the two sides are not the same line. A pixel whose centre falls in the
//! sliver between is inside no triangle.

use stet_fonts::geometry::Matrix;
use stet_graphics::color::DeviceColor;
use stet_graphics::device::{
    CMYK_ALL, MeshShadingParams, ShadingColorSpace, ShadingTriangle, ShadingVertex, TransferState,
};
use stet_graphics::display_list::{DisplayElement, DisplayList};
use stet_render::render_to_rgba;

const W: u32 = 200;
const H: u32 = 200;

/// A black mesh of two slanted tiles side by side, each a fan of
/// triangles, the second starting `apart` pixels to the right of where
/// the first ends.
fn two_tiles(apart: f64) -> Vec<u8> {
    let vertex = |p: (f64, f64)| ShadingVertex {
        x: p.0,
        y: p.1,
        color: DeviceColor::from_gray(0.0),
        raw_components: vec![0.0],
    };
    let fan = |points: [(f64, f64); 4]| {
        [[0, 1, 2], [0, 2, 3]].map(|[a, b, c]| ShadingTriangle {
            v0: vertex(points[a]),
            v1: vertex(points[b]),
            v2: vertex(points[c]),
        })
    };
    let left = fan([(20.0, 20.0), (100.0, 24.0), (104.0, 180.0), (16.0, 176.0)]);
    let right = fan([
        (100.0 + apart, 24.0),
        (180.0, 20.0),
        (184.0, 176.0),
        (104.0 + apart, 180.0),
    ]);
    let mut list = DisplayList::new();
    list.push(DisplayElement::MeshShading {
        params: MeshShadingParams {
            triangles: left.into_iter().chain(right).collect(),
            ctm: Matrix::default(),
            bbox: None,
            color_space: ShadingColorSpace::DeviceGray,
            overprint: false,
            overprint_mode: 0,
            painted_channels: CMYK_ALL,
            color_lut: None,
            color_lut_components: None,
            alpha: 1.0,
            blend_mode: 0,
            alpha_is_shape: false,
            rendering_intent: 0,
            transfer: TransferState::default(),
        },
    });
    render_to_rgba(&list, W, H, 72.0, None, false)
        .as_chunks::<4>()
        .0
        .iter()
        .map(|px| px[0])
        .collect()
}

/// How many pixels are left unpainted across the middle of the two tiles,
/// well inside their outline.
fn bare_between(page: &[u8]) -> usize {
    (30..170)
        .flat_map(|y| (40..160).map(move |x| (y * W + x) as usize))
        .filter(|&at| page[at] != 0)
        .count()
}

#[test]
fn tiles_that_meet_leave_nothing_bare() {
    assert_eq!(bare_between(&two_tiles(0.0)), 0);
}

/// A sliver far thinner than a pixel still holds the centre of a pixel
/// here and there along a slanted edge.
#[test]
fn a_sliver_between_tiles_is_closed() {
    for apart in [0.05, 0.2, 0.4] {
        assert_eq!(bare_between(&two_tiles(apart)), 0, "{apart} apart");
    }
}

/// Tiles drawn apart stay apart: what is closed is a sliver, not a gap
/// that was drawn.
#[test]
fn a_gap_that_was_drawn_stays_open() {
    assert!(bare_between(&two_tiles(3.0)) > 200);
}
