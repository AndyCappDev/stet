// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Relational, boolean, and bitwise operators: eq, ne, gt, ge, lt, le,
//! and, or, xor, not, bitshift.

use stet_core::context::Context;
use stet_core::error::PsError;
use stet_core::object::{PsObject, PsValue};

/// Compare two objects for equality (cross-type numeric comparison).
fn objects_equal(ctx: &Context, a: &PsObject, b: &PsObject) -> bool {
    match (&a.value, &b.value) {
        // Numeric cross-type comparison
        (PsValue::Int(av), PsValue::Int(bv)) => av == bv,
        (PsValue::Real(av), PsValue::Real(bv)) => av == bv,
        (PsValue::Int(av), PsValue::Real(bv)) => (*av as f64) == *bv,
        (PsValue::Real(av), PsValue::Int(bv)) => *av == (*bv as f64),

        // Bool
        (PsValue::Bool(a), PsValue::Bool(b)) => a == b,

        // Name (by NameId)
        (PsValue::Name(a), PsValue::Name(b)) => a == b,

        // String — compare bytes
        (
            PsValue::String {
                entity: ea,
                start: sa,
                len: la,
            },
            PsValue::String {
                entity: eb,
                start: sb,
                len: lb,
            },
        ) => {
            if la != lb {
                return false;
            }
            ctx.strings.get(*ea, *sa, *la) == ctx.strings.get(*eb, *sb, *lb)
        }

        // Name vs String — compare bytes
        (PsValue::Name(name_id), PsValue::String { entity, start, len })
        | (PsValue::String { entity, start, len }, PsValue::Name(name_id)) => {
            let name_bytes = ctx.names.get_bytes(*name_id);
            let str_bytes = ctx.strings.get(*entity, *start, *len);
            name_bytes == str_bytes
        }

        // Null
        (PsValue::Null, PsValue::Null) => true,

        // Mark (both regular and dict marks are equal to each other)
        (PsValue::Mark | PsValue::DictMark, PsValue::Mark | PsValue::DictMark) => true,

        // Operator
        (PsValue::Operator(a), PsValue::Operator(b)) => a == b,

        // File — same entity
        (PsValue::File(a), PsValue::File(b)) => a == b,

        // Dict — same entity
        (PsValue::Dict(a), PsValue::Dict(b)) => a == b,

        // Array — same entity, start, len (identity comparison)
        (
            PsValue::Array {
                entity: ea,
                start: sa,
                len: la,
            },
            PsValue::Array {
                entity: eb,
                start: sb,
                len: lb,
            },
        ) => ea == eb && sa == sb && la == lb,

        // PackedArray — value comparison (compare contents element-by-element)
        (
            PsValue::PackedArray {
                entity: ea,
                start: sa,
                len: la,
            },
            PsValue::PackedArray {
                entity: eb,
                start: sb,
                len: lb,
            },
        ) => {
            if la != lb {
                return false;
            }
            if ea == eb && sa == sb {
                return true;
            }
            let slice_a = ctx.arrays.get(*ea, *sa, *la);
            let slice_b = ctx.arrays.get(*eb, *sb, *lb);
            slice_a
                .iter()
                .zip(slice_b.iter())
                .all(|(x, y)| objects_equal(ctx, x, y))
        }

        _ => false,
    }
}

/// Compare two objects for ordering. Returns `None` if not comparable.
fn objects_compare(
    ctx: &Context,
    a: &PsObject,
    b: &PsObject,
) -> Result<std::cmp::Ordering, PsError> {
    match (&a.value, &b.value) {
        (PsValue::Int(av), PsValue::Int(bv)) => Ok(av.cmp(bv)),
        (PsValue::Real(av), PsValue::Real(bv)) => {
            av.partial_cmp(bv).ok_or(PsError::UndefinedResult)
        }
        (PsValue::Int(av), PsValue::Real(bv)) => {
            (*av as f64).partial_cmp(bv).ok_or(PsError::UndefinedResult)
        }
        (PsValue::Real(av), PsValue::Int(bv)) => av
            .partial_cmp(&(*bv as f64))
            .ok_or(PsError::UndefinedResult),
        (
            PsValue::String {
                entity: ea,
                start: sa,
                len: la,
            },
            PsValue::String {
                entity: eb,
                start: sb,
                len: lb,
            },
        ) => {
            // Access check: both strings must be readable
            a.flags.require_read()?;
            b.flags.require_read()?;
            let bytes_a = ctx.strings.get(*ea, *sa, *la);
            let bytes_b = ctx.strings.get(*eb, *sb, *lb);
            Ok(bytes_a.cmp(bytes_b))
        }
        _ => Err(PsError::TypeCheck),
    }
}

/// `eq`: any1 any2 → bool
///
/// PLRM 3.3.1: "The access attribute of objects are not considered
/// in comparisons between objects."  Therefore `eq`/`ne` do NOT
/// perform access checks on their operands.
pub fn op_eq(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let b = ctx.o_stack.peek(0)?;
    let a = ctx.o_stack.peek(1)?;
    let result = objects_equal(ctx, &a, &b);
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.push(PsObject::bool(result))?;
    Ok(())
}

/// `ne`: any1 any2 → bool
pub fn op_ne(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let b = ctx.o_stack.peek(0)?;
    let a = ctx.o_stack.peek(1)?;
    let result = !objects_equal(ctx, &a, &b);
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.push(PsObject::bool(result))?;
    Ok(())
}

macro_rules! comparison_op {
    ($name:ident, $op:expr) => {
        pub fn $name(ctx: &mut Context) -> Result<(), PsError> {
            if ctx.o_stack.len() < 2 {
                return Err(PsError::StackUnderflow);
            }
            let b = ctx.o_stack.peek(0)?;
            let a = ctx.o_stack.peek(1)?;
            let ord = objects_compare(ctx, &a, &b)?;
            let result = $op(ord);
            ctx.o_stack.pop()?;
            ctx.o_stack.pop()?;
            ctx.o_stack.push(PsObject::bool(result))?;
            Ok(())
        }
    };
}

// gt: obj1 obj2 → bool
comparison_op!(op_gt, |ord: std::cmp::Ordering| ord
    == std::cmp::Ordering::Greater);
// ge: obj1 obj2 → bool
comparison_op!(op_ge, |ord: std::cmp::Ordering| ord
    != std::cmp::Ordering::Less);
// lt: obj1 obj2 → bool
comparison_op!(op_lt, |ord: std::cmp::Ordering| ord
    == std::cmp::Ordering::Less);
// le: obj1 obj2 → bool
comparison_op!(op_le, |ord: std::cmp::Ordering| ord
    != std::cmp::Ordering::Greater);

/// `and`: bool1 bool2 → bool; int1 int2 → int (bitwise)
pub fn op_and(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let b = ctx.o_stack.peek(0)?;
    let a = ctx.o_stack.peek(1)?;
    let result = match (a.value, b.value) {
        (PsValue::Bool(a), PsValue::Bool(b)) => PsObject::bool(a && b),
        (PsValue::Int(a), PsValue::Int(b)) => PsObject::int(a & b),
        _ => return Err(PsError::TypeCheck),
    };
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.push(result)?;
    Ok(())
}

/// `or`: bool1 bool2 → bool; int1 int2 → int (bitwise)
pub fn op_or(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let b = ctx.o_stack.peek(0)?;
    let a = ctx.o_stack.peek(1)?;
    let result = match (a.value, b.value) {
        (PsValue::Bool(a), PsValue::Bool(b)) => PsObject::bool(a || b),
        (PsValue::Int(a), PsValue::Int(b)) => PsObject::int(a | b),
        _ => return Err(PsError::TypeCheck),
    };
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.push(result)?;
    Ok(())
}

/// `xor`: bool1 bool2 → bool; int1 int2 → int (bitwise)
pub fn op_xor(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let b = ctx.o_stack.peek(0)?;
    let a = ctx.o_stack.peek(1)?;
    let result = match (a.value, b.value) {
        (PsValue::Bool(a), PsValue::Bool(b)) => PsObject::bool(a ^ b),
        (PsValue::Int(a), PsValue::Int(b)) => PsObject::int(a ^ b),
        _ => return Err(PsError::TypeCheck),
    };
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.push(result)?;
    Ok(())
}

/// `not`: bool → bool; int → int (bitwise complement)
pub fn op_not(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let a = ctx.o_stack.peek(0)?;
    let result = match a.value {
        PsValue::Bool(v) => PsObject::bool(!v),
        PsValue::Int(v) => PsObject::int(!v),
        _ => return Err(PsError::TypeCheck),
    };
    ctx.o_stack.pop()?;
    ctx.o_stack.push(result)?;
    Ok(())
}

/// `bitshift`: int shift → int
pub fn op_bitshift(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let shift = ctx.o_stack.peek(0)?;
    let val = ctx.o_stack.peek(1)?;

    let shift_amount = match shift.value {
        PsValue::Int(v) => v,
        _ => return Err(PsError::TypeCheck),
    };
    let value = match val.value {
        PsValue::Int(v) => v,
        _ => return Err(PsError::TypeCheck),
    };

    let result = if shift_amount > 0 {
        if shift_amount >= 32 {
            0
        } else {
            value.wrapping_shl(shift_amount as u32)
        }
    } else if shift_amount < 0 {
        let abs_shift = (-shift_amount) as u32;
        if abs_shift >= 32 {
            0
        } else {
            // Logical right shift (unsigned)
            ((value as u32) >> abs_shift) as i32
        }
    } else {
        value
    };

    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.push(PsObject::int(result))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx() -> Context {
        Context::new()
    }

    #[test]
    fn test_eq_int() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(7)).unwrap();
        ctx.o_stack.push(PsObject::int(7)).unwrap();
        op_eq(&mut ctx).unwrap();
        assert!(matches!(
            ctx.o_stack.pop().unwrap().value,
            PsValue::Bool(true)
        ));
    }

    #[test]
    fn test_eq_cross_type() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(4)).unwrap();
        ctx.o_stack.push(PsObject::real(4.0)).unwrap();
        op_eq(&mut ctx).unwrap();
        assert!(matches!(
            ctx.o_stack.pop().unwrap().value,
            PsValue::Bool(true)
        ));
    }

    #[test]
    fn test_ne() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(3)).unwrap();
        ctx.o_stack.push(PsObject::int(4)).unwrap();
        op_ne(&mut ctx).unwrap();
        assert!(matches!(
            ctx.o_stack.pop().unwrap().value,
            PsValue::Bool(true)
        ));
    }

    #[test]
    fn test_gt() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(5)).unwrap();
        ctx.o_stack.push(PsObject::int(3)).unwrap();
        op_gt(&mut ctx).unwrap();
        assert!(matches!(
            ctx.o_stack.pop().unwrap().value,
            PsValue::Bool(true)
        ));
    }

    #[test]
    fn test_and_bool() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::bool(true)).unwrap();
        ctx.o_stack.push(PsObject::bool(false)).unwrap();
        op_and(&mut ctx).unwrap();
        assert!(matches!(
            ctx.o_stack.pop().unwrap().value,
            PsValue::Bool(false)
        ));
    }

    #[test]
    fn test_and_int() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(0xFF)).unwrap();
        ctx.o_stack.push(PsObject::int(0x0F)).unwrap();
        op_and(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.pop().unwrap().as_i32(), Some(0x0F));
    }

    #[test]
    fn test_not_bool() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::bool(true)).unwrap();
        op_not(&mut ctx).unwrap();
        assert!(matches!(
            ctx.o_stack.pop().unwrap().value,
            PsValue::Bool(false)
        ));
    }

    #[test]
    fn test_bitshift() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(7)).unwrap();
        ctx.o_stack.push(PsObject::int(3)).unwrap();
        op_bitshift(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.pop().unwrap().as_i32(), Some(56)); // 7 << 3

        ctx.o_stack.push(PsObject::int(56)).unwrap();
        ctx.o_stack.push(PsObject::int(-3)).unwrap();
        op_bitshift(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.pop().unwrap().as_i32(), Some(7)); // 56 >> 3
    }
}
