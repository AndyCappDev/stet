// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Path query operators: pathbbox, flattenpath, reversepath, strokepath, pathforall.

use stet_core::context::Context;
use stet_core::error::PsError;
use stet_core::graphics_state::{PathSegment, PsPath};
use stet_core::object::{EntityId, PsObject};

/// `pathbbox`: — → llx lly urx ury (bounding box of current path in user space)
///
/// Path is in device space. Compute bbox in device space, transform all 4 corners
/// of the device-space bbox through iCTM, then take min/max for user-space bbox.
pub fn op_pathbbox(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.gstate.path.is_empty() {
        return Err(PsError::NoCurrentPoint);
    }

    // If setbbox was called, return the stored user-space bbox
    if let Some(bbox) = ctx.gstate.bbox {
        ctx.o_stack.push(PsObject::real(bbox[0]))?;
        ctx.o_stack.push(PsObject::real(bbox[1]))?;
        ctx.o_stack.push(PsObject::real(bbox[2]))?;
        ctx.o_stack.push(PsObject::real(bbox[3]))?;
        return Ok(());
    }

    let (dev_min_x, dev_min_y, dev_max_x, dev_max_y) = path_bbox(&ctx.gstate.path);
    let ictm = ctx.gstate.ctm.invert().ok_or(PsError::UndefinedResult)?;

    // Transform all 4 corners of device-space bbox to user space
    let corners = [
        ictm.transform_point(dev_min_x, dev_min_y),
        ictm.transform_point(dev_max_x, dev_min_y),
        ictm.transform_point(dev_min_x, dev_max_y),
        ictm.transform_point(dev_max_x, dev_max_y),
    ];

    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for (cx, cy) in &corners {
        min_x = min_x.min(*cx);
        min_y = min_y.min(*cy);
        max_x = max_x.max(*cx);
        max_y = max_y.max(*cy);
    }

    ctx.o_stack.push(PsObject::real(min_x))?;
    ctx.o_stack.push(PsObject::real(min_y))?;
    ctx.o_stack.push(PsObject::real(max_x))?;
    ctx.o_stack.push(PsObject::real(max_y))?;
    Ok(())
}

/// Compute bounding box of path (conservative: uses control points for curves).
fn path_bbox(path: &PsPath) -> (f64, f64, f64, f64) {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;

    fn update(x: f64, y: f64, min_x: &mut f64, min_y: &mut f64, max_x: &mut f64, max_y: &mut f64) {
        *min_x = min_x.min(x);
        *min_y = min_y.min(y);
        *max_x = max_x.max(x);
        *max_y = max_y.max(y);
    }

    for seg in &path.segments {
        match seg {
            PathSegment::MoveTo(x, y) | PathSegment::LineTo(x, y) => {
                update(*x, *y, &mut min_x, &mut min_y, &mut max_x, &mut max_y);
            }
            PathSegment::CurveTo {
                x1,
                y1,
                x2,
                y2,
                x3,
                y3,
            } => {
                update(*x1, *y1, &mut min_x, &mut min_y, &mut max_x, &mut max_y);
                update(*x2, *y2, &mut min_x, &mut min_y, &mut max_x, &mut max_y);
                update(*x3, *y3, &mut min_x, &mut min_y, &mut max_x, &mut max_y);
            }
            PathSegment::ClosePath => {}
        }
    }

    (min_x, min_y, max_x, max_y)
}

/// `flattenpath`: — → — (convert curves to line segments)
pub fn op_flattenpath(ctx: &mut Context) -> Result<(), PsError> {
    let flatness = ctx.gstate.flatness;
    let old_path = ctx.gstate.path.clone();
    ctx.gstate.path = flatten_path(&old_path, flatness);
    Ok(())
}

/// Flatten a path: convert all CurveTo segments to LineTo segments.
fn flatten_path(path: &PsPath, flatness: f64) -> PsPath {
    let mut result = PsPath::new();
    let mut cx = 0.0f64;
    let mut cy = 0.0f64;

    for seg in &path.segments {
        match seg {
            PathSegment::MoveTo(x, y) => {
                result.segments.push(PathSegment::MoveTo(*x, *y));
                cx = *x;
                cy = *y;
            }
            PathSegment::LineTo(x, y) => {
                result.segments.push(PathSegment::LineTo(*x, *y));
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
                subdivide_cubic(
                    cx,
                    cy,
                    *x1,
                    *y1,
                    *x2,
                    *y2,
                    *x3,
                    *y3,
                    flatness,
                    &mut result.segments,
                );
                cx = *x3;
                cy = *y3;
            }
            PathSegment::ClosePath => {
                result.segments.push(PathSegment::ClosePath);
            }
        }
    }
    result
}

/// Recursive de Casteljau subdivision of a cubic bezier to line segments.
#[allow(clippy::too_many_arguments)]
fn subdivide_cubic(
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
    x3: f64,
    y3: f64,
    flatness: f64,
    segments: &mut Vec<PathSegment>,
) {
    // Check if curve is flat enough (midpoint deviation test)
    let mx = (x0 + x3) / 2.0;
    let my = (y0 + y3) / 2.0;
    let cx = (x0 + 3.0 * x1 + 3.0 * x2 + x3) / 8.0;
    let cy = (y0 + 3.0 * y1 + 3.0 * y2 + y3) / 8.0;
    let dx = cx - mx;
    let dy = cy - my;
    let dist_sq = dx * dx + dy * dy;

    if dist_sq <= flatness * flatness {
        segments.push(PathSegment::LineTo(x3, y3));
        return;
    }

    // Split at t=0.5
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

    subdivide_cubic(
        x0, y0, x01, y01, x012, y012, x0123, y0123, flatness, segments,
    );
    subdivide_cubic(
        x0123, y0123, x123, y123, x23, y23, x3, y3, flatness, segments,
    );
}

/// `reversepath`: — → — (reverse the order of path segments)
pub fn op_reversepath(ctx: &mut Context) -> Result<(), PsError> {
    let old_path = ctx.gstate.path.clone();
    let new_path = reverse_path(&old_path);

    // Update current point based on last reversed subpath
    if let Some(last_seg) = new_path.segments.last() {
        match last_seg {
            PathSegment::ClosePath => {
                // After closepath, currentpoint is the moveto of the last subpath
                // Find the last moveto
                for seg in new_path.segments.iter().rev() {
                    if let PathSegment::MoveTo(x, y) = seg {
                        ctx.gstate.current_point = Some((*x, *y));
                        break;
                    }
                }
            }
            PathSegment::MoveTo(x, y) | PathSegment::LineTo(x, y) => {
                ctx.gstate.current_point = Some((*x, *y));
            }
            PathSegment::CurveTo { x3, y3, .. } => {
                ctx.gstate.current_point = Some((*x3, *y3));
            }
        }
    } else if !old_path.is_empty() {
        ctx.gstate.current_point = None;
    }

    ctx.gstate.path = new_path;
    Ok(())
}

/// Reverse a path's direction.
fn reverse_path(path: &PsPath) -> PsPath {
    if path.is_empty() {
        return PsPath::new();
    }

    // Split into subpaths at each MoveTo
    let mut subpaths: Vec<Vec<PathSegment>> = Vec::new();
    let mut current_sub: Vec<PathSegment> = Vec::new();

    for seg in &path.segments {
        if matches!(seg, PathSegment::MoveTo(..)) && !current_sub.is_empty() {
            subpaths.push(current_sub);
            current_sub = Vec::new();
        }
        current_sub.push(seg.clone());
    }
    if !current_sub.is_empty() {
        subpaths.push(current_sub);
    }

    let mut result = PsPath::new();
    for sub in &subpaths {
        reverse_subpath(sub, &mut result.segments);
    }

    result
}

/// Reverse a single subpath.
///
/// Algorithm (matching PostForge):
/// 1. Extract moveto point and drawing segments (LineTo/CurveTo), note if closepath present
/// 2. Build points list: points[0] = moveto, points[i+1] = endpoint of segment i
/// 3. Start reversed subpath with MoveTo at last point
/// 4. Walk segments in reverse: for segment i, target = points[i]
///    - LineTo → LineTo(target)
///    - CurveTo → CurveTo with swapped control points (cp2→cp1, cp1→cp2), endpoint = target
/// 5. Append ClosePath if original had one
fn reverse_subpath(segments: &[PathSegment], result: &mut Vec<PathSegment>) {
    if segments.is_empty() {
        return;
    }

    let has_close = segments
        .last()
        .is_some_and(|s| matches!(s, PathSegment::ClosePath));

    // Collect non-moveto, non-closepath drawing segments
    let mut drawing_segs: Vec<&PathSegment> = Vec::new();
    let mut moveto_pt: Option<(f64, f64)> = None;

    for seg in segments {
        match seg {
            PathSegment::MoveTo(x, y) => {
                moveto_pt = Some((*x, *y));
            }
            PathSegment::ClosePath => {}
            _ => {
                drawing_segs.push(seg);
            }
        }
    }

    let moveto_pt = match moveto_pt {
        Some(pt) => pt,
        None => return,
    };

    // Build points list: points[0] = moveto, points[i+1] = endpoint of drawing_segs[i]
    let mut points = vec![moveto_pt];
    for seg in &drawing_segs {
        match seg {
            PathSegment::LineTo(x, y) => points.push((*x, *y)),
            PathSegment::CurveTo { x3, y3, .. } => points.push((*x3, *y3)),
            _ => {}
        }
    }

    // Start reversed subpath from last point
    let last = *points.last().unwrap();
    result.push(PathSegment::MoveTo(last.0, last.1));

    // Walk drawing segments in reverse
    for i in (0..drawing_segs.len()).rev() {
        let target = points[i]; // previous point becomes endpoint
        match drawing_segs[i] {
            PathSegment::LineTo(..) => {
                result.push(PathSegment::LineTo(target.0, target.1));
            }
            PathSegment::CurveTo { x1, y1, x2, y2, .. } => {
                // Swap control points: cp2 becomes cp1, cp1 becomes cp2
                result.push(PathSegment::CurveTo {
                    x1: *x2,
                    y1: *y2,
                    x2: *x1,
                    y2: *y1,
                    x3: target.0,
                    y3: target.1,
                });
            }
            _ => {}
        }
    }

    if has_close {
        result.push(PathSegment::ClosePath);
    }
}

/// `strokepath`: — → — (replace current path with its stroked outline)
///
/// Replaces the current path with a path that outlines the area that would
/// be painted by `stroke` using the current graphics state parameters.
pub fn op_strokepath(ctx: &mut Context) -> Result<(), PsError> {
    use crate::paint_ops::{ctm_singular_values, is_anisotropic};
    use crate::strokepath_algorithm as algo;


    if ctx.gstate.path.is_empty() {
        return Ok(());
    }

    let line_width = ctx.gstate.line_width;
    let line_cap = ctx.gstate.line_cap as i32;
    let line_join = ctx.gstate.line_join as i32;
    let miter_limit = ctx.gstate.miter_limit;
    let dash_array = ctx.gstate.dash_pattern.array.clone();
    let dash_offset = ctx.gstate.dash_pattern.offset;

    let ctm = ctx.gstate.ctm;
    let a = ctm.a;
    let b = ctm.b;
    let c = ctm.c;
    let d = ctm.d;
    let tx = ctm.tx;
    let ty = ctm.ty;
    let det = a * d - b * c;

    let anisotropic = is_anisotropic(&ctm);

    // Convert PS path to algorithm format (list of subpaths)
    let mut algo_path = ps_path_to_algo(&ctx.gstate.path);

    let groups = if anisotropic {
        // Anisotropic path: transform to user space, stroke there, transform back
        let inv_a = d / det;
        let inv_b = -b / det;
        let inv_c = -c / det;
        let inv_d = a / det;

        let user_path = algo::transform_algo_path(&algo_path, inv_a, inv_b, inv_c, inv_d, tx, ty);

        let (s_max, _s_min) = ctm_singular_values(&ctm);
        let min_user_lw = if s_max > 0.0 { 1.0 / s_max } else { 1.0 };
        let mut user_lw = line_width.max(min_user_lw);
        if user_lw < 1e-12 {
            user_lw = min_user_lw;
        }

        let da = if dash_array.is_empty() {
            None
        } else {
            Some(dash_array.as_slice())
        };

        let groups = algo::strokepath_grouped(
            &user_path,
            user_lw,
            line_cap,
            line_join,
            miter_limit,
            da,
            dash_offset,
            (user_lw * 0.05).min(0.1),
        );

        algo::transform_stroke_groups(&groups, a, b, c, d, tx, ty)
    } else {
        // Isotropic path: stroke in device space with pixel snapping
        let scale = det.abs().sqrt().max(1e-12);

        let mut device_line_width = line_width * scale;
        if device_line_width < 1e-12 {
            device_line_width = 1.0;
        }

        let device_dash_array: Vec<f64> = dash_array.iter().map(|v| v * scale).collect();
        let device_dash_offset = dash_offset * scale;

        // Pixel-snap path coordinates
        let half_width = device_line_width / 2.0;
        algo::snap_path_to_pixels(&mut algo_path, half_width);

        let da = if device_dash_array.is_empty() {
            None
        } else {
            Some(device_dash_array.as_slice())
        };

        algo::strokepath_grouped(
            &algo_path,
            device_line_width,
            line_cap,
            line_join,
            miter_limit,
            da,
            device_dash_offset,
            (device_line_width * 0.05).min(0.1),
        )
    };

    // Convert result back to PS path
    let new_path = algo_path_to_ps(&groups);

    // Update current point to last point of new path
    let new_cp = find_last_point(&new_path);

    ctx.gstate.path = new_path;
    ctx.gstate.current_point = new_cp;

    Ok(())
}

/// Convert PsPath → algorithm format (list of subpaths, each a Vec<PathElement>).
fn ps_path_to_algo(ps_path: &PsPath) -> crate::strokepath_algorithm::Path {
    use crate::strokepath_algorithm::PathElement as AE;

    let mut result: Vec<Vec<AE>> = Vec::new();
    let mut current_sp: Vec<AE> = Vec::new();

    for seg in &ps_path.segments {
        match seg {
            PathSegment::MoveTo(x, y) => {
                if !current_sp.is_empty() {
                    result.push(std::mem::take(&mut current_sp));
                }
                current_sp.push(AE::MoveTo(*x, *y));
            }
            PathSegment::LineTo(x, y) => {
                current_sp.push(AE::LineTo(*x, *y));
            }
            PathSegment::CurveTo {
                x1,
                y1,
                x2,
                y2,
                x3,
                y3,
            } => {
                current_sp.push(AE::CurveTo {
                    x1: *x1,
                    y1: *y1,
                    x2: *x2,
                    y2: *y2,
                    x3: *x3,
                    y3: *y3,
                });
            }
            PathSegment::ClosePath => {
                current_sp.push(AE::ClosePath);
            }
        }
    }
    if !current_sp.is_empty() {
        result.push(current_sp);
    }
    result
}

/// Convert algorithm output (groups of subpaths) back to PsPath.
fn algo_path_to_ps(groups: &[crate::strokepath_algorithm::Path]) -> PsPath {
    use crate::strokepath_algorithm::PathElement as AE;

    let mut path = PsPath::new();
    for group in groups {
        for sp in group {
            for elem in sp {
                match elem {
                    AE::MoveTo(x, y) => {
                        path.segments.push(PathSegment::MoveTo(*x, *y));
                    }
                    AE::LineTo(x, y) => {
                        path.segments.push(PathSegment::LineTo(*x, *y));
                    }
                    AE::CurveTo {
                        x1,
                        y1,
                        x2,
                        y2,
                        x3,
                        y3,
                    } => {
                        path.segments.push(PathSegment::CurveTo {
                            x1: *x1,
                            y1: *y1,
                            x2: *x2,
                            y2: *y2,
                            x3: *x3,
                            y3: *y3,
                        });
                    }
                    AE::ClosePath => {
                        path.segments.push(PathSegment::ClosePath);
                    }
                }
            }
        }
    }
    path
}

/// Find the last point in a path for currentpoint update.
fn find_last_point(path: &PsPath) -> Option<(f64, f64)> {
    for seg in path.segments.iter().rev() {
        match seg {
            PathSegment::MoveTo(x, y) | PathSegment::LineTo(x, y) => {
                return Some((*x, *y));
            }
            PathSegment::CurveTo { x3, y3, .. } => {
                return Some((*x3, *y3));
            }
            PathSegment::ClosePath => {}
        }
    }
    None
}

/// `pathforall`: moveproc lineproc curveproc closeproc → —
///
/// Path is in device space; inverse-transform coordinates through iCTM
/// before pushing to callbacks (PLRM requires user-space coordinates).
pub fn op_pathforall(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 4 {
        return Err(PsError::StackUnderflow);
    }

    let close_proc = ctx.o_stack.peek(0)?;
    let curve_proc = ctx.o_stack.peek(1)?;
    let line_proc = ctx.o_stack.peek(2)?;
    let move_proc = ctx.o_stack.peek(3)?;

    // Validate all are procedures (executable arrays)
    for proc in &[move_proc, line_proc, curve_proc, close_proc] {
        if !proc.is_array_type() || !proc.flags.is_executable() {
            return Err(PsError::TypeCheck);
        }
    }

    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;

    let ictm = ctx.gstate.ctm.invert().ok_or(PsError::UndefinedResult)?;

    // Clone segments to iterate (path might be modified during iteration)
    let segments = ctx.gstate.path.segments.clone();

    // Use incremental PathForall loop — processes one segment per eval iteration
    // to avoid blowing up the e_stack for paths with thousands of segments.
    let loop_entity = ctx.alloc_loop(stet_core::context::LoopState {
        loop_type: stet_core::context::LoopType::PathForall,
        proc_entity: EntityId(0),
        proc_start: 0,
        proc_len: 0,
        counter: 0.0,
        increment: 0.0,
        limit: 0.0,
        use_int: false,
        source: PsObject::null(),
        index: 0,
        dict_keys: None,
        path_segments: Some(segments),
        path_procs: Some([move_proc, line_proc, curve_proc, close_proc]),
        path_ictm: Some(ictm),
    });
    ctx.e_stack.push(PsObject::loop_mark(loop_entity))?;

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
    fn test_pathbbox_simple() {
        let mut ctx = setup();
        ctx.gstate
            .path
            .segments
            .push(PathSegment::MoveTo(10.0, 20.0));
        ctx.gstate
            .path
            .segments
            .push(PathSegment::LineTo(100.0, 200.0));
        ctx.gstate
            .path
            .segments
            .push(PathSegment::LineTo(50.0, 50.0));
        op_pathbbox(&mut ctx).unwrap();
        let ury = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        let urx = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        let lly = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        let llx = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        assert!((llx - 10.0).abs() < 1e-10);
        assert!((lly - 20.0).abs() < 1e-10);
        assert!((urx - 100.0).abs() < 1e-10);
        assert!((ury - 200.0).abs() < 1e-10);
    }

    #[test]
    fn test_pathbbox_empty() {
        let mut ctx = setup();
        assert_eq!(op_pathbbox(&mut ctx), Err(PsError::NoCurrentPoint));
    }

    #[test]
    fn test_flattenpath() {
        let mut ctx = setup();
        ctx.gstate.path.segments.push(PathSegment::MoveTo(0.0, 0.0));
        ctx.gstate.path.segments.push(PathSegment::CurveTo {
            x1: 50.0,
            y1: 100.0,
            x2: 150.0,
            y2: 100.0,
            x3: 200.0,
            y3: 0.0,
        });
        op_flattenpath(&mut ctx).unwrap();
        // Should have no more CurveTo segments
        for seg in &ctx.gstate.path.segments {
            assert!(!matches!(seg, PathSegment::CurveTo { .. }));
        }
        // Should have multiple LineTo segments
        let line_count = ctx
            .gstate
            .path
            .segments
            .iter()
            .filter(|s| matches!(s, PathSegment::LineTo(..)))
            .count();
        assert!(line_count > 1);
    }

    #[test]
    fn test_reversepath() {
        let mut ctx = setup();
        ctx.gstate.path.segments.push(PathSegment::MoveTo(0.0, 0.0));
        ctx.gstate
            .path
            .segments
            .push(PathSegment::LineTo(100.0, 0.0));
        ctx.gstate
            .path
            .segments
            .push(PathSegment::LineTo(100.0, 100.0));
        op_reversepath(&mut ctx).unwrap();
        // First segment should be MoveTo at last point
        assert!(
            matches!(ctx.gstate.path.segments[0], PathSegment::MoveTo(x, y) if (x - 100.0).abs() < 1e-10 && (y - 100.0).abs() < 1e-10)
        );
    }

    #[test]
    fn test_strokepath_stub() {
        let mut ctx = setup();
        op_strokepath(&mut ctx).unwrap(); // should not error
    }

    #[test]
    fn test_pathbbox_with_curve() {
        let mut ctx = setup();
        ctx.gstate.path.segments.push(PathSegment::MoveTo(0.0, 0.0));
        ctx.gstate.path.segments.push(PathSegment::CurveTo {
            x1: 50.0,
            y1: 100.0,
            x2: 150.0,
            y2: 100.0,
            x3: 200.0,
            y3: 0.0,
        });
        op_pathbbox(&mut ctx).unwrap();
        let ury = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        let urx = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        let _lly = ctx.o_stack.pop().unwrap();
        let _llx = ctx.o_stack.pop().unwrap();
        // Control points extend to y=100 and x=200
        assert!(urx >= 200.0 - 1e-10);
        assert!(ury >= 100.0 - 1e-10);
    }

    #[test]
    fn test_flattenpath_no_curves() {
        let mut ctx = setup();
        ctx.gstate.path.segments.push(PathSegment::MoveTo(0.0, 0.0));
        ctx.gstate
            .path
            .segments
            .push(PathSegment::LineTo(100.0, 0.0));
        let orig_len = ctx.gstate.path.segments.len();
        op_flattenpath(&mut ctx).unwrap();
        assert_eq!(ctx.gstate.path.segments.len(), orig_len);
    }
}
