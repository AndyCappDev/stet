// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Clipping operators: clip, eoclip, clippath, initclip, rectclip, clipsave, cliprestore.

use stet_core::context::Context;
use stet_core::error::PsError;
use stet_core::object::PsValue;
use stet_fonts::geometry::{Matrix, PathSegment, PsPath};
use stet_graphics::color::FillRule;
use stet_graphics::device::ClipParams;
use stet_graphics::display_list::DisplayElement;

/// Close all open subpaths in a path (PLRM: clip/eoclip implicitly close subpaths).
/// If the last segment is a LineTo that returns to the subpath's MoveTo start,
/// replace it with ClosePath rather than appending a redundant ClosePath.
fn close_subpaths(path: &mut PsPath) {
    if path.segments.is_empty() {
        return;
    }
    let mut result = Vec::with_capacity(path.segments.len());
    let mut subpath_start: Option<(f64, f64)> = None;

    for seg in path.segments.drain(..) {
        match seg {
            PathSegment::MoveTo(x, y) => {
                // Close previous subpath if open
                if let Some((sx, sy)) = subpath_start {
                    close_last_segment(&mut result, sx, sy);
                }
                subpath_start = Some((x, y));
                result.push(seg);
            }
            PathSegment::ClosePath => {
                subpath_start = None;
                result.push(seg);
            }
            _ => {
                result.push(seg);
            }
        }
    }
    // Close final subpath if open
    if let Some((sx, sy)) = subpath_start {
        close_last_segment(&mut result, sx, sy);
    }
    path.segments = result;
}

/// If the last segment is a LineTo returning to (sx, sy), replace it with ClosePath.
/// Otherwise append ClosePath.
fn close_last_segment(segs: &mut Vec<PathSegment>, sx: f64, sy: f64) {
    if let Some(PathSegment::LineTo(lx, ly)) = segs.last()
        && (*lx - sx).abs() < 0.01
        && (*ly - sy).abs() < 0.01
    {
        // Replace redundant LineTo with ClosePath
        *segs.last_mut().unwrap() = PathSegment::ClosePath;
        return;
    }
    segs.push(PathSegment::ClosePath);
}

/// `clip`: — → — (intersect clip with current path, non-zero winding)
///
/// Path is already in device space; pass identity transform to device.
/// PLRM: "clip implicitly closes any open subpaths of the current path."
pub fn op_clip(ctx: &mut Context) -> Result<(), PsError> {
    let mut path = ctx.gstate.path.clone();
    if path.is_empty() {
        return Ok(());
    }
    close_subpaths(&mut path);
    ctx.gstate.clip_path = Some(path.clone());
    ctx.gstate.clip_path_version += 1;
    let params = ClipParams {
        fill_rule: FillRule::NonZeroWinding,
        ctm: Matrix::identity(),
        stroke_params: None,
    };
    ctx.current_display_list_mut()
        .push(DisplayElement::Clip { path, params });
    // Note: clip does NOT clear the path (unlike fill/stroke)
    Ok(())
}

/// `eoclip`: — → — (intersect clip with current path, even-odd rule)
///
/// Path is already in device space; pass identity transform to device.
/// PLRM: "eoclip implicitly closes any open subpaths of the current path."
pub fn op_eoclip(ctx: &mut Context) -> Result<(), PsError> {
    let mut path = ctx.gstate.path.clone();
    if path.is_empty() {
        return Ok(());
    }
    close_subpaths(&mut path);
    ctx.gstate.clip_path = Some(path.clone());
    ctx.gstate.clip_path_version += 1;
    let params = ClipParams {
        fill_rule: FillRule::EvenOdd,
        ctm: Matrix::identity(),
        stroke_params: None,
    };
    ctx.current_display_list_mut()
        .push(DisplayElement::Clip { path, params });
    Ok(())
}

/// `clippath`: — → — (set current path to clip boundary)
///
/// Clip paths are stored in device space. When restoring default page rect,
/// reads PageSize from the page device dict if available.
pub fn op_clippath(ctx: &mut Context) -> Result<(), PsError> {
    if let Some(ref clip) = ctx.gstate.clip_path {
        ctx.gstate.path = clip.clone();
        // Set current point to last endpoint (already device space)
        if let Some(pt) = path_last_point(&ctx.gstate.path) {
            ctx.gstate.current_point = Some(pt);
        }
    } else {
        // Default clip is full page — read dimensions from page_device or fallback
        let (w, h) = if ctx.gstate.page_device.is_some() {
            if crate::device_ops::is_null_device(ctx) {
                // Null device: degenerate clip
                ctx.gstate.path.clear();
                ctx.gstate.path.segments.push(PathSegment::MoveTo(0.0, 0.0));
                ctx.gstate.current_point = Some((0.0, 0.0));
                return Ok(());
            }
            crate::device_ops::get_pd_f64_pair(ctx, b"PageSize")
                .unwrap_or((ctx.page_width as f64, ctx.page_height as f64))
        } else {
            (ctx.page_width as f64, ctx.page_height as f64)
        };

        let ctm = &ctx.gstate.ctm;
        let (dx0, dy0) = ctm.transform_point(0.0, 0.0);
        let (dx1, dy1) = ctm.transform_point(w, 0.0);
        let (dx2, dy2) = ctm.transform_point(w, h);
        let (dx3, dy3) = ctm.transform_point(0.0, h);
        ctx.gstate.path.clear();
        ctx.gstate.path.segments.push(PathSegment::MoveTo(dx0, dy0));
        ctx.gstate.path.segments.push(PathSegment::LineTo(dx1, dy1));
        ctx.gstate.path.segments.push(PathSegment::LineTo(dx2, dy2));
        ctx.gstate.path.segments.push(PathSegment::LineTo(dx3, dy3));
        ctx.gstate.path.segments.push(PathSegment::ClosePath);
        ctx.gstate.current_point = Some((dx0, dy0));
    }
    Ok(())
}

/// `initclip`: — → — (reset clip to page boundary)
///
/// For null devices, sets clip to a degenerate path (single MoveTo(0,0)).
/// Otherwise clears the clip path and resets the device clip.
pub fn op_initclip(ctx: &mut Context) -> Result<(), PsError> {
    if crate::device_ops::is_null_device(ctx) {
        // Null device: degenerate clip
        let mut p = stet_fonts::geometry::PsPath::new();
        p.segments.push(PathSegment::MoveTo(0.0, 0.0));
        ctx.gstate.clip_path = Some(p);
        return Ok(());
    }
    ctx.gstate.clip_path = None;
    ctx.gstate.clip_path_version += 1;
    ctx.current_display_list_mut()
        .push(DisplayElement::InitClip);
    Ok(())
}

/// `rectclip`: x y width height → —
pub fn op_rectclip(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let top = ctx.o_stack.peek(0)?;
    let (x, y, w, h) = match top.value {
        PsValue::Array { entity, start, len } => {
            // Array form: [x y w h]
            if len < 4 {
                return Err(PsError::RangeCheck);
            }
            let elems = ctx.arrays.get(entity, start, 4);
            let x = elems[0].as_f64().ok_or(PsError::TypeCheck)?;
            let y = elems[1].as_f64().ok_or(PsError::TypeCheck)?;
            let w = elems[2].as_f64().ok_or(PsError::TypeCheck)?;
            let h = elems[3].as_f64().ok_or(PsError::TypeCheck)?;
            ctx.o_stack.pop()?;
            (x, y, w, h)
        }
        _ => {
            // Numeric form: x y w h
            if ctx.o_stack.len() < 4 {
                return Err(PsError::StackUnderflow);
            }
            let h = ctx.o_stack.peek(0)?.as_f64().ok_or(PsError::TypeCheck)?;
            let w = ctx.o_stack.peek(1)?.as_f64().ok_or(PsError::TypeCheck)?;
            let y = ctx.o_stack.peek(2)?.as_f64().ok_or(PsError::TypeCheck)?;
            let x = ctx.o_stack.peek(3)?.as_f64().ok_or(PsError::TypeCheck)?;
            ctx.o_stack.pop()?;
            ctx.o_stack.pop()?;
            ctx.o_stack.pop()?;
            ctx.o_stack.pop()?;
            (x, y, w, h)
        }
    };

    // Transform rect corners to device space
    let ctm = &ctx.gstate.ctm;
    let (dx0, dy0) = ctm.transform_point(x, y);
    let (dx1, dy1) = ctm.transform_point(x + w, y);
    let (dx2, dy2) = ctm.transform_point(x + w, y + h);
    let (dx3, dy3) = ctm.transform_point(x, y + h);

    let mut path = PsPath::new();
    path.segments.push(PathSegment::MoveTo(dx0, dy0));
    path.segments.push(PathSegment::LineTo(dx1, dy1));
    path.segments.push(PathSegment::LineTo(dx2, dy2));
    path.segments.push(PathSegment::LineTo(dx3, dy3));
    path.segments.push(PathSegment::ClosePath);

    ctx.gstate.clip_path = Some(path.clone());
    ctx.gstate.clip_path_version += 1;
    let params = ClipParams {
        fill_rule: FillRule::NonZeroWinding,
        ctm: Matrix::identity(),
        stroke_params: None,
    };
    ctx.current_display_list_mut()
        .push(DisplayElement::Clip { path, params });
    // Implicit newpath
    ctx.gstate.path.clear();
    ctx.gstate.current_point = None;
    Ok(())
}

/// `clipsave`: — → — (push clip path)
pub fn op_clipsave(ctx: &mut Context) -> Result<(), PsError> {
    ctx.gstate.clip_stack.push(ctx.gstate.clip_path.clone());
    Ok(())
}

/// `cliprestore`: — → — (pop clip path)
///
/// Clip paths are already in device space; use identity transform.
pub fn op_cliprestore(ctx: &mut Context) -> Result<(), PsError> {
    if let Some(saved_clip) = ctx.gstate.clip_stack.pop() {
        let old_version = ctx.gstate.clip_path_version;
        ctx.gstate.clip_path = saved_clip;
        ctx.gstate.clip_path_version += 1;
        // Restore device clip only if it changed
        if ctx.gstate.clip_path_version != old_version {
            ctx.current_display_list_mut()
                .push(DisplayElement::InitClip);
            if let Some(clip_path) = ctx.gstate.clip_path.clone() {
                let params = ClipParams {
                    fill_rule: FillRule::NonZeroWinding,
                    ctm: Matrix::identity(),
                    stroke_params: None,
                };
                ctx.current_display_list_mut().push(DisplayElement::Clip {
                    path: clip_path,
                    params,
                });
            }
        }
    }
    Ok(())
}

/// Get the last explicit point in a path.
fn path_last_point(path: &PsPath) -> Option<(f64, f64)> {
    for seg in path.segments.iter().rev() {
        match seg {
            PathSegment::MoveTo(x, y) | PathSegment::LineTo(x, y) => return Some((*x, *y)),
            PathSegment::CurveTo { x3, y3, .. } => return Some((*x3, *y3)),
            PathSegment::ClosePath => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use stet_core::context::Context;
    use stet_core::object::PsObject;

    fn setup() -> Context {
        let mut ctx = Context::new();
        crate::build_system_dict(&mut ctx);
        ctx
    }

    #[test]
    fn test_clip_preserves_path() {
        let mut ctx = setup();
        ctx.gstate.path.segments.push(PathSegment::MoveTo(0.0, 0.0));
        ctx.gstate
            .path
            .segments
            .push(PathSegment::LineTo(100.0, 100.0));
        ctx.gstate.current_point = Some((100.0, 100.0));
        op_clip(&mut ctx).unwrap();
        // Path should NOT be cleared
        assert!(!ctx.gstate.path.is_empty());
        assert!(ctx.gstate.clip_path.is_some());
    }

    #[test]
    fn test_initclip() {
        let mut ctx = setup();
        ctx.gstate.clip_path = Some(PsPath::new());
        op_initclip(&mut ctx).unwrap();
        assert!(ctx.gstate.clip_path.is_none());
    }

    #[test]
    fn test_clippath_default() {
        let mut ctx = setup();
        op_clippath(&mut ctx).unwrap();
        assert!(!ctx.gstate.path.is_empty());
        // Should have page-sized rect
        assert_eq!(ctx.gstate.path.segments.len(), 5); // moveto + 3 lineto + close
    }

    #[test]
    fn test_rectclip() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(10.0)).unwrap();
        ctx.o_stack.push(PsObject::real(20.0)).unwrap();
        ctx.o_stack.push(PsObject::real(100.0)).unwrap();
        ctx.o_stack.push(PsObject::real(50.0)).unwrap();
        op_rectclip(&mut ctx).unwrap();
        assert!(ctx.gstate.clip_path.is_some());
        assert!(ctx.gstate.path.is_empty()); // implicit newpath
    }

    #[test]
    fn test_clipsave_cliprestore() {
        let mut ctx = setup();
        ctx.gstate.clip_path = Some(PsPath::new());
        op_clipsave(&mut ctx).unwrap();
        ctx.gstate.clip_path = None;
        op_cliprestore(&mut ctx).unwrap();
        assert!(ctx.gstate.clip_path.is_some());
    }

    #[test]
    fn test_eoclip() {
        let mut ctx = setup();
        ctx.gstate.path.segments.push(PathSegment::MoveTo(0.0, 0.0));
        ctx.gstate
            .path
            .segments
            .push(PathSegment::LineTo(100.0, 0.0));
        ctx.gstate.current_point = Some((100.0, 0.0));
        op_eoclip(&mut ctx).unwrap();
        assert!(ctx.gstate.clip_path.is_some());
        assert!(!ctx.gstate.path.is_empty()); // clip doesn't clear
    }
}
