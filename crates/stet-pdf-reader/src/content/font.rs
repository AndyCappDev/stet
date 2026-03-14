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
use crate::FontProvider;

/// Resolved PDF font, ready for glyph rendering.
pub enum PdfFont {
    Type1(Type1PdfFont),
    TrueType(TrueTypePdfFont),
    Cff(CffPdfFont),
    /// Type 0 composite font (CIDFontType2 with TrueType outlines)
    CidTrueType(CidTrueTypePdfFont),
    /// Type 0 composite font (CIDFontType0 with CFF outlines)
    CidCff(CidCffPdfFont),
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
    /// Glyph name → GID mapping from the `post` table (for ligatures and
    /// other glyphs not reachable via Unicode cmap lookup).
    pub post_name_to_gid: HashMap<String, u16>,
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
    /// If true, font data was loaded from the system (not embedded in PDF).
    /// For substituted fonts, CIDs are treated as Unicode and mapped via cmap.
    pub substituted: bool,
    /// Explicit CID-to-GID mapping from a CIDToGIDMap stream.
    /// Index = CID, value = GID. Takes priority over identity mapping.
    pub cid_to_gid_map: Option<Vec<u16>>,
    /// CID → Unicode mapping from the Type 0 font's /ToUnicode CMap.
    /// Used for substituted fonts to convert CID → Unicode → GID via cmap.
    pub to_unicode: HashMap<u16, u32>,
    /// CIDSystemInfo /Ordering (e.g. b"Japan1", b"GB1") for CID→Unicode fallback.
    pub ordering: Vec<u8>,
}

/// CIDFontType0: CFF outlines accessed by CID (2-byte char codes).
pub struct CidCffPdfFont {
    pub font: CffFont,
    /// Default glyph width (from /DW, in text space ÷1000).
    pub default_width: f64,
    /// CID → width mapping (from /W array, in text space ÷1000).
    pub cid_widths: HashMap<u16, f64>,
    pub font_matrix: Matrix,
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
pub fn resolve_font(
    resolver: &Resolver,
    font_ref: &PdfObj,
    font_provider: Option<&FontProvider>,
) -> Result<PdfFont, PdfError> {
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

    // Parse widths array from font dict.
    // /Widths may be a direct array or an indirect reference — resolve if needed.
    let mut widths = [0.0f64; 256];
    let mut has_pdf_widths = false;
    let widths_obj = font_dict.get(b"Widths").and_then(|obj| {
        if obj.as_array().is_some() {
            Some(obj.clone())
        } else {
            resolver.deref(obj).ok()
        }
    });
    if let Some(PdfObj::Array(w_arr)) = &widths_obj {
        for (i, obj) in w_arr.iter().enumerate() {
            let code = first_char + i;
            if code < 256 {
                widths[code] = obj.as_f64().unwrap_or(0.0) / 1000.0;
            }
        }
        has_pdf_widths = true;
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
                        substitute_font(&base_font_name, encoding.clone(), widths, has_pdf_widths, font_provider)
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
                        substitute_font(&base_font_name, encoding.clone(), widths, has_pdf_widths, font_provider)
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
                        substitute_font(&base_font_name, encoding.clone(), widths, has_pdf_widths, font_provider)
                    {
                        return Ok(font);
                    }
                }
            }
        }
    }
    // No embedded font program — try font substitution
    if let Some(font) = substitute_font(&base_font_name, encoding.clone(), widths, has_pdf_widths, font_provider) {
        return Ok(font);
    }

    // For TrueType fonts, try loading from system fonts before giving up
    if subtype == b"TrueType" {
        if let Ok(data) = load_system_truetype_font(&base_font_name) {
            let units_per_em = get_units_per_em(&data) as f64;
            let cmap = parse_cmap(&data);
            let post_name_to_gid = stet_core::system_fonts::parse_post_table(&data)
                .map(|gid_to_name| {
                    gid_to_name
                        .into_iter()
                        .map(|(gid, name)| (name, gid))
                        .collect()
                })
                .unwrap_or_default();
            return Ok(PdfFont::TrueType(TrueTypePdfFont {
                data,
                encoding,
                widths,
                cmap,
                post_name_to_gid,
                units_per_em,
            }));
        }
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

/// Load a fallback font (Helvetica/NimbusSans) for when no font resource exists.
pub fn fallback_font(font_provider: Option<&FontProvider>) -> Option<PdfFont> {
    let encoding: [Option<String>; 256] = std::array::from_fn(|i| {
        WINANSI_ENCODING.get(i).and_then(|&s| {
            if s.is_empty() { None } else { Some(s.to_string()) }
        })
    });
    let widths = super::standard_fonts::standard_font_widths(b"Helvetica")
        .unwrap_or([0.0f64; 256]);
    substitute_font("Helvetica", encoding, widths, false, font_provider)
}

/// Try to load a substitute font for a non-embedded font.
fn substitute_font(
    base_font: &str,
    encoding: [Option<String>; 256],
    widths: [f64; 256],
    has_pdf_widths: bool,
    font_provider: Option<&FontProvider>,
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

    // Try the font provider first (for WASM and other non-filesystem environments)
    let font_data = if let Some(provider) = font_provider {
        provider(font_file_name)
    } else {
        None
    };

    // Fall back to filesystem
    let font_data = font_data.or_else(|| {
        let font_path = format!("resources/Font/{}.t1", font_file_name);
        std::fs::read(&font_path).ok()
    })?;

    let font = parse_type1(&font_data).ok()?;
    let fm = font.font_matrix;
    let font_matrix = Matrix::new(fm[0], fm[1], fm[2], fm[3], fm[4], fm[5]);

    // If the PDF didn't provide an explicit /Widths array, derive widths from
    // the substitute font's charstrings. Standard 14 font fallback tables are
    // indexed by StandardEncoding and give wrong widths for other encodings
    // (WinAnsiEncoding, MacRomanEncoding, or custom /Differences).
    let widths = if !has_pdf_widths {
        let mut derived = [0.0f64; 256];
        // Get .notdef width as fallback for unmapped codes
        let notdef_width = font
            .charstrings
            .get(".notdef")
            .and_then(|cs| execute_charstring(cs, &font.subrs, font.len_iv, false).ok())
            .map(|r| r.width_x * fm[0])
            .unwrap_or(0.0);
        for code in 0..256usize {
            let glyph_name = encoding[code].as_deref().unwrap_or(".notdef");
            if let Some(cs) = font.charstrings.get(glyph_name) {
                if let Ok(result) = execute_charstring(cs, &font.subrs, font.len_iv, false) {
                    // Width is in glyph space; scale by font matrix to get text space
                    derived[code] = result.width_x * fm[0];
                }
            } else {
                // Glyph not found — use .notdef width
                derived[code] = notdef_width;
            }
        }
        derived
    } else {
        widths
    };

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
    if lower.contains("helvetica") || lower.contains("arial") || lower.contains("sans")
        || lower.contains("calibri") || lower.contains("verdana") || lower.contains("tahoma")
    {
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

/// Known CID font substitutions for fonts commonly missing on Linux.
const CID_FONT_SUBSTITUTIONS: &[(&str, &str)] = &[
    ("ArialUnicodeMS", "DejaVuSans"),
    ("Arial-BoldMT", "LiberationSans-Bold"),
    ("Arial-BoldItalicMT", "LiberationSans-BoldItalic"),
    ("Arial-ItalicMT", "LiberationSans-Italic"),
    ("ArialMT", "LiberationSans"),
    ("Calibri", "LiberationSans"),
    ("Calibri,Bold", "LiberationSans-Bold"),
    ("Calibri,BoldItalic", "LiberationSans-BoldItalic"),
    ("Calibri,Italic", "LiberationSans-Italic"),
    ("TimesNewRomanPS-BoldMT", "LiberationSerif-Bold"),
    ("TimesNewRomanPS-BoldItalicMT", "LiberationSerif-BoldItalic"),
    ("TimesNewRomanPS-ItalicMT", "LiberationSerif-Italic"),
    ("TimesNewRomanPSMT", "LiberationSerif"),
    // Japanese CJK fonts → DroidSansFallback (matches GhostScript's CIDFSubst)
    ("MS-Gothic", "DroidSansFallback"),
    ("MS-PGothic", "DroidSansFallback"),
    ("MS-Mincho", "DroidSansFallback"),
    ("MS-PMincho", "DroidSansFallback"),
    ("MSGothic", "DroidSansFallback"),
    ("MSPGothic", "DroidSansFallback"),
    ("MSMincho", "DroidSansFallback"),
    ("MSPMincho", "DroidSansFallback"),
    // Korean CJK fonts
    ("Batang", "DroidSansFallback"),
    ("BatangChe", "DroidSansFallback"),
    ("Dotum", "DroidSansFallback"),
    ("DotumChe", "DroidSansFallback"),
    ("Gulim", "DroidSansFallback"),
    ("GulimChe", "DroidSansFallback"),
    // Chinese CJK fonts
    ("SimSun", "DroidSansFallback"),
    ("SimHei", "DroidSansFallback"),
    ("MingLiU", "DroidSansFallback"),
    ("PMingLiU", "DroidSansFallback"),
];

/// Try to load a TrueType font from the system font cache.
///
/// Used when a CIDFontType2 font is not embedded in the PDF (missing FontFile2).
/// Falls back to substitution table and fuzzy name matching.
fn load_system_truetype_font(base_font: &str) -> Result<Vec<u8>, PdfError> {
    use stet_core::system_fonts::get_system_font_cache;

    let cache = get_system_font_cache();

    // Strip subset prefix (e.g. "ABCDEF+Calibri,Bold" → "Calibri,Bold")
    let mut clean_name = base_font;
    if clean_name.len() > 7 && clean_name.as_bytes().get(6) == Some(&b'+') {
        clean_name = &clean_name[7..];
    }

    // Try exact match first
    if let Some(path) = cache.get_font_path(clean_name) {
        if let Ok(data) = std::fs::read(path) {
            return Ok(data);
        }
    }

    // Try known substitutions
    for &(from, to) in CID_FONT_SUBSTITUTIONS {
        if from == clean_name {
            if let Some(path) = cache.get_font_path(to) {
                if let Ok(data) = std::fs::read(path) {
                    return Ok(data);
                }
            }
        }
    }

    // Fuzzy family match — split on '-' or ',' to extract family name
    let lower = clean_name.to_ascii_lowercase();
    let is_bold = lower.contains("bold") || lower.contains("demi");
    let is_italic = lower.contains("italic") || lower.contains("oblique");

    for (ps_name, path) in cache.iter() {
        let ps_lower = ps_name.to_ascii_lowercase();
        let family = lower.split(&['-', ','][..]).next().unwrap_or(&lower);
        if ps_lower.contains(family) || family.contains(&ps_lower.split('-').next().unwrap_or("")) {
            let name_bold = ps_lower.contains("bold") || ps_lower.contains("demi");
            let name_italic = ps_lower.contains("italic") || ps_lower.contains("oblique");
            if name_bold == is_bold && name_italic == is_italic {
                if let Ok(data) = std::fs::read(path) {
                    return Ok(data);
                }
            }
        }
    }

    Err(PdfError::Other(format!(
        "font '{}' not found on system",
        clean_name
    )))
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
    // May be a direct dict or an indirect reference
    let char_procs_dict = if let Some(obj) = font_dict.get(b"CharProcs") {
        match resolver.deref(obj)? {
            PdfObj::Dict(d) => d,
            _ => return Err(PdfError::Other("Type3 CharProcs is not a dict".into())),
        }
    } else {
        return Err(PdfError::Other("Type3 font missing CharProcs".into()));
    };

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
                if let Ok(data) = resolver.stream_data_from_obj(proc_ref) {
                    char_procs.insert(code as u8, data);
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
    // Check FontFile first (traditional Type 1), then FontFile3 (CFF or Type1C)
    if let Some(ff3_ref) = desc.get(b"FontFile3") {
        // FontFile3 may contain CFF (Type1C) data — handle via CFF parser
        let ff3_obj = resolver.deref(ff3_ref)?;
        let ff3_dict = ff3_obj.as_dict();
        let subtype = ff3_dict.and_then(|d| d.get_name(b"Subtype")).unwrap_or(b"");
        if subtype == b"Type1C" || subtype == b"CIDFontType0C" || subtype == b"OpenType" {
            let raw_data = resolver.stream_data_from_obj(ff3_ref)?;
            // If data starts with "OTTO" it's an OpenType container — extract CFF table
            let font_data = if raw_data.starts_with(b"OTTO") {
                use stet_core::truetype::find_table;
                let (offset, length) = find_table(&raw_data, b"CFF ")
                    .ok_or(PdfError::Other("OpenType font has no CFF table".into()))?;
                raw_data[offset..offset + length].to_vec()
            } else {
                raw_data
            };
            let fonts = parse_cff(&font_data)
                .map_err(|e| PdfError::Other(format!("CFF parse error: {e}")))?;
            let font = fonts
                .into_iter()
                .next()
                .ok_or(PdfError::Other("CFF contains no fonts".into()))?;

            let fm = font.font_matrix;
            let font_matrix = Matrix::new(fm[0], fm[1], fm[2], fm[3], fm[4], fm[5]);

            return Ok(PdfFont::Cff(CffPdfFont {
                font,
                encoding,
                widths,
                font_matrix,
            }));
        }
    }

    let ff_ref = desc
        .get(b"FontFile")
        .or_else(|| desc.get(b"FontFile3"))
        .ok_or(PdfError::Other("Type1 font missing FontFile".into()))?;
    let font_data = resolver.stream_data_from_obj(ff_ref)?;

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
    let data = resolver.stream_data_from_obj(ff_ref)?;

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

    // Parse post table (GID → name) and invert to name → GID for fallback lookup
    let post_name_to_gid = stet_core::system_fonts::parse_post_table(&data)
        .map(|gid_to_name| {
            gid_to_name
                .into_iter()
                .map(|(gid, name)| (name, gid))
                .collect()
        })
        .unwrap_or_default();

    Ok(PdfFont::TrueType(TrueTypePdfFont {
        data,
        encoding,
        widths,
        cmap,
        post_name_to_gid,
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
    let font_data = resolver.stream_data_from_obj(ff_ref)?;

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
    // Get DescendantFonts array (must have exactly one entry).
    // May be a direct array or an indirect reference to one.
    let descendants_obj = font_dict
        .get(b"DescendantFonts")
        .ok_or(PdfError::Other("Type0 font missing DescendantFonts".into()))?;
    let descendants_resolved = resolver.deref(descendants_obj)?;
    let descendants = descendants_resolved
        .as_array()
        .ok_or(PdfError::Other("DescendantFonts is not an array".into()))?;
    let cid_font_ref = descendants
        .first()
        .ok_or(PdfError::Other("DescendantFonts is empty".into()))?;
    let cid_font_obj = resolver.deref(cid_font_ref)?;
    let cid_font_dict = cid_font_obj
        .as_dict()
        .ok_or(PdfError::Other("CIDFont is not a dict".into()))?;

    let cid_subtype = cid_font_dict.get_name(b"Subtype").unwrap_or(b"");

    // Get FontDescriptor from the CIDFont
    let descriptor = get_font_descriptor(cid_font_dict, resolver)?;
    let desc = descriptor
        .as_ref()
        .ok_or(PdfError::Other("CIDFont missing FontDescriptor".into()))?;

    // Parse /DW (default width)
    let default_width = cid_font_dict.get_int(b"DW").unwrap_or(1000) as f64 / 1000.0;

    // Parse /W array (CID-specific widths)
    let cid_widths = parse_cid_widths(cid_font_dict, resolver);

    // Parse /ToUnicode CMap from the parent Type 0 font dict
    let to_unicode = if let Some(tu_obj) = font_dict.get(b"ToUnicode") {
        match resolver.stream_data_from_obj(tu_obj) {
            Ok(data) => parse_to_unicode(&data),
            Err(_) => HashMap::new(),
        }
    } else {
        HashMap::new()
    };

    // Extract CIDSystemInfo /Ordering for CID→Unicode fallback lookup.
    // /Ordering is a string (parenthesized), not a name.
    let ordering = {
        let si_dict = cid_font_dict
            .get_dict(b"CIDSystemInfo")
            .cloned()
            .or_else(|| {
                cid_font_dict
                    .get(b"CIDSystemInfo")
                    .and_then(|obj| resolver.deref(obj).ok())
                    .and_then(|obj| obj.as_dict().cloned())
            });
        si_dict
            .and_then(|d| {
                d.get(b"Ordering").and_then(|v| match v {
                    PdfObj::Str(s) => Some(s.clone()),
                    PdfObj::Name(n) => Some(n.clone()),
                    _ => None,
                })
            })
            .unwrap_or_default()
    };

    match cid_subtype {
        b"CIDFontType2" => {
            let substituted;
            let data = if let Some(ff_ref) = desc.get(b"FontFile2") {
                substituted = false;
                resolver.stream_data_from_obj(ff_ref)?
            } else {
                // Font not embedded — try system font lookup
                substituted = true;
                let base_font = cid_font_dict
                    .get_name(b"BaseFont")
                    .map(|n| {
                        let s = String::from_utf8_lossy(n);
                        // Strip subset prefix (e.g. "ABCDEF+PalatinoLinotype-BoldItalic")
                        if s.len() > 7 && s.as_bytes().get(6) == Some(&b'+') {
                            s[7..].to_string()
                        } else {
                            s.to_string()
                        }
                    })
                    .unwrap_or_default();
                load_system_truetype_font(&base_font)?
            };

            let units_per_em = get_units_per_em(&data) as f64;
            let cmap = parse_cmap(&data);

            // Parse CIDToGIDMap: either /Identity name or a stream of big-endian u16 pairs
            let (identity_cid_to_gid, cid_to_gid_map) =
                if let Some(name) = cid_font_dict.get_name(b"CIDToGIDMap") {
                    (name == b"Identity", None)
                } else if let Some(map_obj) = cid_font_dict.get(b"CIDToGIDMap") {
                    match resolver.stream_data_from_obj(map_obj) {
                        Ok(stream_data) => {
                            let mut gid_map =
                                Vec::with_capacity(stream_data.len() / 2);
                            for pair in stream_data.chunks_exact(2) {
                                gid_map.push(u16::from_be_bytes([pair[0], pair[1]]));
                            }
                            (false, Some(gid_map))
                        }
                        Err(_) => (true, None), // fallback to identity
                    }
                } else {
                    (true, None) // no CIDToGIDMap → default to identity
                };

            Ok(PdfFont::CidTrueType(CidTrueTypePdfFont {
                data,
                default_width,
                cid_widths,
                cmap,
                units_per_em,
                identity_cid_to_gid,
                substituted,
                cid_to_gid_map,
                to_unicode,
                ordering: ordering.clone(),
            }))
        }
        b"CIDFontType0" => {
            // CFF-based CID font: FontFile3 with /Subtype /CIDFontType0C
            let ff_ref = desc
                .get(b"FontFile3")
                .ok_or(PdfError::Other("CIDFontType0 missing FontFile3".into()))?;
            let font_data = resolver.stream_data_from_obj(ff_ref)?;

            let fonts = parse_cff(&font_data)
                .map_err(|e| PdfError::Other(format!("CFF parse error: {e}")))?;
            let font = fonts
                .into_iter()
                .next()
                .ok_or(PdfError::Other("CFF contains no fonts".into()))?;

            let fm = font.font_matrix;
            let font_matrix = Matrix::new(fm[0], fm[1], fm[2], fm[3], fm[4], fm[5]);

            Ok(PdfFont::CidCff(CidCffPdfFont {
                font,
                default_width,
                cid_widths,
                font_matrix,
            }))
        }
        _ => Err(PdfError::Other(format!(
            "Unsupported CIDFont subtype: {}",
            String::from_utf8_lossy(cid_subtype)
        ))),
    }
}

/// Parse /W array from CIDFont dict into CID → width map.
///
/// Format: `[ cid_first [w1 w2 ...] cid_first cid_last w ... ]`
/// Extract `<hex>` tokens from a string, returning raw hex strings.
fn extract_hex_tokens(s: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut rest = s;
    while let Some(start) = rest.find('<') {
        rest = &rest[start + 1..];
        if let Some(end) = rest.find('>') {
            let hex = rest[..end].trim();
            if !hex.is_empty() {
                tokens.push(hex);
            }
            rest = &rest[end + 1..];
        } else {
            break;
        }
    }
    tokens
}

/// Parse a hex string as a Unicode codepoint.
/// For multi-byte destinations (>4 hex digits), extract just the first codepoint (first 4 digits).
fn hex_to_unicode(hex: &str) -> Option<u32> {
    if hex.len() <= 4 {
        u32::from_str_radix(hex, 16).ok()
    } else {
        // Multi-byte: two or more 16-bit codepoints packed together.
        // Check for common ligature sequences and map to Unicode ligature codepoints.
        match hex {
            "00660066" => Some(0xFB00),             // ff
            "00660069" => Some(0xFB01),             // fi
            "0066006C" => Some(0xFB02),             // fl
            "006600660069" => Some(0xFB03),         // ffi
            "00660066006C" => Some(0xFB04),         // ffl
            "017F0074" => Some(0xFB05),             // ſt (long s + t)
            "00730074" => Some(0xFB06),             // st
            _ => {
                // Unknown sequence — use first 16-bit codepoint
                u32::from_str_radix(&hex[..hex.len().min(4)], 16).ok()
            }
        }
    }
}

/// Parse a ToUnicode CMap stream into a CID → Unicode mapping.
///
/// Handles `beginbfchar` and `beginbfrange` sections with hex-encoded values.
/// Multi-byte destination values (ligatures etc.) are mapped to their first codepoint.
fn parse_to_unicode(data: &[u8]) -> HashMap<u16, u32> {
    let mut map = HashMap::new();
    let text = String::from_utf8_lossy(data);

    // Parse bfchar entries: <src_cid> <dst_unicode>
    // Process line-by-line to avoid pairing issues with multi-byte destinations
    let mut in_bfchar = false;
    let mut in_bfrange = false;
    let mut range_tokens: Vec<&str> = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.ends_with("beginbfchar") {
            in_bfchar = true;
            continue;
        }
        if trimmed == "endbfchar" {
            in_bfchar = false;
            continue;
        }
        if trimmed.ends_with("beginbfrange") {
            in_bfrange = true;
            range_tokens.clear();
            continue;
        }
        if trimmed == "endbfrange" {
            in_bfrange = false;
            range_tokens.clear();
            continue;
        }

        if in_bfchar {
            let tokens = extract_hex_tokens(trimmed);
            if tokens.len() >= 2 {
                if let Some(cid) = u32::from_str_radix(tokens[0], 16).ok() {
                    if let Some(unicode) = hex_to_unicode(tokens[1]) {
                        map.insert(cid as u16, unicode);
                    }
                }
            }
        }

        if in_bfrange {
            let line_tokens = extract_hex_tokens(trimmed);
            // Check for array syntax: <start> <end> [<u1> <u2> ...]
            if trimmed.contains('[') {
                // Collect start/end from previous tokens or this line
                let all_before_bracket: Vec<&str> = {
                    let before = trimmed.split('[').next().unwrap_or("");
                    extract_hex_tokens(before)
                };
                let in_bracket = {
                    let after_open = trimmed.split('[').nth(1).unwrap_or("");
                    let before_close = after_open.split(']').next().unwrap_or(after_open);
                    extract_hex_tokens(before_close)
                };
                if all_before_bracket.len() >= 2 {
                    if let (Some(start), Some(end)) = (
                        u32::from_str_radix(all_before_bracket[0], 16).ok(),
                        u32::from_str_radix(all_before_bracket[1], 16).ok(),
                    ) {
                        for (j, cid) in (start..=end).enumerate() {
                            if j < in_bracket.len() {
                                if let Some(u) = hex_to_unicode(in_bracket[j]) {
                                    map.insert(cid as u16, u);
                                }
                            }
                        }
                    }
                }
            } else if line_tokens.len() >= 3 {
                // <start> <end> <dst_start>
                if let (Some(start), Some(end), Some(mut dst)) = (
                    u32::from_str_radix(line_tokens[0], 16).ok(),
                    u32::from_str_radix(line_tokens[1], 16).ok(),
                    hex_to_unicode(line_tokens[2]),
                ) {
                    for cid in start..=end {
                        map.insert(cid as u16, dst);
                        dst += 1;
                    }
                }
            }
        }
    }

    map
}

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
            PdfFont::CidCff(f) => f.glyph_path_cid(char_code as u16),
            PdfFont::Type3(_) => None,
        }
    }

    /// Get glyph outline path for a CID (2-byte composite fonts).
    pub fn glyph_path_cid(&self, cid: u16) -> Option<PsPath> {
        match self {
            PdfFont::CidTrueType(f) => f.glyph_path_cid(cid),
            PdfFont::CidCff(f) => f.glyph_path_cid(cid),
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
            PdfFont::CidCff(f) => f.glyph_width_cid(char_code as u16),
            PdfFont::Type3(f) => f.widths[char_code as usize],
        }
    }

    /// Get width for a CID (2-byte composite fonts).
    pub fn glyph_width_cid(&self, cid: u16) -> f64 {
        match self {
            PdfFont::CidTrueType(f) => f.glyph_width_cid(cid),
            PdfFont::CidCff(f) => f.glyph_width_cid(cid),
            _ => self.glyph_width(cid as u8),
        }
    }

    /// Font matrix (glyph space → text space).
    pub fn font_matrix(&self) -> Matrix {
        match self {
            PdfFont::Type1(f) => f.font_matrix,
            PdfFont::TrueType(_) | PdfFont::CidTrueType(_) => Matrix::identity(),
            PdfFont::Cff(f) => f.font_matrix,
            PdfFont::CidCff(f) => f.font_matrix,
            PdfFont::Type3(f) => f.font_matrix,
        }
    }

    /// Whether this is a composite (CID) font that uses 2-byte character codes.
    pub fn is_composite(&self) -> bool {
        matches!(self, PdfFont::CidTrueType(_) | PdfFont::CidCff(_))
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
        let charstring = self.font.charstrings.get(glyph_name);
        let charstring = charstring?;
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
                // Fallback: look up glyph name directly in post table
                // (handles ligatures like fl/fi that may not be in the cmap)
                if let Some(&gid) = self.post_name_to_gid.get(glyph_name.as_str()) {
                    return Some(gid);
                }
            }
            // Fallback: direct cmap lookup by char code
            // (handles PDF subset fonts where encoding byte maps directly to GID via cmap)
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
        let gid = if let Some(ref map) = self.cid_to_gid_map {
            // Explicit CIDToGIDMap stream: look up CID → GID
            *map.get(cid as usize).unwrap_or(&0)
        } else if self.substituted && !self.to_unicode.is_empty() {
            // Substituted font: CID → Unicode (via ToUnicode) → GID (via cmap)
            let unicode = *self.to_unicode.get(&cid)?;
            *self.cmap.get(&unicode)?
        } else if self.substituted && !self.ordering.is_empty() {
            // Substituted font with Adobe CID registry: use CID→Unicode table
            let unicode = super::cid_unicode::cid_to_unicode(&self.ordering, cid)?;
            *self.cmap.get(&unicode)?
        } else if self.identity_cid_to_gid && !self.substituted {
            // Embedded font with Identity CIDToGIDMap: CID = GID directly
            cid
        } else if !self.cmap.is_empty() {
            // Non-Identity mapping: CID is Unicode, use cmap
            *self.cmap.get(&(cid as u32))?
        } else {
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

impl CidCffPdfFont {
    fn glyph_path_cid(&self, cid: u16) -> Option<PsPath> {
        // In CID-keyed CFF, use cid_to_gid mapping if available,
        // otherwise fall back to CID = GID (identity mapping).
        let gid = if !self.font.cid_to_gid.is_empty() {
            let g = *self.font.cid_to_gid.get(cid as usize)?;
            if g == 0xFFFF {
                return None; // unmapped CID
            }
            g as usize
        } else {
            cid as usize
        };
        if gid >= self.font.char_strings.len() {
            return None;
        }
        // For CID fonts, use per-FD private dict values
        let (default_width_x, nominal_width_x, local_subrs) = if self.font.is_cid
            && !self.font.fd_select.is_empty()
            && !self.font.fd_array.is_empty()
        {
            let fd_idx = *self.font.fd_select.get(gid).unwrap_or(&0) as usize;
            if let Some(fd) = self.font.fd_array.get(fd_idx) {
                (fd.default_width_x, fd.nominal_width_x, &fd.local_subrs)
            } else {
                (
                    self.font.default_width_x,
                    self.font.nominal_width_x,
                    &self.font.local_subrs,
                )
            }
        } else {
            (
                self.font.default_width_x,
                self.font.nominal_width_x,
                &self.font.local_subrs,
            )
        };
        let result = execute_type2_charstring(
            &self.font.char_strings[gid],
            local_subrs,
            &self.font.global_subrs,
            default_width_x,
            nominal_width_x,
            false,
        )
        .ok()?;
        Some(result.path)
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
        // Find glyph index from charset (charset[0]=.notdef, charset[i]=GID i)
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

        // Handle deprecated seac (accented character composition)
        if let Some((adx, ady, bchar, achar)) = result.seac {
            return self.compose_seac(adx, ady, bchar, achar);
        }

        Some(result.path)
    }

    /// Compose a seac (Standard Encoding Accented Character) glyph from
    /// base and accent glyphs. bchar/achar are Standard Encoding codes.
    fn compose_seac(&self, adx: f64, ady: f64, bchar: u8, achar: u8) -> Option<PsPath> {
        use stet_core::encoding::STANDARD_ENCODING;

        let base_name = STANDARD_ENCODING.get(bchar as usize).copied().unwrap_or("");
        let accent_name = STANDARD_ENCODING.get(achar as usize).copied().unwrap_or("");

        let base_gid = self.font.charset.iter().position(|n| n == base_name)?;
        let accent_gid = self.font.charset.iter().position(|n| n == accent_name)?;

        let base_result = execute_type2_charstring(
            &self.font.char_strings[base_gid],
            &self.font.local_subrs,
            &self.font.global_subrs,
            self.font.default_width_x,
            self.font.nominal_width_x,
            false,
        )
        .ok()?;

        let accent_result = execute_type2_charstring(
            &self.font.char_strings[accent_gid],
            &self.font.local_subrs,
            &self.font.global_subrs,
            self.font.default_width_x,
            self.font.nominal_width_x,
            false,
        )
        .ok()?;

        // Combine: base path + accent path offset by (adx, ady)
        let mut combined = base_result.path;
        let offset = Matrix::translate(adx, ady);
        let shifted_accent = accent_result.path.transform(&offset);
        combined.segments.extend_from_slice(&shifted_accent.segments);
        Some(combined)
    }
}
