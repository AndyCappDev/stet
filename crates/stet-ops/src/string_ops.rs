// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! String operators: string, anchorsearch, search, token.

use stet_core::context::Context;
use stet_core::error::PsError;
use stet_core::object::{PsObject, PsValue};

/// `string`: int → string (create string of given length, zero-filled)
pub fn op_string(ctx: &mut Context) -> Result<(), PsError> {
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
    let entity = crate::vm_ops::alloc_string_empty(ctx, len);
    let obj = crate::vm_ops::make_string_obj(ctx, entity, len as u32);
    ctx.o_stack.push(obj)?;
    Ok(())
}

/// `anchorsearch`: string seek → post match true | string false
pub fn op_anchorsearch(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let seek_obj = ctx.o_stack.peek(0)?;
    let str_obj = ctx.o_stack.peek(1)?;

    let (str_entity, str_start, str_len) = match str_obj.value {
        PsValue::String { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };
    let (seek_entity, seek_start, seek_len) = match seek_obj.value {
        PsValue::String { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };

    let str_bytes = ctx.strings.get(str_entity, str_start, str_len).to_vec();
    let seek_bytes = ctx.strings.get(seek_entity, seek_start, seek_len).to_vec();

    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;

    if str_bytes.starts_with(&seek_bytes) {
        // post = remainder of str after seek match (same entity, advanced start)
        let post_start = str_start + seek_len;
        let post_len = str_len - seek_len;
        // match = the seek portion of str (same entity as str)
        let match_start = str_start;
        let match_len = seek_len;

        ctx.o_stack.push(PsObject {
            value: PsValue::String {
                entity: str_entity,
                start: post_start,
                len: post_len,
            },
            flags: str_obj.flags,
        })?;
        ctx.o_stack.push(PsObject {
            value: PsValue::String {
                entity: str_entity,
                start: match_start,
                len: match_len,
            },
            flags: str_obj.flags,
        })?;
        ctx.o_stack.push(PsObject::bool(true))?;
    } else {
        // Push original string back
        ctx.o_stack.push(str_obj)?;
        ctx.o_stack.push(PsObject::bool(false))?;
    }
    Ok(())
}

/// `search`: string seek → post match pre true | string false
pub fn op_search(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let seek_obj = ctx.o_stack.peek(0)?;
    let str_obj = ctx.o_stack.peek(1)?;

    let (str_entity, str_start, str_len) = match str_obj.value {
        PsValue::String { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };
    let (seek_entity, seek_start, seek_len) = match seek_obj.value {
        PsValue::String { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };

    let str_bytes = ctx.strings.get(str_entity, str_start, str_len).to_vec();
    let seek_bytes = ctx.strings.get(seek_entity, seek_start, seek_len).to_vec();

    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;

    if let Some(pos) = find_subsequence(&str_bytes, &seek_bytes) {
        // All substrings reference the original str entity
        let post_start = str_start + pos as u32 + seek_len;
        let post_len = str_len - pos as u32 - seek_len;
        let match_start = str_start + pos as u32;
        let match_len = seek_len;
        let pre_start = str_start;
        let pre_len = pos as u32;

        ctx.o_stack.push(PsObject {
            value: PsValue::String {
                entity: str_entity,
                start: post_start,
                len: post_len,
            },
            flags: str_obj.flags,
        })?;
        ctx.o_stack.push(PsObject {
            value: PsValue::String {
                entity: str_entity,
                start: match_start,
                len: match_len,
            },
            flags: str_obj.flags,
        })?;
        ctx.o_stack.push(PsObject {
            value: PsValue::String {
                entity: str_entity,
                start: pre_start,
                len: pre_len,
            },
            flags: str_obj.flags,
        })?;
        ctx.o_stack.push(PsObject::bool(true))?;
    } else {
        // Push original string back
        ctx.o_stack.push(str_obj)?;
        ctx.o_stack.push(PsObject::bool(false))?;
    }
    Ok(())
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx() -> Context {
        Context::new()
    }

    #[test]
    fn test_string_create() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(10)).unwrap();
        op_string(&mut ctx).unwrap();
        let s = ctx.o_stack.pop().unwrap();
        match s.value {
            PsValue::String { len, .. } => assert_eq!(len, 10),
            _ => panic!("Expected string"),
        }
    }

    #[test]
    fn test_anchorsearch_found() {
        let mut ctx = test_ctx();
        let s = ctx.strings.allocate_from(b"abcdef");
        let seek = ctx.strings.allocate_from(b"abc");
        ctx.o_stack.push(PsObject::string(s, 6)).unwrap();
        ctx.o_stack.push(PsObject::string(seek, 3)).unwrap();
        op_anchorsearch(&mut ctx).unwrap();
        // Should have: post match true
        assert!(matches!(
            ctx.o_stack.pop().unwrap().value,
            PsValue::Bool(true)
        ));
    }

    #[test]
    fn test_search_found() {
        let mut ctx = test_ctx();
        let s = ctx.strings.allocate_from(b"abcdef");
        let seek = ctx.strings.allocate_from(b"cd");
        ctx.o_stack.push(PsObject::string(s, 6)).unwrap();
        ctx.o_stack.push(PsObject::string(seek, 2)).unwrap();
        op_search(&mut ctx).unwrap();
        assert!(matches!(
            ctx.o_stack.pop().unwrap().value,
            PsValue::Bool(true)
        ));
    }
}
