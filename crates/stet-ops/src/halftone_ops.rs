// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Halftone, transfer, pattern, and device operators.
//!
//! These operators store their parameters in the graphics state so that
//! the corresponding `current*` operators return what was set. Since stet
//! renders to raster devices directly, the halftone/transfer values are
//! not used during rendering, but they must be preserved for PS programs
//! that query them.

use stet_core::context::Context;
use stet_core::dict::DictKey;
use stet_core::error::PsError;
use stet_core::object::{PsObject, PsValue};

// ---------- Halftone screen operators ----------

/// `setscreen`: freq angle proc → —
pub fn op_setscreen(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 3 {
        return Err(PsError::StackUnderflow);
    }
    // Validate types: freq=num, angle=num, proc=proc/dict
    let proc_obj = ctx.o_stack.peek(0)?;
    match proc_obj.value {
        PsValue::Array { .. } | PsValue::PackedArray { .. } | PsValue::Dict(_) => {}
        _ => return Err(PsError::TypeCheck),
    }
    let angle = ctx.o_stack.peek(1)?.as_f64().ok_or(PsError::TypeCheck)?;
    let freq = ctx.o_stack.peek(2)?.as_f64().ok_or(PsError::TypeCheck)?;
    let proc_obj = ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.gstate.screen_freq = freq;
    ctx.gstate.screen_angle = angle;
    ctx.gstate.screen_proc = Some(proc_obj);
    // setscreen supersedes any halftone dictionary
    ctx.gstate.halftone = None;
    Ok(())
}

/// `currentscreen`: — → freq angle proc
pub fn op_currentscreen(ctx: &mut Context) -> Result<(), PsError> {
    // If a halftone dict was set, return its Frequency/Angle/dict
    if let Some(ht_obj) = ctx.gstate.halftone {
        if let PsValue::Dict(entity) = ht_obj.value {
            let freq_key = DictKey::Name(ctx.names.intern(b"Frequency"));
            let angle_key = DictKey::Name(ctx.names.intern(b"Angle"));
            let freq = ctx
                .dicts
                .get(entity, &freq_key)
                .and_then(|o| o.as_f64())
                .unwrap_or(ctx.gstate.screen_freq);
            let angle = ctx
                .dicts
                .get(entity, &angle_key)
                .and_then(|o| o.as_f64())
                .unwrap_or(ctx.gstate.screen_angle);
            ctx.o_stack.push(PsObject::real(freq))?;
            ctx.o_stack.push(PsObject::real(angle))?;
            ctx.o_stack.push(ht_obj)?;
            return Ok(());
        }
    }
    ctx.o_stack.push(PsObject::real(ctx.gstate.screen_freq))?;
    ctx.o_stack.push(PsObject::real(ctx.gstate.screen_angle))?;
    match ctx.gstate.screen_proc {
        Some(proc_obj) => ctx.o_stack.push(proc_obj)?,
        None => {
            let entity = ctx.arrays.allocate_from(&[]);
            ctx.o_stack.push(PsObject::procedure(entity, 0))?;
        }
    }
    Ok(())
}

/// `setcolorscreen`: freq1 angle1 proc1 ... freq4 angle4 proc4 → —
///
/// Order: red(bottom) green blue gray(top)
pub fn op_setcolorscreen(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 12 {
        return Err(PsError::StackUnderflow);
    }
    // Validate all 12 operands before popping: 4 × (freq, angle, proc)
    // Stack top-to-bottom: gray_proc gray_angle gray_freq ... red_proc red_angle red_freq
    for i in 0..4 {
        let base = i * 3;
        match ctx.o_stack.peek(base)?.value {
            PsValue::Array { .. } | PsValue::PackedArray { .. } | PsValue::Dict(_) => {}
            _ => return Err(PsError::TypeCheck),
        }
        ctx.o_stack.peek(base + 1)?.as_f64().ok_or(PsError::TypeCheck)?;
        ctx.o_stack.peek(base + 2)?.as_f64().ok_or(PsError::TypeCheck)?;
    }
    // Now pop: gray(top), blue, green, red(bottom)
    let mut components: [(f64, f64, PsObject); 4] = [(0.0, 0.0, PsObject::null()); 4];
    for i in (0..4).rev() {
        let proc_obj = ctx.o_stack.pop()?;
        let angle = ctx.o_stack.pop()?.as_f64().unwrap();
        let freq = ctx.o_stack.pop()?.as_f64().unwrap();
        components[i] = (freq, angle, proc_obj);
    }
    // Gray component also becomes the screen
    ctx.gstate.screen_freq = components[3].0;
    ctx.gstate.screen_angle = components[3].1;
    ctx.gstate.screen_proc = Some(components[3].2);
    ctx.gstate.color_screen = Some(components);
    // setcolorscreen supersedes any halftone dictionary
    ctx.gstate.halftone = None;
    Ok(())
}

/// `currentcolorscreen`: — → freq1 angle1 proc1 ... freq4 angle4 proc4
///
/// Returns: red(bottom) green blue gray(top)
pub fn op_currentcolorscreen(ctx: &mut Context) -> Result<(), PsError> {
    if let Some(cs) = ctx.gstate.color_screen {
        for (freq, angle, proc_obj) in &cs {
            ctx.o_stack.push(PsObject::real(*freq))?;
            ctx.o_stack.push(PsObject::real(*angle))?;
            ctx.o_stack.push(*proc_obj)?;
        }
    } else {
        // Default: all 4 components use current screen params
        let proc_obj = match ctx.gstate.screen_proc {
            Some(p) => p,
            None => {
                let entity = ctx.arrays.allocate_from(&[]);
                PsObject::procedure(entity, 0)
            }
        };
        for _ in 0..4 {
            ctx.o_stack.push(PsObject::real(ctx.gstate.screen_freq))?;
            ctx.o_stack.push(PsObject::real(ctx.gstate.screen_angle))?;
            ctx.o_stack.push(proc_obj)?;
        }
    }
    Ok(())
}

// ---------- Transfer function operators ----------

/// `settransfer`: proc → —
pub fn op_settransfer(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    match ctx.o_stack.peek(0)?.value {
        PsValue::Array { .. } | PsValue::PackedArray { .. } => {}
        _ => return Err(PsError::TypeCheck),
    }
    let proc_obj = ctx.o_stack.pop()?;
    ctx.gstate.transfer_function = Some(proc_obj);
    Ok(())
}

/// `currenttransfer`: — → proc
pub fn op_currenttransfer(ctx: &mut Context) -> Result<(), PsError> {
    match ctx.gstate.transfer_function {
        Some(proc_obj) => ctx.o_stack.push(proc_obj)?,
        None => {
            let entity = ctx.arrays.allocate_from(&[]);
            ctx.o_stack.push(PsObject::procedure(entity, 0))?;
        }
    }
    Ok(())
}

/// `setcolortransfer`: redproc greenproc blueproc grayproc → —
pub fn op_setcolortransfer(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 4 {
        return Err(PsError::StackUnderflow);
    }
    // Validate all 4 are procedures before popping
    for i in 0..4 {
        match ctx.o_stack.peek(i)?.value {
            PsValue::Array { .. } | PsValue::PackedArray { .. } => {}
            _ => return Err(PsError::TypeCheck),
        }
    }
    let gray = ctx.o_stack.pop()?;
    let blue = ctx.o_stack.pop()?;
    let green = ctx.o_stack.pop()?;
    let red = ctx.o_stack.pop()?;
    ctx.gstate.color_transfer = Some([red, green, blue, gray]);
    // Gray component also becomes the transfer function
    ctx.gstate.transfer_function = Some(gray);
    Ok(())
}

/// `currentcolortransfer`: — → redproc greenproc blueproc grayproc
pub fn op_currentcolortransfer(ctx: &mut Context) -> Result<(), PsError> {
    if let Some(ct) = ctx.gstate.color_transfer {
        for proc_obj in &ct {
            ctx.o_stack.push(*proc_obj)?;
        }
    } else {
        // Default: all 4 components use current transfer function
        let proc_obj = match ctx.gstate.transfer_function {
            Some(p) => p,
            None => {
                let entity = ctx.arrays.allocate_from(&[]);
                PsObject::procedure(entity, 0)
            }
        };
        for _ in 0..4 {
            ctx.o_stack.push(proc_obj)?;
        }
    }
    Ok(())
}

// ---------- Black generation / undercolor removal ----------

/// `setblackgeneration`: proc → —
pub fn op_setblackgeneration(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    match ctx.o_stack.peek(0)?.value {
        PsValue::Array { .. } | PsValue::PackedArray { .. } => {}
        _ => return Err(PsError::TypeCheck),
    }
    let proc_obj = ctx.o_stack.pop()?;
    ctx.gstate.black_generation = Some(proc_obj);
    Ok(())
}

/// `currentblackgeneration`: — → proc
pub fn op_currentblackgeneration(ctx: &mut Context) -> Result<(), PsError> {
    match ctx.gstate.black_generation {
        Some(proc_obj) => ctx.o_stack.push(proc_obj)?,
        None => {
            let entity = ctx.arrays.allocate_from(&[]);
            ctx.o_stack.push(PsObject::procedure(entity, 0))?;
        }
    }
    Ok(())
}

/// `setundercolorremoval`: proc → —
pub fn op_setundercolorremoval(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    match ctx.o_stack.peek(0)?.value {
        PsValue::Array { .. } | PsValue::PackedArray { .. } => {}
        _ => return Err(PsError::TypeCheck),
    }
    let proc_obj = ctx.o_stack.pop()?;
    ctx.gstate.undercolor_removal = Some(proc_obj);
    Ok(())
}

/// `currentundercolorremoval`: — → proc
pub fn op_currentundercolorremoval(ctx: &mut Context) -> Result<(), PsError> {
    match ctx.gstate.undercolor_removal {
        Some(proc_obj) => ctx.o_stack.push(proc_obj)?,
        None => {
            let entity = ctx.arrays.allocate_from(&[]);
            ctx.o_stack.push(PsObject::procedure(entity, 0))?;
        }
    }
    Ok(())
}

// ---------- Halftone dictionary operators ----------

/// `sethalftone`: dict → —
pub fn op_sethalftone(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    match obj.value {
        PsValue::Dict(entity) => {
            // Extract Frequency/Angle from dict to update screen params
            let freq_key = DictKey::Name(ctx.names.intern(b"Frequency"));
            let angle_key = DictKey::Name(ctx.names.intern(b"Angle"));
            if let Some(freq_obj) = ctx.dicts.get(entity, &freq_key) {
                if let Some(f) = freq_obj.as_f64() {
                    ctx.gstate.screen_freq = f;
                }
            }
            if let Some(angle_obj) = ctx.dicts.get(entity, &angle_key) {
                if let Some(a) = angle_obj.as_f64() {
                    ctx.gstate.screen_angle = a;
                }
            }
            let obj = ctx.o_stack.pop()?;
            ctx.gstate.halftone = Some(obj);
            Ok(())
        }
        _ => Err(PsError::TypeCheck),
    }
}

/// `currenthalftone`: — → dict
///
/// Returns the halftone dictionary set by `sethalftone`, or a default
/// Type 1 halftone matching `currentscreen`.
pub fn op_currenthalftone(ctx: &mut Context) -> Result<(), PsError> {
    if let Some(ht_obj) = ctx.gstate.halftone {
        ctx.o_stack.push(ht_obj)?;
    } else {
        let entity = crate::vm_ops::alloc_dict(ctx, 5, b"halftone");
        ctx.dicts.put(
            entity,
            DictKey::Name(ctx.names.intern(b"HalftoneType")),
            PsObject::int(1),
        );
        ctx.dicts.put(
            entity,
            DictKey::Name(ctx.names.intern(b"Frequency")),
            PsObject::real(ctx.gstate.screen_freq),
        );
        ctx.dicts.put(
            entity,
            DictKey::Name(ctx.names.intern(b"Angle")),
            PsObject::real(ctx.gstate.screen_angle),
        );
        let proc_obj = match ctx.gstate.screen_proc {
            Some(p) => p,
            None => {
                let proc_entity = ctx.arrays.allocate_from(&[]);
                PsObject::procedure(proc_entity, 0)
            }
        };
        ctx.dicts.put(
            entity,
            DictKey::Name(ctx.names.intern(b"SpotFunction")),
            proc_obj,
        );
        ctx.o_stack.push(PsObject::dict(entity))?;
    }
    Ok(())
}

// ---------- Color rendering ----------

/// `setcolorrendering`: dict → —
pub fn op_setcolorrendering(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    match obj.value {
        PsValue::Dict(_) => {
            let obj = ctx.o_stack.pop()?;
            ctx.gstate.color_rendering = Some(obj);
            Ok(())
        }
        _ => Err(PsError::TypeCheck),
    }
}

/// `currentcolorrendering`: — → dict
pub fn op_currentcolorrendering(ctx: &mut Context) -> Result<(), PsError> {
    match ctx.gstate.color_rendering {
        Some(obj) => ctx.o_stack.push(obj)?,
        None => {
            let entity = crate::vm_ops::alloc_dict(ctx, 1, b"colorrendering");
            ctx.o_stack.push(PsObject::dict(entity))?;
        }
    }
    Ok(())
}

// ---------- Smoothness ----------

/// `setsmoothness`: num → —
pub fn op_setsmoothness(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let val = ctx.o_stack.peek(0)?.as_f64().ok_or(PsError::TypeCheck)?;
    ctx.o_stack.pop()?;
    // Clamp to [0.0, 1.0]
    ctx.gstate.smoothness = val.clamp(0.0, 1.0);
    Ok(())
}

/// `currentsmoothness`: — → num
pub fn op_currentsmoothness(ctx: &mut Context) -> Result<(), PsError> {
    ctx.o_stack.push(PsObject::real(ctx.gstate.smoothness))?;
    Ok(())
}

// ---------- Trapping stubs (Level 3) ----------

/// `settrapparams`: dict → —
pub fn op_settrapparams(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    match ctx.o_stack.peek(0)?.value {
        PsValue::Dict(_) => {
            ctx.o_stack.pop()?;
            Ok(())
        }
        _ => Err(PsError::TypeCheck),
    }
}

/// `currenttrapparams`: — → dict
pub fn op_currenttrapparams(ctx: &mut Context) -> Result<(), PsError> {
    let entity = crate::vm_ops::alloc_dict(ctx, 1, b"trapparams");
    ctx.o_stack.push(PsObject::dict(entity))?;
    Ok(())
}

/// `settrapzone`: — → —
pub fn op_settrapzone(ctx: &mut Context) -> Result<(), PsError> {
    let _ = ctx;
    Ok(())
}

// ---------- Pattern stubs ----------

/// `makepattern`: dict matrix → dict
pub fn op_makepattern(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    ctx.o_stack.pop()?; // matrix
    // Leave dict on stack
    Ok(())
}

/// `execform`: dict → —
pub fn op_execform(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    ctx.o_stack.pop()?;
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
    fn test_setscreen_stores_values() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(120.0)).unwrap();
        ctx.o_stack.push(PsObject::real(30.0)).unwrap();
        let e = ctx.arrays.allocate_from(&[]);
        ctx.o_stack.push(PsObject::procedure(e, 0)).unwrap();
        op_setscreen(&mut ctx).unwrap();
        assert!(ctx.o_stack.is_empty());
        assert!((ctx.gstate.screen_freq - 120.0).abs() < 1e-10);
        assert!((ctx.gstate.screen_angle - 30.0).abs() < 1e-10);
    }

    #[test]
    fn test_currentscreen_returns_stored() {
        let mut ctx = setup();
        ctx.gstate.screen_freq = 90.0;
        ctx.gstate.screen_angle = 15.0;
        op_currentscreen(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.len(), 3);
        let _proc = ctx.o_stack.pop().unwrap();
        let angle = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        let freq = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        assert!((freq - 90.0).abs() < 1e-10);
        assert!((angle - 15.0).abs() < 1e-10);
    }

    #[test]
    fn test_settransfer_stores_proc() {
        let mut ctx = setup();
        let e = ctx.arrays.allocate_from(&[]);
        ctx.o_stack.push(PsObject::procedure(e, 0)).unwrap();
        op_settransfer(&mut ctx).unwrap();
        assert!(ctx.o_stack.is_empty());
        assert!(ctx.gstate.transfer_function.is_some());
    }

    #[test]
    fn test_currenttransfer_returns_stored() {
        let mut ctx = setup();
        let e = ctx.arrays.allocate_from(&[]);
        let proc_obj = PsObject::procedure(e, 0);
        ctx.gstate.transfer_function = Some(proc_obj);
        op_currenttransfer(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.len(), 1);
    }

    #[test]
    fn test_setsmoothness_clamp() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(2.0)).unwrap();
        op_setsmoothness(&mut ctx).unwrap();
        assert!((ctx.gstate.smoothness - 1.0).abs() < 1e-10);
        ctx.o_stack.push(PsObject::real(-0.5)).unwrap();
        op_setsmoothness(&mut ctx).unwrap();
        assert!((ctx.gstate.smoothness - 0.0).abs() < 1e-10);
    }
}
