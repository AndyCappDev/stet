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
use stet_core::truetype;
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

/// Find all Subr indices transitively referenced by a charstring, simulating
/// the stack across `callsubr` boundaries so that dynamically-determined indices
/// (e.g. hint-replacement wrappers: `<N> 4 callsubr` where Subr 4 does
/// `1 3 callothersubr pop callsubr`) are correctly detected.
fn find_subr_refs_deep(decrypted: &[u8], subrs: &[Vec<u8>], found: &mut HashSet<usize>) {
    let mut stack: Vec<f64> = Vec::new();
    let mut ps_stack: Vec<f64> = Vec::new();
    scan_charstring(decrypted, subrs, found, &mut stack, &mut ps_stack, 0);
}

/// Recursive charstring scanner with stack simulation. `depth` limits recursion
/// to prevent infinite loops in mutually-recursive Subrs.
///
/// Always re-enters a Subr even if already found, because the caller's stack
/// may provide different dynamically-determined Subr indices each time (e.g.
/// hint-replacement wrapper called as `<N> 4 callsubr`).
fn scan_charstring(
    data: &[u8],
    subrs: &[Vec<u8>],
    found: &mut HashSet<usize>,
    stack: &mut Vec<f64>,
    ps_stack: &mut Vec<f64>,
    depth: u32,
) {
    if depth > 10 {
        return;
    }
    let mut i = 0;
    while i < data.len() {
        let b = data[i];
        i += 1;
        match b {
            12 => {
                if i < data.len() {
                    let b2 = data[i];
                    i += 1;
                    match b2 {
                        12 => {
                            // div: pop 2, push quotient
                            if stack.len() >= 2 {
                                let divisor = stack.pop().unwrap();
                                let dividend = stack.pop().unwrap();
                                if divisor != 0.0 {
                                    stack.push(dividend / divisor);
                                }
                            }
                        }
                        16 => {
                            // callothersubr: pop subr_num, pop n_args, pop n_args values
                            // Args are pushed to PS stack for retrieval by pop (12,17)
                            if stack.len() >= 2 {
                                let _subr_num = stack.pop().unwrap();
                                let n = stack.pop().unwrap() as usize;
                                let n = n.min(stack.len());
                                for _ in 0..n {
                                    ps_stack.push(stack.pop().unwrap());
                                }
                            }
                        }
                        17 => {
                            // pop: retrieve value from PS stack
                            if let Some(v) = ps_stack.pop() {
                                stack.push(v);
                            }
                        }
                        _ => stack.clear(),
                    }
                }
            }
            11 => return,        // return from Subr
            14 => return,        // endchar
            13 => stack.clear(), // hsbw
            10 => {
                // callsubr: pop index, record it, then inline the Subr to
                // discover any stack-dependent callsubr references within it.
                if let Some(idx) = stack.pop() {
                    let idx = idx as i64;
                    if idx >= 0 {
                        let idx = idx as usize;
                        if idx < subrs.len() {
                            found.insert(idx);
                            scan_charstring(&subrs[idx], subrs, found, stack, ps_stack, depth + 1);
                        }
                    }
                }
            }
            // Number encoding
            32..=246 => stack.push((b as i32 - 139) as f64),
            247..=250 => {
                if i < data.len() {
                    let b2 = data[i] as i32;
                    i += 1;
                    stack.push(((b as i32 - 247) * 256 + b2 + 108) as f64);
                }
            }
            251..=254 => {
                if i < data.len() {
                    let b2 = data[i] as i32;
                    i += 1;
                    stack.push((-(b as i32 - 251) * 256 - b2 - 108) as f64);
                }
            }
            255 => {
                if i + 3 < data.len() {
                    let val = i32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]);
                    i += 4;
                    stack.push(val as f64);
                }
            }
            _ => stack.clear(),
        }
    }
}

/// Compute the set of Subr indices transitively referenced by the given charstrings.
/// Uses stack simulation across `callsubr` boundaries to detect dynamically-determined
/// Subr indices (e.g. hint-replacement wrappers).
fn compute_used_subrs(glyph_charstrings: &[Vec<u8>], subrs: &[Vec<u8>]) -> HashSet<usize> {
    let mut needed: HashSet<usize> = HashSet::new();
    for cs in glyph_charstrings {
        find_subr_refs_deep(cs, subrs, &mut needed);
    }
    needed
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
    charstrings_entities: &[EntityId],
    encoding_entities: &[Option<EntityId>],
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

    // Determine which glyphs to include (subset) — merge from all encoding entities
    let glyph_set = compute_glyph_subset_multi(
        ctx,
        charstrings_entities,
        encoding_entities,
        &usage.used_codes,
        len_iv,
    );

    // Build encoding array (256 entries) — merge from all encoding entities
    let encoding = build_encoding_array_multi(ctx, encoding_entities);

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

    // Subrs array (subsetted: unused Subrs replaced with `return` stubs)
    if !subrs.is_empty() {
        // Collect decrypted charstrings for included glyphs to find Subr references
        let mut glyph_charstrings: Vec<Vec<u8>> = Vec::new();
        for glyph_name in &glyph_set {
            if let Some(cs_bytes) = get_raw_charstring_bytes(ctx, charstrings_entities, glyph_name)
            {
                glyph_charstrings.push(decrypt_charstring(&cs_bytes, len_iv));
            }
        }
        let used_subrs = compute_used_subrs(&glyph_charstrings, subrs);

        // Trim array to max referenced index + 1
        let trimmed_len = used_subrs.iter().copied().max().map(|m| m + 1).unwrap_or(0);

        if trimmed_len > 0 {
            let return_stub = charstring_encrypt(&[11], len_iv);
            lines.push(format!("/Subrs {} array", trimmed_len).into_bytes());
            for i in 0..trimmed_len {
                let encrypted = if used_subrs.contains(&i) {
                    charstring_encrypt(&subrs[i], len_iv)
                } else {
                    return_stub.clone()
                };
                let mut entry = format!("dup {} {} RD ", i, encrypted.len()).into_bytes();
                entry.extend_from_slice(&encrypted);
                entry.extend_from_slice(b"NP");
                lines.push(entry);
            }
            lines.push(b"ND".to_vec());
        }
    }

    // CharStrings dict (subset)
    // "2 index" brings the font dict to the stack for the later put operations
    lines.push(format!("2 index /CharStrings {} dict dup begin", glyph_set.len()).into_bytes());

    // Always emit .notdef first (required for reliable binary eexec parsing)
    if let Some(cs_bytes) = get_charstring_bytes(ctx, charstrings_entities, ".notdef") {
        let mut entry = format!("/.notdef {} RD ", cs_bytes.len()).into_bytes();
        entry.extend_from_slice(&cs_bytes);
        entry.extend_from_slice(b"ND");
        lines.push(entry);
    }

    for glyph_name in &glyph_set {
        if glyph_name == ".notdef" {
            continue;
        }
        if let Some(cs_bytes) = get_charstring_bytes(ctx, charstrings_entities, glyph_name) {
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

    // Wrap in PFB format (binary segment markers).
    // PDF /FontFile streams accept both PFA and PFB; PFB is more reliable
    // with PDF viewers (matches PostForge's approach).
    let mut result = Vec::with_capacity(cleartext.len() + encrypted.len() + footer.len() + 20);
    // Segment 1: ASCII header
    result.push(0x80);
    result.push(0x01);
    result.extend_from_slice(&(cleartext.len() as u32).to_le_bytes());
    result.extend_from_slice(&cleartext);
    // Segment 2: Binary eexec
    result.push(0x80);
    result.push(0x02);
    result.extend_from_slice(&(encrypted.len() as u32).to_le_bytes());
    result.extend_from_slice(&encrypted);
    // Segment 3: ASCII trailer
    if !footer.is_empty() {
        result.push(0x80);
        result.push(0x01);
        result.extend_from_slice(&(footer.len() as u32).to_le_bytes());
        result.extend_from_slice(&footer);
    }
    // EOF marker
    result.push(0x80);
    result.push(0x03);

    Some((result, length1, length2, length3))
}

/// Compute the subset of glyphs needed: used glyphs + .notdef + seac dependencies.
/// Compute glyph subset merging from multiple encoding entities.
fn compute_glyph_subset_multi(
    ctx: &Context,
    charstrings_entities: &[EntityId],
    encoding_entities: &[Option<EntityId>],
    used_codes: &HashSet<u16>,
    len_iv: usize,
) -> Vec<String> {
    let mut needed: HashSet<String> = HashSet::new();
    needed.insert(".notdef".to_string());

    // Map used codes to glyph names via all encoding entities
    for enc in encoding_entities {
        let Some(enc_entity) = enc else { continue };
        for &code in used_codes {
            if code > 255 {
                continue;
            }
            let glyph_obj = ctx.arrays.get_element(*enc_entity, code as u32);
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
        if let Some(cs_bytes) =
            get_raw_charstring_bytes(ctx, charstrings_entities, &glyph_name)
        {
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
/// Searches multiple CharStrings dicts (dvips creates re-encoded subsets).
fn get_raw_charstring_bytes(
    ctx: &Context,
    charstrings_entities: &[EntityId],
    glyph_name: &str,
) -> Option<Vec<u8>> {
    let name_id = ctx.names.find(glyph_name.as_bytes())?;
    for &cs_entity in charstrings_entities {
        if let Some(cs_obj) = ctx.dicts.get(cs_entity, &DictKey::Name(name_id)) {
            match cs_obj.value {
                PsValue::String { entity, start, len } => {
                    return Some(ctx.strings.get(entity, start, len).to_vec());
                }
                _ => {}
            }
        }
    }
    None
}

/// Get charstring bytes for embedding (already encrypted from the PS font).
fn get_charstring_bytes(
    ctx: &Context,
    charstrings_entities: &[EntityId],
    glyph_name: &str,
) -> Option<Vec<u8>> {
    get_raw_charstring_bytes(ctx, charstrings_entities, glyph_name)
}

/// Build encoding array from PS font's encoding entity.
/// Build a merged encoding array from multiple encoding entities.
fn build_encoding_array_multi(
    ctx: &Context,
    encoding_entities: &[Option<EntityId>],
) -> Vec<String> {
    let mut encoding = vec![".notdef".to_string(); 256];
    for enc in encoding_entities {
        let Some(enc_entity) = enc else { continue };
        for code in 0..256u32 {
            if encoding[code as usize] != ".notdef" {
                continue; // already filled from a previous entity
            }
            let obj = ctx.arrays.get_element(*enc_entity, code);
            if let PsValue::Name(id) = obj.value {
                let name = String::from_utf8_lossy(ctx.names.get_bytes(id)).to_string();
                if name != ".notdef" {
                    encoding[code as usize] = name;
                }
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



/// Format a float value concisely.
fn format_float(v: f64) -> String {
    if v == v.floor() && v.abs() < 1e9 {
        format!("{}", v as i64)
    } else {
        format!("{:.6}", v)
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
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
                    match b2 {
                        7 => {
                            // sbw: sbx sby wx wy → width is stack[-2]
                            if stack.len() >= 2 {
                                return Some(stack[stack.len() - 2]);
                            }
                        }
                        12 => {
                            // div: num1 num2 → num1/num2
                            if stack.len() >= 2 {
                                let b_val = stack.pop().unwrap();
                                let a_val = stack.pop().unwrap();
                                stack.push(if b_val != 0.0 { a_val / b_val } else { 0.0 });
                            }
                        }
                        _ => {} // Other escape ops — don't clear stack
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
                // Other opcodes before hsbw/sbw — ignore (don't clear stack,
                // as CM fonts use div and other ops to compute width arguments)
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
    let result = match usage.font_type {
        1 => build_type1_font(writer, usage, ctx),
        2 => build_type2_font(writer, usage, ctx),
        3 => None, // Type 3 can't be embedded
        0 | 42 => build_cid_font(writer, usage, ctx),
        _ => None,
    };
    if result.is_none() {
        eprintln!(
            "[font_embedder] FAILED type={} name={} entity={:?} codes={}",
            usage.font_type,
            String::from_utf8_lossy(&usage.font_name),
            usage.font_entity,
            usage.used_codes.len()
        );
    }
    result
}

/// Build a Type 1 font resource.
fn build_type1_font(writer: &mut PdfWriter, usage: &FontUsage, ctx: &Context) -> Option<u32> {
    let font_entity = usage.font_entity;
    let font_name_str = String::from_utf8_lossy(&usage.font_name);

    // Collect all CharStrings dicts from all entities. dvips re-encoded instances
    // each have a subset of CharStrings — we need to search all of them.
    let mut charstrings_entities: Vec<EntityId> = Vec::new();
    for &ent in &usage.all_entities {
        if let Some(obj) = ctx.dicts.get(ent, &DictKey::Name(ctx.name_cache.n_char_strings)) {
            if let PsValue::Dict(e) = obj.value {
                if !charstrings_entities.contains(&e) {
                    charstrings_entities.push(e);
                }
            }
        }
    }
    // Sort by entry count descending so the most complete dict is searched first
    charstrings_entities.sort_by(|a, b| {
        let a_count = ctx.dicts.entry(*a).entries.len();
        let b_count = ctx.dicts.entry(*b).entries.len();
        b_count.cmp(&a_count)
    });

    let mut private_entity = None;
    for &ent in &usage.all_entities {
        if let Some(obj) = ctx.dicts.get(ent, &DictKey::Name(ctx.name_cache.n_private)) {
            if let PsValue::Dict(e) = obj.value {
                private_entity = Some(e);
                break;
            }
        }
    }

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
    // Merge from all encoding entities (dvips re-encoded instances)
    let mut widths: HashMap<u16, i32> = HashMap::new();
    if !charstrings_entities.is_empty() {
        for &ent in &usage.all_entities {
            let enc = ctx
                .dicts
                .get(ent, &DictKey::Name(ctx.name_cache.n_encoding))
                .and_then(|obj| match obj.value {
                    PsValue::Array { entity, .. } => Some(entity),
                    _ => None,
                });
            let Some(enc_entity) = enc else { continue };
            for code in first_char..=last_char {
                if widths.contains_key(&code) {
                    continue;
                }
                let glyph_name_obj = ctx.arrays.get_element(enc_entity, code as u32);
                let glyph_name_id = match glyph_name_obj.value {
                    PsValue::Name(id) => id,
                    _ => continue,
                };
                // Skip .notdef — a later entity may have the real glyph at this code
                let glyph_name_bytes = ctx.names.get_bytes(glyph_name_id);
                if glyph_name_bytes == b".notdef" {
                    continue;
                }

                // Search all CharStrings dicts for this glyph
                let mut found = None;
                for &cs_entity in &charstrings_entities {
                    if let Some(obj) = ctx.dicts.get(cs_entity, &DictKey::Name(glyph_name_id)) {
                        if let PsValue::String { entity, start, len } = obj.value {
                            found = Some((entity, start, len));
                            break;
                        }
                    }
                }
                let Some((cs_ent, cs_start, cs_len)) = found else {
                    continue;
                };

                let cs_bytes = ctx.strings.get(cs_ent, cs_start, cs_len);
                let decrypted = decrypt_charstring(cs_bytes, len_iv);
                if let Some(w) = extract_charstring_width(&decrypted, &subrs) {
                    widths.insert(code, w as i32);
                }
            }
        }
    }

    // Build Widths array
    let widths_array: Vec<PdfObj> = (first_char..=last_char)
        .map(|code| PdfObj::Int(*widths.get(&code).unwrap_or(&0) as i64))
        .collect();

    // Build ToUnicode CMap — merge encoding arrays from all font instances
    // (dvips creates multiple re-encoded instances of the same base font)
    let all_enc_entities: Vec<Option<EntityId>> = usage
        .all_entities
        .iter()
        .map(|&ent| {
            ctx.dicts
                .get(ent, &DictKey::Name(ctx.name_cache.n_encoding))
                .and_then(|obj| match obj.value {
                    PsValue::Array { entity, .. } => Some(entity),
                    _ => None,
                })
        })
        .collect();
    let tounicode_map =
        build_tounicode_map_multi(ctx, &all_enc_entities, &usage.used_codes);
    let tounicode_ref = if !tounicode_map.is_empty() {
        let cmap_data = generate_tounicode_cmap(&tounicode_map, &font_name_str);
        Some(writer.add_stream(Vec::new(), &cmap_data, true))
    } else {
        None
    };

    // Build Type 1 font file (embedded font program)
    let font_file_ref = if !usage.is_standard_14 {
        if !charstrings_entities.is_empty() {
            build_type1_font_file(
                ctx,
                font_entity,
                usage,
                &charstrings_entities,
                &all_enc_entities,
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

    // Build Encoding with Differences array merged from all font instances
    let encoding_obj =
        build_encoding_differences_multi(ctx, &all_enc_entities, first_char, last_char);

    // Build Font dict
    let mut font_entries: Vec<(Vec<u8>, PdfObj)> = vec![
        (b"Type".to_vec(), PdfObj::name("Font")),
        (b"Subtype".to_vec(), PdfObj::name("Type1")),
        (b"BaseFont".to_vec(), PdfObj::Name(usage.font_name.clone())),
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
fn build_type2_font(writer: &mut PdfWriter, usage: &FontUsage, ctx: &Context) -> Option<u32> {
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
        (b"BaseFont".to_vec(), PdfObj::Name(usage.font_name.clone())),
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
pub fn build_tounicode_map(
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

/// Build a merged ToUnicode map from multiple encoding entities.
///
/// dvips creates multiple re-encoded font instances from the same base font,
/// each with a different encoding subset. This merges glyph name→Unicode
/// mappings from all encoding arrays to produce a complete ToUnicode map.
pub fn build_tounicode_map_multi(
    ctx: &Context,
    encoding_entities: &[Option<EntityId>],
    used_codes: &HashSet<u16>,
) -> HashMap<u16, u16> {
    let mut map = HashMap::new();
    for enc in encoding_entities {
        let Some(enc_entity) = enc else { continue };
        for &code in used_codes {
            if code > 255 || map.contains_key(&code) {
                continue;
            }
            let glyph_name_obj = ctx.arrays.get_element(*enc_entity, code as u32);
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
    }
    map
}

/// Generate a ToUnicode CMap stream.
pub fn generate_tounicode_cmap(map: &HashMap<u16, u16>, font_name: &str) -> Vec<u8> {
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

/// Build an encoding-aware ToUnicode CMap for a font that failed full embedding.
///
/// Extracts the font's `/Encoding` array, maps glyph names → Unicode via AGL,
/// and generates a proper ToUnicode CMap stream. This provides correct text
/// extraction for non-ASCII characters even when font embedding fails.
pub fn build_tounicode_for_fallback(
    writer: &mut PdfWriter,
    usage: &FontUsage,
    ctx: &Context,
) -> Option<u32> {
    let all_enc_entities: Vec<Option<EntityId>> = usage
        .all_entities
        .iter()
        .map(|&ent| {
            ctx.dicts
                .get(ent, &DictKey::Name(ctx.name_cache.n_encoding))
                .and_then(|obj| match obj.value {
                    PsValue::Array { entity, .. } => Some(entity),
                    _ => None,
                })
        })
        .collect();
    let tounicode_map =
        build_tounicode_map_multi(ctx, &all_enc_entities, &usage.used_codes);
    if tounicode_map.is_empty() {
        return None;
    }
    let font_name_str = String::from_utf8_lossy(&usage.font_name);
    let cmap_data = generate_tounicode_cmap(&tounicode_map, &font_name_str);
    Some(writer.add_stream(Vec::new(), &cmap_data, true))
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
    build_encoding_differences_multi(ctx, &[encoding_entity], first_char, last_char)
}

/// Build a PDF Encoding dict merging glyph names from multiple encoding entities.
///
/// dvips creates multiple re-encoded font instances from the same base font,
/// each with only the glyphs needed for that instance. We merge all encoding
/// arrays so the PDF Differences contains all glyph names used across all instances.
fn build_encoding_differences_multi(
    ctx: &Context,
    encoding_entities: &[Option<EntityId>],
    first_char: u16,
    last_char: u16,
) -> Option<PdfObj> {
    // Build Differences array: [code1 /name1 /name2 ... codeN /nameN ...]
    // Consecutive glyph names after a code number increment the code automatically.
    let mut differences: Vec<PdfObj> = Vec::new();
    let mut need_code = true; // whether we need to emit the next code number

    for code in first_char..=last_char {
        // Find a non-.notdef glyph name from any encoding entity
        let mut found_name: Option<Vec<u8>> = None;
        for enc in encoding_entities {
            let Some(enc_entity) = enc else { continue };
            let glyph_obj = ctx.arrays.get_element(*enc_entity, code as u32);
            if let PsValue::Name(id) = glyph_obj.value {
                let name_bytes = ctx.names.get_bytes(id);
                if name_bytes != b".notdef" {
                    found_name = Some(name_bytes.to_vec());
                    break;
                }
            }
        }

        let Some(name_bytes) = found_name else {
            need_code = true;
            continue;
        };

        if need_code {
            differences.push(PdfObj::Int(code as i64));
            need_code = false;
        }
        differences.push(PdfObj::Name(name_bytes));
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

// ============================================================
// TrueType font subsetting
// ============================================================

/// Extract component GIDs from a composite glyf entry.
///
/// Walks the component records in a composite glyph (numberOfContours < 0),
/// collecting referenced glyph indices. Only extracts direct references —
/// use `compute_used_gids` for transitive closure.
fn find_composite_glyph_deps(glyf_data: &[u8]) -> Vec<u16> {
    let mut deps = Vec::new();
    if glyf_data.len() < 10 {
        return deps;
    }
    let num_contours = truetype::read_i16(glyf_data, 0);
    if num_contours >= 0 {
        return deps; // simple glyph, no deps
    }
    let mut offset = 10; // skip header
    loop {
        if offset + 4 > glyf_data.len() {
            break;
        }
        let flags = truetype::read_u16(glyf_data, offset);
        let glyph_index = truetype::read_u16(glyf_data, offset + 2);
        deps.push(glyph_index);
        offset += 4;

        // Skip arguments
        if flags & 0x0001 != 0 {
            offset += 4; // ARG_1_AND_2_ARE_WORDS
        } else {
            offset += 2;
        }
        // Skip transform
        if flags & 0x0008 != 0 {
            offset += 2; // WE_HAVE_A_SCALE
        } else if flags & 0x0040 != 0 {
            offset += 4; // WE_HAVE_AN_X_AND_Y_SCALE
        } else if flags & 0x0080 != 0 {
            offset += 8; // WE_HAVE_A_TWO_BY_TWO
        }
        if flags & 0x0020 == 0 {
            break; // MORE_COMPONENTS flag not set
        }
    }
    deps
}

/// Compute the full set of used GIDs including transitive composite dependencies.
///
/// Takes a seed set of directly-used GIDs, adds GID 0 (.notdef), and transitively
/// resolves all composite glyph component references.
fn compute_used_gids(font_data: &[u8], seed_gids: &HashSet<u16>) -> HashSet<u16> {
    let mut used = seed_gids.clone();
    used.insert(0); // always include .notdef
    let mut worklist: Vec<u16> = used.iter().copied().collect();

    while let Some(gid) = worklist.pop() {
        if let Some(glyf_bytes) = truetype::get_glyf_data(font_data, gid) {
            for dep_gid in find_composite_glyph_deps(&glyf_bytes) {
                if used.insert(dep_gid) {
                    worklist.push(dep_gid);
                }
            }
        }
    }
    used
}

/// Subset a TrueType font by zeroing out unused glyf entries.
///
/// Preserves GID numbering — unused glyphs become zero-length entries in loca.
/// All other tables (cmap, hmtx, head, hhea, maxp, etc.) are kept unchanged
/// except head.indexToLocFormat which is set to 1 (long format).
fn subset_truetype(font_data: &[u8], used_gids: &HashSet<u16>) -> Option<Vec<u8>> {
    // Get numGlyphs from maxp
    let (maxp_off, _) = truetype::find_table(font_data, b"maxp")?;
    if maxp_off + 6 > font_data.len() {
        return None;
    }
    let num_glyphs = truetype::read_u16(font_data, maxp_off + 4) as usize;
    let has_glyf = truetype::find_table(font_data, b"glyf").is_some();
    let has_loca = truetype::find_table(font_data, b"loca").is_some();
    if !has_glyf || !has_loca {
        return None; // no glyf/loca tables — can't subset
    }

    // Build new glyf and loca tables
    let mut new_glyf: Vec<u8> = Vec::new();
    let mut loca_offsets: Vec<u32> = Vec::with_capacity(num_glyphs + 1);
    for gid in 0..num_glyphs {
        loca_offsets.push(new_glyf.len() as u32);
        if used_gids.contains(&(gid as u16)) {
            if let Some(glyf_bytes) = truetype::get_glyf_data(font_data, gid as u16) {
                new_glyf.extend_from_slice(&glyf_bytes);
                // Pad to 2-byte alignment
                if new_glyf.len() % 2 != 0 {
                    new_glyf.push(0);
                }
            }
        }
        // Unused glyphs: no data written, loca[gid] == loca[gid+1] → zero-length
    }
    loca_offsets.push(new_glyf.len() as u32);

    // Build loca table (long format)
    let mut loca_data = Vec::with_capacity(loca_offsets.len() * 4);
    for &off in &loca_offsets {
        loca_data.extend_from_slice(&off.to_be_bytes());
    }

    // Reassemble font: copy all tables except glyf/loca, replace those
    let num_tables = truetype::read_u16(font_data, 4) as usize;
    let mut keep_tables: Vec<(&[u8], Vec<u8>)> = Vec::new();

    for i in 0..num_tables {
        let entry_off = 12 + i * 16;
        if entry_off + 16 > font_data.len() {
            break;
        }
        let tag = &font_data[entry_off..entry_off + 4];
        let tbl_offset = truetype::read_u32(font_data, entry_off + 8) as usize;
        let tbl_length = truetype::read_u32(font_data, entry_off + 12) as usize;

        if tag == b"glyf" || tag == b"loca" {
            continue; // replaced below
        }
        if tbl_offset + tbl_length <= font_data.len() {
            let mut data = font_data[tbl_offset..tbl_offset + tbl_length].to_vec();
            // Update head.indexToLocFormat to long (1)
            if tag == b"head" && data.len() >= 52 {
                data[50] = 0;
                data[51] = 1;
            }
            keep_tables.push((tag, data));
        }
    }

    keep_tables.push((b"glyf", new_glyf));
    keep_tables.push((b"loca", loca_data));
    keep_tables.sort_by_key(|(tag, _)| *tag);

    Some(assemble_truetype(&keep_tables))
}

// ============================================================
// CID / TrueType font embedding (Type 0, Type 42)
// ============================================================

/// Reconstruct a valid TrueType font by adding glyf/loca tables
/// built from the PostScript GlyphDirectory.
///
/// CUPS-style CIDFont Type 2 fonts store glyph outlines in PostScript
/// GlyphDirectory dict (keyed by CID=GID via identity CIDMap). The sfnts
/// only contains header/metric tables but no glyf/loca.
fn reconstruct_truetype(
    raw_sfnts: &[u8],
    ctx: &Context,
    cidfont_entity: EntityId,
) -> Option<Vec<u8>> {
    // Get GlyphDirectory dict from the CIDFont
    let gd_name = ctx.names.find(b"GlyphDirectory")?;
    let gd_obj = ctx.dicts.get(cidfont_entity, &DictKey::Name(gd_name))?;
    let gd_entity = match gd_obj.value {
        PsValue::Dict(e) => e,
        _ => return None,
    };

    // Get numGlyphs from maxp table
    let (maxp_off, _) = truetype::find_table(raw_sfnts, b"maxp")?;
    if maxp_off + 6 > raw_sfnts.len() {
        return None;
    }
    let num_glyphs = truetype::read_u16(raw_sfnts, maxp_off + 4) as usize;
    if num_glyphs == 0 {
        return None;
    }

    // Build glyf table from GlyphDirectory entries
    let mut glyf_data: Vec<u8> = Vec::new();
    let mut loca_offsets: Vec<u32> = Vec::with_capacity(num_glyphs + 1);

    for gid in 0..num_glyphs {
        loca_offsets.push(glyf_data.len() as u32);
        if let Some(entry_obj) = ctx.dicts.get(gd_entity, &DictKey::Int(gid as i32)) {
            if let PsValue::String { entity, start, len } = entry_obj.value {
                let glyph_bytes = ctx.strings.get(entity, start, len);
                if !glyph_bytes.is_empty() {
                    glyf_data.extend_from_slice(glyph_bytes);
                    // Pad to 2-byte alignment
                    if glyf_data.len() % 2 != 0 {
                        glyf_data.push(0);
                    }
                }
            }
        }
        // Empty glyphs get same offset as next → zero-length
    }
    // Final loca entry = total glyf size
    loca_offsets.push(glyf_data.len() as u32);

    // Build loca table (long format, uint32 entries)
    let mut loca_data = Vec::with_capacity(loca_offsets.len() * 4);
    for &offset in &loca_offsets {
        loca_data.extend_from_slice(&offset.to_be_bytes());
    }

    // Parse existing table directory to collect table data
    let num_tables_raw = if raw_sfnts.len() >= 6 {
        truetype::read_u16(raw_sfnts, 4) as usize
    } else {
        return None;
    };

    let mut keep_tables: Vec<(&[u8], Vec<u8>)> = Vec::new();
    for i in 0..num_tables_raw {
        let entry_off = 12 + i * 16;
        if entry_off + 16 > raw_sfnts.len() {
            break;
        }
        let tag = &raw_sfnts[entry_off..entry_off + 4];
        let tbl_offset = truetype::read_u32(raw_sfnts, entry_off + 8) as usize;
        let tbl_length = truetype::read_u32(raw_sfnts, entry_off + 12) as usize;

        // Skip gdir (non-standard) and any existing glyf/loca
        if tag == b"gdir" || tag == b"glyf" || tag == b"loca" {
            continue;
        }

        if tbl_offset + tbl_length <= raw_sfnts.len() {
            let mut data = raw_sfnts[tbl_offset..tbl_offset + tbl_length].to_vec();
            // Update head table: set indexToLocFormat = 1 (long format)
            if tag == b"head" && data.len() >= 52 {
                data[50] = 0;
                data[51] = 1; // indexToLocFormat = 1 (long)
            }
            keep_tables.push((tag, data));
        }
    }

    // Add new glyf and loca tables
    keep_tables.push((b"glyf", glyf_data));
    keep_tables.push((b"loca", loca_data));

    // Sort by tag
    keep_tables.sort_by_key(|(tag, _)| *tag);

    Some(assemble_truetype(&keep_tables))
}

/// Assemble a TrueType font binary from a list of (tag, data) pairs.
///
/// Builds the offset table, table directory, and concatenated table data
/// with proper alignment and TrueType checksums.
fn assemble_truetype(tables: &[(&[u8], Vec<u8>)]) -> Vec<u8> {
    let num_tables = tables.len();
    let entry_selector = if num_tables > 0 {
        (num_tables as f64).log2().floor() as u16
    } else {
        0
    };
    let search_range = (1u16 << entry_selector) * 16;
    let range_shift = (num_tables as u16) * 16 - search_range;

    let header_size = 12 + num_tables * 16;
    let mut current_offset = header_size;

    // Plan table layout with 4-byte padding
    let mut table_entries: Vec<(&[u8], &[u8], usize, usize)> = Vec::new();
    for (tag, data) in tables {
        let padded_len = (data.len() + 3) & !3;
        table_entries.push((tag, data.as_slice(), current_offset, padded_len));
        current_offset += padded_len;
    }

    let mut result = Vec::with_capacity(current_offset);

    // Offset table (sfVersion 1.0)
    result.extend_from_slice(&0x00010000u32.to_be_bytes());
    result.extend_from_slice(&(num_tables as u16).to_be_bytes());
    result.extend_from_slice(&search_range.to_be_bytes());
    result.extend_from_slice(&entry_selector.to_be_bytes());
    result.extend_from_slice(&range_shift.to_be_bytes());

    // Table directory entries
    for &(tag, data, offset, _padded_len) in &table_entries {
        let checksum = calc_table_checksum(data);
        result.extend_from_slice(tag);
        result.extend_from_slice(&checksum.to_be_bytes());
        result.extend_from_slice(&(offset as u32).to_be_bytes());
        result.extend_from_slice(&(data.len() as u32).to_be_bytes());
    }

    // Table data with 4-byte padding
    for &(_tag, data, _offset, padded_len) in &table_entries {
        result.extend_from_slice(data);
        let padding = padded_len - data.len();
        for _ in 0..padding {
            result.push(0);
        }
    }

    result
}

/// Calculate TrueType table checksum (sum of uint32 values).
fn calc_table_checksum(data: &[u8]) -> u32 {
    let mut total: u32 = 0;
    let mut i = 0;
    while i + 4 <= data.len() {
        total = total.wrapping_add(u32::from_be_bytes([
            data[i],
            data[i + 1],
            data[i + 2],
            data[i + 3],
        ]));
        i += 4;
    }
    // Handle remaining bytes (pad with zeros)
    if i < data.len() {
        let mut last = [0u8; 4];
        for (j, &b) in data[i..].iter().enumerate() {
            last[j] = b;
        }
        total = total.wrapping_add(u32::from_be_bytes(last));
    }
    total
}

/// Build a CID (Type 0 / Type 42) font resource for PDF.
///
/// Creates the PDF Type 0 → CIDFontType2 → FontFile2 hierarchy
/// with Identity-H encoding and a ToUnicode CMap.
fn build_cid_font(writer: &mut PdfWriter, usage: &FontUsage, ctx: &Context) -> Option<u32> {
    let font_entity = usage.font_entity;

    // Navigate to CIDFont descendant: FDepVector[0]
    let cidfont_entity = match get_cidfont_descendant(ctx, font_entity) {
        Some(e) => e,
        None => {
            // Type 42 fonts have sfnts directly on the font dict (not Type 0 wrapper)
            return build_type42_font(writer, usage, ctx);
        }
    };

    // Extract TrueType binary from sfnts array
    let raw_font_data = concatenate_sfnts(ctx, cidfont_entity)?;

    // Check if glyf/loca tables exist — CUPS-style fonts store glyph data
    // in PostScript GlyphDirectory instead of TrueType tables
    let needs_reconstruction = truetype::find_table(&raw_font_data, b"glyf").is_none()
        || truetype::find_table(&raw_font_data, b"loca").is_none();
    let font_data = if needs_reconstruction {
        match reconstruct_truetype(&raw_font_data, ctx, cidfont_entity) {
            Some(reconstructed) => reconstructed,
            None => raw_font_data,
        }
    } else {
        raw_font_data
    };

    let units_per_em = truetype::get_units_per_em(&font_data);
    let scale = 1000.0 / units_per_em as f64;

    // Build CID → GID mapping
    // For reconstructed fonts (glyf built from GlyphDirectory with CID=GID),
    // use identity mapping. For normal fonts, parse cmap table.
    let cid_to_gid = if needs_reconstruction {
        // Identity: CID = GID, just map used codes to themselves
        usage
            .used_codes
            .iter()
            .map(|&cid| (cid, cid))
            .collect::<HashMap<u16, u16>>()
    } else {
        build_cid_to_gid_from_cmap(&font_data)
    };
    let max_cid = usage.used_codes.iter().copied().max().unwrap_or(0);

    // Build /W array (compact widths)
    let w_array = build_w_array(&font_data, &usage.used_codes, &cid_to_gid, scale);

    // Default width (CID 0 / GID 0)
    let default_width = truetype::get_advance_width(&font_data, 0)
        .map(|w| (w as f64 * scale).round() as i64)
        .unwrap_or(1000);

    // CIDToGIDMap: for reconstructed fonts use /Identity name (CID=GID),
    // for normal fonts build a binary stream mapping CID→GID via cmap.
    let cid_to_gid_value = if needs_reconstruction {
        PdfObj::name("Identity")
    } else {
        let cid_to_gid_data = build_cid_to_gid_stream(&cid_to_gid, max_cid);
        PdfObj::Ref(writer.add_stream(Vec::new(), &cid_to_gid_data, true))
    };

    // Subset TrueType font: zero out unused glyf entries to reduce size
    let seed_gids: HashSet<u16> = usage
        .used_codes
        .iter()
        .filter_map(|&cid| cid_to_gid.get(&cid).copied())
        .collect();
    let used_gids = compute_used_gids(&font_data, &seed_gids);
    let font_data = subset_truetype(&font_data, &used_gids).unwrap_or(font_data);

    // Embed TrueType binary as FontFile2
    let font_file_entries = vec![(b"Length1".to_vec(), PdfObj::Int(font_data.len() as i64))];
    let font_file2_ref = writer.add_stream(font_file_entries, &font_data, true);

    // Font metrics from TrueType tables
    let bbox = get_truetype_bbox(&font_data, scale);
    let (ascent, descent) = get_truetype_ascent_descent(&font_data, scale);

    // Build FontDescriptor with FontFile2
    let descriptor_ref = {
        let mut entries = vec![
            (b"Type".to_vec(), PdfObj::name("FontDescriptor")),
            (b"FontName".to_vec(), PdfObj::Name(usage.font_name.clone())),
            (b"Flags".to_vec(), PdfObj::Int(0x0004)), // Symbolic
            (
                b"FontBBox".to_vec(),
                PdfObj::Array(bbox.iter().map(|&v| PdfObj::Real(v)).collect()),
            ),
            (b"Ascent".to_vec(), PdfObj::Real(ascent)),
            (b"Descent".to_vec(), PdfObj::Real(descent)),
            (b"StemV".to_vec(), PdfObj::Int(80)),
            (b"ItalicAngle".to_vec(), PdfObj::Int(0)),
            (b"CapHeight".to_vec(), PdfObj::Int(700)),
            (b"FontFile2".to_vec(), PdfObj::Ref(font_file2_ref)),
        ];
        // CIDFontType2 fonts with CIDToGIDMap need Symbolic flag
        // (not Nonsymbolic) per PDF spec
        let _ = &mut entries;
        writer.add_object(&PdfObj::Dict(entries))
    };

    // CIDSystemInfo
    let cid_system_info = PdfObj::Dict(vec![
        (b"Registry".to_vec(), PdfObj::LitString(b"Adobe".to_vec())),
        (
            b"Ordering".to_vec(),
            PdfObj::LitString(b"Identity".to_vec()),
        ),
        (b"Supplement".to_vec(), PdfObj::Int(0)),
    ]);

    // Build CIDFont dict (the descendant)
    let mut cid_font_entries: Vec<(Vec<u8>, PdfObj)> = vec![
        (b"Type".to_vec(), PdfObj::name("Font")),
        (b"Subtype".to_vec(), PdfObj::name("CIDFontType2")),
        (b"BaseFont".to_vec(), PdfObj::Name(usage.font_name.clone())),
        (b"CIDSystemInfo".to_vec(), cid_system_info),
        (b"FontDescriptor".to_vec(), PdfObj::Ref(descriptor_ref)),
        (b"DW".to_vec(), PdfObj::Int(default_width)),
        (b"CIDToGIDMap".to_vec(), cid_to_gid_value),
    ];
    if !w_array.is_empty() {
        cid_font_entries.push((b"W".to_vec(), PdfObj::Array(w_array)));
    }
    let cid_font_ref = writer.add_object(&PdfObj::Dict(cid_font_entries));

    // Build ToUnicode CMap for 2-byte CID codes
    let tounicode_map = build_cid_tounicode(&font_data, &usage.used_codes, &cid_to_gid);
    let tounicode_ref = if !tounicode_map.is_empty() {
        let font_name_str = String::from_utf8_lossy(&usage.font_name);
        let cmap_data = generate_cid_tounicode_cmap(&tounicode_map, &font_name_str);
        Some(writer.add_stream(Vec::new(), &cmap_data, true))
    } else {
        None
    };

    // Build Type 0 wrapper font
    let mut type0_entries: Vec<(Vec<u8>, PdfObj)> = vec![
        (b"Type".to_vec(), PdfObj::name("Font")),
        (b"Subtype".to_vec(), PdfObj::name("Type0")),
        (b"BaseFont".to_vec(), PdfObj::Name(usage.font_name.clone())),
        (b"Encoding".to_vec(), PdfObj::name("Identity-H")),
        (
            b"DescendantFonts".to_vec(),
            PdfObj::Array(vec![PdfObj::Ref(cid_font_ref)]),
        ),
    ];
    if let Some(tu_ref) = tounicode_ref {
        type0_entries.push((b"ToUnicode".to_vec(), PdfObj::Ref(tu_ref)));
    }

    Some(writer.add_object(&PdfObj::Dict(type0_entries)))
}

/// Navigate from a Type 0 font dict to its CIDFont descendant (FDepVector[0]).
fn get_cidfont_descendant(ctx: &Context, font_entity: EntityId) -> Option<EntityId> {
    let fdep_name = ctx.names.find(b"FDepVector")?;
    let fdep_obj = ctx.dicts.get(font_entity, &DictKey::Name(fdep_name))?;
    let (arr_entity, arr_start, _) = match fdep_obj.value {
        PsValue::Array { entity, start, len } => (entity, start, len),
        _ => return None,
    };
    let cidfont_obj = ctx.arrays.get_element(arr_entity, arr_start);
    match cidfont_obj.value {
        PsValue::Dict(e) => Some(e),
        _ => None,
    }
}

/// Concatenate sfnts array from a CIDFont/Type42 dict into raw TrueType data.
fn concatenate_sfnts(ctx: &Context, dict_entity: EntityId) -> Option<Vec<u8>> {
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

/// Build CID → GID mapping from TrueType cmap table.
///
/// Parses cmap format 4 (BMP) and format 12 (full Unicode) subtables
/// to build a reverse mapping from Unicode code point (used as CID) to GID.
fn build_cid_to_gid_from_cmap(font_data: &[u8]) -> HashMap<u16, u16> {
    let mut map = HashMap::new();

    let Some((cmap_off, _cmap_len)) = truetype::find_table(font_data, b"cmap") else {
        return map;
    };

    if cmap_off + 4 > font_data.len() {
        return map;
    }
    let num_subtables = truetype::read_u16(font_data, cmap_off + 2) as usize;

    for i in 0..num_subtables {
        let rec_off = cmap_off + 4 + i * 8;
        if rec_off + 8 > font_data.len() {
            break;
        }
        let platform_id = truetype::read_u16(font_data, rec_off);
        let encoding_id = truetype::read_u16(font_data, rec_off + 2);
        let subtable_offset = truetype::read_u32(font_data, rec_off + 4) as usize;

        // We want Unicode subtables: (0, *) or (3, 1) or (3, 10)
        let is_unicode =
            platform_id == 0 || (platform_id == 3 && (encoding_id == 1 || encoding_id == 10));
        if !is_unicode {
            continue;
        }

        let st_off = cmap_off + subtable_offset;
        if st_off + 2 > font_data.len() {
            continue;
        }
        let format = truetype::read_u16(font_data, st_off);

        match format {
            4 => parse_cmap_format4(font_data, st_off, &mut map),
            12 => parse_cmap_format12(font_data, st_off, &mut map),
            _ => {}
        }
    }

    map
}

/// Parse cmap format 4 (segment mapping to delta values).
fn parse_cmap_format4(font_data: &[u8], offset: usize, map: &mut HashMap<u16, u16>) {
    if offset + 14 > font_data.len() {
        return;
    }
    let seg_count = truetype::read_u16(font_data, offset + 6) as usize / 2;
    let end_codes_off = offset + 14;
    let start_codes_off = end_codes_off + seg_count * 2 + 2; // +2 for reservedPad
    let id_delta_off = start_codes_off + seg_count * 2;
    let id_range_off = id_delta_off + seg_count * 2;

    for seg in 0..seg_count {
        if end_codes_off + seg * 2 + 2 > font_data.len()
            || start_codes_off + seg * 2 + 2 > font_data.len()
            || id_delta_off + seg * 2 + 2 > font_data.len()
            || id_range_off + seg * 2 + 2 > font_data.len()
        {
            break;
        }
        let end_code = truetype::read_u16(font_data, end_codes_off + seg * 2);
        let start_code = truetype::read_u16(font_data, start_codes_off + seg * 2);
        let id_delta = truetype::read_i16(font_data, id_delta_off + seg * 2) as i32;
        let id_range_offset = truetype::read_u16(font_data, id_range_off + seg * 2) as usize;

        if start_code == 0xFFFF {
            break;
        }

        for code in start_code..=end_code {
            let gid = if id_range_offset == 0 {
                ((code as i32 + id_delta) & 0xFFFF) as u16
            } else {
                let glyph_idx_off = id_range_off
                    + seg * 2
                    + id_range_offset
                    + (code as usize - start_code as usize) * 2;
                if glyph_idx_off + 2 <= font_data.len() {
                    let gid_raw = truetype::read_u16(font_data, glyph_idx_off);
                    if gid_raw != 0 {
                        ((gid_raw as i32 + id_delta) & 0xFFFF) as u16
                    } else {
                        0
                    }
                } else {
                    0
                }
            };
            if gid != 0 {
                map.insert(code, gid);
            }
        }
    }
}

/// Parse cmap format 12 (segmented coverage, 32-bit).
fn parse_cmap_format12(font_data: &[u8], offset: usize, map: &mut HashMap<u16, u16>) {
    if offset + 16 > font_data.len() {
        return;
    }
    let n_groups = truetype::read_u32(font_data, offset + 12) as usize;
    let groups_off = offset + 16;

    for i in 0..n_groups {
        let g_off = groups_off + i * 12;
        if g_off + 12 > font_data.len() {
            break;
        }
        let start_char = truetype::read_u32(font_data, g_off);
        let end_char = truetype::read_u32(font_data, g_off + 4);
        let start_gid = truetype::read_u32(font_data, g_off + 8);

        for code in start_char..=end_char.min(0xFFFF) {
            let gid = start_gid + (code - start_char);
            if gid != 0 && gid <= 0xFFFF {
                map.insert(code as u16, gid as u16);
            }
        }
    }
}

/// Build the PDF /W array for CID font widths.
///
/// Format: [cid1 [w1 w2 w3 ...] cid2 [w4 w5 ...] ...]
/// Groups consecutive CIDs that have widths different from the default.
fn build_w_array(
    font_data: &[u8],
    used_codes: &HashSet<u16>,
    cid_to_gid: &HashMap<u16, u16>,
    scale: f64,
) -> Vec<PdfObj> {
    // Collect (cid, width) pairs, sorted by CID
    let mut cid_widths: Vec<(u16, i64)> = Vec::new();
    for &cid in used_codes {
        let gid = cid_to_gid.get(&cid).copied().unwrap_or(cid);
        if let Some(aw) = truetype::get_advance_width(font_data, gid) {
            let w = (aw as f64 * scale).round() as i64;
            cid_widths.push((cid, w));
        }
    }
    cid_widths.sort_by_key(|&(cid, _)| cid);

    // Group into consecutive runs
    let mut result: Vec<PdfObj> = Vec::new();
    let mut i = 0;
    while i < cid_widths.len() {
        let start_cid = cid_widths[i].0;
        let mut widths = vec![PdfObj::Int(cid_widths[i].1)];
        let mut j = i + 1;
        while j < cid_widths.len() && cid_widths[j].0 == cid_widths[j - 1].0 + 1 {
            widths.push(PdfObj::Int(cid_widths[j].1));
            j += 1;
        }
        result.push(PdfObj::Int(start_cid as i64));
        result.push(PdfObj::Array(widths));
        i = j;
    }

    result
}

/// Build CIDToGIDMap binary stream.
/// 2 bytes (big-endian) per CID, mapping CID → GID.
fn build_cid_to_gid_stream(cid_to_gid: &HashMap<u16, u16>, max_cid: u16) -> Vec<u8> {
    let len = (max_cid as usize + 1) * 2;
    let mut data = vec![0u8; len];
    for (&cid, &gid) in cid_to_gid {
        if (cid as usize) <= max_cid as usize {
            let off = cid as usize * 2;
            data[off] = (gid >> 8) as u8;
            data[off + 1] = (gid & 0xFF) as u8;
        }
    }
    data
}

/// Build CID → Unicode mapping for ToUnicode CMap.
///
/// For TrueType CID fonts where CID ≈ Unicode code point (via cmap table),
/// we can reverse the cmap to get CID → Unicode.
fn build_cid_tounicode(
    font_data: &[u8],
    used_codes: &HashSet<u16>,
    cid_to_gid: &HashMap<u16, u16>,
) -> HashMap<u16, u16> {
    // Build reverse map: GID → Unicode from cmap
    let mut gid_to_unicode: HashMap<u16, u16> = HashMap::new();

    if let Some((cmap_off, _)) = truetype::find_table(font_data, b"cmap") {
        if cmap_off + 4 <= font_data.len() {
            let num_subtables = truetype::read_u16(font_data, cmap_off + 2) as usize;
            for i in 0..num_subtables {
                let rec_off = cmap_off + 4 + i * 8;
                if rec_off + 8 > font_data.len() {
                    break;
                }
                let platform_id = truetype::read_u16(font_data, rec_off);
                let encoding_id = truetype::read_u16(font_data, rec_off + 2);
                let st_offset = truetype::read_u32(font_data, rec_off + 4) as usize;

                let is_unicode = platform_id == 0
                    || (platform_id == 3 && (encoding_id == 1 || encoding_id == 10));
                if !is_unicode {
                    continue;
                }

                let st_off = cmap_off + st_offset;
                if st_off + 2 > font_data.len() {
                    continue;
                }
                // Parse the cmap subtable to build unicode→gid, then reverse it
                let mut unicode_to_gid = HashMap::new();
                let format = truetype::read_u16(font_data, st_off);
                match format {
                    4 => parse_cmap_format4(font_data, st_off, &mut unicode_to_gid),
                    12 => parse_cmap_format12(font_data, st_off, &mut unicode_to_gid),
                    _ => {}
                }
                for (unicode, gid) in unicode_to_gid {
                    gid_to_unicode.entry(gid).or_insert(unicode);
                }
                break; // Use first Unicode subtable
            }
        }
    }

    // For each used CID, find Unicode via CID → GID → Unicode
    let mut map = HashMap::new();
    for &cid in used_codes {
        let gid = cid_to_gid.get(&cid).copied().unwrap_or(cid);
        if let Some(&unicode) = gid_to_unicode.get(&gid) {
            map.insert(cid, unicode);
        } else if cid > 0 && cid < 0xFFFE {
            // Fallback: assume CID = Unicode (common for Identity-H CMaps)
            map.insert(cid, cid);
        }
    }
    map
}

/// Generate a ToUnicode CMap for CID fonts (2-byte codespace).
fn generate_cid_tounicode_cmap(map: &HashMap<u16, u16>, font_name: &str) -> Vec<u8> {
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
    lines.push(b"<0000> <FFFF>".to_vec());
    lines.push(b"endcodespacerange".to_vec());

    let mut sorted: Vec<_> = map.iter().collect();
    sorted.sort_by_key(|&(&code, _)| code);

    for chunk in sorted.chunks(100) {
        lines.push(format!("{} beginbfchar", chunk.len()).into_bytes());
        for &(&cid, &unicode) in chunk {
            lines.push(format!("<{:04X}> <{:04X}>", cid, unicode).into_bytes());
        }
        lines.push(b"endbfchar".to_vec());
    }

    lines.push(b"endcmap".to_vec());
    lines.push(b"CMapName currentdict /CMap defineresource pop".to_vec());
    lines.push(b"end".to_vec());
    lines.push(b"end".to_vec());

    lines.join(&b'\n')
}

/// Get font bounding box from TrueType head table, scaled to 1000-unit space.
fn get_truetype_bbox(font_data: &[u8], scale: f64) -> [f64; 4] {
    if let Some((head_off, _)) = truetype::find_table(font_data, b"head") {
        if head_off + 44 <= font_data.len() {
            let x_min = truetype::read_i16(font_data, head_off + 36) as f64 * scale;
            let y_min = truetype::read_i16(font_data, head_off + 38) as f64 * scale;
            let x_max = truetype::read_i16(font_data, head_off + 40) as f64 * scale;
            let y_max = truetype::read_i16(font_data, head_off + 42) as f64 * scale;
            return [x_min, y_min, x_max, y_max];
        }
    }
    [0.0, -200.0, 1000.0, 800.0]
}

/// Get ascent and descent from TrueType hhea or OS/2 table, scaled to 1000-unit space.
fn get_truetype_ascent_descent(font_data: &[u8], scale: f64) -> (f64, f64) {
    // Try hhea table first
    if let Some((hhea_off, _)) = truetype::find_table(font_data, b"hhea") {
        if hhea_off + 8 <= font_data.len() {
            let ascent = truetype::read_i16(font_data, hhea_off + 4) as f64 * scale;
            let descent = truetype::read_i16(font_data, hhea_off + 6) as f64 * scale;
            return (ascent, descent);
        }
    }
    (800.0, -200.0)
}

/// Build a standalone Type 42 (TrueType) font resource for PDF.
///
/// Type 42 fonts have sfnts directly on the font dict (not wrapped in Type 0).
/// They use single-byte encoding with CharStrings mapping glyph names to GIDs.
/// In PDF, these become `/Subtype /TrueType` with `/FontFile2`.
fn build_type42_font(writer: &mut PdfWriter, usage: &FontUsage, ctx: &Context) -> Option<u32> {
    let font_entity = usage.font_entity;
    let font_name_str = String::from_utf8_lossy(&usage.font_name);

    // Extract TrueType binary from sfnts array on the font dict itself
    let font_data = concatenate_sfnts(ctx, font_entity)?;
    let units_per_em = truetype::get_units_per_em(&font_data);
    let scale = 1000.0 / units_per_em as f64;

    // Get encoding array
    let encoding_entity = ctx
        .dicts
        .get(font_entity, &DictKey::Name(ctx.name_cache.n_encoding))
        .and_then(|obj| match obj.value {
            PsValue::Array { entity, .. } => Some(entity),
            _ => None,
        });

    // Get CharStrings dict (maps glyph names → GID integers for Type 42)
    let charstrings_entity = ctx
        .dicts
        .get(font_entity, &DictKey::Name(ctx.name_cache.n_char_strings))
        .and_then(|obj| match obj.value {
            PsValue::Dict(e) => Some(e),
            _ => None,
        });

    // Determine FirstChar/LastChar
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

    // Extract widths via Encoding → CharStrings(glyph_name → GID) → hmtx
    let mut widths: HashMap<u16, i32> = HashMap::new();
    if let (Some(enc_entity), Some(cs_entity)) = (encoding_entity, charstrings_entity) {
        for code in first_char..=last_char {
            let glyph_name_obj = ctx.arrays.get_element(enc_entity, code as u32);
            let glyph_name_id = match glyph_name_obj.value {
                PsValue::Name(id) => id,
                _ => continue,
            };

            // CharStrings maps glyph name → GID (integer) for Type 42
            let cs_obj = match ctx.dicts.get(cs_entity, &DictKey::Name(glyph_name_id)) {
                Some(obj) => obj,
                None => continue,
            };

            let gid = match cs_obj.as_i32() {
                Some(g) => g as u16,
                None => continue,
            };

            if let Some(aw) = truetype::get_advance_width(&font_data, gid) {
                widths.insert(code, (aw as f64 * scale).round() as i32);
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

    // Subset TrueType font: collect used GIDs via Encoding → CharStrings mapping
    let font_data =
        if let (Some(enc_entity), Some(cs_entity)) = (encoding_entity, charstrings_entity) {
            let mut seed_gids: HashSet<u16> = HashSet::new();
            for &code in &usage.used_codes {
                if code > 255 {
                    continue;
                }
                let glyph_name_obj = ctx.arrays.get_element(enc_entity, code as u32);
                if let PsValue::Name(name_id) = glyph_name_obj.value {
                    if let Some(cs_obj) = ctx.dicts.get(cs_entity, &DictKey::Name(name_id)) {
                        if let Some(gid) = cs_obj.as_i32() {
                            seed_gids.insert(gid as u16);
                        }
                    }
                }
            }
            if seed_gids.is_empty() {
                font_data // can't determine used GIDs, embed whole
            } else {
                let used_gids = compute_used_gids(&font_data, &seed_gids);
                subset_truetype(&font_data, &used_gids).unwrap_or(font_data)
            }
        } else {
            font_data // no Encoding/CharStrings, embed whole
        };

    // Embed TrueType binary as FontFile2
    let font_file_entries = vec![(b"Length1".to_vec(), PdfObj::Int(font_data.len() as i64))];
    let font_file2_ref = writer.add_stream(font_file_entries, &font_data, true);

    // Font metrics
    let bbox = get_truetype_bbox(&font_data, scale);
    let (ascent, descent) = get_truetype_ascent_descent(&font_data, scale);

    // Build FontDescriptor with FontFile2
    let descriptor_ref = {
        let entries = vec![
            (b"Type".to_vec(), PdfObj::name("FontDescriptor")),
            (b"FontName".to_vec(), PdfObj::Name(usage.font_name.clone())),
            (b"Flags".to_vec(), PdfObj::Int(0x0020)), // Nonsymbolic
            (
                b"FontBBox".to_vec(),
                PdfObj::Array(bbox.iter().map(|&v| PdfObj::Real(v)).collect()),
            ),
            (b"Ascent".to_vec(), PdfObj::Real(ascent)),
            (b"Descent".to_vec(), PdfObj::Real(descent)),
            (b"StemV".to_vec(), PdfObj::Int(80)),
            (b"ItalicAngle".to_vec(), PdfObj::Int(0)),
            (b"CapHeight".to_vec(), PdfObj::Int(700)),
            (b"FontFile2".to_vec(), PdfObj::Ref(font_file2_ref)),
        ];
        writer.add_object(&PdfObj::Dict(entries))
    };

    // Build Encoding with Differences
    let encoding_obj = build_encoding_differences(ctx, encoding_entity, first_char, last_char);

    // Build Font dict with TrueType subtype
    let mut font_entries: Vec<(Vec<u8>, PdfObj)> = vec![
        (b"Type".to_vec(), PdfObj::name("Font")),
        (b"Subtype".to_vec(), PdfObj::name("TrueType")),
        (b"BaseFont".to_vec(), PdfObj::Name(usage.font_name.clone())),
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

/// Extract glyph widths for a font (in 1000ths of a unit) without building PDF objects.
///
/// Used to populate FontUsage::widths for TJ kern value computation.
/// Returns a map from character code (or CID) to width in 1000ths of text space.
pub fn extract_widths(usage: &FontUsage, ctx: &Context) -> HashMap<u16, i32> {
    match usage.font_type {
        1 => extract_type1_widths(usage, ctx),
        2 => extract_type2_widths(usage, ctx),
        0 | 42 => extract_cid_widths(usage, ctx),
        _ => HashMap::new(),
    }
}

/// Extract widths from a Type 1 font via CharString interpretation.
fn extract_type1_widths(usage: &FontUsage, ctx: &Context) -> HashMap<u16, i32> {
    let font_entity = usage.font_entity;

    let encoding_entity = ctx
        .dicts
        .get(font_entity, &DictKey::Name(ctx.name_cache.n_encoding))
        .and_then(|obj| match obj.value {
            PsValue::Array { entity, .. } => Some(entity),
            _ => None,
        });

    let charstrings_entity = ctx
        .dicts
        .get(font_entity, &DictKey::Name(ctx.name_cache.n_char_strings))
        .and_then(|obj| match obj.value {
            PsValue::Dict(e) => Some(e),
            _ => None,
        });

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

    let subrs = get_decrypted_subrs(ctx, private_entity, len_iv);

    let mut widths: HashMap<u16, i32> = HashMap::new();
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
            }
        }
    }
    widths
}

/// Extract widths from a Type 2 (CFF) font via Type 2 charstring interpreter.
fn extract_type2_widths(usage: &FontUsage, ctx: &Context) -> HashMap<u16, i32> {
    let font_entity = usage.font_entity;

    let encoding_entity = ctx
        .dicts
        .get(font_entity, &DictKey::Name(ctx.name_cache.n_encoding))
        .and_then(|obj| match obj.value {
            PsValue::Array { entity, .. } => Some(entity),
            _ => None,
        });

    let charstrings_entity = ctx
        .dicts
        .get(font_entity, &DictKey::Name(ctx.name_cache.n_char_strings))
        .and_then(|obj| match obj.value {
            PsValue::Dict(e) => Some(e),
            _ => None,
        });

    let private_entity = ctx
        .dicts
        .get(font_entity, &DictKey::Name(ctx.name_cache.n_private))
        .and_then(|obj| match obj.value {
            PsValue::Dict(e) => Some(e),
            _ => None,
        });

    let mut default_width_x = 0.0;
    let mut nominal_width_x = 0.0;
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
    }

    let mut local_subrs: Vec<Vec<u8>> = Vec::new();
    if let Some(pe) = private_entity
        && let Some(name_id) = ctx.names.find(b"Subrs")
        && let Some(obj) = ctx.dicts.get(pe, &DictKey::Name(name_id))
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

    let mut widths: HashMap<u16, i32> = HashMap::new();
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
    widths
}

/// Extract widths from a CID/Type 42 font via TrueType hmtx table.
fn extract_cid_widths(usage: &FontUsage, ctx: &Context) -> HashMap<u16, i32> {
    let font_entity = usage.font_entity;

    // For Type 0, find CIDFont descendant
    let cid_entity = if usage.font_type == 0 {
        get_cidfont_descendant(ctx, font_entity).unwrap_or(font_entity)
    } else {
        font_entity
    };

    let font_data = match concatenate_sfnts(ctx, cid_entity) {
        Some(d) => d,
        None => return HashMap::new(),
    };

    if font_data.len() < 20 {
        return HashMap::new();
    }

    let upm = truetype::get_units_per_em(&font_data);
    let scale = 1000.0 / upm as f64;

    let mut cid_to_gid: HashMap<u16, u16> = HashMap::new();

    if usage.font_type == 0 {
        cid_to_gid = build_cid_to_gid_from_cmap(&font_data);
    }

    if usage.font_type == 42 {
        let encoding_entity = ctx
            .dicts
            .get(font_entity, &DictKey::Name(ctx.name_cache.n_encoding))
            .and_then(|obj| match obj.value {
                PsValue::Array { entity, .. } => Some(entity),
                _ => None,
            });

        let charstrings_entity = ctx
            .dicts
            .get(font_entity, &DictKey::Name(ctx.name_cache.n_char_strings))
            .and_then(|obj| match obj.value {
                PsValue::Dict(e) => Some(e),
                _ => None,
            });

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
                if let Some(gid) = cs_obj.as_i32() {
                    cid_to_gid.insert(code, gid as u16);
                }
            }
        }
    }

    let mut widths: HashMap<u16, i32> = HashMap::new();
    for &code in &usage.used_codes {
        let gid = cid_to_gid.get(&code).copied().unwrap_or(code);
        if let Some(aw) = truetype::get_advance_width(&font_data, gid) {
            widths.insert(code, (aw as f64 * scale).round() as i32);
        }
    }
    widths
}
