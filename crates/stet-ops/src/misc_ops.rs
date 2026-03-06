// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Miscellaneous operators: bind, run, handleerror, join, and internal stubs.

use stet_core::context::Context;
use stet_core::dict::DictKey;
use stet_core::error::PsError;
use stet_core::object::{EntityId, NameId, ObjFlags, PsObject, PsValue};

/// `bind`: proc → proc (replace names in proc with operator objects)
pub fn op_bind(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    match obj.value {
        PsValue::Array { entity, start, len } if obj.flags.is_executable() => {
            bind_procedure(ctx, entity, start, len);
            Ok(())
        }
        PsValue::Array { .. } | PsValue::PackedArray { .. } => Ok(()), // literal array: no-op
        _ => Err(PsError::TypeCheck),
    }
}

fn bind_procedure(ctx: &mut Context, entity: stet_core::object::EntityId, start: u32, len: u32) {
    let mut needs_cow = false;
    // First pass: check if any names will be replaced
    for i in 0..len {
        let elem = ctx.arrays.get_element(entity, start + i);
        if let PsValue::Name(name_id) = elem.value
            && elem.flags.is_executable()
        {
            let key = DictKey::Name(name_id);
            if let Some(val) = ctx.dict_load(&key)
                && matches!(val.value, PsValue::Operator(_))
            {
                needs_cow = true;
                break;
            }
        }
    }
    if needs_cow {
        ctx.cow_check_array(entity);
    }

    for i in 0..len {
        let elem = ctx.arrays.get_element(entity, start + i);
        match elem.value {
            PsValue::Name(name_id) if elem.flags.is_executable() => {
                // Look up in dict stack — if it's an operator, replace
                let key = DictKey::Name(name_id);
                if let Some(val) = ctx.dict_load(&key)
                    && matches!(val.value, PsValue::Operator(_))
                {
                    ctx.arrays.set_element(entity, start + i, val);
                }
            }
            PsValue::Array {
                entity: sub_e,
                start: sub_s,
                len: sub_l,
            } if elem.flags.is_executable() => {
                // Recursively bind nested procedures
                bind_procedure(ctx, sub_e, sub_s, sub_l);
            }
            _ => {}
        }
    }
}

/// `handleerror`: — → — (print error information from `$error` dict)
pub fn op_handleerror(ctx: &mut Context) -> Result<(), PsError> {
    use std::io::Write;

    let newerror_id = ctx.names.intern(b"newerror");
    let errorname_id = ctx.names.intern(b"errorname");

    let has_error = ctx
        .dicts
        .get(ctx.dollar_error, &DictKey::Name(newerror_id))
        .and_then(|obj| match obj.value {
            PsValue::Bool(b) => Some(b),
            _ => None,
        })
        .unwrap_or(false);

    if !has_error {
        return Ok(());
    }

    let error_name = ctx
        .dicts
        .get(ctx.dollar_error, &DictKey::Name(errorname_id))
        .map(|obj| match obj.value {
            PsValue::Name(id) => String::from_utf8_lossy(ctx.names.get_bytes(id)).to_string(),
            _ => "unknown".to_string(),
        })
        .unwrap_or_else(|| "unknown".to_string());

    let command_id = ctx.names.intern(b"command");
    let command_name = ctx
        .dicts
        .get(ctx.dollar_error, &DictKey::Name(command_id))
        .map(|obj| match obj.value {
            PsValue::Name(id) => String::from_utf8_lossy(ctx.names.get_bytes(id)).to_string(),
            _ => "?".to_string(),
        })
        .unwrap_or_else(|| "?".to_string());

    let msg = format!("Error: /{} in {}\n", error_name, command_name);
    let _ = ctx.stdout.write_all(msg.as_bytes());
    let _ = ctx.stdout.flush();

    // Clear newerror
    ctx.dicts.put(
        ctx.dollar_error,
        DictKey::Name(newerror_id),
        PsObject::bool(false),
    );

    Ok(())
}

/// `run`: filename → — (execute a PostScript file)
///
/// Resolves paths using `resolve_filename`: tries the literal path, then
/// the directory of the currently executing file (exec stack walk), then
/// `resource_base_path`. This allows init scripts to use paths like
/// `(resources/Init/fontmapping.ps)` and PS files to `run` siblings via
/// relative paths.
pub fn op_run(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let filename = match obj.value {
        PsValue::String { entity, start, len } => {
            obj.flags.require_read()?;
            let bytes = ctx.strings.get(entity, start, len).to_vec();
            String::from_utf8(bytes).map_err(|_| PsError::SyntaxError)?
        }
        _ => return Err(PsError::TypeCheck),
    };

    ctx.o_stack.pop()?;

    // Check embedded files first (for WASM builds where no real filesystem exists).
    // get_embedded_file normalizes paths (strips leading "/", collapses "//").
    if let Some(data) = ctx.files.get_embedded_file(&filename) {
        let ps_data = stet_core::eps::strip_dos_eps_header(data);
        let file_entity = ctx.files.create_string_source(ps_data.to_vec());
        ctx.e_stack.push(PsObject {
            value: PsValue::File(file_entity),
            flags: stet_core::object::ObjFlags::executable_composite(),
        })?;
        return Ok(());
    }

    let resolved = crate::file_ops::resolve_filename(ctx, &filename);

    let source = std::fs::read(&resolved).map_err(|_| PsError::UndefinedFilename)?;

    // Strip DOS EPS binary header if present
    let ps_data = stet_core::eps::strip_dos_eps_header(&source);

    // Create a file-backed source and push it on the exec stack.
    // Using File (not String) means `currentfile` can find it —
    // matching PostForge's Run object behavior.
    // Set the resolved path as the file name so nested run/file calls
    // can find this file's directory on the exec stack.
    let file_entity = ctx.files.create_string_source(ps_data.to_vec());
    ctx.files.set_name(
        file_entity,
        std::path::Path::new(&resolved)
            .canonicalize()
            .unwrap_or_else(|_| std::path::PathBuf::from(&resolved))
            .to_string_lossy()
            .to_string(),
    );
    ctx.e_stack.push(PsObject {
        value: PsValue::File(file_entity),
        flags: stet_core::object::ObjFlags::executable_composite(),
    })?;
    Ok(())
}

/// `join`: separator array dest_string → result_string
///
/// PostForge extension. Joins the strings in array using separator as the
/// delimiter, writing the result into dest_string. Returns a substring of
/// dest_string containing the joined result.
pub fn op_join(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 3 {
        return Err(PsError::StackUnderflow);
    }
    let dest_obj = ctx.o_stack.peek(0)?;
    let array_obj = ctx.o_stack.peek(1)?;
    let sep_obj = ctx.o_stack.peek(2)?;

    // dest must be a string
    let (dest_entity, dest_start, dest_len) = match dest_obj.value {
        PsValue::String { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };

    // array must be an array
    let (arr_entity, arr_start, arr_len) = match array_obj.value {
        PsValue::Array { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };

    // separator must be a string
    let sep_bytes = match sep_obj.value {
        PsValue::String { entity, start, len } => ctx.strings.get(entity, start, len).to_vec(),
        _ => return Err(PsError::TypeCheck),
    };

    // Build the joined result
    let mut result = Vec::new();
    for i in 0..arr_len {
        if i > 0 {
            result.extend_from_slice(&sep_bytes);
        }
        let elem = ctx.arrays.get_element(arr_entity, arr_start + i);
        match elem.value {
            PsValue::String {
                entity: se,
                start: ss,
                len: sl,
            } => {
                let bytes = ctx.strings.get(se, ss, sl);
                result.extend_from_slice(bytes);
            }
            _ => return Err(PsError::TypeCheck),
        }
    }

    // Check that result fits in dest
    if result.len() > dest_len as usize {
        return Err(PsError::RangeCheck);
    }

    ctx.o_stack.pop()?; // dest
    ctx.o_stack.pop()?; // array
    ctx.o_stack.pop()?; // separator

    // Write result into dest string
    let dest_buf = ctx.strings.get_mut(dest_entity, dest_start, dest_len);
    dest_buf[..result.len()].copy_from_slice(&result);

    // Return a substring of dest with the actual result length
    let result_obj = PsObject {
        value: PsValue::String {
            entity: dest_entity,
            start: dest_start,
            len: result.len() as u32,
        },
        flags: dest_obj.flags,
    };
    ctx.o_stack.push(result_obj)?;
    Ok(())
}

/// `.nextfid`: — → fontID (return next font ID, increment counter)
pub fn op_nextfid(ctx: &mut Context) -> Result<(), PsError> {
    let fid = ctx.next_fid;
    ctx.next_fid += 1;
    ctx.o_stack.push(PsObject {
        value: PsValue::FontID(fid),
        flags: ObjFlags::literal(),
    })?;
    Ok(())
}

/// `.loadsystemfont`: name → path true | false
///
/// Return false — stet has no system font cache.
pub fn op_loadsystemfont(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    ctx.o_stack.pop()?;
    ctx.o_stack.push(PsObject::bool(false))?;
    Ok(())
}

/// `.loadbinarysystemfont`: name → bool
///
/// Return false — stet has no binary system font support.
pub fn op_loadbinarysystemfont(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    ctx.o_stack.pop()?;
    ctx.o_stack.push(PsObject::bool(false))?;
    Ok(())
}

/// `.loadbinaryfontfile`: name path → bool
///
/// Return false — stet has no binary font file loading.
pub fn op_loadbinaryfontfile(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.push(PsObject::bool(false))?;
    Ok(())
}

/// `.systemundef`: dict key → — (undef ignoring access restrictions)
pub fn op_systemundef(ctx: &mut Context) -> Result<(), PsError> {
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
    ctx.dicts.remove(dict_entity, &key);
    Ok(())
}

/// `.setinteractivepaint`: bool → — (no-op)
pub fn op_setinteractivepaint(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    ctx.o_stack.pop()?;
    Ok(())
}

/// `pauseexechistory`: — → — (no-op)
pub fn op_pauseexechistory(_ctx: &mut Context) -> Result<(), PsError> {
    Ok(())
}

/// `resumeexechistory`: — → — (no-op)
pub fn op_resumeexechistory(_ctx: &mut Context) -> Result<(), PsError> {
    Ok(())
}

/// `exechistorystack`: array → subarray (return 0-length subarray)
pub fn op_exechistorystack(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    match obj.value {
        PsValue::Array { entity, .. } => {
            ctx.o_stack.pop()?;
            // Return 0-length subarray
            let result = PsObject {
                value: PsValue::Array {
                    entity,
                    start: 0,
                    len: 0,
                },
                flags: obj.flags,
            };
            ctx.o_stack.push(result)?;
            Ok(())
        }
        _ => Err(PsError::TypeCheck),
    }
}

/// Check if a password operand matches `StartJobPassword` in system params.
/// Password can be integer or string; integers are compared as string representation.
fn check_start_job_password(ctx: &Context, password_obj: &PsObject) -> bool {
    // Get StartJobPassword from system params (default "0")
    let key = DictKey::Name(ctx.names.find(b"StartJobPassword").unwrap_or(NameId(0)));
    let stored = match ctx.dicts.get(ctx.system_params, &key) {
        Some(obj) => match obj.value {
            PsValue::String { entity, start, len } => {
                ctx.strings.get(entity, start, len).to_vec()
            }
            _ => b"0".to_vec(),
        },
        None => b"0".to_vec(),
    };

    match password_obj.value {
        PsValue::Int(i) => {
            let pw_str = i.to_string();
            pw_str.as_bytes() == stored.as_slice()
        }
        PsValue::String { entity, start, len } => {
            ctx.strings.get(entity, start, len) == stored.as_slice()
        }
        _ => false,
    }
}

/// `exitserver`: password → — (PLRM 3.7.7)
///
/// Equivalent to: `true password startjob not { /exitserver /invalidaccess .error } if`
pub fn op_exitserver(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    // Validate type: must be integer or string
    let pw_obj = ctx.o_stack.peek(0)?;
    match pw_obj.value {
        PsValue::Int(_) | PsValue::String { .. } => {}
        _ => return Err(PsError::TypeCheck),
    }

    // Check password before popping (validate-before-pop)
    if !check_start_job_password(ctx, &pw_obj) {
        return Err(PsError::InvalidAccess);
    }

    // Password correct — pop and succeed
    ctx.o_stack.pop()?;
    Ok(())
}

/// `startjob`: bool password → bool (PLRM 3.7.7)
///
/// Three conditions must be met for success (returns true):
/// 1. Current execution context supports job encapsulation
/// 2. Password matches StartJobPassword system parameter
/// 3. Save nesting not deeper than job start level
/// On failure, returns false.
pub fn op_startjob(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    // Validate types: password (top) must be integer or string, bool below
    let pw_obj = ctx.o_stack.peek(0)?;
    let bool_obj = ctx.o_stack.peek(1)?;
    match pw_obj.value {
        PsValue::Int(_) | PsValue::String { .. } => {}
        _ => return Err(PsError::TypeCheck),
    }
    match bool_obj.value {
        PsValue::Bool(_) => {}
        _ => return Err(PsError::TypeCheck),
    }

    let pw_obj = ctx.o_stack.pop()?;
    let _bool_obj = ctx.o_stack.pop()?;

    // Condition 2: Password must match StartJobPassword
    let password_ok = check_start_job_password(ctx, &pw_obj);

    // Condition 3: Save nesting must not be deeper than job start level.
    // Job starts at save level 0 (no saves), so any active save means failure.
    let save_nesting_ok = ctx.save_stack.depth() == 0;

    if password_ok && save_nesting_ok {
        // Success: job would start (simplified — no full job server sequence)
        ctx.o_stack.push(PsObject::bool(true))?;
    } else {
        ctx.o_stack.push(PsObject::bool(false))?;
    }
    Ok(())
}

/// `.error`: command errorname → — (core error dispatch)
///
/// This is the Rust-native implementation used by error handlers in errordict.
/// It populates `$error`, snapshots stacks, and calls `stop`.
pub fn op_dot_error(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let errorname_obj = ctx.o_stack.pop()?;
    let command_obj = ctx.o_stack.pop()?;

    // Populate $error dict
    let newerror_id = ctx.names.intern(b"newerror");
    ctx.dicts.put(
        ctx.dollar_error,
        DictKey::Name(newerror_id),
        PsObject::bool(true),
    );

    let errorname_key = ctx.names.intern(b"errorname");
    ctx.dicts.put(
        ctx.dollar_error,
        DictKey::Name(errorname_key),
        errorname_obj,
    );

    let command_key = ctx.names.intern(b"command");
    ctx.dicts
        .put(ctx.dollar_error, DictKey::Name(command_key), command_obj);

    // Snapshot operand stack into ostack array in $error
    let ostack_key = ctx.names.intern(b"ostack");
    let stack_len = ctx.o_stack.len();
    let stack_copy: Vec<PsObject> = (0..stack_len)
        .map(|i| ctx.o_stack.peek(stack_len - 1 - i).unwrap())
        .collect();
    let ostack_entity = ctx.arrays.allocate_from(&stack_copy);
    ctx.dicts.put(
        ctx.dollar_error,
        DictKey::Name(ostack_key),
        PsObject::array(ostack_entity, stack_len as u32),
    );

    Err(PsError::Stop)
}

/// `setpacking`: bool → — (set packing mode)
pub fn op_setpacking(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let mode = match obj.value {
        PsValue::Bool(b) => b,
        _ => return Err(PsError::TypeCheck),
    };
    ctx.o_stack.pop()?;
    ctx.packing_mode = mode;
    Ok(())
}

/// `currentpacking`: — → bool (return current packing mode)
pub fn op_currentpacking(ctx: &mut Context) -> Result<(), PsError> {
    ctx.o_stack.push(PsObject::bool(ctx.packing_mode))?;
    Ok(())
}

/// `packedarray`: obj0 ... objN-1 n → packedarray
///
/// Creates a packed array from the top n elements. In stet, packed arrays
/// are represented as regular arrays with read-only access.
pub fn op_packedarray(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let count_obj = ctx.o_stack.peek(0)?;
    let count = match count_obj.value {
        PsValue::Int(v) => {
            if v < 0 {
                return Err(PsError::RangeCheck);
            }
            v as usize
        }
        _ => return Err(PsError::TypeCheck),
    };
    if ctx.o_stack.len() < count + 1 {
        return Err(PsError::StackUnderflow);
    }

    // VM access check: if allocating in global VM, all elements must be global or non-composite
    if ctx.vm_alloc_mode && count > 0 {
        let slice = ctx.o_stack.as_slice();
        let top_idx = slice.len() - 1; // count is at top
        for i in 0..count {
            let elem = &slice[top_idx - 1 - i];
            if elem.is_composite() && !elem.flags.is_global() {
                return Err(PsError::InvalidAccess);
            }
        }
    }

    ctx.o_stack.pop()?; // count

    let mut elements = Vec::with_capacity(count);
    for _ in 0..count {
        elements.push(ctx.o_stack.pop()?);
    }
    elements.reverse();

    let save_level = ctx.save_stack.current_level();
    let global = ctx.vm_alloc_mode;
    let created = ctx.save_stack.last_save_id();
    let entity = ctx.arrays.allocate_with(count, save_level, global, created);
    let dest = ctx.arrays.get_mut(entity, 0, count as u32);
    dest.copy_from_slice(&elements);
    // Packed arrays are read-only
    let obj = PsObject {
        value: PsValue::PackedArray {
            entity,
            start: 0,
            len: count as u32,
        },
        flags: ObjFlags::new(ObjFlags::ACCESS_READ_ONLY, false, global, true),
    };
    ctx.o_stack.push(obj)?;
    Ok(())
}

/// `internaldict`: int → dict (return internal dictionary, password-protected)
///
/// The integer operand must be 1183615869. Returns a lazily-created internal
/// dictionary used primarily by Type 1 font programs for Flex and hint
/// replacement procedures.
pub fn op_internaldict(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let password = match obj.value {
        PsValue::Int(v) => v,
        _ => return Err(PsError::TypeCheck),
    };
    if password != 1183615869 {
        return Err(PsError::InvalidAccess);
    }

    // Lazily create the internal dictionary
    let entity = match ctx.internaldict {
        Some(e) => e,
        None => {
            let e = ctx.dicts.allocate(50, b"internaldict");
            ctx.internaldict = Some(e);
            e
        }
    };

    ctx.o_stack.pop()?;
    ctx.o_stack.push(PsObject::dict(entity))?;
    Ok(())
}

/// `break`: — → — (debugging no-op)
pub fn op_break(_ctx: &mut Context) -> Result<(), PsError> {
    Ok(())
}

/// `setoverprint`: bool → —
pub fn op_setoverprint(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let val = match ctx.o_stack.peek(0)?.value {
        PsValue::Bool(b) => b,
        _ => return Err(PsError::TypeCheck),
    };
    ctx.o_stack.pop()?;
    ctx.gstate.overprint = val;
    Ok(())
}

/// `currentoverprint`: — → bool
pub fn op_currentoverprint(ctx: &mut Context) -> Result<(), PsError> {
    ctx.o_stack.push(PsObject::bool(ctx.gstate.overprint))?;
    Ok(())
}

/// `setcacheparams`: mark int ... → — (set font cache parameters)
/// `setcacheparams`: mark ... int(s) → —
///
/// PLRM: Accepts 1-3 ints above the mark: [upper], [lower upper], [size lower upper].
/// Stores upper as MaxFontItem, lower as MinFontCompress, size as MaxFontCache.
pub fn op_setcacheparams(ctx: &mut Context) -> Result<(), PsError> {
    // Validate all elements above the mark before popping any
    let mut count = 0;
    loop {
        if count >= ctx.o_stack.len() {
            return Err(PsError::UnmatchedMark);
        }
        let obj = ctx.o_stack.peek(count)?;
        if matches!(obj.value, PsValue::Mark | PsValue::DictMark) {
            break;
        }
        match obj.value {
            PsValue::Int(i) => {
                if i < 0 {
                    return Err(PsError::RangeCheck);
                }
            }
            _ => return Err(PsError::TypeCheck),
        }
        count += 1;
    }

    // Now pop: values above mark (in stack order: top first), then mark
    let mut values: Vec<i32> = Vec::new();
    for _ in 0..count {
        let obj = ctx.o_stack.pop()?;
        if let PsValue::Int(i) = obj.value {
            values.push(i);
        }
    }
    ctx.o_stack.pop()?; // mark
    values.reverse(); // stack order → positional order

    let up_key = DictKey::Name(ctx.names.intern(b"MaxFontItem"));
    let lo_key = DictKey::Name(ctx.names.intern(b"MinFontCompress"));
    let sz_key = DictKey::Name(ctx.names.intern(b"MaxFontCache"));

    match values.len() {
        1 => {
            ctx.dicts
                .put(ctx.user_params, up_key, PsObject::int(values[0]));
        }
        2 => {
            ctx.dicts
                .put(ctx.user_params, lo_key, PsObject::int(values[0]));
            ctx.dicts
                .put(ctx.user_params, up_key, PsObject::int(values[1]));
        }
        3 => {
            ctx.dicts
                .put(ctx.system_params, sz_key, PsObject::int(values[0]));
            ctx.dicts
                .put(ctx.user_params, lo_key, PsObject::int(values[1]));
            ctx.dicts
                .put(ctx.user_params, up_key, PsObject::int(values[2]));
        }
        _ => {} // 0 values: no-op
    }
    Ok(())
}

/// `currentcacheparams`: — → mark size lower upper
pub fn op_currentcacheparams(ctx: &mut Context) -> Result<(), PsError> {
    let sz_key = DictKey::Name(ctx.names.intern(b"MaxFontCache"));
    let lo_key = DictKey::Name(ctx.names.intern(b"MinFontCompress"));
    let up_key = DictKey::Name(ctx.names.intern(b"MaxFontItem"));

    let size = ctx
        .dicts
        .get(ctx.system_params, &sz_key)
        .and_then(|o| o.as_i32())
        .unwrap_or(67108864);
    let lower = ctx
        .dicts
        .get(ctx.user_params, &lo_key)
        .and_then(|o| o.as_i32())
        .unwrap_or(0);
    let upper = ctx
        .dicts
        .get(ctx.user_params, &up_key)
        .and_then(|o| o.as_i32())
        .unwrap_or(0);

    ctx.o_stack.push(PsObject::mark())?;
    ctx.o_stack.push(PsObject::int(size))?;
    ctx.o_stack.push(PsObject::int(lower))?;
    ctx.o_stack.push(PsObject::int(upper))?;
    Ok(())
}

/// `cachestatus`: — → bsize bmax msize mmax csize cmax blimit
pub fn op_cachestatus(ctx: &mut Context) -> Result<(), PsError> {
    ctx.o_stack.push(PsObject::int(0))?; // bsize (current bytes)
    ctx.o_stack.push(PsObject::int(1000000))?; // bmax
    ctx.o_stack.push(PsObject::int(0))?; // msize
    ctx.o_stack.push(PsObject::int(100000))?; // mmax
    ctx.o_stack.push(PsObject::int(0))?; // csize
    ctx.o_stack.push(PsObject::int(500))?; // cmax
    ctx.o_stack.push(PsObject::int(100000))?; // blimit
    Ok(())
}

/// `setvmthreshold`: int → — (set GC threshold, no-op)
///
/// Valid values: -1 (disable) or >= 0 (threshold). Other negatives are rangecheck.
pub fn op_setvmthreshold(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    match obj.value {
        PsValue::Int(i) => {
            if i < -1 {
                return Err(PsError::RangeCheck);
            }
            ctx.o_stack.pop()?;
            Ok(()) // No-op (no GC in stet)
        }
        _ => Err(PsError::TypeCheck),
    }
}

/// `ucachestatus`: — → mark bsize bmax rsize rmax blimit
pub fn op_ucachestatus(ctx: &mut Context) -> Result<(), PsError> {
    let blimit_key = DictKey::Name(ctx.names.intern(b"UCacheBLimit"));
    let blimit = ctx
        .dicts
        .get(ctx.user_params, &blimit_key)
        .and_then(|o| o.as_i32())
        .unwrap_or(0);

    ctx.o_stack.push(PsObject::mark())?;
    ctx.o_stack.push(PsObject::int(0))?; // bsize
    ctx.o_stack.push(PsObject::int(0))?; // bmax
    ctx.o_stack.push(PsObject::int(0))?; // rsize
    ctx.o_stack.push(PsObject::int(0))?; // rmax
    ctx.o_stack.push(PsObject::int(blimit))?; // blimit
    Ok(())
}

/// `setucacheparams`: mark ... int → —
///
/// Sets user path cache parameters. Only the topmost int (blimit) is used.
pub fn op_setucacheparams(ctx: &mut Context) -> Result<(), PsError> {
    // Validate all elements above the mark before popping any
    let mut count = 0;
    loop {
        if count >= ctx.o_stack.len() {
            return Err(PsError::UnmatchedMark);
        }
        let obj = ctx.o_stack.peek(count)?;
        if matches!(obj.value, PsValue::Mark | PsValue::DictMark) {
            break;
        }
        match obj.value {
            PsValue::Int(i) => {
                if i < 0 {
                    return Err(PsError::RangeCheck);
                }
            }
            _ => return Err(PsError::TypeCheck),
        }
        count += 1;
    }

    // Now pop values and mark
    let mut values: Vec<i32> = Vec::new();
    for _ in 0..count {
        let obj = ctx.o_stack.pop()?;
        if let PsValue::Int(i) = obj.value {
            values.push(i);
        }
    }
    ctx.o_stack.pop()?; // mark
    // The first value (was topmost on stack) is blimit
    if let Some(&blimit) = values.first() {
        let key = DictKey::Name(ctx.names.intern(b"UCacheBLimit"));
        ctx.dicts
            .put(ctx.user_params, key, PsObject::int(blimit));
    }
    Ok(())
}

/// `setcachelimit`: int → — (set maximum cached character bitmap size)
///
/// Equivalent to `mark exch setcacheparams`. Stores value as MaxFontItem.
pub fn op_setcachelimit(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    match obj.value {
        PsValue::Int(i) => {
            if i < 0 {
                return Err(PsError::RangeCheck);
            }
            ctx.o_stack.pop()?;
            let key = DictKey::Name(ctx.names.intern(b"MaxFontItem"));
            ctx.dicts.put(ctx.user_params, key, PsObject::int(i));
            Ok(())
        }
        _ => Err(PsError::TypeCheck),
    }
}

/// `copypage`: — → — (copy current page, no-op in single-page mode)
pub fn op_copypage(_ctx: &mut Context) -> Result<(), PsError> {
    Ok(())
}

/// `resetfile`: file → — (clear buffered data for file)
pub fn op_resetfile(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    match ctx.o_stack.peek(0)?.value {
        PsValue::File(_) => {}
        _ => return Err(PsError::TypeCheck),
    }
    ctx.o_stack.pop()?;
    Ok(())
}

/// Helper: get the UserObjects array entity + length from userdict, if it exists.
fn get_userobjects(ctx: &Context) -> Option<(EntityId, u32)> {
    let name_id = ctx.names.find(b"UserObjects")?;
    let obj = ctx.dicts.get(ctx.userdict, &DictKey::Name(name_id))?;
    match obj.value {
        PsValue::Array { entity, start: 0, len } => Some((entity, len)),
        _ => None,
    }
}

/// `defineuserobject`: index obj → — (define a user object)
///
/// Stores obj at the given index in the UserObjects array in userdict.
/// Creates or grows the array as needed.
pub fn op_defineuserobject(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let idx_obj = ctx.o_stack.peek(1)?;
    let index = match idx_obj.value {
        PsValue::Int(v) if v >= 0 => v as u32,
        PsValue::Int(_) => return Err(PsError::RangeCheck),
        _ => return Err(PsError::TypeCheck),
    };

    // Pop operands after validation
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;

    let needed = index + 1;

    match get_userobjects(ctx) {
        Some((entity, len)) if needed <= len => {
            // Array exists and is large enough — just store
            ctx.arrays.set_element(entity, index, obj);
        }
        Some((entity, old_len)) => {
            // Array exists but too small — allocate new, copy old, store
            let new_len = needed.max(old_len * 2);
            let old_data: Vec<PsObject> =
                ctx.arrays.get(entity, 0, old_len).to_vec();
            let new_entity = ctx.arrays.allocate(new_len as usize);
            let new_slice = ctx.arrays.get_mut(new_entity, 0, new_len);
            new_slice[..old_len as usize].copy_from_slice(&old_data);
            ctx.arrays.set_element(new_entity, index, obj);
            // Update userdict to point to new array
            let name_id = ctx.names.intern(b"UserObjects");
            let arr_obj = PsObject::array(new_entity, new_len);
            ctx.dicts.put(ctx.userdict, DictKey::Name(name_id), arr_obj);
        }
        None => {
            // No UserObjects array — create one
            let new_len = needed.max(4);
            let new_entity = ctx.arrays.allocate(new_len as usize);
            ctx.arrays.set_element(new_entity, index, obj);
            let name_id = ctx.names.intern(b"UserObjects");
            let arr_obj = PsObject::array(new_entity, new_len);
            ctx.dicts.put(ctx.userdict, DictKey::Name(name_id), arr_obj);
        }
    }

    Ok(())
}

/// `undefineuserobject`: index → — (remove a user object by setting to null)
pub fn op_undefineuserobject(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let idx_obj = ctx.o_stack.peek(0)?;
    let index = match idx_obj.value {
        PsValue::Int(v) if v >= 0 => v as u32,
        PsValue::Int(_) => return Err(PsError::RangeCheck),
        _ => return Err(PsError::TypeCheck),
    };

    let (entity, len) = get_userobjects(ctx).ok_or(PsError::Undefined)?;
    if index >= len {
        return Err(PsError::RangeCheck);
    }

    ctx.o_stack.pop()?;
    ctx.arrays.set_element(entity, index, PsObject::null());
    Ok(())
}

/// `execuserobject`: index → — (execute a user object)
///
/// Equivalent to: userdict /UserObjects get exch get exec
pub fn op_execuserobject(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let idx_obj = ctx.o_stack.peek(0)?;
    let index = match idx_obj.value {
        PsValue::Int(v) if v >= 0 => v as u32,
        PsValue::Int(_) => return Err(PsError::RangeCheck),
        _ => return Err(PsError::TypeCheck),
    };

    let (entity, len) = get_userobjects(ctx).ok_or(PsError::Undefined)?;
    if index >= len {
        return Err(PsError::RangeCheck);
    }

    let obj = ctx.arrays.get_element(entity, index);
    ctx.o_stack.pop()?;
    // Push on e_stack like op_exec — eval loop handles literal vs executable
    ctx.e_stack.push(obj)?;
    Ok(())
}

/// `printobject`: obj tag → — (write binary object to stdout)
pub fn op_printobject(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let tag_obj = ctx.o_stack.peek(0)?;
    let tag = match tag_obj.value {
        PsValue::Int(v) => v,
        _ => return Err(PsError::TypeCheck),
    };
    if !(0..=255).contains(&tag) {
        return Err(PsError::RangeCheck);
    }
    if ctx.object_format == 0 {
        return Err(PsError::Undefined);
    }
    let obj = ctx.o_stack.peek(1)?;
    let data = serialize_binary_object(ctx, obj, tag as u8)?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    use std::io::Write;
    let _ = ctx.stdout.write_all(&data);
    Ok(())
}

/// `writeobject`: file obj tag → — (write binary object to file)
pub fn op_writeobject(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 3 {
        return Err(PsError::StackUnderflow);
    }
    let tag_obj = ctx.o_stack.peek(0)?;
    let file_obj = ctx.o_stack.peek(2)?;

    let tag = match tag_obj.value {
        PsValue::Int(v) => v,
        _ => return Err(PsError::TypeCheck),
    };
    let file_entity = match file_obj.value {
        PsValue::File(e) => e,
        _ => return Err(PsError::TypeCheck),
    };
    if !(0..=255).contains(&tag) {
        return Err(PsError::RangeCheck);
    }
    if ctx.object_format == 0 {
        return Err(PsError::Undefined);
    }
    let obj = ctx.o_stack.peek(1)?;
    let data = serialize_binary_object(ctx, obj, tag as u8)?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.files
        .write_from(file_entity, &data)
        .map_err(|_| PsError::IOError)?;
    Ok(())
}

/// Serialize a PostScript object as a Binary Object Sequence (PLRM 3.14.2).
fn serialize_binary_object(ctx: &Context, obj: PsObject, tag: u8) -> Result<Vec<u8>, PsError> {
    let format = ctx.object_format;
    let big_endian = format == 1 || format == 3;

    // Collected object entries: (type_byte, tag_byte, length_u16, value_u32)
    let mut objects: Vec<(u8, u8, u16, u32)> = Vec::new();
    // String/name data appended after all object entries
    let mut strings: Vec<u8> = Vec::new();

    // Recursively encode an object at a given slot index
    fn encode(
        ctx: &Context,
        obj: PsObject,
        idx: usize,
        objects: &mut Vec<(u8, u8, u16, u32)>,
        strings: &mut Vec<u8>,
        big_endian: bool,
        depth: u32,
    ) -> Result<(), PsError> {
        if depth > 100 {
            return Err(PsError::LimitCheck);
        }

        let (bos_type, length, value) = match obj.value {
            PsValue::Null => (0u8, 0u16, 0u32),
            PsValue::Int(v) => (1, 0, v as u32),
            PsValue::Real(v) => {
                let f = v as f32;
                let bits = f.to_bits();
                (2, 0, bits)
            }
            PsValue::Name(name_id) => {
                let name_bytes = ctx.names.get_bytes(name_id);
                let str_offset = strings.len() as u32;
                strings.extend_from_slice(name_bytes);
                (3, name_bytes.len() as u16, str_offset)
            }
            PsValue::Bool(v) => (4, 0, if v { 1 } else { 0 }),
            PsValue::String { entity, start, len } => {
                let str_data = ctx.strings.get(entity, start, len);
                let str_offset = strings.len() as u32;
                strings.extend_from_slice(str_data);
                (5, len as u16, str_offset)
            }
            PsValue::Array { entity, start, len } | PsValue::PackedArray { entity, start, len } => {
                // Reserve consecutive slots for children
                let first_child = objects.len();
                for _ in 0..len {
                    objects.push((0, 0, 0, 0)); // placeholder
                }
                // Recursively encode each child
                let elems = ctx.arrays.get(entity, start, len);
                for (i, &child) in elems.iter().enumerate() {
                    encode(
                        ctx,
                        child,
                        first_child + i,
                        objects,
                        strings,
                        big_endian,
                        depth + 1,
                    )?;
                }
                (9, len as u16, (first_child * 8) as u32)
            }
            PsValue::Mark | PsValue::DictMark => (10, 0, 0),
            _ => return Err(PsError::TypeCheck),
        };

        // Executable bit is bit 7 of type byte
        let type_byte = if obj.flags.is_executable() {
            bos_type | 0x80
        } else {
            bos_type
        };

        objects[idx] = (type_byte, 0, length, value);
        Ok(())
    }

    // Reserve slot 0 for the top-level object
    objects.push((0, 0, 0, 0));
    encode(ctx, obj, 0, &mut objects, &mut strings, big_endian, 0)?;

    // Set tag on top-level object
    let (tb, _, len, val) = objects[0];
    objects[0] = (tb, tag, len, val);

    // Adjust string/name offsets: string data starts after all object entries
    let string_base = (objects.len() * 8) as u32;
    for entry in objects.iter_mut() {
        let bos_type = entry.0 & 0x7F;
        if bos_type == 3 || bos_type == 5 {
            // name or string: adjust offset
            entry.3 += string_base;
        }
    }

    // Serialize object entries
    let mut obj_data: Vec<u8> = Vec::with_capacity(objects.len() * 8);
    for &(tb, tg, length, value) in &objects {
        obj_data.push(tb);
        obj_data.push(tg);
        if big_endian {
            obj_data.extend_from_slice(&length.to_be_bytes());
            obj_data.extend_from_slice(&value.to_be_bytes());
        } else {
            obj_data.extend_from_slice(&length.to_le_bytes());
            obj_data.extend_from_slice(&value.to_le_bytes());
        }
    }

    // Build header
    let token_type = (127 + format) as u8;
    let body_size = obj_data.len() + strings.len();
    let normal_length = 4 + body_size;

    let mut result: Vec<u8> = Vec::with_capacity(normal_length);
    if normal_length <= 255 {
        // Normal 4-byte header
        result.push(token_type);
        result.push(normal_length as u8);
        if big_endian {
            result.extend_from_slice(&1u16.to_be_bytes());
        } else {
            result.extend_from_slice(&1u16.to_le_bytes());
        }
    } else {
        // Extended 8-byte header
        let extended_length = (8 + body_size) as u32;
        result.push(token_type);
        result.push(0); // indicates extended header
        if big_endian {
            result.extend_from_slice(&1u16.to_be_bytes());
            result.extend_from_slice(&extended_length.to_be_bytes());
        } else {
            result.extend_from_slice(&1u16.to_le_bytes());
            result.extend_from_slice(&extended_length.to_le_bytes());
        }
    }

    result.extend_from_slice(&obj_data);
    result.extend_from_slice(&strings);
    Ok(result)
}

/// `setobjectformat`: int → — (set binary object format, 0-4)
pub fn op_setobjectformat(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let v = match obj.value {
        PsValue::Int(i) => i,
        _ => return Err(PsError::TypeCheck),
    };
    if !(0..=4).contains(&v) {
        return Err(PsError::RangeCheck);
    }
    ctx.o_stack.pop()?;
    ctx.object_format = v;
    Ok(())
}

/// `currentobjectformat`: — → int (return current binary object format)
pub fn op_currentobjectformat(ctx: &mut Context) -> Result<(), PsError> {
    ctx.o_stack.push(PsObject::int(ctx.object_format))?;
    Ok(())
}

/// `realtime`: — → int (wall-clock time in milliseconds)
///
/// Returns milliseconds since an arbitrary epoch. Used for seeding RNG
/// and interval timing.
pub fn op_realtime(ctx: &mut Context) -> Result<(), PsError> {
    let ms = ctx
        .start_time
        .map(|t| t.elapsed().as_millis() as i32)
        .unwrap_or(0);
    ctx.o_stack.push(PsObject::int(ms))?;
    Ok(())
}

/// `usertime`: — → int (interpreter execution time in milliseconds)
///
/// Returns milliseconds of execution time since the interpreter started.
pub fn op_usertime(ctx: &mut Context) -> Result<(), PsError> {
    let ms = ctx
        .start_time
        .map(|t| t.elapsed().as_millis() as i64)
        .unwrap_or(0);
    ctx.o_stack.push(PsObject::int(ms as i32))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx() -> Context {
        Context::new()
    }

    #[test]
    fn test_bind_typecheck_on_non_proc() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(42)).unwrap();
        let err = op_bind(&mut ctx).unwrap_err();
        assert_eq!(err, PsError::TypeCheck);
    }

    #[test]
    fn test_join_with_separator() {
        let mut ctx = test_ctx();
        // join: separator array dest → result
        let sep = ctx.strings.allocate_from(b"/");
        let s1 = ctx.strings.allocate_from(b"abc");
        let s2 = ctx.strings.allocate_from(b"def");
        let arr = ctx
            .arrays
            .allocate_from(&[PsObject::string(s1, 3), PsObject::string(s2, 3)]);
        let dest = ctx.strings.allocate_from(&[0u8; 256]);
        ctx.o_stack.push(PsObject::string(sep, 1)).unwrap();
        ctx.o_stack.push(PsObject::array(arr, 2)).unwrap();
        ctx.o_stack.push(PsObject::string(dest, 256)).unwrap();
        op_join(&mut ctx).unwrap();
        let result = ctx.o_stack.pop().unwrap();
        if let PsValue::String { entity, start, len } = result.value {
            assert_eq!(ctx.strings.get(entity, start, len), b"abc/def");
        } else {
            panic!("Expected string");
        }
    }

    #[test]
    fn test_join_empty_separator() {
        let mut ctx = test_ctx();
        let sep = ctx.strings.allocate_from(b"");
        let s1 = ctx.strings.allocate_from(b"Hello");
        let s2 = ctx.strings.allocate_from(b"World");
        let arr = ctx
            .arrays
            .allocate_from(&[PsObject::string(s1, 5), PsObject::string(s2, 5)]);
        let dest = ctx.strings.allocate_from(&[0u8; 256]);
        ctx.o_stack.push(PsObject::string(sep, 0)).unwrap();
        ctx.o_stack.push(PsObject::array(arr, 2)).unwrap();
        ctx.o_stack.push(PsObject::string(dest, 256)).unwrap();
        op_join(&mut ctx).unwrap();
        let result = ctx.o_stack.pop().unwrap();
        if let PsValue::String { entity, start, len } = result.value {
            assert_eq!(ctx.strings.get(entity, start, len), b"HelloWorld");
        } else {
            panic!("Expected string");
        }
    }

    #[test]
    fn test_nextfid() {
        let mut ctx = test_ctx();
        op_nextfid(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.pop().unwrap().value, PsValue::FontID(0));
        op_nextfid(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.pop().unwrap().value, PsValue::FontID(1));
    }

    #[test]
    fn test_loadsystemfont_returns_false() {
        let mut ctx = test_ctx();
        let name_id = ctx.names.intern(b"Helvetica");
        ctx.o_stack.push(PsObject::name_lit(name_id)).unwrap();
        op_loadsystemfont(&mut ctx).unwrap();
        let result = ctx.o_stack.pop().unwrap();
        assert!(matches!(result.value, PsValue::Bool(false)));
    }

    #[test]
    fn test_startjob_correct_password() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::bool(false)).unwrap();
        ctx.o_stack.push(PsObject::int(0)).unwrap();
        op_startjob(&mut ctx).unwrap();
        let result = ctx.o_stack.pop().unwrap();
        // Correct password (0), no save nesting → true
        assert!(matches!(result.value, PsValue::Bool(true)));
    }

    #[test]
    fn test_startjob_wrong_password() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::bool(true)).unwrap();
        ctx.o_stack.push(PsObject::int(999)).unwrap();
        op_startjob(&mut ctx).unwrap();
        let result = ctx.o_stack.pop().unwrap();
        // Wrong password → false
        assert!(matches!(result.value, PsValue::Bool(false)));
    }

    #[test]
    fn test_startjob_typecheck() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::bool(true)).unwrap();
        ctx.o_stack.push(PsObject::real(0.5)).unwrap();
        let err = op_startjob(&mut ctx).unwrap_err();
        assert!(matches!(err, PsError::TypeCheck));
    }

    #[test]
    fn test_exitserver_wrong_password() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(999)).unwrap();
        let err = op_exitserver(&mut ctx).unwrap_err();
        assert!(matches!(err, PsError::InvalidAccess));
    }

    #[test]
    fn test_exitserver_typecheck() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::bool(true)).unwrap();
        let err = op_exitserver(&mut ctx).unwrap_err();
        assert!(matches!(err, PsError::TypeCheck));
    }

    #[test]
    fn test_systemundef() {
        let mut ctx = test_ctx();
        let dict = ctx.dicts.allocate(10, b"test");
        let key_name = ctx.names.intern(b"foo");
        ctx.dicts
            .put(dict, DictKey::Name(key_name), PsObject::int(42));
        assert!(ctx.dicts.known(dict, &DictKey::Name(key_name)));

        ctx.o_stack.push(PsObject::dict(dict)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(key_name)).unwrap();
        op_systemundef(&mut ctx).unwrap();
        assert!(!ctx.dicts.known(dict, &DictKey::Name(key_name)));
    }

    #[test]
    fn test_dot_error() {
        let mut ctx = test_ctx();
        let cmd = ctx.names.intern(b"myop");
        let err = ctx.names.intern(b"typecheck");
        ctx.o_stack.push(PsObject::name_lit(cmd)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(err)).unwrap();
        let result = op_dot_error(&mut ctx);
        assert_eq!(result, Err(PsError::Stop));
        // Check $error was populated
        let newerror_id = ctx.names.intern(b"newerror");
        let ne = ctx
            .dicts
            .get(ctx.dollar_error, &DictKey::Name(newerror_id))
            .unwrap();
        assert!(matches!(ne.value, PsValue::Bool(true)));
    }
}
