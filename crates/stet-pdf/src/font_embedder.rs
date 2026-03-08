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


use std::collections::{HashMap, HashSet};
use std::io::Write as IoWrite;

use stet_core::context::Context;
use stet_core::dict::DictKey;
use stet_core::encoding::STANDARD_ENCODING;
use stet_core::object::{EntityId, PsValue};
use stet_core::type2_charstring;

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

/// Charstring encryption/decryption constants.
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

/// Encrypt a charstring using the Type 1 charstring cipher (R=4330).
/// Prepends `len_iv` zero bytes as the random prefix.
fn charstring_encrypt(plaintext: &[u8], len_iv: usize) -> Vec<u8> {
    let prefix = vec![0u8; len_iv];
    let mut r: u16 = CS_R;
    let mut result = Vec::with_capacity(plaintext.len() + len_iv);

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

/// Find seac dependencies in a decrypted charstring.
/// Returns glyph names (from StandardEncoding) referenced by seac opcodes.
fn find_seac_deps(decrypted: &[u8]) -> Vec<String> {
    let mut deps = Vec::new();
    let mut stack: Vec<f64> = Vec::new();
    let mut i = 0;
    while i < decrypted.len() {
        let b = decrypted[i];
        i += 1;
        match b {
            12 => {
                if i < decrypted.len() {
                    let b2 = decrypted[i];
                    i += 1;
                    if b2 == 6 {
                        // seac: asb adx ady bchar achar
                        if stack.len() >= 5 {
                            let achar = stack[stack.len() - 1] as u8;
                            let bchar = stack[stack.len() - 2] as u8;
                            let bname = STANDARD_ENCODING[bchar as usize];
                            let aname = STANDARD_ENCODING[achar as usize];
                            if !bname.is_empty() && bname != ".notdef" {
                                deps.push(bname.to_string());
                            }
                            if !aname.is_empty() && aname != ".notdef" {
                                deps.push(aname.to_string());
                            }
                        }
                    }
                    stack.clear();
                }
            }
            11 | 14 => return deps, // return / endchar
            13 => stack.clear(),    // hsbw
            10 => {
                stack.pop();
            } // callsubr
            // Number encoding (same as extract_charstring_width)
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
            _ => stack.clear(),
        }
    }
    deps
}

/// Build a Type 1 font file (three-section binary) for PDF embedding.
///
/// Returns (font_file_data, length1, length2, length3) where:
/// - length1 = cleartext section length
/// - length2 = eexec encrypted section length
/// - length3 = footer length (always 522)
#[allow(clippy::too_many_arguments)]
fn build_type1_font_file(
    ctx: &Context,
    font_entity: EntityId,
    usage: &FontUsage,
    charstrings_entity: EntityId,
    encoding_entity: Option<EntityId>,
    private_entity: Option<EntityId>,
    len_iv: usize,
    subrs: &[Vec<u8>],
) -> Option<(Vec<u8>, usize, usize, usize)> {
    let font_name_str = String::from_utf8_lossy(&usage.font_name);

    // Always use standard Type 1 FontMatrix [0.001 0 0 0.001 0 0] for embedded fonts.
    // PostForge does the same — the embedded font program must use the standard
    // 1000-unit coordinate system regardless of the original font's matrix.
    let font_matrix = [0.001, 0.0, 0.0, 0.001, 0.0, 0.0];

    // Get FontBBox
    let bbox = get_font_bbox(ctx, font_entity);

    // Determine which glyphs to include (subset)
    let glyph_set = compute_glyph_subset(
        ctx,
        charstrings_entity,
        encoding_entity,
        &usage.used_codes,
        len_iv,
    );

    // Build encoding array (256 entries)
    let encoding = build_encoding_array(ctx, encoding_entity);

    // === Section 1: Cleartext ===
    let mut cleartext = Vec::new();

    // Header
    writeln!(cleartext, "%!PS-AdobeFont-1.0: {}", font_name_str).unwrap();
    writeln!(cleartext, "12 dict begin").unwrap();
    writeln!(cleartext, "/FontName /{} def", font_name_str).unwrap();
    writeln!(cleartext, "/FontType 1 def").unwrap();
    write!(cleartext, "/FontMatrix [").unwrap();
    for (i, &v) in font_matrix.iter().enumerate() {
        if i > 0 {
            write!(cleartext, " ").unwrap();
        }
        write!(cleartext, "{}", format_float(v)).unwrap();
    }
    writeln!(cleartext, "] readonly def").unwrap();
    writeln!(
        cleartext,
        "/FontBBox [{} {} {} {}] readonly def",
        bbox[0] as i32, bbox[1] as i32, bbox[2] as i32, bbox[3] as i32
    )
    .unwrap();

    // Encoding array
    writeln!(cleartext, "/Encoding 256 array").unwrap();
    writeln!(cleartext, "0 1 255 {{1 index exch /.notdef put}} for").unwrap();
    for (code, name) in encoding.iter().enumerate() {
        if *name != ".notdef" {
            writeln!(cleartext, "dup {} /{} put", code, name).unwrap();
        }
    }
    writeln!(cleartext, "readonly def").unwrap();
    writeln!(cleartext, "currentdict end").unwrap();
    // PostForge: "currentfile eexec\n" — newline after eexec, no trailing space
    cleartext.extend_from_slice(b"currentfile eexec\n");
    let length1 = cleartext.len();

    // === Section 2: Private dict (to be eexec encrypted) ===
    // Build plaintext that will be eexec-encrypted.
    // Matches PostForge's _build_private_and_charstrings exactly.
    let mut lines: Vec<Vec<u8>> = Vec::new();

    lines.push(b"dup".to_vec());
    lines.push(b"/Private 17 dict dup begin".to_vec());
    lines.push(b"/RD {string currentfile exch readstring pop} executeonly def".to_vec());
    lines.push(b"/ND {noaccess def} executeonly def".to_vec());
    lines.push(b"/NP {noaccess put} executeonly def".to_vec());
    lines.push(format!("/lenIV {} def", len_iv).into_bytes());
    lines.push(b"/MinFeature {16 16} def".to_vec());
    lines.push(b"/password 5839 def".to_vec());

    // Copy Private dict hint values if available
    if let Some(pe) = private_entity {
        emit_private_hint_lines(&mut lines, ctx, pe);
    }

    // Subrs array (include all — not subsetted)
    // Format: "dup {i} {len} RD <space><binary>NP" per line
    if !subrs.is_empty() {
        lines.push(format!("/Subrs {} array", subrs.len()).into_bytes());
        for (i, subr) in subrs.iter().enumerate() {
            let encrypted = charstring_encrypt(subr, len_iv);
            let mut entry = format!("dup {} {} RD ", i, encrypted.len()).into_bytes();
            entry.extend_from_slice(&encrypted);
            entry.extend_from_slice(b"NP");
            lines.push(entry);
        }
        lines.push(b"ND".to_vec());
    }

    // CharStrings dict (subset)
    // "2 index" brings the font dict to the stack for the later put operations
    lines.push(
        format!("2 index /CharStrings {} dict dup begin", glyph_set.len()).into_bytes(),
    );

    // Always emit .notdef first (required for reliable binary eexec parsing)
    if let Some(cs_bytes) = get_charstring_bytes(ctx, charstrings_entity, ".notdef") {
        let mut entry = format!("/.notdef {} RD ", cs_bytes.len()).into_bytes();
        entry.extend_from_slice(&cs_bytes);
        entry.extend_from_slice(b"ND");
        lines.push(entry);
    }

    for glyph_name in &glyph_set {
        if glyph_name == ".notdef" {
            continue;
        }
        if let Some(cs_bytes) = get_charstring_bytes(ctx, charstrings_entity, glyph_name) {
            let mut entry = format!("/{} {} RD ", glyph_name, cs_bytes.len()).into_bytes();
            entry.extend_from_slice(&cs_bytes);
            entry.extend_from_slice(b"ND");
            lines.push(entry);
        }
    }

    lines.push(b"end".to_vec());
    lines.push(b"end".to_vec());
    lines.push(b"readonly put".to_vec());
    lines.push(b"noaccess put".to_vec());
    lines.push(b"dup /FontName get exch definefont pop".to_vec());
    lines.push(b"mark currentfile closefile".to_vec());

    // Join with newlines (no trailing newline, matches PostForge)
    let private: Vec<u8> = lines.join(&b'\n');
    let encrypted = eexec_encrypt(&private);
    let length2 = encrypted.len();

    // === Section 3: Footer ===
    // 512 ASCII '0' chars + newline + cleartomark + newline (matches PostForge)
    let mut footer = Vec::new();
    footer.extend_from_slice(&[b'0'; 512]);
    footer.extend_from_slice(b"\ncleartomark\n");
    let length3 = footer.len();

    // Wrap in PFB format (binary segment markers), matching PostForge's _to_pfb.
    // PDF viewers expect PFB-wrapped Type 1 fonts in /FontFile streams.
    let result = build_pfb(&cleartext, &encrypted, &footer);

    Some((result, length1, length2, length3))
}

/// Compute the subset of glyphs needed: used glyphs + .notdef + seac dependencies.
fn compute_glyph_subset(
    ctx: &Context,
    charstrings_entity: EntityId,
    encoding_entity: Option<EntityId>,
    used_codes: &HashSet<u16>,
    len_iv: usize,
) -> Vec<String> {
    let mut needed: HashSet<String> = HashSet::new();
    needed.insert(".notdef".to_string());

    // Map used codes to glyph names via encoding
    if let Some(enc_entity) = encoding_entity {
        for &code in used_codes {
            if code > 255 {
                continue;
            }
            let glyph_obj = ctx.arrays.get_element(enc_entity, code as u32);
            if let PsValue::Name(id) = glyph_obj.value {
                let name = String::from_utf8_lossy(ctx.names.get_bytes(id)).to_string();
                if name != ".notdef" {
                    needed.insert(name);
                }
            }
        }
    }

    // Find seac dependencies (iterate until stable)
    let mut pending: Vec<String> = needed.iter().cloned().collect();
    while let Some(glyph_name) = pending.pop() {
        if let Some(cs_bytes) = get_raw_charstring_bytes(ctx, charstrings_entity, &glyph_name) {
            let decrypted = decrypt_charstring(&cs_bytes, len_iv);
            for dep in find_seac_deps(&decrypted) {
                if needed.insert(dep.clone()) {
                    pending.push(dep);
                }
            }
        }
    }

    let mut result: Vec<String> = needed.into_iter().collect();
    result.sort();
    result
}

/// Get raw (encrypted) charstring bytes for a glyph by name.
fn get_raw_charstring_bytes(
    ctx: &Context,
    charstrings_entity: EntityId,
    glyph_name: &str,
) -> Option<Vec<u8>> {
    let name_id = ctx.names.find(glyph_name.as_bytes())?;
    let cs_obj = ctx.dicts.get(charstrings_entity, &DictKey::Name(name_id))?;
    match cs_obj.value {
        PsValue::String { entity, start, len } => Some(ctx.strings.get(entity, start, len).to_vec()),
        _ => None,
    }
}

/// Get charstring bytes for embedding (already encrypted from the PS font).
fn get_charstring_bytes(
    ctx: &Context,
    charstrings_entity: EntityId,
    glyph_name: &str,
) -> Option<Vec<u8>> {
    get_raw_charstring_bytes(ctx, charstrings_entity, glyph_name)
}

/// Build encoding array from PS font's encoding entity.
fn build_encoding_array(ctx: &Context, encoding_entity: Option<EntityId>) -> Vec<String> {
    let mut encoding = vec![".notdef".to_string(); 256];
    if let Some(enc_entity) = encoding_entity {
        for code in 0..256u32 {
            let obj = ctx.arrays.get_element(enc_entity, code);
            if let PsValue::Name(id) = obj.value {
                let name = String::from_utf8_lossy(ctx.names.get_bytes(id)).to_string();
                encoding[code as usize] = name;
            }
        }
    }
    encoding
}

/// Emit Private dict hint values as lines (for joining with \n).
fn emit_private_hint_lines(lines: &mut Vec<Vec<u8>>, ctx: &Context, pe: EntityId) {
    // Numeric hint keys
    let numeric_keys = [
        "BlueFuzz",
        "BlueScale",
        "BlueShift",
        "ForceBold",
        "LanguageGroup",
        "ExpansionFactor",
    ];
    for key in &numeric_keys {
        if let Some(name_id) = ctx.names.find(key.as_bytes())
            && let Some(obj) = ctx.dicts.get(pe, &DictKey::Name(name_id))
        {
            if let Some(v) = obj.as_f64() {
                lines.push(format!("/{} {} def", key, format_float(v)).into_bytes());
            } else if let PsValue::Bool(b) = obj.value {
                lines.push(format!("/{} {} def", key, b).into_bytes());
            }
        }
    }

    // Array hint keys
    let array_keys = [
        "BlueValues",
        "OtherBlues",
        "FamilyBlues",
        "FamilyOtherBlues",
        "StdHW",
        "StdVW",
        "StemSnapH",
        "StemSnapV",
    ];
    for key in &array_keys {
        if let Some(name_id) = ctx.names.find(key.as_bytes())
            && let Some(obj) = ctx.dicts.get(pe, &DictKey::Name(name_id))
            && let PsValue::Array { entity, start, len } = obj.value
        {
            let elems = ctx.arrays.get(entity, start, len);
            let mut s = format!("/{} [", key);
            for (i, elem) in elems.iter().enumerate() {
                if i > 0 {
                    s.push(' ');
                }
                if let Some(v) = elem.as_f64() {
                    s.push_str(&format_float(v));
                }
            }
            s.push_str("] def");
            lines.push(s.into_bytes());
        }
    }
}

/// Build PFB (Printer Font Binary) format from three Type 1 font sections.
///
/// PFB wraps each section with a marker byte and length:
/// - `\x80\x01` + LE32 length = ASCII segment (cleartext, footer)
/// - `\x80\x02` + LE32 length = Binary segment (eexec encrypted)
/// - `\x80\x03` = EOF marker
fn build_pfb(cleartext: &[u8], encrypted: &[u8], footer: &[u8]) -> Vec<u8> {
    let mut pfb = Vec::with_capacity(cleartext.len() + encrypted.len() + footer.len() + 20);

    // ASCII header segment
    pfb.extend_from_slice(&[0x80, 0x01]);
    pfb.extend_from_slice(&(cleartext.len() as u32).to_le_bytes());
    pfb.extend_from_slice(cleartext);

    // Binary eexec segment
    pfb.extend_from_slice(&[0x80, 0x02]);
    pfb.extend_from_slice(&(encrypted.len() as u32).to_le_bytes());
    pfb.extend_from_slice(encrypted);

    // ASCII trailer segment
    if !footer.is_empty() {
        pfb.extend_from_slice(&[0x80, 0x01]);
        pfb.extend_from_slice(&(footer.len() as u32).to_le_bytes());
        pfb.extend_from_slice(footer);
    }

    // EOF marker
    pfb.extend_from_slice(&[0x80, 0x03]);

    pfb
}

/// Format a float value concisely.
fn format_float(v: f64) -> String {
    if v == v.floor() && v.abs() < 1e9 {
        format!("{}", v as i64)
    } else {
        format!("{:.6}", v).trim_end_matches('0').trim_end_matches('.').to_string()
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
                    if idx < subrs.len()
                        && let Some(w) = extract_charstring_width(&subrs[idx], subrs)
                    {
                        return Some(w);
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
        2 => build_type2_font(writer, usage, ctx),
        3 => None, // Type 3 can't be embedded
        0 | 42 => None, // CID/TrueType embedding not yet implemented
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
    // Determine FirstChar/LastChar from used codes
    let mut first_char: u16 = 255;
    let mut last_char: u16 = 0;
    for &code in &usage.used_codes {
        if code <= 255 {
            first_char = first_char.min(code);
            last_char = last_char.max(code);
        }
    }
    if first_char > last_char {
        first_char = 0;
        last_char = 0;
    }

    // Extract widths for ALL characters in FirstChar..LastChar range
    // (PDF viewers need correct widths for the entire range, not just used chars)
    let mut widths: HashMap<u16, i32> = HashMap::new();
    if let (Some(enc_entity), Some(cs_entity)) = (encoding_entity, charstrings_entity) {
        for code in first_char..=last_char {
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
            }
        }
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

    // Build Type 1 font file (embedded font program)
    let font_file_ref = if !usage.is_standard_14 {
        if let Some(cs_entity) = charstrings_entity {
            build_type1_font_file(
                ctx,
                font_entity,
                usage,
                cs_entity,
                encoding_entity,
                private_entity,
                len_iv,
                &subrs,
            )
            .map(|(data, len1, len2, len3)| {
                let entries = vec![
                    (b"Length1".to_vec(), PdfObj::Int(len1 as i64)),
                    (b"Length2".to_vec(), PdfObj::Int(len2 as i64)),
                    (b"Length3".to_vec(), PdfObj::Int(len3 as i64)),
                ];
                writer.add_stream(entries, &data, true)
            })
        } else {
            None
        }
    } else {
        None
    };

    // Build FontDescriptor
    let bbox = get_font_bbox(ctx, font_entity);
    let flags = compute_font_flags(ctx, font_entity);
    let descriptor_ref =
        build_font_descriptor(writer, &usage.font_name, &bbox, flags, font_file_ref, None);

    // Build Encoding with Differences array from PS font's actual encoding
    let encoding_obj = build_encoding_differences(ctx, encoding_entity, first_char, last_char);

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

    if let Some(enc) = encoding_obj {
        font_entries.push((b"Encoding".to_vec(), enc));
    }

    if let Some(tu_ref) = tounicode_ref {
        font_entries.push((b"ToUnicode".to_vec(), PdfObj::Ref(tu_ref)));
    }

    Some(writer.add_object(&PdfObj::Dict(font_entries)))
}

/// Build a Type 2 (CFF) font resource with widths extracted from Type 2 charstrings.
fn build_type2_font(
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

    // Get Private dict → defaultWidthX, nominalWidthX, local Subrs
    let private_entity = ctx
        .dicts
        .get(font_entity, &DictKey::Name(ctx.name_cache.n_private))
        .and_then(|obj| match obj.value {
            PsValue::Dict(e) => Some(e),
            _ => None,
        });

    let mut default_width_x = 0.0;
    let mut nominal_width_x = 0.0;
    let mut local_subrs: Vec<Vec<u8>> = Vec::new();

    if let Some(pe) = private_entity {
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
    let mut global_subrs: Vec<Vec<u8>> = Vec::new();
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

    // Determine FirstChar/LastChar from used codes
    let mut first_char: u16 = 255;
    let mut last_char: u16 = 0;
    for &code in &usage.used_codes {
        if code <= 255 {
            first_char = first_char.min(code);
            last_char = last_char.max(code);
        }
    }
    if first_char > last_char {
        first_char = 0;
        last_char = 0;
    }

    // Extract widths for all characters in range using Type 2 charstring interpreter
    let mut widths: HashMap<u16, i32> = HashMap::new();
    if let (Some(enc_entity), Some(cs_entity)) = (encoding_entity, charstrings_entity) {
        for code in first_char..=last_char {
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

            let cs_bytes = ctx.strings.get(cs_ent, cs_start, cs_len).to_vec();
            if let Ok(result) = type2_charstring::execute_type2_charstring(
                &cs_bytes,
                &local_subrs,
                &global_subrs,
                default_width_x,
                nominal_width_x,
                false,
            ) {
                widths.insert(code, result.width_x as i32);
            }
        }
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

    // Build Encoding with Differences
    let encoding_obj = build_encoding_differences(ctx, encoding_entity, first_char, last_char);

    // Embed raw CFF binary as FontFile3 with Subtype Type1C
    let font_file3_ref = ctx
        .names
        .find(b"_CFFData")
        .and_then(|name_id| ctx.dicts.get(font_entity, &DictKey::Name(name_id)))
        .and_then(|obj| match obj.value {
            PsValue::String { entity, start, len } => {
                let cff_bytes = ctx.strings.get(entity, start, len).to_vec();
                let entries = vec![(b"Subtype".to_vec(), PdfObj::name("Type1C"))];
                Some(writer.add_stream(entries, &cff_bytes, true))
            }
            _ => None,
        });

    // Build FontDescriptor
    let bbox = get_font_bbox(ctx, font_entity);
    let flags = compute_font_flags(ctx, font_entity);
    let descriptor_ref =
        build_font_descriptor(writer, &usage.font_name, &bbox, flags, None, font_file3_ref);

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

    if let Some(enc) = encoding_obj {
        font_entries.push((b"Encoding".to_vec(), enc));
    }

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
            if let Ok(name_str) = std::str::from_utf8(name_bytes)
                && name_str != ".notdef"
                && let Some(unicode) = unicode_mapping::glyph_name_to_unicode(name_str)
            {
                map.insert(code, unicode);
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

/// Build a PDF Encoding dict with /Differences from the PS font's encoding array.
///
/// The Differences array tells the PDF viewer which glyph name maps to each
/// character code, overriding the base encoding. This is essential for fonts
/// with custom encodings (e.g., bullet at code 170 instead of ª).
fn build_encoding_differences(
    ctx: &Context,
    encoding_entity: Option<EntityId>,
    first_char: u16,
    last_char: u16,
) -> Option<PdfObj> {
    let enc_entity = encoding_entity?;

    // Build Differences array: [code1 /name1 /name2 ... codeN /nameN ...]
    // Consecutive glyph names after a code number increment the code automatically.
    let mut differences: Vec<PdfObj> = Vec::new();
    let mut need_code = true; // whether we need to emit the next code number

    for code in first_char..=last_char {
        let glyph_obj = ctx.arrays.get_element(enc_entity, code as u32);
        let name_id = match glyph_obj.value {
            PsValue::Name(id) => id,
            _ => {
                need_code = true;
                continue;
            }
        };

        let name_bytes = ctx.names.get_bytes(name_id);
        if name_bytes == b".notdef" {
            need_code = true;
            continue;
        }

        if need_code {
            differences.push(PdfObj::Int(code as i64));
            need_code = false;
        }
        differences.push(PdfObj::Name(name_bytes.to_vec()));
    }

    if differences.is_empty() {
        return None;
    }

    Some(PdfObj::Dict(vec![
        (b"Type".to_vec(), PdfObj::name("Encoding")),
        (b"Differences".to_vec(), PdfObj::Array(differences)),
    ]))
}

/// Read FontBBox from font dict.
fn get_font_bbox(ctx: &Context, font_entity: EntityId) -> [f64; 4] {
    if let Some(name_id) = ctx.names.find(b"FontBBox")
        && let Some(obj) = ctx.dicts.get(font_entity, &DictKey::Name(name_id))
        && let PsValue::Array { entity, start, len } = obj.value
        && len >= 4
    {
        let elems = ctx.arrays.get(entity, start, len);
        return [
            elems[0].as_f64().unwrap_or(0.0),
            elems[1].as_f64().unwrap_or(0.0),
            elems[2].as_f64().unwrap_or(0.0),
            elems[3].as_f64().unwrap_or(0.0),
        ];
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
/// Build a PDF FontDescriptor object.
///
/// `font_file_ref`: object ref for /FontFile (Type 1 embedding)
/// `font_file3_ref`: object ref for /FontFile3 (CFF embedding)
fn build_font_descriptor(
    writer: &mut PdfWriter,
    font_name: &[u8],
    bbox: &[f64; 4],
    flags: u32,
    font_file_ref: Option<u32>,
    font_file3_ref: Option<u32>,
) -> u32 {
    let mut entries = vec![
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

    if let Some(ff_ref) = font_file_ref {
        entries.push((b"FontFile".to_vec(), PdfObj::Ref(ff_ref)));
    }
    if let Some(ff3_ref) = font_file3_ref {
        entries.push((b"FontFile3".to_vec(), PdfObj::Ref(ff3_ref)));
    }

    writer.add_object(&PdfObj::Dict(entries))
}
