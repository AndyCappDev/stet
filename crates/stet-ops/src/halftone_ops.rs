// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Halftone, transfer, pattern, and device stubs.
//!
//! These operators consume their operands and return sensible defaults,
//! preventing `undefined` errors in real-world PostScript files without
//! affecting rendering quality.

use stet_core::context::Context;
use stet_core::error::PsError;
use stet_core::object::{PsObject, PsValue};

// ---------- Halftone screen operators ----------

/// `setscreen`: freq angle proc → —
pub fn op_setscreen(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 3 {
        return Err(PsError::StackUnderflow);
    }
    ctx.o_stack.pop()?; // proc
    ctx.o_stack.pop()?; // angle
    ctx.o_stack.pop()?; // freq
    Ok(())
}

/// `currentscreen`: — → freq angle proc
pub fn op_currentscreen(ctx: &mut Context) -> Result<(), PsError> {
    ctx.o_stack.push(PsObject::real(60.0))?; // freq
    ctx.o_stack.push(PsObject::real(45.0))?; // angle
    // Empty procedure
    let entity = ctx.arrays.allocate_from(&[]);
    ctx.o_stack.push(PsObject::procedure(entity, 0))?;
    Ok(())
}

/// `setcolorscreen`: freq1 angle1 proc1 ... freq4 angle4 proc4 → —
pub fn op_setcolorscreen(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 12 {
        return Err(PsError::StackUnderflow);
    }
    for _ in 0..12 {
        ctx.o_stack.pop()?;
    }
    Ok(())
}

/// `currentcolorscreen`: — → freq1 angle1 proc1 ... freq4 angle4 proc4
pub fn op_currentcolorscreen(ctx: &mut Context) -> Result<(), PsError> {
    let entity = ctx.arrays.allocate_from(&[]);
    for _ in 0..4 {
        ctx.o_stack.push(PsObject::real(60.0))?;
        ctx.o_stack.push(PsObject::real(45.0))?;
        ctx.o_stack.push(PsObject::procedure(entity, 0))?;
    }
    Ok(())
}

// ---------- Transfer function operators ----------

/// `settransfer`: proc → —
pub fn op_settransfer(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    ctx.o_stack.pop()?;
    Ok(())
}

/// `currenttransfer`: — → proc
pub fn op_currenttransfer(ctx: &mut Context) -> Result<(), PsError> {
    let entity = ctx.arrays.allocate_from(&[]);
    ctx.o_stack.push(PsObject::procedure(entity, 0))?;
    Ok(())
}

/// `setcolortransfer`: redproc greenproc blueproc grayproc → —
pub fn op_setcolortransfer(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 4 {
        return Err(PsError::StackUnderflow);
    }
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    Ok(())
}

/// `currentcolortransfer`: — → redproc greenproc blueproc grayproc
pub fn op_currentcolortransfer(ctx: &mut Context) -> Result<(), PsError> {
    let entity = ctx.arrays.allocate_from(&[]);
    for _ in 0..4 {
        ctx.o_stack.push(PsObject::procedure(entity, 0))?;
    }
    Ok(())
}

// ---------- Black generation / undercolor removal ----------

/// `setblackgeneration`: proc → —
pub fn op_setblackgeneration(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    ctx.o_stack.pop()?;
    Ok(())
}

/// `currentblackgeneration`: — → proc
pub fn op_currentblackgeneration(ctx: &mut Context) -> Result<(), PsError> {
    let entity = ctx.arrays.allocate_from(&[]);
    ctx.o_stack.push(PsObject::procedure(entity, 0))?;
    Ok(())
}

/// `setundercolorremoval`: proc → —
pub fn op_setundercolorremoval(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    ctx.o_stack.pop()?;
    Ok(())
}

/// `currentundercolorremoval`: — → proc
pub fn op_currentundercolorremoval(ctx: &mut Context) -> Result<(), PsError> {
    let entity = ctx.arrays.allocate_from(&[]);
    ctx.o_stack.push(PsObject::procedure(entity, 0))?;
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
        PsValue::Dict(_) => {
            ctx.o_stack.pop()?;
            Ok(())
        }
        _ => Err(PsError::TypeCheck),
    }
}

/// `currenthalftone`: — → dict
pub fn op_currenthalftone(ctx: &mut Context) -> Result<(), PsError> {
    let entity = crate::vm_ops::alloc_dict(ctx, 0, b"halftone");
    ctx.o_stack.push(PsObject::dict(entity))?;
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
    fn test_setscreen_pops_3() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(60.0)).unwrap();
        ctx.o_stack.push(PsObject::real(45.0)).unwrap();
        let e = ctx.arrays.allocate_from(&[]);
        ctx.o_stack.push(PsObject::procedure(e, 0)).unwrap();
        op_setscreen(&mut ctx).unwrap();
        assert!(ctx.o_stack.is_empty());
    }

    #[test]
    fn test_currentscreen_pushes_3() {
        let mut ctx = setup();
        op_currentscreen(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.len(), 3);
        let _proc = ctx.o_stack.pop().unwrap();
        let angle = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        let freq = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        assert!((freq - 60.0).abs() < 1e-10);
        assert!((angle - 45.0).abs() < 1e-10);
    }

    #[test]
    fn test_settransfer_pops_1() {
        let mut ctx = setup();
        let e = ctx.arrays.allocate_from(&[]);
        ctx.o_stack.push(PsObject::procedure(e, 0)).unwrap();
        op_settransfer(&mut ctx).unwrap();
        assert!(ctx.o_stack.is_empty());
    }

    #[test]
    fn test_currenttransfer_pushes_1() {
        let mut ctx = setup();
        op_currenttransfer(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.len(), 1);
    }
}
