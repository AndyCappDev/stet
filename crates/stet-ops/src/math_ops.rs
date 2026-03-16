// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Arithmetic operators: add, sub, mul, div, idiv, mod, abs, neg,
//! ceiling, floor, round, truncate, sqrt, exp, ln, log, sin, cos, atan,
//! rand, srand, rrand, max, min.

use stet_core::context::Context;
use stet_core::error::PsError;
use stet_core::object::{PsObject, PsValue};

/// Helper: apply a binary numeric operation with type promotion.
#[inline]
fn binary_op(
    ctx: &mut Context,
    int_op: fn(i32, i32) -> Result<PsObject, PsError>,
    real_op: fn(f64, f64) -> Result<PsObject, PsError>,
) -> Result<(), PsError> {
    let data = ctx.o_stack.as_mut_slice();
    let len = data.len();
    if len < 2 {
        return Err(PsError::StackUnderflow);
    }
    let a = data[len - 2];
    let b = data[len - 1];

    let result = match (a.value, b.value) {
        (PsValue::Int(a), PsValue::Int(b)) => int_op(a, b)?,
        (PsValue::Real(a), PsValue::Real(b)) => real_op(a, b)?,
        (PsValue::Int(a), PsValue::Real(b)) => real_op(a as f64, b)?,
        (PsValue::Real(a), PsValue::Int(b)) => real_op(a, b as f64)?,
        _ => return Err(PsError::TypeCheck),
    };

    // Replace second-from-top with result, pop top
    data[len - 2] = result;
    ctx.o_stack.truncate(len - 1);
    Ok(())
}

/// `add`: num1 num2 → sum
pub fn op_add(ctx: &mut Context) -> Result<(), PsError> {
    binary_op(
        ctx,
        |a, b| match a.checked_add(b) {
            Some(v) => Ok(PsObject::int(v)),
            None => Ok(PsObject::real(a as f64 + b as f64)),
        },
        |a, b| Ok(PsObject::real(a + b)),
    )
}

/// `sub`: num1 num2 → difference
pub fn op_sub(ctx: &mut Context) -> Result<(), PsError> {
    binary_op(
        ctx,
        |a, b| match a.checked_sub(b) {
            Some(v) => Ok(PsObject::int(v)),
            None => Ok(PsObject::real(a as f64 - b as f64)),
        },
        |a, b| Ok(PsObject::real(a - b)),
    )
}

/// `mul`: num1 num2 → product
pub fn op_mul(ctx: &mut Context) -> Result<(), PsError> {
    binary_op(
        ctx,
        |a, b| match a.checked_mul(b) {
            Some(v) => Ok(PsObject::int(v)),
            None => Ok(PsObject::real(a as f64 * b as f64)),
        },
        |a, b| Ok(PsObject::real(a * b)),
    )
}

/// `div`: num1 num2 → quotient (always real)
pub fn op_div(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let b = ctx.o_stack.peek(0)?;
    let a = ctx.o_stack.peek(1)?;

    let bv = b.as_f64().ok_or(PsError::TypeCheck)?;
    let av = a.as_f64().ok_or(PsError::TypeCheck)?;

    if bv == 0.0 {
        return Err(PsError::UndefinedResult);
    }

    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.push(PsObject::real(av / bv))?;
    Ok(())
}

/// `idiv`: int1 int2 → quotient (integer division, truncates toward zero)
pub fn op_idiv(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let b = ctx.o_stack.peek(0)?;
    let a = ctx.o_stack.peek(1)?;

    let bv = match b.value {
        PsValue::Int(v) => v,
        _ => return Err(PsError::TypeCheck),
    };
    let av = match a.value {
        PsValue::Int(v) => v,
        _ => return Err(PsError::TypeCheck),
    };

    if bv == 0 {
        return Err(PsError::UndefinedResult);
    }

    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.push(PsObject::int(av / bv))?;
    Ok(())
}

/// `mod`: int1 int2 → remainder (sign matches dividend per PLRM)
pub fn op_mod(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let b = ctx.o_stack.peek(0)?;
    let a = ctx.o_stack.peek(1)?;

    let bv = match b.value {
        PsValue::Int(v) => v,
        _ => return Err(PsError::TypeCheck),
    };
    let av = match a.value {
        PsValue::Int(v) => v,
        _ => return Err(PsError::TypeCheck),
    };

    if bv == 0 {
        return Err(PsError::UndefinedResult);
    }

    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.push(PsObject::int(av % bv))?;
    Ok(())
}

/// `abs`: num → |num|
pub fn op_abs(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let a = ctx.o_stack.peek(0)?;
    let result = match a.value {
        PsValue::Int(v) => match v.checked_abs() {
            Some(r) => PsObject::int(r),
            None => PsObject::real((v as f64).abs()),
        },
        PsValue::Real(v) => PsObject::real(v.abs()),
        _ => return Err(PsError::TypeCheck),
    };
    ctx.o_stack.pop()?;
    ctx.o_stack.push(result)?;
    Ok(())
}

/// `neg`: num → -num
pub fn op_neg(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let a = ctx.o_stack.peek(0)?;
    let result = match a.value {
        PsValue::Int(v) => match v.checked_neg() {
            Some(r) => PsObject::int(r),
            None => PsObject::real(-(v as f64)),
        },
        PsValue::Real(v) => PsObject::real(-v),
        _ => return Err(PsError::TypeCheck),
    };
    ctx.o_stack.pop()?;
    ctx.o_stack.push(result)?;
    Ok(())
}

/// `ceiling`: num → int (smallest int >= num)
pub fn op_ceiling(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let a = ctx.o_stack.peek(0)?;
    let result = match a.value {
        PsValue::Int(_) => a,
        PsValue::Real(v) => PsObject::real(v.ceil()),
        _ => return Err(PsError::TypeCheck),
    };
    ctx.o_stack.pop()?;
    ctx.o_stack.push(result)?;
    Ok(())
}

/// `floor`: num → int (largest int <= num)
pub fn op_floor(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let a = ctx.o_stack.peek(0)?;
    let result = match a.value {
        PsValue::Int(_) => a,
        PsValue::Real(v) => PsObject::real(v.floor()),
        _ => return Err(PsError::TypeCheck),
    };
    ctx.o_stack.pop()?;
    ctx.o_stack.push(result)?;
    Ok(())
}

/// `round`: num → int (round to nearest, 0.5 rounds up)
pub fn op_round(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let a = ctx.o_stack.peek(0)?;
    let result = match a.value {
        PsValue::Int(_) => a,
        PsValue::Real(v) => PsObject::real((v + 0.5).floor()),
        _ => return Err(PsError::TypeCheck),
    };
    ctx.o_stack.pop()?;
    ctx.o_stack.push(result)?;
    Ok(())
}

/// `truncate`: num → int (truncate toward zero)
pub fn op_truncate(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let a = ctx.o_stack.peek(0)?;
    let result = match a.value {
        PsValue::Int(_) => a,
        PsValue::Real(v) => PsObject::real(v.trunc()),
        _ => return Err(PsError::TypeCheck),
    };
    ctx.o_stack.pop()?;
    ctx.o_stack.push(result)?;
    Ok(())
}

/// `sqrt`: num → sqrt(num)
pub fn op_sqrt(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let a = ctx.o_stack.peek(0)?;
    let v = a.as_f64().ok_or(PsError::TypeCheck)?;
    if v < 0.0 {
        return Err(PsError::RangeCheck);
    }
    ctx.o_stack.pop()?;
    ctx.o_stack.push(PsObject::real(v.sqrt()))?;
    Ok(())
}

/// `exp`: base exponent → base^exponent
pub fn op_exp(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let exp = ctx.o_stack.peek(0)?.as_f64().ok_or(PsError::TypeCheck)?;
    let base = ctx.o_stack.peek(1)?.as_f64().ok_or(PsError::TypeCheck)?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.push(PsObject::real(base.powf(exp)))?;
    Ok(())
}

/// `ln`: num → ln(num)
pub fn op_ln(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let v = ctx.o_stack.peek(0)?.as_f64().ok_or(PsError::TypeCheck)?;
    if v <= 0.0 {
        return Err(PsError::RangeCheck);
    }
    ctx.o_stack.pop()?;
    ctx.o_stack.push(PsObject::real(v.ln()))?;
    Ok(())
}

/// `log`: num → log10(num)
pub fn op_log(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let v = ctx.o_stack.peek(0)?.as_f64().ok_or(PsError::TypeCheck)?;
    if v <= 0.0 {
        return Err(PsError::RangeCheck);
    }
    ctx.o_stack.pop()?;
    ctx.o_stack.push(PsObject::real(v.log10()))?;
    Ok(())
}

/// `sin`: angle → sin(angle) (angle in degrees)
pub fn op_sin(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let v = ctx.o_stack.peek(0)?.as_f64().ok_or(PsError::TypeCheck)?;
    ctx.o_stack.pop()?;
    ctx.o_stack.push(PsObject::real(v.to_radians().sin()))?;
    Ok(())
}

/// `cos`: angle → cos(angle) (angle in degrees)
pub fn op_cos(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let v = ctx.o_stack.peek(0)?.as_f64().ok_or(PsError::TypeCheck)?;
    ctx.o_stack.pop()?;
    ctx.o_stack.push(PsObject::real(v.to_radians().cos()))?;
    Ok(())
}

/// `atan`: num den → angle (degrees in [0, 360))
pub fn op_atan(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let den = ctx.o_stack.peek(0)?.as_f64().ok_or(PsError::TypeCheck)?;
    let num = ctx.o_stack.peek(1)?.as_f64().ok_or(PsError::TypeCheck)?;

    if num == 0.0 && den == 0.0 {
        return Err(PsError::UndefinedResult);
    }

    let mut angle = num.atan2(den).to_degrees();
    if angle < 0.0 {
        angle += 360.0;
    }

    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.push(PsObject::real(angle))?;
    Ok(())
}

/// `rand`: → int (random integer 0..2^31-1)
pub fn op_rand(ctx: &mut Context) -> Result<(), PsError> {
    // Simple LCG RNG
    ctx.rand_state = ctx
        .rand_state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1);
    let val = ((ctx.rand_state >> 33) as i32).abs();
    ctx.o_stack.push(PsObject::int(val))?;
    Ok(())
}

/// `srand`: int → — (seed RNG)
pub fn op_srand(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let seed = match ctx.o_stack.peek(0)?.value {
        PsValue::Int(v) => v,
        _ => return Err(PsError::TypeCheck),
    };
    ctx.o_stack.pop()?;
    ctx.rand_seed = seed;
    ctx.rand_state = seed as u64;
    Ok(())
}

/// `rrand`: → int (return last seed set by srand)
pub fn op_rrand(ctx: &mut Context) -> Result<(), PsError> {
    ctx.o_stack.push(PsObject::int(ctx.rand_seed))?;
    Ok(())
}

/// `max`: num1 num2 → max(num1, num2)
pub fn op_max(ctx: &mut Context) -> Result<(), PsError> {
    binary_op(
        ctx,
        |a, b| Ok(PsObject::int(a.max(b))),
        |a, b| Ok(PsObject::real(a.max(b))),
    )
}

/// `min`: num1 num2 → min(num1, num2)
pub fn op_min(ctx: &mut Context) -> Result<(), PsError> {
    binary_op(
        ctx,
        |a, b| Ok(PsObject::int(a.min(b))),
        |a, b| Ok(PsObject::real(a.min(b))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx() -> Context {
        Context::new()
    }

    #[test]
    fn test_add_int_int() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(3)).unwrap();
        ctx.o_stack.push(PsObject::int(4)).unwrap();
        op_add(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.pop().unwrap().as_i32(), Some(7));
    }

    #[test]
    fn test_add_overflow() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(i32::MAX)).unwrap();
        ctx.o_stack.push(PsObject::int(1)).unwrap();
        op_add(&mut ctx).unwrap();
        match ctx.o_stack.pop().unwrap().value {
            PsValue::Real(v) => assert_eq!(v, i32::MAX as f64 + 1.0),
            _ => panic!("Expected Real"),
        }
    }

    #[test]
    fn test_add_int_real() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(3)).unwrap();
        ctx.o_stack.push(PsObject::real(0.5)).unwrap();
        op_add(&mut ctx).unwrap();
        match ctx.o_stack.pop().unwrap().value {
            PsValue::Real(v) => assert_eq!(v, 3.5),
            _ => panic!("Expected Real"),
        }
    }

    #[test]
    fn test_sub() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(10)).unwrap();
        ctx.o_stack.push(PsObject::int(3)).unwrap();
        op_sub(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.pop().unwrap().as_i32(), Some(7));
    }

    #[test]
    fn test_mul() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(6)).unwrap();
        ctx.o_stack.push(PsObject::int(7)).unwrap();
        op_mul(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.pop().unwrap().as_i32(), Some(42));
    }

    #[test]
    fn test_div() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(10)).unwrap();
        ctx.o_stack.push(PsObject::int(3)).unwrap();
        op_div(&mut ctx).unwrap();
        match ctx.o_stack.pop().unwrap().value {
            PsValue::Real(v) => assert!((v - 10.0 / 3.0).abs() < 1e-10),
            _ => panic!("Expected Real"),
        }
    }

    #[test]
    fn test_div_by_zero() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(10)).unwrap();
        ctx.o_stack.push(PsObject::int(0)).unwrap();
        assert_eq!(op_div(&mut ctx), Err(PsError::UndefinedResult));
    }

    #[test]
    fn test_idiv() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(7)).unwrap();
        ctx.o_stack.push(PsObject::int(2)).unwrap();
        op_idiv(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.pop().unwrap().as_i32(), Some(3));
    }

    #[test]
    fn test_mod_sign() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(-5)).unwrap();
        ctx.o_stack.push(PsObject::int(3)).unwrap();
        op_mod(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.pop().unwrap().as_i32(), Some(-2));
    }

    #[test]
    fn test_neg() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(5)).unwrap();
        op_neg(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.pop().unwrap().as_i32(), Some(-5));
    }

    #[test]
    fn test_abs() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(-5)).unwrap();
        op_abs(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.pop().unwrap().as_i32(), Some(5));
    }

    #[test]
    fn test_max_min() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(3)).unwrap();
        ctx.o_stack.push(PsObject::int(7)).unwrap();
        op_max(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.pop().unwrap().as_i32(), Some(7));

        ctx.o_stack.push(PsObject::int(3)).unwrap();
        ctx.o_stack.push(PsObject::int(7)).unwrap();
        op_min(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.pop().unwrap().as_i32(), Some(3));
    }
}
