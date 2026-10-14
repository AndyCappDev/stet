// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The edge of a mesh shading of its own, with no clip along it: smooth,
//! as a fill's is, while every pixel inside the mesh is painted as it was.
//! Where two meshes meet, both are hard along the line they share, as they
//! always were, so the paper does not show between them.

use stet_fonts::geometry::{Matrix, PathSegment, PsPath};
use stet_graphics::color::{DeviceColor, FillRule};
use stet_graphics::device::{
    CMYK_ALL, FillParams, MeshShadingParams, PatchShadingParams, ShadingColorSpace, ShadingPatch,
    ShadingTriangle, ShadingVertex, TransferState,
};
use stet_graphics::display_list::{DisplayElement, DisplayList, GroupColorSpace, GroupParams};
use stet_render::{render_region, render_to_rgba, render_to_rgba_viewport};

/// Tall enough to be drawn in bands of 128 rows.
const W: u32 = 200;
const H: u32 = 520;

/// A patch, in data order, its corners gray `corners`; drawn with no
/// clip, smooth unless `hard`, banded and whole alike. The red channel of
/// each pixel on white paper.
fn render(points: [(f64, f64); 12], corners: [f64; 4], hard: bool) -> Vec<u8> {
    render_list(&patch(points, corners), hard)
}

/// `list` drawn as [`render`] draws a patch.
fn render_list(list: &DisplayList, hard: bool) -> Vec<u8> {
    let banded = render_to_rgba(list, W, H, 72.0, None, hard);
    let whole = render_to_rgba_viewport(list, W, H, 72.0, None, hard);
    assert!(banded == whole, "banded and whole renders disagree");
    banded.as_chunks::<4>().0.iter().map(|px| px[0]).collect()
}

fn patch(points: [(f64, f64); 12], corners: [f64; 4]) -> DisplayList {
    let mut list = DisplayList::new();
    list.push(DisplayElement::PatchShading {
        params: PatchShadingParams {
            patches: vec![ShadingPatch {
                points: points.to_vec(),
                colors: corners.map(DeviceColor::from_gray),
                raw_colors: corners.map(|g| vec![g]),
            }],
            ctm: Matrix::default(),
            color_space: ShadingColorSpace::DeviceGray,
            ..PatchShadingParams::default()
        },
    });
    list
}

/// A patch level along the top and bottom and upright at the sides, from
/// `x0` to `x1` across and `y0` to `y1` down, in data order.
fn rect(x0: f64, x1: f64, y0: f64, y1: f64) -> [(f64, f64); 12] {
    let side =
        |a: (f64, f64), b: (f64, f64), k: f64| (a.0 + (b.0 - a.0) * k, a.1 + (b.1 - a.1) * k);
    let (p00, p03, p33, p30) = ((x0, y0), (x0, y1), (x1, y1), (x1, y0));
    [
        p00,
        side(p00, p03, 1.0 / 3.0),
        side(p00, p03, 2.0 / 3.0),
        p03,
        side(p03, p33, 1.0 / 3.0),
        side(p03, p33, 2.0 / 3.0),
        p33,
        side(p30, p33, 2.0 / 3.0),
        side(p30, p33, 1.0 / 3.0),
        p30,
        side(p00, p30, 2.0 / 3.0),
        side(p00, p30, 1.0 / 3.0),
    ]
}

/// A black mesh of two triangles over the same box as [`rect`].
fn triangles(x0: f64, x1: f64, y0: f64, y1: f64) -> DisplayElement {
    let vertex = |x: f64, y: f64| ShadingVertex {
        x,
        y,
        color: DeviceColor::from_gray(0.0),
        raw_components: vec![0.0],
    };
    let triangle = |a: (f64, f64), b: (f64, f64), c: (f64, f64)| ShadingTriangle {
        v0: vertex(a.0, a.1),
        v1: vertex(b.0, b.1),
        v2: vertex(c.0, c.1),
    };
    DisplayElement::MeshShading {
        params: MeshShadingParams {
            triangles: vec![
                triangle((x0, y0), (x1, y0), (x0, y1)),
                triangle((x1, y0), (x1, y1), (x0, y1)),
            ],
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
    }
}

fn at(page: &[u8], x: u32, y: u32) -> u8 {
    page[(y * W + x) as usize]
}

/// A sheet with bowed sides, its top edge level a little below the first
/// band's last row of pixel centres, so the pixels that edge covers in
/// part lie in one band and those the mesh holds beside them in the next.
const SHEET: [(f64, f64); 12] = [
    // p00 to p03, down the left side.
    (20.0, 127.6),
    (40.0, 250.0),
    (0.0, 380.0),
    (30.0, 500.0),
    // p13, p23 to p33, along the bottom.
    (80.0, 520.0),
    (120.0, 440.0),
    (170.0, 480.0),
    // p32, p31 to p30, up the right side.
    (200.0, 360.0),
    (150.0, 240.0),
    (180.0, 127.6),
    // p20, p10, along the top.
    (126.0, 127.6),
    (74.0, 127.6),
];

/// Smoothing the edge touches the pixels of the edge and nothing else:
/// each pixel it changes has paper beside it when the edge is hard, so no
/// seam opens between the triangles inside.
#[test]
fn a_mesh_has_a_smooth_edge_and_an_unchanged_inside() {
    let corners = [0.0, 0.3, 0.6, 0.9];
    let smooth = render(SHEET, corners, false);
    let hard = render(SHEET, corners, true);
    let mut changed = 0;
    for y in 1..H - 1 {
        for x in 1..W - 1 {
            if at(&smooth, x, y) == at(&hard, x, y) {
                continue;
            }
            changed += 1;
            let paper = (y - 1..=y + 1)
                .flat_map(|ny| (x - 1..=x + 1).map(move |nx| (nx, ny)))
                .any(|(nx, ny)| at(&hard, nx, ny) == 255);
            assert!(paper, "({x}, {y}) changed inside the mesh");
        }
    }
    assert!(changed > 600, "only {changed} pixels of the edge smoothed");
}

/// A black patch with curved sides has the edge a fill of its four sides
/// has: the curves themselves, not the chords its triangles are cut
/// along, and inside it every pixel is black.
#[test]
fn a_patch_has_the_edge_of_its_curved_sides() {
    let p = SHEET;
    let curve = |a: usize, b: usize, c: usize| PathSegment::CurveTo {
        x1: p[a].0,
        y1: p[a].1,
        x2: p[b].0,
        y2: p[b].1,
        x3: p[c % 12].0,
        y3: p[c % 12].1,
    };
    let mut fill = DisplayList::new();
    fill.push(DisplayElement::Fill {
        path: PsPath {
            segments: vec![
                PathSegment::MoveTo(p[0].0, p[0].1),
                curve(1, 2, 3),
                curve(4, 5, 6),
                curve(7, 8, 9),
                curve(10, 11, 12),
                PathSegment::ClosePath,
            ],
        },
        params: FillParams {
            color: DeviceColor::from_gray(0.0),
            fill_rule: FillRule::NonZeroWinding,
            ..FillParams::default()
        },
    });
    let filled = render_list(&fill, false);
    let page = render(SHEET, [0.0; 4], false);
    let mut edge = 0;
    for y in 0..H {
        for x in 0..W {
            let (want, got) = (at(&filled, x, y), at(&page, x, y));
            // Within the rounding of the two ways of blending.
            assert!(
                want.abs_diff(got) <= 3,
                "({x}, {y}) is {got}, and {want} filled"
            );
            edge += usize::from(want != 0 && want != 255);
        }
    }
    assert!(edge > 600, "only {edge} pixels on the edge");
}

/// The level top edge covers 0.4 of each pixel of row 127, which is in
/// the first band, though the mesh holds none of them; the row below is
/// covered whole. (A fill's edge is found to a quarter of a row, so 0.4
/// comes out as a half.)
#[test]
fn a_mesh_edge_across_a_band_boundary_is_covered_in_part() {
    let page = render(SHEET, [0.0; 4], false);
    for x in 60..140 {
        let edge = at(&page, x, 127);
        assert!(
            (64..=192).contains(&edge),
            "({x}, 127) is {edge}, not covered in part"
        );
        assert_eq!(at(&page, x, 128), 0, "({x}, 128)");
    }
    assert!(render(SHEET, [0.0; 4], true)[(127 * W + 100) as usize] == 255);
}

/// A sheet that runs out in u and comes back along exactly the same path:
/// its two layers lie one on the other and face opposite ways. Counted by
/// how they wind, as a fill would, they would cancel and leave the shared
/// edge bare; the mesh covers each pixel of it once.
#[test]
fn a_patch_folded_onto_itself_keeps_its_edge() {
    // Out to the right and back: x = 20.5 at u = 0 and at u = 1.
    let (x0, out) = (20.5, 200.0);
    let at_v = |y: f64| [(x0, y), (x0 + out, y), (x0 + out, y), (x0, y)];
    let [t0, t1, t2, t3] = at_v(140.0);
    let [b0, b1, b2, b3] = at_v(400.0);
    let lerp =
        |a: (f64, f64), b: (f64, f64), k: f64| (a.0 + (b.0 - a.0) * k, a.1 + (b.1 - a.1) * k);
    let points = [
        t0,
        lerp(t0, b0, 1.0 / 3.0),
        lerp(t0, b0, 2.0 / 3.0),
        b0,
        b1,
        b2,
        b3,
        lerp(t3, b3, 2.0 / 3.0),
        lerp(t3, b3, 1.0 / 3.0),
        t3,
        t2,
        t1,
    ];
    let page = render(points, [0.0; 4], false);
    for y in 160..380 {
        let edge = at(&page, 20, y);
        assert!(
            (110..=145).contains(&edge),
            "(20, {y}) is {edge}, not half covered"
        );
        assert_eq!(at(&page, 21, y), 0, "(21, {y})");
    }
}

/// A region of the page whose first column the mesh's edge passes
/// through, the pixels it holds beside that column lying outside the
/// region: the column is covered in part as it is on the whole page.
#[test]
fn a_mesh_edge_at_the_side_of_a_region_is_covered_in_part() {
    // The right side at x = 100.4.
    let points = rect(20.0, 100.4, 40.0, 400.0);
    let page = render(points, [0.0; 4], false);
    let list = patch(points, [0.0; 4]);
    let region = render_region(
        &list, 100.0, 0.0, 50.0, H as f64, 50, H, 72.0, None, None, false,
    );
    for y in 60..380 {
        let whole = at(&page, 100, y);
        assert!((64..=192).contains(&whole), "(100, {y}) is {whole}");
        assert_eq!(
            region[(y * 50) as usize * 4],
            whole,
            "column 0 of the region, row {y}"
        );
    }
}

/// A black patch from x = 19.6 to 100.4 and, beside it, a black mesh of
/// triangles from there to 180.6 or from `gap` further on.
fn side_by_side(gap: f64) -> DisplayList {
    let mut list = DisplayList::new();
    list.push(patch(rect(19.6, 100.4, 40.0, 400.0), [0.0; 4]).elements()[0].clone());
    list.push(triangles(100.4 + gap, 180.6, 40.0, 400.0));
    list
}

/// Each of two meshes that meet along x = 100.4 covers part of the pixels
/// of column 100. Painted one after the other at those parts they would
/// leave the paper showing in it; both are hard there instead, and the
/// column is painted whole. Their outer edges are smooth.
#[test]
fn meshes_side_by_side_meet_without_a_seam() {
    let page = render_list(&side_by_side(0.0), false);
    for y in 60..380 {
        assert_eq!(at(&page, 100, y), 0, "(100, {y}) shows the paper");
        let left = at(&page, 19, y);
        assert!((64..=192).contains(&left), "(19, {y}) is {left}");
        let right = at(&page, 180, y);
        assert!((64..=192).contains(&right), "(180, {y}) is {right}");
    }
}

/// Meshes a few pixels apart do not meet: the edge of each beside the gap
/// is smooth.
#[test]
fn meshes_apart_keep_their_smooth_edges() {
    let page = render_list(&side_by_side(5.0), false);
    for y in 60..380 {
        let edge = at(&page, 100, y);
        assert!((64..=192).contains(&edge), "(100, {y}) is {edge}");
    }
}

/// The same when one of the meshes is in a transparency group, which is
/// drawn on a pixmap of its own covering only the group's box.
#[test]
fn a_mesh_in_a_group_meets_one_outside_it_without_a_seam() {
    let flat = side_by_side(0.0);
    let mut inner = DisplayList::new();
    inner.push(flat.elements()[1].clone());
    let mut list = DisplayList::new();
    list.push(flat.elements()[0].clone());
    list.push(DisplayElement::Group {
        elements: inner,
        params: GroupParams {
            bbox: [90.0, 30.0, 190.0, 410.0],
            isolated: true,
            knockout: false,
            blend_mode: 0,
            alpha: 1.0,
            color_space: GroupColorSpace::Inherited,
        },
    });
    let page = render_list(&list, false);
    for y in 60..380 {
        assert_eq!(at(&page, 100, y), 0, "(100, {y}) shows the paper");
    }
    assert_eq!(page, render_list(&flat, false));
}
