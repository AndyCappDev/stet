// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Parameter operators: setuserparams, currentuserparams, setsystemparams,
//! currentsystemparams, setdevparams, currentdevparams.

use stet_core::context::Context;
use stet_core::dict::DictKey;
use stet_core::error::PsError;
use stet_core::object::{PsObject, PsValue};

/// `setuserparams`: dict → — (store user parameters)
///
/// Updates user parameters from the provided dict. Only keys that already
/// exist in the user_params dict are updated. After storing, applies
/// relevant parameters to the context (stack sizes, etc.).
pub fn op_setuserparams(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let dict_entity = match obj.value {
        PsValue::Dict(e) => e,
        _ => return Err(PsError::TypeCheck),
    };

    // Validate: all keys must be names, values must be int/bool/string/name
    let new_entries: Vec<(DictKey, PsObject)> = ctx
        .dicts
        .entry(dict_entity)
        .entries
        .iter()
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    for (key, val) in &new_entries {
        if !matches!(key, DictKey::Name(_)) {
            return Err(PsError::TypeCheck);
        }
        if !matches!(
            val.value,
            PsValue::Int(_) | PsValue::Bool(_) | PsValue::String { .. } | PsValue::Name(_)
        ) {
            return Err(PsError::TypeCheck);
        }
    }

    ctx.o_stack.pop()?;

    // Only update keys that already exist in user_params
    for (key, val) in &new_entries {
        if ctx.dicts.known(ctx.user_params, key) {
            ctx.dicts.put(ctx.user_params, key.clone(), *val);
        }
    }

    // Apply parameters from user_params to context
    apply_user_params(ctx);

    Ok(())
}

/// Apply user parameters from the user_params dict to the context.
fn apply_user_params(ctx: &mut Context) {
    let max_op_name = ctx.names.intern(b"MaxOpStack");
    if let Some(obj) = ctx.dicts.get(ctx.user_params, &DictKey::Name(max_op_name))
        && let Some(v) = obj.as_i32()
        && v > 0
    {
        ctx.o_stack.set_max_size(v as usize);
    }
    let max_exec_name = ctx.names.intern(b"MaxExecStack");
    if let Some(obj) = ctx
        .dicts
        .get(ctx.user_params, &DictKey::Name(max_exec_name))
        && let Some(v) = obj.as_i32()
        && v > 0
    {
        ctx.e_stack.set_max_size(v as usize);
    }
}

/// `currentuserparams`: — → dict (return current user parameters)
pub fn op_currentuserparams(ctx: &mut Context) -> Result<(), PsError> {
    // Return a copy of user_params
    let copy = ctx
        .dicts
        .allocate(ctx.dicts.max_length(ctx.user_params), b"userparams_copy");
    let keys: Vec<DictKey> = ctx.dicts.keys(ctx.user_params).cloned().collect();
    for key in keys {
        if let Some(val) = ctx.dicts.get(ctx.user_params, &key) {
            ctx.dicts.put(copy, key, val);
        }
    }
    ctx.o_stack.push(PsObject::dict(copy))?;
    Ok(())
}

/// `setsystemparams`: dict → — (stub: pop, no-op)
pub fn op_setsystemparams(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    if !matches!(obj.value, PsValue::Dict(_)) {
        return Err(PsError::TypeCheck);
    }
    ctx.o_stack.pop()?;
    Ok(())
}

/// `currentsystemparams`: — → dict (return system parameters)
pub fn op_currentsystemparams(ctx: &mut Context) -> Result<(), PsError> {
    let copy = ctx
        .dicts
        .allocate(ctx.dicts.max_length(ctx.system_params), b"sysparams_copy");
    let keys: Vec<DictKey> = ctx.dicts.keys(ctx.system_params).cloned().collect();
    for key in keys {
        if let Some(val) = ctx.dicts.get(ctx.system_params, &key) {
            ctx.dicts.put(copy, key, val);
        }
    }
    ctx.o_stack.push(PsObject::dict(copy))?;
    Ok(())
}

/// `setdevparams`: string dict → — (set device parameters, no-op stub)
pub fn op_setdevparams(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let dict_obj = ctx.o_stack.peek(0)?;
    let str_obj = ctx.o_stack.peek(1)?;
    match dict_obj.value {
        PsValue::Dict(_) => {}
        _ => return Err(PsError::TypeCheck),
    }
    match str_obj.value {
        PsValue::String { .. } => {}
        _ => return Err(PsError::TypeCheck),
    }
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    Ok(())
}

/// `setdistillerparams`: dict → — (Distiller compatibility stub)
///
/// Adobe Distiller / GhostScript expose Distiller parameters via the
/// `currentdistillerparams` / `setdistillerparams` pair. PostScript code
/// commonly written for those workflows checks `systemdict /pdfmark
/// known` and, when true, queries `currentdistillerparams /CoreDistVersion
/// get` before issuing pdfmarks. stet's PDF output is not Distiller-
/// compatible the way Adobe's is, but registering both operators with
/// sensible defaults keeps the production prologue path runnable instead
/// of hitting `undefined` after `pdfmark` was loaded.
pub fn op_setdistillerparams(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    if !matches!(ctx.o_stack.peek(0)?.value, PsValue::Dict(_)) {
        return Err(PsError::TypeCheck);
    }
    ctx.o_stack.pop()?;
    Ok(())
}

/// `currentdistillerparams`: — → dict
///
/// Returns a small dict carrying the entries production PostScript
/// prologues actually inspect — chiefly `/CoreDistVersion` and
/// `/CompatibilityLevel`. We report `CoreDistVersion = 4000` (Distiller
/// 6.0 era): high enough to satisfy `>= 2000` checks that older
/// FrameMaker-style scripts use to gate pdfmark emission, but low
/// enough that newer Adobe Illustrator scripts which gate distiller-
/// only stream injection on `< 5000` route their binary metadata
/// streams down the `flushfile cleartomark` path stet can actually
/// handle. See [`op_setdistillerparams`].
pub fn op_currentdistillerparams(ctx: &mut Context) -> Result<(), PsError> {
    let d = ctx.dicts.allocate(8, b"distillerparams");
    let core = ctx.names.intern(b"CoreDistVersion");
    ctx.dicts.put(d, DictKey::Name(core), PsObject::int(4000));
    let compat = ctx.names.intern(b"CompatibilityLevel");
    ctx.dicts.put(d, DictKey::Name(compat), PsObject::real(1.7));
    ctx.o_stack.push(PsObject::dict(d))?;
    Ok(())
}

/// `currentdevparams`: string → dict (get device parameters, stub returns empty dict)
pub fn op_currentdevparams(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    match obj.value {
        PsValue::String { .. } => {}
        _ => return Err(PsError::TypeCheck),
    }
    ctx.o_stack.pop()?;
    let d = ctx.dicts.allocate(5, b"devparams");
    ctx.o_stack.push(PsObject::dict(d))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use stet_core::context::Context;

    fn test_ctx() -> Context {
        let mut ctx = Context::new();
        crate::build_system_dict(&mut ctx);
        ctx
    }

    #[test]
    fn test_setuserparams_currentuserparams() {
        let mut ctx = test_ctx();

        // Create a dict with a param
        let d = ctx.dicts.allocate(5, b"test");
        let key = ctx.names.intern(b"MaxDictStack");
        ctx.dicts.put(d, DictKey::Name(key), PsObject::int(500));

        ctx.o_stack.push(PsObject::dict(d)).unwrap();
        op_setuserparams(&mut ctx).unwrap();

        // Retrieve
        op_currentuserparams(&mut ctx).unwrap();
        let result = ctx.o_stack.pop().unwrap();
        if let PsValue::Dict(e) = result.value {
            let val = ctx.dicts.get(e, &DictKey::Name(key));
            assert_eq!(val.unwrap().as_i32(), Some(500));
        } else {
            panic!("Expected dict");
        }
    }

    #[test]
    fn test_setsystemparams_no_crash() {
        let mut ctx = test_ctx();
        let d = ctx.dicts.allocate(5, b"test");
        ctx.o_stack.push(PsObject::dict(d)).unwrap();
        op_setsystemparams(&mut ctx).unwrap();
        assert!(ctx.o_stack.is_empty());
    }

    #[test]
    fn test_currentdevparams() {
        let mut ctx = test_ctx();
        let s = ctx.strings.allocate_from(b"%stdin");
        ctx.o_stack.push(PsObject::string(s, 6)).unwrap();
        op_currentdevparams(&mut ctx).unwrap();
        let result = ctx.o_stack.pop().unwrap();
        assert!(matches!(result.value, PsValue::Dict(_)));
    }
}
