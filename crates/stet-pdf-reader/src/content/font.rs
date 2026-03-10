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
    /// Type 0 composite font (CIDFontType2 with TrueType outlines)
    CidTrueType(CidTrueTypePdfFont),
    /// Type 3 font: glyphs defined as content streams.
    Type3(Type3PdfFont),
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

/// CIDFontType2: TrueType outlines accessed by CID (2-byte char codes).
pub struct CidTrueTypePdfFont {
    pub data: Vec<u8>,
    /// Default glyph width (from /DW, in text space ÷1000).
    pub default_width: f64,
    /// CID → width mapping (from /W array, in text space ÷1000).
    pub cid_widths: HashMap<u16, f64>,
    pub cmap: HashMap<u32, u16>,
    pub units_per_em: f64,
    /// If true, CID maps directly to GID (Identity CIDToGIDMap).
    pub identity_cid_to_gid: bool,
}

/// Type 3 font: glyphs defined as content streams (CharProcs).
pub struct Type3PdfFont {
    /// Char code → decoded content stream bytes for the glyph.
    pub char_procs: HashMap<u8, Vec<u8>>,
    /// Char code → resources dict for the glyph stream (from font dict).
    pub resources: PdfDict,
    pub widths: [f64; 256],
    pub font_matrix: Matrix,
    pub font_bbox: [f64; 4],
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

    // Handle Type 0 composite fonts (CID fonts)
    if subtype == b"Type0" {
        return resolve_type0(resolver, font_dict);
    }

    // Handle Type 3 fonts (glyph content streams)
    if subtype == b"Type3" {
        return resolve_type3(resolver, font_dict);
    }

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

    let base_font_name = font_dict
        .get_name(b"BaseFont")
        .map(|n| String::from_utf8_lossy(n).to_string())
        .unwrap_or_default();

    // Route based on what font program is actually available in FontDescriptor,
    // not just the /Subtype (which says "Type1" even for CFF-embedded fonts).
    if let Some(ref desc) = descriptor {
        if desc.get(b"FontFile3").is_some() {
            // Try embedded CFF; fall back to substitution if decompression/parsing fails
            match resolve_cff(resolver, &descriptor, encoding.clone(), widths) {
                Ok(font) => return Ok(font),
                Err(_) => {
                    if let Some(font) =
                        substitute_font(&base_font_name, encoding.clone(), widths)
                    {
                        return Ok(font);
                    }
                }
            }
        }
        if desc.get(b"FontFile2").is_some() {
            // Try embedded TrueType; fall back to substitution if font data is unusable
            // (some PDFs embed only table metadata without actual glyph outlines)
            match resolve_truetype(resolver, &descriptor, encoding.clone(), widths) {
                Ok(font) => return Ok(font),
                Err(_) => {
                    if let Some(font) =
                        substitute_font(&base_font_name, encoding.clone(), widths)
                    {
                        return Ok(font);
                    }
                }
            }
        }
        if desc.get(b"FontFile").is_some() {
            match resolve_type1(resolver, &descriptor, encoding.clone(), widths) {
                Ok(font) => return Ok(font),
                Err(_) => {
                    if let Some(font) =
                        substitute_font(&base_font_name, encoding.clone(), widths)
                    {
                        return Ok(font);
                    }
                }
            }
        }
    }
    // No embedded font program — try font substitution
    if let Some(font) = substitute_font(&base_font_name, encoding.clone(), widths) {
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
    let mut clean_name: &str = base_font;
    if clean_name.len() > 7 && clean_name.as_bytes().get(6) == Some(&b'+') {
        clean_name = &clean_name[7..];
    }
    // Strip trailing "*N" suffix (e.g. "ArialMT*1" → "ArialMT")
    if let Some(star_pos) = clean_name.rfind('*') {
        clean_name = &clean_name[..star_pos];
    }

    // Look up substitution (exact match first, then fuzzy family match)
    let urw_name = FONT_SUBSTITUTIONS
        .iter()
        .find(|&&(ps, _)| ps == clean_name)
        .map(|&(_, urw)| urw)
        .or_else(|| fuzzy_font_match(clean_name));

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

/// Fuzzy font family matching for names not in the substitution table.
/// Detects common family name patterns and maps to URW equivalents.
fn fuzzy_font_match(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    let is_bold = lower.contains("bold") || lower.contains("demi");
    let is_italic = lower.contains("italic") || lower.contains("oblique");

    if lower.contains("times") || lower.contains("roman") || lower.contains("serif") {
        return Some(match (is_bold, is_italic) {
            (true, true) => "NimbusRoman-BoldItalic",
            (true, false) => "NimbusRoman-Bold",
            (false, true) => "NimbusRoman-Italic",
            (false, false) => "NimbusRoman-Regular",
        });
    }
    if lower.contains("helvetica") || lower.contains("arial") || lower.contains("sans") {
        return Some(match (is_bold, is_italic) {
            (true, true) => "NimbusSans-BoldItalic",
            (true, false) => "NimbusSans-Bold",
            (false, true) => "NimbusSans-Italic",
            (false, false) => "NimbusSans-Regular",
        });
    }
    if lower.contains("courier") || lower.contains("mono") {
        return Some(match (is_bold, is_italic) {
            (true, true) => "NimbusMonoPS-BoldItalic",
            (true, false) => "NimbusMonoPS-Bold",
            (false, true) => "NimbusMonoPS-Italic",
            (false, false) => "NimbusMonoPS-Regular",
        });
    }
    None
}

/// Resolve a Type 3 font: glyphs defined as content streams.
fn resolve_type3(resolver: &Resolver, font_dict: &PdfDict) -> Result<PdfFont, PdfError> {
    let first_char = font_dict.get_int(b"FirstChar").unwrap_or(0) as usize;

    // Parse widths array (already in glyph space — Type 3 FontMatrix maps to text space)
    let mut widths = [0.0f64; 256];
    if let Some(w_arr) = font_dict.get_array(b"Widths") {
        for (i, obj) in w_arr.iter().enumerate() {
            let code = first_char + i;
            if code < 256 {
                widths[code] = obj.as_f64().unwrap_or(0.0);
            }
        }
    }

    // FontMatrix (typically something like [0.01 0 0 0.01 0 0] for 100-unit glyph space)
    let font_matrix = font_dict
        .get_array(b"FontMatrix")
        .map(|a| {
            let v: Vec<f64> = a.iter().filter_map(|o| o.as_f64()).collect();
            if v.len() >= 6 {
                Matrix::new(v[0], v[1], v[2], v[3], v[4], v[5])
            } else {
                Matrix::new(0.001, 0.0, 0.0, 0.001, 0.0, 0.0)
            }
        })
        .unwrap_or_else(|| Matrix::new(0.001, 0.0, 0.0, 0.001, 0.0, 0.0));

    let font_bbox = font_dict
        .get_array(b"FontBBox")
        .map(|a| {
            let v: Vec<f64> = a.iter().filter_map(|o| o.as_f64()).collect();
            if v.len() >= 4 {
                [v[0], v[1], v[2], v[3]]
            } else {
                [0.0, 0.0, 1.0, 1.0]
            }
        })
        .unwrap_or([0.0, 0.0, 1.0, 1.0]);

    // Resolve encoding: maps char codes → glyph names in CharProcs
    let encoding = resolve_encoding(font_dict, resolver)?;

    // Get CharProcs dict: maps glyph names → content streams
    let char_procs_dict = font_dict
        .get_dict(b"CharProcs")
        .ok_or(PdfError::Other("Type3 font missing CharProcs".into()))?;

    // Resources for interpreting CharProc streams
    let resources = if let Some(res_ref) = font_dict.get(b"Resources") {
        match resolver.deref(res_ref)? {
            PdfObj::Dict(d) => d,
            _ => PdfDict::new(),
        }
    } else {
        PdfDict::new()
    };

    // Pre-decode all CharProc streams: encoding[code] → stream bytes
    let mut char_procs = HashMap::new();
    for code in 0..256u16 {
        if let Some(glyph_name) = &encoding[code as usize] {
            if let Some(proc_ref) = char_procs_dict.get(glyph_name.as_bytes()) {
                if let Ok(proc_obj) = resolver.deref(proc_ref) {
                    if let Ok(data) = resolver.stream_data_from_obj(&proc_obj) {
                        char_procs.insert(code as u8, data);
                    }
                }
            }
        }
    }

    Ok(PdfFont::Type3(Type3PdfFont {
        char_procs,
        resources,
        widths,
        font_matrix,
        font_bbox,
    }))
}

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

    // Validate that glyph outline data is actually present
    use stet_core::truetype::find_table;
    let has_glyf = find_table(&data, b"glyf").is_some();
    let has_usable_glyx = if let Some((off, len)) = find_table(&data, b"glyx") {
        off + len <= data.len()
    } else {
        false
    };
    if !has_glyf && !has_usable_glyx {
        return Err(PdfError::Other(
            "TrueType font has no usable glyph outline data".into(),
        ));
    }

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

/// Resolve a Type 0 composite font (CIDFontType2 descendant with TrueType outlines).
fn resolve_type0(resolver: &Resolver, font_dict: &PdfDict) -> Result<PdfFont, PdfError> {
    // Get DescendantFonts array (must have exactly one entry)
    let descendants = font_dict
        .get_array(b"DescendantFonts")
        .ok_or(PdfError::Other("Type0 font missing DescendantFonts".into()))?;
    let cid_font_ref = descendants
        .first()
        .ok_or(PdfError::Other("DescendantFonts is empty".into()))?;
    let cid_font_obj = resolver.deref(cid_font_ref)?;
    let cid_font_dict = cid_font_obj
        .as_dict()
        .ok_or(PdfError::Other("CIDFont is not a dict".into()))?;

    let cid_subtype = cid_font_dict.get_name(b"Subtype").unwrap_or(b"");
    if cid_subtype != b"CIDFontType2" {
        return Err(PdfError::Other(format!(
            "Unsupported CIDFont subtype: {}",
            String::from_utf8_lossy(cid_subtype)
        )));
    }

    // Get FontDescriptor from the CIDFont
    let descriptor = get_font_descriptor(cid_font_dict, resolver)?;
    let desc = descriptor
        .as_ref()
        .ok_or(PdfError::Other("CIDFont missing FontDescriptor".into()))?;
    let ff_ref = desc
        .get(b"FontFile2")
        .ok_or(PdfError::Other("CIDFontType2 missing FontFile2".into()))?;
    let data = resolver.stream_data_from_obj(&resolver.deref(ff_ref)?)?;

    let units_per_em = get_units_per_em(&data) as f64;
    let cmap = parse_cmap(&data);

    // Parse /DW (default width)
    let default_width = cid_font_dict.get_int(b"DW").unwrap_or(1000) as f64 / 1000.0;

    // Parse /W array (CID-specific widths)
    let cid_widths = parse_cid_widths(cid_font_dict, resolver);

    // Check CIDToGIDMap
    let identity_cid_to_gid = cid_font_dict
        .get_name(b"CIDToGIDMap")
        .map(|n| n == b"Identity")
        .unwrap_or(true); // Default is Identity

    Ok(PdfFont::CidTrueType(CidTrueTypePdfFont {
        data,
        default_width,
        cid_widths,
        cmap,
        units_per_em,
        identity_cid_to_gid,
    }))
}

/// Parse /W array from CIDFont dict into CID → width map.
///
/// Format: `[ cid_first [w1 w2 ...] cid_first cid_last w ... ]`
fn parse_cid_widths(cid_font_dict: &PdfDict, resolver: &Resolver) -> HashMap<u16, f64> {
    let mut widths = HashMap::new();
    let w_arr = match cid_font_dict.get_array(b"W") {
        Some(arr) => arr,
        None => return widths,
    };
    let mut i = 0;
    while i < w_arr.len() {
        let first_cid = match &w_arr[i] {
            PdfObj::Int(n) => *n as u16,
            _ => break,
        };
        i += 1;
        if i >= w_arr.len() {
            break;
        }
        // Next element: array (individual widths) or int (range end)
        let next = resolver.deref(&w_arr[i]).unwrap_or(w_arr[i].clone());
        match &next {
            PdfObj::Array(arr) => {
                // [ cid_first [w1 w2 w3 ...] ] — consecutive CID widths
                for (j, w_obj) in arr.iter().enumerate() {
                    let w = w_obj.as_f64().unwrap_or(0.0) / 1000.0;
                    widths.insert(first_cid + j as u16, w);
                }
                i += 1;
            }
            _ => {
                // [ cid_first cid_last w ] — range with uniform width
                let last_cid = match &next {
                    PdfObj::Int(n) => *n as u16,
                    _ => first_cid,
                };
                i += 1;
                let w = if i < w_arr.len() {
                    w_arr[i].as_f64().unwrap_or(0.0) / 1000.0
                } else {
                    0.0
                };
                i += 1;
                for cid in first_cid..=last_cid {
                    widths.insert(cid, w);
                }
            }
        }
    }
    widths
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
    /// Get glyph outline path for a character code (single-byte fonts).
    /// Returns None for Type 3 fonts (they use content streams, not outlines).
    pub fn glyph_path(&self, char_code: u8) -> Option<PsPath> {
        match self {
            PdfFont::Type1(f) => f.glyph_path(char_code),
            PdfFont::TrueType(f) => f.glyph_path(char_code),
            PdfFont::Cff(f) => f.glyph_path(char_code),
            PdfFont::CidTrueType(f) => f.glyph_path_cid(char_code as u16),
            PdfFont::Type3(_) => None,
        }
    }

    /// Get glyph outline path for a CID (2-byte composite fonts).
    pub fn glyph_path_cid(&self, cid: u16) -> Option<PsPath> {
        match self {
            PdfFont::CidTrueType(f) => f.glyph_path_cid(cid),
            _ => self.glyph_path(cid as u8),
        }
    }

    /// Get width for a character code (in text space units, already ÷1000).
    pub fn glyph_width(&self, char_code: u8) -> f64 {
        match self {
            PdfFont::Type1(f) => f.widths[char_code as usize],
            PdfFont::TrueType(f) => f.widths[char_code as usize],
            PdfFont::Cff(f) => f.widths[char_code as usize],
            PdfFont::CidTrueType(f) => f.glyph_width_cid(char_code as u16),
            PdfFont::Type3(f) => f.widths[char_code as usize],
        }
    }

    /// Get width for a CID (2-byte composite fonts).
    pub fn glyph_width_cid(&self, cid: u16) -> f64 {
        match self {
            PdfFont::CidTrueType(f) => f.glyph_width_cid(cid),
            _ => self.glyph_width(cid as u8),
        }
    }

    /// Font matrix (glyph space → text space).
    pub fn font_matrix(&self) -> Matrix {
        match self {
            PdfFont::Type1(f) => f.font_matrix,
            PdfFont::TrueType(_) | PdfFont::CidTrueType(_) => Matrix::identity(),
            PdfFont::Cff(f) => f.font_matrix,
            PdfFont::Type3(f) => f.font_matrix,
        }
    }

    /// Whether this is a composite (CID) font that uses 2-byte character codes.
    pub fn is_composite(&self) -> bool {
        matches!(self, PdfFont::CidTrueType(_))
    }

    /// Whether this is a Type 3 font (glyphs are content streams).
    pub fn is_type3(&self) -> bool {
        matches!(self, PdfFont::Type3(_))
    }

    /// Get the Type 3 glyph stream data for a character code.
    pub fn type3_char_proc(&self, char_code: u8) -> Option<&[u8]> {
        match self {
            PdfFont::Type3(f) => f.char_procs.get(&char_code).map(|v| v.as_slice()),
            _ => None,
        }
    }

    /// Get the Type 3 font resources dict.
    pub fn type3_resources(&self) -> Option<&PdfDict> {
        match self {
            PdfFont::Type3(f) => Some(&f.resources),
            _ => None,
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
        // Try encoding → glyph name → Unicode → cmap
        if !self.cmap.is_empty() {
            if let Some(glyph_name) = &self.encoding[char_code as usize] {
                if let Some(unicode) = stet_core::agl::glyph_name_to_unicode(glyph_name) {
                    if let Some(&gid) = self.cmap.get(&(unicode as u32)) {
                        return Some(gid);
                    }
                }
            }
            // Fallback: direct cmap lookup
            if let Some(&gid) = self.cmap.get(&(char_code as u32)) {
                return Some(gid);
            }
            None
        } else {
            // No cmap table: use char code as GID directly (PDF subset identity mapping)
            Some(char_code as u16)
        }
    }
}

impl CidTrueTypePdfFont {
    fn glyph_path_cid(&self, cid: u16) -> Option<PsPath> {
        let gid = if self.identity_cid_to_gid {
            cid
        } else {
            // Non-identity CIDToGIDMap would be parsed from stream data
            cid
        };
        let glyf_data = get_glyf_data(&self.data, gid)?;
        let data_clone = self.data.clone();
        let path = parse_glyf_to_path(&glyf_data, &|component_gid| {
            get_glyf_data(&data_clone, component_gid)
        });
        if path.is_empty() {
            return None;
        }
        let scale = 1.0 / self.units_per_em;
        let m = Matrix::scale(scale, scale);
        Some(path.transform(&m))
    }

    fn glyph_width_cid(&self, cid: u16) -> f64 {
        self.cid_widths
            .get(&cid)
            .copied()
            .unwrap_or(self.default_width)
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
