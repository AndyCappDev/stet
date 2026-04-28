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

use stet_core::context::Context;
use stet_core::error::PsError;
use stet_core::object::{PsObject, PsValue};

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
