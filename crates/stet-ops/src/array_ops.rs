// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Array operators: array, aload, astore, ] (array_from_mark).

use stet_core::context::Context;
use stet_core::error::PsError;
use stet_core::object::{PsObject, PsValue};

/// `array`: int → array (create array of given length, filled with null)
pub fn op_array(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let size = ctx.o_stack.peek(0)?;
    let len = match size.value {
        PsValue::Int(v) => {
            if v < 0 {
                return Err(PsError::RangeCheck);
            }
            v as usize
        }
        _ => return Err(PsError::TypeCheck),
    };
    ctx.o_stack.pop()?;
    let entity = crate::vm_ops::alloc_array(ctx, len);
    let obj = crate::vm_ops::make_array_obj(ctx, entity, len as u32);
    ctx.o_stack.push(obj)?;
    Ok(())
}

/// `aload`: array → obj0 obj1 ... objN-1 array
pub fn op_aload(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let arr = ctx.o_stack.peek(0)?;
    let (entity, start, len) = match arr.value {
        PsValue::Array { entity, start, len } | PsValue::PackedArray { entity, start, len } => {
            (entity, start, len)
        }
        _ => return Err(PsError::TypeCheck),
    };

    let elements: Vec<PsObject> = ctx.arrays.get(entity, start, len).to_vec();
    ctx.o_stack.pop()?;

    for elem in elements {
        ctx.o_stack.push(elem)?;
    }
    ctx.o_stack.push(arr)?;
    Ok(())
}

/// `astore`: obj0 ... objN-1 array → array
///
/// PLRM: any0 ... anyn-1 array astore array
/// Errors: invalidaccess, stackunderflow, typecheck
pub fn op_astore(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let arr = ctx.o_stack.peek(0)?;
    let (entity, start, len) = match arr.value {
        PsValue::Array { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };

    if ctx.o_stack.len() < (len as usize) + 1 {
        return Err(PsError::StackUnderflow);
    }

    // VM access check: if array is global, all elements must be non-composite or global
    // Use entity tag bit (authoritative) rather than PsObject flags
    if entity.is_global() {
        let slice = ctx.o_stack.as_slice();
        // Elements are below the array on the stack: slice[..slice.len()-1] top-to-bottom
        // The array is at the top (index slice.len()-1), elements are below it
        let arr_idx = slice.len() - 1;
        for i in 0..len as usize {
            let elem = &slice[arr_idx - 1 - i];
            if elem.is_composite() && !elem.is_global_vm() {
                return Err(PsError::InvalidAccess);
            }
        }
    }

    ctx.o_stack.pop()?; // array

    // Pop elements in reverse order
    let mut elements = Vec::with_capacity(len as usize);
    for _ in 0..len {
        elements.push(ctx.o_stack.pop()?);
    }
    elements.reverse();

    // Store into array
    ctx.cow_check_array(entity);
    let dest = ctx.arrays.get_mut(entity, start, len);
    dest.copy_from_slice(&elements);

    ctx.o_stack.push(arr)?;
    Ok(())
}

/// `]`: mark obj0 ... objN-1 → array (build array from mark)
pub fn op_array_from_mark(ctx: &mut Context) -> Result<(), PsError> {
    // Find the mark
    let slice = ctx.o_stack.as_slice();
    let mut mark_pos = None;
    for (i, obj) in slice.iter().enumerate().rev() {
        if matches!(obj.value, PsValue::Mark) {
            mark_pos = Some(i);
            break;
        }
    }

    let mark_pos = mark_pos.ok_or(PsError::UnmatchedMark)?;
    let n_elements = slice.len() - mark_pos - 1;

    // Collect elements above the mark
    let elements: Vec<PsObject> = slice[mark_pos + 1..].to_vec();

    // VM access check: if allocating in global mode, all elements must be non-composite or global
    if ctx.vm_alloc_mode {
        for elem in &elements {
            if elem.is_composite() && !elem.is_global_vm() {
                return Err(PsError::InvalidAccess);
            }
        }
    }

    // Pop all elements + mark
    ctx.o_stack.truncate(mark_pos);

    let entity = crate::vm_ops::alloc_array(ctx, n_elements);
    let dest = ctx.arrays.get_mut(entity, 0, n_elements as u32);
    dest.copy_from_slice(&elements);
    let obj = crate::vm_ops::make_array_obj(ctx, entity, n_elements as u32);
    ctx.o_stack.push(obj)?;
    Ok(())
}

/// `>>`: mark key value ... → dict (build dict from mark)
pub fn op_dict_from_mark(ctx: &mut Context) -> Result<(), PsError> {
    let slice = ctx.o_stack.as_slice();
    let mut mark_pos = None;
    for (i, obj) in slice.iter().enumerate().rev() {
        if matches!(obj.value, PsValue::Mark | PsValue::DictMark) {
            mark_pos = Some(i);
            break;
        }
    }

    let mark_pos = mark_pos.ok_or(PsError::UnmatchedMark)?;
    let n_elements = slice.len() - mark_pos - 1;

    if !n_elements.is_multiple_of(2) {
        return Err(PsError::RangeCheck);
    }

    let pairs: Vec<PsObject> = slice[mark_pos + 1..].to_vec();

    // VM access check: if allocating in global mode, all values must be non-composite or global
    if ctx.vm_alloc_mode {
        for chunk in pairs.chunks(2) {
            if chunk[1].is_composite() && !chunk[1].is_global_vm() {
                return Err(PsError::InvalidAccess);
            }
        }
    }

    ctx.o_stack.truncate(mark_pos);

    let dict_entity = crate::vm_ops::alloc_dict(ctx, n_elements / 2, b"");

    for chunk in pairs.chunks(2) {
        let key = ctx.make_dict_key(&chunk[0])?;
        ctx.dicts.put(dict_entity, key, chunk[1]);
    }

    ctx.o_stack.push(PsObject::dict(dict_entity))?;
    Ok(())
}

/// `reverse`: array → array (reverse elements in place)
///
/// PostForge extension. Reverses the elements of an array or string in place.
pub fn op_reverse(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    match obj.value {
        PsValue::Array { entity, start, len } => {
            let slice = ctx.arrays.get_mut(entity, start, len);
            slice.reverse();
        }
        PsValue::String { entity, start, len } => {
            let slice = ctx.strings.get_mut(entity, start, len);
            slice.reverse();
        }
        _ => return Err(PsError::TypeCheck),
    }
    Ok(())
}

/// `printarray`: array → (print array contents to stdout)
///
/// PostForge extension. Prints the array in PostScript notation: [elem1 elem2 ...]
pub fn op_printarray(ctx: &mut Context) -> Result<(), PsError> {
    use std::io::Write;

    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let (entity, start, len) = match obj.value {
        PsValue::Array { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };
    ctx.o_stack.pop()?;

    let _ = write!(ctx.stdout, "[");
    let elements: Vec<PsObject> = ctx.arrays.get(entity, start, len).to_vec();
    for (i, elem) in elements.iter().enumerate() {
        if i > 0 {
            let _ = write!(ctx.stdout, " ");
        }
        match elem.value {
            PsValue::Int(n) => {
                let _ = write!(ctx.stdout, "{}", n);
            }
            PsValue::Real(f) => {
                let _ = write!(ctx.stdout, "{}", f);
            }
            PsValue::Bool(b) => {
                let _ = write!(ctx.stdout, "{}", b);
            }
            PsValue::Null => {
                let _ = write!(ctx.stdout, "null");
            }
            PsValue::Name(id) => {
                let name = ctx.names.get_bytes(id);
                if elem.flags.is_executable() {
                    let _ = ctx.stdout.write_all(name);
                } else {
                    let _ = write!(ctx.stdout, "/");
                    let _ = ctx.stdout.write_all(name);
                }
            }
            PsValue::String { entity, start, len } => {
                let bytes = ctx.strings.get(entity, start, len);
                let _ = write!(ctx.stdout, "(");
                let _ = ctx.stdout.write_all(bytes);
                let _ = write!(ctx.stdout, ")");
            }
            PsValue::Array { .. } => {
                // Nested arrays — just show type
                if elem.flags.is_executable() {
                    let _ = write!(ctx.stdout, "{{...}}");
                } else {
                    let _ = write!(ctx.stdout, "[...]");
                }
            }
            PsValue::Mark | PsValue::DictMark => {
                let _ = write!(ctx.stdout, "-mark-");
            }
            _ => {
                let _ = write!(ctx.stdout, "-");
                let _ = ctx.stdout.write_all(elem.type_name());
                let _ = write!(ctx.stdout, "-");
            }
        }
    }
    let _ = write!(ctx.stdout, "]");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx() -> Context {
        Context::new()
    }

    #[test]
    fn test_array_create() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(5)).unwrap();
        op_array(&mut ctx).unwrap();
        let arr = ctx.o_stack.pop().unwrap();
        match arr.value {
            PsValue::Array { len, .. } => assert_eq!(len, 5),
            _ => panic!("Expected array"),
        }
    }

    #[test]
    fn test_array_from_mark() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::int(1)).unwrap();
        ctx.o_stack.push(PsObject::int(2)).unwrap();
        ctx.o_stack.push(PsObject::int(3)).unwrap();
        op_array_from_mark(&mut ctx).unwrap();
        let arr = ctx.o_stack.pop().unwrap();
        match arr.value {
            PsValue::Array { entity, start, len } => {
                assert_eq!(len, 3);
                assert_eq!(ctx.arrays.get_element(entity, start).as_i32(), Some(1));
                assert_eq!(ctx.arrays.get_element(entity, start + 2).as_i32(), Some(3));
            }
            _ => panic!("Expected array"),
        }
    }

    #[test]
    fn test_aload() {
        let mut ctx = test_ctx();
        let items = [PsObject::int(10), PsObject::int(20)];
        let entity = ctx.arrays.allocate_from(&items);
        ctx.o_stack.push(PsObject::array(entity, 2)).unwrap();
        op_aload(&mut ctx).unwrap();
        // Stack: 10 20 array
        assert_eq!(ctx.o_stack.len(), 3);
        let arr = ctx.o_stack.pop().unwrap();
        assert!(matches!(arr.value, PsValue::Array { .. }));
        assert_eq!(ctx.o_stack.pop().unwrap().as_i32(), Some(20));
        assert_eq!(ctx.o_stack.pop().unwrap().as_i32(), Some(10));
    }
}
