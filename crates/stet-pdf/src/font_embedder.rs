// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Type 1 font embedding for PDF output.
//!
//! Reconstructs Type 1 font programs from PostScript font dicts and embeds
//! them as PDF font resources with proper Widths, FontDescriptor, and ToUnicode.
//!
//! NOTE: These functions require a Context reference to access font dicts.
//! Currently unused because the PDF device doesn't have Context access.
//! Will be wired up when we add Context-aware font embedding.

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};

use stet_core::context::Context;
use stet_core::dict::DictKey;
use stet_core::object::{EntityId, PsValue};

use crate::font_tracker::FontUsage;
use crate::pdf_objects::PdfObj;
use crate::pdf_writer::PdfWriter;
use crate::unicode_mapping;

/// eexec encryption constants (per Adobe Type 1 spec).
const EEXEC_R: u16 = 55665;
const EEXEC_C1: u16 = 52845;
const EEXEC_C2: u16 = 22719;

/// Encrypt data using Adobe eexec encryption.
fn eexec_encrypt(plaintext: &[u8]) -> Vec<u8> {
    // 4-byte random prefix (all zeros, like PostForge)
    let prefix = [0u8; 4];
    let mut r: u16 = EEXEC_R;
    let mut result = Vec::with_capacity(plaintext.len() + 4);

    for &plain_byte in prefix.iter().chain(plaintext.iter()) {
        let cipher_byte = plain_byte ^ (r >> 8) as u8;
        result.push(cipher_byte);
        r = (cipher_byte as u16)
            .wrapping_add(r)
            .wrapping_mul(EEXEC_C1)
            .wrapping_add(EEXEC_C2);
    }
    result
}

/// Charstring decryption constants.
const CS_R: u16 = 4330;

/// Decrypt a Type 1 charstring.
fn decrypt_charstring(data: &[u8], len_iv: usize) -> Vec<u8> {
    let mut r: u16 = CS_R;
    let mut result = Vec::with_capacity(data.len());
    for &cipher in data {
        let plain = cipher ^ (r >> 8) as u8;
        result.push(plain);
        r = (cipher as u16)
            .wrapping_add(r)
            .wrapping_mul(EEXEC_C1)
            .wrapping_add(EEXEC_C2);
    }
    if result.len() > len_iv {
        result[len_iv..].to_vec()
    } else {
        Vec::new()
    }
}

/// Extract width from a decrypted Type 1 charstring.
/// Parses until hsbw (opcode 13) or sbw (12,7) to get the advance width.
fn extract_charstring_width(decrypted: &[u8], subrs: &[Vec<u8>]) -> Option<f64> {
    let mut stack: Vec<f64> = Vec::new();
    let mut i = 0;
    while i < decrypted.len() {
        let b = decrypted[i];
        i += 1;
        match b {
            13 => {
                // hsbw: sbx wx → width is top of stack
                return stack.last().copied();
            }
            12 => {
                if i < decrypted.len() {
                    let b2 = decrypted[i];
                    i += 1;
                    if b2 == 7 {
                        // sbw: sbx sby wx wy → width is stack[-2]
                        if stack.len() >= 2 {
                            return Some(stack[stack.len() - 2]);
                        }
                    }
                }
            }
            10 => {
                // callsubr: index on stack → follow into subr
                if let Some(idx) = stack.pop() {
                    let idx = idx as usize;
                    if idx < subrs.len() {
                        if let Some(w) = extract_charstring_width(&subrs[idx], subrs) {
                            return Some(w);
                        }
                    }
                }
            }
            11 | 14 => {
                // return / endchar — stop
                return None;
            }
            // Number encoding
            32..=246 => stack.push((b as i32 - 139) as f64),
            247..=250 => {
                if i < decrypted.len() {
                    let b2 = decrypted[i] as i32;
                    i += 1;
                    stack.push(((b as i32 - 247) * 256 + b2 + 108) as f64);
                }
            }
            251..=254 => {
                if i < decrypted.len() {
                    let b2 = decrypted[i] as i32;
                    i += 1;
                    stack.push((-(b as i32 - 251) * 256 - b2 - 108) as f64);
                }
            }
            255 => {
                if i + 3 < decrypted.len() {
                    let val = i32::from_be_bytes([
                        decrypted[i],
                        decrypted[i + 1],
                        decrypted[i + 2],
                        decrypted[i + 3],
                    ]);
                    i += 4;
                    stack.push(val as f64);
                }
            }
            _ => {
                // Other opcodes — clear stack
                stack.clear();
            }
        }
    }
    None
}

/// Build a PDF font resource for a tracked font and return its object reference.
///
/// Returns `None` for Type 3 fonts (not embeddable) or if the font dict is invalid.
pub fn build_font_resource(
    writer: &mut PdfWriter,
    usage: &FontUsage,
    ctx: &Context,
) -> Option<u32> {
    match usage.font_type {
        1 => build_type1_font(writer, usage, ctx),
        2 => build_type1_font(writer, usage, ctx), // CFF treated same as Type 1 for PDF
        3 => None, // Type 3 can't be embedded
        0 | 42 => build_type1_font(writer, usage, ctx), // Simplified for now
        _ => None,
    }
}

/// Build a Type 1 font resource.
fn build_type1_font(
    writer: &mut PdfWriter,
    usage: &FontUsage,
    ctx: &Context,
) -> Option<u32> {
    let font_entity = usage.font_entity;
    let font_name_str = String::from_utf8_lossy(&usage.font_name);

    // Get encoding array
    let encoding_entity = ctx
        .dicts
        .get(font_entity, &DictKey::Name(ctx.name_cache.n_encoding))
        .and_then(|obj| match obj.value {
            PsValue::Array { entity, .. } => Some(entity),
            _ => None,
        });

    // Get CharStrings dict
    let charstrings_entity = ctx
        .dicts
        .get(font_entity, &DictKey::Name(ctx.name_cache.n_char_strings))
        .and_then(|obj| match obj.value {
            PsValue::Dict(e) => Some(e),
            _ => None,
        });

    // Get Private dict for lenIV and Subrs
    let private_entity = ctx
        .dicts
        .get(font_entity, &DictKey::Name(ctx.name_cache.n_private))
        .and_then(|obj| match obj.value {
            PsValue::Dict(e) => Some(e),
            _ => None,
        });

    let len_iv = private_entity
        .and_then(|pe| {
            ctx.dicts
                .get(pe, &DictKey::Name(ctx.name_cache.n_len_iv))
                .and_then(|v| v.as_i32())
        })
        .unwrap_or(4) as usize;

    // Pre-decrypt Subrs for width extraction
    let subrs = get_decrypted_subrs(ctx, private_entity, len_iv);

    // Compute widths for used character codes
    let mut widths: HashMap<u16, i32> = HashMap::new();
    let mut first_char: u16 = 255;
    let mut last_char: u16 = 0;

    if let (Some(enc_entity), Some(cs_entity)) = (encoding_entity, charstrings_entity) {
        for &code in &usage.used_codes {
            if code > 255 {
                continue;
            }
            let glyph_name_obj = ctx.arrays.get_element(enc_entity, code as u32);
            let glyph_name_id = match glyph_name_obj.value {
                PsValue::Name(id) => id,
                _ => continue,
            };

            let cs_obj = match ctx.dicts.get(cs_entity, &DictKey::Name(glyph_name_id)) {
                Some(obj) => obj,
                None => continue,
            };

            let (cs_ent, cs_start, cs_len) = match cs_obj.value {
                PsValue::String { entity, start, len } => (entity, start, len),
                _ => continue,
            };

            let cs_bytes = ctx.strings.get(cs_ent, cs_start, cs_len);
            let decrypted = decrypt_charstring(cs_bytes, len_iv);
            if let Some(w) = extract_charstring_width(&decrypted, &subrs) {
                widths.insert(code, w as i32);
                first_char = first_char.min(code);
                last_char = last_char.max(code);
            }
        }
    }

    if widths.is_empty() {
        first_char = 0;
        last_char = 0;
    }

    // Build Widths array
    let widths_array: Vec<PdfObj> = (first_char..=last_char)
        .map(|code| PdfObj::Int(*widths.get(&code).unwrap_or(&0) as i64))
        .collect();

    // Build ToUnicode CMap
    let tounicode_map = build_tounicode_map(ctx, encoding_entity, &usage.used_codes);
    let tounicode_ref = if !tounicode_map.is_empty() {
        let cmap_data = generate_tounicode_cmap(&tounicode_map, &font_name_str);
        Some(writer.add_stream(Vec::new(), &cmap_data, true))
    } else {
        None
    };

    // Build FontDescriptor
    let bbox = get_font_bbox(ctx, font_entity);
    let flags = compute_font_flags(ctx, font_entity);
    let descriptor_ref = build_font_descriptor(writer, &usage.font_name, &bbox, flags);

    // Build Font dict
    let mut font_entries: Vec<(Vec<u8>, PdfObj)> = vec![
        (b"Type".to_vec(), PdfObj::name("Font")),
        (b"Subtype".to_vec(), PdfObj::name("Type1")),
        (
            b"BaseFont".to_vec(),
            PdfObj::Name(usage.font_name.clone()),
        ),
        (b"FirstChar".to_vec(), PdfObj::Int(first_char as i64)),
        (b"LastChar".to_vec(), PdfObj::Int(last_char as i64)),
        (b"Widths".to_vec(), PdfObj::Array(widths_array)),
        (b"FontDescriptor".to_vec(), PdfObj::Ref(descriptor_ref)),
    ];

    if let Some(tu_ref) = tounicode_ref {
        font_entries.push((b"ToUnicode".to_vec(), PdfObj::Ref(tu_ref)));
    }

    Some(writer.add_object(&PdfObj::Dict(font_entries)))
}

/// Get pre-decrypted Subrs from Private dict.
fn get_decrypted_subrs(
    ctx: &Context,
    private_entity: Option<EntityId>,
    len_iv: usize,
) -> Vec<Vec<u8>> {
    let Some(pe) = private_entity else {
        return Vec::new();
    };

    let subrs_obj = ctx.dicts.get(pe, &DictKey::Name(ctx.name_cache.n_subrs));
    let Some(obj) = subrs_obj else {
        return Vec::new();
    };

    let (entity, start, len) = match obj.value {
        PsValue::Array { entity, start, len } => (entity, start, len),
        _ => return Vec::new(),
    };

    let elems = ctx.arrays.get(entity, start, len);
    elems
        .iter()
        .map(|o| match o.value {
            PsValue::String { entity, start, len } => {
                let raw = ctx.strings.get(entity, start, len);
                decrypt_charstring(raw, len_iv)
            }
            _ => Vec::new(),
        })
        .collect()
}

/// Build the ToUnicode map: char_code → Unicode string.
fn build_tounicode_map(
    ctx: &Context,
    encoding_entity: Option<EntityId>,
    used_codes: &HashSet<u16>,
) -> HashMap<u16, u16> {
    let mut map = HashMap::new();
    let Some(enc_entity) = encoding_entity else {
        return map;
    };

    for &code in used_codes {
        if code > 255 {
            continue;
        }
        let glyph_name_obj = ctx.arrays.get_element(enc_entity, code as u32);
        if let PsValue::Name(id) = glyph_name_obj.value {
            let name_bytes = ctx.names.get_bytes(id);
            if let Ok(name_str) = std::str::from_utf8(name_bytes) {
                if name_str != ".notdef" {
                    if let Some(unicode) = unicode_mapping::glyph_name_to_unicode(name_str) {
                        map.insert(code, unicode);
                    }
                }
            }
        }
    }
    map
}

/// Generate a ToUnicode CMap stream.
fn generate_tounicode_cmap(map: &HashMap<u16, u16>, font_name: &str) -> Vec<u8> {
    let mut lines: Vec<Vec<u8>> = Vec::new();

    lines.push(b"/CIDInit /ProcSet findresource begin".to_vec());
    lines.push(b"12 dict begin".to_vec());
    lines.push(b"begincmap".to_vec());
    lines.push(b"/CIDSystemInfo <<".to_vec());
    lines.push(b"  /Registry (Adobe)".to_vec());
    lines.push(b"  /Ordering (UCS)".to_vec());
    lines.push(b"  /Supplement 0".to_vec());
    lines.push(b">> def".to_vec());
    lines.push(format!("/CMapName /{}-UCS def", font_name).into_bytes());
    lines.push(b"/CMapType 2 def".to_vec());
    lines.push(b"1 begincodespacerange".to_vec());
    lines.push(b"<00> <FF>".to_vec());
    lines.push(b"endcodespacerange".to_vec());

    let mut sorted: Vec<_> = map.iter().collect();
    sorted.sort_by_key(|&(&code, _)| code);

    // Emit in batches of 100
    for chunk in sorted.chunks(100) {
        lines.push(format!("{} beginbfchar", chunk.len()).into_bytes());
        for &(&code, &unicode) in chunk {
            lines.push(format!("<{:02X}> <{:04X}>", code, unicode).into_bytes());
        }
        lines.push(b"endbfchar".to_vec());
    }

    lines.push(b"endcmap".to_vec());
    lines.push(b"CMapName currentdict /CMap defineresource pop".to_vec());
    lines.push(b"end".to_vec());
    lines.push(b"end".to_vec());

    lines.join(&b'\n')
}

/// Read FontBBox from font dict.
fn get_font_bbox(ctx: &Context, font_entity: EntityId) -> [f64; 4] {
    let bbox_name = ctx.names.find(b"FontBBox");
    if let Some(name_id) = bbox_name {
        if let Some(obj) = ctx.dicts.get(font_entity, &DictKey::Name(name_id)) {
            if let PsValue::Array { entity, start, len } = obj.value {
                if len >= 4 {
                    let elems = ctx.arrays.get(entity, start, len);
                    return [
                        elems[0].as_f64().unwrap_or(0.0),
                        elems[1].as_f64().unwrap_or(0.0),
                        elems[2].as_f64().unwrap_or(0.0),
                        elems[3].as_f64().unwrap_or(0.0),
                    ];
                }
            }
        }
    }
    [0.0, 0.0, 1000.0, 1000.0]
}

/// Compute PDF font flags.
fn compute_font_flags(_ctx: &Context, _font_entity: EntityId) -> u32 {
    // Bit 2: Serif (assume serif for now)
    // Bit 6: Nonsymbolic
    0x0020 // Nonsymbolic
}

/// Build a PDF FontDescriptor object.
fn build_font_descriptor(
    writer: &mut PdfWriter,
    font_name: &[u8],
    bbox: &[f64; 4],
    flags: u32,
) -> u32 {
    let entries = vec![
        (b"Type".to_vec(), PdfObj::name("FontDescriptor")),
        (b"FontName".to_vec(), PdfObj::Name(font_name.to_vec())),
        (b"Flags".to_vec(), PdfObj::Int(flags as i64)),
        (
            b"FontBBox".to_vec(),
            PdfObj::Array(bbox.iter().map(|&v| PdfObj::Real(v)).collect()),
        ),
        // Approximate values — good enough for most fonts
        (b"Ascent".to_vec(), PdfObj::Real(bbox[3].max(800.0))),
        (b"Descent".to_vec(), PdfObj::Real(bbox[1].min(-200.0))),
        (b"StemV".to_vec(), PdfObj::Int(80)),
        (b"ItalicAngle".to_vec(), PdfObj::Int(0)),
        (b"CapHeight".to_vec(), PdfObj::Int(700)),
    ];
    writer.add_object(&PdfObj::Dict(entries))
}
