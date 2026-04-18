// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Diagnostic: compare tiny-skia AA coverage for the same X path rendered
//! onto two contexts:
//!   (A) a pink-backdropped pixmap (the parent-form path in GWG 16.2)
//!   (B) a transparent pixmap (the isolated-group offscreen path for Fm17)
//!
//! If the AA coverage is bit-identical between contexts, stet is spec-
//! compliant and the Opacity(0%) X-outline artefact is a natural result
//! of source-over compositing a partial-alpha source onto a parent whose
//! AA edges already mix black with magenta. Any divergence with Acrobat
//! would be rasterizer-quality, not a bug.
//!
//! If the coverage DIFFERS between contexts, the isolated-group path has
//! a subpixel rasterization bug that fails to match what the parent draws
//! — and *that* is the outline.

use stet_tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Transform};

fn make_x_path() -> stet_tiny_skia::Path {
    // The X path as used in GWG 16.2 Opacity(0%) Painter A (obj 428).
    // PDF coords — we'll apply a transform to rasterize at 300 DPI.
    // Translation matrix from the PDF: `1 0 0 1 247.5673981 86.3047028 cm`
    // Path verbs (relative-translated): 0 0 m, -3.004 3.004 l, ...
    let mut pb = PathBuilder::new();
    let tx = 247.5674_f32;
    let ty = 86.3047_f32;
    let pt = |x: f32, y: f32| (tx + x, ty + y);
    let (x, y) = pt(0.0, 0.0);
    pb.move_to(x, y);
    for &(lx, ly) in &[
        (-3.004, 3.004),
        (-9.939, -3.932),
        (-16.874, 3.004),
        (-19.878, 0.0),
        (-12.942, -6.936),
        (-19.878, -13.869),
        (-16.874, -16.873),
        (-9.939, -9.936),
        (-3.004, -16.873),
        (0.0, -13.869),
        (-6.936, -6.936),
    ] {
        let (x, y) = pt(lx, ly);
        pb.line_to(x, y);
    }
    pb.close();
    pb.finish().expect("valid X path")
}

fn x_path_transform() -> Transform {
    // PDF Y-up → device Y-down, scale 300/72 ≈ 4.1667 for 300 DPI.
    // The PDF page is 612.283 × 858.898; the Opacity(0%) swatch sits near
    // the right end of GWG 16.2's bottom row. We approximate the swatch's
    // device-space transform by translating so the X rasterizes into a
    // 200×200 pixmap centered on the path's device-space center.
    let scale = 300.0_f32 / 72.0;
    // Fit the path at (247.5673981, 86.3047028) into a 200×200 canvas by
    // translating so the path's anchor lands at (150, 150) device pixels
    // (leaves room for the ~83×83 pixel X path).
    let anchor_dev_x = 247.5674_f32 * scale;
    let anchor_dev_y = 86.3047_f32 * scale;
    Transform::from_row(
        scale,
        0.0,
        0.0,
        -scale, // Y-flip
        150.0 - anchor_dev_x,
        150.0 + anchor_dev_y,
    )
}

fn render_fill(pixmap: &mut Pixmap, color: Color, path: &stet_tiny_skia::Path) {
    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;
    pixmap.fill_path(path, &paint, FillRule::Winding, x_path_transform(), None);
}

fn main() {
    let path = make_x_path();

    // Context A: pink-backdropped pixmap, then fill black X.
    //   Mirrors what the parent form does (fill magenta rect, then fill
    //   black X at alpha=1 on top).
    let mut a = Pixmap::new(200, 200).unwrap();
    // Pink backdrop (hex 0xec148d = decimal (236, 20, 141), alpha 255).
    a.fill(Color::from_rgba(236.0 / 255.0, 20.0 / 255.0, 141.0 / 255.0, 1.0).unwrap());
    render_fill(&mut a, Color::BLACK, &path);

    // Context B: transparent pixmap, fill black X at alpha=1.
    //   Mirrors what an isolated transparency group does — start
    //   transparent, paint the X.
    let mut b = Pixmap::new(200, 200).unwrap();
    render_fill(&mut b, Color::BLACK, &path);

    // Context C: transparent pixmap, fill MAGENTA X.
    //   Mirrors Fm17's Painter A (the isolated group's interior).
    let mut c = Pixmap::new(200, 200).unwrap();
    render_fill(
        &mut c,
        Color::from_rgba(236.0 / 255.0, 20.0 / 255.0, 141.0 / 255.0, 1.0).unwrap(),
        &path,
    );

    // Compute the AA coverage map from B: it's the alpha channel of the
    // black-on-transparent fill. Same path, same transform — this is the
    // "ground truth" for what the rasterizer produced.
    let mut coverage_map: Vec<u8> = b.data().chunks(4).map(|p| p[3]).collect();

    // Derive A's coverage by comparing A's RGB against the known backdrop
    // color. Where A's color moved toward black, coverage was applied.
    //   A_rgb = coverage * black + (1 - coverage) * pink
    //   → coverage = (pink - A_rgb) / (pink - black) ≈ 1 - A_r / pink_r
    // Use the red channel (pink_r = 236, black_r = 0) as it has the
    // strongest contrast.
    let mut a_coverage: Vec<u8> = a
        .data()
        .chunks(4)
        .map(|p| {
            let red = p[0] as f32;
            let cov = ((236.0 - red) / 236.0).clamp(0.0, 1.0);
            (cov * 255.0).round() as u8
        })
        .collect();

    // Save the three pixmaps for visual inspection.
    a.save_png("/tmp/aa_a_pink_then_black.png").unwrap();
    b.save_png("/tmp/aa_b_transparent_black.png").unwrap();
    c.save_png("/tmp/aa_c_transparent_magenta.png").unwrap();

    // Also save coverage maps as grayscale pngs so we can eyeball them.
    let mut cov_a = Pixmap::new(200, 200).unwrap();
    for (i, chunk) in cov_a.data_mut().chunks_mut(4).enumerate() {
        let c = a_coverage[i];
        chunk.copy_from_slice(&[c, c, c, 255]);
    }
    cov_a.save_png("/tmp/aa_cov_a.png").unwrap();
    let mut cov_b = Pixmap::new(200, 200).unwrap();
    for (i, chunk) in cov_b.data_mut().chunks_mut(4).enumerate() {
        let c = coverage_map[i];
        chunk.copy_from_slice(&[c, c, c, 255]);
    }
    cov_b.save_png("/tmp/aa_cov_b.png").unwrap();

    // Diff coverage A vs B.
    let mut max_diff = 0i32;
    let mut total_diff = 0u64;
    let mut diff_count = 0u32;
    let mut edge_diffs: Vec<(u32, u32, i32)> = Vec::new();
    for y in 0..200u32 {
        for x in 0..200u32 {
            let i = (y * 200 + x) as usize;
            let diff = a_coverage[i] as i32 - coverage_map[i] as i32;
            if diff != 0 {
                diff_count += 1;
                total_diff += diff.unsigned_abs() as u64;
                max_diff = max_diff.max(diff.abs());
                if diff.abs() > 2 {
                    edge_diffs.push((x, y, diff));
                }
            }
        }
    }

    eprintln!("=== AA coverage diff: pink-backdrop vs transparent ===");
    eprintln!("Pixels with any difference : {} / 40000", diff_count);
    eprintln!("Max channel delta          : {}", max_diff);
    eprintln!("Mean |delta| over diffs    : {:.2}", total_diff as f64 / diff_count.max(1) as f64);
    if !edge_diffs.is_empty() {
        eprintln!("First 20 pixels with |delta| > 2:");
        for (x, y, d) in edge_diffs.iter().take(20) {
            eprintln!("  ({}, {}) delta={:+}", x, y, d);
        }
    }

    // Suppress the unused_assignments warning for the in-place coverage
    // rewrite above (we read it into the diff loop).
    let _ = (&mut coverage_map, &mut a_coverage);
}
