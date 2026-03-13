// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Painting operators: fill, eofill, stroke, rectfill, rectstroke, erasepage, showpage.

use stet_core::context::Context;
use stet_core::device::{
    BgUcrState, FillParams, HalftoneState, PatternFillParams, SimpleColorSpace, SpotColor,
    SpotColorSpace, StrokeParams, TransferState,
};
use stet_core::display_list::DisplayElement;
use stet_core::error::PsError;
use stet_core::graphics_state::{ColorSpace, DashPattern, FillRule, Matrix, PathSegment, PsPath};
use stet_core::object::{PsObject, PsValue};

/// Capture the current SpotColor from graphics state if in Separation/DeviceN mode.
pub(crate) fn capture_spot_color(ctx: &Context) -> Option<SpotColor> {
    let tints = ctx.gstate.tint_values.as_ref()?;
    let table = ctx.gstate.cached_tint_table.as_ref()?;
    let cs = match &ctx.gstate.color_space {
        ColorSpace::Separation {
            name, alt_space, ..
        } => SpotColorSpace::Separation {
            name: name.clone(),
            alt: alt_space_to_simple(alt_space),
            tint_table: table.clone(),
        },
        ColorSpace::DeviceN {
            names, alt_space, ..
        } => SpotColorSpace::DeviceN {
            names: names.clone(),
            alt: alt_space_to_simple(alt_space),
            tint_table: table.clone(),
        },
        _ => return None,
    };
    Some(SpotColor {
        tint_values: tints.clone(),
        color_space: cs,
    })
}

/// Build TransferState from current graphics state sampled transfer functions.
pub(crate) fn capture_transfer_state(ctx: &Context) -> TransferState {
    TransferState {
        gray: ctx.gstate.sampled_transfer.clone(),
        color: ctx.gstate.sampled_color_transfer.clone(),
    }
}

/// Build HalftoneState from current graphics state pre-computed halftone screens.
pub(crate) fn capture_halftone_state(ctx: &Context) -> HalftoneState {
    HalftoneState {
        gray: ctx.gstate.precomputed_halftone.clone(),
        color: ctx.gstate.precomputed_color_halftone.clone(),
    }
}

/// Build BgUcrState from current graphics state sampled BG/UCR functions.
pub(crate) fn capture_bg_ucr_state(ctx: &Context) -> BgUcrState {
    BgUcrState {
        bg: ctx.gstate.sampled_black_generation.clone(),
        ucr: ctx.gstate.sampled_ucr.clone(),
    }
}

/// Map a ColorSpace alt_space to SimpleColorSpace.
fn alt_space_to_simple(cs: &ColorSpace) -> SimpleColorSpace {
    match cs {
        ColorSpace::DeviceGray => SimpleColorSpace::DeviceGray,
        ColorSpace::DeviceRGB => SimpleColorSpace::DeviceRGB,
        ColorSpace::DeviceCMYK => SimpleColorSpace::DeviceCMYK,
        _ => SimpleColorSpace::DeviceCMYK, // fallback
    }
}

/// Compute the scale factor of a CTM for converting user-space line widths to device space.
/// Uses the length of the first column vector (X-axis scale factor).
pub(crate) fn ctm_scale_factor(ctm: &Matrix) -> f64 {
    (ctm.a * ctm.a + ctm.b * ctm.b).sqrt()
}

/// Compute SVD singular values of the 2x2 matrix portion of the CTM.
/// Returns `(s_max, s_min)` — the maximum and minimum singular values.
pub(crate) fn ctm_singular_values(ctm: &Matrix) -> (f64, f64) {
    let a = ctm.a;
    let b = ctm.b;
    let c = ctm.c;
    let d = ctm.d;
    let sum_sq = a * a + b * b + c * c + d * d;
    let diff_term =
        ((a * a + b * b - c * c - d * d).powi(2) + 4.0 * (a * c + b * d).powi(2)).sqrt();
    let s_max = (0.5 * (sum_sq + diff_term)).max(0.0).sqrt();
    let s_min = (0.5 * (sum_sq - diff_term)).max(0.0).sqrt();
    (s_max, s_min)
}

/// Check if CTM has anisotropic scaling (non-uniform in X vs Y).
/// Uses the ratio of SVD singular values, matching PostForge's threshold.
pub(crate) fn is_anisotropic(ctm: &Matrix) -> bool {
    let (s_max, s_min) = ctm_singular_values(ctm);
    let det = (ctm.a * ctm.d - ctm.b * ctm.c).abs();
    s_min > 1e-10 && s_max / s_min > 1.01 && det > 1e-10
}

/// Transform a device-space path back to user space using inverse CTM.
fn inverse_transform_path(path: &PsPath, inv_ctm: &Matrix) -> PsPath {
    let mut result = PsPath::new();
    for seg in &path.segments {
        match seg {
            PathSegment::MoveTo(x, y) => {
                let (ux, uy) = inv_ctm.transform_point(*x, *y);
                result.segments.push(PathSegment::MoveTo(ux, uy));
            }
            PathSegment::LineTo(x, y) => {
                let (ux, uy) = inv_ctm.transform_point(*x, *y);
                result.segments.push(PathSegment::LineTo(ux, uy));
            }
            PathSegment::CurveTo {
                x1,
                y1,
                x2,
                y2,
                x3,
                y3,
            } => {
                let (ux1, uy1) = inv_ctm.transform_point(*x1, *y1);
                let (ux2, uy2) = inv_ctm.transform_point(*x2, *y2);
                let (ux3, uy3) = inv_ctm.transform_point(*x3, *y3);
                result.segments.push(PathSegment::CurveTo {
                    x1: ux1,
                    y1: uy1,
                    x2: ux2,
                    y2: uy2,
                    x3: ux3,
                    y3: uy3,
                });
            }
            PathSegment::ClosePath => {
                result.segments.push(PathSegment::ClosePath);
            }
        }
    }
    result
}

/// Close all open subpaths (for fill semantics).
fn close_subpaths(path: &PsPath) -> PsPath {
    let mut result = PsPath::new();
    let mut in_subpath = false;

    for seg in &path.segments {
        match seg {
            PathSegment::MoveTo(x, y) => {
                if in_subpath {
                    result.segments.push(PathSegment::ClosePath);
                }
                result.segments.push(PathSegment::MoveTo(*x, *y));
                in_subpath = true;
            }
            PathSegment::ClosePath => {
                result.segments.push(PathSegment::ClosePath);
                in_subpath = false;
            }
            other => {
                result.segments.push(other.clone());
            }
        }
    }
    if in_subpath {
        result.segments.push(PathSegment::ClosePath);
    }
    result
}

/// Push either a normal fill or a pattern fill element depending on gstate.
fn push_fill_element(ctx: &mut Context, path: PsPath, fill_rule: FillRule) {
    if let Some(pattern_id) = ctx.gstate.current_pattern
        && let Some(pat) = ctx.pattern_store.get(pattern_id as usize)
    {
        let params = PatternFillParams {
            path,
            fill_rule,
            tile: pat.cached_display_list.clone(),
            pattern_matrix: pat.pattern_matrix,
            bbox: pat.bbox,
            xstep: pat.xstep,
            ystep: pat.ystep,
            paint_type: pat.paint_type,
            underlying_color: ctx.gstate.pattern_underlying_color.clone(),
            pattern_id,
        };
        ctx.display_list
            .push(DisplayElement::PatternFill { params });
        return;
    }
    let params = FillParams {
        color: ctx.gstate.color.clone(),
        fill_rule,
        ctm: Matrix::identity(),
        is_text_glyph: false,
        overprint: ctx.gstate.overprint,
        overprint_mode: 0,
        painted_channels: 0,
        is_device_cmyk: false,
        spot_color: capture_spot_color(ctx),
        rendering_intent: ctx.gstate.rendering_intent,
        transfer: capture_transfer_state(ctx),
        halftone: capture_halftone_state(ctx),
        bg_ucr: capture_bg_ucr_state(ctx),
        alpha: 1.0,
        blend_mode: 0,
    };
    ctx.display_list.push(DisplayElement::Fill { path, params });
}

/// `fill`: — → — (fill current path, non-zero winding)
///
/// Path is already in device space; pass identity transform to device.
pub fn op_fill(ctx: &mut Context) -> Result<(), PsError> {
    let path = close_subpaths(&ctx.gstate.path);
    push_fill_element(ctx, path, FillRule::NonZeroWinding);
    // newpath after fill
    ctx.gstate.path.clear();
    ctx.gstate.current_point = None;
    Ok(())
}

/// `eofill`: — → — (fill with even-odd rule)
///
/// Path is already in device space; pass identity transform to device.
pub fn op_eofill(ctx: &mut Context) -> Result<(), PsError> {
    let path = close_subpaths(&ctx.gstate.path);
    push_fill_element(ctx, path, FillRule::EvenOdd);
    ctx.gstate.path.clear();
    ctx.gstate.current_point = None;
    Ok(())
}

/// `stroke`: — → — (stroke current path)
///
/// Path is stored in device space. For isotropic CTMs, scale line_width by a
/// single factor and pass identity transform. For anisotropic CTMs (non-uniform
/// X/Y scaling), inverse-transform the path back to user space and pass the
/// actual CTM to the device so it handles direction-dependent stroke widths.
pub fn op_stroke(ctx: &mut Context) -> Result<(), PsError> {
    if use_native_stroke(ctx) {
        stroke_native(ctx)
    } else {
        stroke_via_strokepath(ctx)
    }
}

/// Check pagedevice StrokeMethod: /NativeStroke uses tiny-skia, /StrokePathFill
/// (default) uses strokepath+fill.
fn use_native_stroke(ctx: &Context) -> bool {
    use stet_core::dict::DictKey;
    if let Some(pd) = ctx.gstate.page_device
        && let Some(name_id) = ctx.names.find(b"StrokeMethod")
        && let Some(obj) = ctx.dicts.get(pd, &DictKey::Name(name_id))
    {
        if let PsValue::Name(nid) = obj.value {
            return ctx.names.get_bytes(nid) == b"NativeStroke";
        }
    }
    false // default: StrokePathFill
}

/// Native stroke: emit DisplayElement::Stroke for tiny-skia to render.
fn stroke_native(ctx: &mut Context) -> Result<(), PsError> {
    let spot = capture_spot_color(ctx);
    let transfer = capture_transfer_state(ctx);
    let halftone = capture_halftone_state(ctx);
    let bg_ucr = capture_bg_ucr_state(ctx);
    if is_anisotropic(&ctx.gstate.ctm) {
        if let Some(inv_ctm) = ctx.gstate.ctm.invert() {
            let user_path = inverse_transform_path(&ctx.gstate.path, &inv_ctm);
            let params = StrokeParams {
                color: ctx.gstate.color.clone(),
                line_width: ctx.gstate.line_width,
                line_cap: ctx.gstate.line_cap,
                line_join: ctx.gstate.line_join,
                miter_limit: ctx.gstate.miter_limit,
                dash_pattern: ctx.gstate.dash_pattern.clone(),
                ctm: ctx.gstate.ctm,
                stroke_adjust: ctx.gstate.stroke_adjust,
                is_text_glyph: false,
                overprint: ctx.gstate.overprint,
                overprint_mode: 0,
                painted_channels: 0,
                spot_color: spot,
                rendering_intent: ctx.gstate.rendering_intent,
                transfer,
                halftone: halftone.clone(),
                bg_ucr: bg_ucr.clone(),
                alpha: 1.0,
                blend_mode: 0,
            };
            ctx.display_list.push(DisplayElement::Stroke {
                path: user_path,
                params,
            });
        }
    } else {
        let scale = ctm_scale_factor(&ctx.gstate.ctm);
        let params = StrokeParams {
            color: ctx.gstate.color.clone(),
            line_width: ctx.gstate.line_width * scale,
            line_cap: ctx.gstate.line_cap,
            line_join: ctx.gstate.line_join,
            miter_limit: ctx.gstate.miter_limit,
            dash_pattern: DashPattern {
                array: ctx
                    .gstate
                    .dash_pattern
                    .array
                    .iter()
                    .map(|&v| v * scale)
                    .collect(),
                offset: ctx.gstate.dash_pattern.offset * scale,
            },
            ctm: Matrix::identity(),
            stroke_adjust: ctx.gstate.stroke_adjust,
            is_text_glyph: false,
            overprint: ctx.gstate.overprint,
            overprint_mode: 0,
            painted_channels: 0,
            spot_color: spot,
            rendering_intent: ctx.gstate.rendering_intent,
            transfer,
            halftone,
            bg_ucr,
            alpha: 1.0,
            blend_mode: 0,
        };
        ctx.display_list.push(DisplayElement::Stroke {
            path: ctx.gstate.path.clone(),
            params,
        });
    }
    ctx.gstate.path.clear();
    ctx.gstate.current_point = None;
    Ok(())
}

/// StrokePathFill: convert stroke to filled outline via strokepath, then fill.
/// Clamps line width to minimum 1 device pixel so thin lines remain visible.
fn stroke_via_strokepath(ctx: &mut Context) -> Result<(), PsError> {
    // Clamp line width to at least 1 device pixel
    let scale = ctm_scale_factor(&ctx.gstate.ctm);
    let device_width = ctx.gstate.line_width * scale;
    if device_width < 1.0 && device_width > 1e-12 {
        ctx.gstate.line_width = 1.0 / scale;
    }
    crate::path_query_ops::op_strokepath(ctx)?;
    op_fill(ctx)
}

/// `rectfill`: x y width height → —
///
/// Builds rect in user space, transforms corners through CTM to device space.
/// Extract rectangles from operand stack or array argument.
/// Returns a vec of (x, y, w, h) tuples and pops the consumed operands.
fn extract_rects(ctx: &mut Context) -> Result<Vec<(f64, f64, f64, f64)>, PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let top = ctx.o_stack.peek(0)?;
    match top.value {
        PsValue::Array { entity, start, len } | PsValue::PackedArray { entity, start, len } => {
            // Array form: array must have length multiple of 4
            if len % 4 != 0 || len == 0 {
                return Err(PsError::RangeCheck);
            }
            // Validate all elements are numeric before popping
            let mut rects = Vec::with_capacity((len / 4) as usize);
            for i in (0..len).step_by(4) {
                let x_obj = ctx.arrays.get_element(entity, start + i);
                let y_obj = ctx.arrays.get_element(entity, start + i + 1);
                let w_obj = ctx.arrays.get_element(entity, start + i + 2);
                let h_obj = ctx.arrays.get_element(entity, start + i + 3);
                let x = x_obj.as_f64().ok_or(PsError::TypeCheck)?;
                let y = y_obj.as_f64().ok_or(PsError::TypeCheck)?;
                let w = w_obj.as_f64().ok_or(PsError::TypeCheck)?;
                let h = h_obj.as_f64().ok_or(PsError::TypeCheck)?;
                rects.push((x, y, w, h));
            }
            ctx.o_stack.pop()?;
            Ok(rects)
        }
        _ => {
            // 4-number form: x y width height
            if ctx.o_stack.len() < 4 {
                return Err(PsError::StackUnderflow);
            }
            let h_obj = ctx.o_stack.peek(0)?;
            let w_obj = ctx.o_stack.peek(1)?;
            let y_obj = ctx.o_stack.peek(2)?;
            let x_obj = ctx.o_stack.peek(3)?;
            let x = x_obj.as_f64().ok_or(PsError::TypeCheck)?;
            let y = y_obj.as_f64().ok_or(PsError::TypeCheck)?;
            let w = w_obj.as_f64().ok_or(PsError::TypeCheck)?;
            let h = h_obj.as_f64().ok_or(PsError::TypeCheck)?;
            ctx.o_stack.pop()?;
            ctx.o_stack.pop()?;
            ctx.o_stack.pop()?;
            ctx.o_stack.pop()?;
            Ok(vec![(x, y, w, h)])
        }
    }
}

/// Build a device-space path from rectangles, transforming through CTM.
fn build_rect_path_device(ctm: &Matrix, rects: &[(f64, f64, f64, f64)]) -> PsPath {
    let mut path = PsPath::new();
    for &(x, y, w, h) in rects {
        let (dx0, dy0) = ctm.transform_point(x, y);
        let (dx1, dy1) = ctm.transform_point(x + w, y);
        let (dx2, dy2) = ctm.transform_point(x + w, y + h);
        let (dx3, dy3) = ctm.transform_point(x, y + h);
        path.segments.push(PathSegment::MoveTo(dx0, dy0));
        path.segments.push(PathSegment::LineTo(dx1, dy1));
        path.segments.push(PathSegment::LineTo(dx2, dy2));
        path.segments.push(PathSegment::LineTo(dx3, dy3));
        path.segments.push(PathSegment::ClosePath);
    }
    path
}

/// Build a user-space path from rectangles (no CTM transform).
fn build_rect_path_user(rects: &[(f64, f64, f64, f64)]) -> PsPath {
    let mut path = PsPath::new();
    for &(x, y, w, h) in rects {
        path.segments.push(PathSegment::MoveTo(x, y));
        path.segments.push(PathSegment::LineTo(x + w, y));
        path.segments.push(PathSegment::LineTo(x + w, y + h));
        path.segments.push(PathSegment::LineTo(x, y + h));
        path.segments.push(PathSegment::ClosePath);
    }
    path
}

pub fn op_rectfill(ctx: &mut Context) -> Result<(), PsError> {
    let rects = extract_rects(ctx)?;
    let path = build_rect_path_device(&ctx.gstate.ctm, &rects);
    push_fill_element(ctx, path, FillRule::NonZeroWinding);
    Ok(())
}

/// `rectstroke`: x y width height → —
///
/// Builds rect in user space, transforms corners through CTM to device space.
/// For anisotropic CTMs, builds path in user space and passes CTM to device.
pub fn op_rectstroke(ctx: &mut Context) -> Result<(), PsError> {
    let rects = extract_rects(ctx)?;

    let spot = capture_spot_color(ctx);
    let transfer = capture_transfer_state(ctx);
    let halftone = capture_halftone_state(ctx);
    let bg_ucr = capture_bg_ucr_state(ctx);
    if is_anisotropic(&ctx.gstate.ctm) {
        let path = build_rect_path_user(&rects);
        let params = StrokeParams {
            color: ctx.gstate.color.clone(),
            line_width: ctx.gstate.line_width,
            line_cap: ctx.gstate.line_cap,
            line_join: ctx.gstate.line_join,
            miter_limit: ctx.gstate.miter_limit,
            dash_pattern: ctx.gstate.dash_pattern.clone(),
            ctm: ctx.gstate.ctm,
            stroke_adjust: ctx.gstate.stroke_adjust,
            is_text_glyph: false,
            overprint: ctx.gstate.overprint,
            overprint_mode: 0,
            painted_channels: 0,
            spot_color: spot,
            rendering_intent: ctx.gstate.rendering_intent,
            transfer,
            halftone: halftone.clone(),
            bg_ucr: bg_ucr.clone(),
            alpha: 1.0,
            blend_mode: 0,
        };
        ctx.display_list
            .push(DisplayElement::Stroke { path, params });
    } else {
        let path = build_rect_path_device(&ctx.gstate.ctm, &rects);
        let scale = ctm_scale_factor(&ctx.gstate.ctm);
        let params = StrokeParams {
            color: ctx.gstate.color.clone(),
            line_width: ctx.gstate.line_width * scale,
            line_cap: ctx.gstate.line_cap,
            line_join: ctx.gstate.line_join,
            miter_limit: ctx.gstate.miter_limit,
            dash_pattern: DashPattern {
                array: ctx
                    .gstate
                    .dash_pattern
                    .array
                    .iter()
                    .map(|&v| v * scale)
                    .collect(),
                offset: ctx.gstate.dash_pattern.offset * scale,
            },
            ctm: Matrix::identity(),
            stroke_adjust: ctx.gstate.stroke_adjust,
            is_text_glyph: false,
            overprint: ctx.gstate.overprint,
            overprint_mode: 0,
            painted_channels: 0,
            spot_color: spot,
            rendering_intent: ctx.gstate.rendering_intent,
            transfer,
            halftone,
            bg_ucr,
            alpha: 1.0,
            blend_mode: 0,
        };
        ctx.display_list
            .push(DisplayElement::Stroke { path, params });
    }
    Ok(())
}

/// `erasepage`: — → — (fill page with white)
pub fn op_erasepage(ctx: &mut Context) -> Result<(), PsError> {
    ctx.display_list.push(DisplayElement::ErasePage);
    Ok(())
}

/// `showpage`: — → — (output current page)
///
/// If a page device with EndPage/BeginPage procedures is active, uses the
/// continuation pattern: pushes EndPage proc + `.showpage_continue` on the
/// e_stack so PS procedures execute in the eval loop. Otherwise falls back
/// to direct rendering.
pub fn op_showpage(ctx: &mut Context) -> Result<(), PsError> {
    // Check for null device
    if crate::device_ops::is_null_device(ctx) {
        return Ok(());
    }

    // If we have a page device with EndPage, use the continuation protocol
    if ctx.gstate.page_device.is_some()
        && let Some(end_page) = crate::device_ops::get_pd_value(ctx, b"EndPage")
        && matches!(end_page.value, PsValue::Array { len, .. } if len > 0)
        && end_page.flags.is_executable()
    {
        // Push args for EndPage: pagecount reason(0=showpage)
        let page_count = crate::device_ops::get_pd_int(ctx, b"PageCount").unwrap_or(0);
        ctx.o_stack.push(PsObject::int(page_count))?;
        ctx.o_stack.push(PsObject::int(0))?; // reason 0 = showpage

        // Push .showpage_continue first (runs second), then EndPage (runs first)
        let continue_name = ctx.names.intern(b".showpage_continue");
        if let Some(continue_op) = ctx.dict_load(&stet_core::dict::DictKey::Name(continue_name)) {
            ctx.e_stack.push(continue_op)?;
        } else {
            eprintln!("Warning: .showpage_continue not found in dict stack");
        }
        ctx.e_stack.push(end_page)?;
        return Ok(());
    }

    // Fallback: direct rendering (no page device or no EndPage proc)
    if ctx.device.is_some() {
        if ctx.output_path.is_some() {
            let list = ctx.take_display_list();
            let device = ctx.device.as_mut().unwrap();
            let path = ctx.output_path.as_ref().unwrap();
            if let Err(e) = device.replay_and_show(list, path) {
                eprintln!("showpage error: {}", e);
            }
        } else {
            let device = ctx.device.as_mut().unwrap();
            stet_core::display_list::replay_to_device(&ctx.display_list, device.as_mut());
            ctx.display_list.clear();
        }
        ctx.device.as_mut().unwrap().erase_page();
    } else {
        ctx.display_list.clear();
    }

    // Reset graphics state (preserves page_device and current_font per PLRM)
    let page_device = ctx.gstate.page_device;
    let default_ctm = ctx.gstate.default_ctm;
    let current_font = ctx.gstate.current_font;
    ctx.gstate = stet_core::graphics_state::GraphicsState::new();
    ctx.gstate.page_device = page_device;
    ctx.gstate.current_font = current_font;

    if page_device.is_some() {
        crate::matrix_ops::op_initmatrix(ctx)?;
    } else {
        ctx.gstate.ctm = default_ctm;
        ctx.gstate.default_ctm = default_ctm;
    }

    if let Some(ref mut device) = ctx.device {
        device.init_clip();
    }
    // Note: gstate_stack is NOT cleared by showpage (per PLRM / PostForge).
    // Programs like dvi_ps rely on gsave/grestore around showpage to preserve
    // coordinate system setup across page boundaries.

    Ok(())
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
    fn test_fill_clears_path() {
        let mut ctx = setup();
        ctx.gstate.path.segments.push(PathSegment::MoveTo(0.0, 0.0));
        ctx.gstate
            .path
            .segments
            .push(PathSegment::LineTo(100.0, 0.0));
        ctx.gstate.current_point = Some((100.0, 0.0));
        op_fill(&mut ctx).unwrap();
        assert!(ctx.gstate.path.is_empty());
        assert!(ctx.gstate.current_point.is_none());
    }

    #[test]
    fn test_stroke_clears_path() {
        let mut ctx = setup();
        ctx.gstate.path.segments.push(PathSegment::MoveTo(0.0, 0.0));
        ctx.gstate
            .path
            .segments
            .push(PathSegment::LineTo(100.0, 0.0));
        ctx.gstate.current_point = Some((100.0, 0.0));
        op_stroke(&mut ctx).unwrap();
        assert!(ctx.gstate.path.is_empty());
        assert!(ctx.gstate.current_point.is_none());
    }

    #[test]
    fn test_eofill_clears_path() {
        let mut ctx = setup();
        ctx.gstate.path.segments.push(PathSegment::MoveTo(0.0, 0.0));
        ctx.gstate.current_point = Some((0.0, 0.0));
        op_eofill(&mut ctx).unwrap();
        assert!(ctx.gstate.path.is_empty());
    }

    #[test]
    fn test_rectfill() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(10.0)).unwrap();
        ctx.o_stack.push(PsObject::real(20.0)).unwrap();
        ctx.o_stack.push(PsObject::real(100.0)).unwrap();
        ctx.o_stack.push(PsObject::real(50.0)).unwrap();
        op_rectfill(&mut ctx).unwrap();
        assert!(ctx.o_stack.is_empty());
    }

    #[test]
    fn test_rectstroke() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(10.0)).unwrap();
        ctx.o_stack.push(PsObject::real(20.0)).unwrap();
        ctx.o_stack.push(PsObject::real(100.0)).unwrap();
        ctx.o_stack.push(PsObject::real(50.0)).unwrap();
        op_rectstroke(&mut ctx).unwrap();
        assert!(ctx.o_stack.is_empty());
    }

    #[test]
    fn test_erasepage_no_device() {
        let mut ctx = setup();
        op_erasepage(&mut ctx).unwrap(); // should not fail
    }

    #[test]
    fn test_showpage_no_device() {
        let mut ctx = setup();
        ctx.gstate.line_width = 5.0;
        op_showpage(&mut ctx).unwrap();
        assert_eq!(ctx.gstate.line_width, 1.0); // reset to default
    }

    #[test]
    fn test_close_subpaths() {
        let mut path = PsPath::new();
        path.segments.push(PathSegment::MoveTo(0.0, 0.0));
        path.segments.push(PathSegment::LineTo(100.0, 0.0));
        path.segments.push(PathSegment::LineTo(100.0, 100.0));
        // Not explicitly closed
        let closed = close_subpaths(&path);
        assert!(matches!(
            closed.segments.last(),
            Some(PathSegment::ClosePath)
        ));
    }
}
