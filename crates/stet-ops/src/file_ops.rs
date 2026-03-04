// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! File/output operators.
//! Phase 1: print, =, ==, flush, pstack.
//! Phase 2: file, closefile, read, write, readstring, writestring,
//!   readline, readhexstring, writehexstring, token, bytesavailable,
//!   flushfile, currentfile, fileposition, setfileposition, status,
//!   deletefile, renamefile, filenameforall.

use std::io::Write;

use stet_core::context::Context;
use stet_core::error::PsError;
use stet_core::file_store;
use stet_core::object::{EntityId, ObjFlags, PsObject, PsValue};

use std::path::Path;

use crate::type_ops::write_obj_equal;

/// Resolve a relative filename against likely directories.
///
/// Fallback chain (matches PostForge's `_resolve_filename`):
/// 1. Absolute or exists as-is → return it
/// 2. Exec stack walk → scan for File entries, try parent directory of innermost real file
/// 3. Resource base path → try `resource_base_path/filename`
/// 4. Return original → let the caller produce the appropriate error
pub(crate) fn resolve_filename(ctx: &Context, filename: &str) -> String {
    let path = Path::new(filename);

    // 1. Absolute or already exists
    if path.is_absolute() || path.exists() {
        return filename.to_string();
    }

    // 2. Walk exec stack for the innermost real file's directory
    for i in 0..ctx.e_stack.len() {
        if let Ok(obj) = ctx.e_stack.peek(i)
            && let PsValue::File(entity) = obj.value
        {
            let name = ctx.files.name(entity);
            // Skip synthetic names (filters, string sources, stdio)
            if name.starts_with('%') {
                continue;
            }
            if let Some(parent) = Path::new(name).parent() {
                let candidate = parent.join(filename);
                if candidate.exists() {
                    return candidate.to_string_lossy().to_string();
                }
            }
            // Only check the innermost real file (matches PostForge)
            break;
        }
    }

    // 3. Resource base path
    if let Some(ref base) = ctx.resource_base_path {
        let relative = filename.strip_prefix("resources/").unwrap_or(filename);
        let candidate = Path::new(base).join(relative);
        if candidate.exists() {
            return candidate.to_string_lossy().to_string();
        }
    }

    // 4. Return original
    filename.to_string()
}

/// `print`: string → — (write string to stdout)
pub fn op_print(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    match obj.value {
        PsValue::String { entity, start, len } => {
            let bytes = ctx.strings.get(entity, start, len).to_vec();
            ctx.o_stack.pop()?;
            ctx.stdout.write_all(&bytes).map_err(|_| PsError::IOError)?;
            ctx.stdout.flush().map_err(|_| PsError::IOError)?;
            Ok(())
        }
        _ => Err(PsError::TypeCheck),
    }
}

/// `=`: any → — (write object representation + newline)
pub fn op_equal_sign(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.pop()?;

    let mut buf = Vec::new();
    write_obj_equal(ctx, &obj, &mut buf);
    buf.push(b'\n');
    ctx.stdout.write_all(&buf).map_err(|_| PsError::IOError)?;
    ctx.stdout.flush().map_err(|_| PsError::IOError)?;
    Ok(())
}

/// `==`: any → — (write detailed object representation + newline)
pub fn op_double_equal(ctx: &mut Context) -> Result<(), PsError> {
    op_equal_sign(ctx)
}

/// `flush`: — (flush stdout)
pub fn op_flush(ctx: &mut Context) -> Result<(), PsError> {
    ctx.stdout.flush().map_err(|_| PsError::IOError)?;
    Ok(())
}

/// `pstack`: — (print entire operand stack, top first)
pub fn op_pstack(ctx: &mut Context) -> Result<(), PsError> {
    let slice = ctx.o_stack.as_slice();
    let mut buf = Vec::new();
    for obj in slice.iter().rev() {
        write_obj_equal(ctx, obj, &mut buf);
        buf.push(b'\n');
    }
    ctx.stdout.write_all(&buf).map_err(|_| PsError::IOError)?;
    ctx.stdout.flush().map_err(|_| PsError::IOError)?;
    Ok(())
}

// --- Phase 2 file I/O operators ---

/// `file`: filename access → file
pub fn op_file(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let mode_obj = ctx.o_stack.peek(0)?;
    let name_obj = ctx.o_stack.peek(1)?;

    let mode = match mode_obj.value {
        PsValue::String { entity, start, len } => {
            String::from_utf8(ctx.strings.get(entity, start, len).to_vec())
                .map_err(|_| PsError::TypeCheck)?
        }
        _ => return Err(PsError::TypeCheck),
    };
    let name = match name_obj.value {
        PsValue::String { entity, start, len } => {
            String::from_utf8(ctx.strings.get(entity, start, len).to_vec())
                .map_err(|_| PsError::TypeCheck)?
        }
        _ => return Err(PsError::TypeCheck),
    };

    // Resolve relative paths against the currently executing file's directory
    let resolved = resolve_filename(ctx, &name);

    let file_entity = ctx.files.open(&resolved, &mode).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            PsError::UndefinedFilename
        } else {
            PsError::InvalidFileAccess
        }
    })?;

    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;

    let file_obj = PsObject {
        value: PsValue::File(file_entity),
        flags: ObjFlags::literal_composite(),
    };
    ctx.o_stack.push(file_obj)?;
    Ok(())
}

/// `closefile`: file → —
pub fn op_closefile(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let entity = match obj.value {
        PsValue::File(e) => e,
        _ => return Err(PsError::TypeCheck),
    };
    ctx.o_stack.pop()?;
    ctx.files.close(entity).map_err(|_| PsError::IOError)?;
    Ok(())
}

/// `read`: file → byte true | false
pub fn op_read(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let entity = match obj.value {
        PsValue::File(e) => e,
        _ => return Err(PsError::TypeCheck),
    };
    ctx.o_stack.pop()?;

    match ctx.files.read_byte(entity).map_err(|_| PsError::IOError)? {
        Some(byte) => {
            ctx.o_stack.push(PsObject::int(byte as i32))?;
            ctx.o_stack.push(PsObject::bool(true))?;
        }
        None => {
            ctx.o_stack.push(PsObject::bool(false))?;
        }
    }
    Ok(())
}

/// `write`: file byte → —
pub fn op_write(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let byte_obj = ctx.o_stack.peek(0)?;
    let file_obj = ctx.o_stack.peek(1)?;

    let byte = match byte_obj.value {
        PsValue::Int(v) => {
            if !(0..=255).contains(&v) {
                return Err(PsError::RangeCheck);
            }
            v as u8
        }
        _ => return Err(PsError::TypeCheck),
    };
    let entity = match file_obj.value {
        PsValue::File(e) => e,
        _ => return Err(PsError::TypeCheck),
    };

    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.files
        .write_byte(entity, byte)
        .map_err(|_| PsError::IOError)?;
    Ok(())
}

/// `readstring`: file string → substring bool
pub fn op_readstring(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let str_obj = ctx.o_stack.peek(0)?;
    let file_obj = ctx.o_stack.peek(1)?;

    let (str_entity, str_start, str_len) = match str_obj.value {
        PsValue::String { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };
    let str_flags = str_obj.flags;
    let file_entity = match file_obj.value {
        PsValue::File(e) => e,
        _ => return Err(PsError::TypeCheck),
    };

    // Access checks: file must be readable, string must be writable
    if file_obj.flags.access() < ObjFlags::ACCESS_READ_ONLY {
        return Err(PsError::InvalidAccess);
    }
    if str_obj.flags.access() < ObjFlags::ACCESS_WRITE_ONLY {
        return Err(PsError::InvalidAccess);
    }

    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;

    // Read into a temp buffer to avoid borrow conflict
    let mut temp = vec![0u8; str_len as usize];
    let n = ctx
        .files
        .read_into(file_entity, &mut temp)
        .map_err(|_| PsError::IOError)?;

    // Copy into string store
    ctx.cow_check_string(str_entity);
    let dest = ctx.strings.get_mut(str_entity, str_start, str_len);
    dest[..n].copy_from_slice(&temp[..n]);

    // Return substring with original string's flags (preserves global bit)
    let got_all = n == str_len as usize;
    ctx.o_stack.push(PsObject {
        value: PsValue::String {
            entity: str_entity,
            start: str_start,
            len: n as u32,
        },
        flags: str_flags,
    })?;
    ctx.o_stack.push(PsObject::bool(got_all))?;
    Ok(())
}

/// `writestring`: file string → —
pub fn op_writestring(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let str_obj = ctx.o_stack.peek(0)?;
    let file_obj = ctx.o_stack.peek(1)?;

    let (str_entity, str_start, str_len) = match str_obj.value {
        PsValue::String { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };
    let file_entity = match file_obj.value {
        PsValue::File(e) => e,
        _ => return Err(PsError::TypeCheck),
    };

    // Access checks: file must be writable, string must be readable
    if file_obj.flags.access() < ObjFlags::ACCESS_WRITE_ONLY {
        return Err(PsError::InvalidAccess);
    }
    if str_obj.flags.access() < ObjFlags::ACCESS_READ_ONLY {
        return Err(PsError::InvalidAccess);
    }

    let bytes = ctx.strings.get(str_entity, str_start, str_len).to_vec();
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.files
        .write_from(file_entity, &bytes)
        .map_err(|_| PsError::IOError)?;
    Ok(())
}

/// `readline`: file string → substring bool
pub fn op_readline(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let str_obj = ctx.o_stack.peek(0)?;
    let file_obj = ctx.o_stack.peek(1)?;

    let (str_entity, str_start, str_len) = match str_obj.value {
        PsValue::String { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };
    let file_entity = match file_obj.value {
        PsValue::File(e) => e,
        _ => return Err(PsError::TypeCheck),
    };

    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;

    let mut temp = vec![0u8; str_len as usize];
    let (n, got_line) = ctx
        .files
        .readline(file_entity, &mut temp)
        .map_err(|_| PsError::IOError)?;

    ctx.cow_check_string(str_entity);
    let dest = ctx.strings.get_mut(str_entity, str_start, str_len);
    dest[..n].copy_from_slice(&temp[..n]);

    ctx.o_stack.push(PsObject {
        value: PsValue::String {
            entity: str_entity,
            start: str_start,
            len: n as u32,
        },
        flags: ObjFlags::literal_composite(),
    })?;
    ctx.o_stack.push(PsObject::bool(got_line))?;
    Ok(())
}

/// `readhexstring`: file string → substring bool
pub fn op_readhexstring(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let str_obj = ctx.o_stack.peek(0)?;
    let file_obj = ctx.o_stack.peek(1)?;

    let (str_entity, str_start, str_len) = match str_obj.value {
        PsValue::String { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };
    let file_entity = match file_obj.value {
        PsValue::File(e) => e,
        _ => return Err(PsError::TypeCheck),
    };

    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;

    // Read hex digits from file, convert pairs to bytes
    let mut result = Vec::with_capacity(str_len as usize);
    let mut nibble_buf: Option<u8> = None;
    let mut eof = false;

    while result.len() < str_len as usize {
        match ctx
            .files
            .read_byte(file_entity)
            .map_err(|_| PsError::IOError)?
        {
            None => {
                eof = true;
                break;
            }
            Some(b) => {
                let nib = match b {
                    b'0'..=b'9' => Some(b - b'0'),
                    b'a'..=b'f' => Some(b - b'a' + 10),
                    b'A'..=b'F' => Some(b - b'A' + 10),
                    _ => None, // whitespace/other chars are skipped
                };
                if let Some(n) = nib {
                    match nibble_buf.take() {
                        None => nibble_buf = Some(n),
                        Some(high) => result.push((high << 4) | n),
                    }
                }
            }
        }
    }
    // If odd number of hex digits, pad with 0
    if let Some(high) = nibble_buf
        && result.len() < str_len as usize
    {
        result.push(high << 4);
    }

    let n = result.len();
    ctx.cow_check_string(str_entity);
    let dest = ctx.strings.get_mut(str_entity, str_start, str_len);
    dest[..n].copy_from_slice(&result);

    let got_all = n == str_len as usize && !eof;
    ctx.o_stack.push(PsObject {
        value: PsValue::String {
            entity: str_entity,
            start: str_start,
            len: n as u32,
        },
        flags: ObjFlags::literal_composite(),
    })?;
    ctx.o_stack.push(PsObject::bool(got_all))?;
    Ok(())
}

/// `writehexstring`: file string → —
pub fn op_writehexstring(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let str_obj = ctx.o_stack.peek(0)?;
    let file_obj = ctx.o_stack.peek(1)?;

    let (str_entity, str_start, str_len) = match str_obj.value {
        PsValue::String { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };
    let file_entity = match file_obj.value {
        PsValue::File(e) => e,
        _ => return Err(PsError::TypeCheck),
    };

    let bytes = ctx.strings.get(str_entity, str_start, str_len).to_vec();
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;

    let hex: Vec<u8> = bytes
        .iter()
        .flat_map(|b| {
            let hi = b >> 4;
            let lo = b & 0x0f;
            let to_hex = |n: u8| if n < 10 { b'0' + n } else { b'a' + n - 10 };
            [to_hex(hi), to_hex(lo)]
        })
        .collect();

    ctx.files
        .write_from(file_entity, &hex)
        .map_err(|_| PsError::IOError)?;
    Ok(())
}

/// `bytesavailable`: file → int
pub fn op_bytesavailable(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let entity = match obj.value {
        PsValue::File(e) => e,
        _ => return Err(PsError::TypeCheck),
    };

    let avail = ctx.files.bytes_available(entity);
    ctx.o_stack.pop()?;
    ctx.o_stack.push(PsObject::int(avail))?;
    Ok(())
}

/// `flushfile`: file → —
pub fn op_flushfile(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let entity = match obj.value {
        PsValue::File(e) => e,
        _ => return Err(PsError::TypeCheck),
    };
    ctx.o_stack.pop()?;
    ctx.files.flush(entity).map_err(|_| PsError::IOError)?;
    Ok(())
}

/// `currentfile`: — → file
/// Returns the file on the exec stack nearest to the top.
pub fn op_currentfile(ctx: &mut Context) -> Result<(), PsError> {
    // Scan exec stack top-down for a File object
    for i in 0..ctx.e_stack.len() {
        let obj = ctx.e_stack.peek(i)?;
        if matches!(obj.value, PsValue::File(_)) {
            ctx.o_stack.push(obj)?;
            return Ok(());
        }
    }
    // If no file on exec stack, return stdin
    ctx.o_stack.push(PsObject {
        value: PsValue::File(file_store::FILE_STDIN),
        flags: ObjFlags::literal_composite(),
    })?;
    Ok(())
}

/// `line`: file → int (return current line number)
///
/// PostForge extension. Returns the current line number (1-based) of the
/// source being scanned. Line numbers are tracked per-file as the tokenizer
/// processes newlines (CR, LF, CR-LF, FF).
pub fn op_line(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let line = match obj.value {
        PsValue::File(entity) => ctx.files.line_num(entity),
        PsValue::String { .. } => ctx.current_source_line,
        _ => return Err(PsError::TypeCheck),
    };
    ctx.o_stack.pop()?;
    ctx.o_stack.push(PsObject::int(line as i32))?;
    Ok(())
}

/// `filename`: file → name
///
/// Returns the name of the file as a name object.
pub fn op_filename(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let entity = match obj.value {
        PsValue::File(e) => e,
        _ => return Err(PsError::TypeCheck),
    };
    let name = ctx.files.name(entity);
    let name_id = ctx.names.intern(name.as_bytes());
    ctx.o_stack.pop()?;
    ctx.o_stack.push(PsObject::name_lit(name_id))?;
    Ok(())
}

/// `fileposition`: file → int
pub fn op_fileposition(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let entity = match obj.value {
        PsValue::File(e) => e,
        _ => return Err(PsError::TypeCheck),
    };
    // Validate position BEFORE popping — error recovery depends on intact stack
    let pos = ctx.files.position(entity).map_err(|_| PsError::IOError)?;
    ctx.o_stack.pop()?;
    ctx.o_stack.push(PsObject::int(pos as i32))?;
    Ok(())
}

/// `setfileposition`: file int → —
pub fn op_setfileposition(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let pos_obj = ctx.o_stack.peek(0)?;
    let file_obj = ctx.o_stack.peek(1)?;

    let pos = match pos_obj.value {
        PsValue::Int(v) => {
            if v < 0 {
                return Err(PsError::RangeCheck);
            }
            v as u64
        }
        _ => return Err(PsError::TypeCheck),
    };
    let entity = match file_obj.value {
        PsValue::File(e) => e,
        _ => return Err(PsError::TypeCheck),
    };

    // Validate file is open and seekable BEFORE popping
    if !ctx.files.is_open(entity) {
        return Err(PsError::IOError);
    }
    if !ctx.files.is_seekable(entity) {
        return Err(PsError::IOError);
    }

    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.files
        .set_position(entity, pos)
        .map_err(|_| PsError::IOError)?;
    Ok(())
}

/// `status` (filename form): string → pages bytes referenced created true | false
///
/// PLRM: Checks if the named file exists. If it does, returns four dummy
/// values (pages=0, bytes=0, referenced=0, created=0) and true. If not,
/// returns false. Resolves paths relative to `resource_base_path`.
pub fn op_status(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    match obj.value {
        // file status → bool (true if file is still open)
        PsValue::File(entity) => {
            let is_open = ctx.files.is_open(entity);
            ctx.o_stack.pop()?;
            ctx.o_stack.push(PsObject::bool(is_open))?;
        }
        // filename status → pages bytes referenced created true | false
        PsValue::String { entity, start, len } => {
            let name = ctx.strings.get(entity, start, len).to_vec();
            let path = String::from_utf8(name).map_err(|_| PsError::TypeCheck)?;
            // Check embedded files first (for WASM builds)
            let exists = if ctx.files.get_embedded_file(&path).is_some() {
                true
            } else {
                // resolve_filename returns the original if nothing matched,
                // so check whether the resolved path actually exists
                let resolved = resolve_filename(ctx, &path);
                Path::new(&resolved).exists()
            };
            ctx.o_stack.pop()?;
            if exists {
                ctx.o_stack.push(PsObject::int(0))?; // pages
                ctx.o_stack.push(PsObject::int(0))?; // bytes
                ctx.o_stack.push(PsObject::int(0))?; // referenced
                ctx.o_stack.push(PsObject::int(0))?; // created
                ctx.o_stack.push(PsObject::bool(true))?;
            } else {
                ctx.o_stack.push(PsObject::bool(false))?;
            }
        }
        _ => return Err(PsError::TypeCheck),
    }
    Ok(())
}

/// `deletefile`: filename → —
pub fn op_deletefile(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let path = match obj.value {
        PsValue::String { entity, start, len } => {
            String::from_utf8(ctx.strings.get(entity, start, len).to_vec())
                .map_err(|_| PsError::TypeCheck)?
        }
        _ => return Err(PsError::TypeCheck),
    };

    const SPECIAL_FILES: &[&str] = &[
        "%stdin",
        "%stdout",
        "%stderr",
        "%statementedit",
        "%lineedit",
    ];
    if SPECIAL_FILES.contains(&path.as_str()) {
        return Err(PsError::InvalidFileAccess);
    }

    ctx.o_stack.pop()?;
    std::fs::remove_file(&path).map_err(|_| PsError::UndefinedFilename)?;
    Ok(())
}

/// `renamefile`: old new → —
pub fn op_renamefile(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let new_obj = ctx.o_stack.peek(0)?;
    let old_obj = ctx.o_stack.peek(1)?;

    let new_name = match new_obj.value {
        PsValue::String { entity, start, len } => {
            String::from_utf8(ctx.strings.get(entity, start, len).to_vec())
                .map_err(|_| PsError::TypeCheck)?
        }
        _ => return Err(PsError::TypeCheck),
    };
    let old_name = match old_obj.value {
        PsValue::String { entity, start, len } => {
            String::from_utf8(ctx.strings.get(entity, start, len).to_vec())
                .map_err(|_| PsError::TypeCheck)?
        }
        _ => return Err(PsError::TypeCheck),
    };

    // Check for special file names BEFORE popping
    const SPECIAL_FILES: &[&str] = &[
        "%stdin",
        "%stdout",
        "%stderr",
        "%statementedit",
        "%lineedit",
    ];
    if SPECIAL_FILES.contains(&old_name.as_str()) || SPECIAL_FILES.contains(&new_name.as_str()) {
        return Err(PsError::InvalidFileAccess);
    }

    // Check source file exists BEFORE popping
    if !std::path::Path::new(&old_name).exists() {
        return Err(PsError::UndefinedFilename);
    }

    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    std::fs::rename(&old_name, &new_name).map_err(|_| PsError::UndefinedFilename)?;
    Ok(())
}

/// `filenameforall`: template proc scratch → —
/// Simplified: enumerate files matching a glob pattern.
pub fn op_filenameforall(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 3 {
        return Err(PsError::StackUnderflow);
    }
    let _scratch_obj = ctx.o_stack.peek(0)?;
    let _proc_obj = ctx.o_stack.peek(1)?;
    let _template_obj = ctx.o_stack.peek(2)?;

    // Phase 2: simplified no-op (full glob matching deferred)
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    Ok(())
}

/// `token` (file form): file → any true | false
/// Read one PostScript token from a file.
pub fn op_token(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;

    match obj.value {
        PsValue::File(file_entity) => {
            ctx.o_stack.pop()?;
            token_from_file(ctx, file_entity)
        }
        PsValue::String { entity, start, len } => {
            // token from string: string → post any true | false
            let bytes = ctx.strings.get(entity, start, len).to_vec();
            ctx.o_stack.pop()?;
            token_from_string(ctx, &bytes, entity, start, len)
        }
        _ => Err(PsError::TypeCheck),
    }
}

/// Read one token from a file, byte-at-a-time.
fn token_from_file(ctx: &mut Context, file_entity: EntityId) -> Result<(), PsError> {
    use stet_core::tokenizer::stream_next_token;

    match stream_next_token(&mut ctx.files, file_entity)? {
        Some((token, _newlines)) => {
            let obj = ctx.token_to_object(token)?;
            ctx.o_stack.push(obj)?;
            ctx.o_stack.push(PsObject::bool(true))?;
        }
        None => {
            ctx.o_stack.push(PsObject::bool(false))?;
        }
    }
    Ok(())
}

/// Read one token from a string. Returns: post any true | false
///
/// Handles composite objects (`{...}` procedures, `[...]` arrays) as complete
/// tokens. After number/name tokens, consumes one trailing whitespace character
/// per PLRM spec.
fn token_from_string(
    ctx: &mut Context,
    bytes: &[u8],
    entity: EntityId,
    start: u32,
    _len: u32,
) -> Result<(), PsError> {
    use stet_core::tokenizer::{Token, Tokenizer};

    let mut tokenizer = Tokenizer::new(bytes);
    match tokenizer.next_token()? {
        None => {
            ctx.o_stack.push(PsObject::bool(false))?;
        }
        Some(Token::ProcBegin) => {
            let proc_obj = token_parse_procedure(ctx, &mut tokenizer)?;
            let consumed = tokenizer.position() as u32;
            push_remainder_string(ctx, entity, start, bytes.len() as u32, consumed)?;
            ctx.o_stack.push(proc_obj)?;
            ctx.o_stack.push(PsObject::bool(true))?;
        }
        Some(Token::ArrayBegin) => {
            let arr_obj = token_parse_array(ctx, &mut tokenizer)?;
            let consumed = tokenizer.position() as u32;
            push_remainder_string(ctx, entity, start, bytes.len() as u32, consumed)?;
            ctx.o_stack.push(arr_obj)?;
            ctx.o_stack.push(PsObject::bool(true))?;
        }
        Some(Token::DictBegin) => {
            // << is a dict mark (distinct from [ mark)
            let consumed = tokenizer.position() as u32;
            push_remainder_string(ctx, entity, start, bytes.len() as u32, consumed)?;
            ctx.o_stack.push(PsObject::dict_mark())?;
            ctx.o_stack.push(PsObject::bool(true))?;
        }
        Some(Token::ProcEnd) => {
            return Err(PsError::SyntaxError);
        }
        Some(Token::Eof) => {
            ctx.o_stack.push(PsObject::bool(false))?;
        }
        Some(token @ (Token::Int(_) | Token::Real(_) | Token::Name(_, _))) => {
            // Numbers and executable names: consume one trailing whitespace
            let obj = ctx.token_to_object(token)?;
            let mut pos = tokenizer.position();
            if pos < bytes.len() && is_ps_whitespace(bytes[pos]) {
                pos += 1;
            }
            let consumed = pos as u32;
            push_remainder_string(ctx, entity, start, bytes.len() as u32, consumed)?;
            ctx.o_stack.push(obj)?;
            ctx.o_stack.push(PsObject::bool(true))?;
        }
        Some(
            token @ (Token::LiteralName(_)
            | Token::ImmediateName(_)
            | Token::String(_)
            | Token::DictEnd
            | Token::ArrayEnd),
        ) => {
            let obj = ctx.token_to_object(token)?;
            let consumed = tokenizer.position() as u32;
            push_remainder_string(ctx, entity, start, bytes.len() as u32, consumed)?;
            ctx.o_stack.push(obj)?;
            ctx.o_stack.push(PsObject::bool(true))?;
        }
    }
    Ok(())
}

/// Push the remainder substring onto the operand stack.
///
/// Instead of allocating a new string entity, the remainder references
/// the SAME entity with an advanced `start` offset.
fn push_remainder_string(
    ctx: &mut Context,
    entity: EntityId,
    start: u32,
    len: u32,
    consumed: u32,
) -> Result<(), PsError> {
    let new_start = start + consumed;
    let new_len = len - consumed;
    ctx.o_stack.push(PsObject {
        value: PsValue::String {
            entity,
            start: new_start,
            len: new_len,
        },
        flags: ObjFlags::literal_composite(),
    })?;
    Ok(())
}

/// `eexec`: file → — | string → —
///
/// Decrypts using the Adobe Type 1 eexec cipher (R=55665), skips 4 random
/// bytes, pushes systemdict on d_stack, and pushes the decrypted data as an
/// incremental eexec decryption filter on the e_stack.
///
/// The filter wraps the source file and decrypts on-the-fly as the eval
/// loop reads tokens. When `currentfile closefile` executes inside the
/// decrypted section, the filter closes and the source file resumes from
/// where the filter stopped reading. This correctly handles inline fonts
/// embedded in larger PS files (e.g. dvips output).
pub fn op_eexec(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;

    // Get the source entity — must be a file or string
    let source_entity = match obj.value {
        PsValue::File(entity) => {
            ctx.o_stack.pop()?;
            entity
        }
        PsValue::String { entity, start, len } => {
            // For string input, create a StringSource file from the data
            let data = ctx.strings.get(entity, start, len).to_vec();
            ctx.o_stack.pop()?;
            ctx.files.create_string_source(data)
        }
        _ => return Err(PsError::TypeCheck),
    };

    // Create an EexecDecode filter wrapping the source.
    let filter_entity = ctx.files.create_filter(
        source_entity,
        stet_core::file_store::FilterKind::eexec_decode(),
    );

    // Push systemdict on d_stack (PLRM: eexec pushes systemdict)
    let sd = *ctx.d_stack.first().unwrap_or(&ctx.systemdict);
    ctx.d_stack.push(sd);
    ctx.invalidate_name_cache();

    // Push DictEnd below so when the filter finishes, d_stack is popped.
    // Push the filter as an executable file on e_stack — the eval loop's
    // streaming path reads from it byte-by-byte via stream_next_token.
    ctx.e_stack.push(PsObject::dict_end(sd))?;
    ctx.e_stack.push(PsObject {
        value: PsValue::File(filter_entity),
        flags: ObjFlags::executable_composite(),
    })?;
    Ok(())
}

fn is_ps_whitespace(b: u8) -> bool {
    matches!(b, b'\0' | b'\t' | b'\n' | 0x0C | b'\r' | b' ')
}

/// Parse a complete `{ ... }` procedure from tokenizer (used by string `token`).
fn token_parse_procedure(
    ctx: &mut Context,
    tokenizer: &mut stet_core::tokenizer::Tokenizer,
) -> Result<PsObject, PsError> {
    use stet_core::tokenizer::Token;

    let mut elements = Vec::new();
    loop {
        match tokenizer.next_token()? {
            Some(Token::ProcEnd) => break,
            Some(Token::ProcBegin) => {
                let nested = token_parse_procedure(ctx, tokenizer)?;
                elements.push(nested);
            }
            Some(token) => {
                let obj = ctx.token_to_object(token)?;
                elements.push(obj);
            }
            None => return Err(PsError::SyntaxError),
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

/// Parse a complete `[ ... ]` array from tokenizer (used by string `token`).
fn token_parse_array(
    ctx: &mut Context,
    tokenizer: &mut stet_core::tokenizer::Tokenizer,
) -> Result<PsObject, PsError> {
    use stet_core::tokenizer::Token;

    let mut elements = Vec::new();
    loop {
        match tokenizer.next_token()? {
            Some(Token::ArrayEnd) => break,
            Some(Token::ArrayBegin) => {
                let nested = token_parse_array(ctx, tokenizer)?;
                elements.push(nested);
            }
            Some(Token::ProcBegin) => {
                let nested = token_parse_procedure(ctx, tokenizer)?;
                elements.push(nested);
            }
            Some(token) => {
                let obj = ctx.token_to_object(token)?;
                elements.push(obj);
            }
            None => return Err(PsError::SyntaxError),
        }
    }

    let len = elements.len();
    let save_level = ctx.save_stack.current_level();
    let global = ctx.vm_alloc_mode;
    let created = ctx.save_stack.last_save_id();
    let entity = ctx.arrays.allocate_with(len, save_level, global, created);
    let dest = ctx.arrays.get_mut(entity, 0, len as u32);
    dest.copy_from_slice(&elements);

    let mut obj = PsObject::array(entity, len as u32);
    if global {
        obj.flags = ObjFlags::new(ObjFlags::ACCESS_UNLIMITED, false, true, true);
    }
    Ok(obj)
}

#[cfg(test)]
mod tests {
    use super::*;
    use stet_core::object::PsObject;

    fn test_ctx_with_capture() -> (Context, std::sync::Arc<std::sync::Mutex<Vec<u8>>>) {
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

        let ctx = Context::new_with_output(Box::new(ArcWriter(writer)));
        (ctx, buf)
    }

    #[test]
    fn test_print() {
        let (mut ctx, buf) = test_ctx_with_capture();
        let entity = ctx.strings.allocate_from(b"Hello");
        ctx.o_stack.push(PsObject::string(entity, 5)).unwrap();
        op_print(&mut ctx).unwrap();
        assert_eq!(&*buf.lock().unwrap(), b"Hello");
    }

    #[test]
    fn test_equal_sign() {
        let (mut ctx, buf) = test_ctx_with_capture();
        ctx.o_stack.push(PsObject::int(42)).unwrap();
        op_equal_sign(&mut ctx).unwrap();
        assert_eq!(&*buf.lock().unwrap(), b"42\n");
    }

    #[test]
    fn test_file_round_trip() {
        let mut ctx = Context::new();
        let path = "/tmp/stet_test_file_ops.txt";

        // Open for write
        let name_ent = ctx.strings.allocate_from(path.as_bytes());
        let mode_ent = ctx.strings.allocate_from(b"w");
        ctx.o_stack
            .push(PsObject::string(name_ent, path.len() as u32))
            .unwrap();
        ctx.o_stack.push(PsObject::string(mode_ent, 1)).unwrap();
        op_file(&mut ctx).unwrap();
        let file_obj = ctx.o_stack.pop().unwrap();

        // Write a string
        let data_ent = ctx.strings.allocate_from(b"test data");
        ctx.o_stack.push(file_obj).unwrap();
        ctx.o_stack.push(PsObject::string(data_ent, 9)).unwrap();
        op_writestring(&mut ctx).unwrap();

        // Close
        ctx.o_stack.push(file_obj).unwrap();
        op_closefile(&mut ctx).unwrap();

        // Open for read
        let name_ent2 = ctx.strings.allocate_from(path.as_bytes());
        let mode_ent2 = ctx.strings.allocate_from(b"r");
        ctx.o_stack
            .push(PsObject::string(name_ent2, path.len() as u32))
            .unwrap();
        ctx.o_stack.push(PsObject::string(mode_ent2, 1)).unwrap();
        op_file(&mut ctx).unwrap();
        let rfile = ctx.o_stack.pop().unwrap();

        // Readstring
        let buf_ent = ctx.strings.allocate(9);
        ctx.o_stack.push(rfile).unwrap();
        ctx.o_stack.push(PsObject::string(buf_ent, 9)).unwrap();
        op_readstring(&mut ctx).unwrap();

        let got_all = ctx.o_stack.pop().unwrap();
        let result_str = ctx.o_stack.pop().unwrap();
        assert!(matches!(got_all.value, PsValue::Bool(true)));
        match result_str.value {
            PsValue::String { entity, start, len } => {
                assert_eq!(ctx.strings.get(entity, start, len), b"test data");
            }
            _ => panic!("Expected string"),
        }

        // Close + cleanup
        ctx.o_stack.push(rfile).unwrap();
        op_closefile(&mut ctx).unwrap();
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_status() {
        let mut ctx = Context::new();

        // Test existing file — returns pages bytes referenced created true
        let path = "/tmp/stet_test_status.txt";
        std::fs::write(path, "x").ok();
        let ent = ctx.strings.allocate_from(path.as_bytes());
        ctx.o_stack
            .push(PsObject::string(ent, path.len() as u32))
            .unwrap();
        op_status(&mut ctx).unwrap();
        assert!(matches!(
            ctx.o_stack.pop().unwrap().value,
            PsValue::Bool(true)
        ));
        // Pop the 4 dummy values
        ctx.o_stack.pop().unwrap(); // created
        ctx.o_stack.pop().unwrap(); // referenced
        ctx.o_stack.pop().unwrap(); // bytes
        ctx.o_stack.pop().unwrap(); // pages

        // Test non-existing file — returns false
        let ent2 = ctx.strings.allocate_from(b"/tmp/stet_nonexistent_file_xyz");
        ctx.o_stack.push(PsObject::string(ent2, 29)).unwrap();
        op_status(&mut ctx).unwrap();
        assert!(matches!(
            ctx.o_stack.pop().unwrap().value,
            PsValue::Bool(false)
        ));

        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_bytesavailable() {
        let mut ctx = Context::new();
        ctx.o_stack
            .push(PsObject {
                value: PsValue::File(stet_core::file_store::FILE_STDIN),
                flags: ObjFlags::literal_composite(),
            })
            .unwrap();
        op_bytesavailable(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.pop().unwrap().as_i32(), Some(-1));
    }

    #[test]
    fn test_deletefile() {
        let mut ctx = Context::new();
        let path = "/tmp/stet_test_delete.txt";
        std::fs::write(path, "x").unwrap();

        let ent = ctx.strings.allocate_from(path.as_bytes());
        ctx.o_stack
            .push(PsObject::string(ent, path.len() as u32))
            .unwrap();
        op_deletefile(&mut ctx).unwrap();
        assert!(!std::path::Path::new(path).exists());
    }

    #[test]
    fn test_renamefile() {
        let mut ctx = Context::new();
        let old_path = "/tmp/stet_test_rename_old.txt";
        let new_path = "/tmp/stet_test_rename_new.txt";
        std::fs::write(old_path, "x").unwrap();

        let old_ent = ctx.strings.allocate_from(old_path.as_bytes());
        let new_ent = ctx.strings.allocate_from(new_path.as_bytes());
        ctx.o_stack
            .push(PsObject::string(old_ent, old_path.len() as u32))
            .unwrap();
        ctx.o_stack
            .push(PsObject::string(new_ent, new_path.len() as u32))
            .unwrap();
        op_renamefile(&mut ctx).unwrap();
        assert!(std::path::Path::new(new_path).exists());
        assert!(!std::path::Path::new(old_path).exists());
        std::fs::remove_file(new_path).ok();
    }
}
