// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Where a Coons or tensor-product patch folds over itself, the part with
//! the larger v is drawn on top, and of parts with equal v the one with the
//! larger u (ISO 32000-1 §8.7.4.5.7; PLRM 4.9.3, type 6). The control
//! points are named p_ij, i stepping along u and j along v, and the data
//! begins p00 p01 p02 p03 — so its first side runs along v, not u.

use stet_fonts::geometry::Matrix;
use stet_graphics::color::DeviceColor;
use stet_graphics::device::{PatchShadingParams, ShadingColorSpace, ShadingPatch};
use stet_graphics::display_list::{DisplayElement, DisplayList};
use stet_render::{render_to_rgba, render_to_rgba_viewport};

const W: u32 = 70;
const H: u32 = 70;

/// Half the size of a page, flipped, as the tests draw their patches.
const HALF: Matrix = Matrix {
    a: 0.5,
    b: 0.0,
    c: 0.0,
    d: -0.5,
    tx: 5.0,
    ty: 62.0,
};

/// The patch of `shade_via_1.3_006.pdf`: a sheet that curls back over
/// itself, with the corner p00 black and the corner p30 white. In data
/// order, p00 p01 p02 p03 p13 p23 p33 p32 p31 p30 p20 p10.
const POINTS: [(f64, f64); 12] = [
    (0.0, 10.8),
    (0.0, 10.8),
    (70.2, 0.0),
    (70.2, 0.0),
    (93.6, 216.0),
    (93.6, -129.6),
    (117.0, 97.2),
    (117.0, 97.2),
    (40.95, 106.92),
    (40.95, 106.92),
    (23.4, -108.0),
    (23.4, 216.0),
];

/// Gray at p00, p03, p33 and p30.
const CORNERS: [f64; 4] = [0.0, 0.302, 0.702, 1.0];

/// The patch with gray corners `corners`, drawn under `ctm` on a page
/// `w` by `h`.
fn render(points: &[(f64, f64)], corners: [f64; 4], ctm: Matrix, (w, h): (u32, u32)) -> Vec<u8> {
    let mut list = DisplayList::new();
    list.push(DisplayElement::PatchShading {
        params: PatchShadingParams {
            patches: vec![ShadingPatch {
                points: points.to_vec(),
                colors: corners.map(DeviceColor::from_gray),
                raw_colors: corners.map(|g| vec![g]),
            }],
            ctm,
            color_space: ShadingColorSpace::DeviceGray,
            ..PatchShadingParams::default()
        },
    });
    let banded = render_to_rgba(&list, w, h, 72.0, None, false);
    let viewport = render_to_rgba_viewport(&list, w, h, 72.0, None, false);
    assert!(banded == viewport, "banded and viewport renders disagree");
    banded
}

fn gray(data: &[u8], x: u32, y: u32) -> u8 {
    data[((y * W + x) * 4) as usize]
}

/// Where the sheet curls back, a dark part near p00 and p03 lies over a
/// light part near p30. The dark part has the larger v and the smaller u,
/// so it is on top; a renderer that takes the first side for u orders by
/// the spec's u first and puts the light part there instead.
#[test]
fn the_part_with_the_larger_v_is_on_top() {
    let page = render(&POINTS, CORNERS, HALF, (W, H));
    // Covered by both parts.
    for (x, y) in [(28, 26), (32, 40), (38, 50)] {
        let g = gray(&page, x, y);
        assert!(g < 128, "({x}, {y}) is {g}: the light part is on top");
    }
    // Covered by the light part alone.
    let g = gray(&page, 50, 15);
    assert!(g > 200, "(50, 15) is {g}, not the light part");
}

/// The patch sampled finely, each sample painted over those before it in
/// the spec's order: v, then u. Gray per pixel, where a sample lands.
fn truth(
    p: &[(f64, f64); 12],
    corners: [f64; 4],
    ctm: Matrix,
    (w, h): (u32, u32),
) -> Vec<Option<u8>> {
    const N: usize = 1500;
    let (w, h) = (w as usize, h as usize);
    let mut truth = vec![None; w * h];
    let bezier = |q: [(f64, f64); 4], t: f64| {
        let s = 1.0 - t;
        let b = [s * s * s, 3.0 * s * s * t, 3.0 * s * t * t, t * t * t];
        (0..4).fold((0.0, 0.0), |(x, y), k| {
            (x + b[k] * q[k].0, y + b[k] * q[k].1)
        })
    };
    for i in 0..=N {
        let v = i as f64 / N as f64;
        // The sides along v, at u = 0 and u = 1.
        let d0 = bezier([p[0], p[1], p[2], p[3]], v);
        let d1 = bezier([p[9], p[8], p[7], p[6]], v);
        for j in 0..=N {
            let u = j as f64 / N as f64;
            // The sides along u, at v = 0 and v = 1.
            let c0 = bezier([p[0], p[11], p[10], p[9]], u);
            let c1 = bezier([p[3], p[4], p[5], p[6]], u);
            let weights = [(1.0 - u) * (1.0 - v), (1.0 - u) * v, u * v, u * (1.0 - v)];
            let [p00, p03, p33, p30] = [p[0], p[3], p[6], p[9]];
            let at = |k: fn((f64, f64)) -> f64| {
                (1.0 - u) * k(d0) + u * k(d1) + (1.0 - v) * k(c0) + v * k(c1)
                    - (weights[0] * k(p00)
                        + weights[1] * k(p03)
                        + weights[2] * k(p33)
                        + weights[3] * k(p30))
            };
            let (x, y) = ctm.transform_point(at(|q| q.0), at(|q| q.1));
            if x < 0.0 || y < 0.0 || x >= w as f64 || y >= h as f64 {
                continue;
            }
            let gray: f64 = (0..4).map(|k| weights[k] * corners[k]).sum();
            truth[y as usize * w + x as usize] = Some((gray * 255.0).round() as u8);
        }
    }
    truth
}

/// Of the pixels with the patch on all four sides in both `page` and
/// `truth`, how many there are and how many show what no sample at or
/// beside them does. Where two layers meet, the sample that shows in a
/// pixel need not be the one at its centre; a neighbour's will do.
fn disagreements(page: &[u8], truth: &[Option<u8>], (w, h): (u32, u32)) -> (usize, usize) {
    let (w, h) = (w as usize, h as usize);
    let both = |x: usize, y: usize| truth[y * w + x].is_some() && page[(y * w + x) * 4] != 255;
    let (mut checked, mut wrong) = (0, 0);
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let around = [(x, y), (x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)];
            if !around.iter().all(|&(x, y)| both(x, y)) {
                continue;
            }
            checked += 1;
            let shown = page[(y * w + x) * 4];
            let near = (y - 1..=y + 1)
                .flat_map(|ny| (x - 1..=x + 1).map(move |nx| (nx, ny)))
                .filter_map(|(nx, ny)| truth[ny * w + nx])
                .any(|g| g.abs_diff(shown) <= 24);
            if !near {
                wrong += 1;
            }
        }
    }
    (checked, wrong)
}

/// A sheet that rises and falls back across itself in u while leaning in
/// v, from `4217_1.pdf`: where the two layers overlap they are less than a
/// triangle apart in v, so no order of painting whole triangles gets every
/// pixel right.
#[test]
fn a_fold_is_decided_pixel_by_pixel() {
    const POINTS: [(f64, f64); 12] = [
        (100.0, 200.0),
        (200.0, 200.0),
        (300.0, 200.0),
        (400.0, 200.0),
        (300.0, 900.0),
        (600.0, 900.0),
        (330.0, 500.0),
        (230.0, 500.0),
        (130.0, 500.0),
        (30.0, 500.0),
        (300.0, 900.0),
        (0.0, 900.0),
    ];
    const CORNERS: [f64; 4] = [0.2, 0.2, 0.9, 0.6];
    const SIZE: (u32, u32) = (320, 300);
    let ctm = Matrix {
        a: 0.5,
        b: 0.0,
        c: 0.0,
        d: -0.5,
        tx: 0.0,
        ty: 380.0,
    };
    let page = render(&POINTS, CORNERS, ctm, SIZE);
    let (checked, wrong) = disagreements(&page, &truth(&POINTS, CORNERS, ctm, SIZE), SIZE);
    assert!(checked > 30_000, "only {checked} pixels inside");
    assert!(
        wrong * 4000 < checked,
        "{wrong} of {checked} pixels show the wrong layer"
    );
}

/// A sheet folded back on itself in u alone, its sides along v S-shaped
/// and its sides along u reaching out unequally: the two layers meet at
/// exactly the same v, so the larger u shows. The renderer keeps what a
/// pixel shows less precisely than it compares, so v must be taken as
/// level when it nearly is, or rounding picks the layer.
#[test]
fn layers_at_the_same_v_show_the_larger_u() {
    // p00 p01 p02 p03 run along v; the sides along u run out to the
    // right and back.
    let side = |x: f64| [(x, 0.0), (x + 40.0, 40.0), (x - 40.0, 80.0), (x, 120.0)];
    let [a0, a1, a2, a3] = side(0.0);
    let [b0, b1, b2, b3] = side(60.0);
    let out = |p: (f64, f64), k: f64| (p.0 + k, p.1);
    const REACH: [f64; 2] = [140.0, 240.0];
    let points = [
        a0,
        a1,
        a2,
        a3,
        out(a3, REACH[0]),
        out(b3, REACH[1]),
        b3,
        b2,
        b1,
        b0,
        out(b0, REACH[1]),
        out(a0, REACH[0]),
    ];
    const CORNERS: [f64; 4] = [0.1, 0.1, 0.9, 0.9];
    const SIZE: (u32, u32) = (300, 260);
    let ctm = Matrix {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: -1.0,
        tx: 60.0,
        ty: 240.0,
    };
    let page = render(&points, CORNERS, ctm, SIZE);
    let (checked, wrong) = disagreements(&page, &truth(&points, CORNERS, ctm, SIZE), SIZE);
    assert!(checked > 10_000, "only {checked} pixels inside");
    assert!(
        wrong * 4000 < checked,
        "{wrong} of {checked} pixels show the wrong layer"
    );
}
