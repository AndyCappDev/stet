// Copyright 2006 The Android Open Source Project
// Copyright 2020 Yevhenii Reizner
// Copyright 2026 Scott Bowman
//
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
//
// Cairo-inspired analytical antialiasing. Replaces the 4×4 supersampling grid
// with 256-subpixel horizontal precision, walked at SCALE (4) sub-rows per pixel.
// Effective grid: 256 × 4 = 1024 samples per pixel (vs 16 for original 4×4).

use alloc::vec::Vec;
use core::convert::TryFrom;

use crate::{FillRule, IntRect, LengthU32, Path, Rect};

use crate::alpha_runs::AlphaRuns;
use crate::blitter::Blitter;
use crate::color::AlphaU8;
use crate::edge::{Edge, LineEdge};
use crate::edge_builder::{BasicEdgeBuilder, ShiftedIntRect};
use crate::fixed_point::FDot16;
use crate::geom::{IntRectExt, ScreenIntRect};
use crate::math::left_shift;

#[cfg(all(not(feature = "std"), feature = "no-std-float"))]
use tiny_skia_path::NoStdFloat;

/// Vertical sub-rows per pixel (must match SUPERSAMPLE_SHIFT from original).
const SHIFT: u32 = 2;
const SCALE: u32 = 1 << SHIFT; // 4
const MASK: u32 = SCALE - 1; // 3

/// Horizontal sub-pixel precision per pixel.
const GRID_X: i32 = 256;

/// Full area for one pixel: 2 * GRID_X * SCALE.
/// Factor of 2 is for exact area computation (entry + exit positions).
const GRID_AREA: i32 = 2 * GRID_X * SCALE as i32; // 2048

/// Convert an area value (0..GRID_AREA) to alpha (0..255).
/// Integer approximation: (area * 255 + GRID_AREA/2) / GRID_AREA
/// Simplified: (area * 255 + 1024) >> 11  (since GRID_AREA = 2048)
#[inline]
fn area_to_alpha(area: u32) -> AlphaU8 {
    let area = area.min(GRID_AREA as u32);
    // 255/2048 ≈ (255 * area + 1024) / 2048 = (255 * area + 1024) >> 11
    let v = (area as u64 * 255 + 1024) >> 11;
    v.min(255) as u8
}

/// Coverage cell for a single pixel column within a scanline.
#[derive(Clone, Copy)]
struct Cell {
    x: i32,
    /// Accumulated signed vertical coverage (in sub-row units, range -SCALE..+SCALE per edge).
    covered_height: i32,
    /// Accumulated signed area correction (in GRID_X*2 units per sub-row).
    uncovered_area: i32,
}

/// Sorted list of cells for one pixel row.
struct CellList {
    cells: Vec<Cell>,
}

impl CellList {
    fn new() -> Self {
        CellList {
            cells: Vec::with_capacity(64),
        }
    }

    fn clear(&mut self) {
        self.cells.clear();
    }

    /// Add coverage contribution to cell at x.
    #[inline]
    fn add(&mut self, x: i32, covered_height: i32, uncovered_area: i32) {
        // Linear scan for existing cell (cells are typically few per row).
        for cell in self.cells.iter_mut() {
            if cell.x == x {
                cell.covered_height += covered_height;
                cell.uncovered_area += uncovered_area;
                return;
            }
        }
        self.cells.push(Cell {
            x,
            covered_height,
            uncovered_area,
        });
    }

    /// Sort cells by x coordinate for sweep.
    fn sort(&mut self) {
        self.cells.sort_unstable_by_key(|c| c.x);
    }
}

pub fn fill_path(
    path: &Path,
    fill_rule: FillRule,
    clip: &ScreenIntRect,
    blitter: &mut dyn Blitter,
) {
    let ir = Rect::from_ltrb(
        path.bounds().left().floor(),
        path.bounds().top().floor(),
        path.bounds().right().ceil(),
        path.bounds().bottom().ceil(),
    )
    .and_then(|r| r.round_out());
    let ir = match ir {
        Some(v) => v,
        None => return,
    };

    let clipped_ir = match ir.intersect(&clip.to_int_rect()) {
        Some(v) => v,
        None => return,
    };

    // If coordinates overflow when shifted, fall back to non-AA.
    if rect_overflows_short_shift(&clipped_ir, SHIFT as i32) != 0 {
        super::path::fill_path(path, fill_rule, clip, blitter);
        return;
    }

    if clip.right() > 32767 || clip.bottom() > 32767 {
        return;
    }

    fill_path_impl(path, fill_rule, &ir, clip, blitter);
}

fn rect_overflows_short_shift(rect: &IntRect, shift: i32) -> i32 {
    overflows_short_shift(rect.left(), shift)
        | overflows_short_shift(rect.top(), shift)
        | overflows_short_shift(rect.right(), shift)
        | overflows_short_shift(rect.bottom(), shift)
}

fn overflows_short_shift(value: i32, shift: i32) -> i32 {
    let s = 16 + shift;
    (left_shift(value, s) >> s) - value
}

fn fill_path_impl(
    path: &Path,
    fill_rule: FillRule,
    bounds: &IntRect,
    clip: &ScreenIntRect,
    blitter: &mut dyn Blitter,
) {
    let path_contained_in_clip = if let Some(bounds) = bounds.to_screen_int_rect() {
        clip.contains(&bounds)
    } else {
        false
    };

    let shifted_clip = match ShiftedIntRect::new(clip, SHIFT as i32) {
        Some(v) => v,
        None => return,
    };

    let edge_clip = if path_contained_in_clip {
        None
    } else {
        Some(&shifted_clip)
    };

    // Build edges at SHIFT resolution (same as original SuperBlitter).
    let mut edges = match BasicEdgeBuilder::build_edges(path, edge_clip, SHIFT as i32) {
        Some(v) => v,
        None => return,
    };

    // Sort edges by first_y then x.
    edges.sort_by(|a, b| {
        let mut va = a.as_line().first_y;
        let mut vb = b.as_line().first_y;
        if va == vb {
            va = a.as_line().x;
            vb = b.as_line().x;
        }
        va.cmp(&vb)
    });

    // Set up linked list with sentinel head and tail.
    for i in 0..edges.len() {
        edges[i].prev = Some(i as u32);
        edges[i].next = Some(i as u32 + 2);
    }

    edges.insert(
        0,
        Edge::Line(LineEdge {
            prev: None,
            next: Some(1),
            x: i32::MIN,
            first_y: i32::MIN,
            ..LineEdge::default()
        }),
    );

    edges.push(Edge::Line(LineEdge {
        prev: Some(edges.len() as u32 - 1),
        next: None,
        first_y: i32::MAX,
        ..LineEdge::default()
    }));

    // Compute sub-row start/stop.
    let mut start_y = bounds.top() << SHIFT;
    let mut stop_y = bounds.bottom() << SHIFT;

    let top = shifted_clip.shifted().y() as i32;
    if !path_contained_in_clip && start_y < top {
        start_y = top;
    }

    let bottom = shifted_clip.shifted().bottom() as i32;
    if !path_contained_in_clip && stop_y > bottom {
        stop_y = bottom;
    }

    let start_y = match u32::try_from(start_y) {
        Ok(v) => v,
        Err(_) => return,
    };
    let stop_y = match u32::try_from(stop_y) {
        Ok(v) => v,
        Err(_) => return,
    };

    let clip_left = clip.left() as i32;
    let clip_right = clip.right() as i32;
    let width = (clip_right - clip_left) as u32;
    let width_len = match LengthU32::new(width) {
        Some(v) => v,
        None => return,
    };

    let is_even_odd = fill_rule == FillRule::EvenOdd;

    walk_edges_subrow(
        start_y,
        stop_y,
        clip_left,
        clip_right,
        width_len,
        is_even_odd,
        &mut edges,
        blitter,
    );
}

/// Walk edges at sub-row resolution (SCALE sub-rows per pixel).
/// For each sub-row, record each active edge's x position as a cell contribution.
/// Every SCALE sub-rows, emit the accumulated cells as one pixel row of alpha runs.
fn walk_edges_subrow(
    start_y: u32,
    stop_y: u32,
    clip_left: i32,
    clip_right: i32,
    width: LengthU32,
    is_even_odd: bool,
    edges: &mut [Edge],
    blitter: &mut dyn Blitter,
) {
    let mut cell_list = CellList::new();
    let mut runs = AlphaRuns::new(width);
    let mut curr_y = start_y;

    loop {
        // --- Accumulate sub-row contributions into cells ---

        // For each active edge at this sub-row, add its x position as a cell.
        let mut curr_idx = edges[0].next.unwrap() as usize;
        while edges[curr_idx].first_y <= curr_y as i32 {
            debug_assert!(edges[curr_idx].last_y >= curr_y as i32);

            let x = edges[curr_idx].x; // FDot16 at shifted resolution
            let winding = edges[curr_idx].winding as i32;

            // Convert FDot16 to GRID_X sub-pixel coordinates.
            // x is in shifted FDot16: the integer part is (pixel << SHIFT + sub_row).
            // We need the pixel-space sub-pixel position.
            // sub_pixel_x = (x_fdot16 / (1 << SHIFT)) * GRID_X / 65536
            //             = x_fdot16 * GRID_X / (65536 << SHIFT)
            //             = x_fdot16 * 256 / (65536 * 4)
            //             = x_fdot16 / 1024
            let sx = ((x as i64) * GRID_X as i64) >> (16 + SHIFT);
            let ix = floor_div(sx as i32, GRID_X);
            let fx = sx as i32 - ix * GRID_X;

            // Each sub-row contributes 1 unit of covered_height.
            // uncovered_area = fx * 2 (the area to the left of the edge in this pixel).
            cell_list.add(ix, winding, winding * fx * 2);

            curr_idx = edges[curr_idx].next.unwrap() as usize;
        }

        // --- Emit pixel row when we've accumulated SCALE sub-rows ---

        if (curr_y & MASK) == MASK {
            // We've completed all sub-rows for this pixel row.
            let pixel_y = curr_y >> SHIFT;

            if !cell_list.cells.is_empty() {
                cell_list.sort();
                emit_spans(
                    &cell_list,
                    clip_left,
                    clip_right,
                    pixel_y,
                    is_even_odd,
                    &mut runs,
                    width,
                    blitter,
                );
            }

            cell_list.clear();
        }

        // --- Advance edges (identical to path.rs walk_edges) ---

        let mut prev_x = edges[0].x;
        curr_idx = edges[0].next.unwrap() as usize;
        while edges[curr_idx].first_y <= curr_y as i32 {
            let next_idx = edges[curr_idx].next.unwrap();

            if edges[curr_idx].last_y == curr_y as i32 {
                match &mut edges[curr_idx] {
                    Edge::Line(_) => {
                        remove_edge(curr_idx, edges);
                    }
                    Edge::Quadratic(ref mut quad) => {
                        if quad.curve_count > 0 && quad.update() {
                            let new_x = quad.line.x;
                            if new_x < prev_x {
                                backward_insert_edge_based_on_x(curr_idx, edges);
                            } else {
                                prev_x = new_x;
                            }
                        } else {
                            remove_edge(curr_idx, edges);
                        }
                    }
                    Edge::Cubic(ref mut cubic) => {
                        if cubic.curve_count < 0 && cubic.update() {
                            debug_assert!(cubic.line.first_y == curr_y as i32 + 1);
                            let new_x = cubic.line.x;
                            if new_x < prev_x {
                                backward_insert_edge_based_on_x(curr_idx, edges);
                            } else {
                                prev_x = new_x;
                            }
                        } else {
                            remove_edge(curr_idx, edges);
                        }
                    }
                }
            } else {
                debug_assert!(edges[curr_idx].last_y > curr_y as i32);
                let new_x = edges[curr_idx].x + edges[curr_idx].dx;
                edges[curr_idx].x = new_x;

                if new_x < prev_x {
                    backward_insert_edge_based_on_x(curr_idx, edges);
                } else {
                    prev_x = new_x;
                }
            }

            curr_idx = next_idx as usize;
        }

        curr_y += 1;
        if curr_y >= stop_y {
            break;
        }

        insert_new_edges(curr_idx, curr_y as i32, edges);
    }

    // Flush any remaining cells (if stop_y isn't pixel-aligned).
    if !cell_list.cells.is_empty() {
        let pixel_y = (curr_y - 1) >> SHIFT;
        cell_list.sort();
        emit_spans(
            &cell_list,
            clip_left,
            clip_right,
            pixel_y,
            is_even_odd,
            &mut runs,
            width,
            blitter,
        );
    }
}

/// Sweep cells left-to-right, computing coverage from running winding count,
/// and emit alpha runs to the blitter.
fn emit_spans(
    cell_list: &CellList,
    clip_left: i32,
    clip_right: i32,
    y: u32,
    is_even_odd: bool,
    runs: &mut AlphaRuns,
    width: LengthU32,
    blitter: &mut dyn Blitter,
) {
    runs.reset(width);
    let mut has_content = false;
    let mut cover: i32 = 0;
    let mut last_x = clip_left;

    for cell in &cell_list.cells {
        let x = cell.x;

        // Fill from last_x to x with current cover level.
        if x > last_x {
            let alpha = cover_alpha(cover, is_even_odd);
            if alpha > 0 {
                let span_start = last_x.max(clip_left);
                let span_end = x.min(clip_right);
                if span_end > span_start {
                    set_alpha_run(
                        runs,
                        (span_start - clip_left) as u32,
                        (span_end - span_start) as usize,
                        alpha,
                    );
                    has_content = true;
                }
            }
        }

        // Process this cell's coverage change.
        cover += cell.covered_height * (GRID_X * 2);
        let area = cover - cell.uncovered_area;
        let alpha = raw_alpha(area, is_even_odd);

        if alpha > 0 && x >= clip_left && x < clip_right {
            set_alpha_run(runs, (x - clip_left) as u32, 1, alpha);
            has_content = true;
        }

        last_x = x + 1;
    }

    // Fill trailing pixels.
    if last_x < clip_right {
        let alpha = cover_alpha(cover, is_even_odd);
        if alpha > 0 {
            let span_start = last_x.max(clip_left);
            if clip_right > span_start {
                set_alpha_run(
                    runs,
                    (span_start - clip_left) as u32,
                    (clip_right - span_start) as usize,
                    alpha,
                );
                has_content = true;
            }
        }
    }

    if has_content {
        blitter.blit_anti_h(clip_left as u32, y, &mut runs.alpha, &mut runs.runs);
    }
}

/// Convert running cover to alpha (for pixels between cells).
#[inline]
fn cover_alpha(cover: i32, is_even_odd: bool) -> AlphaU8 {
    let mut a = cover.unsigned_abs();
    if is_even_odd {
        a %= (2 * GRID_AREA) as u32;
        if a > GRID_AREA as u32 {
            a = (2 * GRID_AREA) as u32 - a;
        }
    }
    area_to_alpha(a)
}

/// Convert area (cover - uncovered_area) to alpha (for cell pixels).
#[inline]
fn raw_alpha(area: i32, is_even_odd: bool) -> AlphaU8 {
    let mut a = area.unsigned_abs();
    if is_even_odd {
        a %= (2 * GRID_AREA) as u32;
        if a > GRID_AREA as u32 {
            a = (2 * GRID_AREA) as u32 - a;
        }
    }
    area_to_alpha(a)
}

/// Set alpha value for a run of pixels in the AlphaRuns buffer.
fn set_alpha_run(runs: &mut AlphaRuns, x: u32, count: usize, alpha: AlphaU8) {
    if count == 0 || alpha == 0 {
        return;
    }
    AlphaRuns::break_run(&mut runs.runs, &mut runs.alpha, x as usize, count);

    let mut offset = x as usize;
    let mut remaining = count;
    while remaining > 0 {
        let run_len = match runs.runs[offset] {
            Some(n) => n.get() as usize,
            None => break,
        };
        let n = run_len.min(remaining);
        runs.alpha[offset] = alpha;
        remaining -= n;
        offset += n;
    }
}

/// Integer floor division (rounds toward negative infinity).
#[inline]
fn floor_div(a: i32, b: i32) -> i32 {
    let d = a / b;
    let r = a % b;
    if (r != 0) && ((r ^ b) < 0) {
        d - 1
    } else {
        d
    }
}

// --- Edge list manipulation (same as path.rs) ---

fn remove_edge(curr_idx: usize, edges: &mut [Edge]) {
    let prev = edges[curr_idx].prev.unwrap();
    let next = edges[curr_idx].next.unwrap();
    edges[prev as usize].next = Some(next);
    edges[next as usize].prev = Some(prev);
}

fn backward_insert_edge_based_on_x(curr_idx: usize, edges: &mut [Edge]) {
    let x = edges[curr_idx].x;
    let mut prev_idx = edges[curr_idx].prev.unwrap() as usize;
    while prev_idx != 0 {
        if edges[prev_idx].x > x {
            prev_idx = edges[prev_idx].prev.unwrap() as usize;
        } else {
            break;
        }
    }

    let next_idx = edges[prev_idx].next.unwrap() as usize;
    if next_idx != curr_idx {
        remove_edge(curr_idx, edges);
        insert_edge_after(curr_idx, prev_idx, edges);
    }
}

fn insert_edge_after(curr_idx: usize, after_idx: usize, edges: &mut [Edge]) {
    edges[curr_idx].prev = Some(after_idx as u32);
    edges[curr_idx].next = edges[after_idx].next;
    let after_next_idx = edges[after_idx].next.unwrap() as usize;
    edges[after_next_idx].prev = Some(curr_idx as u32);
    edges[after_idx].next = Some(curr_idx as u32);
}

fn backward_insert_start(mut prev_idx: usize, x: FDot16, edges: &mut [Edge]) -> usize {
    while let Some(prev) = edges[prev_idx].prev {
        prev_idx = prev as usize;
        if edges[prev_idx].x <= x {
            break;
        }
    }
    prev_idx
}

fn insert_new_edges(mut new_idx: usize, curr_y: i32, edges: &mut [Edge]) {
    if edges[new_idx].first_y != curr_y {
        return;
    }

    let prev_idx = edges[new_idx].prev.unwrap() as usize;
    if edges[prev_idx].x <= edges[new_idx].x {
        return;
    }

    let mut start_idx = backward_insert_start(prev_idx, edges[new_idx].x, edges);
    loop {
        let next_idx = edges[new_idx].next.unwrap() as usize;
        let mut keep_edge = false;
        loop {
            let after_idx = edges[start_idx].next.unwrap() as usize;
            if after_idx == new_idx {
                keep_edge = true;
                break;
            }
            if edges[after_idx].x >= edges[new_idx].x {
                break;
            }
            start_idx = after_idx;
        }

        if !keep_edge {
            remove_edge(new_idx, edges);
            insert_edge_after(new_idx, start_idx, edges);
        }

        start_idx = new_idx;
        new_idx = next_idx;

        if edges[new_idx].first_y != curr_y {
            break;
        }
    }
}
