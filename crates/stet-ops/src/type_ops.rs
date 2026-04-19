// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Type and conversion operators: type, cvx, cvlit, cvn, cvs, cvrs, cvi, cvr,
//! xcheck, executeonly, noaccess, readonly, rcheck, wcheck.

use std::io::Write;

use stet_core::context::Context;
use stet_core::error::PsError;
use stet_core::object::{ObjFlags, PsObject, PsValue};

/// `type`: obj → nametype (pushes the type name as an executable name)
pub fn op_type(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let type_bytes = obj.type_name();
    let name_id = ctx.names.intern(type_bytes);
    ctx.o_stack.pop()?;
    ctx.o_stack.push(PsObject::name_exec(name_id))?;
    Ok(())
}

/// `cvx`: obj → obj (make executable)
pub fn op_cvx(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek_mut(0)?;
    obj.flags.set_executable();
    Ok(())
}

/// `cvlit`: obj → obj (make literal)
pub fn op_cvlit(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek_mut(0)?;
    obj.flags.set_literal();
    Ok(())
}

/// `cvn`: string → name
pub fn op_cvn(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    match obj.value {
        PsValue::String { entity, start, len } => {
            // Access check: string must be readable
            obj.flags.require_read()?;
            let bytes = ctx.strings.get(entity, start, len).to_vec();
            let name_id = ctx.names.intern(&bytes);
            ctx.o_stack.pop()?;
            if obj.flags.is_executable() {
                ctx.o_stack.push(PsObject::name_exec(name_id))?;
            } else {
                ctx.o_stack.push(PsObject::name_lit(name_id))?;
            }
            Ok(())
        }
        _ => Err(PsError::TypeCheck),
    }
}

/// `cvs`: any string → substring (convert to string representation)
pub fn op_cvs(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let str_obj = ctx.o_stack.peek(0)?;
    let val_obj = ctx.o_stack.peek(1)?;

    let (str_entity, str_start, str_len) = match str_obj.value {
        PsValue::String { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };
    // Access check: dest string must be writable
    str_obj.flags.require_write()?;

    let repr = format_object_cvs(ctx, &val_obj);

    if repr.len() > str_len as usize {
        return Err(PsError::RangeCheck);
    }

    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;

    let dest = ctx.strings.get_mut(str_entity, str_start, str_len);
    dest[..repr.len()].copy_from_slice(repr.as_bytes());

    ctx.o_stack.push(PsObject {
        value: PsValue::String {
            entity: str_entity,
            start: str_start,
            len: repr.len() as u32,
        },
        flags: str_obj.flags,
    })?;
    Ok(())
}

/// Format an object for `cvs` (simple representation).
fn format_object_cvs(ctx: &Context, obj: &PsObject) -> String {
    match obj.value {
        PsValue::Int(v) => format!("{}", v),
        PsValue::Real(v) => format_real(v),
        PsValue::Bool(v) => {
            if v {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
        PsValue::Name(id) => String::from_utf8_lossy(ctx.names.get_bytes(id)).to_string(),
        PsValue::String { entity, start, len } => {
            String::from_utf8_lossy(ctx.strings.get(entity, start, len)).to_string()
        }
        PsValue::Operator(op) => {
            let name_id = ctx.operators[op.0 as usize].name;
            String::from_utf8_lossy(ctx.names.get_bytes(name_id)).to_string()
        }
        PsValue::Null => "--nostringval--".to_string(),
        PsValue::Mark | PsValue::DictMark => "--nostringval--".to_string(),
        _ => "--nostringval--".to_string(),
    }
}

/// Format a real number following PostScript conventions.
fn format_real(v: f64) -> String {
    // Round to 6 decimal places to avoid floating-point noise
    let rounded = (v * 1e6).round() / 1e6;

    // Check if it's effectively an integer
    if rounded == (rounded as i64) as f64 && rounded.abs() < 1e10 {
        return format!("{:.1}", rounded);
    }

    // Format with up to 6 decimal places, stripping trailing zeros
    let formatted = format!("{:.6}", rounded);
    let trimmed = formatted.trim_end_matches('0');
    if trimmed.ends_with('.') {
        format!("{}0", trimmed)
    } else {
        trimmed.to_string()
    }
}

/// `cvrs`: num radix string → substring
pub fn op_cvrs(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 3 {
        return Err(PsError::StackUnderflow);
    }
    let str_obj = ctx.o_stack.peek(0)?;
    let radix_obj = ctx.o_stack.peek(1)?;
    let num_obj = ctx.o_stack.peek(2)?;

    let (str_entity, str_start, str_len) = match str_obj.value {
        PsValue::String { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };
    // Access check: dest string must be writable
    str_obj.flags.require_write()?;

    let radix = match radix_obj.value {
        PsValue::Int(v) => {
            if !(2..=36).contains(&v) {
                return Err(PsError::RangeCheck);
            }
            v as u32
        }
        _ => return Err(PsError::TypeCheck),
    };

    let repr = match num_obj.value {
        PsValue::Int(v) => {
            if radix == 10 {
                format!("{}", v)
            } else {
                // Format as unsigned for non-base-10
                format_radix(v as u32, radix)
            }
        }
        PsValue::Real(v) => {
            if radix != 10 {
                // Real with non-base-10 → convert to int first
                format_radix(v as u32, radix)
            } else {
                format_real(v)
            }
        }
        _ => return Err(PsError::TypeCheck),
    };

    if repr.len() > str_len as usize {
        return Err(PsError::RangeCheck);
    }

    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;

    let dest = ctx.strings.get_mut(str_entity, str_start, str_len);
    dest[..repr.len()].copy_from_slice(repr.as_bytes());

    ctx.o_stack.push(PsObject {
        value: PsValue::String {
            entity: str_entity,
            start: str_start,
            len: repr.len() as u32,
        },
        flags: str_obj.flags,
    })?;
    Ok(())
}

fn format_radix(value: u32, radix: u32) -> String {
    if value == 0 {
        return "0".to_string();
    }
    let digits = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut result = Vec::new();
    let mut v = value;
    while v > 0 {
        result.push(digits.as_bytes()[(v % radix) as usize]);
        v /= radix;
    }
    result.reverse();
    String::from_utf8(result).unwrap()
}

/// `cvi`: num/string → int
pub fn op_cvi(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let result = match obj.value {
        PsValue::Int(_) => obj,
        PsValue::Real(v) => {
            let truncated = v.trunc();
            if truncated >= i32::MIN as f64 && truncated <= i32::MAX as f64 {
                PsObject::int(truncated as i32)
            } else {
                return Err(PsError::RangeCheck);
            }
        }
        PsValue::String { entity, start, len } => {
            // Access check: string must be readable
            obj.flags.require_read()?;
            let bytes = ctx.strings.get(entity, start, len);
            let s = std::str::from_utf8(bytes).map_err(|_| PsError::SyntaxError)?;
            let s = s.trim();
            if let Ok(v) = s.parse::<i32>() {
                PsObject::int(v)
            } else if let Ok(v) = s.parse::<f64>() {
                let truncated = v.trunc();
                if truncated >= i32::MIN as f64 && truncated <= i32::MAX as f64 {
                    PsObject::int(truncated as i32)
                } else {
                    return Err(PsError::RangeCheck);
                }
            } else {
                return Err(PsError::SyntaxError);
            }
        }
        _ => return Err(PsError::TypeCheck),
    };
    ctx.o_stack.pop()?;
    ctx.o_stack.push(result)?;
    Ok(())
}

/// `cvr`: num/string → real
pub fn op_cvr(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let result = match obj.value {
        PsValue::Int(v) => PsObject::real(v as f64),
        PsValue::Real(_) => obj,
        PsValue::String { entity, start, len } => {
            // Access check: string must be readable
            obj.flags.require_read()?;
            let bytes = ctx.strings.get(entity, start, len);
            let s = std::str::from_utf8(bytes).map_err(|_| PsError::SyntaxError)?;
            let v: f64 = s.trim().parse().map_err(|_| PsError::SyntaxError)?;
            PsObject::real(v)
        }
        _ => return Err(PsError::TypeCheck),
    };
    ctx.o_stack.pop()?;
    ctx.o_stack.push(result)?;
    Ok(())
}

/// `xcheck`: obj → bool (is executable?)
pub fn op_xcheck(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let result = obj.flags.is_executable();
    ctx.o_stack.pop()?;
    ctx.o_stack.push(PsObject::bool(result))?;
    Ok(())
}

/// Returns true if the object is an access-controllable composite type
/// (array, packedarray, string, dict, file).
fn is_access_type(obj: &PsObject) -> bool {
    matches!(
        obj.value,
        PsValue::Array { .. }
            | PsValue::PackedArray { .. }
            | PsValue::String { .. }
            | PsValue::Dict(_)
            | PsValue::File(_)
    )
}

/// Get the effective access level for an object. For dicts, reads from DictStore.
fn get_access(ctx: &Context, obj: &PsObject) -> u8 {
    match obj.value {
        PsValue::Dict(entity) => ctx.dicts.access(entity),
        _ => obj.flags.access(),
    }
}

/// `executeonly`: obj → obj (set access to execute-only)
/// Accepts: array, packedarray, string, file. NOT dict.
pub fn op_executeonly(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    if matches!(obj.value, PsValue::Dict(_)) || !is_access_type(&obj) {
        return Err(PsError::TypeCheck);
    }
    // Can't elevate from noaccess
    if obj.flags.access() == ObjFlags::ACCESS_NONE {
        return Err(PsError::InvalidAccess);
    }
    let obj = ctx.o_stack.peek_mut(0)?;
    obj.flags.set_access(ObjFlags::ACCESS_EXECUTE_ONLY);
    Ok(())
}

/// `noaccess`: obj → obj (set access to none)
/// Accepts: array, packedarray, string, dict, file.
/// For dicts: if currently read-only, raises invalidaccess.
pub fn op_noaccess(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    if !is_access_type(&obj) {
        return Err(PsError::TypeCheck);
    }
    if let PsValue::Dict(entity) = obj.value {
        let access = ctx.dicts.access(entity);
        if access <= ObjFlags::ACCESS_READ_ONLY {
            return Err(PsError::InvalidAccess);
        }
        ctx.dicts.set_access(entity, ObjFlags::ACCESS_NONE);
    } else {
        let obj = ctx.o_stack.peek_mut(0)?;
        obj.flags.set_access(ObjFlags::ACCESS_NONE);
    }
    Ok(())
}

/// `readonly`: obj → obj (set access to read-only)
/// Accepts: array, packedarray, string, dict, file.
pub fn op_readonly(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    if !is_access_type(&obj) {
        return Err(PsError::TypeCheck);
    }
    if let PsValue::Dict(entity) = obj.value {
        let access = ctx.dicts.access(entity);
        if access < ObjFlags::ACCESS_READ_ONLY {
            return Err(PsError::InvalidAccess);
        }
        ctx.dicts.set_access(entity, ObjFlags::ACCESS_READ_ONLY);
    } else {
        let obj = ctx.o_stack.peek_mut(0)?;
        obj.flags.set_access(ObjFlags::ACCESS_READ_ONLY);
    }
    Ok(())
}

/// `rcheck`: obj → bool (read access?)
/// Accepts: array, packedarray, string, dict, file.
pub fn op_rcheck(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    if !is_access_type(&obj) {
        return Err(PsError::TypeCheck);
    }
    let access = get_access(ctx, &obj);
    let result = access >= ObjFlags::ACCESS_READ_ONLY && access != ObjFlags::ACCESS_WRITE_ONLY;
    ctx.o_stack.pop()?;
    ctx.o_stack.push(PsObject::bool(result))?;
    Ok(())
}

/// `wcheck`: obj → bool (write access?)
/// Accepts: array, packedarray, string, dict, file.
pub fn op_wcheck(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    if !is_access_type(&obj) {
        return Err(PsError::TypeCheck);
    }
    let access = get_access(ctx, &obj);
    let result = access >= ObjFlags::ACCESS_UNLIMITED;
    ctx.o_stack.pop()?;
    ctx.o_stack.push(PsObject::bool(result))?;
    Ok(())
}

/// Write the `=` representation of an object (for the `=` and `==` operators).
pub fn write_obj_equal(ctx: &Context, obj: &PsObject, out: &mut dyn Write) {
    match obj.value {
        PsValue::Int(v) => {
            write!(out, "{}", v).ok();
        }
        PsValue::Real(v) => {
            write!(out, "{}", format_real(v)).ok();
        }
        PsValue::Bool(v) => {
            write!(out, "{}", v).ok();
        }
        PsValue::Null => {
            write!(out, "null").ok();
        }
        PsValue::Mark | PsValue::DictMark => {
            write!(out, "-mark-").ok();
        }
        PsValue::Name(id) => {
            let bytes = ctx.names.get_bytes(id);
            if obj.flags.is_executable() {
                out.write_all(bytes).ok();
            } else {
                write!(out, "/").ok();
                out.write_all(bytes).ok();
            }
        }
        PsValue::String { entity, start, len } => {
            let bytes = ctx.strings.get(entity, start, len);
            write!(out, "(").ok();
            out.write_all(bytes).ok();
            write!(out, ")").ok();
        }
        PsValue::Array { entity, start, len } => {
            if obj.flags.is_executable() {
                write!(out, "{{").ok();
            } else {
                write!(out, "[").ok();
            }
            let elements = ctx.arrays.get(entity, start, len);
            for (i, elem) in elements.iter().enumerate() {
                if i > 0 {
                    write!(out, " ").ok();
                }
                write_obj_equal(ctx, elem, out);
            }
            if obj.flags.is_executable() {
                write!(out, "}}").ok();
            } else {
                write!(out, "]").ok();
            }
        }
        PsValue::Operator(op) => {
            let name_id = ctx.operators[op.0 as usize].name;
            let bytes = ctx.names.get_bytes(name_id);
            write!(out, "--").ok();
            out.write_all(bytes).ok();
            write!(out, "--").ok();
        }
        PsValue::Dict(_) => {
            write!(out, "-dict-").ok();
        }
        PsValue::FontID(v) => {
            write!(out, "-fontID:{}-", v).ok();
        }
        _ => {
            write!(out, "--nostringval--").ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx() -> Context {
        Context::new()
    }

    #[test]
    fn test_type_op() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(42)).unwrap();
        op_type(&mut ctx).unwrap();
        let result = ctx.o_stack.pop().unwrap();
        match result.value {
            PsValue::Name(id) => assert_eq!(ctx.names.get_bytes(id), b"integertype"),
            _ => panic!("Expected name"),
        }
    }

    #[test]
    fn test_cvx_cvlit() {
        let mut ctx = test_ctx();
        let id = ctx.names.intern(b"foo");
        ctx.o_stack.push(PsObject::name_lit(id)).unwrap();
        assert!(ctx.o_stack.peek(0).unwrap().flags.is_literal());
        op_cvx(&mut ctx).unwrap();
        assert!(ctx.o_stack.peek(0).unwrap().flags.is_executable());
        op_cvlit(&mut ctx).unwrap();
        assert!(ctx.o_stack.peek(0).unwrap().flags.is_literal());
    }

    #[test]
    fn test_cvi() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::real(3.7)).unwrap();
        op_cvi(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.pop().unwrap().as_i32(), Some(3));
    }

    #[test]
    fn test_cvr() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(42)).unwrap();
        op_cvr(&mut ctx).unwrap();
        match ctx.o_stack.pop().unwrap().value {
            PsValue::Real(v) => assert_eq!(v, 42.0),
            _ => panic!("Expected Real"),
        }
    }

    #[test]
    fn test_xcheck() {
        let mut ctx = test_ctx();
        let id = ctx.names.intern(b"foo");
        ctx.o_stack.push(PsObject::name_exec(id)).unwrap();
        op_xcheck(&mut ctx).unwrap();
        assert!(matches!(
            ctx.o_stack.pop().unwrap().value,
            PsValue::Bool(true)
        ));

        ctx.o_stack.push(PsObject::int(42)).unwrap();
        op_xcheck(&mut ctx).unwrap();
        assert!(matches!(
            ctx.o_stack.pop().unwrap().value,
            PsValue::Bool(false)
        ));
    }
}
