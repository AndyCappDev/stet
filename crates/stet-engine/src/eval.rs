// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Core execution engine — the eval loop that drives PostScript execution.

use stet_core::context::Context;
use stet_core::dict::DictKey;
use stet_core::error::PsError;
use stet_core::object::{EntityId, ObjFlags, PsObject, PsValue};
use stet_core::tokenizer::{Token, Tokenizer, stream_next_token};

/// Main eval loop: pop objects from the execution stack and execute them.
///
/// All PostScript errors are routed through `dispatch_error` → errordict
/// so they produce standard PS error output. Only `Quit` and un-caught
/// `Stop` propagate to the caller.
pub fn eval(ctx: &mut Context) -> Result<(), PsError> {
    while let Some(mut obj) = ctx.e_stack.try_pop() {
        if let Some(ref flag) = ctx.interrupt_flag
            && flag.load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(PsError::Quit);
        }
        // Deferred objects (nested procs from exec_procedure) → push to operand stack
        // Clear the deferred flag so the executable flag is preserved.
        if obj.flags.is_deferred() {
            obj.flags.clear_deferred();
            if let Err(e) = ctx.o_stack.push(obj) {
                dispatch_error(ctx, &e)?;
            }
            continue;
        }

        // Literal objects (except internal markers) → push to operand stack
        if obj.flags.is_literal()
            && !matches!(
                obj.value,
                PsValue::Stopped
                    | PsValue::Loop(_)
                    | PsValue::HardReturn
                    | PsValue::DictEnd(_)
                    | PsValue::ExecArray { .. }
            )
        {
            if let Err(e) = ctx.o_stack.push(obj) {
                dispatch_error(ctx, &e)?;
            }
            continue;
        }

        match eval_one(ctx, obj) {
            Ok(()) => {}
            Err(PsError::Quit) => return Ok(()),
            Err(PsError::Stop) => {
                if unwind_to_stopped(ctx).is_ok() {
                    if let Err(e) = ctx.o_stack.push(PsObject::bool(true)) {
                        dispatch_error(ctx, &e)?;
                    }
                } else {
                    return Err(PsError::Stop);
                }
            }
            Err(PsError::Exit) => {
                if let Err(e) = unwind_to_loop(ctx) {
                    dispatch_error(ctx, &e)?;
                }
            }
            Err(e) => {
                dispatch_error(ctx, &e)?;
            }
        }
    }
    Ok(())
}

/// Synchronously execute a PostScript procedure and return.
///
/// Pushes the procedure onto the e_stack and runs the eval loop until the
/// e_stack returns to its original depth. Used by operators that need to
/// call PS procedures as callbacks (filter/image data sources, tint
/// transforms, BuildChar, etc.).
pub fn exec_sync(ctx: &mut Context, proc_obj: PsObject) -> Result<(), PsError> {
    let base_depth = ctx.e_stack.len();
    ctx.e_stack.push(proc_obj)?;

    while ctx.e_stack.len() > base_depth {
        if let Some(ref flag) = ctx.interrupt_flag
            && flag.load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(PsError::Quit);
        }
        let Some(mut obj) = ctx.e_stack.try_pop() else {
            break;
        };

        if obj.flags.is_deferred() {
            obj.flags.clear_deferred();
            ctx.o_stack.push(obj)?;
            continue;
        }

        if obj.flags.is_literal()
            && !matches!(
                obj.value,
                PsValue::Stopped
                    | PsValue::Loop(_)
                    | PsValue::HardReturn
                    | PsValue::DictEnd(_)
                    | PsValue::ExecArray { .. }
            )
        {
            ctx.o_stack.push(obj)?;
            continue;
        }

        match eval_one(ctx, obj) {
            Ok(()) => {}
            Err(PsError::Quit) => return Ok(()),
            Err(PsError::Stop) => {
                // Only unwind within our scope
                if unwind_to_stopped_bounded(ctx, base_depth).is_ok() {
                    ctx.o_stack.push(PsObject::bool(true))?;
                } else {
                    return Err(PsError::Stop);
                }
            }
            Err(PsError::Exit) => {
                if unwind_to_loop_bounded(ctx, base_depth).is_err() {
                    return Err(PsError::Exit);
                }
            }
            Err(e) => {
                dispatch_error(ctx, &e)?;
            }
        }
    }

    Ok(())
}

/// Unwind to nearest `Stopped` marker, but don't go below `min_depth`.
fn unwind_to_stopped_bounded(ctx: &mut Context, min_depth: usize) -> Result<(), PsError> {
    while ctx.e_stack.len() > min_depth {
        if let Some(obj) = ctx.e_stack.try_pop() {
            match obj.value {
                PsValue::Stopped => return Ok(()),
                PsValue::DictEnd(expected) => {
                    pop_dict_end(ctx, expected);
                }
                _ => {}
            }
        }
    }
    Err(PsError::Stop)
}

/// Unwind to nearest `Loop` marker, but don't go below `min_depth`.
fn unwind_to_loop_bounded(ctx: &mut Context, min_depth: usize) -> Result<(), PsError> {
    while ctx.e_stack.len() > min_depth {
        if let Some(obj) = ctx.e_stack.try_pop() {
            match obj.value {
                PsValue::Loop(_) => return Ok(()),
                PsValue::Stopped => {
                    ctx.e_stack.push(obj)?;
                    return Err(PsError::InvalidExit);
                }
                PsValue::DictEnd(expected) => {
                    pop_dict_end(ctx, expected);
                }
                _ => {}
            }
        }
    }
    Err(PsError::InvalidExit)
}

/// Process a single object from the execution stack.
///
/// Returns errors that the caller routes through `dispatch_error`.
fn eval_one(ctx: &mut Context, obj: PsObject) -> Result<(), PsError> {
    match obj.value {
        // Simple types always push (even if executable flag is set)
        PsValue::Int(_)
        | PsValue::Real(_)
        | PsValue::Bool(_)
        | PsValue::Null
        | PsValue::Mark
        | PsValue::DictMark => {
            ctx.o_stack.push(obj)?;
        }

        // Operators → dispatch (current_operator set lazily on error)
        PsValue::Operator(opcode) => {
            let func = ctx.operators[opcode.0 as usize].func;
            if let Err(e) = func(ctx) {
                ctx.current_operator = Some(ctx.operators[opcode.0 as usize].name);
                return Err(e);
            }
        }

        // Executable names → dictionary lookup
        PsValue::Name(name_id) => {
            let key = DictKey::Name(name_id);
            match ctx.dict_load(&key) {
                Some(val) => {
                    if val.flags.is_executable() {
                        ctx.e_stack.push(val)?;
                    } else {
                        ctx.o_stack.push(val)?;
                    }
                }
                None => {
                    ctx.current_operator = Some(name_id);
                    return Err(PsError::Undefined);
                }
            }
        }

        // Executable arrays (procedures) → push elements in reverse
        PsValue::Array { entity, start, len } => {
            exec_procedure(ctx, entity, start, len)?;
        }

        // Executable strings → tokenize one token, advance start/len in place.
        // SAFETY: String store data is not modified or reallocated during
        // tokenization — we use a raw pointer to break the borrow conflict
        // with &mut Context needed by scan_token_from_bytes.
        PsValue::String { entity, start, len } => {
            let bytes = ctx.strings.get(entity, start, len);
            let (ptr, byte_len) = (bytes.as_ptr(), bytes.len());
            let bytes = unsafe { std::slice::from_raw_parts(ptr, byte_len) };
            if let Some((tok_obj, consumed, is_immediate, auto_exec)) =
                scan_token_from_bytes(ctx, bytes)?
            {
                let newlines = count_newlines(&bytes[..consumed]);
                ctx.current_source_line += newlines;

                // Push continuation with advanced start and reduced len
                let remaining = len - consumed as u32;
                if remaining > 0 {
                    ctx.e_stack.push(PsObject {
                        value: PsValue::String {
                            entity,
                            start: start + consumed as u32,
                            len: remaining,
                        },
                        flags: ObjFlags::executable_composite(),
                    })?;
                }

                if auto_exec {
                    ctx.e_stack.push(tok_obj)?;
                } else {
                    dispatch_scanned_token(ctx, tok_obj, is_immediate)?;
                }
            }
        }

        // Procedure cursor → tight inner loop over consecutive elements.
        //
        // Instead of processing one element per eval loop iteration (which
        // requires pushing/popping the cursor on e_stack each time), we loop
        // through elements inline. We only yield back to the eval loop when
        // an operator pushes to e_stack (control flow like `if`, `exec`,
        // loops) or when we encounter a non-operator executable value.
        PsValue::ExecArray {
            entity,
            start,
            len,
            pos,
        } => {
            let mut cur_pos = pos;
            let ea_flags = obj.flags;

            // Dispatch an operator inline. Returns true if the eval loop
            // should take over (operator pushed to e_stack). On error,
            // pushes continuation cursor so error recovery can resume.
            //
            // current_operator is set lazily — only on error — to avoid
            // 2 writes per operator call in the hot path.
            macro_rules! dispatch_op {
                ($opcode:expr) => {{
                    let e_depth = ctx.e_stack.len();
                    let func = ctx.operators[$opcode.0 as usize].func;
                    let result = func(ctx);
                    match result {
                        Ok(()) => {
                            if ctx.e_stack.len() > e_depth {
                                // Operator pushed to e_stack (if/exec/loop/etc).
                                // Insert our continuation below what was pushed.
                                if cur_pos < len {
                                    ctx.e_stack.insert_at(
                                        e_depth,
                                        PsObject {
                                            value: PsValue::ExecArray {
                                                entity,
                                                start,
                                                len,
                                                pos: cur_pos,
                                            },
                                            flags: ea_flags,
                                        },
                                    )?;
                                }
                                true // yield to eval loop
                            } else {
                                false // continue inner loop
                            }
                        }
                        Err(e) => {
                            // Set current_operator lazily on error
                            ctx.current_operator = Some(ctx.operators[$opcode.0 as usize].name);
                            if cur_pos < len {
                                ctx.e_stack.push(PsObject {
                                    value: PsValue::ExecArray {
                                        entity,
                                        start,
                                        len,
                                        pos: cur_pos,
                                    },
                                    flags: ea_flags,
                                })?;
                            }
                            return Err(e);
                        }
                    }
                }};
            }

            'ea_loop: loop {
                let elem = ctx.arrays.get_element(entity, start + cur_pos);
                cur_pos += 1;

                match elem.value {
                    PsValue::Operator(opcode) => {
                        if dispatch_op!(opcode) {
                            break 'ea_loop;
                        }
                    }

                    PsValue::Name(name_id) if elem.flags.is_executable() => {
                        // Inline name cache check — avoids DictKey construction
                        // and dict_load function call overhead on cache hits.
                        let idx = name_id.0 as usize;
                        let val = if idx < ctx.name_resolve_cache.len() {
                            let (ver, cached) = ctx.name_resolve_cache[idx];
                            if ver == ctx.dict_version {
                                cached
                            } else {
                                match ctx.dict_load(&DictKey::Name(name_id)) {
                                    Some(v) => v,
                                    None => {
                                        ctx.current_operator = Some(name_id);
                                        if cur_pos < len {
                                            ctx.e_stack.push(PsObject {
                                                value: PsValue::ExecArray {
                                                    entity,
                                                    start,
                                                    len,
                                                    pos: cur_pos,
                                                },
                                                flags: ea_flags,
                                            })?;
                                        }
                                        return Err(PsError::Undefined);
                                    }
                                }
                            }
                        } else {
                            match ctx.dict_load(&DictKey::Name(name_id)) {
                                Some(v) => v,
                                None => {
                                    ctx.current_operator = Some(name_id);
                                    if cur_pos < len {
                                        ctx.e_stack.push(PsObject {
                                            value: PsValue::ExecArray {
                                                entity,
                                                start,
                                                len,
                                                pos: cur_pos,
                                            },
                                            flags: ea_flags,
                                        })?;
                                    }
                                    return Err(PsError::Undefined);
                                }
                            }
                        };

                        match val.value {
                            PsValue::Operator(opcode) => {
                                if dispatch_op!(opcode) {
                                    break 'ea_loop;
                                }
                            }
                            _ => {
                                // Non-operator: push cursor and dispatch value
                                if cur_pos < len {
                                    ctx.e_stack.push(PsObject {
                                        value: PsValue::ExecArray {
                                            entity,
                                            start,
                                            len,
                                            pos: cur_pos,
                                        },
                                        flags: ea_flags,
                                    })?;
                                }
                                if val.flags.is_executable() {
                                    ctx.e_stack.push(val)?;
                                } else {
                                    ctx.o_stack.push(val)?;
                                }
                                break 'ea_loop;
                            }
                        }
                    }

                    _ => {
                        if elem.is_array_type() && elem.flags.is_executable() {
                            // Nested procedure body → o_stack (for if/exec to pick up)
                            ctx.o_stack.push(elem)?;
                        } else if matches!(
                            elem.value,
                            PsValue::Int(_)
                                | PsValue::Real(_)
                                | PsValue::Bool(_)
                                | PsValue::Null
                                | PsValue::Mark
                                | PsValue::DictMark
                        ) || elem.flags.is_literal()
                        {
                            // Literals and simple values → o_stack directly
                            ctx.o_stack.push(elem)?;
                        } else {
                            // Rare: executable non-name/non-operator → e_stack, yield
                            if cur_pos < len {
                                ctx.e_stack.push(PsObject {
                                    value: PsValue::ExecArray {
                                        entity,
                                        start,
                                        len,
                                        pos: cur_pos,
                                    },
                                    flags: ea_flags,
                                })?;
                            }
                            ctx.e_stack.push(elem)?;
                            break 'ea_loop;
                        }
                    }
                }

                if cur_pos >= len {
                    break;
                }
            }
        }

        // Executable file → tokenize one token from FileStore
        PsValue::File(file_entity) => {
            // Flush deferred newlines from the previous token so `line`
            // reports the line this token is on, not the previous one.
            ctx.files.flush_pending_newlines(file_entity);

            // Fast path: StringSource files have all data in memory.
            // We grab a raw pointer to avoid copying the remaining bytes on
            // every token read (which was an O(n^2) bottleneck for large files).
            // SAFETY: The StringSource data Vec is not modified or reallocated
            // during tokenization — only `pos` is advanced afterward.
            let remaining = ctx.files.get_remaining_bytes(file_entity);
            let (ptr, len) = (remaining.as_ptr(), remaining.len());
            if len > 0 {
                let remaining = unsafe { std::slice::from_raw_parts(ptr, len) };
                if let Some((tok_obj, consumed, is_immediate, auto_exec)) =
                    scan_token_from_bytes(ctx, remaining)?
                {
                    let newlines = count_newlines(&remaining[..consumed]);
                    ctx.current_source_line += newlines;
                    ctx.files.add_pending_newlines(file_entity, newlines);
                    ctx.files.advance_position(file_entity, consumed);
                    if consumed < remaining.len() {
                        ctx.e_stack.push(obj)?;
                    }
                    if auto_exec {
                        ctx.e_stack.push(tok_obj)?;
                    } else {
                        dispatch_scanned_token(ctx, tok_obj, is_immediate)?;
                    }
                }
            } else if ctx.files.is_readable(file_entity) {
                // Streaming path: filter/real files — byte-at-a-time tokenization
                if let Some((token, newlines)) = stream_next_token(&mut ctx.files, file_entity)? {
                    ctx.current_source_line += newlines;
                    ctx.files.add_pending_newlines(file_entity, newlines);
                    let is_immediate = matches!(token, Token::ImmediateName(_));
                    let (tok_obj, auto_exec) = if let Token::BinaryTokenByte(tag) = token {
                        let result =
                            stet_core::binary_token::parse_from_stream(ctx, tag, file_entity)?;
                        match result {
                            stet_core::binary_token::BinaryTokenResult::Single(o) => (o, false),
                            stet_core::binary_token::BinaryTokenResult::Sequence(o) => (o, true),
                        }
                    } else if matches!(token, Token::ProcBegin) {
                        (stream_parse_procedure(ctx, file_entity)?, false)
                    } else {
                        (token_to_object(ctx, token)?, false)
                    };
                    if ctx.files.is_readable(file_entity) {
                        ctx.e_stack.push(obj)?;
                    }
                    if auto_exec {
                        ctx.e_stack.push(tok_obj)?;
                    } else {
                        dispatch_scanned_token(ctx, tok_obj, is_immediate)?;
                    }
                }
            }
        }

        // Stopped marker → push false (normal completion, no stop)
        PsValue::Stopped => {
            ctx.o_stack.push(PsObject::bool(false))?;
        }

        // Loop state → advance loop
        PsValue::Loop(loop_entity) => {
            advance_loop(ctx, loop_entity)?;
        }

        // HardReturn → just consume (exit current procedure level)
        PsValue::HardReturn => {}

        // DictEnd → conditionally pop the dict stack (resource operator cleanup)
        PsValue::DictEnd(expected) => {
            pop_dict_end(ctx, expected);
        }

        // Everything else → push to operand stack
        _ => {
            ctx.o_stack.push(obj)?;
        }
    }
    Ok(())
}

/// Push a procedure cursor onto the execution stack.
///
/// Instead of expanding all procedure elements onto the e_stack, we push a
/// single `ExecArray` cursor that the eval loop advances one element at a time.
/// Each procedure occupies exactly one exec-stack item.
fn exec_procedure(
    ctx: &mut Context,
    entity: EntityId,
    start: u32,
    len: u32,
) -> Result<(), PsError> {
    if len == 0 {
        return Ok(());
    }
    ctx.e_stack.push(PsObject {
        value: PsValue::ExecArray {
            entity,
            start,
            len,
            pos: 0,
        },
        flags: ObjFlags::executable_composite(),
    })?;
    Ok(())
}

/// Scan one token from a byte slice, handling procedures and immediate names.
///
/// Returns `(token_object, bytes_consumed, is_immediate, auto_exec)` or `None` at EOF.
/// `auto_exec` is true for BOS sequences which should be pushed to e_stack.
fn scan_token_from_bytes(
    ctx: &mut Context,
    bytes: &[u8],
) -> Result<Option<(PsObject, usize, bool, bool)>, PsError> {
    let mut tokenizer = Tokenizer::new(bytes);
    match tokenizer.next_token()? {
        Some(Token::BinaryTokenByte(tag)) => {
            let pos = tokenizer.position();
            let (result, consumed) =
                stet_core::binary_token::parse_from_slice(ctx, tag, &bytes[pos..])?;
            let total = pos + consumed;
            match result {
                stet_core::binary_token::BinaryTokenResult::Single(obj) => {
                    Ok(Some((obj, total, false, false)))
                }
                stet_core::binary_token::BinaryTokenResult::Sequence(obj) => {
                    Ok(Some((obj, total, false, true)))
                }
            }
        }
        Some(token) => {
            let is_immediate = matches!(token, Token::ImmediateName(_));
            // Numbers and executable names: consume one trailing whitespace (PLRM)
            let eats_whitespace =
                matches!(token, Token::Int(_) | Token::Real(_) | Token::Name(_, _));
            let tok_obj = if matches!(token, Token::ProcBegin) {
                parse_procedure(ctx, &mut tokenizer)?
            } else {
                token_to_object(ctx, token)?
            };
            let mut consumed = tokenizer.position();
            if eats_whitespace && consumed < bytes.len() && is_ps_whitespace(bytes[consumed]) {
                consumed += 1;
            }
            Ok(Some((tok_obj, consumed, is_immediate, false)))
        }
        None => Ok(None),
    }
}

/// PostScript whitespace check for trailing-whitespace consumption.
fn is_ps_whitespace(b: u8) -> bool {
    matches!(b, b'\0' | b'\t' | b'\n' | 0x0C | b'\r' | b' ')
}

/// Dispatch a freshly-scanned token to the appropriate stack.
///
/// Executable names → e_stack (for dict lookup), everything else → o_stack.
/// Immediate names (`//name`) skip execution since they were already resolved.
fn dispatch_scanned_token(
    ctx: &mut Context,
    tok_obj: PsObject,
    is_immediate: bool,
) -> Result<(), PsError> {
    if matches!(tok_obj.value, PsValue::Name(_)) && tok_obj.flags.is_executable() && !is_immediate {
        ctx.e_stack.push(tok_obj)?;
    } else {
        ctx.o_stack.push(tok_obj)?;
    }
    Ok(())
}

/// Parse a `{ ... }` procedure body from a streaming file source.
///
/// Called when the streaming tokenizer returns `ProcBegin`. Reads tokens
/// from the file byte-by-byte until the matching `}`.
fn stream_parse_procedure(ctx: &mut Context, file_entity: EntityId) -> Result<PsObject, PsError> {
    let mut elements = Vec::new();

    loop {
        match stream_next_token(&mut ctx.files, file_entity)? {
            None => return Err(PsError::SyntaxError), // unterminated
            Some((Token::ProcEnd, _)) => break,
            Some((Token::ProcBegin, _)) => {
                let nested = stream_parse_procedure(ctx, file_entity)?;
                elements.push(nested);
            }
            Some((Token::BinaryTokenByte(tag), _)) => {
                let result = stet_core::binary_token::parse_from_stream(ctx, tag, file_entity)?;
                let obj = match result {
                    stet_core::binary_token::BinaryTokenResult::Single(o) => o,
                    stet_core::binary_token::BinaryTokenResult::Sequence(o) => o,
                };
                elements.push(obj);
            }
            Some((token, _)) => {
                let obj = token_to_object(ctx, token)?;
                elements.push(obj);
            }
        }
    }

    let len = elements.len();
    let save_level = ctx.save_stack.current_level();
    let global = ctx.vm_alloc_mode;
    let created = ctx.save_stack.last_save_id();
    let entity = ctx.arrays.allocate_with(len, save_level, global, created);
    let dest = ctx.arrays.get_mut(entity, 0, len as u32);
    dest.copy_from_slice(&elements);

    let mut obj = PsObject::procedure(entity, len as u32);
    if global {
        obj.flags = ObjFlags::new(ObjFlags::ACCESS_UNLIMITED, true, true, true);
    }
    Ok(obj)
}

/// Count newline characters in a byte slice.
/// Handles CR, LF, and CR-LF (treated as one newline).
fn count_newlines(bytes: &[u8]) -> u32 {
    let mut count = 0u32;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\r' => {
                count += 1;
                // CR-LF counts as one newline
                if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                    i += 1;
                }
            }
            b'\n' | b'\x0c' => {
                count += 1;
            }
            _ => {}
        }
        i += 1;
    }
    count
}

/// Advance a loop iteration (for, repeat, loop, forall).
fn advance_loop(ctx: &mut Context, loop_entity: EntityId) -> Result<(), PsError> {
    use stet_core::context::LoopType;

    let loop_state = ctx.get_loop(loop_entity);
    let loop_type = match loop_state.loop_type {
        LoopType::For => 0,
        LoopType::Repeat => 1,
        LoopType::Loop => 2,
        LoopType::Forall => 3,
        LoopType::PathForall => 4,
    };
    let proc_entity = loop_state.proc_entity;
    let proc_start = loop_state.proc_start;
    let proc_len = loop_state.proc_len;

    match loop_type {
        0 => {
            // For loop
            let counter = ctx.get_loop(loop_entity).counter;
            let increment = ctx.get_loop(loop_entity).increment;
            let limit = ctx.get_loop(loop_entity).limit;
            let use_int = ctx.get_loop(loop_entity).use_int;

            let done = if increment > 0.0 {
                counter > limit
            } else {
                counter < limit
            };

            if done {
                return Ok(());
            }

            // Push counter value
            if use_int {
                ctx.o_stack.push(PsObject::int(counter as i32))?;
            } else {
                ctx.o_stack.push(PsObject::real(counter))?;
            }

            // Update counter for next iteration
            let new_counter = counter + increment;
            ctx.get_loop_mut(loop_entity).counter = new_counter;

            // Push loop marker back, then procedure
            ctx.e_stack.push(PsObject::loop_mark(loop_entity))?;
            exec_procedure(ctx, proc_entity, proc_start, proc_len)?;
        }
        1 => {
            // Repeat loop
            let counter = ctx.get_loop(loop_entity).counter;
            if counter <= 0.0 {
                return Ok(());
            }
            ctx.get_loop_mut(loop_entity).counter = counter - 1.0;
            ctx.e_stack.push(PsObject::loop_mark(loop_entity))?;
            exec_procedure(ctx, proc_entity, proc_start, proc_len)?;
        }
        2 => {
            // Infinite loop
            ctx.e_stack.push(PsObject::loop_mark(loop_entity))?;
            exec_procedure(ctx, proc_entity, proc_start, proc_len)?;
        }
        3 => {
            // Forall
            advance_forall(ctx, loop_entity, proc_entity, proc_start, proc_len)?;
        }
        4 => {
            // PathForall
            advance_pathforall(ctx, loop_entity)?;
        }
        _ => unreachable!(),
    }

    Ok(())
}

/// Advance a forall iteration.
fn advance_forall(
    ctx: &mut Context,
    loop_entity: EntityId,
    proc_entity: EntityId,
    proc_start: u32,
    proc_len: u32,
) -> Result<(), PsError> {
    let source = ctx.get_loop(loop_entity).source;
    let index = ctx.get_loop(loop_entity).index;

    match source.value {
        PsValue::Array { entity, start, len } | PsValue::PackedArray { entity, start, len } => {
            if index >= len {
                return Ok(());
            }
            let elem = ctx.arrays.get_element(entity, start + index);
            ctx.o_stack.push(elem)?;
            ctx.get_loop_mut(loop_entity).index = index + 1;
            ctx.e_stack.push(PsObject::loop_mark(loop_entity))?;
            exec_procedure(ctx, proc_entity, proc_start, proc_len)?;
        }
        PsValue::String { entity, start, len } => {
            if index >= len {
                return Ok(());
            }
            let byte = ctx.strings.get_byte(entity, start + index);
            ctx.o_stack.push(PsObject::int(byte as i32))?;
            ctx.get_loop_mut(loop_entity).index = index + 1;
            ctx.e_stack.push(PsObject::loop_mark(loop_entity))?;
            exec_procedure(ctx, proc_entity, proc_start, proc_len)?;
        }
        PsValue::Dict(dict_entity) => {
            // Keys were snapshotted at forall creation time.
            // SAFETY: dict_keys is always Some for dict forall loops.
            let keys = ctx.get_loop(loop_entity).dict_keys.as_ref().unwrap();
            if (index as usize) >= keys.len() {
                return Ok(());
            }
            let key = keys[index as usize].clone();
            let val = ctx.dicts.get(dict_entity, &key).unwrap_or(PsObject::null());

            // Push key and value
            let key_obj = dict_key_to_object(ctx, &key);
            ctx.o_stack.push(key_obj)?;
            ctx.o_stack.push(val)?;

            ctx.get_loop_mut(loop_entity).index = index + 1;
            ctx.e_stack.push(PsObject::loop_mark(loop_entity))?;
            exec_procedure(ctx, proc_entity, proc_start, proc_len)?;
        }
        _ => return Err(PsError::TypeCheck),
    }

    Ok(())
}

/// Advance a pathforall iteration: process one path segment per call.
fn advance_pathforall(ctx: &mut Context, loop_entity: EntityId) -> Result<(), PsError> {
    use stet_core::geometry::PathSegment;

    let index = ctx.get_loop(loop_entity).index as usize;

    // Check if we've exhausted all segments
    let seg_len = ctx
        .get_loop(loop_entity)
        .path_segments
        .as_ref()
        .map_or(0, |s| s.len());
    if index >= seg_len {
        return Ok(());
    }

    // Extract segment data, ictm, and proc for this iteration
    let loop_state = ctx.get_loop(loop_entity);
    let seg = loop_state.path_segments.as_ref().unwrap()[index].clone();
    let ictm = loop_state.path_ictm.unwrap();
    let procs = loop_state.path_procs.unwrap();

    // Determine which proc to call and push the arguments
    let proc = match seg {
        PathSegment::MoveTo(dx, dy) => {
            let (ux, uy) = ictm.transform_point(dx, dy);
            ctx.o_stack.push(PsObject::real(ux))?;
            ctx.o_stack.push(PsObject::real(uy))?;
            procs[0] // move_proc
        }
        PathSegment::LineTo(dx, dy) => {
            let (ux, uy) = ictm.transform_point(dx, dy);
            ctx.o_stack.push(PsObject::real(ux))?;
            ctx.o_stack.push(PsObject::real(uy))?;
            procs[1] // line_proc
        }
        PathSegment::CurveTo {
            x1,
            y1,
            x2,
            y2,
            x3,
            y3,
        } => {
            let (ux1, uy1) = ictm.transform_point(x1, y1);
            let (ux2, uy2) = ictm.transform_point(x2, y2);
            let (ux3, uy3) = ictm.transform_point(x3, y3);
            ctx.o_stack.push(PsObject::real(ux1))?;
            ctx.o_stack.push(PsObject::real(uy1))?;
            ctx.o_stack.push(PsObject::real(ux2))?;
            ctx.o_stack.push(PsObject::real(uy2))?;
            ctx.o_stack.push(PsObject::real(ux3))?;
            ctx.o_stack.push(PsObject::real(uy3))?;
            procs[2] // curve_proc
        }
        PathSegment::ClosePath => {
            procs[3] // close_proc
        }
    };

    // Advance index for next iteration
    ctx.get_loop_mut(loop_entity).index = (index + 1) as u32;

    // Push loop marker back, then the callback procedure
    ctx.e_stack.push(PsObject::loop_mark(loop_entity))?;
    let (proc_entity, proc_start, proc_len) = match proc.value {
        PsValue::Array { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };
    exec_procedure(ctx, proc_entity, proc_start, proc_len)?;

    Ok(())
}

/// Convert a DictKey back to a PsObject for forall iteration.
fn dict_key_to_object(ctx: &mut Context, key: &DictKey) -> PsObject {
    match key {
        DictKey::Name(id) => PsObject::name_lit(*id),
        DictKey::Int(v) => PsObject::int(*v),
        DictKey::Real(bits) => PsObject::real(f64::from_bits(*bits)),
        DictKey::Bool(v) => PsObject::bool(*v),
        DictKey::String(bytes) => {
            let entity = ctx.strings.allocate_from(bytes);
            PsObject::string(entity, bytes.len() as u32)
        }
        DictKey::Operator(op) => {
            use stet_core::object::OpCode;
            PsObject::operator(OpCode(*op))
        }
        DictKey::Identity(eid, _start, len) => {
            // Return as array (best approximation for identity keys)
            PsObject::array(EntityId(*eid), *len)
        }
    }
}

/// Dispatch a PostScript error via errordict.
///
/// PLRM error dispatch:
/// 1. Look up the error name in errordict → get handler procedure
/// 2. Handler populates `$error` dict and calls `stop`
/// 3. If no handler found, or if already in error handler, fall back to eprintln
fn dispatch_error(ctx: &mut Context, error: &PsError) -> Result<(), PsError> {
    // Guard against infinite recursion
    if ctx.in_error_handler {
        use std::io::Write;
        let _ = writeln!(ctx.stdout, "Error (in handler): {}", error);
        return Ok(());
    }

    let error_name = error.to_string();
    let error_name_id = ctx.names.intern(error_name.as_bytes());
    let error_key = DictKey::Name(error_name_id);

    // Look up error handler in errordict
    if let Some(handler) = ctx.dicts.get(ctx.errordict, &error_key)
        && handler.flags.is_executable()
    {
        ctx.in_error_handler = true;

        // Push offending command name on operand stack — the errordict handler
        // pushes the error name, then `.error` expects `command errorname` on
        // the stack. Use the current operator if available, otherwise the error
        // name itself.
        let cmd_name = ctx.current_operator.unwrap_or(error_name_id);
        ctx.current_operator = None;
        ctx.o_stack.push(PsObject::name_lit(cmd_name))?;

        // Push handler on e_stack for natural execution by the eval loop.
        // The handler (e.g. `{ /undefined //.error exec }`) will populate
        // $error and call `stop`, which propagates naturally to be caught by
        // any enclosing `stopped` context.
        ctx.e_stack.push(handler)?;

        ctx.in_error_handler = false;
        return Ok(());
    }

    // Fallback: no handler found, print to stdout (like stderr)
    use std::io::Write;
    let _ = writeln!(ctx.stdout, "Error: {}", error);
    Ok(())
}

/// Unwind execution stack to the nearest `Stopped` marker.
/// DictEnd markers encountered during unwinding conditionally pop the dict
/// stack (only if the expected dict is still on top).
fn unwind_to_stopped(ctx: &mut Context) -> Result<(), PsError> {
    while let Some(obj) = ctx.e_stack.try_pop() {
        match obj.value {
            PsValue::Stopped => return Ok(()),
            PsValue::DictEnd(expected) => {
                pop_dict_end(ctx, expected);
            }
            _ => {}
        }
    }
    // No stopped marker found — propagate
    Err(PsError::Stop)
}

/// Unwind execution stack to the nearest `Loop` marker.
fn unwind_to_loop(ctx: &mut Context) -> Result<(), PsError> {
    while let Some(obj) = ctx.e_stack.try_pop() {
        match obj.value {
            PsValue::Loop(_) => return Ok(()),
            PsValue::Stopped => {
                // Don't pop past stopped — push it back and dispatch error
                ctx.e_stack.push(obj)?;
                return Err(PsError::InvalidExit);
            }
            PsValue::DictEnd(expected) => {
                pop_dict_end(ctx, expected);
            }
            _ => {}
        }
    }
    Err(PsError::InvalidExit)
}

/// Conditionally pop the dict stack for a DictEnd marker.
/// Only pops if the top of d_stack is still the expected entity — the PS
/// procedure may have already called `end` to clean up (e.g., on error paths).
fn pop_dict_end(ctx: &mut Context, expected: EntityId) {
    if ctx.d_stack.last() == Some(&expected) {
        ctx.d_stack.pop();
        ctx.invalidate_name_cache();
    }
}

/// Convert a token to a PsObject. Delegates to `Context::token_to_object`.
pub fn token_to_object(ctx: &mut Context, token: Token) -> Result<PsObject, PsError> {
    ctx.token_to_object(token)
}

/// Tokenize a byte stream and execute it.
///
/// Creates a file-backed source in the FileStore and pushes it as an
/// executable `File` on the exec stack. The eval loop tokenizes and
/// executes one token at a time, ensuring `//name` immediate lookups
/// see earlier definitions and ALL errors route through errordict.
///
/// Using a File (not an executable string) means `currentfile` can find
/// the source — this is correct for top-level execution of PS files.
pub fn parse_and_exec(ctx: &mut Context, source: &[u8]) -> Result<(), PsError> {
    let file_entity = ctx.files.create_string_source(source.to_vec());
    ctx.e_stack.push(PsObject {
        value: PsValue::File(file_entity),
        flags: ObjFlags::executable_composite(),
    })?;
    eval(ctx)
}

/// Execute PostScript source loaded from a named file.
///
/// Like `parse_and_exec`, but records the file path on the StringSource
/// entity so that `resolve_filename` can find the file's parent directory
/// when resolving relative paths in nested `run`/`file` calls.
pub fn parse_and_exec_file(ctx: &mut Context, source: &[u8], path: &str) -> Result<(), PsError> {
    let file_entity = ctx.files.create_string_source(source.to_vec());
    // Record the canonical path so resolve_filename can extract its directory.
    let canonical = std::path::Path::new(path)
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(path));
    ctx.files
        .set_name(file_entity, canonical.to_string_lossy().to_string());
    ctx.e_stack.push(PsObject {
        value: PsValue::File(file_entity),
        flags: ObjFlags::executable_composite(),
    })?;
    eval(ctx)
}

/// Parse a `{ ... }` procedure body (recursive for nested procedures).
fn parse_procedure(ctx: &mut Context, tokenizer: &mut Tokenizer) -> Result<PsObject, PsError> {
    let mut elements = Vec::new();

    loop {
        match tokenizer.next_token()? {
            Some(Token::ProcEnd) => break,
            Some(Token::ProcBegin) => {
                let nested = parse_procedure(ctx, tokenizer)?;
                elements.push(nested);
            }
            Some(Token::BinaryTokenByte(tag)) => {
                let pos = tokenizer.position();
                // SAFETY: tokenizer borrows from a stable slice; we access the
                // underlying input via position.
                let input_bytes = tokenizer.remaining_from(pos);
                let (result, consumed) =
                    stet_core::binary_token::parse_from_slice(ctx, tag, input_bytes)?;
                tokenizer.advance(consumed);
                let obj = match result {
                    stet_core::binary_token::BinaryTokenResult::Single(o) => o,
                    stet_core::binary_token::BinaryTokenResult::Sequence(o) => o,
                };
                elements.push(obj);
            }
            Some(token) => {
                let obj = token_to_object(ctx, token)?;
                elements.push(obj);
            }
            None => return Err(PsError::SyntaxError), // unterminated procedure
        }
    }

    let len = elements.len();
    let save_level = ctx.save_stack.current_level();
    let global = ctx.vm_alloc_mode;
    let created = ctx.save_stack.last_save_id();
    let entity = ctx.arrays.allocate_with(len, save_level, global, created);
    let dest = ctx.arrays.get_mut(entity, 0, len as u32);
    dest.copy_from_slice(&elements);

    let mut obj = PsObject::procedure(entity, len as u32);
    if global {
        obj.flags = ObjFlags::new(ObjFlags::ACCESS_UNLIMITED, true, true, true);
    }
    Ok(obj)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_parse_procedure() {
        let mut ctx = Context::new();
        let mut tokenizer = Tokenizer::new(b"1 2 add }");
        let proc_obj = parse_procedure(&mut ctx, &mut tokenizer).unwrap();
        assert!(proc_obj.flags.is_executable());
        assert!(proc_obj.is_array_type());
        match proc_obj.value {
            PsValue::Array { len, .. } => assert_eq!(len, 3),
            _ => panic!("Expected array"),
        }
    }

    #[test]
    fn test_nested_procedure() {
        let mut ctx = Context::new();
        let mut tokenizer = Tokenizer::new(b"{ 1 add } exec }");
        let proc_obj = parse_procedure(&mut ctx, &mut tokenizer).unwrap();
        match proc_obj.value {
            PsValue::Array { len, .. } => assert_eq!(len, 2), // { 1 add } and exec
            _ => panic!("Expected array"),
        }
    }

    // --- Phase 2 integration tests (done-when criteria) ---

    fn setup_ctx() -> (Context, std::sync::Arc<std::sync::Mutex<Vec<u8>>>) {
        let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let writer = buf.clone();

        struct ArcWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl Write for ArcWriter {
            fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(data);
                Ok(data.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let mut ctx = Context::new_with_output(Box::new(ArcWriter(writer)));
        stet_ops::build_system_dict(&mut ctx);
        (ctx, buf)
    }

    fn run_ps(source: &[u8]) -> String {
        let (mut ctx, buf) = setup_ctx();
        parse_and_exec(&mut ctx, source).ok();
        String::from_utf8(buf.lock().unwrap().clone()).unwrap()
    }

    /// Done-when #2: save/restore reverts definitions.
    /// `save /s exch def /x 1 def s restore x =` — x should be undefined after restore
    #[test]
    fn test_save_restore_reverts_def() {
        let (mut ctx, buf) = setup_ctx();
        // Define x before save for baseline
        let result = parse_and_exec(&mut ctx, b"save /s exch def /x 1 def s restore");
        assert!(result.is_ok());
        // x should be undefined now — trying to use it should trigger an error
        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        // Verify x is not defined by checking that a lookup would fail
        let x_id = ctx.names.intern(b"x");
        let key = DictKey::Name(x_id);
        assert!(
            ctx.dict_load(&key).is_none(),
            "x should be undefined after restore"
        );
        drop(output);
    }

    /// Done-when #3: save/restore reverts array mutations.
    /// Define array before save, mutate after save, restore reverts mutation.
    #[test]
    fn test_save_restore_reverts_array() {
        let output = run_ps(b"[1 2 3] /a exch def save /s exch def a 1 99 put s restore a 1 get =");
        assert_eq!(output.trim(), "2");
    }

    /// Done-when #4: File round-trip
    #[test]
    fn test_file_round_trip() {
        // Use forward slashes even on Windows — PostScript string-literal
        // syntax treats `\` as an escape character, so a native Windows
        // path like `C:\Users\...` would be mangled inside `(...)`.
        // Rust's fs APIs on Windows accept `/` just fine.
        let path = std::env::temp_dir()
            .join("stet_phase2_file_test.txt")
            .to_string_lossy()
            .replace('\\', "/");
        let source = format!(
            "({}) (w) file /f exch def f (hello world) writestring f closefile \
             ({}) (r) file /f exch def f 11 string readstring pop print f closefile",
            path, path
        );
        let output = run_ps(source.as_bytes());
        assert_eq!(output, "hello world");
        std::fs::remove_file(&path).ok();
    }

    /// Done-when #5: `{ 1 0 div } stopped { (caught\n) print } if` → prints "caught"
    #[test]
    fn test_stopped_catches_error() {
        let output = run_ps(b"{ 1 0 div } stopped { (caught\n) print } if");
        assert_eq!(output.trim(), "caught");
    }

    /// Done-when #6: `true setglobal 3 array gcheck` → returns true
    #[test]
    fn test_setglobal_gcheck() {
        let output = run_ps(b"true setglobal 3 array gcheck =");
        assert_eq!(output.trim(), "true");
    }

    /// Done-when #7: `vmstatus` returns three integers
    #[test]
    fn test_vmstatus() {
        let output = run_ps(b"vmstatus = = =");
        let lines: Vec<&str> = output.trim().lines().collect();
        assert_eq!(lines.len(), 3, "vmstatus should push 3 values");
        // All should be parseable as integers
        for line in &lines {
            assert!(
                line.trim().parse::<i32>().is_ok(),
                "Expected integer: {}",
                line
            );
        }
    }

    /// Test that error dispatch populates $error and calls stop
    #[test]
    fn test_error_dispatch_stop() {
        let output = run_ps(b"{ 1 0 div } stopped { (error caught\n) print } if");
        assert!(output.contains("error caught"));
    }

    /// Test nested save/restore
    #[test]
    fn test_nested_save_restore() {
        let output = run_ps(
            b"/x 10 def \
              save /s1 exch def \
              /x 20 def \
              save /s2 exch def \
              /x 30 def \
              x = \
              s2 restore \
              x = \
              s1 restore \
              x =",
        );
        let lines: Vec<&str> = output.trim().lines().collect();
        assert_eq!(lines, vec!["30", "20", "10"]);
    }

    /// Test that global entities are not affected by restore.
    /// The array is allocated in global mode and defined before save,
    /// so the mutation survives restore.
    #[test]
    fn test_global_survives_restore() {
        let output = run_ps(
            b"true setglobal \
              3 array /ga exch def \
              false setglobal \
              save /s exch def \
              ga 0 42 put \
              s restore \
              ga 0 get =",
        );
        assert_eq!(output.trim(), "42");
    }
}
