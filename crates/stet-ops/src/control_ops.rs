// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Control flow operators: exec, if, ifelse, for, repeat, loop, forall,
//! exit, stop, stopped, quit.

use stet_core::context::{Context, LoopState, LoopType};
use stet_core::error::PsError;
use stet_core::object::{PsObject, PsValue};

/// `exec`: obj → — (push obj onto exec stack)
pub fn op_exec(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.pop()?;
    ctx.e_stack.push(obj)?;
    Ok(())
}

/// `if`: bool proc → — (execute proc if bool is true)
pub fn op_if(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let proc_obj = ctx.o_stack.peek(0)?;
    let bool_obj = ctx.o_stack.peek(1)?;

    // Validate types
    if !matches!(proc_obj.value, PsValue::Array { .. }) || !proc_obj.flags.is_executable() {
        return Err(PsError::TypeCheck);
    }
    let condition = match bool_obj.value {
        PsValue::Bool(v) => v,
        _ => return Err(PsError::TypeCheck),
    };

    let proc = ctx.o_stack.pop()?;
    ctx.o_stack.pop()?; // bool

    if condition {
        ctx.e_stack.push(proc)?;
    }
    Ok(())
}

/// `ifelse`: bool proc_true proc_false → —
pub fn op_ifelse(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 3 {
        return Err(PsError::StackUnderflow);
    }
    let proc_false = ctx.o_stack.peek(0)?;
    let proc_true = ctx.o_stack.peek(1)?;
    let bool_obj = ctx.o_stack.peek(2)?;

    // Validate types
    if !matches!(proc_false.value, PsValue::Array { .. }) || !proc_false.flags.is_executable() {
        return Err(PsError::TypeCheck);
    }
    if !matches!(proc_true.value, PsValue::Array { .. }) || !proc_true.flags.is_executable() {
        return Err(PsError::TypeCheck);
    }
    let condition = match bool_obj.value {
        PsValue::Bool(v) => v,
        _ => return Err(PsError::TypeCheck),
    };

    let pf = ctx.o_stack.pop()?;
    let pt = ctx.o_stack.pop()?;
    ctx.o_stack.pop()?; // bool

    if condition {
        ctx.e_stack.push(pt)?;
    } else {
        ctx.e_stack.push(pf)?;
    }
    Ok(())
}

/// `for`: initial increment limit proc → —
pub fn op_for(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 4 {
        return Err(PsError::StackUnderflow);
    }
    let proc_obj = ctx.o_stack.peek(0)?;
    let limit_obj = ctx.o_stack.peek(1)?;
    let incr_obj = ctx.o_stack.peek(2)?;
    let init_obj = ctx.o_stack.peek(3)?;

    // Validate proc
    let (proc_entity, proc_start, proc_len) = match proc_obj.value {
        PsValue::Array { entity, start, len } if proc_obj.flags.is_executable() => {
            (entity, start, len)
        }
        _ => return Err(PsError::TypeCheck),
    };

    let init = init_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let incr = incr_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let limit = limit_obj.as_f64().ok_or(PsError::TypeCheck)?;

    // Check if all are integers for integer loop
    let use_int = init_obj.is_int() && incr_obj.is_int() && limit_obj.is_int();

    ctx.o_stack.pop()?; // proc
    ctx.o_stack.pop()?; // limit
    ctx.o_stack.pop()?; // increment
    ctx.o_stack.pop()?; // initial

    let loop_state = LoopState {
        loop_type: LoopType::For,
        proc_entity,
        proc_start,
        proc_len,
        counter: init,
        increment: incr,
        limit,
        use_int,
        source: PsObject::null(),
        index: 0,
        dict_keys: None,
        path_segments: None,
        path_procs: None,
        path_ictm: None,
    };

    let loop_entity = ctx.alloc_loop(loop_state);
    ctx.e_stack.push(PsObject::loop_mark(loop_entity))?;
    Ok(())
}

/// `repeat`: int proc → —
pub fn op_repeat(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let proc_obj = ctx.o_stack.peek(0)?;
    let count_obj = ctx.o_stack.peek(1)?;

    let (proc_entity, proc_start, proc_len) = match proc_obj.value {
        PsValue::Array { entity, start, len } if proc_obj.flags.is_executable() => {
            (entity, start, len)
        }
        _ => return Err(PsError::TypeCheck),
    };

    let count = match count_obj.value {
        PsValue::Int(v) => {
            if v < 0 {
                return Err(PsError::RangeCheck);
            }
            v as f64
        }
        _ => return Err(PsError::TypeCheck),
    };

    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;

    if count <= 0.0 {
        return Ok(());
    }

    let loop_state = LoopState {
        loop_type: LoopType::Repeat,
        proc_entity,
        proc_start,
        proc_len,
        counter: count,
        increment: 0.0,
        limit: 0.0,
        use_int: false,
        source: PsObject::null(),
        index: 0,
        dict_keys: None,
        path_segments: None,
        path_procs: None,
        path_ictm: None,
    };

    let loop_entity = ctx.alloc_loop(loop_state);
    ctx.e_stack.push(PsObject::loop_mark(loop_entity))?;
    Ok(())
}

/// `loop`: proc → — (infinite loop until exit)
pub fn op_loop(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let proc_obj = ctx.o_stack.peek(0)?;

    let (proc_entity, proc_start, proc_len) = match proc_obj.value {
        PsValue::Array { entity, start, len } if proc_obj.flags.is_executable() => {
            (entity, start, len)
        }
        _ => return Err(PsError::TypeCheck),
    };

    ctx.o_stack.pop()?;

    let loop_state = LoopState {
        loop_type: LoopType::Loop,
        proc_entity,
        proc_start,
        proc_len,
        counter: 0.0,
        increment: 0.0,
        limit: 0.0,
        use_int: false,
        source: PsObject::null(),
        index: 0,
        dict_keys: None,
        path_segments: None,
        path_procs: None,
        path_ictm: None,
    };

    let loop_entity = ctx.alloc_loop(loop_state);
    ctx.e_stack.push(PsObject::loop_mark(loop_entity))?;
    Ok(())
}

/// `forall`: collection proc → —
pub fn op_forall(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let proc_obj = ctx.o_stack.peek(0)?;
    let source_obj = ctx.o_stack.peek(1)?;

    let (proc_entity, proc_start, proc_len) = match proc_obj.value {
        PsValue::Array { entity, start, len } if proc_obj.flags.is_executable() => {
            (entity, start, len)
        }
        _ => return Err(PsError::TypeCheck),
    };

    // Validate source is iterable
    match source_obj.value {
        PsValue::Array { .. }
        | PsValue::PackedArray { .. }
        | PsValue::String { .. }
        | PsValue::Dict(_) => {}
        _ => return Err(PsError::TypeCheck),
    }

    let source = ctx.o_stack.peek(1)?;
    ctx.o_stack.pop()?; // proc
    ctx.o_stack.pop()?; // source

    // For dict forall, snapshot keys once to avoid re-collecting every iteration
    let dict_keys = if let PsValue::Dict(dict_entity) = source.value {
        Some(ctx.dicts.keys(dict_entity).cloned().collect())
    } else {
        None
    };

    let loop_state = LoopState {
        loop_type: LoopType::Forall,
        proc_entity,
        proc_start,
        proc_len,
        counter: 0.0,
        increment: 0.0,
        limit: 0.0,
        use_int: false,
        source,
        index: 0,
        dict_keys,
        path_segments: None,
        path_procs: None,
        path_ictm: None,
    };

    let loop_entity = ctx.alloc_loop(loop_state);
    ctx.e_stack.push(PsObject::loop_mark(loop_entity))?;
    Ok(())
}

/// `exit`: — (exit innermost loop)
pub fn op_exit(_ctx: &mut Context) -> Result<(), PsError> {
    Err(PsError::Exit)
}

/// `stop`: — (stop execution to nearest stopped)
pub fn op_stop(_ctx: &mut Context) -> Result<(), PsError> {
    Err(PsError::Stop)
}

/// `stopped`: obj → bool (execute obj, catching stop)
pub fn op_stopped(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.pop()?;
    // Push stopped marker, then the object to execute
    ctx.e_stack.push(PsObject::stopped_mark())?;
    ctx.e_stack.push(obj)?;
    Ok(())
}

/// `quit`: — (terminate interpreter)
pub fn op_quit(_ctx: &mut Context) -> Result<(), PsError> {
    Err(PsError::Quit)
}

/// `countexecstack`: — → int (return exec stack depth)
pub fn op_countexecstack(ctx: &mut Context) -> Result<(), PsError> {
    let depth = ctx.e_stack.len() as i32;
    ctx.o_stack.push(PsObject::int(depth))?;
    Ok(())
}

/// `execstack`: array → subarray (copy exec stack into array)
pub fn op_execstack(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let (entity, _start, arr_len) = match obj.value {
        PsValue::Array { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };

    // VM access check: global array cannot receive local composite values
    let arr_is_global = entity.is_global();
    if arr_is_global {
        let e_slice = ctx.e_stack.as_slice();
        for elem in e_slice {
            if elem.is_composite() && !elem.flags.is_global() {
                return Err(PsError::InvalidAccess);
            }
        }
    }

    let e_len = ctx.e_stack.len();
    if (arr_len as usize) < e_len {
        return Err(PsError::RangeCheck);
    }

    ctx.o_stack.pop()?;

    // Copy exec stack contents into the array, converting internal
    // markers to PS-visible representations
    let e_slice = ctx.e_stack.as_slice();
    for (i, &elem) in e_slice.iter().enumerate() {
        let visible = match elem.value {
            PsValue::ExecArray {
                entity: ea,
                start: es,
                len: el,
                pos: ep,
            } => PsObject {
                value: PsValue::Array {
                    entity: ea,
                    start: es + ep,
                    len: el - ep,
                },
                flags: elem.flags,
            },
            PsValue::Stopped => {
                let s = ctx.strings.allocate_from(b"-stopped-");
                PsObject::string(s, 9)
            }
            PsValue::Loop(_) => {
                let s = ctx.strings.allocate_from(b"-loop-");
                PsObject::string(s, 6)
            }
            PsValue::DictEnd(_) => {
                let s = ctx.strings.allocate_from(b"-dictend-");
                PsObject::string(s, 9)
            }
            _ => elem,
        };
        ctx.arrays.set_element(entity, i as u32, visible);
    }

    // Return subarray of actual length
    let result = PsObject {
        value: PsValue::Array {
            entity,
            start: 0,
            len: e_len as u32,
        },
        flags: obj.flags,
    };
    ctx.o_stack.push(result)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use stet_core::object::EntityId;

    fn test_ctx() -> Context {
        Context::new()
    }

    #[test]
    fn test_if_true() {
        let mut ctx = test_ctx();
        let proc = PsObject::procedure(EntityId(0), 0);
        ctx.o_stack.push(PsObject::bool(true)).unwrap();
        ctx.o_stack.push(proc).unwrap();
        op_if(&mut ctx).unwrap();
        // Proc should be on exec stack
        assert!(!ctx.e_stack.is_empty());
    }

    #[test]
    fn test_if_false() {
        let mut ctx = test_ctx();
        let proc = PsObject::procedure(EntityId(0), 0);
        ctx.o_stack.push(PsObject::bool(false)).unwrap();
        ctx.o_stack.push(proc).unwrap();
        op_if(&mut ctx).unwrap();
        assert!(ctx.e_stack.is_empty());
    }

    #[test]
    fn test_ifelse_true() {
        let mut ctx = test_ctx();
        let proc_t = PsObject::procedure(EntityId(0), 0);
        let proc_f = PsObject::procedure(EntityId(1), 0);
        ctx.o_stack.push(PsObject::bool(true)).unwrap();
        ctx.o_stack.push(proc_t).unwrap();
        ctx.o_stack.push(proc_f).unwrap();
        op_ifelse(&mut ctx).unwrap();
        // True proc should be on exec stack
        let on_estack = ctx.e_stack.pop().unwrap();
        match on_estack.value {
            PsValue::Array { entity, .. } => assert_eq!(entity, EntityId(0)),
            _ => panic!("Expected array"),
        }
    }

    #[test]
    fn test_ifelse_false() {
        let mut ctx = test_ctx();
        let proc_t = PsObject::procedure(EntityId(0), 0);
        let proc_f = PsObject::procedure(EntityId(1), 0);
        ctx.o_stack.push(PsObject::bool(false)).unwrap();
        ctx.o_stack.push(proc_t).unwrap();
        ctx.o_stack.push(proc_f).unwrap();
        op_ifelse(&mut ctx).unwrap();
        let on_estack = ctx.e_stack.pop().unwrap();
        match on_estack.value {
            PsValue::Array { entity, .. } => assert_eq!(entity, EntityId(1)),
            _ => panic!("Expected array"),
        }
    }

    #[test]
    fn test_exec() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(42)).unwrap();
        op_exec(&mut ctx).unwrap();
        assert!(ctx.o_stack.is_empty());
        assert!(!ctx.e_stack.is_empty());
    }
}
