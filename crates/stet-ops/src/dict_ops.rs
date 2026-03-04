// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Dictionary operators: dict, begin, end, def, load, store, known, where,
//! maxlength, currentdict, countdictstack, dictstack, undef, cleardictstack.

use stet_core::context::Context;
use stet_core::error::PsError;
use stet_core::object::{ObjFlags, PsObject, PsValue};

/// `dict`: int → dict (create new dict with capacity)
pub fn op_dict(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let cap = ctx.o_stack.peek(0)?;
    let max_len = match cap.value {
        PsValue::Int(v) => {
            if v < 0 {
                return Err(PsError::RangeCheck);
            }
            v as usize
        }
        _ => return Err(PsError::TypeCheck),
    };
    ctx.o_stack.pop()?;
    let entity = crate::vm_ops::alloc_dict(ctx, max_len, b"");
    ctx.o_stack
        .push(crate::vm_ops::make_dict_obj(ctx, entity))?;
    Ok(())
}

/// `begin`: dict → — (push dict onto dict stack)
pub fn op_begin(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let entity = match obj.value {
        PsValue::Dict(e) => e,
        _ => return Err(PsError::TypeCheck),
    };
    if ctx.d_stack.len() >= 250 {
        return Err(PsError::DictStackOverflow);
    }
    ctx.o_stack.pop()?;
    ctx.d_stack.push(entity);
    ctx.invalidate_name_cache();
    Ok(())
}

/// `end`: — (pop current dict from dict stack)
pub fn op_end(ctx: &mut Context) -> Result<(), PsError> {
    // Must keep at least 3 permanent dicts (systemdict, globaldict, userdict)
    if ctx.d_stack.len() <= 3 {
        return Err(PsError::DictStackUnderflow);
    }
    ctx.d_stack.pop();
    ctx.invalidate_name_cache();
    Ok(())
}

/// `def`: key value → — (define in current dict)
pub fn op_def(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let val = ctx.o_stack.peek(0)?;
    let key_obj = ctx.o_stack.peek(1)?;

    // Access checks (skipped during initialization)
    if !ctx.initializing {
        // Check dict access: must have unlimited access
        let current_dict = *ctx.d_stack.last().ok_or(PsError::DictStackUnderflow)?;
        let access = ctx.dicts.access(current_dict);
        if access != ObjFlags::ACCESS_UNLIMITED {
            return Err(PsError::InvalidAccess);
        }

        // Check VM access: global dict + local composite value = invalidaccess.
        // Use entity-level global status (authoritative) rather than PsObject
        // flags which can lose the global bit (e.g. after currentdict, get).
        let dict_is_global = current_dict.is_global();
        if dict_is_global && val.is_composite() && !val.is_global_vm() {
            return Err(PsError::InvalidAccess);
        }
    }

    // Name the dict after its key (like PostForge), so it displays nicely
    if let (PsValue::Dict(e), PsValue::Name(name_id)) = (val.value, key_obj.value) {
        let name_bytes = ctx.names.get_bytes(name_id).to_vec();
        ctx.dicts.set_name(e, &name_bytes);
    }

    let key = ctx.make_dict_key(&key_obj)?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.dict_def(key, val)?;
    Ok(())
}

/// `load`: key → value (look up key in dict stack)
pub fn op_load(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let key_obj = ctx.o_stack.peek(0)?;
    let key = ctx.make_dict_key(&key_obj)?;
    match ctx.dict_load(&key) {
        Some(val) => {
            ctx.o_stack.pop()?;
            ctx.o_stack.push(val)?;
            Ok(())
        }
        None => Err(PsError::Undefined),
    }
}

/// `store`: key value → — (store in first dict containing key, or current)
pub fn op_store(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let val = ctx.o_stack.peek(0)?;
    let key_obj = ctx.o_stack.peek(1)?;
    let key = ctx.make_dict_key(&key_obj)?;

    // VM access check: find the target dict and check global/local
    if !ctx.initializing {
        let target_dict = ctx
            .d_stack
            .iter()
            .rev()
            .find(|&&d| ctx.dicts.known(d, &key))
            .copied()
            .unwrap_or(*ctx.d_stack.last().unwrap());
        let dict_is_global = target_dict.is_global();
        if dict_is_global && val.is_composite() && !val.is_global_vm() {
            return Err(PsError::InvalidAccess);
        }
    }

    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.dict_store(key, val)?;
    Ok(())
}

/// `known`: dict key → bool
pub fn op_known(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let key_obj = ctx.o_stack.peek(0)?;
    let dict_obj = ctx.o_stack.peek(1)?;

    let dict_entity = match dict_obj.value {
        PsValue::Dict(e) => e,
        _ => return Err(PsError::TypeCheck),
    };

    let key = ctx.make_dict_key(&key_obj)?;
    let result = ctx.dicts.known(dict_entity, &key);

    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.push(PsObject::bool(result))?;
    Ok(())
}

/// `where`: key → dict true | false
pub fn op_where(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let key_obj = ctx.o_stack.peek(0)?;
    let key = ctx.make_dict_key(&key_obj)?;

    match ctx.dict_where(&key) {
        Some((dict_entity, _)) => {
            ctx.o_stack.pop()?;
            ctx.o_stack.push(PsObject::dict(dict_entity))?;
            ctx.o_stack.push(PsObject::bool(true))?;
            Ok(())
        }
        None => {
            ctx.o_stack.pop()?;
            ctx.o_stack.push(PsObject::bool(false))?;
            Ok(())
        }
    }
}

/// `maxlength`: dict → int
pub fn op_maxlength(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let entity = match obj.value {
        PsValue::Dict(e) => e,
        _ => return Err(PsError::TypeCheck),
    };
    let max_len = ctx.dicts.length(entity) as i32;
    ctx.o_stack.pop()?;
    ctx.o_stack.push(PsObject::int(max_len))?;
    Ok(())
}

/// `currentdict`: → dict
pub fn op_currentdict(ctx: &mut Context) -> Result<(), PsError> {
    let entity = *ctx.d_stack.last().ok_or(PsError::DictStackUnderflow)?;
    ctx.o_stack.push(PsObject::dict(entity))?;
    Ok(())
}

/// `countdictstack`: → int
pub fn op_countdictstack(ctx: &mut Context) -> Result<(), PsError> {
    ctx.o_stack.push(PsObject::int(ctx.d_stack.len() as i32))?;
    Ok(())
}

/// `dictstack`: array → subarray (copy dict stack into array)
pub fn op_dictstack(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let arr_obj = ctx.o_stack.peek(0)?;
    let (entity, _start, arr_len) = match arr_obj.value {
        PsValue::Array { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };

    let d_len = ctx.d_stack.len();
    if d_len > arr_len as usize {
        return Err(PsError::RangeCheck);
    }

    let d_stack_copy: Vec<_> = ctx.d_stack.clone();
    ctx.o_stack.pop()?;

    for (i, &dict_id) in d_stack_copy.iter().enumerate() {
        ctx.arrays
            .set_element(entity, i as u32, PsObject::dict(dict_id));
    }

    ctx.o_stack.push(PsObject {
        value: PsValue::Array {
            entity,
            start: 0,
            len: d_len as u32,
        },
        flags: arr_obj.flags,
    })?;
    Ok(())
}

/// `undef`: dict key → — (remove key from dict)
pub fn op_undef(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let key_obj = ctx.o_stack.peek(0)?;
    let dict_obj = ctx.o_stack.peek(1)?;

    let dict_entity = match dict_obj.value {
        PsValue::Dict(e) => e,
        _ => return Err(PsError::TypeCheck),
    };

    let key = ctx.make_dict_key(&key_obj)?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.cow_check_dict(dict_entity);
    ctx.dicts.remove(dict_entity, &key);
    Ok(())
}

/// `cleardictstack`: — (pop all but permanent dicts)
pub fn op_cleardictstack(ctx: &mut Context) -> Result<(), PsError> {
    ctx.d_stack.truncate(3); // keep systemdict, globaldict, userdict
    Ok(())
}

/// `dictname`: dict → name (return the dict's name)
///
/// PostForge extension. Replaces the dict on the stack with its name.
pub fn op_dictname(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let entity = match obj.value {
        PsValue::Dict(e) => e,
        _ => return Err(PsError::TypeCheck),
    };
    let name_bytes = ctx.dicts.get_name(entity);
    let name_id = ctx.names.intern(name_bytes);
    ctx.o_stack.pop()?;
    ctx.o_stack.push(PsObject::name_lit(name_id))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx() -> Context {
        Context::new()
    }

    #[test]
    fn test_dict_create() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(10)).unwrap();
        op_dict(&mut ctx).unwrap();
        let obj = ctx.o_stack.pop().unwrap();
        assert!(matches!(obj.value, PsValue::Dict(_)));
    }

    #[test]
    fn test_def_and_load() {
        let mut ctx = test_ctx();
        let key_id = ctx.names.intern(b"myvar");
        ctx.o_stack.push(PsObject::name_lit(key_id)).unwrap();
        ctx.o_stack.push(PsObject::int(42)).unwrap();
        op_def(&mut ctx).unwrap();

        ctx.o_stack.push(PsObject::name_lit(key_id)).unwrap();
        op_load(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.pop().unwrap().as_i32(), Some(42));
    }

    #[test]
    fn test_begin_end() {
        let mut ctx = test_ctx();
        let initial_depth = ctx.d_stack.len();
        ctx.o_stack.push(PsObject::int(10)).unwrap();
        op_dict(&mut ctx).unwrap();
        op_begin(&mut ctx).unwrap();
        assert_eq!(ctx.d_stack.len(), initial_depth + 1);
        op_end(&mut ctx).unwrap();
        assert_eq!(ctx.d_stack.len(), initial_depth);
    }

    #[test]
    fn test_known() {
        let mut ctx = test_ctx();
        let sd = ctx.systemdict;
        let key_id = ctx.names.intern(b"true");
        ctx.o_stack.push(PsObject::dict(sd)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(key_id)).unwrap();
        op_known(&mut ctx).unwrap();
        assert!(matches!(
            ctx.o_stack.pop().unwrap().value,
            PsValue::Bool(true)
        ));
    }

    #[test]
    fn test_countdictstack() {
        let mut ctx = test_ctx();
        op_countdictstack(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.pop().unwrap().as_i32(), Some(3));
    }
}
