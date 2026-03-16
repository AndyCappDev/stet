// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Path construction operators: newpath, currentpoint, moveto, rmoveto, lineto,
//! rlineto, curveto, rcurveto, closepath, arc, arcn, arcto, arct.

use std::f64::consts::PI;

use stet_core::context::Context;
use stet_core::error::PsError;
use stet_fonts::geometry::{Matrix, PathSegment, PsPath};
use stet_core::object::PsObject;

/// `newpath`: — → — (clear current path)
pub fn op_newpath(ctx: &mut Context) -> Result<(), PsError> {
    ctx.gstate.path.clear();
    ctx.gstate.current_point = None;
    ctx.gstate.bbox = None;
    Ok(())
}

/// `currentpoint`: — → x y
///
/// current_point is stored in device space; inverse-transform via iCTM to return user-space.
pub fn op_currentpoint(ctx: &mut Context) -> Result<(), PsError> {
    let (dx, dy) = ctx.gstate.current_point.ok_or(PsError::NoCurrentPoint)?;
    let ictm = ctx.gstate.ctm.invert().ok_or(PsError::UndefinedResult)?;
    let (ux, uy) = ictm.transform_point(dx, dy);
    ctx.o_stack.push(PsObject::real(ux))?;
    ctx.o_stack.push(PsObject::real(uy))?;
    Ok(())
}

/// `moveto`: x y → —
///
/// Transform user-space operands through CTM → store device-space coordinates.
pub fn op_moveto(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let y_obj = ctx.o_stack.peek(0)?;
    let x_obj = ctx.o_stack.peek(1)?;
    let x = x_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let y = y_obj.as_f64().ok_or(PsError::TypeCheck)?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    let (dx, dy) = ctx.gstate.ctm.transform_point(x, y);
    // Per PLRM: consecutive movetos replace the previous one
    path_moveto(&mut ctx.gstate.path, dx, dy);
    ctx.gstate.current_point = Some((dx, dy));
    Ok(())
}

/// `rmoveto`: dx dy → —
///
/// current_point is device-space; deltas are user-space, transformed via `transform_delta`.
pub fn op_rmoveto(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let dy_obj = ctx.o_stack.peek(0)?;
    let dx_obj = ctx.o_stack.peek(1)?;
    let dx = dx_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let dy = dy_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let (cx, cy) = ctx.gstate.current_point.ok_or(PsError::NoCurrentPoint)?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    let (ddx, ddy) = ctx.gstate.ctm.transform_delta(dx, dy);
    let nx = cx + ddx;
    let ny = cy + ddy;
    // Per PLRM: consecutive movetos replace the previous one
    path_moveto(&mut ctx.gstate.path, nx, ny);
    ctx.gstate.current_point = Some((nx, ny));
    Ok(())
}

/// Add a MoveTo to the path, replacing the previous MoveTo if it was the last segment.
///
/// Per PLRM: "If the previous path operation in the current path was moveto or rmoveto,
/// that point is deleted from the current path and the new moveto point replaces it."
pub fn path_moveto(path: &mut PsPath, x: f64, y: f64) {
    if let Some(PathSegment::MoveTo(px, py)) = path.segments.last_mut() {
        *px = x;
        *py = y;
    } else {
        path.segments.push(PathSegment::MoveTo(x, y));
    }
}

/// `lineto`: x y → —
///
/// Transform user-space operands through CTM → store device-space coordinates.
pub fn op_lineto(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let y_obj = ctx.o_stack.peek(0)?;
    let x_obj = ctx.o_stack.peek(1)?;
    let x = x_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let y = y_obj.as_f64().ok_or(PsError::TypeCheck)?;
    if ctx.gstate.current_point.is_none() {
        return Err(PsError::NoCurrentPoint);
    }
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    let (dx, dy) = ctx.gstate.ctm.transform_point(x, y);
    ctx.gstate.path.segments.push(PathSegment::LineTo(dx, dy));
    ctx.gstate.current_point = Some((dx, dy));
    Ok(())
}

/// `rlineto`: dx dy → —
///
/// current_point is device-space; deltas are user-space, transformed via `transform_delta`.
pub fn op_rlineto(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let dy_obj = ctx.o_stack.peek(0)?;
    let dx_obj = ctx.o_stack.peek(1)?;
    let dx = dx_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let dy = dy_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let (cx, cy) = ctx.gstate.current_point.ok_or(PsError::NoCurrentPoint)?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    let (ddx, ddy) = ctx.gstate.ctm.transform_delta(dx, dy);
    let nx = cx + ddx;
    let ny = cy + ddy;
    ctx.gstate.path.segments.push(PathSegment::LineTo(nx, ny));
    ctx.gstate.current_point = Some((nx, ny));
    Ok(())
}

/// `curveto`: x1 y1 x2 y2 x3 y3 → —
pub fn op_curveto(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 6 {
        return Err(PsError::StackUnderflow);
    }
    let y3_obj = ctx.o_stack.peek(0)?;
    let x3_obj = ctx.o_stack.peek(1)?;
    let y2_obj = ctx.o_stack.peek(2)?;
    let x2_obj = ctx.o_stack.peek(3)?;
    let y1_obj = ctx.o_stack.peek(4)?;
    let x1_obj = ctx.o_stack.peek(5)?;
    let x1 = x1_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let y1 = y1_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let x2 = x2_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let y2 = y2_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let x3 = x3_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let y3 = y3_obj.as_f64().ok_or(PsError::TypeCheck)?;
    if ctx.gstate.current_point.is_none() {
        return Err(PsError::NoCurrentPoint);
    }
    for _ in 0..6 {
        ctx.o_stack.pop()?;
    }
    let (dx1, dy1) = ctx.gstate.ctm.transform_point(x1, y1);
    let (dx2, dy2) = ctx.gstate.ctm.transform_point(x2, y2);
    let (dx3, dy3) = ctx.gstate.ctm.transform_point(x3, y3);
    ctx.gstate.path.segments.push(PathSegment::CurveTo {
        x1: dx1,
        y1: dy1,
        x2: dx2,
        y2: dy2,
        x3: dx3,
        y3: dy3,
    });
    ctx.gstate.current_point = Some((dx3, dy3));
    Ok(())
}

/// `rcurveto`: dx1 dy1 dx2 dy2 dx3 dy3 → —
pub fn op_rcurveto(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 6 {
        return Err(PsError::StackUnderflow);
    }
    let dy3_obj = ctx.o_stack.peek(0)?;
    let dx3_obj = ctx.o_stack.peek(1)?;
    let dy2_obj = ctx.o_stack.peek(2)?;
    let dx2_obj = ctx.o_stack.peek(3)?;
    let dy1_obj = ctx.o_stack.peek(4)?;
    let dx1_obj = ctx.o_stack.peek(5)?;
    let dx1 = dx1_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let dy1 = dy1_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let dx2 = dx2_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let dy2 = dy2_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let dx3 = dx3_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let dy3 = dy3_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let (cx, cy) = ctx.gstate.current_point.ok_or(PsError::NoCurrentPoint)?;
    for _ in 0..6 {
        ctx.o_stack.pop()?;
    }
    // current_point is device-space; deltas are user-space, transformed via transform_delta
    let (ddx1, ddy1) = ctx.gstate.ctm.transform_delta(dx1, dy1);
    let (ddx2, ddy2) = ctx.gstate.ctm.transform_delta(dx2, dy2);
    let (ddx3, ddy3) = ctx.gstate.ctm.transform_delta(dx3, dy3);
    ctx.gstate.path.segments.push(PathSegment::CurveTo {
        x1: cx + ddx1,
        y1: cy + ddy1,
        x2: cx + ddx2,
        y2: cy + ddy2,
        x3: cx + ddx3,
        y3: cy + ddy3,
    });
    ctx.gstate.current_point = Some((cx + ddx3, cy + ddy3));
    Ok(())
}

/// `closepath`: — → —
pub fn op_closepath(ctx: &mut Context) -> Result<(), PsError> {
    ctx.gstate.path.segments.push(PathSegment::ClosePath);
    // Set current point to subpath start
    if let Some(start) = find_subpath_start(&ctx.gstate.path.segments) {
        ctx.gstate.current_point = Some(start);
    }
    Ok(())
}

/// Find the start point of the current subpath (last MoveTo before end).
fn find_subpath_start(segments: &[PathSegment]) -> Option<(f64, f64)> {
    for seg in segments.iter().rev() {
        if let PathSegment::MoveTo(x, y) = seg {
            return Some((*x, *y));
        }
    }
    None
}

/// Convert a circular arc to cubic bezier segments (≤90° each).
/// Ported from PostForge's `_acuteArcToBezier`.
fn acute_arc_to_bezier(start: f64, size: f64) -> (f64, f64, f64, f64, f64, f64, f64, f64) {
    let alpha = size / 2.0;
    let cos_alpha = alpha.cos();
    let sin_alpha = alpha.sin();
    let cot_alpha = 1.0 / alpha.tan();
    let phi = start + alpha;
    let cos_phi = phi.cos();
    let sin_phi = phi.sin();

    let lmbda = (4.0 - cos_alpha) / 3.0;
    let mu = sin_alpha + (cos_alpha - lmbda) * cot_alpha;

    (
        start.cos(),
        start.sin(),
        lmbda * cos_phi + mu * sin_phi,
        lmbda * sin_phi - mu * cos_phi,
        lmbda * cos_phi - mu * sin_phi,
        lmbda * sin_phi + mu * cos_phi,
        (start + size).cos(),
        (start + size).sin(),
    )
}

/// Generate arc path segments from center, radius, and angles (in degrees).
///
/// All coordinates are computed in user space (center+radius), then transformed
/// through the CTM to device space before storing in the path.
///
/// Matches PostForge's algorithm:
/// - `arc` (counterclockwise=false): greedy segments of up to PI/2
/// - `arcn` (counterclockwise=true): generate forward arc, then reverse
///   segment order and swap control points (PostForge's battle-tested approach)
#[allow(clippy::too_many_arguments)]
fn arc_segments(
    cx: f64,
    cy: f64,
    r: f64,
    angle1_deg: f64,
    angle2_deg: f64,
    counterclockwise: bool,
    segments: &mut Vec<PathSegment>,
    has_current_point: bool,
    ctm: &Matrix,
) -> (f64, f64) {
    if counterclockwise {
        arc_segments_arcn(
            cx,
            cy,
            r,
            angle1_deg,
            angle2_deg,
            segments,
            has_current_point,
            ctm,
        )
    } else {
        arc_segments_arc(
            cx,
            cy,
            r,
            angle1_deg,
            angle2_deg,
            segments,
            has_current_point,
            ctm,
        )
    }
}

/// Generate CCW arc segments (the `arc` operator).
/// Uses greedy segment sizing: each segment is min(remaining, PI/2).
/// Points are computed in user space, then transformed through CTM to device space.
#[allow(clippy::too_many_arguments)]
fn arc_segments_arc(
    cx: f64,
    cy: f64,
    r: f64,
    angle1_deg: f64,
    angle2_deg: f64,
    segments: &mut Vec<PathSegment>,
    has_current_point: bool,
    ctm: &Matrix,
) -> (f64, f64) {
    let start = angle1_deg;
    let mut stop = angle2_deg;
    while stop < start {
        stop += 360.0;
    }
    let start_rad = start.to_radians();
    let stop_rad = stop.to_radians();

    let half_pi = PI / 2.0;
    let epsilon = 1e-5;

    let mut current = start_rad;
    let mut first = true;

    while stop_rad - current > epsilon {
        let arc_to_draw = (stop_rad - current).min(half_pi);
        let (p0x, p0y, p1x, p1y, p2x, p2y, p3x, p3y) = acute_arc_to_bezier(current, arc_to_draw);

        if first {
            let (dx0, dy0) = ctm.transform_point(cx + r * p0x, cy + r * p0y);
            if !has_current_point {
                segments.push(PathSegment::MoveTo(dx0, dy0));
            } else {
                segments.push(PathSegment::LineTo(dx0, dy0));
            }
            first = false;
        }

        let (dx1, dy1) = ctm.transform_point(cx + r * p1x, cy + r * p1y);
        let (dx2, dy2) = ctm.transform_point(cx + r * p2x, cy + r * p2y);
        let (dx3, dy3) = ctm.transform_point(cx + r * p3x, cy + r * p3y);
        segments.push(PathSegment::CurveTo {
            x1: dx1,
            y1: dy1,
            x2: dx2,
            y2: dy2,
            x3: dx3,
            y3: dy3,
        });

        current += arc_to_draw;
    }

    if first {
        // Degenerate: angle1 == angle2 (or very close)
        let (dx, dy) = ctm.transform_point(cx + r * start_rad.cos(), cy + r * start_rad.sin());
        if !has_current_point {
            segments.push(PathSegment::MoveTo(dx, dy));
        } else {
            segments.push(PathSegment::LineTo(dx, dy));
        }
        return (dx, dy);
    }

    // Endpoint from the last bezier segment — transform to device space
    let (end_dx, end_dy) = ctm.transform_point(cx + r * stop_rad.cos(), cy + r * stop_rad.sin());
    (end_dx, end_dy)
}

/// Generate CW arc segments (the `arcn` operator).
/// PostForge's approach: swap angles, generate forward (CCW) arc,
/// then reverse segment order and swap control points within each segment.
/// Points are computed in user space, then transformed through CTM to device space.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn arc_segments_arcn(
    cx: f64,
    cy: f64,
    r: f64,
    angle1_deg: f64,
    angle2_deg: f64,
    segments: &mut Vec<PathSegment>,
    has_current_point: bool,
    ctm: &Matrix,
) -> (f64, f64) {
    // PostForge swaps angle1 and angle2 for arcn
    let start = angle2_deg;
    let mut stop = angle1_deg;
    while stop < start {
        stop += 360.0;
    }
    let start_rad = start.to_radians();
    let stop_rad = stop.to_radians();

    let half_pi = PI / 2.0;
    let epsilon = 1e-5;

    // Generate forward (CCW) bezier curves, collecting them for reversal
    // Each curve stores (p0x, p0y, p1x, p1y, p2x, p2y, p3x, p3y) on unit circle
    let mut curves: Vec<(f64, f64, f64, f64, f64, f64, f64, f64)> = Vec::new();
    let mut current = start_rad;

    while stop_rad - current > epsilon {
        let arc_to_draw = (stop_rad - current).min(half_pi);
        let (p0x, p0y, p1x, p1y, p2x, p2y, p3x, p3y) = acute_arc_to_bezier(current, arc_to_draw);
        // Reverse control point order: p3,p2,p1,p0 (matching PostForge's curves.insert(0, ...))
        curves.insert(0, (p3x, p3y, p2x, p2y, p1x, p1y, p0x, p0y));
        current += arc_to_draw;
    }

    if curves.is_empty() {
        // Degenerate: angle1 == angle2
        let (dx, dy) = ctm.transform_point(
            cx + r * angle1_deg.to_radians().cos(),
            cy + r * angle1_deg.to_radians().sin(),
        );
        if !has_current_point {
            segments.push(PathSegment::MoveTo(dx, dy));
        } else {
            segments.push(PathSegment::LineTo(dx, dy));
        }
        return (dx, dy);
    }

    // Now emit the reversed curves, transforming through CTM
    let mut first = true;
    for (p0x, p0y, p1x, p1y, p2x, p2y, p3x, p3y) in &curves {
        if first {
            let (dx0, dy0) = ctm.transform_point(cx + r * p0x, cy + r * p0y);
            if !has_current_point {
                segments.push(PathSegment::MoveTo(dx0, dy0));
            } else {
                segments.push(PathSegment::LineTo(dx0, dy0));
            }
            first = false;
        }

        let (dx1, dy1) = ctm.transform_point(cx + r * p1x, cy + r * p1y);
        let (dx2, dy2) = ctm.transform_point(cx + r * p2x, cy + r * p2y);
        let (dx3, dy3) = ctm.transform_point(cx + r * p3x, cy + r * p3y);
        segments.push(PathSegment::CurveTo {
            x1: dx1,
            y1: dy1,
            x2: dx2,
            y2: dy2,
            x3: dx3,
            y3: dy3,
        });
    }

    // Last curve's p3 is the endpoint — transform to device space
    let last = curves.last().unwrap();
    let (end_dx, end_dy) = ctm.transform_point(cx + r * last.6, cy + r * last.7);
    (end_dx, end_dy)
}

/// `arc`: cx cy r angle1 angle2 → —
pub fn op_arc(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 5 {
        return Err(PsError::StackUnderflow);
    }
    let a2_obj = ctx.o_stack.peek(0)?;
    let a1_obj = ctx.o_stack.peek(1)?;
    let r_obj = ctx.o_stack.peek(2)?;
    let cy_obj = ctx.o_stack.peek(3)?;
    let cx_obj = ctx.o_stack.peek(4)?;
    let cx = cx_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let cy = cy_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let r = r_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let a1 = a1_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let a2 = a2_obj.as_f64().ok_or(PsError::TypeCheck)?;
    for _ in 0..5 {
        ctx.o_stack.pop()?;
    }
    let has_cp = ctx.gstate.current_point.is_some();
    let ctm = ctx.gstate.ctm;
    let (ex, ey) = arc_segments(
        cx,
        cy,
        r,
        a1,
        a2,
        false,
        &mut ctx.gstate.path.segments,
        has_cp,
        &ctm,
    );
    ctx.gstate.current_point = Some((ex, ey));
    Ok(())
}

/// `arcn`: cx cy r angle1 angle2 → — (clockwise arc)
pub fn op_arcn(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 5 {
        return Err(PsError::StackUnderflow);
    }
    let a2_obj = ctx.o_stack.peek(0)?;
    let a1_obj = ctx.o_stack.peek(1)?;
    let r_obj = ctx.o_stack.peek(2)?;
    let cy_obj = ctx.o_stack.peek(3)?;
    let cx_obj = ctx.o_stack.peek(4)?;
    let cx = cx_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let cy = cy_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let r = r_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let a1 = a1_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let a2 = a2_obj.as_f64().ok_or(PsError::TypeCheck)?;
    for _ in 0..5 {
        ctx.o_stack.pop()?;
    }
    let has_cp = ctx.gstate.current_point.is_some();
    let ctm = ctx.gstate.ctm;
    let (ex, ey) = arc_segments(
        cx,
        cy,
        r,
        a1,
        a2,
        true,
        &mut ctx.gstate.path.segments,
        has_cp,
        &ctm,
    );
    ctx.gstate.current_point = Some((ex, ey));
    Ok(())
}

/// `arcto`: x1 y1 x2 y2 r → xt1 yt1 xt2 yt2
///
/// Algorithm ported from PostForge's `_compute_arc_from_tangents` + `arcto`.
/// Tangent computation is in user space (for return values); path storage is device space.
pub fn op_arcto(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 5 {
        return Err(PsError::StackUnderflow);
    }
    let r_obj = ctx.o_stack.peek(0)?;
    let y2_obj = ctx.o_stack.peek(1)?;
    let x2_obj = ctx.o_stack.peek(2)?;
    let y1_obj = ctx.o_stack.peek(3)?;
    let x1_obj = ctx.o_stack.peek(4)?;
    let x1 = x1_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let y1 = y1_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let x2 = x2_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let y2 = y2_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let r = r_obj.as_f64().ok_or(PsError::TypeCheck)?.abs();
    // current_point is device-space; inverse-transform to get user-space x0,y0
    let (dev_x0, dev_y0) = ctx.gstate.current_point.ok_or(PsError::NoCurrentPoint)?;
    let ictm = ctx.gstate.ctm.invert().ok_or(PsError::UndefinedResult)?;
    let (x0, y0) = ictm.transform_point(dev_x0, dev_y0);
    for _ in 0..5 {
        ctx.o_stack.pop()?;
    }

    let ctm = ctx.gstate.ctm;

    // Vectors from vertex (x1,y1) toward the other points (user space)
    let u1x = x0 - x1;
    let u1y = y0 - y1;
    let u2x = x2 - x1;
    let u2y = y2 - y1;

    let u1_len = (u1x * u1x + u1y * u1y).sqrt();
    let u2_len = (u2x * u2x + u2y * u2y).sqrt();

    if u1_len < 1e-10 || u2_len < 1e-10 {
        // Degenerate: just lineto x1,y1 (device space)
        let (dx1, dy1) = ctm.transform_point(x1, y1);
        ctx.gstate.path.segments.push(PathSegment::LineTo(dx1, dy1));
        ctx.gstate.current_point = Some((dx1, dy1));
        ctx.o_stack.push(PsObject::real(x1))?;
        ctx.o_stack.push(PsObject::real(y1))?;
        ctx.o_stack.push(PsObject::real(x1))?;
        ctx.o_stack.push(PsObject::real(y1))?;
        return Ok(());
    }

    // Normalize
    let u1x = u1x / u1_len;
    let u1y = u1y / u1_len;
    let u2x = u2x / u2_len;
    let u2y = u2y / u2_len;

    // Cross product for collinearity and direction
    let cross = u1x * u2y - u1y * u2x;
    if cross.abs() < 1e-8 {
        // Collinear
        let (dx1, dy1) = ctm.transform_point(x1, y1);
        ctx.gstate.path.segments.push(PathSegment::LineTo(dx1, dy1));
        ctx.gstate.current_point = Some((dx1, dy1));
        ctx.o_stack.push(PsObject::real(x1))?;
        ctx.o_stack.push(PsObject::real(y1))?;
        ctx.o_stack.push(PsObject::real(x1))?;
        ctx.o_stack.push(PsObject::real(y1))?;
        return Ok(());
    }

    // Half angle between the two lines (PostForge's acos approach)
    let dot = (u1x * u2x + u1y * u2y).clamp(-1.0, 1.0);
    let half_angle = dot.acos() / 2.0;

    if half_angle.sin().abs() < 1e-10 {
        // Nearly collinear
        let (dx1, dy1) = ctm.transform_point(x1, y1);
        ctx.gstate.path.segments.push(PathSegment::LineTo(dx1, dy1));
        ctx.gstate.current_point = Some((dx1, dy1));
        ctx.o_stack.push(PsObject::real(x1))?;
        ctx.o_stack.push(PsObject::real(y1))?;
        ctx.o_stack.push(PsObject::real(x1))?;
        ctx.o_stack.push(PsObject::real(y1))?;
        return Ok(());
    }

    // Distance from vertex to center along the angle bisector
    let dist_to_center = r / half_angle.sin();
    // Distance from vertex to tangent points along each line
    let dist_to_tangent = r / half_angle.tan();

    // Angle bisector direction (u1 + u2, normalized)
    let bx = u1x + u2x;
    let by = u1y + u2y;
    let blen = (bx * bx + by * by).sqrt();

    if blen < 1e-10 {
        // Collinear (antiparallel)
        let (dx1, dy1) = ctm.transform_point(x1, y1);
        ctx.gstate.path.segments.push(PathSegment::LineTo(dx1, dy1));
        ctx.gstate.current_point = Some((dx1, dy1));
        ctx.o_stack.push(PsObject::real(x1))?;
        ctx.o_stack.push(PsObject::real(y1))?;
        ctx.o_stack.push(PsObject::real(x1))?;
        ctx.o_stack.push(PsObject::real(y1))?;
        return Ok(());
    }
    let bx = bx / blen;
    let by = by / blen;

    // Center of the arc (user space)
    let arc_cx = x1 + bx * dist_to_center;
    let arc_cy = y1 + by * dist_to_center;

    // Tangent points (user space — returned on stack)
    let xt1 = x1 + u1x * dist_to_tangent;
    let yt1 = y1 + u1y * dist_to_tangent;
    let xt2 = x1 + u2x * dist_to_tangent;
    let yt2 = y1 + u2y * dist_to_tangent;

    // Arc angles from center to tangent points
    let start_angle = (yt1 - arc_cy).atan2(xt1 - arc_cx).to_degrees();
    let end_angle = (yt2 - arc_cy).atan2(xt2 - arc_cx).to_degrees();

    // Direction: cross > 0 in user space means left turn → CW arc
    let clockwise = cross > 0.0;

    // Line to first tangent point in device space (if not already there)
    let (dev_xt1, dev_yt1) = ctm.transform_point(xt1, yt1);
    if (dev_x0 - dev_xt1).abs() > 1e-10 || (dev_y0 - dev_yt1).abs() > 1e-10 {
        ctx.gstate
            .path
            .segments
            .push(PathSegment::LineTo(dev_xt1, dev_yt1));
    }

    // Generate arc bezier segments with endpoint snapping (device space)
    let (dev_xt2, dev_yt2) = ctm.transform_point(xt2, yt2);
    arcto_bezier_segments(
        arc_cx,
        arc_cy,
        r,
        start_angle,
        end_angle,
        clockwise,
        dev_xt2,
        dev_yt2,
        &mut ctx.gstate.path.segments,
        &ctm,
    );

    // Set currentpoint to second tangent point (device space)
    ctx.gstate.current_point = Some((dev_xt2, dev_yt2));

    // Push user-space tangent points on stack
    ctx.o_stack.push(PsObject::real(xt1))?;
    ctx.o_stack.push(PsObject::real(yt1))?;
    ctx.o_stack.push(PsObject::real(xt2))?;
    ctx.o_stack.push(PsObject::real(yt2))?;
    Ok(())
}

/// Generate arc bezier segments for arcto/arct with endpoint snapping.
/// The last segment's endpoint is snapped to (snap_dx, snap_dy) in device space.
/// Control points are computed in user space (center+radius), then transformed through CTM.
#[allow(clippy::too_many_arguments)]
fn arcto_bezier_segments(
    cx: f64,
    cy: f64,
    r: f64,
    start_angle_deg: f64,
    end_angle_deg: f64,
    clockwise: bool,
    snap_dx: f64,
    snap_dy: f64,
    segments: &mut Vec<PathSegment>,
    ctm: &Matrix,
) {
    let start_rad = start_angle_deg.to_radians();
    let mut end_rad = end_angle_deg.to_radians();

    if clockwise {
        while end_rad >= start_rad {
            end_rad -= 2.0 * PI;
        }
    } else {
        while end_rad <= start_rad {
            end_rad += 2.0 * PI;
        }
    }

    let half_pi = PI / 2.0;
    let epsilon = 1e-5;
    let mut current = start_rad;

    if clockwise {
        while current > end_rad + epsilon {
            let arc_to_draw = (end_rad - current).max(-half_pi);
            let is_last = (current + arc_to_draw) <= end_rad + epsilon;
            let (_p0x, _p0y, p1x, p1y, p2x, p2y, p3x, p3y) =
                acute_arc_to_bezier(current, arc_to_draw);

            let (dx1, dy1) = ctm.transform_point(cx + r * p1x, cy + r * p1y);
            let (dx2, dy2) = ctm.transform_point(cx + r * p2x, cy + r * p2y);
            let (ex, ey) = if is_last {
                (snap_dx, snap_dy)
            } else {
                ctm.transform_point(cx + r * p3x, cy + r * p3y)
            };

            segments.push(PathSegment::CurveTo {
                x1: dx1,
                y1: dy1,
                x2: dx2,
                y2: dy2,
                x3: ex,
                y3: ey,
            });
            current += arc_to_draw;
        }
    } else {
        while current < end_rad - epsilon {
            let arc_to_draw = (end_rad - current).min(half_pi);
            let is_last = (current + arc_to_draw) >= end_rad - epsilon;
            let (_p0x, _p0y, p1x, p1y, p2x, p2y, p3x, p3y) =
                acute_arc_to_bezier(current, arc_to_draw);

            let (dx1, dy1) = ctm.transform_point(cx + r * p1x, cy + r * p1y);
            let (dx2, dy2) = ctm.transform_point(cx + r * p2x, cy + r * p2y);
            let (ex, ey) = if is_last {
                (snap_dx, snap_dy)
            } else {
                ctm.transform_point(cx + r * p3x, cy + r * p3y)
            };

            segments.push(PathSegment::CurveTo {
                x1: dx1,
                y1: dy1,
                x2: dx2,
                y2: dy2,
                x3: ex,
                y3: ey,
            });
            current += arc_to_draw;
        }
    }
}

/// `arct`: x1 y1 x2 y2 r → — (like arcto but no tangent points returned)
pub fn op_arct(ctx: &mut Context) -> Result<(), PsError> {
    // arct is the same as arcto but pops the 4 tangent results
    op_arcto(ctx)?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use stet_core::context::Context;

    fn setup() -> Context {
        let mut ctx = Context::new();
        crate::build_system_dict(&mut ctx);
        ctx
    }

    #[test]
    fn test_newpath() {
        let mut ctx = setup();
        ctx.gstate.current_point = Some((1.0, 2.0));
        ctx.gstate.path.segments.push(PathSegment::MoveTo(1.0, 2.0));
        op_newpath(&mut ctx).unwrap();
        assert!(ctx.gstate.path.is_empty());
        assert!(ctx.gstate.current_point.is_none());
    }

    #[test]
    fn test_moveto_currentpoint() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(10.0)).unwrap();
        ctx.o_stack.push(PsObject::real(20.0)).unwrap();
        op_moveto(&mut ctx).unwrap();
        assert_eq!(ctx.gstate.current_point, Some((10.0, 20.0)));
        op_currentpoint(&mut ctx).unwrap();
        let y = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        let x = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        assert!((x - 10.0).abs() < 1e-10);
        assert!((y - 20.0).abs() < 1e-10);
    }

    #[test]
    fn test_lineto() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(0.0)).unwrap();
        ctx.o_stack.push(PsObject::real(0.0)).unwrap();
        op_moveto(&mut ctx).unwrap();
        ctx.o_stack.push(PsObject::real(100.0)).unwrap();
        ctx.o_stack.push(PsObject::real(200.0)).unwrap();
        op_lineto(&mut ctx).unwrap();
        assert_eq!(ctx.gstate.current_point, Some((100.0, 200.0)));
        assert_eq!(ctx.gstate.path.segments.len(), 2);
    }

    #[test]
    fn test_lineto_no_currentpoint() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(100.0)).unwrap();
        ctx.o_stack.push(PsObject::real(200.0)).unwrap();
        assert_eq!(op_lineto(&mut ctx), Err(PsError::NoCurrentPoint));
    }

    #[test]
    fn test_rmoveto() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(10.0)).unwrap();
        ctx.o_stack.push(PsObject::real(20.0)).unwrap();
        op_moveto(&mut ctx).unwrap();
        ctx.o_stack.push(PsObject::real(5.0)).unwrap();
        ctx.o_stack.push(PsObject::real(3.0)).unwrap();
        op_rmoveto(&mut ctx).unwrap();
        assert_eq!(ctx.gstate.current_point, Some((15.0, 23.0)));
    }

    #[test]
    fn test_rlineto() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(10.0)).unwrap();
        ctx.o_stack.push(PsObject::real(20.0)).unwrap();
        op_moveto(&mut ctx).unwrap();
        ctx.o_stack.push(PsObject::real(100.0)).unwrap();
        ctx.o_stack.push(PsObject::real(200.0)).unwrap();
        op_rlineto(&mut ctx).unwrap();
        assert_eq!(ctx.gstate.current_point, Some((110.0, 220.0)));
    }

    #[test]
    fn test_curveto() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(0.0)).unwrap();
        ctx.o_stack.push(PsObject::real(0.0)).unwrap();
        op_moveto(&mut ctx).unwrap();
        for &v in &[10.0, 20.0, 30.0, 40.0, 50.0, 60.0] {
            ctx.o_stack.push(PsObject::real(v)).unwrap();
        }
        op_curveto(&mut ctx).unwrap();
        assert_eq!(ctx.gstate.current_point, Some((50.0, 60.0)));
    }

    #[test]
    fn test_closepath() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(10.0)).unwrap();
        ctx.o_stack.push(PsObject::real(20.0)).unwrap();
        op_moveto(&mut ctx).unwrap();
        ctx.o_stack.push(PsObject::real(100.0)).unwrap();
        ctx.o_stack.push(PsObject::real(200.0)).unwrap();
        op_lineto(&mut ctx).unwrap();
        op_closepath(&mut ctx).unwrap();
        assert_eq!(ctx.gstate.current_point, Some((10.0, 20.0)));
    }

    #[test]
    fn test_arc() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(100.0)).unwrap(); // cx
        ctx.o_stack.push(PsObject::real(100.0)).unwrap(); // cy
        ctx.o_stack.push(PsObject::real(50.0)).unwrap(); // r
        ctx.o_stack.push(PsObject::real(0.0)).unwrap(); // a1
        ctx.o_stack.push(PsObject::real(90.0)).unwrap(); // a2
        op_arc(&mut ctx).unwrap();
        let (x, y) = ctx.gstate.current_point.unwrap();
        // End should be at (100, 150) approximately
        assert!((x - 100.0).abs() < 0.5);
        assert!((y - 150.0).abs() < 0.5);
    }

    #[test]
    fn test_arcn() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(100.0)).unwrap();
        ctx.o_stack.push(PsObject::real(100.0)).unwrap();
        ctx.o_stack.push(PsObject::real(50.0)).unwrap();
        ctx.o_stack.push(PsObject::real(90.0)).unwrap(); // start
        ctx.o_stack.push(PsObject::real(0.0)).unwrap(); // end
        op_arcn(&mut ctx).unwrap();
        let (x, y) = ctx.gstate.current_point.unwrap();
        // End should be at (150, 100) approximately
        assert!((x - 150.0).abs() < 0.5);
        assert!((y - 100.0).abs() < 0.5);
    }

    #[test]
    fn test_arc_full_circle() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(0.0)).unwrap();
        ctx.o_stack.push(PsObject::real(0.0)).unwrap();
        ctx.o_stack.push(PsObject::real(100.0)).unwrap();
        ctx.o_stack.push(PsObject::real(0.0)).unwrap();
        ctx.o_stack.push(PsObject::real(360.0)).unwrap();
        op_arc(&mut ctx).unwrap();
        // Should have segments for full circle (4 bezier segments)
        assert!(ctx.gstate.path.segments.len() >= 5); // moveto + 4 curves
    }

    #[test]
    fn test_rcurveto() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(10.0)).unwrap();
        ctx.o_stack.push(PsObject::real(20.0)).unwrap();
        op_moveto(&mut ctx).unwrap();
        for &v in &[5.0, 0.0, 10.0, 5.0, 15.0, 10.0] {
            ctx.o_stack.push(PsObject::real(v)).unwrap();
        }
        op_rcurveto(&mut ctx).unwrap();
        let (x, y) = ctx.gstate.current_point.unwrap();
        assert!((x - 25.0).abs() < 1e-10);
        assert!((y - 30.0).abs() < 1e-10);
    }

    #[test]
    fn test_currentpoint_no_point() {
        let mut ctx = setup();
        assert_eq!(op_currentpoint(&mut ctx), Err(PsError::NoCurrentPoint));
    }

    #[test]
    fn test_arc_negative_radius() {
        // Negative radius is treated as abs(r), matching PostForge
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(0.0)).unwrap();
        ctx.o_stack.push(PsObject::real(0.0)).unwrap();
        ctx.o_stack.push(PsObject::real(-50.0)).unwrap();
        ctx.o_stack.push(PsObject::real(0.0)).unwrap();
        ctx.o_stack.push(PsObject::real(90.0)).unwrap();
        assert!(op_arc(&mut ctx).is_ok());
    }
}
