// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Text show operators: show, ashow, widthshow, awidthshow, kshow,
//! stringwidth, charpath, setcachedevice, setcharwidth.

use stet_core::charstring;
use stet_core::context::Context;
use stet_core::device::FillParams;
use stet_core::dict::DictKey;
use stet_core::display_list::DisplayElement;
use stet_core::error::PsError;
use stet_core::graphics_state::{FillRule, Matrix, PathSegment, PsPath};
use stet_core::object::{EntityId, PsObject, PsValue};
use stet_core::truetype;
use stet_core::type2_charstring;

/// `show`: string → —
///
/// Render each character at the current point, advancing by glyph width.
pub fn op_show(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let str_obj = ctx.o_stack.peek(0)?;
    let (entity, start, len) = match str_obj.value {
        PsValue::String { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };

    if ctx.gstate.current_point.is_none() {
        return Err(PsError::NoCurrentPoint);
    }

    let bytes = ctx.strings.get(entity, start, len).to_vec();
    ctx.o_stack.pop()?;

    render_show(ctx, &bytes, 0.0, 0.0, -1, 0.0, 0.0)?;
    Ok(())
}

/// `ashow`: ax ay string → —
///
/// Like show but adds (ax, ay) extra spacing after each character.
pub fn op_ashow(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 3 {
        return Err(PsError::StackUnderflow);
    }
    let str_obj = ctx.o_stack.peek(0)?;
    let ay_obj = ctx.o_stack.peek(1)?;
    let ax_obj = ctx.o_stack.peek(2)?;

    let (entity, start, len) = match str_obj.value {
        PsValue::String { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };
    let ay = ay_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let ax = ax_obj.as_f64().ok_or(PsError::TypeCheck)?;

    if ctx.gstate.current_point.is_none() {
        return Err(PsError::NoCurrentPoint);
    }

    let bytes = ctx.strings.get(entity, start, len).to_vec();
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;

    render_show(ctx, &bytes, ax, ay, -1, 0.0, 0.0)?;
    Ok(())
}

/// `widthshow`: cx cy char string → —
///
/// Like show but adds (cx, cy) extra spacing for a specific character code.
pub fn op_widthshow(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 4 {
        return Err(PsError::StackUnderflow);
    }
    let str_obj = ctx.o_stack.peek(0)?;
    let char_obj = ctx.o_stack.peek(1)?;
    let cy_obj = ctx.o_stack.peek(2)?;
    let cx_obj = ctx.o_stack.peek(3)?;

    let (entity, start, len) = match str_obj.value {
        PsValue::String { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };
    let width_char = match char_obj.value {
        PsValue::Int(v) => v,
        _ => return Err(PsError::TypeCheck),
    };
    let cy = cy_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let cx = cx_obj.as_f64().ok_or(PsError::TypeCheck)?;

    if !(0..=255).contains(&width_char) {
        return Err(PsError::RangeCheck);
    }

    if ctx.gstate.current_point.is_none() {
        return Err(PsError::NoCurrentPoint);
    }

    let bytes = ctx.strings.get(entity, start, len).to_vec();
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;

    render_show(ctx, &bytes, 0.0, 0.0, width_char, cx, cy)?;
    Ok(())
}

/// `awidthshow`: cx cy char ax ay string → —
///
/// Combined ashow + widthshow.
pub fn op_awidthshow(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 6 {
        return Err(PsError::StackUnderflow);
    }
    let str_obj = ctx.o_stack.peek(0)?;
    let ay_obj = ctx.o_stack.peek(1)?;
    let ax_obj = ctx.o_stack.peek(2)?;
    let char_obj = ctx.o_stack.peek(3)?;
    let cy_obj = ctx.o_stack.peek(4)?;
    let cx_obj = ctx.o_stack.peek(5)?;

    let (entity, start, len) = match str_obj.value {
        PsValue::String { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };
    let ay = ay_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let ax = ax_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let width_char = match char_obj.value {
        PsValue::Int(v) => v,
        _ => return Err(PsError::TypeCheck),
    };
    let cy = cy_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let cx = cx_obj.as_f64().ok_or(PsError::TypeCheck)?;

    if !(0..=255).contains(&width_char) {
        return Err(PsError::RangeCheck);
    }

    if ctx.gstate.current_point.is_none() {
        return Err(PsError::NoCurrentPoint);
    }

    let bytes = ctx.strings.get(entity, start, len).to_vec();
    for _ in 0..6 {
        ctx.o_stack.pop()?;
    }

    render_show(ctx, &bytes, ax, ay, width_char, cx, cy)?;
    Ok(())
}

/// `kshow`: proc string → —
///
/// Show string, calling proc between each pair of adjacent characters.
/// For each call, pushes charcode_just_shown and charcode_next onto
/// the operand stack before invoking proc.
pub fn op_kshow(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let str_obj = ctx.o_stack.peek(0)?;
    let proc_obj = ctx.o_stack.peek(1)?;

    let (entity, start, len) = match str_obj.value {
        PsValue::String { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };
    // Proc must be an executable array
    if !proc_obj.is_array_type() || !proc_obj.flags.is_executable() {
        return Err(PsError::TypeCheck);
    }

    if ctx.gstate.current_point.is_none() {
        return Err(PsError::NoCurrentPoint);
    }

    let bytes = ctx.strings.get(entity, start, len).to_vec();
    let proc = proc_obj;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;

    if bytes.is_empty() {
        return Ok(());
    }

    // Show first character
    render_show(ctx, &bytes[..1], 0.0, 0.0, -1, 0.0, 0.0)?;

    // For each subsequent character, call proc then show
    for i in 1..bytes.len() {
        let code_shown = bytes[i - 1] as i32;
        let code_next = bytes[i] as i32;
        ctx.o_stack.push(PsObject::int(code_shown))?;
        ctx.o_stack.push(PsObject::int(code_next))?;

        let exec_fn = ctx.exec_sync_fn.ok_or(PsError::Unregistered)?;
        exec_fn(ctx, proc)?;

        render_show(ctx, &bytes[i..i + 1], 0.0, 0.0, -1, 0.0, 0.0)?;
    }

    Ok(())
}

/// `stringwidth`: string → wx wy
///
/// Measure text width without rendering.
pub fn op_stringwidth(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let str_obj = ctx.o_stack.peek(0)?;
    let (entity, start, len) = match str_obj.value {
        PsValue::String { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };

    let bytes = ctx.strings.get(entity, start, len).to_vec();
    ctx.o_stack.pop()?;

    let (wx, wy) = measure_string_width(ctx, &bytes)?;
    ctx.o_stack.push(PsObject::real(wx))?;
    ctx.o_stack.push(PsObject::real(wy))?;
    Ok(())
}

/// `charpath`: string bool → —
///
/// Append glyph outlines to the current path.
pub fn op_charpath(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let bool_obj = ctx.o_stack.peek(0)?;
    let str_obj = ctx.o_stack.peek(1)?;

    // Validate types: bool first, then string (PLRM order)
    if !matches!(bool_obj.value, PsValue::Bool(_)) {
        return Err(PsError::TypeCheck);
    }
    let (entity, start, len) = match str_obj.value {
        PsValue::String { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };

    if ctx.gstate.current_point.is_none() {
        return Err(PsError::NoCurrentPoint);
    }

    let bytes = ctx.strings.get(entity, start, len).to_vec();
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;

    render_charpath(ctx, &bytes)?;
    Ok(())
}

/// `cshow`: proc string → —
///
/// Invoke proc for each character without painting. For each character,
/// pushes charcode wx wy on the operand stack and calls proc.
/// This is primarily used with composite fonts for per-character positioning.
///
/// For Type 0 composite fonts, multi-byte characters are decoded using the
/// CMap's CodeSpaceRange to determine byte width.
pub fn op_cshow(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let str_obj = ctx.o_stack.peek(0)?;
    let proc_obj = ctx.o_stack.peek(1)?;

    let (entity, start, len) = match str_obj.value {
        PsValue::String { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };

    // Proc must be executable array
    if !proc_obj.is_array_type() || !proc_obj.flags.is_executable() {
        return Err(PsError::TypeCheck);
    }

    if ctx.gstate.current_font.is_none() {
        return Err(PsError::InvalidFont);
    }

    let bytes = ctx.strings.get(entity, start, len).to_vec();
    let proc = proc_obj;
    ctx.o_stack.pop()?; // string
    ctx.o_stack.pop()?; // proc

    if bytes.is_empty() {
        return Ok(());
    }

    // Determine font type and byte width for composite fonts
    let font_obj = ctx.gstate.current_font.ok_or(PsError::InvalidFont)?;
    let font_entity = match font_obj.value {
        PsValue::Dict(e) => e,
        _ => return Err(PsError::InvalidFont),
    };
    let font_type = ctx
        .dicts
        .get(font_entity, &DictKey::Name(ctx.name_cache.n_font_type))
        .and_then(|obj| obj.as_i32())
        .unwrap_or(1);

    if font_type == 0 {
        // Type 0: decode through CMap for proper CID mapping
        let cid_pairs = decode_cmap_bytes(ctx, font_entity, &bytes);
        for (cid, _font_idx) in cid_pairs {
            ctx.o_stack.push(PsObject::int(cid))?;
            ctx.o_stack.push(PsObject::real(0.0))?; // wx
            ctx.o_stack.push(PsObject::real(0.0))?; // wy

            ctx.cshow_pending_cid = Some(cid);
            ctx.exec_sync(proc)?;
            ctx.cshow_pending_cid = None;
        }
    } else {
        // Simple font: one byte per character
        for &byte in &bytes {
            ctx.o_stack.push(PsObject::int(byte as i32))?;
            ctx.o_stack.push(PsObject::real(0.0))?; // wx
            ctx.o_stack.push(PsObject::real(0.0))?; // wy

            ctx.exec_sync(proc)?;
        }
    }

    Ok(())
}

/// Decode text bytes through a CMap, returning (CID, font_index) pairs.
///
/// Reads CodeSpaceRange (byte width), CIDRangeMappings (triples: lo hi base_cid),
/// and CIDCharMappings (pairs: src dst) from the CMap dict to map character codes
/// to CIDs. Falls back to identity mapping if no range matches.
fn decode_cmap_characters(
    ctx: &Context,
    font_entity: EntityId,
) -> Option<(EntityId, usize, usize)> {
    let cmap_name_id = ctx.names.find(b"CMap")?;
    let cmap_obj = ctx.dicts.get(font_entity, &DictKey::Name(cmap_name_id))?;
    let cmap_entity = match cmap_obj.value {
        PsValue::Dict(e) => e,
        _ => return None,
    };

    // Determine byte width from CodeSpaceRange
    let csr_name_id = ctx.names.find(b"CodeSpaceRange")?;
    let csr_obj = ctx.dicts.get(cmap_entity, &DictKey::Name(csr_name_id))?;
    let byte_width = match csr_obj.value {
        PsValue::Array { entity, start, len } if len >= 2 => {
            let first = ctx.arrays.get_element(entity, start);
            match first.value {
                PsValue::String { len: str_len, .. } => str_len as usize,
                _ => 1,
            }
        }
        _ => 1,
    };

    // Get CurrentFontNum (default 0)
    let font_num = ctx
        .names
        .find(b"CurrentFontNum")
        .and_then(|id| ctx.dicts.get(cmap_entity, &DictKey::Name(id)))
        .and_then(|obj| obj.as_i32())
        .unwrap_or(0) as usize;

    Some((cmap_entity, byte_width, font_num))
}

/// Convert a PS string object's bytes to an integer (big-endian).
fn string_to_int(ctx: &Context, obj: &PsObject) -> Option<i32> {
    match obj.value {
        PsValue::String { entity, start, len } => {
            let bytes = ctx.strings.get(entity, start, len);
            let mut val: i32 = 0;
            for &b in bytes {
                val = (val << 8) | (b as i32);
            }
            Some(val)
        }
        _ => None,
    }
}

/// Look up a character code in CMap mappings, returning the mapped CID.
///
/// Checks CIDCharMappings (exact match) first, then CIDRangeMappings (range lookup).
/// Falls back to identity (char_code) if no mapping matches.
fn cmap_lookup_cid(ctx: &Context, cmap_entity: EntityId, char_code: i32) -> i32 {
    // Check CIDCharMappings: flat array of pairs [src_string, dst_int, ...]
    if let Some(name_id) = ctx.names.find(b"CIDCharMappings")
        && let Some(obj) = ctx.dicts.get(cmap_entity, &DictKey::Name(name_id))
        && let PsValue::Array { entity, start, len } = obj.value
    {
        let mut i = 0;
        while i + 1 < len {
            let src = ctx.arrays.get_element(entity, start + i);
            let dst = ctx.arrays.get_element(entity, start + i + 1);
            if let Some(src_val) = string_to_int(ctx, &src)
                && src_val == char_code
            {
                return dst.as_i32().unwrap_or(char_code);
            }
            i += 2;
        }
    }

    // Check CIDRangeMappings: flat array of triples [lo_string, hi_string, base_cid_int, ...]
    if let Some(name_id) = ctx.names.find(b"CIDRangeMappings")
        && let Some(obj) = ctx.dicts.get(cmap_entity, &DictKey::Name(name_id))
        && let PsValue::Array { entity, start, len } = obj.value
    {
        let mut i = 0;
        while i + 2 < len {
            let lo = ctx.arrays.get_element(entity, start + i);
            let hi = ctx.arrays.get_element(entity, start + i + 1);
            let base = ctx.arrays.get_element(entity, start + i + 2);
            if let (Some(lo_val), Some(hi_val)) = (string_to_int(ctx, &lo), string_to_int(ctx, &hi))
                && char_code >= lo_val
                && char_code <= hi_val
            {
                let base_cid = base.as_i32().unwrap_or(0);
                return base_cid + (char_code - lo_val);
            }
            i += 3;
        }
    }

    // Fallback: identity
    char_code
}

/// Decode bytes through CMap and return (CID, font_index) pairs.
fn decode_cmap_bytes(ctx: &Context, font_entity: EntityId, bytes: &[u8]) -> Vec<(i32, usize)> {
    let (cmap_entity, byte_width, font_num) = match decode_cmap_characters(ctx, font_entity) {
        Some(v) => v,
        None => {
            // No CMap — treat each byte as a CID (identity)
            return bytes.iter().map(|&b| (b as i32, 0)).collect();
        }
    };

    let mut result = Vec::new();
    let mut pos = 0;
    while pos + byte_width <= bytes.len() {
        let mut char_code: i32 = 0;
        for i in 0..byte_width {
            char_code = (char_code << 8) | (bytes[pos + i] as i32);
        }
        let cid = cmap_lookup_cid(ctx, cmap_entity, char_code);
        result.push((cid, font_num));
        pos += byte_width;
    }
    result
}

/// `xshow`: string numarray → —
///
/// Show with per-character x displacements. Each number in the array
/// specifies the x displacement for the corresponding character; y is 0.
pub fn op_xshow(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let disp_obj = ctx.o_stack.peek(0)?;
    let str_obj = ctx.o_stack.peek(1)?;

    let (str_entity, str_start, str_len) = match str_obj.value {
        PsValue::String { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };
    let displacements = extract_displacements(ctx, &disp_obj)?;

    if ctx.gstate.current_point.is_none() {
        return Err(PsError::NoCurrentPoint);
    }

    let bytes = ctx.strings.get(str_entity, str_start, str_len).to_vec();

    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;

    render_show_displaced(ctx, &bytes, &displacements, DisplacementMode::X)?;
    Ok(())
}

/// `yshow`: string numarray → —
///
/// Show with per-character y displacements. Each number in the array
/// specifies the y displacement for the corresponding character; x is 0.
pub fn op_yshow(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let disp_obj = ctx.o_stack.peek(0)?;
    let str_obj = ctx.o_stack.peek(1)?;

    let (str_entity, str_start, str_len) = match str_obj.value {
        PsValue::String { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };
    let displacements = extract_displacements(ctx, &disp_obj)?;

    if ctx.gstate.current_point.is_none() {
        return Err(PsError::NoCurrentPoint);
    }

    let bytes = ctx.strings.get(str_entity, str_start, str_len).to_vec();

    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;

    render_show_displaced(ctx, &bytes, &displacements, DisplacementMode::Y)?;
    Ok(())
}

/// `xyshow`: string numarray → —
///
/// Show with per-character x,y displacement pairs. The array must contain
/// 2 × len(string) numbers: x0 y0 x1 y1 ...
pub fn op_xyshow(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let disp_obj = ctx.o_stack.peek(0)?;
    let str_obj = ctx.o_stack.peek(1)?;

    let (str_entity, str_start, str_len) = match str_obj.value {
        PsValue::String { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };
    let displacements = extract_displacements(ctx, &disp_obj)?;

    if ctx.gstate.current_point.is_none() {
        return Err(PsError::NoCurrentPoint);
    }

    let bytes = ctx.strings.get(str_entity, str_start, str_len).to_vec();

    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;

    render_show_displaced(ctx, &bytes, &displacements, DisplacementMode::XY)?;
    Ok(())
}

/// `setcachedevice`: wx wy llx lly urx ury → —
///
/// Set cache device parameters. Records the character width (wx, wy)
/// for Type 3 font BuildChar procedures.
pub fn op_setcachedevice(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 6 {
        return Err(PsError::StackUnderflow);
    }
    // Read wx, wy before popping
    let wx = ctx.o_stack.peek(5)?.as_f64().ok_or(PsError::TypeCheck)?;
    let wy = ctx.o_stack.peek(4)?.as_f64().ok_or(PsError::TypeCheck)?;
    for _ in 0..6 {
        ctx.o_stack.pop()?;
    }
    ctx.char_width = Some((wx, wy));
    Ok(())
}

/// `setcachedevice2`: w0x w0y llx lly urx ury w1x w1y vx vy → —
///
/// Set cache device parameters for dual writing mode metrics.
/// Records the mode 0 character width (w0x, w0y) for Type 3 font
/// BuildChar procedures. Mode 1 metrics are validated but not stored.
pub fn op_setcachedevice2(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 10 {
        return Err(PsError::StackUnderflow);
    }
    // Validate all 10 operands are numeric
    for i in 0..10 {
        ctx.o_stack.peek(i)?.as_f64().ok_or(PsError::TypeCheck)?;
    }
    // Read w0x, w0y (mode 0 width) before popping
    let w0x = ctx.o_stack.peek(9)?.as_f64().unwrap();
    let w0y = ctx.o_stack.peek(8)?.as_f64().unwrap();
    for _ in 0..10 {
        ctx.o_stack.pop()?;
    }
    ctx.char_width = Some((w0x, w0y));
    Ok(())
}

/// `setcharwidth`: wx wy → —
///
/// Set character width (for Type 3 fonts). Records the width for
/// BuildChar character advancement.
pub fn op_setcharwidth(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let wy = ctx.o_stack.peek(0)?.as_f64().ok_or(PsError::TypeCheck)?;
    let wx = ctx.o_stack.peek(1)?.as_f64().ok_or(PsError::TypeCheck)?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.char_width = Some((wx, wy));
    Ok(())
}

/// `glyphshow`: name → —
///
/// Render a single glyph identified by name at the current point,
/// advancing by the glyph's width. The glyph is looked up in the
/// current font's CharStrings dictionary by name.
pub fn op_glyphshow(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let name_obj = ctx.o_stack.peek(0)?;
    let glyph_name_id = match name_obj.value {
        PsValue::Name(id) => id,
        _ => return Err(PsError::TypeCheck),
    };

    if ctx.gstate.current_point.is_none() {
        return Err(PsError::NoCurrentPoint);
    }

    let font_obj = ctx.gstate.current_font.ok_or(PsError::InvalidFont)?;
    let font_entity = match font_obj.value {
        PsValue::Dict(e) => e,
        _ => return Err(PsError::InvalidFont),
    };

    ctx.o_stack.pop()?;

    let font_type = ctx
        .dicts
        .get(font_entity, &DictKey::Name(ctx.name_cache.n_font_type))
        .and_then(|obj| obj.as_i32())
        .unwrap_or(1);

    // Get CharStrings dict
    let cs_entity = ctx
        .dicts
        .get(font_entity, &DictKey::Name(ctx.name_cache.n_char_strings))
        .and_then(|obj| match obj.value {
            PsValue::Dict(e) => Some(e),
            _ => None,
        })
        .ok_or(PsError::InvalidFont)?;

    // Look up charstring by glyph name
    let cs_obj = ctx
        .dicts
        .get(cs_entity, &DictKey::Name(glyph_name_id))
        .ok_or(PsError::Undefined)?;

    let (cs_str_entity, cs_start, cs_len) = match cs_obj.value {
        PsValue::String { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::InvalidFont),
    };
    let cs_bytes = ctx.strings.get(cs_str_entity, cs_start, cs_len).to_vec();

    let font_matrix = read_font_matrix(ctx, font_entity);

    // Inverse-transform device-space current_point to user space
    let (dev_cpx, dev_cpy) = ctx.gstate.current_point.ok_or(PsError::NoCurrentPoint)?;
    let ictm = ctx.gstate.ctm.invert().ok_or(PsError::UndefinedResult)?;
    let (cur_x, cur_y) = ictm.transform_point(dev_cpx, dev_cpy);
    let ctm = ctx.gstate.ctm;

    if font_type == 2 {
        // Type 2 (CFF) charstring
        let info = get_type2_info(ctx, font_entity)?;
        let result = type2_charstring::execute_type2_charstring(
            &cs_bytes,
            &info.local_subrs,
            &info.global_subrs,
            info.default_width_x,
            info.nominal_width_x,
            false,
        )
        .map_err(|_| PsError::InvalidFont)?;

        if !result.path.is_empty() {
            let user_path = transform_path(&result.path, &font_matrix, cur_x, cur_y);
            let device_path = ctm_transform_path(&user_path, &ctm);
            let params = FillParams {
                color: ctx.gstate.color.clone(),
                ctm: Matrix::identity(),
                fill_rule: FillRule::NonZeroWinding,
            };
            ctx.display_list.push(DisplayElement::Fill {
                path: device_path,
                params,
            });
        }

        let (wx, wy) = font_matrix.transform_delta(result.width_x, result.width_y);
        let (dev_x, dev_y) = ctm.transform_point(cur_x + wx, cur_y + wy);
        ctx.gstate.current_point = Some((dev_x, dev_y));
    } else {
        // Type 1 charstring
        let info = get_font_info(ctx)?;
        let subrs = get_subrs(ctx, &info);
        let result = charstring::execute_charstring(&cs_bytes, &subrs, info.len_iv, false)
            .map_err(|_| PsError::InvalidFont)?;

        if !result.path.is_empty() {
            let user_path = transform_path(&result.path, &font_matrix, cur_x, cur_y);
            let device_path = ctm_transform_path(&user_path, &ctm);
            let params = FillParams {
                color: ctx.gstate.color.clone(),
                ctm: Matrix::identity(),
                fill_rule: FillRule::NonZeroWinding,
            };
            ctx.display_list.push(DisplayElement::Fill {
                path: device_path,
                params,
            });
        }

        let (wx, wy) = font_matrix.transform_delta(result.width_x, result.width_y);
        let (dev_x, dev_y) = ctm.transform_point(cur_x + wx, cur_y + wy);
        ctx.gstate.current_point = Some((dev_x, dev_y));
    }

    Ok(())
}

// --- Internal rendering helpers ---

/// Extract font components needed for glyph rendering.
struct FontInfo {
    #[allow(dead_code)]
    font_entity: EntityId,
    font_matrix: Matrix,
    encoding_entity: EntityId,
    charstrings_entity: EntityId,
    subrs_entity: EntityId,
    subrs_len: u32,
    len_iv: usize,
    /// Optional Metrics dict entity for width overrides (PLRM 5.9.2, used by dvips)
    metrics_entity: Option<EntityId>,
}

/// Extract font info from the current font dict.
fn get_font_info(ctx: &Context) -> Result<FontInfo, PsError> {
    let font_obj = ctx.gstate.current_font.ok_or(PsError::InvalidFont)?;
    let font_entity = match font_obj.value {
        PsValue::Dict(e) => e,
        _ => return Err(PsError::InvalidFont),
    };

    // FontMatrix
    let fm_obj = ctx
        .dicts
        .get(font_entity, &DictKey::Name(ctx.name_cache.n_font_matrix))
        .ok_or(PsError::InvalidFont)?;
    let (fm_e, fm_s, fm_l) = match fm_obj.value {
        PsValue::Array { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::InvalidFont),
    };
    let fm_elems = ctx.arrays.get(fm_e, fm_s, fm_l);
    let font_matrix = Matrix::new(
        fm_elems[0].as_f64().ok_or(PsError::InvalidFont)?,
        fm_elems[1].as_f64().ok_or(PsError::InvalidFont)?,
        fm_elems[2].as_f64().ok_or(PsError::InvalidFont)?,
        fm_elems[3].as_f64().ok_or(PsError::InvalidFont)?,
        fm_elems[4].as_f64().ok_or(PsError::InvalidFont)?,
        fm_elems[5].as_f64().ok_or(PsError::InvalidFont)?,
    );

    // Encoding
    let enc_obj = ctx
        .dicts
        .get(font_entity, &DictKey::Name(ctx.name_cache.n_encoding))
        .ok_or(PsError::InvalidFont)?;
    let encoding_entity = match enc_obj.value {
        PsValue::Array { entity, .. } => entity,
        _ => return Err(PsError::InvalidFont),
    };

    // CharStrings
    let cs_obj = ctx
        .dicts
        .get(font_entity, &DictKey::Name(ctx.name_cache.n_char_strings))
        .ok_or(PsError::InvalidFont)?;
    let charstrings_entity = match cs_obj.value {
        PsValue::Dict(e) => e,
        _ => return Err(PsError::InvalidFont),
    };

    // Private dict (contains Subrs and lenIV per Type 1 spec)
    let private_entity = ctx
        .dicts
        .get(font_entity, &DictKey::Name(ctx.name_cache.n_private))
        .and_then(|obj| match obj.value {
            PsValue::Dict(e) => Some(e),
            _ => None,
        });

    // Subrs from Private dict
    let (subrs_entity, subrs_len) = private_entity
        .and_then(|pe| ctx.dicts.get(pe, &DictKey::Name(ctx.name_cache.n_subrs)))
        .and_then(|obj| match obj.value {
            PsValue::Array { entity, len, .. } => Some((entity, len)),
            _ => None,
        })
        .unwrap_or((EntityId(0), 0));

    // lenIV from Private dict
    let len_iv = private_entity
        .and_then(|pe| {
            ctx.dicts
                .get(pe, &DictKey::Name(ctx.name_cache.n_len_iv))
                .and_then(|v| v.as_i32())
        })
        .unwrap_or(4) as usize;

    // Metrics dict (optional, used by dvips for width overrides)
    let metrics_entity = ctx
        .dicts
        .get(font_entity, &DictKey::Name(ctx.name_cache.n_metrics))
        .and_then(|obj| match obj.value {
            PsValue::Dict(e) => Some(e),
            _ => None,
        });

    Ok(FontInfo {
        font_entity,
        font_matrix,
        encoding_entity,
        charstrings_entity,
        subrs_entity,
        subrs_len,
        len_iv,
        metrics_entity,
    })
}

/// Extract subroutines as a Vec<Vec<u8>> from the font's Subrs array.
fn get_subrs(ctx: &Context, info: &FontInfo) -> Vec<Vec<u8>> {
    if info.subrs_len == 0 {
        return Vec::new();
    }
    let elems = ctx.arrays.get(info.subrs_entity, 0, info.subrs_len);
    elems
        .iter()
        .map(|obj| match obj.value {
            PsValue::String { entity, start, len } => ctx.strings.get(entity, start, len).to_vec(),
            _ => Vec::new(),
        })
        .collect()
}

/// Build a lookup table mapping StandardEncoding glyph names to encrypted charstring bytes.
/// Used by the charstring interpreter for seac (composite character) support.
fn build_seac_map(ctx: &Context, info: &FontInfo) -> std::collections::HashMap<String, Vec<u8>> {
    use stet_core::encoding::STANDARD_ENCODING;
    let mut map = std::collections::HashMap::new();
    for &name in STANDARD_ENCODING.iter() {
        if name == ".notdef" {
            continue;
        }
        if let Some(PsValue::String { entity, start, len }) = ctx
            .names
            .find(name.as_bytes())
            .and_then(|name_id| {
                ctx.dicts
                    .get(info.charstrings_entity, &DictKey::Name(name_id))
            })
            .map(|obj| obj.value)
        {
            map.insert(
                name.to_string(),
                ctx.strings.get(entity, start, len).to_vec(),
            );
        }
    }
    map
}

/// Look up a Metrics dict width override for a character (PLRM 5.9.2).
///
/// Supports both dvips-style integer char code keys and PLRM-standard glyph name keys.
/// Returns width in character space (needs FontMatrix transform by caller).
fn get_metrics_width(
    ctx: &Context,
    info: &FontInfo,
    glyph_name_id: stet_core::object::NameId,
    char_code: u8,
) -> Option<f64> {
    let metrics_entity = info.metrics_entity?;

    // Try integer char code (dvips) first, then glyph name (PLRM standard)
    let entry = ctx
        .dicts
        .get(metrics_entity, &DictKey::Int(char_code as i32))
        .or_else(|| ctx.dicts.get(metrics_entity, &DictKey::Name(glyph_name_id)));

    let entry = entry?;

    // Extract numeric width from Metrics entry
    // PLRM formats: wx | [wx wy] | [llx lly wx wy]
    match entry.value {
        PsValue::Int(v) => Some(v as f64),
        PsValue::Real(v) => Some(v),
        PsValue::Array { entity, start, len } => {
            let elems = ctx.arrays.get(entity, start, len);
            if len == 4 {
                // [llx lly wx wy] — width is element 2
                elems[2].as_f64()
            } else if len >= 2 {
                // [wx wy]
                elems[0].as_f64()
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Core show rendering: iterate over string bytes, look up glyphs, render paths.
///
/// Glyph advancement is tracked in user space. Glyph paths are built in user space
/// via FontMatrix, then transformed through CTM to device space for rendering.
/// current_point is stored in device space.
fn render_show(
    ctx: &mut Context,
    bytes: &[u8],
    extra_ax: f64,
    extra_ay: f64,
    width_char: i32,
    cx: f64,
    cy: f64,
) -> Result<(), PsError> {
    if bytes.is_empty() {
        return Ok(());
    }

    // Check FontType to dispatch between Type 1 and Type 3
    let font_obj = ctx.gstate.current_font.ok_or(PsError::InvalidFont)?;
    let font_entity = match font_obj.value {
        PsValue::Dict(e) => e,
        _ => return Err(PsError::InvalidFont),
    };
    let font_type = ctx
        .dicts
        .get(font_entity, &DictKey::Name(ctx.name_cache.n_font_type))
        .and_then(|obj| obj.as_i32())
        .unwrap_or(1);

    if font_type == 3 {
        return render_show_type3(ctx, bytes, extra_ax, extra_ay, width_char, cx, cy);
    }

    // Type 0 (composite) and Type 42 (TrueType)
    if font_type == 0 || font_type == 42 {
        return render_show_composite(
            ctx,
            font_entity,
            bytes,
            extra_ax,
            extra_ay,
            width_char,
            cx,
            cy,
        );
    }

    // Type 2 (CFF)
    if font_type == 2 {
        return render_show_type2(
            ctx,
            font_entity,
            bytes,
            extra_ax,
            extra_ay,
            width_char,
            cx,
            cy,
        );
    }

    let info = get_font_info(ctx)?;
    let subrs = get_subrs(ctx, &info);

    // Build seac lookup table: StandardEncoding glyph names → encrypted charstring bytes
    let seac_map = build_seac_map(ctx, &info);
    let cs_lookup = |name: &str| -> Option<Vec<u8>> { seac_map.get(name).cloned() };

    // Inverse-transform device-space current_point to user space for advancement math
    let (dev_cpx, dev_cpy) = ctx.gstate.current_point.ok_or(PsError::NoCurrentPoint)?;
    let ictm = ctx.gstate.ctm.invert().ok_or(PsError::UndefinedResult)?;
    let (mut cur_x, mut cur_y) = ictm.transform_point(dev_cpx, dev_cpy);

    let ctm = ctx.gstate.ctm;

    for &byte in bytes {
        // Look up glyph name from encoding
        let glyph_name_obj = ctx.arrays.get_element(info.encoding_entity, byte as u32);
        let glyph_name_id = match glyph_name_obj.value {
            PsValue::Name(id) => id,
            _ => continue,
        };

        // Look up charstring (works for both .notdef and regular glyphs)
        let cs_key = DictKey::Name(glyph_name_id);
        let cs_obj = match ctx.dicts.get(info.charstrings_entity, &cs_key) {
            Some(obj) => obj,
            None => continue, // glyph not in font
        };

        let (cs_entity, cs_start, cs_len) = match cs_obj.value {
            PsValue::String { entity, start, len } => (entity, start, len),
            _ => continue,
        };

        let cs_bytes = ctx.strings.get(cs_entity, cs_start, cs_len).to_vec();

        // Execute charstring with seac support
        let result = charstring::execute_charstring_ex(
            &cs_bytes,
            &subrs,
            info.len_iv,
            false,
            Some(&cs_lookup as &charstring::CharstringLookup<'_>),
        )
        .map_err(|_| PsError::InvalidFont)?;

        // Transform glyph path: glyph space → user space (FontMatrix), then → device space (CTM)
        if !result.path.is_empty() {
            let user_path = transform_path(&result.path, &info.font_matrix, cur_x, cur_y);
            let device_path = ctm_transform_path(&user_path, &ctm);

            // Fill the glyph (path is already device-space)
            let params = FillParams {
                color: ctx.gstate.color.clone(),
                ctm: Matrix::identity(),
                fill_rule: FillRule::NonZeroWinding,
            };
            ctx.display_list.push(DisplayElement::Fill {
                path: device_path,
                params,
            });
        }

        // Advance currentpoint: use Metrics override if present, else charstring width
        let (wx, wy) = if let Some(metrics_wx) = get_metrics_width(ctx, &info, glyph_name_id, byte)
        {
            // Metrics widths are in character space — transform through FontMatrix
            info.font_matrix.transform_delta(metrics_wx, 0.0)
        } else {
            info.font_matrix
                .transform_delta(result.width_x, result.width_y)
        };
        cur_x += wx + extra_ax;
        cur_y += wy + extra_ay;

        if byte as i32 == width_char {
            cur_x += cx;
            cur_y += cy;
        }
    }

    // Transform final user-space position through CTM to device-space current_point
    let (dev_x, dev_y) = ctm.transform_point(cur_x, cur_y);
    ctx.gstate.current_point = Some((dev_x, dev_y));
    Ok(())
}

// ---------------------------------------------------------------------------
// Type 2 (CFF) font rendering
// ---------------------------------------------------------------------------

/// Extract Type 2 font info from a font dictionary.
struct Type2Info {
    font_matrix: Matrix,
    encoding_entity: EntityId,
    charstrings_entity: EntityId,
    default_width_x: f64,
    nominal_width_x: f64,
    local_subrs: Vec<Vec<u8>>,
    global_subrs: Vec<Vec<u8>>,
    metrics_entity: Option<EntityId>,
}

/// Extract Type 2 specific font info from a CFF font dictionary.
fn get_type2_info(ctx: &Context, font_entity: EntityId) -> Result<Type2Info, PsError> {
    let font_matrix = read_font_matrix(ctx, font_entity);

    // Encoding array
    let enc_obj = ctx
        .dicts
        .get(font_entity, &DictKey::Name(ctx.name_cache.n_encoding))
        .ok_or(PsError::InvalidFont)?;
    let encoding_entity = match enc_obj.value {
        PsValue::Array { entity, .. } => entity,
        _ => return Err(PsError::InvalidFont),
    };

    // CharStrings dict
    let cs_obj = ctx
        .dicts
        .get(font_entity, &DictKey::Name(ctx.name_cache.n_char_strings))
        .ok_or(PsError::InvalidFont)?;
    let charstrings_entity = match cs_obj.value {
        PsValue::Dict(e) => e,
        _ => return Err(PsError::InvalidFont),
    };

    // Private dict → defaultWidthX, nominalWidthX, Subrs
    let private_entity = ctx
        .dicts
        .get(font_entity, &DictKey::Name(ctx.name_cache.n_private))
        .and_then(|obj| match obj.value {
            PsValue::Dict(e) => Some(e),
            _ => None,
        });

    let mut default_width_x = 0.0;
    let mut nominal_width_x = 0.0;
    let mut local_subrs = Vec::new();

    if let Some(pe) = private_entity {
        if let Some(obj) = ctx.dicts.get(
            pe,
            &DictKey::Name(
                ctx.names
                    .find(b"defaultWidthX")
                    .unwrap_or(ctx.name_cache.n_len_iv),
            ),
        ) && let Some(v) = obj.as_f64()
        {
            default_width_x = v;
        }
        // Try with interned name
        if let Some(name_id) = ctx.names.find(b"defaultWidthX")
            && let Some(obj) = ctx.dicts.get(pe, &DictKey::Name(name_id))
            && let Some(v) = obj.as_f64()
        {
            default_width_x = v;
        }
        if let Some(name_id) = ctx.names.find(b"nominalWidthX")
            && let Some(obj) = ctx.dicts.get(pe, &DictKey::Name(name_id))
            && let Some(v) = obj.as_f64()
        {
            nominal_width_x = v;
        }

        // Local subrs
        if let Some(obj) = ctx.dicts.get(pe, &DictKey::Name(ctx.name_cache.n_subrs))
            && let PsValue::Array { entity, start, len } = obj.value
        {
            let elems = ctx.arrays.get(entity, start, len);
            local_subrs = elems
                .iter()
                .map(|o| match o.value {
                    PsValue::String { entity, start, len } => {
                        ctx.strings.get(entity, start, len).to_vec()
                    }
                    _ => Vec::new(),
                })
                .collect();
        }
    }

    // Global subrs
    let mut global_subrs = Vec::new();
    if let Some(name_id) = ctx.names.find(b"_cff_global_subrs")
        && let Some(obj) = ctx.dicts.get(font_entity, &DictKey::Name(name_id))
        && let PsValue::Array { entity, start, len } = obj.value
    {
        let elems = ctx.arrays.get(entity, start, len);
        global_subrs = elems
            .iter()
            .map(|o| match o.value {
                PsValue::String { entity, start, len } => {
                    ctx.strings.get(entity, start, len).to_vec()
                }
                _ => Vec::new(),
            })
            .collect();
    }

    // Metrics dict (optional)
    let metrics_entity = ctx
        .dicts
        .get(font_entity, &DictKey::Name(ctx.name_cache.n_metrics))
        .and_then(|obj| match obj.value {
            PsValue::Dict(e) => Some(e),
            _ => None,
        });

    Ok(Type2Info {
        font_matrix,
        encoding_entity,
        charstrings_entity,
        default_width_x,
        nominal_width_x,
        local_subrs,
        global_subrs,
        metrics_entity,
    })
}

/// Render show for Type 2 (CFF) fonts.
#[allow(clippy::too_many_arguments)]
fn render_show_type2(
    ctx: &mut Context,
    font_entity: EntityId,
    bytes: &[u8],
    extra_ax: f64,
    extra_ay: f64,
    width_char: i32,
    cx: f64,
    cy: f64,
) -> Result<(), PsError> {
    let info = get_type2_info(ctx, font_entity)?;

    let (dev_cpx, dev_cpy) = ctx.gstate.current_point.ok_or(PsError::NoCurrentPoint)?;
    let ictm = ctx.gstate.ctm.invert().ok_or(PsError::UndefinedResult)?;
    let (mut cur_x, mut cur_y) = ictm.transform_point(dev_cpx, dev_cpy);
    let ctm = ctx.gstate.ctm;

    for &byte in bytes {
        // Look up glyph name from encoding
        let glyph_name_obj = ctx.arrays.get_element(info.encoding_entity, byte as u32);
        let glyph_name_id = match glyph_name_obj.value {
            PsValue::Name(id) => id,
            _ => continue,
        };

        // Look up charstring
        let cs_key = DictKey::Name(glyph_name_id);
        let cs_obj = match ctx.dicts.get(info.charstrings_entity, &cs_key) {
            Some(obj) => obj,
            None => continue,
        };

        let (cs_entity, cs_start, cs_len) = match cs_obj.value {
            PsValue::String { entity, start, len } => (entity, start, len),
            _ => continue,
        };

        let cs_bytes = ctx.strings.get(cs_entity, cs_start, cs_len).to_vec();

        // Execute Type 2 charstring
        let result = type2_charstring::execute_type2_charstring(
            &cs_bytes,
            &info.local_subrs,
            &info.global_subrs,
            info.default_width_x,
            info.nominal_width_x,
            false,
        )
        .map_err(|_| PsError::InvalidFont)?;

        // Transform glyph path and fill
        if !result.path.is_empty() {
            let user_path = transform_path(&result.path, &info.font_matrix, cur_x, cur_y);
            let device_path = ctm_transform_path(&user_path, &ctm);

            let params = FillParams {
                color: ctx.gstate.color.clone(),
                ctm: Matrix::identity(),
                fill_rule: FillRule::NonZeroWinding,
            };
            ctx.display_list.push(DisplayElement::Fill {
                path: device_path,
                params,
            });
        }

        // Advance currentpoint
        let (wx, wy) = if let Some(metrics_entity) = info.metrics_entity {
            if let Some(mw) = get_metrics_width_type2(ctx, metrics_entity, glyph_name_id, byte) {
                info.font_matrix.transform_delta(mw, 0.0)
            } else {
                info.font_matrix
                    .transform_delta(result.width_x, result.width_y)
            }
        } else {
            info.font_matrix
                .transform_delta(result.width_x, result.width_y)
        };
        cur_x += wx + extra_ax;
        cur_y += wy + extra_ay;

        if byte as i32 == width_char {
            cur_x += cx;
            cur_y += cy;
        }
    }

    let (dev_x, dev_y) = ctm.transform_point(cur_x, cur_y);
    ctx.gstate.current_point = Some((dev_x, dev_y));
    Ok(())
}

/// Measure string width for Type 2 (CFF) fonts.
fn measure_string_width_type2(
    ctx: &mut Context,
    font_entity: EntityId,
    bytes: &[u8],
) -> Result<(f64, f64), PsError> {
    let info = get_type2_info(ctx, font_entity)?;

    let mut total_wx = 0.0;
    let mut total_wy = 0.0;

    for &byte in bytes {
        let glyph_name_obj = ctx.arrays.get_element(info.encoding_entity, byte as u32);
        let glyph_name_id = match glyph_name_obj.value {
            PsValue::Name(id) => id,
            _ => continue,
        };

        let cs_key = DictKey::Name(glyph_name_id);
        let cs_obj = match ctx.dicts.get(info.charstrings_entity, &cs_key) {
            Some(obj) => obj,
            None => continue,
        };

        let (cs_entity, cs_start, cs_len) = match cs_obj.value {
            PsValue::String { entity, start, len } => (entity, start, len),
            _ => continue,
        };

        let cs_bytes = ctx.strings.get(cs_entity, cs_start, cs_len).to_vec();

        let result = type2_charstring::execute_type2_charstring(
            &cs_bytes,
            &info.local_subrs,
            &info.global_subrs,
            info.default_width_x,
            info.nominal_width_x,
            true, // width_only
        )
        .map_err(|_| PsError::InvalidFont)?;

        let (wx, wy) = if let Some(metrics_entity) = info.metrics_entity {
            if let Some(mw) = get_metrics_width_type2(ctx, metrics_entity, glyph_name_id, byte) {
                info.font_matrix.transform_delta(mw, 0.0)
            } else {
                info.font_matrix
                    .transform_delta(result.width_x, result.width_y)
            }
        } else {
            info.font_matrix
                .transform_delta(result.width_x, result.width_y)
        };
        total_wx += wx;
        total_wy += wy;
    }

    Ok((total_wx, total_wy))
}

/// Render charpath for Type 2 (CFF) fonts.
fn render_charpath_type2(
    ctx: &mut Context,
    font_entity: EntityId,
    bytes: &[u8],
) -> Result<(), PsError> {
    let info = get_type2_info(ctx, font_entity)?;

    let (dev_cpx, dev_cpy) = ctx.gstate.current_point.ok_or(PsError::NoCurrentPoint)?;
    let ictm = ctx.gstate.ctm.invert().ok_or(PsError::UndefinedResult)?;
    let (mut cur_x, mut cur_y) = ictm.transform_point(dev_cpx, dev_cpy);
    let ctm = ctx.gstate.ctm;

    for &byte in bytes {
        let glyph_name_obj = ctx.arrays.get_element(info.encoding_entity, byte as u32);
        let glyph_name_id = match glyph_name_obj.value {
            PsValue::Name(id) => id,
            _ => continue,
        };

        let cs_key = DictKey::Name(glyph_name_id);
        let cs_obj = match ctx.dicts.get(info.charstrings_entity, &cs_key) {
            Some(obj) => obj,
            None => continue,
        };

        let (cs_entity, cs_start, cs_len) = match cs_obj.value {
            PsValue::String { entity, start, len } => (entity, start, len),
            _ => continue,
        };

        let cs_bytes = ctx.strings.get(cs_entity, cs_start, cs_len).to_vec();

        let result = type2_charstring::execute_type2_charstring(
            &cs_bytes,
            &info.local_subrs,
            &info.global_subrs,
            info.default_width_x,
            info.nominal_width_x,
            false,
        )
        .map_err(|_| PsError::InvalidFont)?;

        // Append device-space glyph path to current path
        if !result.path.is_empty() {
            let user_path = transform_path(&result.path, &info.font_matrix, cur_x, cur_y);
            let device_path = ctm_transform_path(&user_path, &ctm);

            let mut segs_iter = device_path.segments.into_iter();
            if let Some(first_seg) = segs_iter.next() {
                if let PathSegment::MoveTo(x, y) = first_seg {
                    crate::path_ops::path_moveto(&mut ctx.gstate.path, x, y);
                } else {
                    ctx.gstate.path.segments.push(first_seg);
                }
                ctx.gstate.path.segments.extend(segs_iter);
            }
        }

        let (wx, wy) = info
            .font_matrix
            .transform_delta(result.width_x, result.width_y);
        cur_x += wx;
        cur_y += wy;
    }

    let (dev_x, dev_y) = ctm.transform_point(cur_x, cur_y);
    ctx.gstate.current_point = Some((dev_x, dev_y));
    Ok(())
}

/// Look up a Metrics dict width override for Type 2 fonts.
fn get_metrics_width_type2(
    ctx: &Context,
    metrics_entity: EntityId,
    glyph_name_id: stet_core::object::NameId,
    char_code: u8,
) -> Option<f64> {
    let entry = ctx
        .dicts
        .get(metrics_entity, &DictKey::Int(char_code as i32))
        .or_else(|| ctx.dicts.get(metrics_entity, &DictKey::Name(glyph_name_id)));

    let entry = entry?;

    match entry.value {
        PsValue::Int(v) => Some(v as f64),
        PsValue::Real(v) => Some(v),
        PsValue::Array { entity, start, len } => {
            let elems = ctx.arrays.get(entity, start, len);
            if len == 4 {
                elems[2].as_f64()
            } else if len >= 2 {
                elems[0].as_f64()
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Read a FontMatrix from a font dictionary, returning a Matrix.
fn read_font_matrix(ctx: &Context, dict_entity: EntityId) -> Matrix {
    let fm_obj = match ctx
        .dicts
        .get(dict_entity, &DictKey::Name(ctx.name_cache.n_font_matrix))
    {
        Some(obj) => obj,
        None => return Matrix::new(0.001, 0.0, 0.0, 0.001, 0.0, 0.0),
    };
    match fm_obj.value {
        PsValue::Array { entity, start, len } if len >= 6 => {
            let elems = ctx.arrays.get(entity, start, len);
            Matrix::new(
                elems[0].as_f64().unwrap_or(0.001),
                elems[1].as_f64().unwrap_or(0.0),
                elems[2].as_f64().unwrap_or(0.0),
                elems[3].as_f64().unwrap_or(0.001),
                elems[4].as_f64().unwrap_or(0.0),
                elems[5].as_f64().unwrap_or(0.0),
            )
        }
        _ => Matrix::new(0.001, 0.0, 0.0, 0.001, 0.0, 0.0),
    }
}

/// Concatenate the sfnts array from a Type 42 / CIDFont dictionary into raw font data.
fn concatenate_sfnts_array(ctx: &Context, dict_entity: EntityId) -> Option<Vec<u8>> {
    let sfnts_name = ctx.names.find(b"sfnts")?;
    let sfnts_obj = ctx.dicts.get(dict_entity, &DictKey::Name(sfnts_name))?;
    let (arr_entity, arr_start, arr_len) = match sfnts_obj.value {
        PsValue::Array { entity, start, len } => (entity, start, len),
        _ => return None,
    };

    let mut strings: Vec<&[u8]> = Vec::new();
    for i in 0..arr_len {
        let elem = ctx.arrays.get_element(arr_entity, arr_start + i);
        if let PsValue::String { entity, start, len } = elem.value {
            strings.push(ctx.strings.get(entity, start, len));
        }
    }

    if strings.is_empty() {
        return None;
    }
    Some(truetype::concatenate_sfnts(&strings))
}

/// Render show for Type 0 (composite) and Type 42 (TrueType) fonts.
///
/// For Type 0 fonts with CIDFont Type 42 descendants, parses TrueType glyf
/// data from GlyphDirectory, converts quadratic B-splines to cubic Bezier
/// paths, and renders through the existing fill pipeline.
#[allow(clippy::too_many_arguments)]
fn render_show_composite(
    ctx: &mut Context,
    font_entity: EntityId,
    bytes: &[u8],
    extra_ax: f64,
    extra_ay: f64,
    width_char: i32,
    cx: f64,
    cy: f64,
) -> Result<(), PsError> {
    if bytes.is_empty() {
        return Ok(());
    }

    let font_type = ctx
        .dicts
        .get(font_entity, &DictKey::Name(ctx.name_cache.n_font_type))
        .and_then(|obj| obj.as_i32())
        .unwrap_or(1);

    let type0_fm = read_font_matrix(ctx, font_entity);

    // Inverse-transform device-space current_point to user space
    let (dev_cpx, dev_cpy) = ctx.gstate.current_point.ok_or(PsError::NoCurrentPoint)?;
    let ictm = ctx.gstate.ctm.invert().ok_or(PsError::UndefinedResult)?;
    let (mut cur_x, mut cur_y) = ictm.transform_point(dev_cpx, dev_cpy);
    let ctm = ctx.gstate.ctm;

    if font_type == 0 {
        // --- Type 0 composite font with CIDFont descendant ---

        // Get CIDFont from FDepVector[0]
        let fdep_name = ctx.names.intern(b"FDepVector");
        let fdep_obj = ctx
            .dicts
            .get(font_entity, &DictKey::Name(fdep_name))
            .ok_or(PsError::InvalidFont)?;
        let (fdep_entity, fdep_start, _) = match fdep_obj.value {
            PsValue::Array { entity, start, len } => (entity, start, len),
            _ => return Err(PsError::InvalidFont),
        };
        let cidfont_obj = ctx.arrays.get_element(fdep_entity, fdep_start);
        let cidfont_entity = match cidfont_obj.value {
            PsValue::Dict(e) => e,
            _ => return Err(PsError::InvalidFont),
        };

        let cidfont_fm = read_font_matrix(ctx, cidfont_entity);

        // Collect CIDs to render — use CMap decoding for proper CID mapping
        let pending_cid = ctx.cshow_pending_cid.take();
        let cid_pairs: Vec<(i32, usize)> = if let Some(cid) = pending_cid {
            vec![(cid, 0)]
        } else {
            decode_cmap_bytes(ctx, font_entity, bytes)
        };
        let cids: Vec<i32> = cid_pairs.iter().map(|&(cid, _)| cid).collect();

        // Detect CIDFont type: sfnts → TrueType, CharStrings → CFF
        let has_sfnts = ctx
            .names
            .find(b"sfnts")
            .and_then(|id| ctx.dicts.get(cidfont_entity, &DictKey::Name(id)))
            .is_some();

        if has_sfnts {
            // --- CIDFont Type 2 (TrueType) path ---
            render_composite_truetype_cids(
                ctx,
                cidfont_entity,
                &type0_fm,
                &cidfont_fm,
                &cids,
                &mut cur_x,
                &mut cur_y,
                extra_ax,
                extra_ay,
                width_char,
                cx,
                cy,
                &ctm,
            )?;
        } else {
            // --- CIDFont Type 0 (CFF) path ---
            render_composite_cff_cids(
                ctx,
                cidfont_entity,
                &type0_fm,
                &cidfont_fm,
                &cids,
                &mut cur_x,
                &mut cur_y,
                extra_ax,
                extra_ay,
                width_char,
                cx,
                cy,
                &ctm,
            )?;
        }
    } else {
        // Type 42 simple TrueType font (non-composite)
        let font_data = concatenate_sfnts_array(ctx, font_entity);
        let upm = font_data
            .as_ref()
            .map(|fd| truetype::get_units_per_em(fd) as f64)
            .unwrap_or(1000.0);

        let em_scale = Matrix::scale(1.0 / upm, 1.0 / upm);
        let combined_fm = type0_fm.multiply(&em_scale);

        // Get GlyphDirectory dict (PScript5.dll stores per-glyph data here
        // instead of in the sfnts glyf table)
        let gd_name = ctx.names.intern(b"GlyphDirectory");
        let glyph_dir_entity = ctx
            .dicts
            .get(font_entity, &DictKey::Name(gd_name))
            .and_then(|obj| match obj.value {
                PsValue::Dict(e) => Some(e),
                _ => None,
            });

        // Get CharStrings dict for glyph name → GID mapping
        let cs_entity = ctx
            .dicts
            .get(font_entity, &DictKey::Name(ctx.name_cache.n_char_strings))
            .and_then(|obj| match obj.value {
                PsValue::Dict(e) => Some(e),
                _ => None,
            });

        // Get Encoding array
        let enc_entity = ctx
            .dicts
            .get(font_entity, &DictKey::Name(ctx.name_cache.n_encoding))
            .and_then(|obj| match obj.value {
                PsValue::Array { entity, .. } => Some(entity),
                _ => None,
            });

        for &byte in bytes {
            let mut rendered = false;

            // Look up glyph name from Encoding, then GID from CharStrings
            if let (Some(enc_ent), Some(cs_ent)) = (enc_entity, cs_entity) {
                let glyph_name_obj = ctx.arrays.get_element(enc_ent, byte as u32);
                if let PsValue::Name(glyph_name_id) = glyph_name_obj.value {
                    let cs_key = DictKey::Name(glyph_name_id);
                    if let Some(gid_obj) = ctx.dicts.get(cs_ent, &cs_key) {
                        let gid = gid_obj.as_i32().unwrap_or(0) as u16;

                        // Get glyf data: try GlyphDirectory first, then sfnts
                        let glyf_bytes = if let Some(gd_entity) = glyph_dir_entity {
                            ctx.dicts
                                .get(gd_entity, &DictKey::Int(gid as i32))
                                .and_then(|obj| match obj.value {
                                    PsValue::String { entity, start, len } => {
                                        Some(ctx.strings.get(entity, start, len).to_vec())
                                    }
                                    _ => None,
                                })
                        } else {
                            font_data
                                .as_ref()
                                .and_then(|fd| truetype::get_glyf_data(fd, gid))
                        };

                        if let Some(ref glyf_bytes) = glyf_bytes {
                            if glyf_bytes.len() >= 10 {
                                let glyf_path = {
                                    let dicts = &ctx.dicts;
                                    let strings = &ctx.strings;
                                    let gd = glyph_dir_entity;
                                    let fd_ref = font_data.as_deref();
                                    let resolver = |gid: u16| -> Option<Vec<u8>> {
                                        if let Some(gd_entity) = gd {
                                            let key = DictKey::Int(gid as i32);
                                            if let Some(obj) = dicts.get(gd_entity, &key)
                                                && let PsValue::String { entity, start, len } =
                                                    obj.value
                                            {
                                                return Some(
                                                    strings.get(entity, start, len).to_vec(),
                                                );
                                            }
                                        }
                                        fd_ref.and_then(|fd| truetype::get_glyf_data(fd, gid))
                                    };
                                    truetype::parse_glyf_to_path(glyf_bytes, &resolver)
                                };

                                if !glyf_path.is_empty() {
                                    let user_path =
                                        transform_path(&glyf_path, &combined_fm, cur_x, cur_y);
                                    let device_path = ctm_transform_path(&user_path, &ctm);

                                    let params = FillParams {
                                        color: ctx.gstate.color.clone(),
                                        ctm: Matrix::identity(),
                                        fill_rule: FillRule::NonZeroWinding,
                                    };
                                    ctx.display_list.push(DisplayElement::Fill {
                                        path: device_path,
                                        params,
                                    });
                                }
                            }

                            rendered = true;

                            // Use actual advance width from hmtx
                            let advance = font_data
                                .as_ref()
                                .and_then(|fd| truetype::get_advance_width(fd, gid))
                                .unwrap_or(500);
                            let (wx, wy) = combined_fm.transform_delta(advance as f64, 0.0);
                            cur_x += wx + extra_ax;
                            cur_y += wy + extra_ay;
                        }
                    }
                }
            }

            if !rendered {
                // Fallback: advance by default width
                let (wx, _) = combined_fm.transform_delta(500.0, 0.0);
                cur_x += wx + extra_ax;
                cur_y += extra_ay;
            }

            if byte as i32 == width_char {
                cur_x += cx;
                cur_y += cy;
            }
        }
    }

    let (dev_x, dev_y) = ctm.transform_point(cur_x, cur_y);
    ctx.gstate.current_point = Some((dev_x, dev_y));
    Ok(())
}

/// Render CIDs using TrueType (sfnts) data from a CIDFont descriptor.
#[allow(clippy::too_many_arguments)]
fn render_composite_truetype_cids(
    ctx: &mut Context,
    cidfont_entity: EntityId,
    type0_fm: &Matrix,
    cidfont_fm: &Matrix,
    cids: &[i32],
    cur_x: &mut f64,
    cur_y: &mut f64,
    extra_ax: f64,
    extra_ay: f64,
    width_char: i32,
    cx: f64,
    cy: f64,
    ctm: &Matrix,
) -> Result<(), PsError> {
    // Get GlyphDirectory dict (optional)
    let gd_name = ctx.names.intern(b"GlyphDirectory");
    let glyph_dir_entity = ctx
        .dicts
        .get(cidfont_entity, &DictKey::Name(gd_name))
        .and_then(|obj| match obj.value {
            PsValue::Dict(e) => Some(e),
            _ => None,
        });

    let font_data = concatenate_sfnts_array(ctx, cidfont_entity);
    let upm = font_data
        .as_ref()
        .map(|fd| truetype::get_units_per_em(fd) as f64)
        .unwrap_or(1000.0);

    let em_scale = Matrix::scale(1.0 / upm, 1.0 / upm);
    let combined_fm = type0_fm.multiply(cidfont_fm).multiply(&em_scale);

    for &cid in cids {
        let glyf_bytes = if let Some(gd_entity) = glyph_dir_entity {
            ctx.dicts
                .get(gd_entity, &DictKey::Int(cid))
                .and_then(|obj| match obj.value {
                    PsValue::String { entity, start, len } => {
                        Some(ctx.strings.get(entity, start, len).to_vec())
                    }
                    _ => None,
                })
        } else {
            font_data
                .as_ref()
                .and_then(|fd| truetype::get_glyf_data(fd, cid as u16))
        };

        if let Some(ref glyf_bytes) = glyf_bytes
            && glyf_bytes.len() >= 10
        {
            let glyf_path = {
                let dicts = &ctx.dicts;
                let strings = &ctx.strings;
                let gd = glyph_dir_entity;
                let fd_ref = font_data.as_deref();
                let resolver = |gid: u16| -> Option<Vec<u8>> {
                    if let Some(gd_entity) = gd {
                        let key = DictKey::Int(gid as i32);
                        if let Some(obj) = dicts.get(gd_entity, &key)
                            && let PsValue::String { entity, start, len } = obj.value
                        {
                            return Some(strings.get(entity, start, len).to_vec());
                        }
                    }
                    fd_ref.and_then(|fd| truetype::get_glyf_data(fd, gid))
                };
                truetype::parse_glyf_to_path(glyf_bytes, &resolver)
            };

            if !glyf_path.is_empty() {
                let user_path = transform_path(&glyf_path, &combined_fm, *cur_x, *cur_y);
                let device_path = ctm_transform_path(&user_path, ctm);

                let params = FillParams {
                    color: ctx.gstate.color.clone(),
                    ctm: Matrix::identity(),
                    fill_rule: FillRule::NonZeroWinding,
                };
                ctx.display_list.push(DisplayElement::Fill {
                    path: device_path,
                    params,
                });
            }
        }

        let advance = font_data
            .as_ref()
            .and_then(|fd| truetype::get_advance_width(fd, cid as u16))
            .unwrap_or(500);
        let (wx, wy) = combined_fm.transform_delta(advance as f64, 0.0);
        *cur_x += wx + extra_ax;
        *cur_y += wy + extra_ay;

        if cid == width_char {
            *cur_x += cx;
            *cur_y += cy;
        }
    }
    Ok(())
}

/// Extract Type 2 charstring rendering info from a CIDFont's FD array for a given CID.
struct CidFdInfo {
    default_width_x: f64,
    nominal_width_x: f64,
    local_subrs: Vec<Vec<u8>>,
}

/// Get FD info for a CID from the CIDFont's _cff_fd_array / _cff_fd_select.
fn get_cid_fd_info(ctx: &Context, cidfont_entity: EntityId, cid: i32) -> CidFdInfo {
    // Try _cff_fd_select → _cff_fd_array path
    let fd_select_name = ctx.names.find(b"_cff_fd_select");
    let fd_array_name = ctx.names.find(b"_cff_fd_array");

    if let (Some(fds_name), Some(fda_name)) = (fd_select_name, fd_array_name)
        && let Some(fds_obj) = ctx.dicts.get(cidfont_entity, &DictKey::Name(fds_name))
        && let PsValue::Array {
            entity: fds_e,
            start: fds_s,
            len: fds_l,
        } = fds_obj.value
    {
        // Get FD index for this CID
        let fd_idx = if (cid as u32) < fds_l {
            ctx.arrays
                .get_element(fds_e, fds_s + cid as u32)
                .as_i32()
                .unwrap_or(0) as u32
        } else {
            0
        };

        if let Some(fda_obj) = ctx.dicts.get(cidfont_entity, &DictKey::Name(fda_name))
            && let PsValue::Array {
                entity: fda_e,
                start: fda_s,
                len: fda_l,
            } = fda_obj.value
            && fd_idx < fda_l
        {
            let fd_dict_obj = ctx.arrays.get_element(fda_e, fda_s + fd_idx);
            if let PsValue::Dict(fd_dict) = fd_dict_obj.value {
                return extract_fd_info(ctx, fd_dict);
            }
        }
    }

    // Fallback: use single Private dict from CIDFont
    if let Some(priv_obj) = ctx
        .dicts
        .get(cidfont_entity, &DictKey::Name(ctx.name_cache.n_private))
        && let PsValue::Dict(priv_entity) = priv_obj.value
    {
        return extract_private_info(ctx, priv_entity);
    }

    CidFdInfo {
        default_width_x: 0.0,
        nominal_width_x: 0.0,
        local_subrs: Vec::new(),
    }
}

/// Extract defaultWidthX, nominalWidthX, and local Subrs from an FD dict.
fn extract_fd_info(ctx: &Context, fd_dict: EntityId) -> CidFdInfo {
    if let Some(priv_obj) = ctx
        .dicts
        .get(fd_dict, &DictKey::Name(ctx.name_cache.n_private))
        && let PsValue::Dict(priv_entity) = priv_obj.value
    {
        return extract_private_info(ctx, priv_entity);
    }
    CidFdInfo {
        default_width_x: 0.0,
        nominal_width_x: 0.0,
        local_subrs: Vec::new(),
    }
}

/// Extract defaultWidthX, nominalWidthX, and local Subrs from a Private dict.
fn extract_private_info(ctx: &Context, priv_entity: EntityId) -> CidFdInfo {
    let default_width_x = ctx
        .names
        .find(b"defaultWidthX")
        .and_then(|id| ctx.dicts.get(priv_entity, &DictKey::Name(id)))
        .and_then(|obj| obj.as_f64())
        .unwrap_or(0.0);

    let nominal_width_x = ctx
        .names
        .find(b"nominalWidthX")
        .and_then(|id| ctx.dicts.get(priv_entity, &DictKey::Name(id)))
        .and_then(|obj| obj.as_f64())
        .unwrap_or(0.0);

    let local_subrs = ctx
        .dicts
        .get(priv_entity, &DictKey::Name(ctx.name_cache.n_subrs))
        .and_then(|obj| match obj.value {
            PsValue::Array { entity, start, len } => {
                let elems = ctx.arrays.get(entity, start, len);
                Some(
                    elems
                        .iter()
                        .map(|o| match o.value {
                            PsValue::String { entity, start, len } => {
                                ctx.strings.get(entity, start, len).to_vec()
                            }
                            _ => Vec::new(),
                        })
                        .collect(),
                )
            }
            _ => None,
        })
        .unwrap_or_default();

    CidFdInfo {
        default_width_x,
        nominal_width_x,
        local_subrs,
    }
}

/// Extract global subrs array from a font dict.
fn get_global_subrs(ctx: &Context, font_entity: EntityId) -> Vec<Vec<u8>> {
    ctx.names
        .find(b"_cff_global_subrs")
        .and_then(|id| ctx.dicts.get(font_entity, &DictKey::Name(id)))
        .and_then(|obj| match obj.value {
            PsValue::Array { entity, start, len } => {
                let elems = ctx.arrays.get(entity, start, len);
                Some(
                    elems
                        .iter()
                        .map(|o| match o.value {
                            PsValue::String { entity, start, len } => {
                                ctx.strings.get(entity, start, len).to_vec()
                            }
                            _ => Vec::new(),
                        })
                        .collect(),
                )
            }
            _ => None,
        })
        .unwrap_or_default()
}

/// Render CIDs using CFF (Type 2 charstring) data from a CIDFont descriptor.
#[allow(clippy::too_many_arguments)]
fn render_composite_cff_cids(
    ctx: &mut Context,
    cidfont_entity: EntityId,
    type0_fm: &Matrix,
    cidfont_fm: &Matrix,
    cids: &[i32],
    cur_x: &mut f64,
    cur_y: &mut f64,
    extra_ax: f64,
    extra_ay: f64,
    width_char: i32,
    cx: f64,
    cy: f64,
    ctm: &Matrix,
) -> Result<(), PsError> {
    // Get CharStrings dict (int-keyed by CID)
    let cs_entity = ctx
        .dicts
        .get(
            cidfont_entity,
            &DictKey::Name(ctx.name_cache.n_char_strings),
        )
        .and_then(|obj| match obj.value {
            PsValue::Dict(e) => Some(e),
            _ => None,
        })
        .ok_or(PsError::InvalidFont)?;

    // CFF FontMatrix already handles scaling — no em_scale needed
    let combined_fm = type0_fm.multiply(cidfont_fm);

    // Get global subrs
    let global_subrs = get_global_subrs(ctx, cidfont_entity);

    // Get DW (default width) from CIDFont
    let dw = ctx
        .names
        .find(b"DW")
        .and_then(|id| ctx.dicts.get(cidfont_entity, &DictKey::Name(id)))
        .and_then(|obj| obj.as_i32())
        .unwrap_or(1000);

    for &cid in cids {
        // Look up charstring by int CID
        let cs_obj = match ctx.dicts.get(cs_entity, &DictKey::Int(cid)) {
            Some(obj) => obj,
            None => {
                // No charstring for this CID — advance by default width
                let (wx, wy) = combined_fm.transform_delta(dw as f64, 0.0);
                *cur_x += wx + extra_ax;
                *cur_y += wy + extra_ay;
                if cid == width_char {
                    *cur_x += cx;
                    *cur_y += cy;
                }
                continue;
            }
        };

        let cs_bytes = match cs_obj.value {
            PsValue::String { entity, start, len } => ctx.strings.get(entity, start, len).to_vec(),
            _ => continue,
        };

        // Get FD info (defaultWidthX, nominalWidthX, local subrs)
        let fd_info = get_cid_fd_info(ctx, cidfont_entity, cid);

        // Execute Type 2 charstring
        let result = type2_charstring::execute_type2_charstring(
            &cs_bytes,
            &fd_info.local_subrs,
            &global_subrs,
            fd_info.default_width_x,
            fd_info.nominal_width_x,
            false,
        )
        .map_err(|_| PsError::InvalidFont)?;

        // Fill glyph path
        if !result.path.is_empty() {
            let user_path = transform_path(&result.path, &combined_fm, *cur_x, *cur_y);
            let device_path = ctm_transform_path(&user_path, ctm);

            let params = FillParams {
                color: ctx.gstate.color.clone(),
                ctm: Matrix::identity(),
                fill_rule: FillRule::NonZeroWinding,
            };
            ctx.display_list.push(DisplayElement::Fill {
                path: device_path,
                params,
            });
        }

        // Advance by charstring width through combined FontMatrix
        let (wx, wy) = combined_fm.transform_delta(result.width_x, result.width_y);
        *cur_x += wx + extra_ax;
        *cur_y += wy + extra_ay;

        if cid == width_char {
            *cur_x += cx;
            *cur_y += cy;
        }
    }
    Ok(())
}

/// Initiate Type 3 font rendering via the continuation pattern.
///
/// Sets up the first character's BuildChar call and pushes a continuation
/// that processes subsequent characters.
fn render_show_type3(
    ctx: &mut Context,
    bytes: &[u8],
    extra_ax: f64,
    extra_ay: f64,
    width_char: i32,
    cx: f64,
    cy: f64,
) -> Result<(), PsError> {
    let font_obj = ctx.gstate.current_font.ok_or(PsError::InvalidFont)?;
    let font_entity = match font_obj.value {
        PsValue::Dict(e) => e,
        _ => return Err(PsError::InvalidFont),
    };

    // Get BuildChar procedure
    let build_char = ctx
        .dicts
        .get(font_entity, &DictKey::Name(ctx.name_cache.n_build_char))
        .ok_or(PsError::InvalidFont)?;
    if !build_char.flags.is_executable() || !build_char.is_array_type() {
        return Err(PsError::InvalidFont);
    }

    // Get FontMatrix
    let fm_obj = ctx
        .dicts
        .get(font_entity, &DictKey::Name(ctx.name_cache.n_font_matrix))
        .ok_or(PsError::InvalidFont)?;
    let font_matrix = match fm_obj.value {
        PsValue::Array { entity, start, len } => {
            let elems = ctx.arrays.get(entity, start, len);
            Matrix::new(
                elems[0].as_f64().ok_or(PsError::InvalidFont)?,
                elems[1].as_f64().ok_or(PsError::InvalidFont)?,
                elems[2].as_f64().ok_or(PsError::InvalidFont)?,
                elems[3].as_f64().ok_or(PsError::InvalidFont)?,
                elems[4].as_f64().ok_or(PsError::InvalidFont)?,
                elems[5].as_f64().ok_or(PsError::InvalidFont)?,
            )
        }
        _ => return Err(PsError::InvalidFont),
    };

    // Inverse-transform device-space current_point to user space
    let (dev_cpx, dev_cpy) = ctx.gstate.current_point.ok_or(PsError::NoCurrentPoint)?;
    let ictm = ctx.gstate.ctm.invert().ok_or(PsError::UndefinedResult)?;
    let (mut cur_x, mut cur_y) = ictm.transform_point(dev_cpx, dev_cpy);

    // Render each character synchronously
    for &byte in bytes {
        ctx.char_width = None;

        // gsave, translate to current position, concat FontMatrix
        crate::graphics_state_ops::op_gsave(ctx)?;

        ctx.o_stack.push(PsObject::real(cur_x))?;
        ctx.o_stack.push(PsObject::real(cur_y))?;
        crate::matrix_ops::op_translate(ctx)?;

        // Re-read FontMatrix from dict (in case BuildChar modified it)
        let fm_obj = ctx
            .dicts
            .get(font_entity, &DictKey::Name(ctx.name_cache.n_font_matrix))
            .ok_or(PsError::InvalidFont)?;
        ctx.o_stack.push(fm_obj)?;
        crate::matrix_ops::op_concat(ctx)?;

        // Push font dict and char code for BuildChar
        ctx.o_stack.push(font_obj)?;
        ctx.o_stack.push(PsObject::int(byte as i32))?;

        // Execute BuildChar synchronously
        ctx.exec_sync(build_char)?;

        // grestore to undo the gsave+translate+concat
        crate::graphics_state_ops::op_grestore(ctx)?;

        // Get char width set by setcachedevice/setcharwidth during BuildChar
        let char_width = ctx.char_width.take().unwrap_or((0.0, 0.0));
        let (wx, wy) = font_matrix.transform_delta(char_width.0, char_width.1);

        // Advance current position in user space
        cur_x += wx + extra_ax;
        cur_y += wy + extra_ay;

        if byte as i32 == width_char {
            cur_x += cx;
            cur_y += cy;
        }
    }

    // Update device-space current_point
    let ctm = ctx.gstate.ctm;
    let (dev_x, dev_y) = ctm.transform_point(cur_x, cur_y);
    ctx.gstate.current_point = Some((dev_x, dev_y));

    Ok(())
}

/// Measure total string width without rendering.
fn measure_string_width(ctx: &mut Context, bytes: &[u8]) -> Result<(f64, f64), PsError> {
    let font_obj = ctx.gstate.current_font.ok_or(PsError::InvalidFont)?;
    let font_entity_id = match font_obj.value {
        PsValue::Dict(e) => e,
        _ => return Err(PsError::InvalidFont),
    };
    let font_type = ctx
        .dicts
        .get(font_entity_id, &DictKey::Name(ctx.name_cache.n_font_type))
        .and_then(|obj| obj.as_i32())
        .unwrap_or(1);
    if font_type == 2 {
        return measure_string_width_type2(ctx, font_entity_id, bytes);
    }
    if font_type == 0 || font_type == 42 {
        return measure_string_width_composite(ctx, font_entity_id, bytes);
    }

    let info = get_font_info(ctx)?;
    let subrs = get_subrs(ctx, &info);

    let mut total_wx = 0.0;
    let mut total_wy = 0.0;

    for &byte in bytes {
        let glyph_name_obj = ctx.arrays.get_element(info.encoding_entity, byte as u32);
        let glyph_name_id = match glyph_name_obj.value {
            PsValue::Name(id) => id,
            _ => continue,
        };

        let cs_key = DictKey::Name(glyph_name_id);
        let cs_obj = match ctx.dicts.get(info.charstrings_entity, &cs_key) {
            Some(obj) => obj,
            None => continue,
        };

        let (cs_entity, cs_start, cs_len) = match cs_obj.value {
            PsValue::String { entity, start, len } => (entity, start, len),
            _ => continue,
        };

        let cs_bytes = ctx.strings.get(cs_entity, cs_start, cs_len).to_vec();

        let result = charstring::execute_charstring(&cs_bytes, &subrs, info.len_iv, true)
            .map_err(|_| PsError::InvalidFont)?;

        let (wx, wy) = if let Some(metrics_wx) = get_metrics_width(ctx, &info, glyph_name_id, byte)
        {
            info.font_matrix.transform_delta(metrics_wx, 0.0)
        } else {
            info.font_matrix
                .transform_delta(result.width_x, result.width_y)
        };
        total_wx += wx;
        total_wy += wy;
    }

    Ok((total_wx, total_wy))
}

/// Render glyph outlines to the current path (for charpath).
///
/// Glyph paths are built in user space via FontMatrix, then transformed through CTM
/// to device space before appending to `ctx.gstate.path`.
fn render_charpath(ctx: &mut Context, bytes: &[u8]) -> Result<(), PsError> {
    let font_obj = ctx.gstate.current_font.ok_or(PsError::InvalidFont)?;
    let font_entity_id = match font_obj.value {
        PsValue::Dict(e) => e,
        _ => return Err(PsError::InvalidFont),
    };
    let font_type = ctx
        .dicts
        .get(font_entity_id, &DictKey::Name(ctx.name_cache.n_font_type))
        .and_then(|obj| obj.as_i32())
        .unwrap_or(1);
    if font_type == 2 {
        return render_charpath_type2(ctx, font_entity_id, bytes);
    }
    if font_type == 0 || font_type == 42 {
        return render_charpath_composite(ctx, font_entity_id, bytes);
    }

    let info = get_font_info(ctx)?;
    let subrs = get_subrs(ctx, &info);

    // Inverse-transform device-space current_point to user space
    let (dev_cpx, dev_cpy) = ctx.gstate.current_point.ok_or(PsError::NoCurrentPoint)?;
    let ictm = ctx.gstate.ctm.invert().ok_or(PsError::UndefinedResult)?;
    let (mut cur_x, mut cur_y) = ictm.transform_point(dev_cpx, dev_cpy);

    let ctm = ctx.gstate.ctm;

    for &byte in bytes {
        let glyph_name_obj = ctx.arrays.get_element(info.encoding_entity, byte as u32);
        let glyph_name_id = match glyph_name_obj.value {
            PsValue::Name(id) => id,
            _ => continue,
        };

        let cs_key = DictKey::Name(glyph_name_id);
        let cs_obj = match ctx.dicts.get(info.charstrings_entity, &cs_key) {
            Some(obj) => obj,
            None => continue,
        };

        let (cs_entity, cs_start, cs_len) = match cs_obj.value {
            PsValue::String { entity, start, len } => (entity, start, len),
            _ => continue,
        };

        let cs_bytes = ctx.strings.get(cs_entity, cs_start, cs_len).to_vec();

        let result = charstring::execute_charstring(&cs_bytes, &subrs, info.len_iv, false)
            .map_err(|_| PsError::InvalidFont)?;

        // Append device-space glyph path to current path
        if !result.path.is_empty() {
            let user_path = transform_path(&result.path, &info.font_matrix, cur_x, cur_y);
            let device_path = ctm_transform_path(&user_path, &ctm);
            // Per PLRM: consecutive movetos replace the previous one.
            // If the glyph path starts with MoveTo and the current path's last
            // segment is also a bare MoveTo, replace it instead of appending.
            let mut segs_iter = device_path.segments.into_iter();
            if let Some(first_seg) = segs_iter.next() {
                if let PathSegment::MoveTo(x, y) = first_seg {
                    crate::path_ops::path_moveto(&mut ctx.gstate.path, x, y);
                } else {
                    ctx.gstate.path.segments.push(first_seg);
                }
                ctx.gstate.path.segments.extend(segs_iter);
            }
        }

        let (wx, wy) = info
            .font_matrix
            .transform_delta(result.width_x, result.width_y);
        cur_x += wx;
        cur_y += wy;
    }

    // Transform final user-space position to device-space current_point
    let (dev_x, dev_y) = ctm.transform_point(cur_x, cur_y);
    ctx.gstate.current_point = Some((dev_x, dev_y));
    Ok(())
}

/// Measure string width for Type 0 (composite) and Type 42 (TrueType) fonts.
fn measure_string_width_composite(
    ctx: &mut Context,
    font_entity_id: EntityId,
    bytes: &[u8],
) -> Result<(f64, f64), PsError> {
    let font_type = ctx
        .dicts
        .get(font_entity_id, &DictKey::Name(ctx.name_cache.n_font_type))
        .and_then(|obj| obj.as_i32())
        .unwrap_or(1);

    let type0_fm = read_font_matrix(ctx, font_entity_id);

    if font_type == 0 {
        // Type 0 composite: get CIDFont from FDepVector
        let fdep_name = ctx.names.intern(b"FDepVector");
        let fdep_obj = ctx
            .dicts
            .get(font_entity_id, &DictKey::Name(fdep_name))
            .ok_or(PsError::InvalidFont)?;
        let (fdep_entity, fdep_start, _) = match fdep_obj.value {
            PsValue::Array { entity, start, len } => (entity, start, len),
            _ => return Err(PsError::InvalidFont),
        };
        let cidfont_obj = ctx.arrays.get_element(fdep_entity, fdep_start);
        let cidfont_entity = match cidfont_obj.value {
            PsValue::Dict(e) => e,
            _ => return Err(PsError::InvalidFont),
        };

        let cidfont_fm = read_font_matrix(ctx, cidfont_entity);
        let cids = decode_cmap_bytes(ctx, font_entity_id, bytes);

        let has_sfnts = ctx
            .names
            .find(b"sfnts")
            .and_then(|id| ctx.dicts.get(cidfont_entity, &DictKey::Name(id)))
            .is_some();

        let mut total_wx = 0.0;
        let mut total_wy = 0.0;

        if has_sfnts {
            // TrueType CIDFont
            let font_data = concatenate_sfnts_array(ctx, cidfont_entity);
            let upm = font_data
                .as_ref()
                .map(|fd| truetype::get_units_per_em(fd) as f64)
                .unwrap_or(1000.0);
            let em_scale = Matrix::scale(1.0 / upm, 1.0 / upm);
            let combined_fm = type0_fm.multiply(&cidfont_fm).multiply(&em_scale);

            for (cid, _) in &cids {
                let advance = font_data
                    .as_ref()
                    .and_then(|fd| truetype::get_advance_width(fd, *cid as u16))
                    .unwrap_or(500);
                let (wx, wy) = combined_fm.transform_delta(advance as f64, 0.0);
                total_wx += wx;
                total_wy += wy;
            }
        } else {
            // CFF CIDFont
            let cs_entity = ctx
                .dicts
                .get(
                    cidfont_entity,
                    &DictKey::Name(ctx.name_cache.n_char_strings),
                )
                .and_then(|obj| match obj.value {
                    PsValue::Dict(e) => Some(e),
                    _ => None,
                });

            let combined_fm = type0_fm.multiply(&cidfont_fm);
            let global_subrs = get_global_subrs(ctx, cidfont_entity);
            let dw = ctx
                .names
                .find(b"DW")
                .and_then(|id| ctx.dicts.get(cidfont_entity, &DictKey::Name(id)))
                .and_then(|obj| obj.as_i32())
                .unwrap_or(1000);

            for (cid, _) in &cids {
                let width = if let Some(cs_e) = cs_entity {
                    if let Some(cs_obj) = ctx.dicts.get(cs_e, &DictKey::Int(*cid)) {
                        if let PsValue::String { entity, start, len } = cs_obj.value {
                            let cs_bytes = ctx.strings.get(entity, start, len).to_vec();
                            let fd_info = get_cid_fd_info(ctx, cidfont_entity, *cid);
                            if let Ok(result) = type2_charstring::execute_type2_charstring(
                                &cs_bytes,
                                &fd_info.local_subrs,
                                &global_subrs,
                                fd_info.default_width_x,
                                fd_info.nominal_width_x,
                                true,
                            ) {
                                result.width_x
                            } else {
                                dw as f64
                            }
                        } else {
                            dw as f64
                        }
                    } else {
                        dw as f64
                    }
                } else {
                    dw as f64
                };
                let (wx, wy) = combined_fm.transform_delta(width, 0.0);
                total_wx += wx;
                total_wy += wy;
            }
        }
        Ok((total_wx, total_wy))
    } else {
        // Type 42: simple TrueType font
        let font_data = concatenate_sfnts_array(ctx, font_entity_id);
        let upm = font_data
            .as_ref()
            .map(|fd| truetype::get_units_per_em(fd) as f64)
            .unwrap_or(1000.0);
        let em_scale = Matrix::scale(1.0 / upm, 1.0 / upm);
        let combined_fm = type0_fm.multiply(&em_scale);

        let cs_entity = ctx
            .dicts
            .get(
                font_entity_id,
                &DictKey::Name(ctx.name_cache.n_char_strings),
            )
            .and_then(|obj| match obj.value {
                PsValue::Dict(e) => Some(e),
                _ => None,
            });
        let enc_entity = ctx
            .dicts
            .get(font_entity_id, &DictKey::Name(ctx.name_cache.n_encoding))
            .and_then(|obj| match obj.value {
                PsValue::Array { entity, .. } => Some(entity),
                _ => None,
            });

        let mut total_wx = 0.0;
        let mut total_wy = 0.0;

        for &byte in bytes {
            if let (Some(enc_ent), Some(cs_ent), Some(fd)) = (enc_entity, cs_entity, &font_data) {
                let glyph_name_obj = ctx.arrays.get_element(enc_ent, byte as u32);
                if let PsValue::Name(glyph_name_id) = glyph_name_obj.value
                    && let Some(gid_obj) = ctx.dicts.get(cs_ent, &DictKey::Name(glyph_name_id))
                {
                    let gid = gid_obj.as_i32().unwrap_or(0) as u16;
                    let advance = truetype::get_advance_width(fd, gid).unwrap_or(500);
                    let (wx, wy) = combined_fm.transform_delta(advance as f64, 0.0);
                    total_wx += wx;
                    total_wy += wy;
                    continue;
                }
            }
            // Fallback
            let (wx, _) = combined_fm.transform_delta(500.0, 0.0);
            total_wx += wx;
        }
        Ok((total_wx, total_wy))
    }
}

/// Render charpath for Type 0 (composite) and Type 42 (TrueType) fonts.
fn render_charpath_composite(
    ctx: &mut Context,
    font_entity_id: EntityId,
    bytes: &[u8],
) -> Result<(), PsError> {
    let font_type = ctx
        .dicts
        .get(font_entity_id, &DictKey::Name(ctx.name_cache.n_font_type))
        .and_then(|obj| obj.as_i32())
        .unwrap_or(1);

    let type0_fm = read_font_matrix(ctx, font_entity_id);

    let (dev_cpx, dev_cpy) = ctx.gstate.current_point.ok_or(PsError::NoCurrentPoint)?;
    let ictm = ctx.gstate.ctm.invert().ok_or(PsError::UndefinedResult)?;
    let (mut cur_x, mut cur_y) = ictm.transform_point(dev_cpx, dev_cpy);
    let ctm = ctx.gstate.ctm;

    if font_type == 0 {
        // Type 0 composite
        let fdep_name = ctx.names.intern(b"FDepVector");
        let fdep_obj = ctx
            .dicts
            .get(font_entity_id, &DictKey::Name(fdep_name))
            .ok_or(PsError::InvalidFont)?;
        let (fdep_entity, fdep_start, _) = match fdep_obj.value {
            PsValue::Array { entity, start, len } => (entity, start, len),
            _ => return Err(PsError::InvalidFont),
        };
        let cidfont_obj = ctx.arrays.get_element(fdep_entity, fdep_start);
        let cidfont_entity = match cidfont_obj.value {
            PsValue::Dict(e) => e,
            _ => return Err(PsError::InvalidFont),
        };

        let cidfont_fm = read_font_matrix(ctx, cidfont_entity);
        let cids = decode_cmap_bytes(ctx, font_entity_id, bytes);

        let has_sfnts = ctx
            .names
            .find(b"sfnts")
            .and_then(|id| ctx.dicts.get(cidfont_entity, &DictKey::Name(id)))
            .is_some();

        if has_sfnts {
            // TrueType CIDFont charpath
            let font_data = concatenate_sfnts_array(ctx, cidfont_entity);
            let upm = font_data
                .as_ref()
                .map(|fd| truetype::get_units_per_em(fd) as f64)
                .unwrap_or(1000.0);
            let em_scale = Matrix::scale(1.0 / upm, 1.0 / upm);
            let combined_fm = type0_fm.multiply(&cidfont_fm).multiply(&em_scale);
            let gd_name = ctx.names.intern(b"GlyphDirectory");
            let glyph_dir_entity = ctx
                .dicts
                .get(cidfont_entity, &DictKey::Name(gd_name))
                .and_then(|obj| match obj.value {
                    PsValue::Dict(e) => Some(e),
                    _ => None,
                });

            for (cid, _) in &cids {
                let glyf_bytes = if let Some(gd_entity) = glyph_dir_entity {
                    ctx.dicts
                        .get(gd_entity, &DictKey::Int(*cid))
                        .and_then(|obj| match obj.value {
                            PsValue::String { entity, start, len } => {
                                Some(ctx.strings.get(entity, start, len).to_vec())
                            }
                            _ => None,
                        })
                } else {
                    font_data
                        .as_ref()
                        .and_then(|fd| truetype::get_glyf_data(fd, *cid as u16))
                };

                if let Some(ref glyf_bytes) = glyf_bytes
                    && glyf_bytes.len() >= 10
                {
                    let glyf_path = {
                        let dicts = &ctx.dicts;
                        let strings = &ctx.strings;
                        let gd = glyph_dir_entity;
                        let fd_ref = font_data.as_deref();
                        let resolver = |gid: u16| -> Option<Vec<u8>> {
                            if let Some(gd_entity) = gd {
                                let key = DictKey::Int(gid as i32);
                                if let Some(obj) = dicts.get(gd_entity, &key)
                                    && let PsValue::String { entity, start, len } = obj.value
                                {
                                    return Some(strings.get(entity, start, len).to_vec());
                                }
                            }
                            fd_ref.and_then(|fd| truetype::get_glyf_data(fd, gid))
                        };
                        truetype::parse_glyf_to_path(glyf_bytes, &resolver)
                    };

                    if !glyf_path.is_empty() {
                        let user_path = transform_path(&glyf_path, &combined_fm, cur_x, cur_y);
                        let device_path = ctm_transform_path(&user_path, &ctm);
                        append_path_to_current(&mut ctx.gstate.path, device_path);
                    }
                }

                let advance = font_data
                    .as_ref()
                    .and_then(|fd| truetype::get_advance_width(fd, *cid as u16))
                    .unwrap_or(500);
                let (wx, wy) = combined_fm.transform_delta(advance as f64, 0.0);
                cur_x += wx;
                cur_y += wy;
            }
        } else {
            // CFF CIDFont charpath
            let cs_entity = ctx
                .dicts
                .get(
                    cidfont_entity,
                    &DictKey::Name(ctx.name_cache.n_char_strings),
                )
                .and_then(|obj| match obj.value {
                    PsValue::Dict(e) => Some(e),
                    _ => None,
                })
                .ok_or(PsError::InvalidFont)?;

            let combined_fm = type0_fm.multiply(&cidfont_fm);
            let global_subrs = get_global_subrs(ctx, cidfont_entity);

            for (cid, _) in &cids {
                if let Some(cs_obj) = ctx.dicts.get(cs_entity, &DictKey::Int(*cid))
                    && let PsValue::String { entity, start, len } = cs_obj.value
                {
                    let cs_bytes = ctx.strings.get(entity, start, len).to_vec();
                    let fd_info = get_cid_fd_info(ctx, cidfont_entity, *cid);

                    if let Ok(result) = type2_charstring::execute_type2_charstring(
                        &cs_bytes,
                        &fd_info.local_subrs,
                        &global_subrs,
                        fd_info.default_width_x,
                        fd_info.nominal_width_x,
                        false,
                    ) {
                        if !result.path.is_empty() {
                            let user_path =
                                transform_path(&result.path, &combined_fm, cur_x, cur_y);
                            let device_path = ctm_transform_path(&user_path, &ctm);
                            append_path_to_current(&mut ctx.gstate.path, device_path);
                        }
                        let (wx, wy) = combined_fm.transform_delta(result.width_x, result.width_y);
                        cur_x += wx;
                        cur_y += wy;
                    }
                }
            }
        }
    } else {
        // Type 42: simple TrueType font charpath
        let font_data = concatenate_sfnts_array(ctx, font_entity_id);
        let upm = font_data
            .as_ref()
            .map(|fd| truetype::get_units_per_em(fd) as f64)
            .unwrap_or(1000.0);
        let em_scale = Matrix::scale(1.0 / upm, 1.0 / upm);
        let combined_fm = type0_fm.multiply(&em_scale);

        let cs_entity = ctx
            .dicts
            .get(
                font_entity_id,
                &DictKey::Name(ctx.name_cache.n_char_strings),
            )
            .and_then(|obj| match obj.value {
                PsValue::Dict(e) => Some(e),
                _ => None,
            });
        let enc_entity = ctx
            .dicts
            .get(font_entity_id, &DictKey::Name(ctx.name_cache.n_encoding))
            .and_then(|obj| match obj.value {
                PsValue::Array { entity, .. } => Some(entity),
                _ => None,
            });

        for &byte in bytes {
            if let (Some(enc_ent), Some(cs_ent), Some(fd)) = (enc_entity, cs_entity, &font_data) {
                let glyph_name_obj = ctx.arrays.get_element(enc_ent, byte as u32);
                if let PsValue::Name(glyph_name_id) = glyph_name_obj.value
                    && let Some(gid_obj) = ctx.dicts.get(cs_ent, &DictKey::Name(glyph_name_id))
                {
                    let gid = gid_obj.as_i32().unwrap_or(0) as u16;
                    if let Some(glyf_bytes) = truetype::get_glyf_data(fd, gid)
                        && glyf_bytes.len() >= 10
                    {
                        let glyf_path = {
                            let fd_ref = Some(fd.as_slice());
                            let resolver = |gid: u16| -> Option<Vec<u8>> {
                                fd_ref.and_then(|fd| truetype::get_glyf_data(fd, gid))
                            };
                            truetype::parse_glyf_to_path(&glyf_bytes, &resolver)
                        };
                        if !glyf_path.is_empty() {
                            let user_path = transform_path(&glyf_path, &combined_fm, cur_x, cur_y);
                            let device_path = ctm_transform_path(&user_path, &ctm);
                            append_path_to_current(&mut ctx.gstate.path, device_path);
                        }
                    }
                    let advance = truetype::get_advance_width(fd, gid).unwrap_or(500);
                    let (wx, wy) = combined_fm.transform_delta(advance as f64, 0.0);
                    cur_x += wx;
                    cur_y += wy;
                    continue;
                }
            }
            // Fallback
            let (wx, _) = combined_fm.transform_delta(500.0, 0.0);
            cur_x += wx;
        }
    }

    let (dev_x, dev_y) = ctm.transform_point(cur_x, cur_y);
    ctx.gstate.current_point = Some((dev_x, dev_y));
    Ok(())
}

/// Append a glyph path to the current path, handling MoveTo properly.
fn append_path_to_current(current_path: &mut PsPath, glyph_path: PsPath) {
    let mut segs_iter = glyph_path.segments.into_iter();
    if let Some(first_seg) = segs_iter.next() {
        if let PathSegment::MoveTo(x, y) = first_seg {
            crate::path_ops::path_moveto(current_path, x, y);
        } else {
            current_path.segments.push(first_seg);
        }
        current_path.segments.extend(segs_iter);
    }
}

/// Displacement mode for xshow/yshow/xyshow.
enum DisplacementMode {
    /// xshow: one value per char (x displacement, y = 0)
    X,
    /// yshow: one value per char (y displacement, x = 0)
    Y,
    /// xyshow: two values per char (x, y displacement pair)
    XY,
}

/// Extract displacement values from an array or numstring.
fn extract_displacements(ctx: &Context, obj: &PsObject) -> Result<Vec<f64>, PsError> {
    match obj.value {
        PsValue::Array { entity, start, len } | PsValue::PackedArray { entity, start, len } => {
            let elems = ctx.arrays.get(entity, start, len);
            let mut values = Vec::with_capacity(len as usize);
            for elem in elems {
                values.push(elem.as_f64().ok_or(PsError::TypeCheck)?);
            }
            Ok(values)
        }
        PsValue::String { entity, start, len } => {
            // Encoded number string: whitespace-separated floats
            let bytes = ctx.strings.get(entity, start, len);
            let s = std::str::from_utf8(bytes).map_err(|_| PsError::TypeCheck)?;
            let values: Result<Vec<f64>, _> =
                s.split_whitespace().map(|w| w.parse::<f64>()).collect();
            values.map_err(|_| PsError::TypeCheck)
        }
        _ => Err(PsError::TypeCheck),
    }
}

/// Render glyphs with explicit per-character displacements (xshow/yshow/xyshow).
///
/// Like `render_show`, but instead of advancing by glyph width, advances by
/// the displacement values from the array. Displacements are in user space.
fn render_show_displaced(
    ctx: &mut Context,
    bytes: &[u8],
    displacements: &[f64],
    mode: DisplacementMode,
) -> Result<(), PsError> {
    // Check FontType for Type 2 dispatch
    let font_obj = ctx.gstate.current_font.ok_or(PsError::InvalidFont)?;
    let font_entity_check = match font_obj.value {
        PsValue::Dict(e) => e,
        _ => return Err(PsError::InvalidFont),
    };
    let font_type = ctx
        .dicts
        .get(
            font_entity_check,
            &DictKey::Name(ctx.name_cache.n_font_type),
        )
        .and_then(|obj| obj.as_i32())
        .unwrap_or(1);

    if font_type == 2 {
        return render_show_displaced_type2(ctx, font_entity_check, bytes, displacements, mode);
    }
    if font_type == 0 || font_type == 42 {
        return render_show_displaced_composite(ctx, font_entity_check, bytes, displacements, mode);
    }
    if font_type == 3 {
        return render_show_displaced_type3(ctx, font_entity_check, bytes, displacements, mode);
    }

    let info = get_font_info(ctx)?;
    let subrs = get_subrs(ctx, &info);

    let (dev_cpx, dev_cpy) = ctx.gstate.current_point.ok_or(PsError::NoCurrentPoint)?;
    let ictm = ctx.gstate.ctm.invert().ok_or(PsError::UndefinedResult)?;
    let (mut cur_x, mut cur_y) = ictm.transform_point(dev_cpx, dev_cpy);

    let ctm = ctx.gstate.ctm;

    for (i, &byte) in bytes.iter().enumerate() {
        // Look up glyph name from encoding
        let glyph_name_obj = ctx.arrays.get_element(info.encoding_entity, byte as u32);
        let glyph_name_id = match glyph_name_obj.value {
            PsValue::Name(id) => id,
            _ => {
                // Still advance by displacement even if glyph not found
                advance_by_displacement(&mut cur_x, &mut cur_y, displacements, i, &mode);
                continue;
            }
        };

        // Render glyph
        {
            let cs_key = DictKey::Name(glyph_name_id);
            if let Some(cs_obj) = ctx.dicts.get(info.charstrings_entity, &cs_key)
                && let PsValue::String {
                    entity: cs_entity,
                    start: cs_start,
                    len: cs_len,
                } = cs_obj.value
            {
                let cs_bytes = ctx.strings.get(cs_entity, cs_start, cs_len).to_vec();
                if let Ok(result) =
                    charstring::execute_charstring(&cs_bytes, &subrs, info.len_iv, false)
                    && !result.path.is_empty()
                {
                    let user_path = transform_path(&result.path, &info.font_matrix, cur_x, cur_y);
                    let device_path = ctm_transform_path(&user_path, &ctm);
                    let params = FillParams {
                        color: ctx.gstate.color.clone(),
                        ctm: Matrix::identity(),
                        fill_rule: FillRule::NonZeroWinding,
                    };
                    ctx.display_list.push(DisplayElement::Fill {
                        path: device_path,
                        params,
                    });
                }
            }
        }

        // Advance by custom displacement (overrides glyph width)
        advance_by_displacement(&mut cur_x, &mut cur_y, displacements, i, &mode);
    }

    let (dev_x, dev_y) = ctm.transform_point(cur_x, cur_y);
    ctx.gstate.current_point = Some((dev_x, dev_y));
    Ok(())
}

/// Like `render_show_displaced`, but for Type 2 (CFF) fonts.
fn render_show_displaced_type2(
    ctx: &mut Context,
    font_entity: EntityId,
    bytes: &[u8],
    displacements: &[f64],
    mode: DisplacementMode,
) -> Result<(), PsError> {
    let info = get_type2_info(ctx, font_entity)?;

    let (dev_cpx, dev_cpy) = ctx.gstate.current_point.ok_or(PsError::NoCurrentPoint)?;
    let ictm = ctx.gstate.ctm.invert().ok_or(PsError::UndefinedResult)?;
    let (mut cur_x, mut cur_y) = ictm.transform_point(dev_cpx, dev_cpy);
    let ctm = ctx.gstate.ctm;

    for (i, &byte) in bytes.iter().enumerate() {
        let glyph_name_obj = ctx.arrays.get_element(info.encoding_entity, byte as u32);
        let glyph_name_id = match glyph_name_obj.value {
            PsValue::Name(id) => id,
            _ => {
                advance_by_displacement(&mut cur_x, &mut cur_y, displacements, i, &mode);
                continue;
            }
        };

        // Render glyph
        {
            let cs_key = DictKey::Name(glyph_name_id);
            if let Some(cs_obj) = ctx.dicts.get(info.charstrings_entity, &cs_key)
                && let PsValue::String {
                    entity: cs_entity,
                    start: cs_start,
                    len: cs_len,
                } = cs_obj.value
            {
                let cs_bytes = ctx.strings.get(cs_entity, cs_start, cs_len).to_vec();
                if let Ok(result) = type2_charstring::execute_type2_charstring(
                    &cs_bytes,
                    &info.local_subrs,
                    &info.global_subrs,
                    info.default_width_x,
                    info.nominal_width_x,
                    false,
                ) && !result.path.is_empty()
                {
                    let user_path = transform_path(&result.path, &info.font_matrix, cur_x, cur_y);
                    let device_path = ctm_transform_path(&user_path, &ctm);
                    let params = FillParams {
                        color: ctx.gstate.color.clone(),
                        ctm: Matrix::identity(),
                        fill_rule: FillRule::NonZeroWinding,
                    };
                    ctx.display_list.push(DisplayElement::Fill {
                        path: device_path,
                        params,
                    });
                }
            }
        }

        advance_by_displacement(&mut cur_x, &mut cur_y, displacements, i, &mode);
    }

    let (dev_x, dev_y) = ctm.transform_point(cur_x, cur_y);
    ctx.gstate.current_point = Some((dev_x, dev_y));
    Ok(())
}

/// Like `render_show_displaced`, but for Type 0 (composite) and Type 42 (TrueType) fonts.
fn render_show_displaced_composite(
    ctx: &mut Context,
    font_entity: EntityId,
    bytes: &[u8],
    displacements: &[f64],
    mode: DisplacementMode,
) -> Result<(), PsError> {
    let font_type = ctx
        .dicts
        .get(font_entity, &DictKey::Name(ctx.name_cache.n_font_type))
        .and_then(|obj| obj.as_i32())
        .unwrap_or(1);

    let type0_fm = read_font_matrix(ctx, font_entity);

    let (dev_cpx, dev_cpy) = ctx.gstate.current_point.ok_or(PsError::NoCurrentPoint)?;
    let ictm = ctx.gstate.ctm.invert().ok_or(PsError::UndefinedResult)?;
    let (mut cur_x, mut cur_y) = ictm.transform_point(dev_cpx, dev_cpy);
    let ctm = ctx.gstate.ctm;

    if font_type == 0 {
        // Type 0 composite: decode CIDs and render each
        let fdep_name = ctx.names.intern(b"FDepVector");
        let fdep_obj = ctx
            .dicts
            .get(font_entity, &DictKey::Name(fdep_name))
            .ok_or(PsError::InvalidFont)?;
        let (fdep_entity, fdep_start, _) = match fdep_obj.value {
            PsValue::Array { entity, start, len } => (entity, start, len),
            _ => return Err(PsError::InvalidFont),
        };
        let cidfont_obj = ctx.arrays.get_element(fdep_entity, fdep_start);
        let cidfont_entity = match cidfont_obj.value {
            PsValue::Dict(e) => e,
            _ => return Err(PsError::InvalidFont),
        };

        let cidfont_fm = read_font_matrix(ctx, cidfont_entity);
        let cids = decode_cmap_bytes(ctx, font_entity, bytes);

        let has_sfnts = ctx
            .names
            .find(b"sfnts")
            .and_then(|id| ctx.dicts.get(cidfont_entity, &DictKey::Name(id)))
            .is_some();

        if has_sfnts {
            // TrueType CIDFont
            let font_data = concatenate_sfnts_array(ctx, cidfont_entity);
            let upm = font_data
                .as_ref()
                .map(|fd| truetype::get_units_per_em(fd) as f64)
                .unwrap_or(1000.0);
            let em_scale = Matrix::scale(1.0 / upm, 1.0 / upm);
            let combined_fm = type0_fm.multiply(&cidfont_fm).multiply(&em_scale);
            let gd_name = ctx.names.intern(b"GlyphDirectory");
            let glyph_dir_entity = ctx
                .dicts
                .get(cidfont_entity, &DictKey::Name(gd_name))
                .and_then(|obj| match obj.value {
                    PsValue::Dict(e) => Some(e),
                    _ => None,
                });

            for (i, (cid, _)) in cids.iter().enumerate() {
                let glyf_bytes = if let Some(gd_entity) = glyph_dir_entity {
                    ctx.dicts
                        .get(gd_entity, &DictKey::Int(*cid))
                        .and_then(|obj| match obj.value {
                            PsValue::String { entity, start, len } => {
                                Some(ctx.strings.get(entity, start, len).to_vec())
                            }
                            _ => None,
                        })
                } else {
                    font_data
                        .as_ref()
                        .and_then(|fd| truetype::get_glyf_data(fd, *cid as u16))
                };

                if let Some(ref glyf_bytes) = glyf_bytes
                    && glyf_bytes.len() >= 10
                {
                    let glyf_path = {
                        let dicts = &ctx.dicts;
                        let strings = &ctx.strings;
                        let gd = glyph_dir_entity;
                        let fd_ref = font_data.as_deref();
                        let resolver = |gid: u16| -> Option<Vec<u8>> {
                            if let Some(gd_entity) = gd {
                                let key = DictKey::Int(gid as i32);
                                if let Some(obj) = dicts.get(gd_entity, &key)
                                    && let PsValue::String { entity, start, len } = obj.value
                                {
                                    return Some(strings.get(entity, start, len).to_vec());
                                }
                            }
                            fd_ref.and_then(|fd| truetype::get_glyf_data(fd, gid))
                        };
                        truetype::parse_glyf_to_path(glyf_bytes, &resolver)
                    };

                    if !glyf_path.is_empty() {
                        let user_path = transform_path(&glyf_path, &combined_fm, cur_x, cur_y);
                        let device_path = ctm_transform_path(&user_path, &ctm);
                        let params = FillParams {
                            color: ctx.gstate.color.clone(),
                            ctm: Matrix::identity(),
                            fill_rule: FillRule::NonZeroWinding,
                        };
                        ctx.display_list.push(DisplayElement::Fill {
                            path: device_path,
                            params,
                        });
                    }
                }

                advance_by_displacement(&mut cur_x, &mut cur_y, displacements, i, &mode);
            }
        } else {
            // CFF CIDFont
            let cs_entity = ctx
                .dicts
                .get(
                    cidfont_entity,
                    &DictKey::Name(ctx.name_cache.n_char_strings),
                )
                .and_then(|obj| match obj.value {
                    PsValue::Dict(e) => Some(e),
                    _ => None,
                });
            let combined_fm = type0_fm.multiply(&cidfont_fm);
            let global_subrs = get_global_subrs(ctx, cidfont_entity);

            for (i, (cid, _)) in cids.iter().enumerate() {
                if let Some(cs_e) = cs_entity
                    && let Some(cs_obj) = ctx.dicts.get(cs_e, &DictKey::Int(*cid))
                    && let PsValue::String { entity, start, len } = cs_obj.value
                {
                    let cs_bytes = ctx.strings.get(entity, start, len).to_vec();
                    let fd_info = get_cid_fd_info(ctx, cidfont_entity, *cid);
                    if let Ok(result) = type2_charstring::execute_type2_charstring(
                        &cs_bytes,
                        &fd_info.local_subrs,
                        &global_subrs,
                        fd_info.default_width_x,
                        fd_info.nominal_width_x,
                        false,
                    ) && !result.path.is_empty()
                    {
                        let user_path = transform_path(&result.path, &combined_fm, cur_x, cur_y);
                        let device_path = ctm_transform_path(&user_path, &ctm);
                        let params = FillParams {
                            color: ctx.gstate.color.clone(),
                            ctm: Matrix::identity(),
                            fill_rule: FillRule::NonZeroWinding,
                        };
                        ctx.display_list.push(DisplayElement::Fill {
                            path: device_path,
                            params,
                        });
                    }
                }
                advance_by_displacement(&mut cur_x, &mut cur_y, displacements, i, &mode);
            }
        }
    } else {
        // Type 42: simple TrueType font
        let font_data = concatenate_sfnts_array(ctx, font_entity);
        let upm = font_data
            .as_ref()
            .map(|fd| truetype::get_units_per_em(fd) as f64)
            .unwrap_or(1000.0);
        let em_scale = Matrix::scale(1.0 / upm, 1.0 / upm);
        let combined_fm = type0_fm.multiply(&em_scale);

        // Get GlyphDirectory dict (PScript5.dll stores per-glyph data here
        // instead of in the sfnts glyf table)
        let gd_name = ctx.names.intern(b"GlyphDirectory");
        let glyph_dir_entity = ctx
            .dicts
            .get(font_entity, &DictKey::Name(gd_name))
            .and_then(|obj| match obj.value {
                PsValue::Dict(e) => Some(e),
                _ => None,
            });

        let cs_entity = ctx
            .dicts
            .get(font_entity, &DictKey::Name(ctx.name_cache.n_char_strings))
            .and_then(|obj| match obj.value {
                PsValue::Dict(e) => Some(e),
                _ => None,
            });
        let enc_entity = ctx
            .dicts
            .get(font_entity, &DictKey::Name(ctx.name_cache.n_encoding))
            .and_then(|obj| match obj.value {
                PsValue::Array { entity, .. } => Some(entity),
                _ => None,
            });

        for (i, &byte) in bytes.iter().enumerate() {
            if let (Some(enc_ent), Some(cs_ent)) = (enc_entity, cs_entity) {
                let glyph_name_obj = ctx.arrays.get_element(enc_ent, byte as u32);
                if let PsValue::Name(glyph_name_id) = glyph_name_obj.value
                    && let Some(gid_obj) = ctx.dicts.get(cs_ent, &DictKey::Name(glyph_name_id))
                {
                    let gid = gid_obj.as_i32().unwrap_or(0) as u16;

                    // Get glyf data: try GlyphDirectory first, then sfnts
                    let glyf_bytes = if let Some(gd_entity) = glyph_dir_entity {
                        ctx.dicts
                            .get(gd_entity, &DictKey::Int(gid as i32))
                            .and_then(|obj| match obj.value {
                                PsValue::String { entity, start, len } => {
                                    Some(ctx.strings.get(entity, start, len).to_vec())
                                }
                                _ => None,
                            })
                    } else {
                        font_data
                            .as_ref()
                            .and_then(|fd| truetype::get_glyf_data(fd, gid))
                    };

                    if let Some(ref glyf_bytes) = glyf_bytes
                        && glyf_bytes.len() >= 10
                    {
                        let glyf_path = {
                            let dicts = &ctx.dicts;
                            let strings = &ctx.strings;
                            let gd = glyph_dir_entity;
                            let fd_ref = font_data.as_deref();
                            let resolver = |gid: u16| -> Option<Vec<u8>> {
                                if let Some(gd_entity) = gd {
                                    let key = DictKey::Int(gid as i32);
                                    if let Some(obj) = dicts.get(gd_entity, &key)
                                        && let PsValue::String { entity, start, len } = obj.value
                                    {
                                        return Some(strings.get(entity, start, len).to_vec());
                                    }
                                }
                                fd_ref.and_then(|fd| truetype::get_glyf_data(fd, gid))
                            };
                            truetype::parse_glyf_to_path(glyf_bytes, &resolver)
                        };
                        if !glyf_path.is_empty() {
                            let user_path = transform_path(&glyf_path, &combined_fm, cur_x, cur_y);
                            let device_path = ctm_transform_path(&user_path, &ctm);
                            let params = FillParams {
                                color: ctx.gstate.color.clone(),
                                ctm: Matrix::identity(),
                                fill_rule: FillRule::NonZeroWinding,
                            };
                            ctx.display_list.push(DisplayElement::Fill {
                                path: device_path,
                                params,
                            });
                        }
                    }
                }
            }
            advance_by_displacement(&mut cur_x, &mut cur_y, displacements, i, &mode);
        }
    }

    let (dev_x, dev_y) = ctm.transform_point(cur_x, cur_y);
    ctx.gstate.current_point = Some((dev_x, dev_y));
    Ok(())
}

/// Render displaced show for Type 3 fonts (BuildChar-based).
/// Each character is rendered through its BuildChar procedure, but advancement
/// uses the displacement values instead of the glyph width.
fn render_show_displaced_type3(
    ctx: &mut Context,
    font_entity: EntityId,
    bytes: &[u8],
    displacements: &[f64],
    mode: DisplacementMode,
) -> Result<(), PsError> {
    let font_obj = ctx.gstate.current_font.ok_or(PsError::InvalidFont)?;

    // Get BuildChar procedure
    let build_char = ctx
        .dicts
        .get(font_entity, &DictKey::Name(ctx.name_cache.n_build_char))
        .ok_or(PsError::InvalidFont)?;
    if !build_char.flags.is_executable() || !build_char.is_array_type() {
        return Err(PsError::InvalidFont);
    }

    let (dev_cpx, dev_cpy) = ctx.gstate.current_point.ok_or(PsError::NoCurrentPoint)?;
    let ictm = ctx.gstate.ctm.invert().ok_or(PsError::UndefinedResult)?;
    let (mut cur_x, mut cur_y) = ictm.transform_point(dev_cpx, dev_cpy);

    for (i, &byte) in bytes.iter().enumerate() {
        ctx.char_width = None;

        // gsave, translate to current position, concat FontMatrix
        crate::graphics_state_ops::op_gsave(ctx)?;

        ctx.o_stack.push(PsObject::real(cur_x))?;
        ctx.o_stack.push(PsObject::real(cur_y))?;
        crate::matrix_ops::op_translate(ctx)?;

        let fm_obj = ctx
            .dicts
            .get(font_entity, &DictKey::Name(ctx.name_cache.n_font_matrix))
            .ok_or(PsError::InvalidFont)?;
        ctx.o_stack.push(fm_obj)?;
        crate::matrix_ops::op_concat(ctx)?;

        // Push font dict and char code for BuildChar
        ctx.o_stack.push(font_obj)?;
        ctx.o_stack.push(PsObject::int(byte as i32))?;

        // Execute BuildChar synchronously
        ctx.exec_sync(build_char)?;

        // grestore
        crate::graphics_state_ops::op_grestore(ctx)?;

        // Advance by displacement instead of glyph width
        advance_by_displacement(&mut cur_x, &mut cur_y, displacements, i, &mode);
    }

    let ctm = ctx.gstate.ctm;
    let (dev_x, dev_y) = ctm.transform_point(cur_x, cur_y);
    ctx.gstate.current_point = Some((dev_x, dev_y));
    Ok(())
}

/// Advance current position by the displacement for character at index `i`.
fn advance_by_displacement(
    cur_x: &mut f64,
    cur_y: &mut f64,
    displacements: &[f64],
    i: usize,
    mode: &DisplacementMode,
) {
    match mode {
        DisplacementMode::X => {
            if let Some(&dx) = displacements.get(i) {
                *cur_x += dx;
            }
        }
        DisplacementMode::Y => {
            if let Some(&dy) = displacements.get(i) {
                *cur_y += dy;
            }
        }
        DisplacementMode::XY => {
            if let Some(&dx) = displacements.get(i * 2) {
                *cur_x += dx;
            }
            if let Some(&dy) = displacements.get(i * 2 + 1) {
                *cur_y += dy;
            }
        }
    }
}

/// Transform all points in a path through a matrix (e.g., CTM for user→device conversion).
fn ctm_transform_path(path: &PsPath, ctm: &Matrix) -> PsPath {
    let mut result = PsPath::new();
    for seg in &path.segments {
        match *seg {
            PathSegment::MoveTo(x, y) => {
                let (tx, ty) = ctm.transform_point(x, y);
                result.segments.push(PathSegment::MoveTo(tx, ty));
            }
            PathSegment::LineTo(x, y) => {
                let (tx, ty) = ctm.transform_point(x, y);
                result.segments.push(PathSegment::LineTo(tx, ty));
            }
            PathSegment::CurveTo {
                x1,
                y1,
                x2,
                y2,
                x3,
                y3,
            } => {
                let (tx1, ty1) = ctm.transform_point(x1, y1);
                let (tx2, ty2) = ctm.transform_point(x2, y2);
                let (tx3, ty3) = ctm.transform_point(x3, y3);
                result.segments.push(PathSegment::CurveTo {
                    x1: tx1,
                    y1: ty1,
                    x2: tx2,
                    y2: ty2,
                    x3: tx3,
                    y3: ty3,
                });
            }
            PathSegment::ClosePath => {
                result.segments.push(PathSegment::ClosePath);
            }
        }
    }
    result
}

/// Transform a path from glyph space to user space.
/// Applies the font matrix and translates by the current point.
fn transform_path(path: &PsPath, font_matrix: &Matrix, origin_x: f64, origin_y: f64) -> PsPath {
    let mut result = PsPath::new();
    for seg in &path.segments {
        match *seg {
            PathSegment::MoveTo(x, y) => {
                let (tx, ty) = font_matrix.transform_point(x, y);
                result
                    .segments
                    .push(PathSegment::MoveTo(tx + origin_x, ty + origin_y));
            }
            PathSegment::LineTo(x, y) => {
                let (tx, ty) = font_matrix.transform_point(x, y);
                result
                    .segments
                    .push(PathSegment::LineTo(tx + origin_x, ty + origin_y));
            }
            PathSegment::CurveTo {
                x1,
                y1,
                x2,
                y2,
                x3,
                y3,
            } => {
                let (tx1, ty1) = font_matrix.transform_point(x1, y1);
                let (tx2, ty2) = font_matrix.transform_point(x2, y2);
                let (tx3, ty3) = font_matrix.transform_point(x3, y3);
                result.segments.push(PathSegment::CurveTo {
                    x1: tx1 + origin_x,
                    y1: ty1 + origin_y,
                    x2: tx2 + origin_x,
                    y2: ty2 + origin_y,
                    x3: tx3 + origin_x,
                    y3: ty3 + origin_y,
                });
            }
            PathSegment::ClosePath => {
                result.segments.push(PathSegment::ClosePath);
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use stet_core::context::Context;

    fn test_ctx_with_font() -> Option<Context> {
        let font_path = std::path::Path::new(
            "/home/scott/Projects/postforge/postforge/resources/Font/NimbusSans-Regular.t1",
        );
        if !font_path.exists() {
            return None;
        }

        let mut ctx = Context::new();
        crate::build_system_dict(&mut ctx);
        ctx.font_resource_path =
            Some("/home/scott/Projects/postforge/postforge/resources/Font".to_string());

        // Load and scale font: Helvetica 12pt
        let font_data = std::fs::read(font_path).ok()?;
        let font_obj = stet_core::font_loader::load_type1_font(&mut ctx, &font_data).ok()?;

        // Scale by 12
        if let PsValue::Dict(_font_entity) = font_obj.value {
            let scaled = super::super::font_ops::op_scalefont;
            // Do it manually
            ctx.o_stack.push(font_obj).ok()?;
            ctx.o_stack.push(PsObject::real(12.0)).ok()?;
            scaled(&mut ctx).ok()?;
            let scaled_font = ctx.o_stack.pop().ok()?;
            ctx.gstate.current_font = Some(scaled_font);
        }

        // Set current point
        ctx.gstate.current_point = Some((72.0, 700.0));

        Some(ctx)
    }

    #[test]
    fn test_stringwidth() {
        let mut ctx = match test_ctx_with_font() {
            Some(ctx) => ctx,
            None => {
                eprintln!("Skipping test — font file not found");
                return;
            }
        };

        let hello = b"Hello";
        let entity = ctx.strings.allocate_from(hello);
        ctx.o_stack
            .push(PsObject::string(entity, hello.len() as u32))
            .unwrap();
        op_stringwidth(&mut ctx).unwrap();

        let wy = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        let wx = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        assert!(
            wx > 0.0,
            "stringwidth should return positive wx, got {}",
            wx
        );
        assert!(
            (wy).abs() < 0.01,
            "wy should be ~0 for horizontal text, got {}",
            wy
        );
    }

    #[test]
    fn test_show_advances_currentpoint() {
        let mut ctx = match test_ctx_with_font() {
            Some(ctx) => ctx,
            None => {
                eprintln!("Skipping test — font file not found");
                return;
            }
        };

        let (start_x, _start_y) = ctx.gstate.current_point.unwrap();

        let hello = b"Hello";
        let entity = ctx.strings.allocate_from(hello);
        ctx.o_stack
            .push(PsObject::string(entity, hello.len() as u32))
            .unwrap();
        op_show(&mut ctx).unwrap();

        let (end_x, _end_y) = ctx.gstate.current_point.unwrap();
        assert!(
            end_x > start_x,
            "currentpoint should advance: {} > {}",
            end_x,
            start_x
        );
    }

    #[test]
    fn test_charpath_appends_to_path() {
        let mut ctx = match test_ctx_with_font() {
            Some(ctx) => ctx,
            None => {
                eprintln!("Skipping test — font file not found");
                return;
            }
        };

        assert!(ctx.gstate.path.is_empty());

        let a = b"A";
        let entity = ctx.strings.allocate_from(a);
        ctx.o_stack
            .push(PsObject::string(entity, a.len() as u32))
            .unwrap();
        ctx.o_stack.push(PsObject::bool(false)).unwrap();
        op_charpath(&mut ctx).unwrap();

        assert!(
            !ctx.gstate.path.is_empty(),
            "charpath should append segments to current path"
        );
    }

    #[test]
    fn test_setcachedevice_pops_six() {
        let mut ctx = Context::new();
        crate::build_system_dict(&mut ctx);

        for i in 0..6 {
            ctx.o_stack.push(PsObject::real(i as f64)).unwrap();
        }
        op_setcachedevice(&mut ctx).unwrap();
        assert!(ctx.o_stack.is_empty());
    }

    #[test]
    fn test_setcharwidth_pops_two() {
        let mut ctx = Context::new();
        crate::build_system_dict(&mut ctx);

        ctx.o_stack.push(PsObject::real(600.0)).unwrap();
        ctx.o_stack.push(PsObject::real(0.0)).unwrap();
        op_setcharwidth(&mut ctx).unwrap();
        assert!(ctx.o_stack.is_empty());
    }
}
