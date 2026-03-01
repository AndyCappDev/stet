// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Composite object operators: get, put, getinterval, putinterval, length, copy.

use stet_core::context::Context;
use stet_core::dict::DictKey;
use stet_core::error::PsError;
use stet_core::object::{PsObject, PsValue};

/// Check if a composite PsObject is truly global by looking at its entity's
/// allocation status. This is authoritative — PsObject flags can lose the
/// global bit when objects are reconstructed (e.g. `currentdict`, `get`).
fn is_entity_global(ctx: &Context, obj: &PsObject) -> bool {
    match obj.value {
        PsValue::Dict(e) => ctx.dicts.entities.get(e).is_global(),
        PsValue::Array { entity, .. } => ctx.arrays.entities.get(entity).is_global(),
        PsValue::String { entity, .. } => ctx.strings.entities.get(entity).is_global(),
        _ => obj.flags.is_global(),
    }
}

/// `length`: composite → int
pub fn op_length(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let len = match obj.value {
        PsValue::Array { len, .. } | PsValue::PackedArray { len, .. } => len as i32,
        PsValue::String { len, .. } => len as i32,
        PsValue::Dict(entity) => ctx.dicts.length(entity) as i32,
        PsValue::Name(id) => ctx.names.get_bytes(id).len() as i32,
        _ => return Err(PsError::TypeCheck),
    };
    ctx.o_stack.pop()?;
    ctx.o_stack.push(PsObject::int(len))?;
    Ok(())
}

/// `get`: composite index → value
pub fn op_get(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let idx_obj = ctx.o_stack.peek(0)?;
    let coll_obj = ctx.o_stack.peek(1)?;

    match coll_obj.value {
        PsValue::Array { entity, start, len } | PsValue::PackedArray { entity, start, len } => {
            let idx = match idx_obj.value {
                PsValue::Int(v) => v,
                _ => return Err(PsError::TypeCheck),
            };
            if idx < 0 || idx as u32 >= len {
                return Err(PsError::RangeCheck);
            }
            let elem = ctx.arrays.get_element(entity, start + idx as u32);
            ctx.o_stack.pop()?;
            ctx.o_stack.pop()?;
            ctx.o_stack.push(elem)?;
        }
        PsValue::String { entity, start, len } => {
            let idx = match idx_obj.value {
                PsValue::Int(v) => v,
                _ => return Err(PsError::TypeCheck),
            };
            if idx < 0 || idx as u32 >= len {
                return Err(PsError::RangeCheck);
            }
            let byte = ctx.strings.get_byte(entity, start + idx as u32);
            ctx.o_stack.pop()?;
            ctx.o_stack.pop()?;
            ctx.o_stack.push(PsObject::int(byte as i32))?;
        }
        PsValue::Dict(dict_entity) => {
            let key = ctx.make_dict_key(&idx_obj)?;
            match ctx.dicts.get(dict_entity, &key) {
                Some(val) => {
                    ctx.o_stack.pop()?;
                    ctx.o_stack.pop()?;
                    ctx.o_stack.push(val)?;
                }
                None => return Err(PsError::Undefined),
            }
        }
        _ => return Err(PsError::TypeCheck),
    }
    Ok(())
}

/// `put`: composite index value → —
///
/// PLRM: array index any put —
///       dict key any put —
///       string index int put —
/// Errors: dictfull, invalidaccess, rangecheck, stackunderflow, typecheck
pub fn op_put(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 3 {
        return Err(PsError::StackUnderflow);
    }
    let val = ctx.o_stack.peek(0)?;
    let idx_obj = ctx.o_stack.peek(1)?;
    let coll_obj = ctx.o_stack.peek(2)?;

    // Type check: collection must be array, string, or dict
    if !matches!(
        coll_obj.value,
        PsValue::Array { .. } | PsValue::String { .. } | PsValue::Dict(_)
    ) {
        return Err(PsError::TypeCheck);
    }

    match coll_obj.value {
        PsValue::Array { entity, start, len } => {
            // Type check: index must be integer or real (real truncated to int)
            let idx = match idx_obj.value {
                PsValue::Int(v) => v,
                PsValue::Real(v) => v as i32,
                _ => return Err(PsError::TypeCheck),
            };
            // VM access check: global array cannot hold local composite.
            // Use entity-level global status (authoritative) rather than PsObject
            // flags which can lose the global bit (e.g. after currentdict).
            let coll_global = ctx.arrays.entities.get(entity).is_global();
            if coll_global && val.is_composite() && !is_entity_global(ctx, &val) {
                return Err(PsError::InvalidAccess);
            }
            if idx < 0 || idx as u32 >= len {
                return Err(PsError::RangeCheck);
            }
            ctx.o_stack.pop()?;
            ctx.o_stack.pop()?;
            ctx.o_stack.pop()?;
            ctx.cow_check_array(entity);
            ctx.arrays.set_element(entity, start + idx as u32, val);
        }
        PsValue::String { entity, start, len } => {
            // Type check: index must be integer or real (real truncated to int)
            let idx = match idx_obj.value {
                PsValue::Int(v) => v,
                PsValue::Real(v) => v as i32,
                _ => return Err(PsError::TypeCheck),
            };
            // Type check: value must be integer for string put
            let byte = match val.value {
                PsValue::Int(v) => {
                    if !(0..=255).contains(&v) {
                        return Err(PsError::RangeCheck);
                    }
                    v as u8
                }
                _ => return Err(PsError::TypeCheck),
            };
            if idx < 0 || idx as u32 >= len {
                return Err(PsError::RangeCheck);
            }
            ctx.o_stack.pop()?;
            ctx.o_stack.pop()?;
            ctx.o_stack.pop()?;
            ctx.cow_check_string(entity);
            ctx.strings.put_byte(entity, start + idx as u32, byte);
        }
        PsValue::Dict(dict_entity) => {
            // Type check: dict keys must be name, string, int, real, or bool
            if !matches!(
                idx_obj.value,
                PsValue::Name(_)
                    | PsValue::String { .. }
                    | PsValue::Int(_)
                    | PsValue::Real(_)
                    | PsValue::Bool(_)
            ) {
                return Err(PsError::TypeCheck);
            }
            // VM access check: global dict cannot hold local composite value.
            // Use entity-level global status (authoritative).
            let coll_global = ctx.dicts.entities.get(dict_entity).is_global();
            if coll_global && val.is_composite() && !is_entity_global(ctx, &val) {
                return Err(PsError::InvalidAccess);
            }
            let key = ctx.make_dict_key(&idx_obj)?;
            ctx.o_stack.pop()?;
            ctx.o_stack.pop()?;
            ctx.o_stack.pop()?;
            ctx.cow_check_dict(dict_entity);
            ctx.invalidate_name_cache();
            ctx.dicts.put(dict_entity, key, val);
        }
        _ => unreachable!(),
    }
    Ok(())
}

/// `getinterval`: composite index count → sub-composite
///
/// PLRM: array index count getinterval subarray
///       packedarray index count getinterval subarray
///       string index count getinterval substring
/// Errors: invalidaccess, rangecheck, stackunderflow, typecheck
pub fn op_getinterval(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 3 {
        return Err(PsError::StackUnderflow);
    }
    let count_obj = ctx.o_stack.peek(0)?;
    let idx_obj = ctx.o_stack.peek(1)?;
    let coll_obj = ctx.o_stack.peek(2)?;

    // Type checks first — count and index must be integers
    if !matches!(count_obj.value, PsValue::Int(_)) {
        return Err(PsError::TypeCheck);
    }
    if !matches!(idx_obj.value, PsValue::Int(_)) {
        return Err(PsError::TypeCheck);
    }
    // Collection must be array, packedarray, or string
    if !matches!(
        coll_obj.value,
        PsValue::Array { .. } | PsValue::PackedArray { .. } | PsValue::String { .. }
    ) {
        return Err(PsError::TypeCheck);
    }

    // Now safe to extract values for range checks
    let count = match count_obj.value {
        PsValue::Int(v) => {
            if v < 0 {
                return Err(PsError::RangeCheck);
            }
            v as u32
        }
        _ => unreachable!(),
    };
    let idx = match idx_obj.value {
        PsValue::Int(v) => {
            if v < 0 {
                return Err(PsError::RangeCheck);
            }
            v as u32
        }
        _ => unreachable!(),
    };

    match coll_obj.value {
        PsValue::Array { entity, start, len } | PsValue::PackedArray { entity, start, len } => {
            if idx + count > len {
                return Err(PsError::RangeCheck);
            }
            ctx.o_stack.pop()?;
            ctx.o_stack.pop()?;
            ctx.o_stack.pop()?;
            let sub = PsObject {
                value: if matches!(coll_obj.value, PsValue::PackedArray { .. }) {
                    PsValue::PackedArray {
                        entity,
                        start: start + idx,
                        len: count,
                    }
                } else {
                    PsValue::Array {
                        entity,
                        start: start + idx,
                        len: count,
                    }
                },
                flags: coll_obj.flags,
            };
            ctx.o_stack.push(sub)?;
        }
        PsValue::String { entity, start, len } => {
            if idx + count > len {
                return Err(PsError::RangeCheck);
            }
            ctx.o_stack.pop()?;
            ctx.o_stack.pop()?;
            ctx.o_stack.pop()?;
            let sub = PsObject {
                value: PsValue::String {
                    entity,
                    start: start + idx,
                    len: count,
                },
                flags: coll_obj.flags,
            };
            ctx.o_stack.push(sub)?;
        }
        _ => unreachable!(),
    }
    Ok(())
}

/// `putinterval`: composite1 index composite2 → —
///
/// PLRM: array1 index array2 putinterval —
///       array1 index packedarray2 putinterval —
///       string1 index string2 putinterval —
/// Errors: invalidaccess, rangecheck, stackunderflow, typecheck
pub fn op_putinterval(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 3 {
        return Err(PsError::StackUnderflow);
    }
    let src_obj = ctx.o_stack.peek(0)?;
    let idx_obj = ctx.o_stack.peek(1)?;
    let dest_obj = ctx.o_stack.peek(2)?;

    // Type check: source must be array, packedarray, or string
    if !matches!(
        src_obj.value,
        PsValue::Array { .. } | PsValue::PackedArray { .. } | PsValue::String { .. }
    ) {
        return Err(PsError::TypeCheck);
    }
    // Type check: dest must be array or string
    if !matches!(
        dest_obj.value,
        PsValue::Array { .. } | PsValue::String { .. }
    ) {
        return Err(PsError::TypeCheck);
    }
    // Type check: index must be integer
    if !matches!(idx_obj.value, PsValue::Int(_)) {
        return Err(PsError::TypeCheck);
    }
    // Type compatibility: array dest requires array/packedarray source, string dest requires string source
    match dest_obj.value {
        PsValue::Array { .. } => {
            if !matches!(
                src_obj.value,
                PsValue::Array { .. } | PsValue::PackedArray { .. }
            ) {
                return Err(PsError::TypeCheck);
            }
        }
        PsValue::String { .. } => {
            if !matches!(src_obj.value, PsValue::String { .. }) {
                return Err(PsError::TypeCheck);
            }
        }
        _ => unreachable!(),
    }

    let idx = match idx_obj.value {
        PsValue::Int(v) => {
            if v < 0 {
                return Err(PsError::RangeCheck);
            }
            v as u32
        }
        _ => unreachable!(),
    };

    match dest_obj.value {
        PsValue::Array {
            entity: de,
            start: ds,
            len: dl,
        } => {
            let (se, ss, sl) = match src_obj.value {
                PsValue::Array { entity, start, len }
                | PsValue::PackedArray { entity, start, len } => (entity, start, len),
                _ => unreachable!(),
            };
            if idx + sl > dl {
                return Err(PsError::RangeCheck);
            }
            // VM access check: if dest is global, all source elements must be non-composite or global
            if dest_obj.flags.is_global() {
                let src_elems = ctx.arrays.get(se, ss, sl);
                for elem in src_elems {
                    if elem.is_composite() && !elem.flags.is_global() {
                        return Err(PsError::InvalidAccess);
                    }
                }
            }
            let src_elems: Vec<PsObject> = ctx.arrays.get(se, ss, sl).to_vec();
            ctx.o_stack.pop()?;
            ctx.o_stack.pop()?;
            ctx.o_stack.pop()?;
            ctx.cow_check_array(de);
            let dest = ctx.arrays.get_mut(de, ds + idx, sl);
            dest.copy_from_slice(&src_elems);
        }
        PsValue::String {
            entity: de,
            start: dstart,
            len: dl,
        } => {
            let (se, ss, sl) = match src_obj.value {
                PsValue::String { entity, start, len } => (entity, start, len),
                _ => unreachable!(),
            };
            if idx + sl > dl {
                return Err(PsError::RangeCheck);
            }
            let src_bytes = ctx.strings.get(se, ss, sl).to_vec();
            ctx.o_stack.pop()?;
            ctx.o_stack.pop()?;
            ctx.o_stack.pop()?;
            ctx.cow_check_string(de);
            ctx.strings.get_mut(de, dstart, dl)[idx as usize..idx as usize + sl as usize]
                .copy_from_slice(&src_bytes);
        }
        _ => unreachable!(),
    }
    Ok(())
}

/// `copy` for composite objects: src dest → subsetdest
///
/// PLRM: array1 array2 copy subarray2
///       dict1 dict2 copy dict2
///       string1 string2 copy substring2
/// Errors: invalidaccess, rangecheck, stackoverflow, stackunderflow, typecheck
///
/// This is registered separately from the stack-form `copy` in stack_ops.
/// The dispatch logic in lib.rs handles both forms.
pub fn op_copy_composite(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let dest_obj = ctx.o_stack.peek(0)?;
    let src_obj = ctx.o_stack.peek(1)?;

    // Type check: packed arrays cannot be copy destination
    if matches!(dest_obj.value, PsValue::PackedArray { .. }) {
        return Err(PsError::TypeCheck);
    }

    match (src_obj.value, dest_obj.value) {
        (
            PsValue::Array {
                entity: se,
                start: ss,
                len: sl,
            }
            | PsValue::PackedArray {
                entity: se,
                start: ss,
                len: sl,
            },
            PsValue::Array {
                entity: de,
                start: ds,
                len: dl,
            },
        ) => {
            if sl > dl {
                return Err(PsError::RangeCheck);
            }
            // VM access check: if dest is global, check all source elements
            if dest_obj.flags.is_global() {
                let src_elems = ctx.arrays.get(se, ss, sl);
                for elem in src_elems {
                    if elem.is_composite() && !elem.flags.is_global() {
                        return Err(PsError::InvalidAccess);
                    }
                }
            }
            let src_elems: Vec<PsObject> = ctx.arrays.get(se, ss, sl).to_vec();
            ctx.o_stack.pop()?;
            ctx.o_stack.pop()?;
            ctx.cow_check_array(de);
            let dest = ctx.arrays.get_mut(de, ds, sl);
            dest.copy_from_slice(&src_elems);
            ctx.o_stack.push(PsObject {
                value: PsValue::Array {
                    entity: de,
                    start: ds,
                    len: sl,
                },
                flags: dest_obj.flags,
            })?;
        }
        (
            PsValue::String {
                entity: se,
                start: ss,
                len: sl,
            },
            PsValue::String {
                entity: de,
                start: ds,
                len: dl,
            },
        ) => {
            if sl > dl {
                return Err(PsError::RangeCheck);
            }
            let src_bytes = ctx.strings.get(se, ss, sl).to_vec();
            ctx.o_stack.pop()?;
            ctx.o_stack.pop()?;
            ctx.cow_check_string(de);
            ctx.strings.get_mut(de, ds, dl)[..sl as usize].copy_from_slice(&src_bytes);
            ctx.o_stack.push(PsObject {
                value: PsValue::String {
                    entity: de,
                    start: ds,
                    len: sl,
                },
                flags: dest_obj.flags,
            })?;
        }
        (PsValue::Dict(se), PsValue::Dict(de)) => {
            // VM access check: if dest dict is global, check all source values
            if dest_obj.flags.is_global() {
                let entries: Vec<PsObject> =
                    ctx.dicts.entry(se).entries.values().copied().collect();
                for val in &entries {
                    if val.is_composite() && !val.flags.is_global() {
                        return Err(PsError::InvalidAccess);
                    }
                }
            }
            let entries: Vec<(DictKey, PsObject)> = ctx
                .dicts
                .entry(se)
                .entries
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect();
            ctx.o_stack.pop()?;
            ctx.o_stack.pop()?;
            ctx.cow_check_dict(de);
            for (k, v) in entries {
                ctx.dicts.put(de, k, v);
            }
            ctx.o_stack.push(PsObject::dict(de))?;
        }
        _ => return Err(PsError::TypeCheck),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx() -> Context {
        Context::new()
    }

    #[test]
    fn test_length_array() {
        let mut ctx = test_ctx();
        let items = [PsObject::int(1), PsObject::int(2), PsObject::int(3)];
        let entity = ctx.arrays.allocate_from(&items);
        ctx.o_stack.push(PsObject::array(entity, 3)).unwrap();
        op_length(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.pop().unwrap().as_i32(), Some(3));
    }

    #[test]
    fn test_length_string() {
        let mut ctx = test_ctx();
        let entity = ctx.strings.allocate_from(b"hello");
        ctx.o_stack.push(PsObject::string(entity, 5)).unwrap();
        op_length(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.pop().unwrap().as_i32(), Some(5));
    }

    #[test]
    fn test_get_array() {
        let mut ctx = test_ctx();
        let items = [PsObject::int(10), PsObject::int(20), PsObject::int(30)];
        let entity = ctx.arrays.allocate_from(&items);
        ctx.o_stack.push(PsObject::array(entity, 3)).unwrap();
        ctx.o_stack.push(PsObject::int(1)).unwrap();
        op_get(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.pop().unwrap().as_i32(), Some(20));
    }

    #[test]
    fn test_put_array() {
        let mut ctx = test_ctx();
        let entity = ctx.arrays.allocate(3);
        ctx.o_stack.push(PsObject::array(entity, 3)).unwrap();
        ctx.o_stack.push(PsObject::int(1)).unwrap();
        ctx.o_stack.push(PsObject::int(99)).unwrap();
        op_put(&mut ctx).unwrap();
        assert_eq!(ctx.arrays.get_element(entity, 1).as_i32(), Some(99));
    }

    #[test]
    fn test_getinterval() {
        let mut ctx = test_ctx();
        let items = [
            PsObject::int(10),
            PsObject::int(20),
            PsObject::int(30),
            PsObject::int(40),
        ];
        let entity = ctx.arrays.allocate_from(&items);
        ctx.o_stack.push(PsObject::array(entity, 4)).unwrap();
        ctx.o_stack.push(PsObject::int(1)).unwrap();
        ctx.o_stack.push(PsObject::int(2)).unwrap();
        op_getinterval(&mut ctx).unwrap();
        let sub = ctx.o_stack.pop().unwrap();
        match sub.value {
            PsValue::Array { start, len, .. } => {
                assert_eq!(start, 1);
                assert_eq!(len, 2);
            }
            _ => panic!("Expected array"),
        }
    }
}
