// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Graphics state operators: gsave, grestore, grestoreall, setlinewidth,
//! currentlinewidth, setlinecap, currentlinecap, setlinejoin, currentlinejoin,
//! setmiterlimit, currentmiterlimit, setdash, currentdash, setflat, currentflat,
//! setstrokeadjust, currentstrokeadjust, initgraphics.

use stet_core::context::Context;
use stet_core::display_list::DisplayElement;
use stet_core::error::PsError;
use stet_core::graphics_state::{DashPattern, FillRule, GraphicsState, GstateEntry, LineCap, LineJoin};
use stet_core::object::{PsObject, PsValue};

/// `gsave`: — → — (push graphics state)
pub fn op_gsave(ctx: &mut Context) -> Result<(), PsError> {
    let snapshot = ctx.gstate.clone();
    ctx.gstate_stack.push(GstateEntry {
        state: snapshot,
        saved_by_save: false,
    });
    Ok(())
}

/// `grestore`: — → — (pop graphics state)
///
/// PLRM: pops the topmost gsave-created entry. If the topmost entry was
/// created by `save`, grestore restores from it but leaves it on the stack.
pub fn op_grestore(ctx: &mut Context) -> Result<(), PsError> {
    if let Some(top) = ctx.gstate_stack.last() {
        let old_version = ctx.gstate.clip_path_version;
        if top.saved_by_save {
            // save-created entry: restore from it but don't pop
            ctx.gstate = top.state.clone();
        } else {
            // gsave-created entry: restore and pop
            let entry = ctx.gstate_stack.pop().unwrap();
            ctx.gstate = entry.state;
        }
        restore_device_clip(ctx, old_version);
    }
    Ok(())
}

/// `grestoreall`: — → — (restore to bottommost graphics state)
///
/// PLRM: repeatedly performs grestore until hitting a save-created entry,
/// restoring from it but leaving it on the stack. If no save-created entry
/// exists (unencapsulated job), restores from the bottommost entry and
/// clears the stack.
pub fn op_grestoreall(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.gstate_stack.is_empty() {
        return Ok(());
    }

    let old_version = ctx.gstate.clip_path_version;

    // Check if any save-created entry exists
    let has_save = ctx.gstate_stack.iter().any(|e| e.saved_by_save);

    if has_save {
        // Pop gsave-created entries until we hit a save-created one
        while ctx.gstate_stack.last().is_some_and(|e| !e.saved_by_save) {
            ctx.gstate_stack.pop();
        }
        // Restore from save-created entry (leave it on stack)
        if let Some(entry) = ctx.gstate_stack.last() {
            ctx.gstate = entry.state.clone();
        }
    } else {
        // No save-created entries: restore from bottommost, clear stack
        ctx.gstate = ctx.gstate_stack[0].state.clone();
        ctx.gstate_stack.clear();
    }

    restore_device_clip(ctx, old_version);
    Ok(())
}

/// Restore device clip after grestore/grestoreall/restore.
/// Skips emitting display list elements if the clip hasn't changed.
pub fn restore_device_clip(ctx: &mut Context, old_version: u32) {
    if ctx.gstate.clip_path_version == old_version {
        return; // clip unchanged, skip
    }
    ctx.display_list.push(DisplayElement::InitClip);
    if let Some(ref clip) = ctx.gstate.clip_path {
        let params = stet_core::device::ClipParams {
            fill_rule: FillRule::NonZeroWinding,
            ctm: stet_core::graphics_state::Matrix::identity(),
        };
        ctx.display_list.push(DisplayElement::Clip {
            path: clip.clone(),
            params,
        });
    }
}

/// `gstate`: — → gstate (create new gstate object from current graphics state)
pub fn op_gstate(ctx: &mut Context) -> Result<(), PsError> {
    let snapshot = ctx.gstate.clone();
    let idx = ctx.gstate_store.len() as u32;
    ctx.gstate_store.push(snapshot);
    ctx.o_stack.push(PsObject {
        value: PsValue::Gstate(idx),
        flags: stet_core::object::ObjFlags::literal(),
    })?;
    Ok(())
}

/// `currentgstate`: gstate → gstate (copy current graphics state into gstate object)
pub fn op_currentgstate(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let idx = match ctx.o_stack.peek(0)?.value {
        PsValue::Gstate(i) => i as usize,
        _ => return Err(PsError::TypeCheck),
    };
    // Replace the stored graphics state with current state
    ctx.gstate_store[idx] = ctx.gstate.clone();
    // Leave the gstate object on the stack
    Ok(())
}

/// `setgstate`: gstate → — (replace current graphics state from gstate object)
pub fn op_setgstate(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let idx = match ctx.o_stack.peek(0)?.value {
        PsValue::Gstate(i) => i as usize,
        _ => return Err(PsError::TypeCheck),
    };
    let old_version = ctx.gstate.clip_path_version;
    // Deep copy from gstate store — modifications to current state won't affect the stored gstate
    ctx.gstate = ctx.gstate_store[idx].clone();
    ctx.o_stack.pop()?;
    restore_device_clip(ctx, old_version);
    Ok(())
}

/// `setlinewidth`: num → —
pub fn op_setlinewidth(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let w = obj.as_f64().ok_or(PsError::TypeCheck)?;
    ctx.o_stack.pop()?;
    ctx.gstate.line_width = w;
    Ok(())
}

/// `currentlinewidth`: — → num
pub fn op_currentlinewidth(ctx: &mut Context) -> Result<(), PsError> {
    ctx.o_stack.push(PsObject::real(ctx.gstate.line_width))?;
    Ok(())
}

/// `setlinecap`: int → —
pub fn op_setlinecap(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let v = match obj.value {
        PsValue::Int(i) => i,
        _ => return Err(PsError::TypeCheck),
    };
    let cap = LineCap::from_i32(v).ok_or(PsError::RangeCheck)?;
    ctx.o_stack.pop()?;
    ctx.gstate.line_cap = cap;
    Ok(())
}

/// `currentlinecap`: — → int
pub fn op_currentlinecap(ctx: &mut Context) -> Result<(), PsError> {
    ctx.o_stack
        .push(PsObject::int(ctx.gstate.line_cap as i32))?;
    Ok(())
}

/// `setlinejoin`: int → —
pub fn op_setlinejoin(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let v = match obj.value {
        PsValue::Int(i) => i,
        _ => return Err(PsError::TypeCheck),
    };
    let join = LineJoin::from_i32(v).ok_or(PsError::RangeCheck)?;
    ctx.o_stack.pop()?;
    ctx.gstate.line_join = join;
    Ok(())
}

/// `currentlinejoin`: — → int
pub fn op_currentlinejoin(ctx: &mut Context) -> Result<(), PsError> {
    ctx.o_stack
        .push(PsObject::int(ctx.gstate.line_join as i32))?;
    Ok(())
}

/// `setmiterlimit`: num → —
pub fn op_setmiterlimit(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let v = obj.as_f64().ok_or(PsError::TypeCheck)?;
    if v < 1.0 {
        return Err(PsError::RangeCheck);
    }
    ctx.o_stack.pop()?;
    ctx.gstate.miter_limit = v;
    Ok(())
}

/// `currentmiterlimit`: — → num
pub fn op_currentmiterlimit(ctx: &mut Context) -> Result<(), PsError> {
    ctx.o_stack.push(PsObject::real(ctx.gstate.miter_limit))?;
    Ok(())
}

/// `setdash`: array offset → —
pub fn op_setdash(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let offset_obj = ctx.o_stack.peek(0)?;
    let arr_obj = ctx.o_stack.peek(1)?;

    let offset = offset_obj.as_f64().ok_or(PsError::TypeCheck)?;

    let (entity, start, len) = match arr_obj.value {
        PsValue::Array { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };

    // Read dash array values
    let mut dash_array = Vec::with_capacity(len as usize);
    let elems = ctx.arrays.get(entity, start, len);
    for elem in elems {
        let v = elem.as_f64().ok_or(PsError::TypeCheck)?;
        if v < 0.0 {
            return Err(PsError::RangeCheck);
        }
        dash_array.push(v);
    }

    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.gstate.dash_pattern = DashPattern {
        array: dash_array,
        offset,
    };
    Ok(())
}

/// `currentdash`: — → array offset
pub fn op_currentdash(ctx: &mut Context) -> Result<(), PsError> {
    let items: Vec<PsObject> = ctx
        .gstate
        .dash_pattern
        .array
        .iter()
        .map(|&v| PsObject::real(v))
        .collect();
    let offset = ctx.gstate.dash_pattern.offset;
    let entity = crate::vm_ops::alloc_array_from(ctx, &items);
    let arr = crate::vm_ops::make_array_obj(ctx, entity, items.len() as u32);
    ctx.o_stack.push(arr)?;
    ctx.o_stack.push(PsObject::real(offset))?;
    Ok(())
}

/// `setflat`: num → —
pub fn op_setflat(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let v = obj.as_f64().ok_or(PsError::TypeCheck)?;
    ctx.o_stack.pop()?;
    // PLRM: "If num is outside this range, the nearest valid value
    // is substituted without error indication."
    ctx.gstate.flatness = v.clamp(0.2, 100.0);
    Ok(())
}

/// `currentflat`: — → num
pub fn op_currentflat(ctx: &mut Context) -> Result<(), PsError> {
    ctx.o_stack.push(PsObject::real(ctx.gstate.flatness))?;
    Ok(())
}

/// `setstrokeadjust`: bool → —
pub fn op_setstrokeadjust(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let v = match obj.value {
        PsValue::Bool(b) => b,
        _ => return Err(PsError::TypeCheck),
    };
    ctx.o_stack.pop()?;
    ctx.gstate.stroke_adjust = v;
    Ok(())
}

/// `currentstrokeadjust`: — → bool
pub fn op_currentstrokeadjust(ctx: &mut Context) -> Result<(), PsError> {
    ctx.o_stack.push(PsObject::bool(ctx.gstate.stroke_adjust))?;
    Ok(())
}

/// `initgraphics`: — → — (reset graphics state to defaults)
///
/// Preserves the page device dict across the reset. If a page device is active,
/// recomputes CTM from its HWResolution and PageSize via `initmatrix`.
pub fn op_initgraphics(ctx: &mut Context) -> Result<(), PsError> {
    // Per PLRM: initgraphics preserves current font, page_device, and
    // device-dependent parameters. Only resets CTM, path, clip, color,
    // line width/cap/join, miter limit, and dash pattern.
    let page_device = ctx.gstate.page_device;
    let default_ctm = ctx.gstate.default_ctm;
    let current_font = ctx.gstate.current_font;
    ctx.gstate = GraphicsState::new();
    ctx.gstate.page_device = page_device;
    ctx.gstate.current_font = current_font;

    if page_device.is_some() {
        crate::matrix_ops::op_initmatrix(ctx)?;
    } else {
        ctx.gstate.ctm = default_ctm;
        ctx.gstate.default_ctm = default_ctm;
    }

    ctx.display_list.push(DisplayElement::InitClip);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use stet_core::context::Context;
    use stet_core::graphics_state::Matrix;

    fn setup() -> Context {
        let mut ctx = Context::new();
        crate::build_system_dict(&mut ctx);
        ctx
    }

    #[test]
    fn test_gsave_grestore() {
        let mut ctx = setup();
        ctx.gstate.line_width = 5.0;
        op_gsave(&mut ctx).unwrap();
        ctx.gstate.line_width = 10.0;
        assert_eq!(ctx.gstate.line_width, 10.0);
        op_grestore(&mut ctx).unwrap();
        assert_eq!(ctx.gstate.line_width, 5.0);
    }

    #[test]
    fn test_gsave_grestore_empty() {
        let mut ctx = setup();
        // grestore with no saved state: no-op
        op_grestore(&mut ctx).unwrap();
    }

    #[test]
    fn test_grestoreall() {
        let mut ctx = setup();
        ctx.gstate.line_width = 1.0;
        op_gsave(&mut ctx).unwrap();
        ctx.gstate.line_width = 5.0;
        op_gsave(&mut ctx).unwrap();
        ctx.gstate.line_width = 10.0;
        op_grestoreall(&mut ctx).unwrap();
        assert_eq!(ctx.gstate.line_width, 1.0);
        assert!(ctx.gstate_stack.is_empty());
    }

    #[test]
    fn test_setlinewidth_currentlinewidth() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(3.5)).unwrap();
        op_setlinewidth(&mut ctx).unwrap();
        op_currentlinewidth(&mut ctx).unwrap();
        let v = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        assert!((v - 3.5).abs() < 1e-10);
    }

    #[test]
    fn test_setlinecap_currentlinecap() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::int(1)).unwrap();
        op_setlinecap(&mut ctx).unwrap();
        assert_eq!(ctx.gstate.line_cap, LineCap::Round);
        op_currentlinecap(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.pop().unwrap().as_i32(), Some(1));
    }

    #[test]
    fn test_setlinecap_rangecheck() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::int(5)).unwrap();
        assert_eq!(op_setlinecap(&mut ctx), Err(PsError::RangeCheck));
    }

    #[test]
    fn test_setlinejoin_currentlinejoin() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::int(2)).unwrap();
        op_setlinejoin(&mut ctx).unwrap();
        assert_eq!(ctx.gstate.line_join, LineJoin::Bevel);
        op_currentlinejoin(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.pop().unwrap().as_i32(), Some(2));
    }

    #[test]
    fn test_setmiterlimit_currentmiterlimit() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(5.0)).unwrap();
        op_setmiterlimit(&mut ctx).unwrap();
        op_currentmiterlimit(&mut ctx).unwrap();
        let v = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        assert!((v - 5.0).abs() < 1e-10);
    }

    #[test]
    fn test_setmiterlimit_rangecheck() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(0.5)).unwrap();
        assert_eq!(op_setmiterlimit(&mut ctx), Err(PsError::RangeCheck));
    }

    #[test]
    fn test_setdash_currentdash() {
        let mut ctx = setup();
        let entity = ctx
            .arrays
            .allocate_from(&[PsObject::real(5.0), PsObject::real(3.0)]);
        ctx.o_stack.push(PsObject::array(entity, 2)).unwrap();
        ctx.o_stack.push(PsObject::real(0.0)).unwrap();
        op_setdash(&mut ctx).unwrap();
        assert_eq!(ctx.gstate.dash_pattern.array, vec![5.0, 3.0]);

        op_currentdash(&mut ctx).unwrap();
        let offset = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        assert!((offset).abs() < 1e-10);
        // dash array is on stack
        ctx.o_stack.pop().unwrap(); // array
    }

    #[test]
    fn test_setflat_currentflat() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(2.0)).unwrap();
        op_setflat(&mut ctx).unwrap();
        op_currentflat(&mut ctx).unwrap();
        let v = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        assert!((v - 2.0).abs() < 1e-10);
    }

    #[test]
    fn test_setflat_clamps_low() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(0.1)).unwrap();
        op_setflat(&mut ctx).unwrap();
        assert!((ctx.gstate.flatness - 0.2).abs() < 1e-10);
    }

    #[test]
    fn test_setflat_clamps_high() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(200.0)).unwrap();
        op_setflat(&mut ctx).unwrap();
        assert!((ctx.gstate.flatness - 100.0).abs() < 1e-10);
    }

    #[test]
    fn test_setstrokeadjust_currentstrokeadjust() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::bool(true)).unwrap();
        op_setstrokeadjust(&mut ctx).unwrap();
        assert!(ctx.gstate.stroke_adjust);
        op_currentstrokeadjust(&mut ctx).unwrap();
        assert!(matches!(
            ctx.o_stack.pop().unwrap().value,
            PsValue::Bool(true)
        ));
    }

    #[test]
    fn test_initgraphics() {
        let mut ctx = setup();
        ctx.gstate.default_ctm = Matrix::new(1.0, 0.0, 0.0, -1.0, 0.0, 792.0);
        ctx.gstate.line_width = 5.0;
        ctx.gstate.line_cap = LineCap::Round;
        op_initgraphics(&mut ctx).unwrap();
        assert_eq!(ctx.gstate.line_width, 1.0);
        assert_eq!(ctx.gstate.line_cap, LineCap::Butt);
        assert!((ctx.gstate.ctm.d - (-1.0)).abs() < 1e-10);
    }
}
