// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF font resolution and glyph rendering.

use std::collections::HashMap;
use std::sync::Arc;

use stet_core::cff_parser::{CffFont, parse_cff};
use stet_core::charstring::execute_charstring;
use stet_core::encoding::{MACROMAN_ENCODING, STANDARD_ENCODING, WINANSI_ENCODING};
use stet_core::graphics_state::{Matrix, PsPath};
use stet_core::truetype::{get_glyf_data, get_units_per_em, parse_cmap, parse_glyf_to_path};
use stet_core::type1_parser::parse_type1;
use stet_core::type2_charstring::execute_type2_charstring;

use crate::error::PdfError;
use crate::objects::{PdfDict, PdfObj};
use crate::resolver::Resolver;

/// Resolved PDF font, ready for glyph rendering.
pub enum PdfFont {
    Type1(Type1PdfFont),
    TrueType(TrueTypePdfFont),
    Cff(CffPdfFont),
}

pub struct Type1PdfFont {
    pub font: stet_core::type1_parser::Type1Font,
    pub encoding: [Option<String>; 256],
    pub widths: [f64; 256],
    pub font_matrix: Matrix,
}

pub struct TrueTypePdfFont {
    pub data: Vec<u8>,
    pub encoding: [Option<String>; 256],
    pub widths: [f64; 256],
    pub cmap: HashMap<u32, u16>,
    pub units_per_em: f64,
}

pub struct CffPdfFont {
    pub font: CffFont,
    pub encoding: [Option<String>; 256],
    pub widths: [f64; 256],
    pub font_matrix: Matrix,
}

/// Font cache: font resource name → resolved font.
pub type FontCache = HashMap<Vec<u8>, Arc<PdfFont>>;

/// Resolve a PDF font dict into a PdfFont ready for rendering.
pub fn resolve_font(resolver: &Resolver, font_ref: &PdfObj) -> Result<PdfFont, PdfError> {
    let font_obj = resolver.deref(font_ref)?;
    let font_dict = font_obj
        .as_dict()
        .ok_or(PdfError::Other("Font is not a dict".into()))?;

    let subtype = font_dict.get_name(b"Subtype").unwrap_or(b"Type1");
    let first_char = font_dict.get_int(b"FirstChar").unwrap_or(0) as usize;
    let _last_char = font_dict.get_int(b"LastChar").unwrap_or(255) as usize;

    // Parse widths array from font dict
    let mut widths = [0.0f64; 256];
    if let Some(w_arr) = font_dict.get_array(b"Widths") {
        for (i, obj) in w_arr.iter().enumerate() {
            let code = first_char + i;
            if code < 256 {
                widths[code] = obj.as_f64().unwrap_or(0.0) / 1000.0;
            }
        }
    }

    // Resolve encoding
    let encoding = resolve_encoding(font_dict, resolver)?;

    // Get FontDescriptor
    let descriptor = get_font_descriptor(font_dict, resolver)?;

    // Route based on what font program is actually available in FontDescriptor,
    // not just the /Subtype (which says "Type1" even for CFF-embedded fonts).
    if let Some(ref desc) = descriptor {
        if desc.get(b"FontFile3").is_some() {
            return resolve_cff(resolver, &descriptor, encoding, widths);
        }
        if desc.get(b"FontFile2").is_some() {
            return resolve_truetype(resolver, &descriptor, encoding, widths);
        }
        if desc.get(b"FontFile").is_some() {
            return resolve_type1(resolver, &descriptor, encoding, widths);
        }
    }
    // No embedded font program — try font substitution
    let base_font = font_dict
        .get_name(b"BaseFont")
        .map(|n| String::from_utf8_lossy(n).to_string())
        .unwrap_or_default();

    if let Some(font) = substitute_font(&base_font, encoding.clone(), widths) {
        return Ok(font);
    }

    // Final fallback based on subtype (will likely fail)
    match subtype {
        b"TrueType" => resolve_truetype(resolver, &descriptor, encoding, widths),
        _ => resolve_type1(resolver, &descriptor, encoding, widths),
    }
}

/// Get the FontDescriptor dict if present.
fn get_font_descriptor(
    font_dict: &PdfDict,
    resolver: &Resolver,
) -> Result<Option<PdfDict>, PdfError> {
    if let Some(fd_ref) = font_dict.get(b"FontDescriptor") {
        let fd_obj = resolver.deref(fd_ref)?;
        if let Some(d) = fd_obj.as_dict() {
            return Ok(Some(d.clone()));
        }
    }
    Ok(None)
}

/// Resolve encoding from font dict.
///
/// Priority: /Encoding dict with /Differences overlay > /Encoding name > StandardEncoding.
fn resolve_encoding(
    font_dict: &PdfDict,
    resolver: &Resolver,
) -> Result<[Option<String>; 256], PdfError> {
    let mut encoding: [Option<String>; 256] = std::array::from_fn(|_| None);

    // Start with a base encoding
    let mut base_table = &STANDARD_ENCODING[..];

    if let Some(enc_obj) = font_dict.get(b"Encoding") {
        let enc_resolved = resolver.deref(enc_obj)?;
        match &enc_resolved {
            PdfObj::Name(name) => {
                base_table = encoding_table_by_name(name);
            }
            PdfObj::Dict(enc_dict) => {
                // Dict encoding: optional BaseEncoding + Differences
                if let Some(base_name) = enc_dict.get_name(b"BaseEncoding") {
                    base_table = encoding_table_by_name(base_name);
                }
                // Apply base
                for (i, &name) in base_table.iter().enumerate() {
                    if name != ".notdef" {
                        encoding[i] = Some(name.to_string());
                    }
                }
                // Apply Differences array
                if let Some(diffs) = enc_dict.get_array(b"Differences") {
                    let mut code = 0usize;
                    for obj in diffs {
                        let obj = resolver.deref(obj).unwrap_or(obj.clone());
                        match &obj {
                            PdfObj::Int(n) => code = *n as usize,
                            PdfObj::Name(name) => {
                                if code < 256 {
                                    encoding[code] =
                                        Some(String::from_utf8_lossy(name).to_string());
                                    code += 1;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                return Ok(encoding);
            }
            _ => {}
        }
    }

    // Apply base table
    for (i, &name) in base_table.iter().enumerate() {
        if name != ".notdef" {
            encoding[i] = Some(name.to_string());
        }
    }

    Ok(encoding)
}

fn encoding_table_by_name(name: &[u8]) -> &'static [&'static str; 256] {
    match name {
        b"WinAnsiEncoding" => &WINANSI_ENCODING,
        b"MacRomanEncoding" => &MACROMAN_ENCODING,
        b"StandardEncoding" => &STANDARD_ENCODING,
        _ => &STANDARD_ENCODING,
    }
}

/// Try to load a substitute font for a non-embedded font.
fn substitute_font(
    base_font: &str,
    encoding: [Option<String>; 256],
    widths: [f64; 256],
) -> Option<PdfFont> {
    use stet_core::font_loader::FONT_SUBSTITUTIONS;

    // Strip subset prefix (e.g. "ABCDEF+Times-Roman" → "Times-Roman")
    let clean_name = if base_font.len() > 7 && base_font.as_bytes().get(6) == Some(&b'+') {
        &base_font[7..]
    } else {
        base_font
    };

    // Look up substitution
    let urw_name = FONT_SUBSTITUTIONS
        .iter()
        .find(|&&(ps, _)| ps == clean_name)
        .map(|&(_, urw)| urw);

    let font_file_name = urw_name.unwrap_or(clean_name);

    // Try to load from resources/Font/
    let font_path = format!("resources/Font/{}.t1", font_file_name);
    let font_data = std::fs::read(&font_path).ok()?;

    let font = parse_type1(&font_data).ok()?;
    let fm = font.font_matrix;
    let font_matrix = Matrix::new(fm[0], fm[1], fm[2], fm[3], fm[4], fm[5]);

    Some(PdfFont::Type1(Type1PdfFont {
        font,
        encoding,
        widths,
        font_matrix,
    }))
}

/// Resolve a Type 1 font from its FontDescriptor.
fn resolve_type1(
    resolver: &Resolver,
    descriptor: &Option<PdfDict>,
    encoding: [Option<String>; 256],
    widths: [f64; 256],
) -> Result<PdfFont, PdfError> {
    let desc = descriptor
        .as_ref()
        .ok_or(PdfError::Other("Type1 font missing FontDescriptor".into()))?;
    let ff_ref = desc
        .get(b"FontFile")
        .or_else(|| desc.get(b"FontFile3"))
        .ok_or(PdfError::Other("Type1 font missing FontFile".into()))?;
    let font_data = resolver.stream_data_from_obj(&resolver.deref(ff_ref)?)?;

    // Strip PFB (Printer Font Binary) headers if present
    let font_data = strip_pfb(&font_data);

    let font =
        parse_type1(&font_data).map_err(|e| PdfError::Other(format!("Type1 parse error: {e}")))?;

    let fm = font.font_matrix;
    let font_matrix = Matrix::new(fm[0], fm[1], fm[2], fm[3], fm[4], fm[5]);

    Ok(PdfFont::Type1(Type1PdfFont {
        font,
        encoding,
        widths,
        font_matrix,
    }))
}

/// Resolve a TrueType font from its FontDescriptor.
fn resolve_truetype(
    resolver: &Resolver,
    descriptor: &Option<PdfDict>,
    encoding: [Option<String>; 256],
    widths: [f64; 256],
) -> Result<PdfFont, PdfError> {
    let desc = descriptor.as_ref().ok_or(PdfError::Other(
        "TrueType font missing FontDescriptor".into(),
    ))?;
    let ff_ref = desc
        .get(b"FontFile2")
        .ok_or(PdfError::Other("TrueType font missing FontFile2".into()))?;
    let data = resolver.stream_data_from_obj(&resolver.deref(ff_ref)?)?;

    let units_per_em = get_units_per_em(&data) as f64;
    let cmap = parse_cmap(&data);

    Ok(PdfFont::TrueType(TrueTypePdfFont {
        data,
        encoding,
        widths,
        cmap,
        units_per_em,
    }))
}

/// Resolve a CFF (Type1C) font from its FontDescriptor.
fn resolve_cff(
    resolver: &Resolver,
    descriptor: &Option<PdfDict>,
    encoding: [Option<String>; 256],
    widths: [f64; 256],
) -> Result<PdfFont, PdfError> {
    let desc = descriptor
        .as_ref()
        .ok_or(PdfError::Other("CFF font missing FontDescriptor".into()))?;
    let ff_ref = desc
        .get(b"FontFile3")
        .ok_or(PdfError::Other("CFF font missing FontFile3".into()))?;
    let font_data = resolver.stream_data_from_obj(&resolver.deref(ff_ref)?)?;

    let fonts =
        parse_cff(&font_data).map_err(|e| PdfError::Other(format!("CFF parse error: {e}")))?;
    let font = fonts
        .into_iter()
        .next()
        .ok_or(PdfError::Other("CFF contains no fonts".into()))?;

    let fm = font.font_matrix;
    let font_matrix = Matrix::new(fm[0], fm[1], fm[2], fm[3], fm[4], fm[5]);

    Ok(PdfFont::Cff(CffPdfFont {
        font,
        encoding,
        widths,
        font_matrix,
    }))
}

/// Strip PFB (Printer Font Binary) headers from Type 1 font data.
///
/// PFB format wraps ASCII and binary segments with 6-byte headers:
/// [0x80, type, len_lo, len_lo2, len_hi, len_hi2] + segment data
/// Type 1 = ASCII, Type 2 = binary (eexec), Type 3 = EOF.
fn strip_pfb(data: &[u8]) -> Vec<u8> {
    if data.len() < 2 || data[0] != 0x80 {
        return data.to_vec();
    }
    let mut result = Vec::with_capacity(data.len());
    let mut pos = 0;
    while pos + 6 <= data.len() && data[pos] == 0x80 {
        let segment_type = data[pos + 1];
        if segment_type == 3 {
            break; // EOF marker
        }
        let len = u32::from_le_bytes([data[pos + 2], data[pos + 3], data[pos + 4], data[pos + 5]])
            as usize;
        pos += 6;
        let end = (pos + len).min(data.len());
        result.extend_from_slice(&data[pos..end]);
        pos = end;
    }
    result
}

// === Glyph rendering ===

impl PdfFont {
    /// Get glyph outline path for a character code.
    pub fn glyph_path(&self, char_code: u8) -> Option<PsPath> {
        match self {
            PdfFont::Type1(f) => f.glyph_path(char_code),
            PdfFont::TrueType(f) => f.glyph_path(char_code),
            PdfFont::Cff(f) => f.glyph_path(char_code),
        }
    }

    /// Get width for a character code (in text space units, already ÷1000).
    pub fn glyph_width(&self, char_code: u8) -> f64 {
        match self {
            PdfFont::Type1(f) => f.widths[char_code as usize],
            PdfFont::TrueType(f) => f.widths[char_code as usize],
            PdfFont::Cff(f) => f.widths[char_code as usize],
        }
    }

    /// Font matrix (glyph space → text space).
    pub fn font_matrix(&self) -> Matrix {
        match self {
            PdfFont::Type1(f) => f.font_matrix,
            PdfFont::TrueType(_) => Matrix::identity(), // TrueType uses 1/upm scaling
            PdfFont::Cff(f) => f.font_matrix,
        }
    }
}

impl Type1PdfFont {
    fn glyph_path(&self, char_code: u8) -> Option<PsPath> {
        let glyph_name = self.encoding[char_code as usize].as_deref()?;
        let charstring = self.font.charstrings.get(glyph_name)?;
        let result =
            execute_charstring(charstring, &self.font.subrs, self.font.len_iv, false).ok()?;
        Some(result.path)
    }
}

impl TrueTypePdfFont {
    fn glyph_path(&self, char_code: u8) -> Option<PsPath> {
        let gid = self.char_code_to_gid(char_code)?;
        let glyf_data = get_glyf_data(&self.data, gid)?;
        let data_clone = self.data.clone();
        let path = parse_glyf_to_path(&glyf_data, &|component_gid| {
            get_glyf_data(&data_clone, component_gid)
        });
        if path.is_empty() {
            return None;
        }
        // Scale from font units to text space (÷ unitsPerEm)
        let scale = 1.0 / self.units_per_em;
        let m = Matrix::scale(scale, scale);
        Some(path.transform(&m))
    }

    fn char_code_to_gid(&self, char_code: u8) -> Option<u16> {
        // Try cmap lookup first
        if let Some(&gid) = self.cmap.get(&(char_code as u32)) {
            return Some(gid);
        }
        // Try encoding → glyph name → cmap via Unicode mapping
        // For simple TrueType fonts, char code often maps directly
        if let Some(&gid) = self.cmap.get(&(char_code as u32)) {
            return Some(gid);
        }
        // Fallback: use char code as glyph index directly
        // (some PDFs use identity mapping)
        Some(char_code as u16)
    }
}

impl CffPdfFont {
    fn glyph_path(&self, char_code: u8) -> Option<PsPath> {
        let glyph_name = self.encoding[char_code as usize].as_deref()?;
        // Find glyph index from charset
        let gid = self
            .font
            .charset
            .iter()
            .position(|name| name == glyph_name)?;
        if gid >= self.font.char_strings.len() {
            return None;
        }
        let result = execute_type2_charstring(
            &self.font.char_strings[gid],
            &self.font.local_subrs,
            &self.font.global_subrs,
            self.font.default_width_x,
            self.font.nominal_width_x,
            false,
        )
        .ok()?;
        Some(result.path)
    }
}
