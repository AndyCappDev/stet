// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Strokepath algorithm — converts a stroked path into a filled outline.
//!
//! Produces moveto/lineto/curveto geometry (no tessellation).
//! Standalone module with no PostScript interpreter dependencies.
//!
//! Faithfully ported from PostForge's `strokepath_algorithm.py`.
//!
//! Components:
//! 1. Dash pattern processor (de Casteljau splitting for curves)
//! 2. Line/curve offset (Tiller-Hanson adaptive subdivision for cubics)
//! 3. Line joins (miter/round/bevel)
//! 4. Line caps (butt/round/projecting square)
//! 5. Outline assembly

use std::f64::consts::PI;

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct Point {
    x: f64,
    y: f64,
}

impl Point {
    fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    fn length(self) -> f64 {
        self.x.hypot(self.y)
    }

    fn normalized(self) -> Point {
        let ln = self.length();
        if ln < 1e-12 {
            return Point::new(0.0, 0.0);
        }
        Point::new(self.x / ln, self.y / ln)
    }

    fn dot(self, other: Point) -> f64 {
        self.x * other.x + self.y * other.y
    }

    fn cross(self, other: Point) -> f64 {
        self.x * other.y - self.y * other.x
    }

    fn add(self, other: Point) -> Point {
        Point::new(self.x + other.x, self.y + other.y)
    }

    fn sub(self, other: Point) -> Point {
        Point::new(self.x - other.x, self.y - other.y)
    }

    fn scale(self, s: f64) -> Point {
        Point::new(self.x * s, self.y * s)
    }

    fn neg(self) -> Point {
        Point::new(-self.x, -self.y)
    }
}

#[derive(Clone, Debug)]
pub enum PathElement {
    MoveTo(f64, f64),
    LineTo(f64, f64),
    CurveTo {
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
        x3: f64,
        y3: f64,
    },
    ClosePath,
}

pub type SubPath = Vec<PathElement>;
pub type Path = Vec<SubPath>;

fn subpath_is_closed(sp: &[PathElement]) -> bool {
    matches!(sp.last(), Some(PathElement::ClosePath))
}

fn subpath_start(sp: &[PathElement]) -> Point {
    if let Some(PathElement::MoveTo(x, y)) = sp.first() {
        Point::new(*x, *y)
    } else {
        Point::new(0.0, 0.0)
    }
}

fn segment_endpoint(seg: &PathElement) -> Option<Point> {
    match seg {
        PathElement::MoveTo(x, y) | PathElement::LineTo(x, y) => Some(Point::new(*x, *y)),
        PathElement::CurveTo { x3, y3, .. } => Some(Point::new(*x3, *y3)),
        PathElement::ClosePath => None,
    }
}

// ---------------------------------------------------------------------------
// De Casteljau splitting
// ---------------------------------------------------------------------------

fn lerp(a: Point, b: Point, t: f64) -> Point {
    Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

/// Split cubic Bézier at parameter t. Returns (left, right) each as 4 Points.
fn split_cubic(p0: Point, p1: Point, p2: Point, p3: Point, t: f64) -> ([Point; 4], [Point; 4]) {
    let q0 = lerp(p0, p1, t);
    let q1 = lerp(p1, p2, t);
    let q2 = lerp(p2, p3, t);
    let r0 = lerp(q0, q1, t);
    let r1 = lerp(q1, q2, t);
    let s = lerp(r0, r1, t);
    ([p0, q0, r0, s], [s, r1, q2, p3])
}

fn cubic_arc_length_approx(
    p0: Point,
    p1: Point,
    p2: Point,
    p3: Point,
    depth: u32,
    max_depth: u32,
    tol: f64,
) -> f64 {
    let chord = p3.sub(p0).length();
    let poly = p1.sub(p0).length() + p2.sub(p1).length() + p3.sub(p2).length();
    if depth >= max_depth || (poly - chord) < tol {
        return (chord + poly) / 2.0;
    }
    let (left, right) = split_cubic(p0, p1, p2, p3, 0.5);
    cubic_arc_length_approx(
        left[0],
        left[1],
        left[2],
        left[3],
        depth + 1,
        max_depth,
        tol,
    ) + cubic_arc_length_approx(
        right[0],
        right[1],
        right[2],
        right[3],
        depth + 1,
        max_depth,
        tol,
    )
}

fn line_length(p0: Point, p1: Point) -> f64 {
    p1.sub(p0).length()
}

// ---------------------------------------------------------------------------
// Dash pattern processor
// ---------------------------------------------------------------------------

fn find_cubic_t_for_length(
    p0: Point,
    p1: Point,
    p2: Point,
    p3: Point,
    target_len: f64,
    depth: u32,
) -> f64 {
    let mut lo = 0.0f64;
    let mut hi = 1.0f64;
    for _ in 0..depth {
        let mid = (lo + hi) / 2.0;
        let (left, _) = split_cubic(p0, p1, p2, p3, mid);
        let left_len = cubic_arc_length_approx(left[0], left[1], left[2], left[3], 0, 12, 0.1);
        if left_len < target_len {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    (lo + hi) / 2.0
}

/// A geometric segment for dash processing: either a line or a curve with its start point.
enum DashSeg {
    Line(Point, Point),
    Curve(Point, Point, Point, Point),
}

pub fn apply_dash_pattern(subpaths: &Path, dash_array: &[f64], dash_offset: f64) -> Path {
    if dash_array.is_empty() {
        return subpaths.clone();
    }

    let mut result: Path = Vec::new();

    for sp in subpaths {
        let result_start = result.len();
        let closed = subpath_is_closed(sp);
        let mut segments: Vec<DashSeg> = Vec::new();
        let mut current = subpath_start(sp);

        for elem in sp {
            match elem {
                PathElement::MoveTo(x, y) => {
                    current = Point::new(*x, *y);
                }
                PathElement::LineTo(x, y) => {
                    let end = Point::new(*x, *y);
                    segments.push(DashSeg::Line(current, end));
                    current = end;
                }
                PathElement::CurveTo {
                    x1,
                    y1,
                    x2,
                    y2,
                    x3,
                    y3,
                } => {
                    let p1 = Point::new(*x1, *y1);
                    let p2 = Point::new(*x2, *y2);
                    let p3 = Point::new(*x3, *y3);
                    segments.push(DashSeg::Curve(current, p1, p2, p3));
                    current = p3;
                }
                PathElement::ClosePath => {
                    let start = subpath_start(sp);
                    let dx = current.x - start.x;
                    let dy = current.y - start.y;
                    if dx * dx + dy * dy > 1e-12 {
                        segments.push(DashSeg::Line(current, start));
                        current = start;
                    }
                }
            }
        }

        if segments.is_empty() {
            continue;
        }

        // Compute total length
        let mut total_length = 0.0;
        for seg in &segments {
            total_length += match seg {
                DashSeg::Line(p0, p1) => line_length(*p0, *p1),
                DashSeg::Curve(p0, p1, p2, p3) => {
                    cubic_arc_length_approx(*p0, *p1, *p2, *p3, 0, 12, 0.1)
                }
            };
        }

        if total_length < 1e-12 {
            continue;
        }

        // Normalize offset into the dash cycle
        let mut dash_cycle: f64 = dash_array.iter().sum();
        if dash_array.len() % 2 == 1 {
            dash_cycle *= 2.0;
        }
        if dash_cycle < 1e-12 {
            result.push(sp.clone());
            continue;
        }

        let offset = dash_offset.rem_euclid(dash_cycle);

        // Find starting dash index and remaining length in that dash
        let mut dash_idx: usize = 0;
        let mut remaining_offset = offset;
        while remaining_offset >= dash_array[dash_idx % dash_array.len()] {
            remaining_offset -= dash_array[dash_idx % dash_array.len()];
            dash_idx += 1;
        }

        let mut drawing = dash_idx % 2 == 0;
        let starts_drawing = drawing;
        let mut dash_remaining = dash_array[dash_idx % dash_array.len()] - remaining_offset;

        let mut current_subpath: SubPath = Vec::new();
        let mut seg_idx = 0usize;
        let mut seg_consumed = 0.0f64;

        while seg_idx < segments.len() {
            let seg = &segments[seg_idx];

            let seg_total = match seg {
                DashSeg::Line(p0, p1) => line_length(*p0, *p1),
                DashSeg::Curve(p0, p1, p2, p3) => {
                    cubic_arc_length_approx(*p0, *p1, *p2, *p3, 0, 12, 0.1)
                }
            };

            let seg_left = seg_total - seg_consumed;

            if seg_left <= 1e-12 {
                seg_idx += 1;
                seg_consumed = 0.0;
                continue;
            }

            if dash_remaining >= seg_left - 1e-12 {
                // Entire remaining segment fits in current dash
                if drawing {
                    match seg {
                        DashSeg::Line(p0, p1) => {
                            if current_subpath.is_empty() {
                                let cp = if seg_consumed > 1e-12 {
                                    let t = seg_consumed / seg_total;
                                    lerp(*p0, *p1, t)
                                } else {
                                    *p0
                                };
                                current_subpath.push(PathElement::MoveTo(cp.x, cp.y));
                            }
                            current_subpath.push(PathElement::LineTo(p1.x, p1.y));
                        }
                        DashSeg::Curve(p0, p1, p2, p3) => {
                            let (rp0, rp1, rp2, rp3) = if seg_consumed > 1e-12 {
                                let t =
                                    find_cubic_t_for_length(*p0, *p1, *p2, *p3, seg_consumed, 20);
                                let (_, right) = split_cubic(*p0, *p1, *p2, *p3, t);
                                (right[0], right[1], right[2], right[3])
                            } else {
                                (*p0, *p1, *p2, *p3)
                            };
                            if current_subpath.is_empty() {
                                current_subpath.push(PathElement::MoveTo(rp0.x, rp0.y));
                            }
                            current_subpath.push(PathElement::CurveTo {
                                x1: rp1.x,
                                y1: rp1.y,
                                x2: rp2.x,
                                y2: rp2.y,
                                x3: rp3.x,
                                y3: rp3.y,
                            });
                        }
                    }
                }

                dash_remaining -= seg_left;
                seg_idx += 1;
                seg_consumed = 0.0;
            } else {
                // Dash boundary falls within this segment — split it
                let split_at = seg_consumed + dash_remaining;

                match seg {
                    DashSeg::Line(p0, p1) => {
                        let t = if seg_total > 1e-12 {
                            split_at / seg_total
                        } else {
                            0.0
                        };
                        let split_pt = lerp(*p0, *p1, t);
                        if drawing {
                            if current_subpath.is_empty() {
                                let cp = if seg_consumed > 1e-12 {
                                    let t0 = seg_consumed / seg_total;
                                    lerp(*p0, *p1, t0)
                                } else {
                                    *p0
                                };
                                current_subpath.push(PathElement::MoveTo(cp.x, cp.y));
                            }
                            current_subpath.push(PathElement::LineTo(split_pt.x, split_pt.y));
                        }
                    }
                    DashSeg::Curve(p0, p1, p2, p3) => {
                        let t = find_cubic_t_for_length(*p0, *p1, *p2, *p3, split_at, 20);
                        let (left, _right) = split_cubic(*p0, *p1, *p2, *p3, t);
                        if drawing {
                            let (lp0, lp1, lp2, lp3) = if seg_consumed > 1e-12 {
                                let t0 =
                                    find_cubic_t_for_length(*p0, *p1, *p2, *p3, seg_consumed, 20);
                                let (_, rem) = split_cubic(*p0, *p1, *p2, *p3, t0);
                                let rem_total = cubic_arc_length_approx(
                                    rem[0], rem[1], rem[2], rem[3], 0, 12, 0.1,
                                );
                                if rem_total > 1e-12 {
                                    let t2 = find_cubic_t_for_length(
                                        rem[0],
                                        rem[1],
                                        rem[2],
                                        rem[3],
                                        dash_remaining,
                                        20,
                                    );
                                    let (left2, _) =
                                        split_cubic(rem[0], rem[1], rem[2], rem[3], t2);
                                    (left2[0], left2[1], left2[2], left2[3])
                                } else {
                                    (left[0], left[1], left[2], left[3])
                                }
                            } else {
                                (left[0], left[1], left[2], left[3])
                            };
                            if current_subpath.is_empty() {
                                current_subpath.push(PathElement::MoveTo(lp0.x, lp0.y));
                            }
                            current_subpath.push(PathElement::CurveTo {
                                x1: lp1.x,
                                y1: lp1.y,
                                x2: lp2.x,
                                y2: lp2.y,
                                x3: lp3.x,
                                y3: lp3.y,
                            });
                        }
                    }
                }

                seg_consumed = split_at;

                // Switch dash phase
                if drawing && !current_subpath.is_empty() {
                    result.push(std::mem::take(&mut current_subpath));
                } else if !drawing {
                    current_subpath.clear();
                }

                dash_idx += 1;
                drawing = dash_idx % 2 == 0;
                dash_remaining = dash_array[dash_idx % dash_array.len()];
            }
        }

        // Finish last dash segment
        let ends_mid_dash = drawing && !current_subpath.is_empty();
        if ends_mid_dash {
            result.push(std::mem::take(&mut current_subpath));
        }

        // For closed subpaths: merge first and last dashes at closure point
        let num_dashes = result.len() - result_start;
        if closed && starts_drawing && ends_mid_dash && num_dashes >= 2 {
            let first_sp = result[result_start].clone();
            let last_sp = result.last_mut().unwrap();
            for elem in &first_sp {
                if !matches!(elem, PathElement::MoveTo(..)) {
                    last_sp.push(elem.clone());
                }
            }
            result.remove(result_start);
        } else if closed && starts_drawing && num_dashes == 1 {
            result.last_mut().unwrap().push(PathElement::ClosePath);
        }
    }

    result
}

// ---------------------------------------------------------------------------
// Offset computations
// ---------------------------------------------------------------------------

fn normal(p0: Point, p1: Point) -> Point {
    let d = p1.sub(p0);
    let ln = d.length();
    if ln < 1e-12 {
        return Point::new(0.0, 0.0);
    }
    Point::new(-d.y / ln, d.x / ln)
}

fn offset_line(p0: Point, p1: Point, dist: f64) -> (Point, Point) {
    let n = normal(p0, p1);
    let off = n.scale(dist);
    (p0.add(off), p1.add(off))
}

fn normal_raw(p0x: f64, p0y: f64, p1x: f64, p1y: f64) -> (f64, f64) {
    let dx = p1x - p0x;
    let dy = p1y - p0y;
    let ln = dx.hypot(dy);
    if ln < 1e-12 {
        return (0.0, 0.0);
    }
    (-dy / ln, dx / ln)
}

/// Offset a cubic Bézier using raw floats. Appends CurveTo elements to result
/// and returns the offset start point.
#[allow(clippy::too_many_arguments)]
fn offset_cubic_recursive_raw(
    p0x: f64,
    p0y: f64,
    p1x: f64,
    p1y: f64,
    p2x: f64,
    p2y: f64,
    p3x: f64,
    p3y: f64,
    dist: f64,
    tol: f64,
    depth: u32,
    max_depth: u32,
    result: &mut Vec<PathElement>,
) -> (f64, f64) {
    // Endpoint normals (start)
    let (mut n0x, mut n0y) = normal_raw(p0x, p0y, p1x, p1y);
    if (p1x - p0x).hypot(p1y - p0y) < 1e-4 {
        let n = normal_raw(p0x, p0y, p2x, p2y);
        n0x = n.0;
        n0y = n.1;
    }
    if (p2x - p0x).hypot(p2y - p0y) < 1e-4 {
        let n = normal_raw(p0x, p0y, p3x, p3y);
        n0x = n.0;
        n0y = n.1;
    }

    let (mut n3x, mut n3y) = normal_raw(p2x, p2y, p3x, p3y);
    if (p3x - p2x).hypot(p3y - p2y) < 1e-4 {
        let n = normal_raw(p1x, p1y, p3x, p3y);
        n3x = n.0;
        n3y = n.1;
    }
    if (p3x - p1x).hypot(p3y - p1y) < 1e-4 {
        let n = normal_raw(p0x, p0y, p3x, p3y);
        n3x = n.0;
        n3y = n.1;
    }

    // Decide: flatten or subdivide
    let flat;
    if depth >= max_depth {
        flat = true;
    } else {
        // Inline _cubic_is_flat
        let cdx = p3x - p0x;
        let cdy = p3y - p0y;
        let cln = cdx.hypot(cdy);
        let is_flat = if cln < 1e-12 {
            (p1x - p0x).hypot(p1y - p0y) < tol && (p2x - p0x).hypot(p2y - p0y) < tol
        } else {
            let nx = -cdy / cln;
            let ny = cdx / cln;
            let d1 = ((p1x - p0x) * nx + (p1y - p0y) * ny).abs();
            let d2 = ((p2x - p0x) * nx + (p2y - p0y) * ny).abs();
            d1.max(d2) < tol
        };

        if is_flat {
            // Inline _normals_are_close
            let dot = n0x * n3x + n0y * n3y;
            if dot < 0.966 {
                flat = false;
            } else {
                let clamped = dot.clamp(-1.0, 1.0);
                let deviation = dist.abs() * (1.0 - clamped);
                flat = deviation <= tol;
            }
        } else {
            flat = false;
        }
    }

    if flat {
        // Tiller-Hanson offset
        let off0x = p0x + n0x * dist;
        let off0y = p0y + n0y * dist;
        result.push(PathElement::CurveTo {
            x1: p1x + n0x * dist,
            y1: p1y + n0y * dist,
            x2: p2x + n3x * dist,
            y2: p2y + n3y * dist,
            x3: p3x + n3x * dist,
            y3: p3y + n3y * dist,
        });
        return (off0x, off0y);
    }

    // Subdivide at t=0.5 (de Casteljau inlined)
    let q0x = (p0x + p1x) * 0.5;
    let q0y = (p0y + p1y) * 0.5;
    let q1x = (p1x + p2x) * 0.5;
    let q1y = (p1y + p2y) * 0.5;
    let q2x = (p2x + p3x) * 0.5;
    let q2y = (p2y + p3y) * 0.5;
    let r0x = (q0x + q1x) * 0.5;
    let r0y = (q0y + q1y) * 0.5;
    let r1x = (q1x + q2x) * 0.5;
    let r1y = (q1y + q2y) * 0.5;
    let sx = (r0x + r1x) * 0.5;
    let sy = (r0y + r1y) * 0.5;

    let nd1 = depth + 1;
    let off_start = offset_cubic_recursive_raw(
        p0x, p0y, q0x, q0y, r0x, r0y, sx, sy, dist, tol, nd1, max_depth, result,
    );
    offset_cubic_recursive_raw(
        sx, sy, r1x, r1y, q2x, q2y, p3x, p3y, dist, tol, nd1, max_depth, result,
    );
    off_start
}

/// Offset a single path segment. Returns (list_of_elements, offset_start, offset_end).
fn offset_segment(
    start: Point,
    seg: &PathElement,
    dist: f64,
    tol: f64,
) -> (Vec<PathElement>, Point, Point) {
    match seg {
        PathElement::LineTo(x, y) => {
            let end = Point::new(*x, *y);
            let (op0, op1) = offset_line(start, end, dist);
            (vec![PathElement::LineTo(op1.x, op1.y)], op0, op1)
        }
        PathElement::CurveTo {
            x1,
            y1,
            x2,
            y2,
            x3,
            y3,
        } => {
            let (p0x, p0y) = (start.x, start.y);
            let (p1x, p1y) = (*x1, *y1);
            let (p2x, p2y) = (*x2, *y2);
            let (p3x, p3y) = (*x3, *y3);

            // Short curve → treat as line
            let chord = (p3x - p0x).hypot(p3y - p0y);
            if chord < dist.abs() * 0.1 {
                let end = Point::new(p3x, p3y);
                let (op0, op1) = offset_line(start, end, dist);
                return (vec![PathElement::LineTo(op1.x, op1.y)], op0, op1);
            }

            let mut result_elems = Vec::new();
            let (osx, osy) = offset_cubic_recursive_raw(
                p0x,
                p0y,
                p1x,
                p1y,
                p2x,
                p2y,
                p3x,
                p3y,
                dist,
                tol,
                0,
                10,
                &mut result_elems,
            );
            let off_start = Point::new(osx, osy);
            let off_end = if let Some(last) = result_elems.last() {
                match last {
                    PathElement::CurveTo { x3, y3, .. } => Point::new(*x3, *y3),
                    PathElement::LineTo(x, y) => Point::new(*x, *y),
                    _ => off_start,
                }
            } else {
                off_start
            };
            (result_elems, off_start, off_end)
        }
        _ => (vec![], start, start),
    }
}

fn tangent_at_start(start: Point, seg: &PathElement) -> Point {
    match seg {
        PathElement::LineTo(x, y) => Point::new(*x - start.x, *y - start.y),
        PathElement::CurveTo {
            x1,
            y1,
            x2,
            y2,
            x3,
            y3,
        } => {
            let mut t = Point::new(*x1 - start.x, *y1 - start.y);
            if t.length() < 1e-4 {
                t = Point::new(*x2 - start.x, *y2 - start.y);
            }
            if t.length() < 1e-4 {
                t = Point::new(*x3 - start.x, *y3 - start.y);
            }
            t
        }
        _ => Point::new(1.0, 0.0),
    }
}

fn tangent_at_end(start: Point, seg: &PathElement) -> Point {
    match seg {
        PathElement::LineTo(x, y) => Point::new(*x - start.x, *y - start.y),
        PathElement::CurveTo {
            x1,
            y1,
            x2,
            y2,
            x3,
            y3,
        } => {
            let end = Point::new(*x3, *y3);
            let mut t = Point::new(end.x - *x2, end.y - *y2);
            if t.length() < 1e-4 {
                t = Point::new(end.x - *x1, end.y - *y1);
            }
            if t.length() < 1e-4 {
                t = Point::new(end.x - start.x, end.y - start.y);
            }
            t
        }
        _ => Point::new(1.0, 0.0),
    }
}

// ---------------------------------------------------------------------------
// Line joins
// ---------------------------------------------------------------------------

/// Convert arc to cubic Bézier approximations (max 90° per segment).
fn arc_to_cubics(center: Point, radius: f64, start_angle: f64, end_angle: f64) -> Vec<PathElement> {
    let mut result = Vec::new();
    let mut angle = end_angle - start_angle;
    // Normalize to (-pi, pi] with epsilon tolerance
    while angle > PI + 1e-10 {
        angle -= 2.0 * PI;
    }
    while angle < -PI - 1e-10 {
        angle += 2.0 * PI;
    }

    let n_segs = (angle.abs() / (PI / 2.0)).ceil().max(1.0) as usize;
    let seg_angle = angle / n_segs as f64;

    for i in 0..n_segs {
        let a0 = start_angle + i as f64 * seg_angle;
        let a1 = a0 + seg_angle;
        let alpha = 4.0 * (seg_angle / 4.0).tan() / 3.0;

        let (cos0, sin0) = (a0.cos(), a0.sin());
        let (cos1, sin1) = (a1.cos(), a1.sin());

        let p1x = center.x + radius * (cos0 - alpha * sin0);
        let p1y = center.y + radius * (sin0 + alpha * cos0);
        let p2x = center.x + radius * (cos1 + alpha * sin1);
        let p2y = center.y + radius * (sin1 - alpha * cos1);
        let p3x = center.x + radius * cos1;
        let p3y = center.y + radius * sin1;

        result.push(PathElement::CurveTo {
            x1: p1x,
            y1: p1y,
            x2: p2x,
            y2: p2y,
            x3: p3x,
            y3: p3y,
        });
    }

    result
}

fn circle_line_intersection(
    center: Point,
    radius: f64,
    line_pt: Point,
    line_dir: Point,
) -> Vec<Point> {
    let dx = line_pt.x - center.x;
    let dy = line_pt.y - center.y;
    let a = line_dir.x * line_dir.x + line_dir.y * line_dir.y;
    if a < 1e-24 {
        return vec![];
    }
    let b = 2.0 * (dx * line_dir.x + dy * line_dir.y);
    let c = dx * dx + dy * dy - radius * radius;
    let disc = b * b - 4.0 * a * c;
    if disc < 0.0 {
        return vec![];
    }
    let sqrt_disc = disc.max(0.0).sqrt();
    let t1 = (-b - sqrt_disc) / (2.0 * a);
    let t2 = (-b + sqrt_disc) / (2.0 * a);
    vec![
        Point::new(line_pt.x + t1 * line_dir.x, line_pt.y + t1 * line_dir.y),
        Point::new(line_pt.x + t2 * line_dir.x, line_pt.y + t2 * line_dir.y),
    ]
}

fn line_line_intersection(p1: Point, d1: Point, p2: Point, d2: Point) -> Option<Point> {
    let denom = d1.x * d2.y - d1.y * d2.x;
    if denom.abs() < 1e-12 {
        return None;
    }
    let diff = p2.sub(p1);
    let t = (diff.x * d2.y - diff.y * d2.x) / denom;
    Some(Point::new(p1.x + t * d1.x, p1.y + t * d1.y))
}

fn compute_inner_join_point(
    prev_end: Point,
    prev_tangent: Point,
    next_start: Point,
    next_tangent: Point,
    half_width: f64,
) -> Option<Point> {
    let d_prev = prev_tangent.normalized();
    let d_next = next_tangent.normalized();

    let cross = d_prev.cross(d_next);
    if cross.abs() < 1e-6 {
        return None;
    }

    let pt = line_line_intersection(prev_end, d_prev, next_start, d_next)?;

    // Direction check
    let t_prev = pt.sub(prev_end).dot(d_prev);
    let t_next = pt.sub(next_start).dot(d_next);
    if t_prev < 0.0 || t_next > 0.0 {
        return None;
    }

    // Distance sanity check
    if half_width > 0.0 {
        let mid = Point::new(
            (prev_end.x + next_start.x) * 0.5,
            (prev_end.y + next_start.y) * 0.5,
        );
        let dist = pt.sub(mid).length();
        let gap = next_start.sub(prev_end).length();
        let limit = (gap * 2.0).max(half_width * 4.0);
        if dist > limit {
            return None;
        }
    }

    Some(pt)
}

fn make_join(
    prev_end: Point,
    prev_tangent: Point,
    next_start: Point,
    next_tangent: Point,
    half_width: f64,
    join_type: i32,
    miter_limit: f64,
    side: f64,
) -> Vec<PathElement> {
    let n_prev = Point::new(-prev_tangent.y, prev_tangent.x)
        .normalized()
        .scale(half_width * side);
    let n_next = Point::new(-next_tangent.y, next_tangent.x)
        .normalized()
        .scale(half_width * side);

    let cross = prev_tangent.cross(next_tangent);
    let dot = prev_tangent.dot(next_tangent);
    let mut is_outside = (cross * side) < 0.0;
    if cross.abs() < 1e-6 && dot < 0.0 {
        is_outside = true;
    }

    if !is_outside {
        return vec![];
    }

    if join_type == 2 {
        // Bevel
        return vec![PathElement::LineTo(next_start.x, next_start.y)];
    }

    if join_type == 1 {
        // Round
        let vertex = prev_end.sub(n_prev);
        let a0 = n_prev.y.atan2(n_prev.x);
        let a1 = n_next.y.atan2(n_next.x);

        // Handle near-180° arcs
        let mut raw = a1 - a0;
        while raw > PI + 1e-10 {
            raw -= 2.0 * PI;
        }
        while raw < -PI - 1e-10 {
            raw += 2.0 * PI;
        }
        if (raw.abs() - PI).abs() < 0.1 {
            let mid_angle = a0 + raw / 2.0;
            let arc_mid_dir = Point::new(mid_angle.cos(), mid_angle.sin());
            let pt = prev_tangent.normalized();
            if arc_mid_dir.dot(pt) < 0.0 {
                if raw > 0.0 {
                    raw -= 2.0 * PI;
                } else {
                    raw += 2.0 * PI;
                }
            }
        }
        let a1_adjusted = a0 + raw;

        let curves = arc_to_cubics(vertex, half_width, a0, a1_adjusted);
        if curves.is_empty() {
            return vec![PathElement::LineTo(next_start.x, next_start.y)];
        }
        return curves;
    }

    // Miter (type 0)
    let d_prev = prev_tangent.normalized();
    let d_next = next_tangent.normalized();
    let denom = d_prev.cross(d_next);

    if denom.abs() < 1e-12 {
        return vec![PathElement::LineTo(next_start.x, next_start.y)];
    }

    let dot_val = (d_prev.x * d_next.x + d_prev.y * d_next.y).clamp(-1.0, 1.0);
    let cos_half = ((1.0 + dot_val) / 2.0).max(0.0).sqrt();
    if cos_half < 1e-12 {
        return vec![PathElement::LineTo(next_start.x, next_start.y)];
    }
    let miter_ratio = 1.0 / cos_half;
    if miter_ratio > miter_limit {
        return vec![PathElement::LineTo(next_start.x, next_start.y)];
    }

    let diff = next_start.sub(prev_end);
    let t = diff.cross(d_next) / denom;
    let miter_pt = Point::new(prev_end.x + t * d_prev.x, prev_end.y + t * d_prev.y);
    vec![
        PathElement::LineTo(miter_pt.x, miter_pt.y),
        PathElement::LineTo(next_start.x, next_start.y),
    ]
}

// ---------------------------------------------------------------------------
// Line caps
// ---------------------------------------------------------------------------

fn make_cap(
    point: Point,
    tangent: Point,
    half_width: f64,
    cap_type: i32,
    is_start: bool,
) -> Vec<PathElement> {
    let mut t = tangent.normalized();
    if is_start {
        t = t.neg();
    }

    let n = Point::new(-t.y, t.x);
    let _left = point.add(n.scale(half_width));
    let right = point.sub(n.scale(half_width));

    if cap_type == 0 {
        // Butt
        return vec![PathElement::LineTo(right.x, right.y)];
    }

    if cap_type == 1 {
        // Round — two 90° arcs
        let a0 = n.y.atan2(n.x);
        let a_mid = a0 - PI / 2.0;
        let a1 = a_mid - PI / 2.0;
        let mut curves = arc_to_cubics(point, half_width, a0, a_mid);
        curves.extend(arc_to_cubics(point, half_width, a_mid, a1));
        if curves.is_empty() {
            return vec![PathElement::LineTo(right.x, right.y)];
        }
        return curves;
    }

    if cap_type == 2 {
        // Projecting square
        let left = point.add(n.scale(half_width));
        let ext = t.scale(half_width);
        let p1 = left.add(ext);
        let p2 = right.add(ext);
        return vec![
            PathElement::LineTo(p1.x, p1.y),
            PathElement::LineTo(p2.x, p2.y),
            PathElement::LineTo(right.x, right.y),
        ];
    }

    vec![PathElement::LineTo(right.x, right.y)]
}

// ---------------------------------------------------------------------------
// Outline assembly
// ---------------------------------------------------------------------------

fn get_geometric_segments(sp: &[PathElement]) -> Vec<(Point, PathElement)> {
    let mut segments = Vec::new();
    let mut current = subpath_start(sp);
    for elem in sp {
        match elem {
            PathElement::MoveTo(x, y) => {
                current = Point::new(*x, *y);
            }
            PathElement::LineTo(..) | PathElement::CurveTo { .. } => {
                let ep = segment_endpoint(elem).unwrap_or(current);
                // Skip near-zero-length segments
                if let PathElement::CurveTo {
                    x1,
                    y1,
                    x2,
                    y2,
                    x3,
                    y3,
                } = elem
                {
                    let cp1 = Point::new(*x1, *y1);
                    let cp2 = Point::new(*x2, *y2);
                    let ep_pt = Point::new(*x3, *y3);
                    if ep_pt.sub(current).length() < 1e-4
                        && cp1.sub(current).length() < 1e-4
                        && cp2.sub(current).length() < 1e-4
                    {
                        continue;
                    }
                } else if ep.sub(current).length() < 1e-4 {
                    continue;
                }
                segments.push((current, elem.clone()));
                current = ep;
            }
            PathElement::ClosePath => {
                let start = subpath_start(sp);
                let dist = current.sub(start).length();
                if dist > 1e-12 {
                    let close_seg = PathElement::LineTo(start.x, start.y);
                    segments.push((current, close_seg));
                }
            }
        }
    }
    segments
}

fn filter_uturn_segments(
    segments: &[(Point, PathElement)],
    half_width: f64,
) -> Vec<(Point, PathElement)> {
    if segments.len() < 3 {
        return segments.to_vec();
    }

    fn is_uturn(tangent_a: Point, tangent_b: Point) -> bool {
        let la = tangent_a.length();
        let lb = tangent_b.length();
        if la < 1e-10 || lb < 1e-10 {
            return false;
        }
        let na = Point::new(tangent_a.x / la, tangent_a.y / la);
        let nb = Point::new(tangent_b.x / lb, tangent_b.y / lb);
        let cross = na.cross(nb);
        let dot = na.dot(nb);
        cross.abs() < 0.1 && dot < -0.5
    }

    let mut to_remove = std::collections::HashSet::new();
    for i in 0..segments.len() {
        let (start_i, ref seg_i) = segments[i];
        let ep_i = segment_endpoint(seg_i).unwrap_or(start_i);
        let seg_len = ep_i.sub(start_i).length();
        if seg_len > half_width {
            continue;
        }

        let prev_idx = i.wrapping_sub(1);
        let next_idx = i + 1;
        if prev_idx >= segments.len() || next_idx >= segments.len() {
            continue;
        }

        let (prev_start, ref prev_seg) = segments[prev_idx];
        let (next_start, ref next_seg) = segments[next_idx];

        let end_tang_prev = tangent_at_end(prev_start, prev_seg);
        let start_tang_cur = tangent_at_start(start_i, seg_i);
        let end_tang_cur = tangent_at_end(start_i, seg_i);
        let start_tang_next = tangent_at_start(next_start, next_seg);

        let ut1 = is_uturn(end_tang_prev, start_tang_cur);
        let ut2 = is_uturn(end_tang_cur, start_tang_next);

        if ut1 && ut2 {
            to_remove.insert(i);
        } else if (ut1 || ut2) && seg_len < half_width * 0.25 {
            to_remove.insert(i);
        }
    }

    if to_remove.is_empty() {
        return segments.to_vec();
    }

    let mut result: Vec<(Point, PathElement)> = Vec::new();
    for (i, (start, seg)) in segments.iter().enumerate() {
        if to_remove.contains(&i) {
            continue;
        }
        let mut start = *start;
        if !result.is_empty() && i > 0 && to_remove.contains(&(i - 1)) {
            let prev_end = segment_endpoint(&result.last().unwrap().1).unwrap_or(start);
            start = prev_end;
        }
        result.push((start, seg.clone()));
    }
    result
}

/// Convert a stroked path to filled outline path(s), returned as groups.
pub fn strokepath_grouped(
    path: &Path,
    line_width: f64,
    line_cap: i32,
    line_join: i32,
    miter_limit: f64,
    dash_array: Option<&[f64]>,
    dash_offset: f64,
    tolerance: f64,
) -> Vec<Path> {
    let half_width = line_width / 2.0;
    if half_width < 1e-12 {
        return vec![];
    }

    // Apply dash pattern
    let working_subpaths;
    let working_ref: &Path;
    if let Some(da) = dash_array {
        if !da.is_empty() {
            working_subpaths = apply_dash_pattern(path, da, dash_offset);
            working_ref = &working_subpaths;
        } else {
            working_ref = path;
        }
    } else {
        working_ref = path;
    }

    let mut groups: Vec<Path> = Vec::new();

    for sp in working_ref {
        let closed = subpath_is_closed(sp);
        let segments = get_geometric_segments(sp);

        if segments.is_empty() {
            continue;
        }

        let segments = filter_uturn_segments(&segments, half_width);

        if segments.is_empty() {
            continue;
        }

        // Check for degenerate (zero-length) subpath
        let mut total_len = 0.0;
        for (start, seg) in &segments {
            match seg {
                PathElement::CurveTo {
                    x1,
                    y1,
                    x2,
                    y2,
                    x3,
                    y3,
                } => {
                    let p1 = Point::new(*x1, *y1);
                    let p2 = Point::new(*x2, *y2);
                    let p3 = Point::new(*x3, *y3);
                    total_len +=
                        p1.sub(*start).length() + p2.sub(p1).length() + p3.sub(p2).length();
                }
                _ => {
                    let ep = segment_endpoint(seg).unwrap_or(*start);
                    total_len += ep.sub(*start).length();
                }
            }
        }

        if total_len < 1e-12 {
            if line_cap == 1 {
                let pt = segments[0].0;
                let outline = make_circle(pt, half_width);
                groups.push(vec![outline]);
            }
            continue;
        }

        // Split self-overlapping curves (p0 ≈ p3)
        let mut split_segments = Vec::new();
        for (start, seg) in &segments {
            if let PathElement::CurveTo {
                x1,
                y1,
                x2,
                y2,
                x3,
                y3,
            } = seg
            {
                let p3 = Point::new(*x3, *y3);
                if p3.sub(*start).length() < 1e-2 {
                    let p0 = *start;
                    let p1 = Point::new(*x1, *y1);
                    let p2 = Point::new(*x2, *y2);
                    let (left_half, right_half) = split_cubic(p0, p1, p2, p3, 0.5);
                    split_segments.push((
                        left_half[0],
                        PathElement::CurveTo {
                            x1: left_half[1].x,
                            y1: left_half[1].y,
                            x2: left_half[2].x,
                            y2: left_half[2].y,
                            x3: left_half[3].x,
                            y3: left_half[3].y,
                        },
                    ));
                    split_segments.push((
                        right_half[0],
                        PathElement::CurveTo {
                            x1: right_half[1].x,
                            y1: right_half[1].y,
                            x2: right_half[2].x,
                            y2: right_half[2].y,
                            x3: right_half[3].x,
                            y3: right_half[3].y,
                        },
                    ));
                    continue;
                }
            }
            split_segments.push((*start, seg.clone()));
        }
        let segments = split_segments;

        // Compute left and right offsets
        let mut left_offsets: Vec<(Vec<PathElement>, Point, Point)> = Vec::new();
        let mut right_offsets: Vec<(Vec<PathElement>, Point, Point)> = Vec::new();

        for (start, seg) in &segments {
            let (l_elems, l_start, l_end) = offset_segment(*start, seg, half_width, tolerance);
            let (r_elems, r_start, r_end) = offset_segment(*start, seg, -half_width, tolerance);
            left_offsets.push((l_elems, l_start, l_end));
            right_offsets.push((r_elems, r_start, r_end));
        }

        if closed {
            let left_outline = assemble_closed_outline(
                &segments,
                &left_offsets,
                half_width,
                line_join,
                miter_limit,
                1.0,
            );
            let right_outline = assemble_closed_outline(
                &segments,
                &right_offsets,
                half_width,
                line_join,
                miter_limit,
                -1.0,
            );
            let mut group: Path = Vec::new();
            if !left_outline.is_empty() {
                group.push(left_outline);
            }
            if !right_outline.is_empty() {
                group.push(reverse_closed_outline(&right_outline));
            }
            if !group.is_empty() {
                groups.push(group);
            }
        } else {
            let outline = assemble_open_outline(
                &segments,
                &left_offsets,
                &right_offsets,
                half_width,
                line_cap,
                line_join,
                miter_limit,
            );
            if !outline.is_empty() {
                groups.push(vec![outline]);
            }
        }
    }

    groups
}

fn make_circle(center: Point, radius: f64) -> SubPath {
    let mut result: SubPath = Vec::new();
    result.push(PathElement::MoveTo(center.x + radius, center.y));
    for i in 0..4 {
        let a0 = i as f64 * PI / 2.0;
        let a1 = a0 + PI / 2.0;
        let arcs = arc_to_cubics(center, radius, a0, a1);
        result.extend(arcs);
    }
    result.push(PathElement::ClosePath);
    result
}

fn reverse_closed_outline(sp: &[PathElement]) -> SubPath {
    if sp.len() < 3 {
        return sp.to_vec();
    }

    let start = match &sp[0] {
        PathElement::MoveTo(x, y) => Point::new(*x, *y),
        _ => return sp.to_vec(),
    };

    let inner = &sp[1..sp.len() - 1]; // skip MoveTo and ClosePath

    let mut points = vec![start];
    for e in inner {
        match e {
            PathElement::LineTo(x, y) => points.push(Point::new(*x, *y)),
            PathElement::CurveTo { x3, y3, .. } => points.push(Point::new(*x3, *y3)),
            _ => {}
        }
    }

    let mut result: SubPath = Vec::new();
    let last = points.last().copied().unwrap_or(start);
    result.push(PathElement::MoveTo(last.x, last.y));

    for i in (0..inner.len()).rev() {
        let target = points[i];
        match &inner[i] {
            PathElement::LineTo(..) => {
                result.push(PathElement::LineTo(target.x, target.y));
            }
            PathElement::CurveTo { x1, y1, x2, y2, .. } => {
                result.push(PathElement::CurveTo {
                    x1: *x2,
                    y1: *y2,
                    x2: *x1,
                    y2: *y1,
                    x3: target.x,
                    y3: target.y,
                });
            }
            _ => {}
        }
    }

    result.push(PathElement::ClosePath);
    result
}

fn trim_outline_end(outline: &mut SubPath, pt: Point) {
    for j in (0..outline.len()).rev() {
        match &outline[j] {
            PathElement::LineTo(..) => {
                outline[j] = PathElement::LineTo(pt.x, pt.y);
                return;
            }
            PathElement::CurveTo {
                x1,
                y1,
                x2,
                y2,
                x3,
                y3,
            } => {
                let orig_end = Point::new(*x3, *y3);
                let dist = pt.sub(orig_end).length();
                let cp_span = ((*x2 - *x3).abs() + (*y2 - *y3).abs())
                    .max((*x1 - *x3).abs() + (*y1 - *y3).abs())
                    .max(1e-6);
                if dist < cp_span * 0.5 {
                    outline[j] = PathElement::CurveTo {
                        x1: *x1,
                        y1: *y1,
                        x2: *x2,
                        y2: *y2,
                        x3: pt.x,
                        y3: pt.y,
                    };
                } else {
                    outline[j] = PathElement::LineTo(pt.x, pt.y);
                }
                return;
            }
            _ => {}
        }
    }
}

fn assemble_closed_outline(
    segments: &[(Point, PathElement)],
    offsets: &[(Vec<PathElement>, Point, Point)],
    half_width: f64,
    line_join: i32,
    miter_limit: f64,
    side: f64,
) -> SubPath {
    if offsets.is_empty() {
        return vec![];
    }

    let mut outline: SubPath = Vec::new();

    let first_start = offsets[0].1;
    outline.push(PathElement::MoveTo(first_start.x, first_start.y));

    for i in 0..offsets.len() {
        let (ref elems, _, end) = offsets[i];
        outline.extend(elems.iter().cloned());

        let next_i = (i + 1) % offsets.len();
        let next_start = offsets[next_i].1;

        let prev_tangent = tangent_at_end(segments[i].0, &segments[i].1);
        let next_tangent = tangent_at_start(segments[next_i].0, &segments[next_i].1);

        let cross = prev_tangent.cross(next_tangent);
        let dot = prev_tangent.dot(next_tangent);
        let mut is_outside = (cross * side) < 0.0;
        if cross.abs() < 1e-6 && dot < 0.0 {
            is_outside = true;
        }
        if !is_outside {
            let trim_pt =
                compute_inner_join_point(end, prev_tangent, next_start, next_tangent, half_width);
            if let Some(tp) = trim_pt {
                trim_outline_end(&mut outline, tp);
                continue;
            } else {
                outline.push(PathElement::LineTo(next_start.x, next_start.y));
                continue;
            }
        }

        let join_elems = make_join(
            end,
            prev_tangent,
            next_start,
            next_tangent,
            half_width,
            line_join,
            miter_limit,
            side,
        );
        outline.extend(join_elems);
    }

    outline.push(PathElement::ClosePath);
    outline
}

#[allow(clippy::too_many_arguments)]
fn assemble_open_outline(
    segments: &[(Point, PathElement)],
    left_offsets: &[(Vec<PathElement>, Point, Point)],
    right_offsets: &[(Vec<PathElement>, Point, Point)],
    half_width: f64,
    line_cap: i32,
    line_join: i32,
    miter_limit: f64,
) -> SubPath {
    if left_offsets.is_empty() {
        return vec![];
    }

    let mut outline: SubPath = Vec::new();

    // --- Pre-detect degenerate first left segment ---
    let mut degenerate_first_left = false;
    let mut degenerate_first_trim_pt: Option<Point> = None;
    if line_cap == 1 && left_offsets.len() > 1 {
        let end0 = left_offsets[0].2;
        let next_start0 = left_offsets[1].1;
        let prev_tang0 = tangent_at_end(segments[0].0, &segments[0].1);
        let next_tang0 = tangent_at_start(segments[1].0, &segments[1].1);
        let cross0 = prev_tang0.cross(next_tang0);
        let dot0 = prev_tang0.dot(next_tang0);
        let mut is_outside0 = (cross0 * 1.0) < 0.0;
        if cross0.abs() < 1e-6 && dot0 < 0.0 {
            is_outside0 = true;
        }
        if !is_outside0 {
            if let Some(trim_pt0) =
                compute_inner_join_point(end0, prev_tang0, next_start0, next_tang0, half_width)
            {
                let fwd_dir = Point::new(
                    left_offsets[0].2.x - left_offsets[0].1.x,
                    left_offsets[0].2.y - left_offsets[0].1.y,
                );
                let trim_from_start = Point::new(
                    trim_pt0.x - left_offsets[0].1.x,
                    trim_pt0.y - left_offsets[0].1.y,
                );
                if fwd_dir.dot(trim_from_start) < 0.0 {
                    degenerate_first_left = true;
                    degenerate_first_trim_pt = Some(trim_pt0);
                }
            }
        }
    }

    // --- Left side (forward direction) ---
    if degenerate_first_left {
        let tp = degenerate_first_trim_pt.unwrap();
        outline.push(PathElement::MoveTo(tp.x, tp.y));
    } else {
        let first_left_start = left_offsets[0].1;
        outline.push(PathElement::MoveTo(first_left_start.x, first_left_start.y));
    }

    let mut degenerate_last_left = false;
    let mut _degenerate_last_left_trim_pt: Option<Point> = None;
    let mut skip_left_indices = std::collections::HashSet::new();

    for i in 0..left_offsets.len() {
        if i == 0 && degenerate_first_left {
            continue;
        }
        if skip_left_indices.contains(&i) {
            continue;
        }

        let (ref elems, _, end) = left_offsets[i];
        outline.extend(elems.iter().cloned());

        if i < left_offsets.len() - 1 {
            let next_start = left_offsets[i + 1].1;
            let prev_tangent = tangent_at_end(segments[i].0, &segments[i].1);
            let next_tangent = tangent_at_start(segments[i + 1].0, &segments[i + 1].1);

            let cross = prev_tangent.cross(next_tangent);
            let dot = prev_tangent.dot(next_tangent);
            let mut is_outside = (cross * 1.0) < 0.0; // side=+1 for left
            if cross.abs() < 1e-6 && dot < 0.0 {
                is_outside = true;
            }

            if !is_outside {
                let trim_pt = compute_inner_join_point(
                    end,
                    prev_tangent,
                    next_start,
                    next_tangent,
                    half_width,
                );
                if let Some(tp) = trim_pt {
                    // Check if trim overshoots past the NEXT segment's end
                    let next_end = left_offsets[i + 1].2;
                    let next_fwd = Point::new(next_end.x - next_start.x, next_end.y - next_start.y);
                    let trim_from_next_start = Point::new(tp.x - next_start.x, tp.y - next_start.y);
                    let next_len_sq = next_fwd.dot(next_fwd);
                    let t_param = if next_len_sq > 0.0 {
                        next_fwd.dot(trim_from_next_start) / next_len_sq
                    } else {
                        0.0
                    };
                    if t_param > 1.0 {
                        if line_cap == 1 {
                            trim_outline_end(&mut outline, tp);
                            skip_left_indices.insert(i + 1);
                            if i + 1 == left_offsets.len() - 1 {
                                degenerate_last_left = true;
                                _degenerate_last_left_trim_pt = Some(tp);
                            }
                            continue;
                        }
                    }
                    trim_outline_end(&mut outline, tp);
                    continue;
                } else {
                    outline.push(PathElement::LineTo(next_start.x, next_start.y));
                    continue;
                }
            }

            let join_elems = make_join(
                end,
                prev_tangent,
                next_start,
                next_tangent,
                half_width,
                line_join,
                miter_limit,
                1.0,
            );
            outline.extend(join_elems);
        }
    }

    // --- End cap ---
    let (last_seg_start, ref last_seg) = segments[segments.len() - 1];
    let end_tangent = tangent_at_end(last_seg_start, last_seg);
    let end_point = segment_endpoint(last_seg).unwrap_or(last_seg_start);

    // Pre-detect degenerate last right
    let mut degenerate_last_right = false;
    let mut degenerate_trim_pt: Option<Point> = None;
    let last_ri = right_offsets.len() - 1;
    if line_cap == 1 && last_ri > 0 {
        let prev_right_start = right_offsets[last_ri].1;
        let next_right_end = right_offsets[last_ri - 1].2;
        let mut prev_tangent_r = tangent_at_start(segments[last_ri].0, &segments[last_ri].1);
        prev_tangent_r = prev_tangent_r.neg();
        let mut next_tangent_r = tangent_at_end(segments[last_ri - 1].0, &segments[last_ri - 1].1);
        next_tangent_r = next_tangent_r.neg();

        let cross = prev_tangent_r.cross(next_tangent_r);
        let dot_r = prev_tangent_r.dot(next_tangent_r);
        let mut is_outside = (cross * 1.0) < 0.0;
        if cross.abs() < 1e-6 && dot_r < 0.0 {
            is_outside = true;
        }
        if !is_outside {
            if let Some(tp) = compute_inner_join_point(
                prev_right_start,
                prev_tangent_r,
                next_right_end,
                next_tangent_r,
                half_width,
            ) {
                let orig_dir = Point::new(
                    right_offsets[last_ri].1.x - right_offsets[last_ri].2.x,
                    right_offsets[last_ri].1.y - right_offsets[last_ri].2.y,
                );
                let trim_dir = Point::new(
                    tp.x - right_offsets[last_ri].2.x,
                    tp.y - right_offsets[last_ri].2.y,
                );
                if orig_dir.dot(trim_dir) < 0.0 {
                    degenerate_last_right = true;
                    degenerate_trim_pt = Some(tp);
                }
            }
        }
    }

    if degenerate_last_left && line_cap == 1 {
        // Truncated end cap on LEFT side
        let prev_left_idx = left_offsets.len() - 2;
        let prev_left_end = left_offsets[prev_left_idx].2;
        let prev_left_tangent =
            tangent_at_end(segments[prev_left_idx].0, &segments[prev_left_idx].1);
        let prev_left_dir = prev_left_tangent.normalized();

        let hits = circle_line_intersection(end_point, half_width, prev_left_end, prev_left_dir);
        if hits.len() >= 2 {
            let t_n = end_tangent.normalized();
            let n = Point::new(-t_n.y, t_n.x);
            let right_n = Point::new(t_n.y, -t_n.x);
            let a_right = right_n.y.atan2(right_n.x);
            let a_left = n.y.atan2(n.x);

            let mut best_hit: Option<Point> = None;
            let mut best_angle_dist = f64::INFINITY;
            let mut best_a_h = 0.0;
            for h in &hits {
                let a_h = (h.y - end_point.y).atan2(h.x - end_point.x);
                let mut delta = a_left - a_h;
                while delta < 0.0 {
                    delta += 2.0 * PI;
                }
                while delta > 2.0 * PI {
                    delta -= 2.0 * PI;
                }
                if delta < PI + 0.01 && delta < best_angle_dist {
                    best_angle_dist = delta;
                    best_hit = Some(*h);
                    best_a_h = a_h;
                }
            }

            if let Some(bh) = best_hit {
                outline.push(PathElement::LineTo(bh.x, bh.y));
                if (best_a_h - a_right).abs() > 1e-10 {
                    let arc_elems = arc_to_cubics(end_point, half_width, best_a_h, a_right);
                    outline.extend(arc_elems);
                }
            } else {
                let cap_elems = make_cap(end_point, end_tangent, half_width, line_cap, false);
                outline.extend(cap_elems);
            }
        } else {
            let cap_elems = make_cap(end_point, end_tangent, half_width, line_cap, false);
            outline.extend(cap_elems);
        }
    } else if degenerate_last_right && line_cap == 1 {
        // Truncated round cap on RIGHT side
        let seg0_right_end = right_offsets[last_ri - 1].2;
        let seg0_tangent = tangent_at_end(segments[last_ri - 1].0, &segments[last_ri - 1].1);
        let seg0_dir = seg0_tangent.normalized();

        let hits = circle_line_intersection(end_point, half_width, seg0_right_end, seg0_dir);
        if hits.len() >= 2 {
            let t_n = end_tangent.normalized();
            let n = Point::new(-t_n.y, t_n.x);
            let a_start = n.y.atan2(n.x);

            let mut best_hit: Option<Point> = None;
            let mut best_angle_dist = -1.0f64;
            for h in &hits {
                let a_h = (h.y - end_point.y).atan2(h.x - end_point.x);
                let mut delta = a_start - a_h;
                while delta < 0.0 {
                    delta += 2.0 * PI;
                }
                while delta > 2.0 * PI {
                    delta -= 2.0 * PI;
                }
                if delta > 1e-6 && delta < PI + 0.01 && delta > best_angle_dist {
                    best_angle_dist = delta;
                    best_hit = Some(*h);
                }
            }

            if let Some(bh) = best_hit {
                let a_end = (bh.y - end_point.y).atan2(bh.x - end_point.x);
                if (a_start - a_end).abs() > 1e-10 {
                    let arc_elems = arc_to_cubics(end_point, half_width, a_start, a_end);
                    outline.extend(arc_elems);
                }
                outline.push(PathElement::LineTo(
                    degenerate_trim_pt.unwrap().x,
                    degenerate_trim_pt.unwrap().y,
                ));
            } else {
                let cap_elems = make_cap(end_point, end_tangent, half_width, line_cap, false);
                outline.extend(cap_elems);
            }
        } else {
            let cap_elems = make_cap(end_point, end_tangent, half_width, line_cap, false);
            outline.extend(cap_elems);
        }
    } else {
        let cap_elems = make_cap(end_point, end_tangent, half_width, line_cap, false);
        outline.extend(cap_elems);
    }

    // --- Right side (reverse direction) ---
    for i in (0..right_offsets.len()).rev() {
        let (ref elems, start, _end) = right_offsets[i];
        let reversed_elems = reverse_offset_elements(elems, start);

        let mut skip_reversed = false;
        if i == last_ri && degenerate_last_right {
            skip_reversed = true;
        } else if line_cap == 1 && i > 0 {
            let prev_right_start = right_offsets[i].1;
            let next_right_end = right_offsets[i - 1].2;
            let mut prev_tangent_chk = tangent_at_start(segments[i].0, &segments[i].1);
            prev_tangent_chk = prev_tangent_chk.neg();
            let mut next_tangent_chk = tangent_at_end(segments[i - 1].0, &segments[i - 1].1);
            next_tangent_chk = next_tangent_chk.neg();

            let cross = prev_tangent_chk.cross(next_tangent_chk);
            let dot_chk = prev_tangent_chk.dot(next_tangent_chk);
            let mut is_outside = (cross * 1.0) < 0.0;
            if cross.abs() < 1e-6 && dot_chk < 0.0 {
                is_outside = true;
            }
            if !is_outside {
                if let Some(tp) = compute_inner_join_point(
                    prev_right_start,
                    prev_tangent_chk,
                    next_right_end,
                    next_tangent_chk,
                    half_width,
                ) {
                    let orig_dir = Point::new(
                        right_offsets[i].1.x - right_offsets[i].2.x,
                        right_offsets[i].1.y - right_offsets[i].2.y,
                    );
                    let trim_dir =
                        Point::new(tp.x - right_offsets[i].2.x, tp.y - right_offsets[i].2.y);
                    if orig_dir.dot(trim_dir) < 0.0 {
                        skip_reversed = true;
                    }
                }
            }
        }

        if !skip_reversed {
            outline.extend(reversed_elems);
        }

        if i > 0 {
            if skip_reversed {
                continue;
            }

            let prev_right_start = right_offsets[i].1;
            let next_right_end = right_offsets[i - 1].2;
            let mut prev_tangent = tangent_at_start(segments[i].0, &segments[i].1);
            prev_tangent = prev_tangent.neg();
            let mut next_tangent = tangent_at_end(segments[i - 1].0, &segments[i - 1].1);
            next_tangent = next_tangent.neg();

            let cross = prev_tangent.cross(next_tangent);
            let dot_rt = prev_tangent.dot(next_tangent);
            let mut is_outside = (cross * 1.0) < 0.0;
            if cross.abs() < 1e-6 && dot_rt < 0.0 {
                is_outside = true;
            }
            if !is_outside {
                let trim_pt = compute_inner_join_point(
                    prev_right_start,
                    prev_tangent,
                    next_right_end,
                    next_tangent,
                    half_width,
                );
                if let Some(tp) = trim_pt {
                    trim_outline_end(&mut outline, tp);
                    continue;
                } else {
                    outline.push(PathElement::LineTo(next_right_end.x, next_right_end.y));
                    continue;
                }
            }

            let join_elems = make_join(
                prev_right_start,
                prev_tangent,
                next_right_end,
                next_tangent,
                half_width,
                line_join,
                miter_limit,
                1.0,
            );
            outline.extend(join_elems);
        }
    }

    // --- Start cap ---
    let (first_seg_start, ref first_seg) = segments[0];
    let start_tangent = tangent_at_start(first_seg_start, first_seg);

    if degenerate_first_left && line_cap == 1 {
        let t_s = start_tangent.normalized().neg();
        let n_s = Point::new(-t_s.y, t_s.x);
        let a_start_angle = n_s.y.atan2(n_s.x);

        let seg1_left_start = left_offsets[1].1;
        let seg1_tangent = tangent_at_start(segments[1].0, &segments[1].1);
        let seg1_dir = seg1_tangent.normalized();

        let hits = circle_line_intersection(first_seg_start, half_width, seg1_left_start, seg1_dir);
        if hits.len() >= 2 {
            let mut best_hit: Option<Point> = None;
            let mut best_angle_dist = -1.0f64;
            for h in &hits {
                let a_h = (h.y - first_seg_start.y).atan2(h.x - first_seg_start.x);
                let mut delta = a_start_angle - a_h;
                while delta < 0.0 {
                    delta += 2.0 * PI;
                }
                while delta > 2.0 * PI {
                    delta -= 2.0 * PI;
                }
                if delta > 1e-6 && delta < PI + 0.01 && delta > best_angle_dist {
                    best_angle_dist = delta;
                    best_hit = Some(*h);
                }
            }

            if let Some(bh) = best_hit {
                let a_end_angle = (bh.y - first_seg_start.y).atan2(bh.x - first_seg_start.x);
                if (a_start_angle - a_end_angle).abs() > 1e-10 {
                    let arc_elems =
                        arc_to_cubics(first_seg_start, half_width, a_start_angle, a_end_angle);
                    outline.extend(arc_elems);
                }
                outline.push(PathElement::LineTo(
                    degenerate_first_trim_pt.unwrap().x,
                    degenerate_first_trim_pt.unwrap().y,
                ));
            } else {
                let cap_elems =
                    make_cap(first_seg_start, start_tangent, half_width, line_cap, true);
                outline.extend(cap_elems);
            }
        } else {
            let cap_elems = make_cap(first_seg_start, start_tangent, half_width, line_cap, true);
            outline.extend(cap_elems);
        }
    } else {
        let cap_elems = make_cap(first_seg_start, start_tangent, half_width, line_cap, true);
        outline.extend(cap_elems);
    }

    outline.push(PathElement::ClosePath);
    outline
}

fn reverse_offset_elements(elems: &[PathElement], start: Point) -> Vec<PathElement> {
    if elems.is_empty() {
        return vec![];
    }

    let mut points = vec![start];
    for e in elems {
        match e {
            PathElement::LineTo(x, y) => points.push(Point::new(*x, *y)),
            PathElement::CurveTo { x3, y3, .. } => points.push(Point::new(*x3, *y3)),
            _ => {}
        }
    }

    let mut result = Vec::new();
    for i in (0..elems.len()).rev() {
        let target = points[i];
        match &elems[i] {
            PathElement::LineTo(..) => {
                result.push(PathElement::LineTo(target.x, target.y));
            }
            PathElement::CurveTo { x1, y1, x2, y2, .. } => {
                result.push(PathElement::CurveTo {
                    x1: *x2,
                    y1: *y2,
                    x2: *x1,
                    y2: *y1,
                    x3: target.x,
                    y3: target.y,
                });
            }
            _ => {}
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Pixel-snapping helpers (used by the operator layer)
// ---------------------------------------------------------------------------

/// Snap axis-aligned line segments to device pixel grid for crisp outlines.
pub fn snap_path_to_pixels(algo_path: &mut Path, half_width: f64) {
    fn snap_coord(val: f64, half_width: f64) -> f64 {
        (val - half_width).round() + half_width
    }

    for sp in algo_path.iter_mut() {
        let mut i = 0;
        while i < sp.len() {
            if let PathElement::LineTo(lx, ly) = sp[i] {
                if i > 0 {
                    let (px, py) = match &sp[i - 1] {
                        PathElement::MoveTo(x, y) | PathElement::LineTo(x, y) => (*x, *y),
                        _ => {
                            i += 1;
                            continue;
                        }
                    };

                    let dx = (lx - px).abs();
                    let dy = (ly - py).abs();

                    if dy < 0.01 && dx > 0.01 {
                        // Horizontal line — snap y
                        let snapped_y = snap_coord(py, half_width);
                        match &mut sp[i - 1] {
                            PathElement::MoveTo(_, y) | PathElement::LineTo(_, y) => {
                                *y = snapped_y;
                            }
                            _ => {}
                        }
                        sp[i] = PathElement::LineTo(lx, snapped_y);
                    } else if dx < 0.01 && dy > 0.01 {
                        // Vertical line — snap x
                        let snapped_x = snap_coord(px, half_width);
                        match &mut sp[i - 1] {
                            PathElement::MoveTo(x, _) | PathElement::LineTo(x, _) => {
                                *x = snapped_x;
                            }
                            _ => {}
                        }
                        sp[i] = PathElement::LineTo(snapped_x, ly);
                    }
                }
            }
            i += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// Path transform helpers (used by the operator layer)
// ---------------------------------------------------------------------------

/// Transform algo path points from device space to user space using inverse CTM components.
pub fn transform_algo_path(
    algo_path: &Path,
    m_a: f64,
    m_b: f64,
    m_c: f64,
    m_d: f64,
    tx: f64,
    ty: f64,
) -> Path {
    let mut result = Vec::new();
    for sp in algo_path {
        let mut new_sp = Vec::new();
        for elem in sp {
            match elem {
                PathElement::MoveTo(x, y) => {
                    let (dx, dy) = (*x - tx, *y - ty);
                    new_sp.push(PathElement::MoveTo(
                        m_a * dx + m_c * dy,
                        m_b * dx + m_d * dy,
                    ));
                }
                PathElement::LineTo(x, y) => {
                    let (dx, dy) = (*x - tx, *y - ty);
                    new_sp.push(PathElement::LineTo(
                        m_a * dx + m_c * dy,
                        m_b * dx + m_d * dy,
                    ));
                }
                PathElement::CurveTo {
                    x1,
                    y1,
                    x2,
                    y2,
                    x3,
                    y3,
                } => {
                    let (d1x, d1y) = (*x1 - tx, *y1 - ty);
                    let (d2x, d2y) = (*x2 - tx, *y2 - ty);
                    let (d3x, d3y) = (*x3 - tx, *y3 - ty);
                    new_sp.push(PathElement::CurveTo {
                        x1: m_a * d1x + m_c * d1y,
                        y1: m_b * d1x + m_d * d1y,
                        x2: m_a * d2x + m_c * d2y,
                        y2: m_b * d2x + m_d * d2y,
                        x3: m_a * d3x + m_c * d3y,
                        y3: m_b * d3x + m_d * d3y,
                    });
                }
                PathElement::ClosePath => {
                    new_sp.push(PathElement::ClosePath);
                }
            }
        }
        if !new_sp.is_empty() {
            result.push(new_sp);
        }
    }
    result
}

/// Transform stroke outline groups from user space back to device space.
pub fn transform_stroke_groups(
    groups: &[Path],
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    tx: f64,
    ty: f64,
) -> Vec<Path> {
    let mut result = Vec::new();
    for group in groups {
        let mut new_group = Vec::new();
        for sp in group {
            let mut new_sp = Vec::new();
            for elem in sp {
                match elem {
                    PathElement::MoveTo(x, y) => {
                        new_sp.push(PathElement::MoveTo(
                            a * *x + c * *y + tx,
                            b * *x + d * *y + ty,
                        ));
                    }
                    PathElement::LineTo(x, y) => {
                        new_sp.push(PathElement::LineTo(
                            a * *x + c * *y + tx,
                            b * *x + d * *y + ty,
                        ));
                    }
                    PathElement::CurveTo {
                        x1,
                        y1,
                        x2,
                        y2,
                        x3,
                        y3,
                    } => {
                        new_sp.push(PathElement::CurveTo {
                            x1: a * *x1 + c * *y1 + tx,
                            y1: b * *x1 + d * *y1 + ty,
                            x2: a * *x2 + c * *y2 + tx,
                            y2: b * *x2 + d * *y2 + ty,
                            x3: a * *x3 + c * *y3 + tx,
                            y3: b * *x3 + d * *y3 + ty,
                        });
                    }
                    PathElement::ClosePath => {
                        new_sp.push(PathElement::ClosePath);
                    }
                }
            }
            if !new_sp.is_empty() {
                new_group.push(new_sp);
            }
        }
        result.push(new_group);
    }
    result
}
