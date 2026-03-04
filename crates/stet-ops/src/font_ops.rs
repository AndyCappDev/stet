// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Font dictionary operators: definefont, undefinefont, findfont,
//! scalefont, makefont, setfont, currentfont.

use stet_core::context::Context;
use stet_core::dict::DictKey;
use stet_core::error::PsError;
use stet_core::graphics_state::Matrix;
use stet_core::object::{ObjFlags, PsObject, PsValue};

/// `definefont`: key font → font
///
/// Register a font dictionary in FontDirectory with the given key name.
/// Assigns a unique FID if not already present.
pub fn op_definefont(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let font_obj = ctx.o_stack.peek(0)?;
    let key_obj = ctx.o_stack.peek(1)?;

    // Font must be a dict
    let font_entity = match font_obj.value {
        PsValue::Dict(e) => e,
        _ => return Err(PsError::TypeCheck),
    };

    // Key can be any valid dict key type (PLRM allows any)
    let dict_key = ctx.make_dict_key(&key_obj)?;

    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;

    // Assign FID if not present
    let fid_key = DictKey::Name(ctx.name_cache.n_fid);
    if ctx.dicts.get(font_entity, &fid_key).is_none() {
        let fid = ctx.next_fid;
        ctx.next_fid += 1;
        ctx.dicts.put(
            font_entity,
            fid_key,
            PsObject {
                value: PsValue::FontID(fid),
                flags: ObjFlags::literal(),
            },
        );
    }

    // Register in FontDirectory
    ctx.dicts.put(ctx.font_directory, dict_key, font_obj);

    ctx.o_stack.push(font_obj)?;
    Ok(())
}

/// `undefinefont`: key → —
///
/// Remove a font from FontDirectory.
pub fn op_undefinefont(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let key_obj = ctx.o_stack.peek(0)?;
    let key_name = match key_obj.value {
        PsValue::Name(id) => id,
        _ => return Err(PsError::TypeCheck),
    };

    ctx.o_stack.pop()?;

    ctx.dicts
        .remove(ctx.font_directory, &DictKey::Name(key_name));
    Ok(())
}

/// `findfont`: key → font
///
/// Look up a font by name: check FontDirectory, then try loading from disk.
pub fn op_findfont(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let key_obj = ctx.o_stack.peek(0)?;
    let name_bytes = match key_obj.value {
        PsValue::Name(id) => ctx.names.get_bytes(id).to_vec(),
        PsValue::String { entity, start, len } => ctx.strings.get(entity, start, len).to_vec(),
        _ => return Err(PsError::TypeCheck),
    };

    ctx.o_stack.pop()?;

    let font_obj =
        stet_core::font_loader::find_font(ctx, &name_bytes).map_err(|_| PsError::InvalidFont)?;

    ctx.o_stack.push(font_obj)?;
    Ok(())
}

/// `scalefont`: font scale → font'
///
/// Copy font dict and scale its FontMatrix by the given scalar.
pub fn op_scalefont(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let scale_obj = ctx.o_stack.peek(0)?;
    let font_obj = ctx.o_stack.peek(1)?;

    let scale = scale_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let font_entity = match font_obj.value {
        PsValue::Dict(e) => e,
        _ => return Err(PsError::TypeCheck),
    };

    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;

    // Create scaled font
    let scale_matrix = Matrix::scale(scale, scale);
    let new_font = copy_font_with_matrix(ctx, font_entity, &scale_matrix)?;

    ctx.o_stack.push(new_font)?;
    Ok(())
}

/// `makefont`: font matrix → font'
///
/// Copy font dict and compose matrix with its FontMatrix.
pub fn op_makefont(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let matrix_obj = ctx.o_stack.peek(0)?;
    let font_obj = ctx.o_stack.peek(1)?;

    // Extract the 6-element matrix from the array
    let (mat_entity, mat_start, mat_len) = match matrix_obj.value {
        PsValue::Array { entity, start, len } => {
            if len != 6 {
                return Err(PsError::RangeCheck);
            }
            (entity, start, len)
        }
        _ => return Err(PsError::TypeCheck),
    };

    let font_entity = match font_obj.value {
        PsValue::Dict(e) => e,
        _ => return Err(PsError::TypeCheck),
    };

    // Read matrix values
    let elems = ctx.arrays.get(mat_entity, mat_start, mat_len);
    let m = read_matrix_from_array(elems)?;

    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;

    let new_font = copy_font_with_matrix(ctx, font_entity, &m)?;

    ctx.o_stack.push(new_font)?;
    Ok(())
}

/// `setfont`: font → —
///
/// Set the current font in the graphics state.
pub fn op_setfont(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let font_obj = ctx.o_stack.peek(0)?;

    // Must be a dict
    if !matches!(font_obj.value, PsValue::Dict(_)) {
        return Err(PsError::TypeCheck);
    }

    ctx.o_stack.pop()?;
    ctx.gstate.current_font = Some(font_obj);
    Ok(())
}

/// `currentfont`: — → font
///
/// Push the current font from the graphics state.
pub fn op_currentfont(ctx: &mut Context) -> Result<(), PsError> {
    let font = ctx.gstate.current_font.ok_or(PsError::InvalidFont)?;
    ctx.o_stack.push(font)?;
    Ok(())
}

/// `rootfont`: — → font
///
/// Return the root font of the current composite font hierarchy.
/// Inside Type 0 composite rendering this returns the top-level font;
/// otherwise it is identical to `currentfont`.
pub fn op_rootfont(ctx: &mut Context) -> Result<(), PsError> {
    let font = ctx
        .gstate
        .root_font
        .or(ctx.gstate.current_font)
        .ok_or(PsError::InvalidFont)?;
    ctx.o_stack.push(font)?;
    Ok(())
}

/// `selectfont`: key scale → —
///
/// Equivalent to `exch findfont exch scalefont setfont`.
pub fn op_selectfont(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let scale_obj = ctx.o_stack.peek(0)?;
    let key_obj = ctx.o_stack.peek(1)?;

    // Validate types before popping
    let _ = scale_obj.as_f64().ok_or(PsError::TypeCheck)?;
    match key_obj.value {
        PsValue::Name(_) | PsValue::String { .. } => {}
        _ => return Err(PsError::TypeCheck),
    }

    // exch findfont exch scalefont setfont
    // Stack: key scale → scale key → scale font → scaled_font → (set)
    // We already have: key scale on top
    // Swap to get: scale key
    ctx.o_stack.swap_top_two()?;
    // findfont: key → font  =>  scale font
    op_findfont(ctx)?;
    // swap: scale font → font scale
    ctx.o_stack.swap_top_two()?;
    // scalefont: font scale → scaled_font
    op_scalefont(ctx)?;
    // setfont: scaled_font → —
    op_setfont(ctx)?;
    Ok(())
}

/// Page size no-op operators. Page size is already set by CLI.
pub fn op_letter(_ctx: &mut Context) -> Result<(), PsError> {
    Ok(())
}

/// `legal` page size — no-op.
pub fn op_legal(_ctx: &mut Context) -> Result<(), PsError> {
    Ok(())
}

/// `a4` page size — no-op.
pub fn op_a4(_ctx: &mut Context) -> Result<(), PsError> {
    Ok(())
}

/// `a3` page size — no-op.
pub fn op_a3(_ctx: &mut Context) -> Result<(), PsError> {
    Ok(())
}

/// `b5` page size — no-op.
pub fn op_b5(_ctx: &mut Context) -> Result<(), PsError> {
    Ok(())
}

/// `.loadfont`: fontname → fontdict true | false
///
/// Internal operator: loads a font using stet's native Type 1 parser.
/// Called by fontcategory.ps FindResource instead of `run` (which would
/// try to execute the .t1 file as PostScript, hitting the `eexec` operator
/// that stet doesn't implement).
pub fn op_dot_loadfont(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let name_obj = ctx.o_stack.peek(0)?;
    let name_bytes = match name_obj.value {
        PsValue::Name(id) => ctx.names.get_bytes(id).to_vec(),
        PsValue::String { entity, start, len } => ctx.strings.get(entity, start, len).to_vec(),
        _ => return Err(PsError::TypeCheck),
    };

    ctx.o_stack.pop()?;

    match stet_core::font_loader::find_font(ctx, &name_bytes) {
        Ok(font_obj) => {
            ctx.o_stack.push(font_obj)?;
            ctx.o_stack.push(PsObject::bool(true))?;
        }
        Err(_) => {
            ctx.o_stack.push(PsObject::bool(false))?;
        }
    }
    Ok(())
}

/// `composefont`: key cmapname array → font
///
/// Constructs a Type 0 composite font from a CMap and an array of
/// descendant CIDFonts. The CMap is resolved from the CMap resource
/// category, and each element of the array is resolved from CIDFont
/// (then Font) resource categories if not already a dict.
pub fn op_composefont(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 3 {
        return Err(PsError::StackUnderflow);
    }
    let array_obj = ctx.o_stack.peek(0)?;
    let cmap_obj = ctx.o_stack.peek(1)?;
    let key_obj = ctx.o_stack.peek(2)?;

    // Validate types
    let (fdep_entity, fdep_start, fdep_len) = match array_obj.value {
        PsValue::Array { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };

    // Key must be a name or string
    let font_name_id = match key_obj.value {
        PsValue::Name(id) => id,
        PsValue::String { entity, start, len } => {
            let bytes = ctx.strings.get(entity, start, len).to_vec();
            ctx.names.intern(&bytes)
        }
        _ => return Err(PsError::TypeCheck),
    };

    // Resolve CMap: if it's a dict, use directly; if name/string, find from CMap category
    let cmap_dict = match cmap_obj.value {
        PsValue::Dict(_) => cmap_obj,
        PsValue::Name(id) => {
            let name_bytes = ctx.names.get_bytes(id).to_vec();
            find_resource_direct(ctx, &name_bytes, b"CMap").ok_or(PsError::UndefinedResource)?
        }
        PsValue::String { entity, start, len } => {
            let bytes = ctx.strings.get(entity, start, len).to_vec();
            find_resource_direct(ctx, &bytes, b"CMap").ok_or(PsError::UndefinedResource)?
        }
        _ => return Err(PsError::TypeCheck),
    };

    // Resolve descendant fonts from the array
    let fdep_elems = ctx.arrays.get(fdep_entity, fdep_start, fdep_len).to_vec();
    let mut resolved_fonts = Vec::with_capacity(fdep_elems.len());
    for item in &fdep_elems {
        match item.value {
            PsValue::Dict(_) => resolved_fonts.push(*item),
            PsValue::Name(id) => {
                let name_bytes = ctx.names.get_bytes(id).to_vec();
                let font = find_resource_direct(ctx, &name_bytes, b"CIDFont")
                    .or_else(|| find_resource_direct(ctx, &name_bytes, b"Font"))
                    .ok_or(PsError::UndefinedResource)?;
                resolved_fonts.push(font);
            }
            PsValue::String { entity, start, len } => {
                let bytes = ctx.strings.get(entity, start, len).to_vec();
                let font = find_resource_direct(ctx, &bytes, b"CIDFont")
                    .or_else(|| find_resource_direct(ctx, &bytes, b"Font"))
                    .ok_or(PsError::UndefinedResource)?;
                resolved_fonts.push(font);
            }
            _ => return Err(PsError::TypeCheck),
        }
    }

    ctx.o_stack.pop()?; // array
    ctx.o_stack.pop()?; // cmapname
    ctx.o_stack.pop()?; // key

    // Build Type 0 font dictionary
    let save_level = ctx.save_stack.current_level();
    let global = ctx.vm_alloc_mode;
    let created = ctx.save_stack.last_save_id();
    let font_entity = ctx
        .dicts
        .allocate_with(12, b"type0font", save_level, global, created);

    // FontType 0
    let ft_key = DictKey::Name(ctx.name_cache.n_font_type);
    ctx.dicts.put(font_entity, ft_key, PsObject::int(0));

    // FMapType 9 (CMap-based)
    let fmap_id = ctx.names.intern(b"FMapType");
    ctx.dicts
        .put(font_entity, DictKey::Name(fmap_id), PsObject::int(9));

    // FontMatrix [1 0 0 1 0 0] (identity)
    let fm_entity = ctx.arrays.allocate_with(6, save_level, global, created);
    let fm_data = ctx.arrays.get_mut(fm_entity, 0, 6);
    fm_data[0] = PsObject::real(1.0);
    fm_data[1] = PsObject::real(0.0);
    fm_data[2] = PsObject::real(0.0);
    fm_data[3] = PsObject::real(1.0);
    fm_data[4] = PsObject::real(0.0);
    fm_data[5] = PsObject::real(0.0);
    let fm_key = DictKey::Name(ctx.name_cache.n_font_matrix);
    ctx.dicts
        .put(font_entity, fm_key, PsObject::array(fm_entity, 6));

    // Encoding: identity [0, 1, 2, ...]
    let enc_len = resolved_fonts.len();
    let enc_entity = ctx.arrays.allocate_with(enc_len, save_level, global, created);
    let enc_data = ctx.arrays.get_mut(enc_entity, 0, enc_len as u32);
    for (i, slot) in enc_data.iter_mut().enumerate() {
        *slot = PsObject::int(i as i32);
    }
    let enc_key = DictKey::Name(ctx.name_cache.n_encoding);
    ctx.dicts.put(
        font_entity,
        enc_key,
        PsObject::array(enc_entity, enc_len as u32),
    );

    // FDepVector: the resolved fonts
    let fdep_entity_new = ctx
        .arrays
        .allocate_with(resolved_fonts.len(), save_level, global, created);
    let fdep_data = ctx
        .arrays
        .get_mut(fdep_entity_new, 0, resolved_fonts.len() as u32);
    fdep_data.copy_from_slice(&resolved_fonts);
    let fdep_id = ctx.names.intern(b"FDepVector");
    ctx.dicts.put(
        font_entity,
        DictKey::Name(fdep_id),
        PsObject::array(fdep_entity_new, resolved_fonts.len() as u32),
    );

    // CMap
    let cmap_id = ctx.names.intern(b"CMap");
    ctx.dicts
        .put(font_entity, DictKey::Name(cmap_id), cmap_dict);

    // FontName
    let fn_key = DictKey::Name(ctx.name_cache.n_font_name);
    ctx.dicts
        .put(font_entity, fn_key, PsObject::name_lit(font_name_id));

    // WMode from CMap (default 0)
    let wmode_id = ctx.names.intern(b"WMode");
    let wmode = if let PsValue::Dict(cmap_entity) = cmap_dict.value {
        ctx.dicts
            .get(cmap_entity, &DictKey::Name(wmode_id))
            .and_then(|o| o.as_f64())
            .map(|v| v as i32)
            .unwrap_or(0)
    } else {
        0
    };
    ctx.dicts
        .put(font_entity, DictKey::Name(wmode_id), PsObject::int(wmode));

    // FID
    let fid_key = DictKey::Name(ctx.name_cache.n_fid);
    let fid = ctx.next_fid;
    ctx.next_fid += 1;
    ctx.dicts.put(
        font_entity,
        fid_key,
        PsObject {
            value: PsValue::FontID(fid),
            flags: ObjFlags::literal(),
        },
    );

    // Register in FontDirectory
    let font_obj = PsObject::dict(font_entity);
    ctx.dicts
        .put(ctx.font_directory, DictKey::Name(font_name_id), font_obj);

    // Also register in the Font resource dict so /Font findresource can find it.
    // findfont is defined as { /Font findresource } which searches resource dicts,
    // not FontDirectory directly.
    let font_cat_id = ctx.names.intern(b"Font");
    let font_cat_key = DictKey::Name(font_cat_id);
    let res_dict = if ctx.vm_alloc_mode {
        ctx.global_resources
    } else {
        ctx.local_resources
    };
    let cat_dict = if let Some(existing) = ctx.dicts.get(res_dict, &font_cat_key)
        && let PsValue::Dict(e) = existing.value
    {
        e
    } else {
        let d = ctx.dicts.allocate(20, b"font_res_cat");
        ctx.dicts.put(res_dict, font_cat_key, PsObject::dict(d));
        d
    };
    ctx.dicts
        .put(cat_dict, DictKey::Name(font_name_id), font_obj);

    ctx.o_stack.push(font_obj)?;
    Ok(())
}

/// Look up a named resource directly from local/global resource dicts.
/// Does NOT go through the PS-level findresource dispatch (which would
/// require eval loop execution).
fn find_resource_direct(ctx: &mut Context, name: &[u8], category: &[u8]) -> Option<PsObject> {
    let name_id = ctx.names.intern(name);
    let cat_id = ctx.names.intern(category);
    let cat_key = DictKey::Name(cat_id);
    let name_key = DictKey::Name(name_id);

    // Search local resources first
    if let Some(local_cat_obj) = ctx.dicts.get(ctx.local_resources, &cat_key)
        && let PsValue::Dict(local_cat) = local_cat_obj.value
        && let Some(val) = ctx.dicts.get(local_cat, &name_key)
    {
        return Some(val);
    }

    // Then global resources
    if let Some(global_cat_obj) = ctx.dicts.get(ctx.global_resources, &cat_key)
        && let PsValue::Dict(global_cat) = global_cat_obj.value
        && let Some(val) = ctx.dicts.get(global_cat, &name_key)
    {
        return Some(val);
    }

    None
}

// --- Helpers ---

/// Read a 6-element Matrix from an array of PsObjects.
fn read_matrix_from_array(elems: &[PsObject]) -> Result<Matrix, PsError> {
    if elems.len() < 6 {
        return Err(PsError::RangeCheck);
    }
    Ok(Matrix::new(
        elems[0].as_f64().ok_or(PsError::TypeCheck)?,
        elems[1].as_f64().ok_or(PsError::TypeCheck)?,
        elems[2].as_f64().ok_or(PsError::TypeCheck)?,
        elems[3].as_f64().ok_or(PsError::TypeCheck)?,
        elems[4].as_f64().ok_or(PsError::TypeCheck)?,
        elems[5].as_f64().ok_or(PsError::TypeCheck)?,
    ))
}

/// Copy a font dictionary and compose a transformation matrix with its FontMatrix.
/// Removes /FID from the copy (per PLRM).
fn copy_font_with_matrix(
    ctx: &mut Context,
    font_entity: stet_core::object::EntityId,
    transform: &Matrix,
) -> Result<PsObject, PsError> {
    // Read the existing FontMatrix
    let fm_key = DictKey::Name(ctx.name_cache.n_font_matrix);
    let fm_obj = ctx
        .dicts
        .get(font_entity, &fm_key)
        .ok_or(PsError::InvalidFont)?;

    let (fm_entity, fm_start, fm_len) = match fm_obj.value {
        PsValue::Array { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::InvalidFont),
    };

    let fm_elems = ctx.arrays.get(fm_entity, fm_start, fm_len);
    let old_fm = read_matrix_from_array(fm_elems)?;

    // Compose: new_fm = transform * old_fm (in PostScript row-vector convention)
    // In our column-vector multiply: old_fm.multiply(transform) = transform × old_fm (row-vector)
    let new_fm = old_fm.multiply(transform);

    // Create a shallow copy of the font dict
    // We copy all entries from the original dict to a new dict
    let new_dict = ctx
        .dicts
        .allocate(ctx.dicts.max_length(font_entity), b"font");

    // Copy all entries
    let keys: Vec<DictKey> = ctx.dicts.keys(font_entity).cloned().collect();
    for key in keys {
        if let Some(val) = ctx.dicts.get(font_entity, &key) {
            ctx.dicts.put(new_dict, key, val);
        }
    }

    // Replace FontMatrix with new composed matrix
    let new_fm_items: Vec<PsObject> = new_fm
        .to_array()
        .iter()
        .map(|&v| PsObject::real(v))
        .collect();
    let new_fm_entity = ctx.arrays.allocate_from(&new_fm_items);
    ctx.dicts
        .put(new_dict, fm_key, PsObject::array(new_fm_entity, 6));

    // Remove FID (per PLRM — scaled/transformed fonts don't keep the original FID)
    ctx.dicts
        .remove(new_dict, &DictKey::Name(ctx.name_cache.n_fid));

    Ok(PsObject::dict(new_dict))
}

#[cfg(test)]
mod tests {
    use super::*;
    use stet_core::context::Context;

    fn test_ctx() -> Context {
        let mut ctx = Context::new();
        crate::build_system_dict(&mut ctx);
        ctx.font_resource_path =
            Some("/home/scott/Projects/postforge/postforge/resources/Font".to_string());
        ctx
    }

    fn make_simple_font(ctx: &mut Context) -> PsObject {
        // Create a minimal font dict for testing
        let font_dict = ctx.dicts.allocate(20, b"TestFont");

        // FontMatrix [0.001 0 0 0.001 0 0]
        let fm = [0.001, 0.0, 0.0, 0.001, 0.0, 0.0];
        let fm_items: Vec<PsObject> = fm.iter().map(|&v| PsObject::real(v)).collect();
        let fm_entity = ctx.arrays.allocate_from(&fm_items);
        ctx.dicts.put(
            font_dict,
            DictKey::Name(ctx.name_cache.n_font_matrix),
            PsObject::array(fm_entity, 6),
        );

        // FontType 1
        ctx.dicts.put(
            font_dict,
            DictKey::Name(ctx.name_cache.n_font_type),
            PsObject::int(1),
        );

        // FontName
        let name_id = ctx.names.intern(b"TestFont");
        ctx.dicts.put(
            font_dict,
            DictKey::Name(ctx.name_cache.n_font_name),
            PsObject::name_lit(name_id),
        );

        PsObject::dict(font_dict)
    }

    #[test]
    fn test_definefont_findfont_roundtrip() {
        let mut ctx = test_ctx();
        let font = make_simple_font(&mut ctx);
        let key_id = ctx.names.intern(b"TestFont");

        // definefont
        ctx.o_stack.push(PsObject::name_lit(key_id)).unwrap();
        ctx.o_stack.push(font).unwrap();
        op_definefont(&mut ctx).unwrap();

        // Should return the font on stack
        let result = ctx.o_stack.pop().unwrap();
        assert!(matches!(result.value, PsValue::Dict(_)));

        // findfont should find it
        ctx.o_stack.push(PsObject::name_lit(key_id)).unwrap();
        op_findfont(&mut ctx).unwrap();
        let found = ctx.o_stack.pop().unwrap();
        assert!(matches!(found.value, PsValue::Dict(_)));
    }

    #[test]
    fn test_scalefont() {
        let mut ctx = test_ctx();
        let font = make_simple_font(&mut ctx);

        ctx.o_stack.push(font).unwrap();
        ctx.o_stack.push(PsObject::real(12.0)).unwrap();
        op_scalefont(&mut ctx).unwrap();

        let scaled = ctx.o_stack.pop().unwrap();
        if let PsValue::Dict(entity) = scaled.value {
            // Check FontMatrix was scaled
            let fm_obj = ctx
                .dicts
                .get(entity, &DictKey::Name(ctx.name_cache.n_font_matrix))
                .unwrap();
            if let PsValue::Array {
                entity: fm_e,
                start,
                len,
            } = fm_obj.value
            {
                let elems = ctx.arrays.get(fm_e, start, len);
                let a = elems[0].as_f64().unwrap();
                // 0.001 * 12 = 0.012
                assert!((a - 0.012).abs() < 1e-6, "got {}", a);
            }

            // FID should be removed
            assert!(
                ctx.dicts
                    .get(entity, &DictKey::Name(ctx.name_cache.n_fid))
                    .is_none()
            );
        }
    }

    #[test]
    fn test_setfont_currentfont() {
        let mut ctx = test_ctx();
        let font = make_simple_font(&mut ctx);

        // setfont
        ctx.o_stack.push(font).unwrap();
        op_setfont(&mut ctx).unwrap();

        // currentfont
        op_currentfont(&mut ctx).unwrap();
        let result = ctx.o_stack.pop().unwrap();
        assert!(matches!(result.value, PsValue::Dict(_)));
    }

    #[test]
    fn test_findfont_loads_from_disk() {
        let font_path = std::path::Path::new(
            "/home/scott/Projects/postforge/postforge/resources/Font/NimbusSans-Regular.t1",
        );
        if !font_path.exists() {
            eprintln!("Skipping test — font file not found");
            return;
        }

        let mut ctx = test_ctx();

        // findfont Helvetica (should substitute to NimbusSans-Regular)
        let helv_id = ctx.names.intern(b"Helvetica");
        ctx.o_stack.push(PsObject::name_lit(helv_id)).unwrap();
        op_findfont(&mut ctx).unwrap();

        let font = ctx.o_stack.pop().unwrap();
        assert!(matches!(font.value, PsValue::Dict(_)));
    }

    #[test]
    fn test_findfont_unknown_font() {
        let mut ctx = test_ctx();
        let id = ctx.names.intern(b"NoSuchFont");
        ctx.o_stack.push(PsObject::name_lit(id)).unwrap();
        let result = op_findfont(&mut ctx);
        assert_eq!(result, Err(PsError::InvalidFont));
    }

    #[test]
    fn test_undefinefont() {
        let mut ctx = test_ctx();
        let font = make_simple_font(&mut ctx);
        let key_id = ctx.names.intern(b"TestFont");

        // definefont
        ctx.o_stack.push(PsObject::name_lit(key_id)).unwrap();
        ctx.o_stack.push(font).unwrap();
        op_definefont(&mut ctx).unwrap();
        ctx.o_stack.pop().unwrap(); // consume result

        // undefinefont
        ctx.o_stack.push(PsObject::name_lit(key_id)).unwrap();
        op_undefinefont(&mut ctx).unwrap();

        // Should no longer be in FontDirectory
        assert!(
            ctx.dicts
                .get(ctx.font_directory, &DictKey::Name(key_id))
                .is_none()
        );
    }

    #[test]
    fn test_makefont() {
        let mut ctx = test_ctx();
        let font = make_simple_font(&mut ctx);

        // Create a matrix [2 0 0 1 0 0] (scale x by 2)
        let mat_items = [
            PsObject::real(2.0),
            PsObject::real(0.0),
            PsObject::real(0.0),
            PsObject::real(1.0),
            PsObject::real(0.0),
            PsObject::real(0.0),
        ];
        let mat_entity = ctx.arrays.allocate_from(&mat_items);

        ctx.o_stack.push(font).unwrap();
        ctx.o_stack.push(PsObject::array(mat_entity, 6)).unwrap();
        op_makefont(&mut ctx).unwrap();

        let result = ctx.o_stack.pop().unwrap();
        if let PsValue::Dict(entity) = result.value {
            let fm_obj = ctx
                .dicts
                .get(entity, &DictKey::Name(ctx.name_cache.n_font_matrix))
                .unwrap();
            if let PsValue::Array {
                entity: fm_e,
                start,
                len,
            } = fm_obj.value
            {
                let elems = ctx.arrays.get(fm_e, start, len);
                let a = elems[0].as_f64().unwrap();
                // 0.001 * 2 = 0.002
                assert!((a - 0.002).abs() < 1e-6, "got {}", a);
            }
        }
    }
}
