// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF-imaging transparency operators (stet extension): alpha + blend mode.
//!
//! These operators are not part of the PostScript Level 3 spec. They expose
//! the PDF transparency imaging model — constant fill/stroke alpha, blend
//! mode, alpha-is-shape, and text knockout — to PostScript code. See
//! `docs/PLAN-PDF-EXTENSIONS.md` and the GhostScript-compatible aliases in
//! `resources/Init/pdfextensions.ps`.

use stet_core::context::{Context, GroupFrame};
use stet_core::dict::DictKey;
use stet_core::error::PsError;
use stet_core::object::{EntityId, PsObject, PsValue};
use stet_graphics::display_list::{DisplayElement, DisplayList, GroupColorSpace, GroupParams};

/// `setfillopacity`: num → — (PDF `ca`).
pub fn op_setfillopacity(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let v = ctx.o_stack.peek(0)?.as_f64().ok_or(PsError::TypeCheck)?;
    if !(0.0..=1.0).contains(&v) {
        return Err(PsError::RangeCheck);
    }
    ctx.o_stack.pop()?;
    ctx.gstate.fill_opacity = v;
    Ok(())
}

/// `currentfillopacity`: — → num.
pub fn op_currentfillopacity(ctx: &mut Context) -> Result<(), PsError> {
    ctx.o_stack.push(PsObject::real(ctx.gstate.fill_opacity))?;
    Ok(())
}

/// `setstrokeopacity`: num → — (PDF `CA`).
pub fn op_setstrokeopacity(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let v = ctx.o_stack.peek(0)?.as_f64().ok_or(PsError::TypeCheck)?;
    if !(0.0..=1.0).contains(&v) {
        return Err(PsError::RangeCheck);
    }
    ctx.o_stack.pop()?;
    ctx.gstate.stroke_opacity = v;
    Ok(())
}

/// `currentstrokeopacity`: — → num.
pub fn op_currentstrokeopacity(ctx: &mut Context) -> Result<(), PsError> {
    ctx.o_stack
        .push(PsObject::real(ctx.gstate.stroke_opacity))?;
    Ok(())
}

/// Map a PDF blend-mode name to the renderer's u8 index.
///
/// The values come from `u8_to_blend_mode` in `stet-render/src/skia_device.rs`:
/// 0=Normal, 1=Multiply, 2=Screen, 3=Overlay, 4=Darken, 5=Lighten,
/// 6=ColorDodge, 7=ColorBurn, 8=HardLight, 9=SoftLight, 10=Difference,
/// 11=Exclusion, 12=Hue, 13=Saturation, 14=Color, 15=Luminosity.
fn blend_mode_index(name: &[u8]) -> Option<u8> {
    Some(match name {
        b"Normal" | b"Compatible" => 0,
        b"Multiply" => 1,
        b"Screen" => 2,
        b"Overlay" => 3,
        b"Darken" => 4,
        b"Lighten" => 5,
        b"ColorDodge" => 6,
        b"ColorBurn" => 7,
        b"HardLight" => 8,
        b"SoftLight" => 9,
        b"Difference" => 10,
        b"Exclusion" => 11,
        b"Hue" => 12,
        b"Saturation" => 13,
        b"Color" => 14,
        b"Luminosity" => 15,
        _ => return None,
    })
}

/// Reverse of [`blend_mode_index`].
fn blend_mode_name(idx: u8) -> &'static [u8] {
    match idx {
        1 => b"Multiply",
        2 => b"Screen",
        3 => b"Overlay",
        4 => b"Darken",
        5 => b"Lighten",
        6 => b"ColorDodge",
        7 => b"ColorBurn",
        8 => b"HardLight",
        9 => b"SoftLight",
        10 => b"Difference",
        11 => b"Exclusion",
        12 => b"Hue",
        13 => b"Saturation",
        14 => b"Color",
        15 => b"Luminosity",
        _ => b"Normal",
    }
}

/// `setblendmode`: name → —.
pub fn op_setblendmode(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let nid = match ctx.o_stack.peek(0)?.value {
        PsValue::Name(n) => n,
        _ => return Err(PsError::TypeCheck),
    };
    let idx = blend_mode_index(ctx.names.get_bytes(nid)).ok_or(PsError::RangeCheck)?;
    ctx.o_stack.pop()?;
    ctx.gstate.blend_mode = idx;
    Ok(())
}

/// `currentblendmode`: — → name.
pub fn op_currentblendmode(ctx: &mut Context) -> Result<(), PsError> {
    let nid = ctx.names.intern(blend_mode_name(ctx.gstate.blend_mode));
    ctx.o_stack.push(PsObject::name_lit(nid))?;
    Ok(())
}

/// `setalphaisshape`: bool → — (PDF `AIS`).
pub fn op_setalphaisshape(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let b = match ctx.o_stack.peek(0)?.value {
        PsValue::Bool(v) => v,
        _ => return Err(PsError::TypeCheck),
    };
    ctx.o_stack.pop()?;
    ctx.gstate.alpha_is_shape = b;
    Ok(())
}

/// `currentalphaisshape`: — → bool.
pub fn op_currentalphaisshape(ctx: &mut Context) -> Result<(), PsError> {
    ctx.o_stack
        .push(PsObject::bool(ctx.gstate.alpha_is_shape))?;
    Ok(())
}

/// `settextknockout`: bool → — (PDF `TK`).
pub fn op_settextknockout(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let b = match ctx.o_stack.peek(0)?.value {
        PsValue::Bool(v) => v,
        _ => return Err(PsError::TypeCheck),
    };
    ctx.o_stack.pop()?;
    ctx.gstate.text_knockout = b;
    Ok(())
}

/// `currenttextknockout`: — → bool.
pub fn op_currenttextknockout(ctx: &mut Context) -> Result<(), PsError> {
    ctx.o_stack.push(PsObject::bool(ctx.gstate.text_knockout))?;
    Ok(())
}

// ----- Transparency groups (Phase 2) ---------------------------------------

/// Look up a key by name in a dict, returning the raw object.
fn dict_get(ctx: &Context, dict: EntityId, key: &[u8]) -> Option<PsObject> {
    let nid = ctx.names.find(key)?;
    ctx.dicts.get(dict, &DictKey::Name(nid))
}

/// Read a boolean dict entry; returns `default` when absent or non-bool.
fn dict_get_bool(ctx: &Context, dict: EntityId, key: &[u8], default: bool) -> bool {
    match dict_get(ctx, dict, key).map(|o| o.value) {
        Some(PsValue::Bool(b)) => b,
        _ => default,
    }
}

/// Resolve the `/CS` entry on a transparency-group dict to a
/// [`GroupColorSpace`]. PostScript's color-space syntax mirrors PDF's;
/// only the four spec-recognised group color spaces are accepted.
/// Anything unknown raises `rangecheck`.
fn resolve_group_cs(ctx: &Context, dict: EntityId) -> Result<GroupColorSpace, PsError> {
    let Some(obj) = dict_get(ctx, dict, b"CS") else {
        return Ok(GroupColorSpace::Inherited);
    };
    let name = match obj.value {
        PsValue::Name(n) => Some(ctx.names.get_bytes(n).to_vec()),
        PsValue::Array { entity, start, len } if len >= 1 => {
            match ctx.arrays.get_element(entity, start).value {
                PsValue::Name(n) => Some(ctx.names.get_bytes(n).to_vec()),
                _ => None,
            }
        }
        _ => None,
    };
    match name.as_deref() {
        Some(b"DeviceGray" | b"CalGray") => Ok(GroupColorSpace::DeviceGray),
        Some(b"DeviceRGB" | b"CalRGB") => Ok(GroupColorSpace::DeviceRGB),
        Some(b"DeviceCMYK") => Ok(GroupColorSpace::DeviceCMYK),
        Some(b"ICCBased") => {
            // PostScript can't describe an ICC stream inline the way PDF
            // does, so we accept the name and fall back to inherited;
            // embedding profiles would require extending the dict shape.
            Ok(GroupColorSpace::Inherited)
        }
        _ => Err(PsError::RangeCheck),
    }
}

/// Read `/BBox` from the group dict and transform it through the current
/// CTM into device space. Returns `None` when the dict has no `/BBox`.
fn user_bbox_to_device(ctx: &Context, dict: EntityId) -> Result<Option<[f64; 4]>, PsError> {
    let Some(obj) = dict_get(ctx, dict, b"BBox") else {
        return Ok(None);
    };
    let (entity, start, len) = match obj.value {
        PsValue::Array { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };
    if len < 4 {
        return Err(PsError::RangeCheck);
    }
    let mut user = [0.0f64; 4];
    for (i, slot) in user.iter_mut().enumerate() {
        *slot = ctx
            .arrays
            .get_element(entity, start + i as u32)
            .as_f64()
            .ok_or(PsError::TypeCheck)?;
    }
    let ctm = &ctx.gstate.ctm;
    let corners = [
        ctm.transform_point(user[0], user[1]),
        ctm.transform_point(user[2], user[1]),
        ctm.transform_point(user[2], user[3]),
        ctm.transform_point(user[0], user[3]),
    ];
    let xs = corners.map(|(x, _)| x);
    let ys = corners.map(|(_, y)| y);
    let xmin = xs.iter().copied().fold(f64::INFINITY, f64::min);
    let xmax = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let ymin = ys.iter().copied().fold(f64::INFINITY, f64::min);
    let ymax = ys.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    Ok(Some([xmin, ymin, xmax, ymax]))
}

/// Default group bbox when the dict supplies none. We use the active
/// device clip path's bbox if there is one; otherwise the page bbox.
/// Both are already in device space (paths are stored device-space per
/// stet's convention).
fn default_device_bbox(ctx: &Context) -> [f64; 4] {
    if let Some(clip) = ctx.gstate.clip_path.as_ref() {
        let mut xmin = f64::INFINITY;
        let mut ymin = f64::INFINITY;
        let mut xmax = f64::NEG_INFINITY;
        let mut ymax = f64::NEG_INFINITY;
        let mut saw = false;
        for seg in &clip.segments {
            use stet_fonts::geometry::PathSegment::*;
            let pts: &[(f64, f64)] = match seg {
                MoveTo(x, y) | LineTo(x, y) => &[(*x, *y)][..],
                CurveTo {
                    x1,
                    y1,
                    x2,
                    y2,
                    x3,
                    y3,
                } => &[(*x1, *y1), (*x2, *y2), (*x3, *y3)][..],
                ClosePath => &[][..],
            };
            for (x, y) in pts {
                xmin = xmin.min(*x);
                ymin = ymin.min(*y);
                xmax = xmax.max(*x);
                ymax = ymax.max(*y);
                saw = true;
            }
        }
        if saw {
            return [xmin, ymin, xmax, ymax];
        }
    }
    [0.0, 0.0, ctx.page_width as f64, ctx.page_height as f64]
}

/// `begintransparencygroup`: dict → —
///
/// Opens a transparency-group capture frame. Subsequent paint operators
/// emit into the frame's display list until [`op_endtransparencygroup`]
/// closes it. Recognised dict keys: `/Isolated`, `/Knockout`, `/CS`,
/// `/BBox`. `/CS` accepts `/DeviceGray`, `/DeviceRGB`, `/DeviceCMYK`,
/// `/CalGray`, `/CalRGB`, or a `[/ICCBased …]` array (treated as
/// inherited). Other names raise `rangecheck`.
pub fn op_begintransparencygroup(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let dict = match ctx.o_stack.peek(0)?.value {
        PsValue::Dict(e) => e,
        _ => return Err(PsError::TypeCheck),
    };
    let isolated = dict_get_bool(ctx, dict, b"Isolated", false);
    let knockout = dict_get_bool(ctx, dict, b"Knockout", false);
    let color_space = resolve_group_cs(ctx, dict)?;
    let bbox = match user_bbox_to_device(ctx, dict)? {
        Some(b) => b,
        None => default_device_bbox(ctx),
    };
    ctx.o_stack.pop()?;

    // alpha/blend_mode are placeholders here; op_endtransparencygroup
    // overwrites them from the gstate active at end time.
    let params = GroupParams {
        bbox,
        isolated,
        knockout,
        blend_mode: 0,
        alpha: 1.0,
        color_space,
    };
    let saved_clip_path_version = ctx.gstate.clip_path_version;
    let saved_gsave_depth = ctx.gstate_stack.len();
    ctx.group_stack.push(GroupFrame {
        display_list: DisplayList::new(),
        params,
        saved_clip_path_version,
        saved_gsave_depth,
    });
    Ok(())
}

/// `endtransparencygroup`: — → —
///
/// Closes the topmost transparency-group capture frame opened by
/// [`op_begintransparencygroup`] and emits a [`DisplayElement::Group`]
/// containing everything captured into the next-innermost emit target.
/// Raises `rangecheck` when no group is open. The compositing alpha and
/// blend mode are read from the gstate at the moment this operator runs
/// (matching PDF's "the q/Q around `Do` controls the group composite"
/// model). `rangecheck` is also raised when the gsave depth at end
/// differs from begin — a `gsave` made inside the group must be matched
/// by a `grestore` before close.
pub fn op_endtransparencygroup(ctx: &mut Context) -> Result<(), PsError> {
    let Some(mut frame) = ctx.group_stack.pop() else {
        return Err(PsError::RangeCheck);
    };
    if ctx.gstate_stack.len() != frame.saved_gsave_depth {
        ctx.group_stack.push(frame);
        return Err(PsError::RangeCheck);
    }
    let _ = frame.saved_clip_path_version; // informational
    frame.params.alpha = ctx.gstate.fill_opacity;
    frame.params.blend_mode = ctx.gstate.blend_mode;
    let elements = std::mem::take(&mut frame.display_list);
    ctx.current_display_list_mut().push(DisplayElement::Group {
        elements,
        params: frame.params,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use stet_core::object::PsObject;

    fn make_ctx() -> Context {
        Context::new()
    }

    #[test]
    fn fill_opacity_round_trip() {
        let mut ctx = make_ctx();
        ctx.o_stack.push(PsObject::real(0.5)).unwrap();
        op_setfillopacity(&mut ctx).unwrap();
        assert_eq!(ctx.gstate.fill_opacity, 0.5);
        op_currentfillopacity(&mut ctx).unwrap();
        let top = ctx.o_stack.peek(0).unwrap();
        assert_eq!(top.as_f64(), Some(0.5));
    }

    #[test]
    fn fill_opacity_range_check() {
        let mut ctx = make_ctx();
        ctx.o_stack.push(PsObject::real(1.5)).unwrap();
        assert!(matches!(
            op_setfillopacity(&mut ctx),
            Err(PsError::RangeCheck)
        ));
        ctx.o_stack.pop().unwrap();
        ctx.o_stack.push(PsObject::real(-0.1)).unwrap();
        assert!(matches!(
            op_setfillopacity(&mut ctx),
            Err(PsError::RangeCheck)
        ));
    }

    #[test]
    fn fill_opacity_type_check() {
        let mut ctx = make_ctx();
        let nid = ctx.names.intern(b"Normal");
        ctx.o_stack.push(PsObject::name_lit(nid)).unwrap();
        assert!(matches!(
            op_setfillopacity(&mut ctx),
            Err(PsError::TypeCheck)
        ));
    }

    #[test]
    fn stroke_opacity_round_trip() {
        let mut ctx = make_ctx();
        ctx.o_stack.push(PsObject::real(0.25)).unwrap();
        op_setstrokeopacity(&mut ctx).unwrap();
        assert_eq!(ctx.gstate.stroke_opacity, 0.25);
        op_currentstrokeopacity(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.peek(0).unwrap().as_f64(), Some(0.25));
    }

    #[test]
    fn blend_mode_round_trip() {
        let mut ctx = make_ctx();
        let cases: &[(&[u8], u8)] = &[
            (b"Normal", 0),
            (b"Multiply", 1),
            (b"Screen", 2),
            (b"Overlay", 3),
            (b"Darken", 4),
            (b"Lighten", 5),
            (b"ColorDodge", 6),
            (b"ColorBurn", 7),
            (b"HardLight", 8),
            (b"SoftLight", 9),
            (b"Difference", 10),
            (b"Exclusion", 11),
            (b"Hue", 12),
            (b"Saturation", 13),
            (b"Color", 14),
            (b"Luminosity", 15),
        ];
        for (name, expected) in cases {
            let nid = ctx.names.intern(name);
            ctx.o_stack.push(PsObject::name_lit(nid)).unwrap();
            op_setblendmode(&mut ctx).unwrap();
            assert_eq!(ctx.gstate.blend_mode, *expected, "{:?}", name);
            op_currentblendmode(&mut ctx).unwrap();
            let returned = match ctx.o_stack.peek(0).unwrap().value {
                PsValue::Name(n) => ctx.names.get_bytes(n).to_vec(),
                _ => panic!("expected name"),
            };
            assert_eq!(&returned, name);
            ctx.o_stack.pop().unwrap();
        }
    }

    #[test]
    fn blend_mode_compatible_alias() {
        let mut ctx = make_ctx();
        let nid = ctx.names.intern(b"Compatible");
        ctx.o_stack.push(PsObject::name_lit(nid)).unwrap();
        op_setblendmode(&mut ctx).unwrap();
        assert_eq!(ctx.gstate.blend_mode, 0);
    }

    #[test]
    fn blend_mode_unknown_name() {
        let mut ctx = make_ctx();
        let nid = ctx.names.intern(b"NotARealBlendMode");
        ctx.o_stack.push(PsObject::name_lit(nid)).unwrap();
        assert!(matches!(
            op_setblendmode(&mut ctx),
            Err(PsError::RangeCheck)
        ));
    }

    #[test]
    fn alphaisshape_round_trip() {
        let mut ctx = make_ctx();
        ctx.o_stack.push(PsObject::bool(true)).unwrap();
        op_setalphaisshape(&mut ctx).unwrap();
        assert!(ctx.gstate.alpha_is_shape);
        op_currentalphaisshape(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.peek(0).unwrap().value, PsValue::Bool(true));
    }

    #[test]
    fn textknockout_round_trip() {
        let mut ctx = make_ctx();
        ctx.o_stack.push(PsObject::bool(false)).unwrap();
        op_settextknockout(&mut ctx).unwrap();
        assert!(!ctx.gstate.text_knockout);
        op_currenttextknockout(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.peek(0).unwrap().value, PsValue::Bool(false));
    }

    #[test]
    fn textknockout_default_true() {
        let ctx = make_ctx();
        assert!(ctx.gstate.text_knockout);
    }

    fn make_dict(ctx: &mut Context, pairs: &[(&[u8], PsObject)]) -> PsObject {
        let dict_id = ctx.dicts.allocate(pairs.len(), b"<test>");
        for (k, v) in pairs {
            let nid = ctx.names.intern(k);
            ctx.dicts.put(dict_id, DictKey::Name(nid), *v);
        }
        PsObject {
            value: PsValue::Dict(dict_id),
            flags: stet_core::object::ObjFlags::literal(),
        }
    }

    #[test]
    fn group_open_and_close_emits_group_element() {
        let mut ctx = make_ctx();
        let dict = make_dict(&mut ctx, &[]);
        ctx.o_stack.push(dict).unwrap();
        op_begintransparencygroup(&mut ctx).unwrap();
        assert_eq!(ctx.group_stack.len(), 1);
        assert_eq!(ctx.display_list.len(), 0);
        op_endtransparencygroup(&mut ctx).unwrap();
        assert!(ctx.group_stack.is_empty());
        assert_eq!(ctx.display_list.len(), 1);
        match &ctx.display_list.elements()[0] {
            DisplayElement::Group { params, elements } => {
                assert!(elements.is_empty());
                assert!(!params.isolated);
                assert!(!params.knockout);
            }
            _ => panic!("expected Group"),
        }
    }

    #[test]
    fn group_isolated_knockout_flags_propagate() {
        let mut ctx = make_ctx();
        let dict = make_dict(
            &mut ctx,
            &[
                (b"Isolated", PsObject::bool(true)),
                (b"Knockout", PsObject::bool(true)),
            ],
        );
        ctx.o_stack.push(dict).unwrap();
        op_begintransparencygroup(&mut ctx).unwrap();
        op_endtransparencygroup(&mut ctx).unwrap();
        match &ctx.display_list.elements()[0] {
            DisplayElement::Group { params, .. } => {
                assert!(params.isolated);
                assert!(params.knockout);
            }
            _ => panic!("expected Group"),
        }
    }

    #[test]
    fn group_cs_resolves() {
        let mut ctx = make_ctx();
        let cmyk = ctx.names.intern(b"DeviceCMYK");
        let dict = make_dict(&mut ctx, &[(b"CS", PsObject::name_lit(cmyk))]);
        ctx.o_stack.push(dict).unwrap();
        op_begintransparencygroup(&mut ctx).unwrap();
        op_endtransparencygroup(&mut ctx).unwrap();
        match &ctx.display_list.elements()[0] {
            DisplayElement::Group { params, .. } => {
                assert_eq!(params.color_space, GroupColorSpace::DeviceCMYK);
            }
            _ => panic!("expected Group"),
        }
    }

    #[test]
    fn group_cs_unknown_name_rangecheck() {
        let mut ctx = make_ctx();
        let bogus = ctx.names.intern(b"NotAColorSpace");
        let dict = make_dict(&mut ctx, &[(b"CS", PsObject::name_lit(bogus))]);
        ctx.o_stack.push(dict).unwrap();
        assert!(matches!(
            op_begintransparencygroup(&mut ctx),
            Err(PsError::RangeCheck)
        ));
        // Stack unchanged on error.
        assert_eq!(ctx.o_stack.len(), 1);
        assert!(ctx.group_stack.is_empty());
    }

    #[test]
    fn group_end_without_begin_rangecheck() {
        let mut ctx = make_ctx();
        assert!(matches!(
            op_endtransparencygroup(&mut ctx),
            Err(PsError::RangeCheck)
        ));
    }

    #[test]
    fn group_nesting_writes_to_inner_list() {
        let mut ctx = make_ctx();
        let outer = make_dict(&mut ctx, &[]);
        ctx.o_stack.push(outer).unwrap();
        op_begintransparencygroup(&mut ctx).unwrap();
        let inner = make_dict(&mut ctx, &[]);
        ctx.o_stack.push(inner).unwrap();
        op_begintransparencygroup(&mut ctx).unwrap();
        assert_eq!(ctx.group_stack.len(), 2);
        op_endtransparencygroup(&mut ctx).unwrap();
        assert_eq!(ctx.group_stack.len(), 1);
        // Inner group emitted into outer frame's list, not the page list.
        assert_eq!(ctx.display_list.len(), 0);
        assert_eq!(ctx.group_stack.last().unwrap().display_list.len(), 1);
        op_endtransparencygroup(&mut ctx).unwrap();
        assert_eq!(ctx.display_list.len(), 1);
    }

    #[test]
    fn group_alpha_blend_captured_at_end() {
        let mut ctx = make_ctx();
        let dict = make_dict(&mut ctx, &[]);
        ctx.o_stack.push(dict).unwrap();
        op_begintransparencygroup(&mut ctx).unwrap();
        ctx.gstate.fill_opacity = 0.4;
        ctx.gstate.blend_mode = 1; // Multiply
        op_endtransparencygroup(&mut ctx).unwrap();
        match &ctx.display_list.elements()[0] {
            DisplayElement::Group { params, .. } => {
                assert!((params.alpha - 0.4).abs() < 1e-9);
                assert_eq!(params.blend_mode, 1);
            }
            _ => panic!("expected Group"),
        }
    }

    #[test]
    fn group_unbalanced_gsave_blocks_close() {
        let mut ctx = make_ctx();
        let dict = make_dict(&mut ctx, &[]);
        ctx.o_stack.push(dict).unwrap();
        op_begintransparencygroup(&mut ctx).unwrap();
        // Simulate an unbalanced gsave inside the group.
        ctx.gstate_stack
            .push(stet_core::graphics_state::GstateEntry {
                state: ctx.gstate.clone(),
                saved_by_save: false,
            });
        assert!(matches!(
            op_endtransparencygroup(&mut ctx),
            Err(PsError::RangeCheck)
        ));
        assert_eq!(ctx.group_stack.len(), 1);
    }

    #[test]
    fn gsave_grestore_restores_fields() {
        let mut ctx = make_ctx();
        ctx.o_stack.push(PsObject::real(0.5)).unwrap();
        op_setfillopacity(&mut ctx).unwrap();
        let saved = ctx.gstate.clone();
        ctx.gstate_stack
            .push(stet_core::graphics_state::GstateEntry {
                state: saved,
                saved_by_save: false,
            });
        ctx.o_stack.push(PsObject::real(0.1)).unwrap();
        op_setfillopacity(&mut ctx).unwrap();
        assert_eq!(ctx.gstate.fill_opacity, 0.1);
        // Manually restore (mimicking grestore body)
        let entry = ctx.gstate_stack.pop().unwrap();
        ctx.gstate = entry.state;
        assert_eq!(ctx.gstate.fill_opacity, 0.5);
    }
}
