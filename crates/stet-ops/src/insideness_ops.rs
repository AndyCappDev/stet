// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Insideness testing operators: infill, ineofill, instroke.
//!
//! These test whether a user-space point lies inside the area that would
//! be painted by fill, eofill, or stroke, without actually painting.

use stet_core::context::Context;
use stet_core::error::PsError;
use stet_core::object::PsObject;
use stet_fonts::geometry::{PathSegment, PsPath};

/// `infill`: x y → bool — test point against fill (nonzero winding rule)
///
/// Operands are user-space; transform through CTM to device space before testing
/// against the device-space path.
pub fn op_infill(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let y_obj = ctx.o_stack.peek(0)?;
    let x_obj = ctx.o_stack.peek(1)?;
    let y = y_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let x = x_obj.as_f64().ok_or(PsError::TypeCheck)?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;

    let (dx, dy) = ctx.gstate.ctm.transform_point(x, y);
    let result = if ctx.gstate.path.is_empty() {
        false
    } else {
        point_in_path(&ctx.gstate.path, dx, dy, ctx.gstate.flatness, true)
    };
    ctx.o_stack.push(PsObject::bool(result))?;
    Ok(())
}

/// `ineofill`: x y → bool — test point against fill (even-odd rule)
///
/// Operands are user-space; transform through CTM to device space before testing.
pub fn op_ineofill(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let y_obj = ctx.o_stack.peek(0)?;
    let x_obj = ctx.o_stack.peek(1)?;
    let y = y_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let x = x_obj.as_f64().ok_or(PsError::TypeCheck)?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;

    let (dx, dy) = ctx.gstate.ctm.transform_point(x, y);
    let result = if ctx.gstate.path.is_empty() {
        false
    } else {
        point_in_path(&ctx.gstate.path, dx, dy, ctx.gstate.flatness, false)
    };
    ctx.o_stack.push(PsObject::bool(result))?;
    Ok(())
}

/// `instroke`: x y → bool — test point against stroke outline
///
/// Operands are user-space; transform through CTM to device space before testing.
/// Line width must also be scaled to device space.
pub fn op_instroke(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let y_obj = ctx.o_stack.peek(0)?;
    let x_obj = ctx.o_stack.peek(1)?;
    let y = y_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let x = x_obj.as_f64().ok_or(PsError::TypeCheck)?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;

    let (dx, dy) = ctx.gstate.ctm.transform_point(x, y);
    let scale = (ctx.gstate.ctm.a.powi(2) + ctx.gstate.ctm.b.powi(2)).sqrt();
    let device_line_width = ctx.gstate.line_width * scale;
    let result = if ctx.gstate.path.is_empty() {
        false
    } else {
        point_in_stroke(
            &ctx.gstate.path,
            dx,
            dy,
            device_line_width,
            ctx.gstate.flatness,
        )
    };
    ctx.o_stack.push(PsObject::bool(result))?;
    Ok(())
}

// ---------- Algorithms ----------

/// Flatten a path's curves into line segments for ray-casting.
fn flatten_to_segments(path: &PsPath, flatness: f64) -> Vec<Subpath> {
    let mut subpaths: Vec<Subpath> = Vec::new();
    let mut current: Option<Subpath> = None;
    let mut cx = 0.0;
    let mut cy = 0.0;

    for seg in &path.segments {
        match seg {
            PathSegment::MoveTo(x, y) => {
                if let Some(sp) = current.take() {
                    subpaths.push(sp);
                }
                current = Some(Subpath {
                    moveto: (*x, *y),
                    segments: Vec::new(),
                    has_close: false,
                });
                cx = *x;
                cy = *y;
            }
            PathSegment::LineTo(x, y) => {
                if let Some(ref mut sp) = current {
                    sp.segments.push((cx, cy, *x, *y));
                }
                cx = *x;
                cy = *y;
            }
            PathSegment::CurveTo {
                x1,
                y1,
                x2,
                y2,
                x3,
                y3,
            } => {
                if let Some(ref mut sp) = current {
                    flatten_cubic(
                        cx,
                        cy,
                        *x1,
                        *y1,
                        *x2,
                        *y2,
                        *x3,
                        *y3,
                        flatness,
                        &mut sp.segments,
                    );
                }
                cx = *x3;
                cy = *y3;
            }
            PathSegment::ClosePath => {
                if let Some(ref mut sp) = current {
                    sp.has_close = true;
                }
            }
        }
    }
    if let Some(sp) = current {
        subpaths.push(sp);
    }
    subpaths
}

struct Subpath {
    moveto: (f64, f64),
    segments: Vec<(f64, f64, f64, f64)>, // (x0, y0, x1, y1)
    has_close: bool,
}

/// Flatten a cubic bezier into line segments.
#[allow(clippy::too_many_arguments)]
fn flatten_cubic(
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
    x3: f64,
    y3: f64,
    flatness: f64,
    segments: &mut Vec<(f64, f64, f64, f64)>,
) {
    let mx = (x0 + x3) / 2.0;
    let my = (y0 + y3) / 2.0;
    let cx = (x0 + 3.0 * x1 + 3.0 * x2 + x3) / 8.0;
    let cy = (y0 + 3.0 * y1 + 3.0 * y2 + y3) / 8.0;
    let dx = cx - mx;
    let dy = cy - my;

    if dx * dx + dy * dy <= flatness * flatness {
        segments.push((x0, y0, x3, y3));
        return;
    }

    let x01 = (x0 + x1) / 2.0;
    let y01 = (y0 + y1) / 2.0;
    let x12 = (x1 + x2) / 2.0;
    let y12 = (y1 + y2) / 2.0;
    let x23 = (x2 + x3) / 2.0;
    let y23 = (y2 + y3) / 2.0;
    let x012 = (x01 + x12) / 2.0;
    let y012 = (y01 + y12) / 2.0;
    let x123 = (x12 + x23) / 2.0;
    let y123 = (y12 + y23) / 2.0;
    let x0123 = (x012 + x123) / 2.0;
    let y0123 = (y012 + y123) / 2.0;

    flatten_cubic(
        x0, y0, x01, y01, x012, y012, x0123, y0123, flatness, segments,
    );
    flatten_cubic(
        x0123, y0123, x123, y123, x23, y23, x3, y3, flatness, segments,
    );
}

/// Ray-casting point-in-path test.
///
/// Casts a horizontal ray from (px, py) towards +x and counts crossings.
fn point_in_path(path: &PsPath, px: f64, py: f64, flatness: f64, use_winding: bool) -> bool {
    let subpaths = flatten_to_segments(path, flatness);
    let mut winding: i32 = 0;
    let mut crossings: i32 = 0;

    for sp in &subpaths {
        let mut segs = sp.segments.clone();

        // Implicit close: add closing segment if closepath present
        if sp.has_close
            && let Some(&(_, _, last_x, last_y)) = segs.last()
        {
            let (mx, my) = sp.moveto;
            if last_x != mx || last_y != my {
                segs.push((last_x, last_y, mx, my));
            }
        }

        for &(x0, y0, x1, y1) in &segs {
            // Skip if both endpoints on same side of ray
            if (y0 < py) == (y1 < py) {
                continue;
            }

            // Compute x-intercept of segment with horizontal line y = py
            let t = (py - y0) / (y1 - y0);
            let x_intercept = x0 + t * (x1 - x0);

            if x_intercept > px {
                crossings += 1;
                if use_winding {
                    if y1 > y0 {
                        winding += 1; // upward crossing
                    } else {
                        winding -= 1; // downward crossing
                    }
                }
            }
        }
    }

    if use_winding {
        winding != 0
    } else {
        (crossings % 2) == 1
    }
}

/// Test if point is within linewidth/2 of any path segment.
fn point_in_stroke(path: &PsPath, px: f64, py: f64, line_width: f64, flatness: f64) -> bool {
    let half_w = line_width / 2.0;
    let half_w_sq = half_w * half_w;
    let subpaths = flatten_to_segments(path, flatness);

    for sp in &subpaths {
        for &(x0, y0, x1, y1) in &sp.segments {
            let dist_sq = point_to_segment_dist_sq(px, py, x0, y0, x1, y1);
            if dist_sq <= half_w_sq {
                return true;
            }
        }
    }
    false
}

/// Squared distance from point (px, py) to line segment (x0,y0)-(x1,y1).
fn point_to_segment_dist_sq(px: f64, py: f64, x0: f64, y0: f64, x1: f64, y1: f64) -> f64 {
    let dx = x1 - x0;
    let dy = y1 - y0;
    let len_sq = dx * dx + dy * dy;

    if len_sq < 1e-20 {
        // Degenerate segment (point)
        let dpx = px - x0;
        let dpy = py - y0;
        return dpx * dpx + dpy * dpy;
    }

    // Project point onto line, clamped to [0,1]
    let t = ((px - x0) * dx + (py - y0) * dy) / len_sq;
    let t = t.clamp(0.0, 1.0);

    let proj_x = x0 + t * dx;
    let proj_y = y0 + t * dy;
    let dpx = px - proj_x;
    let dpy = py - proj_y;
    dpx * dpx + dpy * dpy
}

#[cfg(test)]
mod tests {
    use super::*;
    use stet_core::context::Context;
    use stet_core::object::{PsObject, PsValue};

    fn setup() -> Context {
        let mut ctx = Context::new();
        crate::build_system_dict(&mut ctx);
        ctx
    }

    #[test]
    fn test_infill_inside_rect() {
        let mut ctx = setup();
        ctx.gstate
            .path
            .segments
            .push(PathSegment::MoveTo(100.0, 100.0));
        ctx.gstate
            .path
            .segments
            .push(PathSegment::LineTo(200.0, 100.0));
        ctx.gstate
            .path
            .segments
            .push(PathSegment::LineTo(200.0, 200.0));
        ctx.gstate
            .path
            .segments
            .push(PathSegment::LineTo(100.0, 200.0));
        ctx.gstate.path.segments.push(PathSegment::ClosePath);
        ctx.o_stack.push(PsObject::real(150.0)).unwrap();
        ctx.o_stack.push(PsObject::real(150.0)).unwrap();
        op_infill(&mut ctx).unwrap();
        assert!(matches!(
            ctx.o_stack.pop().unwrap().value,
            PsValue::Bool(true)
        ));
    }

    #[test]
    fn test_infill_outside_rect() {
        let mut ctx = setup();
        ctx.gstate
            .path
            .segments
            .push(PathSegment::MoveTo(100.0, 100.0));
        ctx.gstate
            .path
            .segments
            .push(PathSegment::LineTo(200.0, 100.0));
        ctx.gstate
            .path
            .segments
            .push(PathSegment::LineTo(200.0, 200.0));
        ctx.gstate
            .path
            .segments
            .push(PathSegment::LineTo(100.0, 200.0));
        ctx.gstate.path.segments.push(PathSegment::ClosePath);
        ctx.o_stack.push(PsObject::real(50.0)).unwrap();
        ctx.o_stack.push(PsObject::real(50.0)).unwrap();
        op_infill(&mut ctx).unwrap();
        assert!(matches!(
            ctx.o_stack.pop().unwrap().value,
            PsValue::Bool(false)
        ));
    }

    #[test]
    fn test_infill_empty_path() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(150.0)).unwrap();
        ctx.o_stack.push(PsObject::real(150.0)).unwrap();
        op_infill(&mut ctx).unwrap();
        assert!(matches!(
            ctx.o_stack.pop().unwrap().value,
            PsValue::Bool(false)
        ));
    }

    #[test]
    fn test_ineofill_concentric_rects() {
        let mut ctx = setup();
        // Outer rect CW
        ctx.gstate.path.segments.push(PathSegment::MoveTo(0.0, 0.0));
        ctx.gstate
            .path
            .segments
            .push(PathSegment::LineTo(300.0, 0.0));
        ctx.gstate
            .path
            .segments
            .push(PathSegment::LineTo(300.0, 300.0));
        ctx.gstate
            .path
            .segments
            .push(PathSegment::LineTo(0.0, 300.0));
        ctx.gstate.path.segments.push(PathSegment::ClosePath);
        // Inner rect also CW
        ctx.gstate
            .path
            .segments
            .push(PathSegment::MoveTo(50.0, 50.0));
        ctx.gstate
            .path
            .segments
            .push(PathSegment::LineTo(250.0, 50.0));
        ctx.gstate
            .path
            .segments
            .push(PathSegment::LineTo(250.0, 250.0));
        ctx.gstate
            .path
            .segments
            .push(PathSegment::LineTo(50.0, 250.0));
        ctx.gstate.path.segments.push(PathSegment::ClosePath);

        // infill (winding): center has winding=2, inside
        ctx.o_stack.push(PsObject::real(150.0)).unwrap();
        ctx.o_stack.push(PsObject::real(150.0)).unwrap();
        op_infill(&mut ctx).unwrap();
        assert!(matches!(
            ctx.o_stack.pop().unwrap().value,
            PsValue::Bool(true)
        ));

        // ineofill (even-odd): 2 crossings = even, outside
        ctx.o_stack.push(PsObject::real(150.0)).unwrap();
        ctx.o_stack.push(PsObject::real(150.0)).unwrap();
        op_ineofill(&mut ctx).unwrap();
        assert!(matches!(
            ctx.o_stack.pop().unwrap().value,
            PsValue::Bool(false)
        ));
    }

    #[test]
    fn test_instroke_on_line() {
        let mut ctx = setup();
        ctx.gstate
            .path
            .segments
            .push(PathSegment::MoveTo(100.0, 100.0));
        ctx.gstate
            .path
            .segments
            .push(PathSegment::LineTo(200.0, 100.0));
        ctx.gstate.line_width = 10.0;
        ctx.o_stack.push(PsObject::real(150.0)).unwrap();
        ctx.o_stack.push(PsObject::real(100.0)).unwrap();
        op_instroke(&mut ctx).unwrap();
        assert!(matches!(
            ctx.o_stack.pop().unwrap().value,
            PsValue::Bool(true)
        ));
    }

    #[test]
    fn test_instroke_away() {
        let mut ctx = setup();
        ctx.gstate
            .path
            .segments
            .push(PathSegment::MoveTo(100.0, 100.0));
        ctx.gstate
            .path
            .segments
            .push(PathSegment::LineTo(200.0, 100.0));
        ctx.gstate.line_width = 1.0;
        ctx.o_stack.push(PsObject::real(150.0)).unwrap();
        ctx.o_stack.push(PsObject::real(200.0)).unwrap();
        op_instroke(&mut ctx).unwrap();
        assert!(matches!(
            ctx.o_stack.pop().unwrap().value,
            PsValue::Bool(false)
        ));
    }
}
