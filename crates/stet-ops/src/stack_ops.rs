// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Stack operators: pop, dup, exch, copy, index, roll, clear, count,
//! mark, cleartomark, counttomark.

use stet_core::context::Context;
use stet_core::error::PsError;
use stet_core::object::{PsObject, PsValue};

/// `pop`: obj → —
pub fn op_pop(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    ctx.o_stack.pop()?;
    Ok(())
}

/// `dup`: obj → obj obj
pub fn op_dup(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    ctx.o_stack.push(obj)?;
    Ok(())
}

/// `exch`: obj1 obj2 → obj2 obj1
pub fn op_exch(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    ctx.o_stack.swap_top_two()?;
    Ok(())
}

/// `copy`: int copy — copies top n elements; OR composite copy (handled in composite_ops)
/// This handles the integer (stack) form only.
pub fn op_copy(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }

    let top = ctx.o_stack.peek(0)?;

    match top.value {
        PsValue::Int(n) => {
            if n < 0 {
                return Err(PsError::RangeCheck);
            }
            let n = n as usize;
            if ctx.o_stack.len() < n + 1 {
                return Err(PsError::StackOverflow);
            }
            ctx.o_stack.pop()?; // remove the count

            // Copy top n elements
            let base = ctx.o_stack.len() - n;
            let copies: Vec<PsObject> = ctx.o_stack.as_slice()[base..].to_vec();
            for obj in copies {
                ctx.o_stack.push(obj)?;
            }
            Ok(())
        }
        _ => {
            // Composite copy — delegate. For Phase 1, we only handle int form here.
            // The composite form is handled by the composite_ops module via a separate
            // registration if needed. For now, typecheck.
            Err(PsError::TypeCheck)
        }
    }
}

/// `index`: n index → obj_n
pub fn op_index(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let top = ctx.o_stack.peek(0)?;
    let n = match top.value {
        PsValue::Int(v) => {
            if v < 0 {
                return Err(PsError::RangeCheck);
            }
            v as usize
        }
        _ => return Err(PsError::TypeCheck),
    };
    // n must index within the elements below the index argument
    // (len - 1 elements below, so n must be <= len - 2)
    if ctx.o_stack.len() < 2 || n > ctx.o_stack.len() - 2 {
        return Err(PsError::RangeCheck);
    }
    ctx.o_stack.pop()?; // remove index
    let obj = ctx.o_stack.peek(n)?;
    ctx.o_stack.push(obj)?;
    Ok(())
}

/// `roll`: n_elements j roll — rotate top n elements by j positions
pub fn op_roll(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }

    let j_obj = ctx.o_stack.peek(0)?;
    let n_obj = ctx.o_stack.peek(1)?;

    let n = match n_obj.value {
        PsValue::Int(v) => {
            if v < 0 {
                return Err(PsError::RangeCheck);
            }
            v as usize
        }
        _ => return Err(PsError::TypeCheck),
    };
    let j = match j_obj.value {
        PsValue::Int(v) => v,
        _ => return Err(PsError::TypeCheck),
    };

    if ctx.o_stack.len() < n + 2 {
        return Err(PsError::RangeCheck);
    }

    ctx.o_stack.pop()?; // j
    ctx.o_stack.pop()?; // n

    if n == 0 {
        return Ok(());
    }

    let j = ((j % n as i32) + n as i32) as usize % n;
    if j == 0 {
        return Ok(());
    }

    let len = ctx.o_stack.len();
    let slice = &mut ctx.o_stack.as_mut_slice()[len - n..];
    slice.rotate_right(j);

    Ok(())
}

/// `clear`: — clear all operand stack
pub fn op_clear(ctx: &mut Context) -> Result<(), PsError> {
    ctx.o_stack.clear();
    Ok(())
}

/// `count`: → n
pub fn op_count(ctx: &mut Context) -> Result<(), PsError> {
    let n = ctx.o_stack.len() as i32;
    ctx.o_stack.push(PsObject::int(n))?;
    Ok(())
}

/// `mark`: → mark
pub fn op_mark(ctx: &mut Context) -> Result<(), PsError> {
    ctx.o_stack.push(PsObject::mark())?;
    Ok(())
}

/// `cleartomark`: mark obj1 ... objn → —
pub fn op_cleartomark(ctx: &mut Context) -> Result<(), PsError> {
    // First validate: find the mark without popping
    let slice = ctx.o_stack.as_slice();
    let mut mark_pos = None;
    for (i, obj) in slice.iter().enumerate().rev() {
        if matches!(obj.value, PsValue::Mark | PsValue::DictMark) {
            mark_pos = Some(i);
            break;
        }
    }
    match mark_pos {
        Some(pos) => {
            // Pop everything from the mark upward
            ctx.o_stack.truncate(pos);
            Ok(())
        }
        None => Err(PsError::UnmatchedMark),
    }
}

/// `counttomark`: mark obj1 ... objn → mark obj1 ... objn n
pub fn op_counttomark(ctx: &mut Context) -> Result<(), PsError> {
    let slice = ctx.o_stack.as_slice();
    for (i, obj) in slice.iter().rev().enumerate() {
        if matches!(obj.value, PsValue::Mark | PsValue::DictMark) {
            ctx.o_stack.push(PsObject::int(i as i32))?;
            return Ok(());
        }
    }
    Err(PsError::UnmatchedMark)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx() -> Context {
        Context::new()
    }

    #[test]
    fn test_pop() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(1)).unwrap();
        op_pop(&mut ctx).unwrap();
        assert!(ctx.o_stack.is_empty());
    }

    #[test]
    fn test_dup() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(5)).unwrap();
        op_dup(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.len(), 2);
        assert_eq!(ctx.o_stack.peek(0).unwrap().as_i32(), Some(5));
        assert_eq!(ctx.o_stack.peek(1).unwrap().as_i32(), Some(5));
    }

    #[test]
    fn test_exch() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(1)).unwrap();
        ctx.o_stack.push(PsObject::int(2)).unwrap();
        op_exch(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.peek(0).unwrap().as_i32(), Some(1));
        assert_eq!(ctx.o_stack.peek(1).unwrap().as_i32(), Some(2));
    }

    #[test]
    fn test_copy_stack() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(1)).unwrap();
        ctx.o_stack.push(PsObject::int(2)).unwrap();
        ctx.o_stack.push(PsObject::int(3)).unwrap();
        ctx.o_stack.push(PsObject::int(2)).unwrap(); // copy 2
        op_copy(&mut ctx).unwrap();
        // Stack: 1 2 3 2 3
        assert_eq!(ctx.o_stack.len(), 5);
        assert_eq!(ctx.o_stack.peek(0).unwrap().as_i32(), Some(3));
        assert_eq!(ctx.o_stack.peek(1).unwrap().as_i32(), Some(2));
    }

    #[test]
    fn test_index() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(10)).unwrap();
        ctx.o_stack.push(PsObject::int(20)).unwrap();
        ctx.o_stack.push(PsObject::int(30)).unwrap();
        ctx.o_stack.push(PsObject::int(1)).unwrap(); // index 1 → 20
        op_index(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.peek(0).unwrap().as_i32(), Some(20));
    }

    #[test]
    fn test_roll() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(1)).unwrap();
        ctx.o_stack.push(PsObject::int(2)).unwrap();
        ctx.o_stack.push(PsObject::int(3)).unwrap();
        ctx.o_stack.push(PsObject::int(3)).unwrap(); // n=3
        ctx.o_stack.push(PsObject::int(1)).unwrap(); // j=1
        op_roll(&mut ctx).unwrap();
        // 1 2 3 rolled by 1 → 3 1 2
        assert_eq!(ctx.o_stack.peek(0).unwrap().as_i32(), Some(2));
        assert_eq!(ctx.o_stack.peek(1).unwrap().as_i32(), Some(1));
        assert_eq!(ctx.o_stack.peek(2).unwrap().as_i32(), Some(3));
    }

    #[test]
    fn test_counttomark() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::int(1)).unwrap();
        ctx.o_stack.push(PsObject::int(2)).unwrap();
        op_counttomark(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.peek(0).unwrap().as_i32(), Some(2));
    }

    #[test]
    fn test_cleartomark() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(99)).unwrap();
        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::int(1)).unwrap();
        ctx.o_stack.push(PsObject::int(2)).unwrap();
        op_cleartomark(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.len(), 1);
        assert_eq!(ctx.o_stack.peek(0).unwrap().as_i32(), Some(99));
    }
}
