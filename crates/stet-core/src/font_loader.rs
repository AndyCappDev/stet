// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Font loading pipeline.
//!
//! Loads Type 1 font files from disk, parses them, and creates PostScript
//! font dictionary objects. Includes font name substitution table.

use crate::context::Context;
use crate::dict::DictKey;
use crate::object::{PsObject, PsValue};

/// Standard PostScript font name → URW replacement filename.
pub const FONT_SUBSTITUTIONS: &[(&str, &str)] = &[
    // Times family → Nimbus Roman
    ("Times-Roman", "NimbusRoman-Regular"),
    ("Times-Bold", "NimbusRoman-Bold"),
    ("Times-Italic", "NimbusRoman-Italic"),
    ("Times-BoldItalic", "NimbusRoman-BoldItalic"),
    // Helvetica family → Nimbus Sans
    ("Helvetica", "NimbusSans-Regular"),
    ("Helvetica-Bold", "NimbusSans-Bold"),
    ("Helvetica-Oblique", "NimbusSans-Italic"),
    ("Helvetica-BoldOblique", "NimbusSans-BoldItalic"),
    // Helvetica Narrow → Nimbus Sans Narrow
    ("Helvetica-Narrow", "NimbusSansNarrow-Regular"),
    ("Helvetica-Narrow-Bold", "NimbusSansNarrow-Bold"),
    ("Helvetica-Narrow-Oblique", "NimbusSansNarrow-Oblique"),
    (
        "Helvetica-Narrow-BoldOblique",
        "NimbusSansNarrow-BoldOblique",
    ),
    // Courier family → Nimbus Mono PS
    ("Courier", "NimbusMonoPS-Regular"),
    ("Courier-Bold", "NimbusMonoPS-Bold"),
    ("Courier-Oblique", "NimbusMonoPS-Italic"),
    ("Courier-BoldOblique", "NimbusMonoPS-BoldItalic"),
    // Symbol fonts
    ("Symbol", "StandardSymbolsPS"),
    ("ZapfDingbats", "D050000L"),
    // Palatino → P052
    ("Palatino-Roman", "P052-Roman"),
    ("Palatino-Bold", "P052-Bold"),
    ("Palatino-Italic", "P052-Italic"),
    ("Palatino-BoldItalic", "P052-BoldItalic"),
    // New Century Schoolbook → C059
    ("NewCenturySchlbk-Roman", "C059-Roman"),
    ("NewCenturySchlbk-Bold", "C059-Bold"),
    ("NewCenturySchlbk-Italic", "C059-Italic"),
    ("NewCenturySchlbk-BoldItalic", "C059-BdIta"),
    // Bookman → URWBookman
    ("Bookman-Light", "URWBookman-Light"),
    ("Bookman-LightItalic", "URWBookman-LightItalic"),
    ("Bookman-Demi", "URWBookman-Demi"),
    ("Bookman-DemiItalic", "URWBookman-DemiItalic"),
    // AvantGarde → URWGothic
    ("AvantGarde-Book", "URWGothic-Book"),
    ("AvantGarde-BookOblique", "URWGothic-BookOblique"),
    ("AvantGarde-Demi", "URWGothic-Demi"),
    ("AvantGarde-DemiOblique", "URWGothic-DemiOblique"),
    // ZapfChancery
    ("ZapfChancery-MediumItalic", "Z003-MediumItalic"),
];

/// Look up a font substitution: "Helvetica" → "NimbusSans-Regular".
pub fn find_substitution(name: &str) -> Option<&'static str> {
    for &(ps_name, urw_name) in FONT_SUBSTITUTIONS {
        if ps_name == name {
            return Some(urw_name);
        }
    }
    None
}

/// Try to load a font by name, using the substitution table.
/// Checks embedded files first (for WASM), then falls back to disk.
pub fn load_font_file(
    font_name: &str,
    resource_path: &str,
    files: &crate::file_store::FileStore,
) -> Option<Vec<u8>> {
    // Try embedded files first (for WASM builds)
    let direct_path = format!("{}/{}.t1", resource_path, font_name);
    if let Some(data) = files.get_embedded_file(&direct_path) {
        return Some(data.to_vec());
    }
    if let Some(urw_name) = find_substitution(font_name) {
        let sub_path = format!("{}/{}.t1", resource_path, urw_name);
        if let Some(data) = files.get_embedded_file(&sub_path) {
            return Some(data.to_vec());
        }
    }

    // Fall back to disk I/O
    if let Ok(data) = std::fs::read(&direct_path) {
        return Some(data);
    }
    if let Some(urw_name) = find_substitution(font_name) {
        let sub_path = format!("{}/{}.t1", resource_path, urw_name);
        if let Ok(data) = std::fs::read(&sub_path) {
            return Some(data);
        }
    }

    None
}

/// Load a Type 1 font from data and register it as a PostScript dict in the context.
/// Returns the font dict PsObject.
pub fn load_type1_font(ctx: &mut Context, font_data: &[u8]) -> Result<PsObject, String> {
    let font = crate::type1_parser::parse_type1(font_data)?;

    let save_level = ctx.save_stack.current_level();
    let global = ctx.vm_alloc_mode;

    // Create the font dictionary (respects current VM allocation mode)
    let font_dict = ctx
        .dicts
        .allocate_with(30, font.font_name.as_bytes(), save_level, global);

    // /FontName → name object
    let name_id = ctx.names.intern(font.font_name.as_bytes());
    ctx.dicts.put(
        font_dict,
        DictKey::Name(ctx.name_cache.n_font_name),
        PsObject::name_lit(name_id),
    );

    // /FontType → Int(1)
    ctx.dicts.put(
        font_dict,
        DictKey::Name(ctx.name_cache.n_font_type),
        PsObject::int(1),
    );

    // /FontMatrix → array of 6 reals
    let fm_items: Vec<PsObject> = font
        .font_matrix
        .iter()
        .map(|&v| PsObject::real(v))
        .collect();
    let fm_entity = ctx.arrays.allocate_from_with(&fm_items, save_level, global);
    ctx.dicts.put(
        font_dict,
        DictKey::Name(ctx.name_cache.n_font_matrix),
        PsObject::array(fm_entity, 6),
    );

    // /FontBBox → array of 4 reals
    let bb_items: Vec<PsObject> = font.font_bbox.iter().map(|&v| PsObject::real(v)).collect();
    let bb_entity = ctx.arrays.allocate_from_with(&bb_items, save_level, global);
    ctx.dicts.put(
        font_dict,
        DictKey::Name(ctx.name_cache.n_font_bbox),
        PsObject::array(bb_entity, 4),
    );

    // /PaintType → Int
    ctx.dicts.put(
        font_dict,
        DictKey::Name(ctx.name_cache.n_paint_type),
        PsObject::int(font.paint_type),
    );

    // /Encoding → array of 256 name objects
    let enc_items: Vec<PsObject> = font
        .encoding
        .iter()
        .map(|name| {
            let id = ctx.names.intern(name.as_bytes());
            PsObject::name_lit(id)
        })
        .collect();
    let enc_entity = ctx
        .arrays
        .allocate_from_with(&enc_items, save_level, global);
    ctx.dicts.put(
        font_dict,
        DictKey::Name(ctx.name_cache.n_encoding),
        PsObject::array(enc_entity, 256),
    );

    // /CharStrings → dict mapping glyph names to string objects (encrypted bytes)
    let cs_dict = ctx.dicts.allocate_with(
        font.charstrings.len().max(10),
        b"CharStrings",
        save_level,
        global,
    );
    for (glyph_name, bytes) in &font.charstrings {
        let glyph_name_id = ctx.names.intern(glyph_name.as_bytes());
        let str_entity = ctx.strings.allocate_from_with(bytes, save_level, global);
        let str_obj = PsObject::string(str_entity, bytes.len() as u32);
        ctx.dicts
            .put(cs_dict, DictKey::Name(glyph_name_id), str_obj);
    }
    ctx.dicts.put(
        font_dict,
        DictKey::Name(ctx.name_cache.n_char_strings),
        PsObject::dict(cs_dict),
    );

    // /Private → dict with lenIV and Subrs (standard Type 1 structure)
    let priv_dict = ctx.dicts.allocate_with(10, b"Private", save_level, global);
    ctx.dicts.put(
        priv_dict,
        DictKey::Name(ctx.name_cache.n_len_iv),
        PsObject::int(font.len_iv as i32),
    );

    // /Subrs → array of string objects (encrypted subroutine bytes) inside Private
    let subr_items: Vec<PsObject> = font
        .subrs
        .iter()
        .map(|bytes| {
            let entity = ctx.strings.allocate_from_with(bytes, save_level, global);
            PsObject::string(entity, bytes.len() as u32)
        })
        .collect();
    let subrs_entity = ctx
        .arrays
        .allocate_from_with(&subr_items, save_level, global);
    ctx.dicts.put(
        priv_dict,
        DictKey::Name(ctx.name_cache.n_subrs),
        PsObject::array(subrs_entity, font.subrs.len() as u32),
    );

    ctx.dicts.put(
        font_dict,
        DictKey::Name(ctx.name_cache.n_private),
        PsObject::dict(priv_dict),
    );

    // /FID → unique font ID
    let fid = ctx.next_fid;
    ctx.next_fid += 1;
    ctx.dicts.put(
        font_dict,
        DictKey::Name(ctx.name_cache.n_fid),
        PsObject::int(fid),
    );

    // Register in FontDirectory under the font name
    let font_obj = PsObject::dict(font_dict);
    ctx.dicts
        .put(ctx.font_directory, DictKey::Name(name_id), font_obj);

    Ok(font_obj)
}

/// Try to find a font by name: check FontDirectory first, then load from disk.
pub fn find_font(ctx: &mut Context, name_bytes: &[u8]) -> Result<PsObject, String> {
    let name_id = ctx.names.intern(name_bytes);
    let key = DictKey::Name(name_id);

    // Check FontDirectory
    if let Some(font_obj) = ctx.dicts.get(ctx.font_directory, &key) {
        return Ok(font_obj);
    }

    // Try to load from disk
    let font_name = String::from_utf8_lossy(name_bytes);
    let resource_path = ctx
        .font_resource_path
        .clone()
        .ok_or_else(|| format!("Font '{}' not found and no resource path set", font_name))?;

    let font_data = load_font_file(&font_name, &resource_path, &ctx.files)
        .ok_or_else(|| format!("Font '{}' not found in {}", font_name, resource_path))?;

    let font_obj = load_type1_font(ctx, &font_data)?;

    // The font may have been registered under a different name (the actual font name
    // from the .t1 file). We need to also register under the requested name.
    let actual_name = get_font_name(ctx, font_obj);
    if actual_name.as_deref() != Some(&*font_name) {
        ctx.dicts.put(ctx.font_directory, key, font_obj);
    }

    Ok(font_obj)
}

/// Extract /FontName from a font dict object.
fn get_font_name(ctx: &Context, font_obj: PsObject) -> Option<String> {
    if let PsValue::Dict(entity) = font_obj.value
        && let Some(name_obj) = ctx
            .dicts
            .get(entity, &DictKey::Name(ctx.name_cache.n_font_name))
        && let PsValue::Name(id) = name_obj.value
    {
        return Some(String::from_utf8_lossy(ctx.names.get_bytes(id)).to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_font_substitution() {
        assert_eq!(find_substitution("Helvetica"), Some("NimbusSans-Regular"));
        assert_eq!(
            find_substitution("Times-Roman"),
            Some("NimbusRoman-Regular")
        );
        assert_eq!(find_substitution("Courier"), Some("NimbusMonoPS-Regular"));
        assert_eq!(find_substitution("Symbol"), Some("StandardSymbolsPS"));
        assert_eq!(find_substitution("NoSuchFont"), None);
    }

    #[test]
    fn test_load_real_font() {
        let font_path = std::path::Path::new(
            "/home/scott/Projects/postforge/postforge/resources/Font/NimbusSans-Regular.t1",
        );
        if !font_path.exists() {
            eprintln!("Skipping test — font file not found");
            return;
        }

        let mut ctx = Context::new();
        let font_data = std::fs::read(font_path).unwrap();
        let font_obj = load_type1_font(&mut ctx, &font_data).unwrap();

        // Verify it's a dict
        assert!(matches!(font_obj.value, PsValue::Dict(_)));

        // Verify FontName
        if let PsValue::Dict(entity) = font_obj.value {
            let name_obj = ctx
                .dicts
                .get(entity, &DictKey::Name(ctx.name_cache.n_font_name))
                .unwrap();
            if let PsValue::Name(id) = name_obj.value {
                assert_eq!(ctx.names.get_bytes(id), b"NimbusSans-Regular");
            }

            // Verify FontType = 1
            let type_obj = ctx
                .dicts
                .get(entity, &DictKey::Name(ctx.name_cache.n_font_type))
                .unwrap();
            assert_eq!(type_obj.as_i32(), Some(1));

            // Verify CharStrings is a dict with entries
            let cs_obj = ctx
                .dicts
                .get(entity, &DictKey::Name(ctx.name_cache.n_char_strings))
                .unwrap();
            if let PsValue::Dict(cs_entity) = cs_obj.value {
                assert!(ctx.dicts.length(cs_entity) > 100);
            } else {
                panic!("CharStrings should be a dict");
            }
        }

        // Verify it's registered in FontDirectory
        let name_id = ctx.names.intern(b"NimbusSans-Regular");
        assert!(
            ctx.dicts
                .get(ctx.font_directory, &DictKey::Name(name_id))
                .is_some()
        );
    }
}
