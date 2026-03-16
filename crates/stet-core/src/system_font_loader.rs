// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! System font loading: TTF/OTF → PostScript font dictionaries.
//!
//! Loads TrueType (.ttf) and OpenType (.otf) fonts from the filesystem,
//! constructing Type 42 font dicts (for TrueType outlines) or delegating
//! to the CFF parser (for CFF outlines).

use std::collections::HashMap;
use std::path::Path;

use crate::cff_parser;
use crate::context::Context;
use crate::dict::DictKey;
use crate::object::{ObjFlags, PsObject, PsValue};
use crate::system_fonts::{
    extract_ps_name_from_name_table, parse_cmap_table, parse_post_table, unicode_to_glyph_name,
};
use crate::truetype::{find_table, get_units_per_em, read_i16};

/// Load an OTF file with CFF outlines.
///
/// Extracts the CFF table, parses it, and registers fonts via the CFF pipeline.
/// Returns true on success.
pub fn load_otf_cff(ctx: &mut Context, path: &Path) -> bool {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(_) => return false,
    };

    // Find CFF table
    let (cff_offset, cff_length) = match find_table(&data, b"CFF ") {
        Some(t) => t,
        None => return false,
    };

    if cff_offset + cff_length > data.len() {
        return false;
    }

    let cff_data = &data[cff_offset..cff_offset + cff_length];

    let cff_fonts = match cff_parser::parse_cff(cff_data) {
        Ok(f) => f,
        Err(_) => return false,
    };

    // Register in global VM
    let saved_vm_mode = ctx.vm_alloc_mode;
    ctx.vm_alloc_mode = true;

    // Store raw CFF binary for PDF embedding
    let sl = ctx.save_stack.current_level();
    let cs = ctx.save_stack.last_save_id();
    let cff_str_entity = ctx.strings.allocate_from_with(cff_data, sl, true, cs);
    let cff_data_len = cff_data.len() as u32;

    let mut success = true;
    for cff_font in &cff_fonts {
        if register_cff_font(ctx, cff_font, cff_str_entity, cff_data_len).is_err() {
            success = false;
            break;
        }
    }

    ctx.vm_alloc_mode = saved_vm_mode;
    success
}

/// Load a TrueType font and register it as a Type 42 PostScript font.
///
/// Returns true on success.
pub fn load_ttf(ctx: &mut Context, path: &Path) -> bool {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(_) => return false,
    };

    load_ttf_from_data(ctx, &data)
}

/// Load a TrueType font from raw data and register as Type 42.
pub fn load_ttf_from_data(ctx: &mut Context, data: &[u8]) -> bool {
    if data.len() < 12 {
        return false;
    }

    // Extract font name from name table
    let font_name = match extract_ps_name_from_name_table(data) {
        Some(n) => n,
        None => return false,
    };

    // Parse head table for unitsPerEm and bbox
    let units_per_em = get_units_per_em(data);
    let em_scale = 1.0 / units_per_em as f64;

    let (x_min, y_min, x_max, y_max) = get_font_bbox(data);
    let bbox = [
        x_min as f64 * em_scale,
        y_min as f64 * em_scale,
        x_max as f64 * em_scale,
        y_max as f64 * em_scale,
    ];

    // Parse cmap for Unicode→GID mapping
    let unicode_to_gid = match parse_cmap_table(data) {
        Some(m) => m,
        None => return false,
    };

    // Parse post table for GID→name mapping (optional)
    let gid_to_name = parse_post_table(data);

    // Build glyph name resolution: GID → name
    // Priority: post table → AGL reverse → uniXXXX
    let resolve_glyph_name = |gid: u16, unicode: Option<u32>| -> String {
        // Try post table first
        if let Some(ref post_map) = gid_to_name
            && let Some(name) = post_map.get(&gid)
        {
            return name.clone();
        }
        // Try AGL reverse mapping
        if let Some(cp) = unicode {
            if let Some(name) = unicode_to_glyph_name(cp) {
                return name.to_string();
            }
            return format!("uni{:04X}", cp);
        }
        format!("glyph{}", gid)
    };

    // Build Encoding (256 entries) and CharStrings
    let mut encoding = vec![".notdef".to_string(); 256];
    let mut charstrings: HashMap<String, u16> = HashMap::new();
    charstrings.insert(".notdef".to_string(), 0);

    // Map Unicode 0-255 to Encoding slots
    for code in 0u32..256 {
        if let Some(&gid) = unicode_to_gid.get(&code) {
            let name = resolve_glyph_name(gid, Some(code));
            encoding[code as usize] = name.clone();
            charstrings.insert(name, gid);
        }
    }

    // Add all other mapped glyphs to CharStrings (for glyphshow)
    for (&unicode, &gid) in &unicode_to_gid {
        if unicode >= 256 {
            let name = resolve_glyph_name(gid, Some(unicode));
            charstrings.entry(name).or_insert(gid);
        }
    }

    // Register in global VM
    let saved_vm_mode = ctx.vm_alloc_mode;
    ctx.vm_alloc_mode = true;

    let success = build_type42_font(ctx, &font_name, &bbox, &encoding, &charstrings, data);

    ctx.vm_alloc_mode = saved_vm_mode;
    success
}

/// Build and register a Type 42 font dictionary.
/// All allocations use global VM (PLRM: fonts always in global VM).
fn build_type42_font(
    ctx: &mut Context,
    font_name: &str,
    bbox: &[f64; 4],
    encoding: &[String],
    charstrings: &HashMap<String, u16>,
    font_data: &[u8],
) -> bool {
    let sl = ctx.save_stack.current_level();
    let cs = ctx.save_stack.last_save_id();

    let font_entity = ctx.dicts.allocate_with(16, b"Type42", sl, true, cs);

    // FontType = 42
    ctx.dicts.put(
        font_entity,
        DictKey::Name(ctx.name_cache.n_font_type),
        PsObject::int(42),
    );

    // FontName
    let font_name_id = ctx.names.intern(font_name.as_bytes());
    ctx.dicts.put(
        font_entity,
        DictKey::Name(ctx.name_cache.n_font_name),
        PsObject::name_lit(font_name_id),
    );

    // FontMatrix = [1 0 0 1 0 0] (identity — unitsPerEm handled by renderer)
    let fm_items = [
        PsObject::real(1.0),
        PsObject::real(0.0),
        PsObject::real(0.0),
        PsObject::real(1.0),
        PsObject::real(0.0),
        PsObject::real(0.0),
    ];
    let fm_entity = ctx.arrays.allocate_from_with(&fm_items, sl, true, cs);
    ctx.dicts.put(
        font_entity,
        DictKey::Name(ctx.name_cache.n_font_matrix),
        PsObject::array(fm_entity, 6),
    );

    // FontBBox
    let bb_items = [
        PsObject::real(bbox[0]),
        PsObject::real(bbox[1]),
        PsObject::real(bbox[2]),
        PsObject::real(bbox[3]),
    ];
    let bb_entity = ctx.arrays.allocate_from_with(&bb_items, sl, true, cs);
    ctx.dicts.put(
        font_entity,
        DictKey::Name(ctx.name_cache.n_font_bbox),
        PsObject::array(bb_entity, 4),
    );

    // PaintType = 0
    ctx.dicts.put(
        font_entity,
        DictKey::Name(ctx.name_cache.n_paint_type),
        PsObject::int(0),
    );

    // Encoding — 256-element array of name objects
    let enc_entity = ctx.arrays.allocate_with(256, sl, true, cs);
    for (i, name) in encoding.iter().enumerate() {
        let name_id = ctx.names.intern(name.as_bytes());
        ctx.arrays
            .set_element(enc_entity, i as u32, PsObject::name_lit(name_id));
    }
    ctx.dicts.put(
        font_entity,
        DictKey::Name(ctx.name_cache.n_encoding),
        PsObject::array(enc_entity, 256),
    );

    // CharStrings — dict mapping glyph name → GID (integer)
    let cs_entity =
        ctx.dicts
            .allocate_with(charstrings.len().max(10), b"CharStrings", sl, true, cs);
    for (name, &gid) in charstrings {
        let name_id = ctx.names.intern(name.as_bytes());
        ctx.dicts
            .put(cs_entity, DictKey::Name(name_id), PsObject::int(gid as i32));
    }
    ctx.dicts.put(
        font_entity,
        DictKey::Name(ctx.name_cache.n_char_strings),
        PsObject::dict(cs_entity),
    );

    // sfnts — single-element array containing the entire font file as a string
    let sfnts_entity = ctx.arrays.allocate_with(1, sl, true, cs);
    let str_entity = ctx.strings.allocate_from_with(font_data, sl, true, cs);
    ctx.arrays.set_element(
        sfnts_entity,
        0,
        PsObject::string(str_entity, font_data.len() as u32),
    );
    let sfnts_key = DictKey::Name(ctx.names.intern(b"sfnts"));
    ctx.dicts
        .put(font_entity, sfnts_key, PsObject::array(sfnts_entity, 1));

    // _unitsPerEm (internal, for renderer)
    let upem_key = DictKey::Name(ctx.names.intern(b"_unitsPerEm"));
    let upem = get_units_per_em(font_data);
    ctx.dicts
        .put(font_entity, upem_key, PsObject::int(upem as i32));

    // FID
    let fid = ctx.next_fid;
    ctx.next_fid += 1;
    ctx.dicts.put(
        font_entity,
        DictKey::Name(ctx.name_cache.n_fid),
        PsObject {
            value: PsValue::FontID(fid),
            flags: ObjFlags::literal(),
        },
    );

    // Register via definefont if available, otherwise direct FontDirectory insert
    let font_dict_obj = PsObject::dict(font_entity);

    let definefont_id = ctx.names.intern(b"definefont");
    let key = DictKey::Name(definefont_id);
    if let Some(op_obj) = ctx.dict_load(&key)
        && let PsValue::Operator(opcode) = op_obj.value
    {
        if ctx.o_stack.push(PsObject::name_lit(font_name_id)).is_err() {
            return false;
        }
        if ctx.o_stack.push(font_dict_obj).is_err() {
            return false;
        }
        let func = ctx.operators[opcode.0 as usize].func;
        if func(ctx).is_err() {
            return false;
        }
        let _ = ctx.o_stack.pop();
    } else {
        // Fallback: register directly in FontDirectory
        ctx.dicts.put(
            ctx.font_directory,
            DictKey::Name(font_name_id),
            font_dict_obj,
        );
    }

    true
}

/// Extract font bounding box from the head table.
fn get_font_bbox(font_data: &[u8]) -> (i16, i16, i16, i16) {
    if let Some((head_off, _)) = find_table(font_data, b"head")
        && head_off + 54 <= font_data.len()
    {
        let x_min = read_i16(font_data, head_off + 36);
        let y_min = read_i16(font_data, head_off + 38);
        let x_max = read_i16(font_data, head_off + 40);
        let y_max = read_i16(font_data, head_off + 42);
        return (x_min, y_min, x_max, y_max);
    }
    (0, 0, 1000, 1000)
}

/// Register a CFF font (reused from cff_ops pattern).
/// All allocations use global VM (PLRM: fonts always in global VM).
fn register_cff_font(
    ctx: &mut Context,
    cff_font: &cff_parser::CffFont,
    cff_str_entity: crate::object::EntityId,
    cff_data_len: u32,
) -> Result<(), crate::error::PsError> {
    let sl = ctx.save_stack.current_level();
    let cs = ctx.save_stack.last_save_id();

    let font_entity = ctx.dicts.allocate_with(16, b"CFF", sl, true, cs);

    // FontType = 2
    ctx.dicts.put(
        font_entity,
        DictKey::Name(ctx.name_cache.n_font_type),
        PsObject::int(2),
    );

    // FontName
    let font_name_id = ctx.names.intern(cff_font.name.as_bytes());
    ctx.dicts.put(
        font_entity,
        DictKey::Name(ctx.name_cache.n_font_name),
        PsObject::name_lit(font_name_id),
    );

    // FontMatrix
    let fm_entity = ctx.arrays.allocate_with(6, sl, true, cs);
    for (i, &val) in cff_font.font_matrix.iter().enumerate() {
        ctx.arrays
            .set_element(fm_entity, i as u32, PsObject::real(val));
    }
    ctx.dicts.put(
        font_entity,
        DictKey::Name(ctx.name_cache.n_font_matrix),
        PsObject::array(fm_entity, 6),
    );

    // FontBBox
    let bbox_entity = ctx.arrays.allocate_with(4, sl, true, cs);
    for (i, &val) in cff_font.font_bbox.iter().enumerate() {
        ctx.arrays
            .set_element(bbox_entity, i as u32, PsObject::real(val));
    }
    ctx.dicts.put(
        font_entity,
        DictKey::Name(ctx.name_cache.n_font_bbox),
        PsObject::array(bbox_entity, 4),
    );

    // Encoding
    let enc_entity = ctx.arrays.allocate_with(256, sl, true, cs);
    for code in 0..256u32 {
        let gid = cff_font.encoding[code as usize] as usize;
        let glyph_name = if gid < cff_font.charset.len() {
            &cff_font.charset[gid]
        } else {
            ".notdef"
        };
        let name_id = ctx.names.intern(glyph_name.as_bytes());
        ctx.arrays
            .set_element(enc_entity, code, PsObject::name_lit(name_id));
    }
    ctx.dicts.put(
        font_entity,
        DictKey::Name(ctx.name_cache.n_encoding),
        PsObject::array(enc_entity, 256),
    );

    // CharStrings
    let cs_entity = ctx.dicts.allocate_with(16, b"CFF", sl, true, cs);
    for (gid, cs_data) in cff_font.char_strings.iter().enumerate() {
        let str_entity = ctx.strings.allocate_from_with(cs_data, sl, true, cs);
        let cs_obj = PsObject::string(str_entity, cs_data.len() as u32);

        if gid < cff_font.charset.len() {
            let glyph_name = &cff_font.charset[gid];
            let name_id = ctx.names.intern(glyph_name.as_bytes());
            ctx.dicts.put(cs_entity, DictKey::Name(name_id), cs_obj);
        }

        if cff_font.is_cid {
            ctx.dicts.put(cs_entity, DictKey::Int(gid as i32), cs_obj);
        }
    }
    ctx.dicts.put(
        font_entity,
        DictKey::Name(ctx.name_cache.n_char_strings),
        PsObject::dict(cs_entity),
    );

    // Private dictionary
    let priv_entity = ctx.dicts.allocate_with(16, b"CFF", sl, true, cs);
    ctx.dicts.put(
        priv_entity,
        DictKey::Name(ctx.names.intern(b"defaultWidthX")),
        PsObject::real(cff_font.default_width_x),
    );
    ctx.dicts.put(
        priv_entity,
        DictKey::Name(ctx.names.intern(b"nominalWidthX")),
        PsObject::real(cff_font.nominal_width_x),
    );

    if !cff_font.local_subrs.is_empty() {
        let subrs_len = cff_font.local_subrs.len() as u32;
        let subrs_entity = ctx.arrays.allocate_with(subrs_len as usize, sl, true, cs);
        for (i, subr_data) in cff_font.local_subrs.iter().enumerate() {
            let str_entity = ctx.strings.allocate_from_with(subr_data, sl, true, cs);
            ctx.arrays.set_element(
                subrs_entity,
                i as u32,
                PsObject::string(str_entity, subr_data.len() as u32),
            );
        }
        ctx.dicts.put(
            priv_entity,
            DictKey::Name(ctx.name_cache.n_subrs),
            PsObject {
                value: PsValue::Array {
                    entity: subrs_entity,
                    start: 0,
                    len: subrs_len,
                },
                flags: ObjFlags::literal_composite(),
            },
        );
    }
    ctx.dicts.put(
        font_entity,
        DictKey::Name(ctx.name_cache.n_private),
        PsObject::dict(priv_entity),
    );

    // Global subroutines
    if !cff_font.global_subrs.is_empty() {
        let gs_key = DictKey::Name(ctx.names.intern(b"_cff_global_subrs"));
        let gs_len = cff_font.global_subrs.len() as u32;
        let gs_entity = ctx.arrays.allocate_with(gs_len as usize, sl, true, cs);
        for (i, subr_data) in cff_font.global_subrs.iter().enumerate() {
            let str_entity = ctx.strings.allocate_from_with(subr_data, sl, true, cs);
            ctx.arrays.set_element(
                gs_entity,
                i as u32,
                PsObject::string(str_entity, subr_data.len() as u32),
            );
        }
        ctx.dicts.put(
            font_entity,
            gs_key,
            PsObject {
                value: PsValue::Array {
                    entity: gs_entity,
                    start: 0,
                    len: gs_len,
                },
                flags: ObjFlags::literal_composite(),
            },
        );
    }

    // Store raw CFF binary for PDF embedding
    let cff_data_key = DictKey::Name(ctx.names.intern(b"_CFFData"));
    ctx.dicts.put(
        font_entity,
        cff_data_key,
        PsObject::string(cff_str_entity, cff_data_len),
    );

    // FID
    let fid = ctx.next_fid;
    ctx.next_fid += 1;
    ctx.dicts.put(
        font_entity,
        DictKey::Name(ctx.name_cache.n_fid),
        PsObject {
            value: PsValue::FontID(fid),
            flags: ObjFlags::literal(),
        },
    );

    // Register via definefont if available, otherwise direct FontDirectory insert
    let font_dict_obj = PsObject::dict(font_entity);

    let definefont_id = ctx.names.intern(b"definefont");
    let key = DictKey::Name(definefont_id);
    if let Some(op_obj) = ctx.dict_load(&key)
        && let PsValue::Operator(opcode) = op_obj.value
    {
        ctx.o_stack.push(PsObject::name_lit(font_name_id))?;
        ctx.o_stack.push(font_dict_obj)?;
        let func = ctx.operators[opcode.0 as usize].func;
        func(ctx)?;
        let _ = ctx.o_stack.pop();
    } else {
        ctx.dicts.put(
            ctx.font_directory,
            DictKey::Name(font_name_id),
            font_dict_obj,
        );
    }

    Ok(())
}

/// Detect whether a font file contains TrueType or CFF outlines.
pub fn is_cff_font(data: &[u8]) -> bool {
    data.len() >= 4 && &data[0..4] == b"OTTO"
}

/// Load a binary font file (TTF or OTF), auto-detecting the format.
pub fn load_binary_font(ctx: &mut Context, path: &Path) -> bool {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(_) => return false,
    };

    if data.len() < 4 {
        return false;
    }

    if is_cff_font(&data) {
        // OTF with CFF outlines
        load_otf_cff(ctx, path)
    } else {
        // TrueType outlines (TTF or OTF with glyf table)
        load_ttf_from_data(ctx, &data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_cff_font() {
        assert!(is_cff_font(b"OTTO\x00\x00\x00\x00"));
        assert!(!is_cff_font(b"\x00\x01\x00\x00"));
        assert!(!is_cff_font(b"tru"));
    }

    #[test]
    fn test_load_system_ttf() {
        // Try to load a real system TrueType font
        let test_fonts = [
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
        ];
        for path_str in &test_fonts {
            let path = Path::new(path_str);
            if path.exists() {
                let mut ctx = Context::new();
                let ok = load_ttf(&mut ctx, path);
                assert!(ok, "Failed to load {}", path_str);

                // Verify font was registered in FontDirectory
                let name = extract_ps_name_from_name_table(&std::fs::read(path).unwrap());
                if let Some(font_name) = name {
                    let name_id = ctx.names.intern(font_name.as_bytes());
                    let key = DictKey::Name(name_id);
                    assert!(
                        ctx.dicts.get(ctx.font_directory, &key).is_some(),
                        "Font '{}' not in FontDirectory",
                        font_name
                    );
                }
                return;
            }
        }
        eprintln!("Skipping TTF loading test — no test font found");
    }

    #[test]
    fn test_load_system_otf() {
        // Try to load a real OTF with CFF
        let test_fonts = ["/usr/share/fonts/opentype/urw-base35/C059-Italic.otf"];
        for path_str in &test_fonts {
            let path = Path::new(path_str);
            if path.exists() {
                let mut ctx = Context::new();
                let ok = load_otf_cff(&mut ctx, path);
                assert!(ok, "Failed to load CFF from {}", path_str);
                return;
            }
        }
        eprintln!("Skipping OTF loading test — no test font found");
    }
}
