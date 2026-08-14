// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! CID-keyed font operators.
//!
//! Implements `.cid_startdata` — the internal operator that backs the
//! `CIDInit` ProcSet's `StartData` procedure.

use stet_core::context::Context;
use stet_core::error::PsError;
use stet_core::object::{EntityId, PsValue};

/// `.cid_startdata`: `(Binary)|(Hex) count → —`
///
/// Consumes `count` bytes of CIDFontType 0 charstring data from the current
/// file.
///
/// Reading the data is not optional even though nothing draws with it yet.
/// The blob sits inline in the PostScript source, immediately after the font's
/// header, and runs to several megabytes; anything left unread goes straight
/// to the scanner, which tokenizes it as PostScript. That is where the whole
/// family of `undefined` errors on hex-looking names comes from — the scanner
/// reading a line of the CID map as a name.
///
/// The data itself is discarded. stet has no renderer for Type 1 charstrings
/// indexed through a CID map, so the font resolves the way any unavailable
/// font does, by substitution. Keeping several megabytes per font alive to be
/// used by nobody would be the only thing worse.
///
/// The `(Hex)` form counts *decoded* bytes, matching Ghostscript's
/// `gs_cidfn.ps`, which reads through an `ASCIIHexDecode` filter and then
/// closes it — closing consumes the trailing `>`, so this does too.
pub fn op_cid_startdata(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }

    let count = ctx.o_stack.peek(0)?.as_i32().ok_or(PsError::TypeCheck)?;
    if count < 0 {
        return Err(PsError::RangeCheck);
    }
    let format_obj = ctx.o_stack.peek(1)?;
    let is_hex = match format_obj.value {
        PsValue::String { entity, start, len } => ctx.strings.get(entity, start, len) == b"Hex",
        _ => return Err(PsError::TypeCheck),
    };

    ctx.o_stack.pop()?; // count
    ctx.o_stack.pop()?; // format

    let file_entity = current_file(ctx)?;
    ctx.pump_proc_sources(file_entity)?;

    let count = count as usize;
    if is_hex {
        skip_hex(ctx, file_entity, count)
    } else {
        skip_binary(ctx, file_entity, count)
    }
}

/// The topmost file on the execution stack — what `currentfile` returns.
fn current_file(ctx: &Context) -> Result<EntityId, PsError> {
    for i in 0..ctx.e_stack.len() {
        if let Ok(obj) = ctx.e_stack.peek(i)
            && let PsValue::File(e) = obj.value
        {
            return Ok(e);
        }
    }
    Err(PsError::InvalidFont)
}

/// Skip `count` raw bytes. A short read is not an error: in a truncated job
/// the data simply runs to the end of the file.
fn skip_binary(ctx: &mut Context, file: EntityId, count: usize) -> Result<(), PsError> {
    let mut remaining = count;
    let mut buf = vec![0u8; 65536];
    while remaining > 0 {
        let want = remaining.min(buf.len());
        let n = ctx
            .files
            .read_into(file, &mut buf[..want])
            .map_err(|_| PsError::IOError)?;
        if n == 0 {
            break;
        }
        remaining -= n;
    }
    Ok(())
}

/// Skip hex digits until `count` bytes' worth have been read, then consume the
/// `>` that ends the data — the byte an `ASCIIHexDecode` filter would eat when
/// it is closed. Whitespace between digits is ignored, as the filter does.
fn skip_hex(ctx: &mut Context, file: EntityId, count: usize) -> Result<(), PsError> {
    let mut digits = 0usize;
    let wanted = count * 2;
    while digits < wanted {
        let b = match ctx.files.read_byte(file).map_err(|_| PsError::IOError)? {
            Some(b) => b,
            None => return Ok(()),
        };
        match b {
            b'0'..=b'9' | b'a'..=b'f' | b'A'..=b'F' => digits += 1,
            b'>' => return Ok(()),
            _ => {} // whitespace and anything else the filter ignores
        }
    }
    // Consume the EOD marker so the scanner does not meet it.
    loop {
        match ctx.files.read_byte(file).map_err(|_| PsError::IOError)? {
            Some(b) if b.is_ascii_whitespace() => continue,
            Some(b'>') | None => break,
            Some(b) => {
                ctx.files.putback_bytes(file, &[b]);
                break;
            }
        }
    }
    Ok(())
}
