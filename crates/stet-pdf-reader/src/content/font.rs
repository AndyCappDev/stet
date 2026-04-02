// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF font resolution and glyph rendering.

use std::collections::HashMap;
use std::sync::Arc;

use skrifa::MetadataProvider;
use stet_fonts::cff_parser::{CffFont, parse_cff};
use stet_fonts::charstring::{execute_charstring, execute_charstring_mm};
use stet_fonts::encoding::{MACROMAN_ENCODING, STANDARD_ENCODING, WINANSI_ENCODING};
use stet_fonts::geometry::PathSegment;
use stet_fonts::geometry::{Matrix, PsPath};
use stet_fonts::truetype::{
    get_glyf_data, get_units_per_em, parse_cmap, parse_cmap_with_info, parse_glyf_to_path,
};
use stet_fonts::type1_parser::parse_type1;
use stet_fonts::type2_charstring::execute_type2_charstring;

use crate::FontProvider;
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
    /// Type 0 composite font (CIDFontType0 with CFF outlines)
    CidCff(CidCffPdfFont),
    /// Type 3 font: glyphs defined as content streams.
    Type3(Type3PdfFont),
}

pub struct Type1PdfFont {
    pub font: stet_fonts::type1_parser::Type1Font,
    pub encoding: [Option<String>; 256],
    pub widths: [f64; 256],
    pub font_matrix: Matrix,
    /// Multiple Master weight vector (for blend OtherSubrs 14-17).
    pub weight_vector: Option<Vec<f64>>,
    /// When true, glyph lookup falls back to the font's built-in encoding
    /// if the PDF encoding's glyph name isn't in CharStrings. Only set for
    /// symbolic fonts where the naming convention is completely incompatible
    /// (e.g. StandardEncoding "A" vs font's custom "G41").
    pub builtin_fallback: bool,
}

pub struct TrueTypePdfFont {
    pub data: Vec<u8>,
    pub encoding: [Option<String>; 256],
    pub widths: [f64; 256],
    pub cmap: HashMap<u32, u16>,
    /// Whether the cmap maps Unicode values (true) or re-encoded char codes (false).
    /// Non-Unicode cmaps come from (1,0) Mac Roman or (3,0) Symbol subtables in
    /// subset fonts — the encoding→unicode→cmap lookup path must be skipped.
    pub cmap_is_unicode: bool,
    /// Glyph name → GID mapping from the `post` table (for ligatures and
    /// other glyphs not reachable via Unicode cmap lookup).
    pub post_name_to_gid: HashMap<String, u16>,
    pub units_per_em: f64,
    /// Char code → Unicode mapping from /ToUnicode CMap (for gNNNN glyph names
    /// in substituted fonts where AGL lookup fails).
    pub to_unicode: HashMap<u16, u32>,
    /// When true, char codes map directly to GIDs (identity mapping).
    /// Set for symbolic TrueType fonts without an explicit /Encoding, where the
    /// cmap subtable maps to misleading Unicode values (re-encoded fonts).
    pub identity_gid: bool,
    /// When true, gNNNN glyph names in the encoding use hexadecimal GIDs
    /// (e.g. g003a = GID 58). Set when any gNNNN name contains hex letters (a-f).
    /// When false, gNNNN names use decimal (e.g. g1863 = GID 1863).
    pub gid_hex: bool,
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
    /// If true, the encoding is UCS2-based (e.g. UniJIS-UCS2-H) and character
    /// codes are Unicode values that need mapping to CIDs for width lookup.
    pub ucs2_encoding: bool,
    /// First-byte → code length table from the encoding CMap's codespace ranges.
    /// Supports mixed-width encodings (e.g. 1-byte space + 2-byte CIDs).
    pub code_lengths: [u8; 256],
    /// Code → CID mapping from the encoding CMap (empty = identity).
    pub code_to_cid: HashMap<u32, u32>,
    /// Writing mode: 0 = horizontal, 1 = vertical.
    pub wmode: u8,
    /// Default vertical metrics [v_y, w1] from /DW2 (default: [880, -1000]).
    /// v_y = vertical origin offset, w1 = vertical advance width.
    pub dw2: [f64; 2],
    /// Per-CID vertical metrics from /W2: CID → (w1, v_x, v_y).
    /// w1 = vertical advance, v_x/v_y = position vector components (in 1/1000 em).
    pub w2: HashMap<u16, [f64; 3]>,
}

/// CIDFontType0: CFF outlines accessed by CID (2-byte char codes).
pub struct CidCffPdfFont {
    pub font: CffFont,
    /// Default glyph width (from /DW, in text space ÷1000).
    pub default_width: f64,
    /// CID → width mapping (from /W array, in text space ÷1000).
    pub cid_widths: HashMap<u16, f64>,
    /// Optional Unicode→GID cmap (from OpenType substitute fonts).
    /// When present, used for UCS2 glyph lookup instead of CFF's CID mapping.
    pub cmap: Option<HashMap<u32, u16>>,
    /// CID → GID mapping from the PDF's /CIDToGIDMap stream.
    /// Used for embedded OpenType/CFF fonts stored as FontFile2.
    pub pdf_cid_to_gid: Option<Vec<u16>>,
    /// When true, CID maps directly to charstring index (GID = CID).
    /// Set when CIDToGIDMap is /Identity or absent in CIDFontType2 fonts.
    pub identity_cid_to_gid: bool,
    /// CIDSystemInfo ordering for Unicode→CID width lookup.
    pub ordering: Vec<u8>,
    pub font_matrix: Matrix,
    /// First-byte → code length table from the encoding CMap's codespace ranges.
    pub code_lengths: [u8; 256],
    /// Code → CID mapping from the encoding CMap (empty = identity).
    pub code_to_cid: HashMap<u32, u32>,
    /// Writing mode: 0 = horizontal, 1 = vertical.
    pub wmode: u8,
    /// Default vertical metrics [v_y, w1] from /DW2 (default: [880, -1000]).
    pub dw2: [f64; 2],
    /// Per-CID vertical metrics from /W2: CID → (w1, v_x, v_y).
    pub w2: HashMap<u16, [f64; 3]>,
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
    let last_char = font_dict.get_int(b"LastChar").unwrap_or(255) as usize;

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

        // Apply /MissingWidth from FontDescriptor to charcodes outside [FirstChar, LastChar].
        // Per PDF spec, charcodes not covered by /Widths use /MissingWidth (default 0).
        let descriptor = get_font_descriptor(font_dict, resolver)?;
        if let Some(ref desc) = descriptor {
            let missing_w = desc.get_f64(b"MissingWidth").unwrap_or(0.0) / 1000.0;
            if missing_w != 0.0 {
                for (code, width) in widths.iter_mut().enumerate() {
                    if code < first_char || code > last_char {
                        *width = missing_w;
                    }
                }
            }
        }
    }

    // Resolve encoding.  Track whether the PDF dict had a valid /Encoding —
    // embedded CFF fonts that lack one should use the CFF's built-in encoding.
    // Invalid encoding names (e.g. /NULL) are treated as absent.
    let (encoding, has_valid_encoding, differences) = resolve_encoding(font_dict, resolver)?;
    let has_explicit_encoding = has_valid_encoding;

    // Get FontDescriptor
    let descriptor = get_font_descriptor(font_dict, resolver)?;

    // Extract font descriptor Flags for serif/sans-serif fallback selection
    let desc_flags = descriptor
        .as_ref()
        .and_then(|d| d.get_int(b"Flags"))
        .unwrap_or(0) as u32;

    let base_font_name = font_dict
        .get_name(b"BaseFont")
        .map(|n| String::from_utf8_lossy(n).to_string())
        .unwrap_or_default();

    // Route based on what font program is actually available in FontDescriptor,
    // not just the /Subtype (which says "Type1" even for CFF-embedded fonts).
    if let Some(ref desc) = descriptor {
        if desc.get(b"FontFile3").is_some() {
            // Try embedded CFF; fall back to substitution if decompression/parsing fails
            match resolve_cff(
                resolver,
                &descriptor,
                encoding.clone(),
                widths,
                has_explicit_encoding,
                has_pdf_widths,
                &differences,
            ) {
                Ok(font) => return Ok(font),
                Err(_) => {
                    if let Some(font) = substitute_font(
                        &base_font_name,
                        encoding.clone(),
                        widths,
                        has_pdf_widths,
                        font_provider,
                        desc_flags,
                        first_char,
                        last_char,
                    ) {
                        return Ok(font);
                    }
                }
            }
        }
        if desc.get(b"FontFile2").is_some() {
            // Try embedded TrueType (falls back to CFF internally if data is OTTO/CFF)
            match resolve_truetype(resolver, &descriptor, encoding.clone(), widths, font_dict) {
                Ok(font) => return Ok(font),
                Err(_) => {
                    if let Some(font) = substitute_font(
                        &base_font_name,
                        encoding.clone(),
                        widths,
                        has_pdf_widths,
                        font_provider,
                        desc_flags,
                        first_char,
                        last_char,
                    ) {
                        return Ok(font);
                    }
                }
            }
        }
        if desc.get(b"FontFile").is_some() {
            match resolve_type1(
                resolver,
                &descriptor,
                encoding.clone(),
                widths,
                has_explicit_encoding,
                &differences,
            ) {
                Ok(font) => return Ok(font),
                Err(_) => {
                    if let Some(font) = substitute_font(
                        &base_font_name,
                        encoding.clone(),
                        widths,
                        has_pdf_widths,
                        font_provider,
                        desc_flags,
                        first_char,
                        last_char,
                    ) {
                        return Ok(font);
                    }
                }
            }
        }
    }
    // No embedded font program — try font substitution
    if let Some(font) = substitute_font(
        &base_font_name,
        encoding.clone(),
        widths,
        has_pdf_widths,
        font_provider,
        desc_flags,
        first_char,
        last_char,
    ) {
        return Ok(font);
    }
    // For TrueType fonts, try loading from system fonts before giving up
    if subtype == b"TrueType"
        && let Ok(data) = load_system_truetype_font(&base_font_name)
    {
        let units_per_em = get_units_per_em(&data) as f64;
        let (cmap, cmap_is_unicode) = parse_cmap_with_info(&data);
        let post_name_to_gid = stet_fonts::system_fonts::parse_post_table(&data)
            .map(|gid_to_name| {
                gid_to_name
                    .into_iter()
                    .map(|(gid, name)| (name, gid))
                    .collect()
            })
            .unwrap_or_default();
        let to_unicode = if let Some(tu_obj) = font_dict.get(b"ToUnicode") {
            resolver.stream_data_from_obj(tu_obj)
                .map(|d| parse_to_unicode(&d))
                .unwrap_or_default()
        } else {
            HashMap::new()
        };
        let gid_hex = TrueTypePdfFont::detect_gid_hex(&encoding);
        return Ok(PdfFont::TrueType(TrueTypePdfFont {
            data,
            encoding,
            widths,
            cmap,
            cmap_is_unicode,
            post_name_to_gid,
            units_per_em,
            to_unicode,
            identity_gid: false, // system font substitutes use normal cmap
            gid_hex,
        }));
    }

    // Final fallback based on subtype (will likely fail)
    match subtype {
        b"TrueType" => resolve_truetype(resolver, &descriptor, encoding, widths, font_dict),
        _ => resolve_type1(
            resolver,
            &descriptor,
            encoding,
            widths,
            has_explicit_encoding,
            &differences,
        ),
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
/// For symbolic fonts (ZapfDingbats, Symbol) with no explicit /BaseEncoding,
/// the font's built-in encoding is used instead of StandardEncoding.
///
/// Returns (encoding, has_valid_encoding, differences):
/// - `encoding`: fully resolved encoding (base + differences applied)
/// - `has_valid_encoding`: false when /Encoding is missing or unrecognized
/// - `differences`: raw (code, name) pairs from /Differences, populated ONLY when
///   the Encoding is a dict without /BaseEncoding (and not a symbol font). When
///   non-empty, embedded font resolvers should re-apply these on top of the font's
///   built-in encoding instead of using `encoding` directly (PDF spec 9.6.6.1).
fn resolve_encoding(
    font_dict: &PdfDict,
    resolver: &Resolver,
) -> Result<([Option<String>; 256], bool, Vec<(usize, String)>), PdfError> {
    let mut encoding: [Option<String>; 256] = std::array::from_fn(|_| None);
    let mut differences: Vec<(usize, String)> = Vec::new();

    // Start with a base encoding — use the font's built-in encoding for
    // symbolic fonts (PDF spec 9.6.6.1: when no BaseEncoding, symbolic fonts
    // use their built-in encoding, not StandardEncoding).
    let base_font = font_dict.get_name(b"BaseFont").unwrap_or(b"");
    // Strip subset prefix for font name matching
    let clean_base = if base_font.len() > 7 && base_font.get(6) == Some(&b'+') {
        &base_font[7..]
    } else {
        base_font
    };
    let is_symbol_font = clean_base == b"ZapfDingbats" || clean_base == b"Symbol";
    let mut base_table: &[&str; 256] = if clean_base == b"ZapfDingbats" {
        &stet_fonts::encoding::ZAPFDINGBATS_ENCODING
    } else if clean_base == b"Symbol" {
        &stet_fonts::encoding::SYMBOL_ENCODING
    } else {
        &STANDARD_ENCODING
    };

    let mut has_valid_encoding = is_symbol_font; // symbol fonts always have valid built-in encoding
    if let Some(enc_obj) = font_dict.get(b"Encoding") {
        let enc_resolved = resolver.deref(enc_obj)?;
        match &enc_resolved {
            PdfObj::Name(name) => {
                // Symbol/ZapfDingbats: keep their fixed encoding, ignore overrides.
                // Unknown encoding names (e.g. /NULL): skip, keep the default base.
                if !is_symbol_font {
                    if let Some(table) = encoding_table_by_name(name) {
                        base_table = table;
                        has_valid_encoding = true;
                    }
                }
            }
            PdfObj::Dict(enc_dict) => {
                // Dict encoding: optional BaseEncoding + Differences.
                // Symbol/ZapfDingbats keep their fixed base encoding.
                has_valid_encoding = true;
                let mut has_base_encoding = false;
                if !is_symbol_font {
                    if let Some(base_name) = enc_dict.get_name(b"BaseEncoding") {
                        if let Some(table) = encoding_table_by_name(base_name) {
                            base_table = table;
                            has_base_encoding = true;
                        }
                    }
                }
                for (i, &name) in base_table.iter().enumerate() {
                    if name != ".notdef" {
                        encoding[i] = Some(name.to_string());
                    }
                }
                // Parse Differences array (may be an indirect reference)
                if let Some(diffs_obj) = enc_dict.get(b"Differences") {
                    let diffs_resolved = resolver.deref(diffs_obj)?;
                    if let Some(diffs) = diffs_resolved.as_array() {
                        let mut code = 0usize;
                        for obj in diffs {
                            let obj = resolver.deref(obj).unwrap_or(obj.clone());
                            match &obj {
                                PdfObj::Int(n) => code = *n as usize,
                                PdfObj::Name(name) => {
                                    if code < 256 {
                                        let name_str =
                                            String::from_utf8_lossy(name).to_string();
                                        encoding[code] = Some(name_str.clone());
                                        // Collect differences when no BaseEncoding was
                                        // specified — embedded fonts need to re-apply
                                        // these on their built-in encoding (PDF 9.6.6.1).
                                        if !has_base_encoding && !is_symbol_font {
                                            differences.push((code, name_str));
                                        }
                                        code += 1;
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                return Ok((encoding, has_valid_encoding, differences));
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

    Ok((encoding, has_valid_encoding, differences))
}

fn encoding_table_by_name(name: &[u8]) -> Option<&'static [&'static str; 256]> {
    match name {
        b"WinAnsiEncoding" => Some(&WINANSI_ENCODING),
        b"MacRomanEncoding" => Some(&MACROMAN_ENCODING),
        b"StandardEncoding" => Some(&STANDARD_ENCODING),
        _ => None,
    }
}

/// Load a fallback font (Helvetica/NimbusSans) for when no font resource exists.
pub fn fallback_font(font_provider: Option<&FontProvider>) -> Option<PdfFont> {
    let encoding: [Option<String>; 256] = std::array::from_fn(|i| {
        WINANSI_ENCODING.get(i).and_then(|&s| {
            if s.is_empty() {
                None
            } else {
                Some(s.to_string())
            }
        })
    });
    let widths = super::standard_fonts::standard_font_widths(b"Helvetica").unwrap_or([0.0f64; 256]);
    substitute_font("Helvetica", encoding, widths, false, font_provider, 0, 0, 255)
}

/// Try to load a substitute font for a non-embedded font.
/// Load a predefined CMap file by searching multiple locations.
///
/// Search order:
/// 1. `STET_CMAP_DIR` environment variable (flat directory of CMap files)
/// 2. `~/.local/share/stet/CMap/` (user-local conventional location)
/// 3. System poppler-data directories (per-collection subdirs)
/// 4. System GhostScript directories
fn load_predefined_cmap(name: &[u8]) -> Option<Vec<u8>> {
    let name_str = std::str::from_utf8(name).ok()?;

    // 1. User-specified directory via environment variable
    if let Ok(dir) = std::env::var("STET_CMAP_DIR") {
        let path = format!("{}/{}", dir, name_str);
        if let Ok(data) = std::fs::read(&path) {
            return Some(data);
        }
    }

    // 2. User-local conventional location (~/.local/share/stet/CMap/)
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        let path = std::path::Path::new(&home)
            .join(".local/share/stet/CMap")
            .join(name_str);
        if let Ok(data) = std::fs::read(&path) {
            return Some(data);
        }
    }

    // 3. System poppler-data directories (Linux/macOS)
    // poppler organizes CMaps in per-collection subdirs (Adobe-GB1/, Adobe-Japan1/, etc.)
    let poppler_dirs = [
        "/usr/share/poppler/cMap",
        "/usr/local/share/poppler/cMap",
        "/opt/homebrew/share/poppler/cMap", // macOS Homebrew ARM
        "/usr/local/opt/poppler-data/share/poppler/cMap", // macOS Homebrew Intel
    ];
    let collections = [
        "Adobe-GB1",
        "Adobe-CNS1",
        "Adobe-Japan1",
        "Adobe-Japan2",
        "Adobe-Korea1",
        "Adobe-KR",
    ];
    for base in &poppler_dirs {
        for collection in &collections {
            let path = format!("{}/{}/{}", base, collection, name_str);
            if let Ok(data) = std::fs::read(&path) {
                return Some(data);
            }
        }
    }

    // 4. GhostScript directories (flat CMap dirs)
    let gs_dirs = [
        "/var/lib/ghostscript/CMap",
        "/usr/share/ghostscript/Resource/CMap",
        "/usr/local/share/ghostscript/Resource/CMap",
    ];
    for dir in &gs_dirs {
        let path = format!("{}/{}", dir, name_str);
        if let Ok(data) = std::fs::read(&path) {
            return Some(data);
        }
    }

    None
}

fn substitute_font(
    base_font: &str,
    encoding: [Option<String>; 256],
    widths: [f64; 256],
    has_pdf_widths: bool,
    font_provider: Option<&FontProvider>,
    descriptor_flags: u32,
    first_char: usize,
    last_char: usize,
) -> Option<PdfFont> {
    use stet_fonts::FONT_SUBSTITUTIONS;

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

    // Try system font (full glyph set) before bundled subset
    let font_data = font_data.or_else(|| {
        let cache = stet_fonts::system_fonts::get_system_font_cache();
        let path = cache.get_font_path(font_file_name)?;
        read_font_file(path, font_file_name).ok()
    });

    // Fall back to bundled subset font
    let font_data = font_data.or_else(|| embedded_font(font_file_name));

    // If the named font wasn't found, use a default substitute based on the
    // font descriptor Flags (serif bit) and weight/style from the font name.
    let font_data = font_data.or_else(|| {
        let lower = clean_name.to_ascii_lowercase();
        let is_bold = lower.contains("bold")
            || lower.contains("demi")
            || lower.contains("black")
            || lower.contains("heavy");
        let is_italic = lower.contains("italic") || lower.contains("oblique");
        let is_serif = descriptor_flags & 2 != 0; // PDF flag bit 2 = Serif
        let default_name = if is_serif {
            match (is_bold, is_italic) {
                (true, true) => "NimbusRoman-BoldItalic",
                (true, false) => "NimbusRoman-Bold",
                (false, true) => "NimbusRoman-Italic",
                (false, false) => "NimbusRoman-Regular",
            }
        } else {
            match (is_bold, is_italic) {
                (true, true) => "NimbusSans-BoldItalic",
                (true, false) => "NimbusSans-Bold",
                (false, true) => "NimbusSans-Italic",
                (false, false) => "NimbusSans-Regular",
            }
        };
        if let Some(provider) = font_provider {
            if let Some(data) = provider(default_name) {
                return Some(data);
            }
        }
        embedded_font(default_name)
    })?;

    let font = parse_type1(&font_data).ok()?;
    let fm = font.font_matrix;
    let mut font_matrix = Matrix::new(fm[0], fm[1], fm[2], fm[3], fm[4], fm[5]);

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
        // When the substitute font's glyph widths differ significantly from the
        // PDF's expected widths, scale glyph outlines horizontally to match.
        // This handles narrow/condensed variants AND unembedded decorative fonts
        // (e.g. Spumoni) where the substitute (NimbusSans) has wider glyphs.
        {
            let mut pdf_sum = 0.0;
            let mut sub_sum = 0.0;
            let mut count = 0;
            // Only compare widths within the PDF's /Widths range [FirstChar, LastChar].
            // Characters outside this range may have /MissingWidth values that don't
            // represent real glyph usage and would skew the scaling ratio.
            for code in first_char..=last_char.min(255) {
                let pdf_w = widths[code];
                if pdf_w <= 0.0 {
                    continue;
                }
                let glyph_name = match encoding[code].as_deref() {
                    Some(n) if n != ".notdef" && n != "space" => n,
                    _ => continue,
                };
                if let Some(cs) = font.charstrings.get(glyph_name)
                    && let Ok(result) = execute_charstring(cs, &font.subrs, font.len_iv, false)
                {
                    let sub_w = result.width_x * fm[0];
                    if sub_w > 0.0 {
                        let ratio = pdf_w / sub_w;
                        // Skip mismatched entries (likely unused encoding slots)
                        if ratio > 0.5 && ratio < 2.0 {
                            pdf_sum += pdf_w;
                            sub_sum += sub_w;
                            count += 1;
                        }
                    }
                }
            }
            if count >= 3 && sub_sum > 0.0 {
                let ratio = pdf_sum / sub_sum;
                if (ratio - 1.0).abs() > 0.03 {
                    font_matrix.a *= ratio;
                }
            }
        }
        widths
    };

    // Substitute fonts don't use Multiple Master blending
    let weight_vector = font.weight_vector.clone();

    Some(PdfFont::Type1(Type1PdfFont {
        font,
        encoding,
        widths,
        font_matrix,
        builtin_fallback: false,
        weight_vector,
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
    if lower.contains("helvetica")
        || lower.contains("arial")
        || lower.contains("sans")
        || lower.contains("calibri")
        || lower.contains("verdana")
        || lower.contains("tahoma")
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
    ("Arial", "LiberationSans"),
    ("Arial,Bold", "LiberationSans-Bold"),
    ("Arial,BoldItalic", "LiberationSans-BoldItalic"),
    ("Arial,Italic", "LiberationSans-Italic"),
    ("Arial-BoldMT", "LiberationSans-Bold"),
    ("Arial-BoldItalicMT", "LiberationSans-BoldItalic"),
    ("Arial-ItalicMT", "LiberationSans-Italic"),
    ("ArialMT", "LiberationSans"),
    ("CourierNew", "LiberationMono"),
    ("CourierNew,Bold", "LiberationMono-Bold"),
    ("CourierNew,BoldItalic", "LiberationMono-BoldItalic"),
    ("CourierNew,Italic", "LiberationMono-Italic"),
    ("CourierNewPS-BoldMT", "LiberationMono-Bold"),
    ("CourierNewPS-BoldItalicMT", "LiberationMono-BoldItalic"),
    ("CourierNewPS-ItalicMT", "LiberationMono-Italic"),
    ("CourierNewPSMT", "LiberationMono"),
    ("LucidaConsole", "LiberationMono"),
    ("LucidaConsole,Bold", "LiberationMono-Bold"),
    ("Calibri", "LiberationSans"),
    ("Calibri,Bold", "LiberationSans-Bold"),
    ("Calibri,BoldItalic", "LiberationSans-BoldItalic"),
    ("Calibri,Italic", "LiberationSans-Italic"),
    ("TimesNewRoman", "LiberationSerif"),
    ("TimesNewRoman,Bold", "LiberationSerif-Bold"),
    ("TimesNewRoman,BoldItalic", "LiberationSerif-BoldItalic"),
    ("TimesNewRoman,Italic", "LiberationSerif-Italic"),
    ("TimesNewRomanPS-BoldMT", "LiberationSerif-Bold"),
    ("TimesNewRomanPS-BoldItalicMT", "LiberationSerif-BoldItalic"),
    ("TimesNewRomanPS-ItalicMT", "LiberationSerif-Italic"),
    ("TimesNewRomanPSMT", "LiberationSerif"),
    // Japanese CJK fonts → NotoSansCJK (OpenType/CFF, has both ASCII and CJK)
    ("HeiseiMin-W3", "NotoSansCJKjp-Regular"),
    ("HeiseiKakuGo-W5", "NotoSansCJKjp-Regular"),
    ("KozMinPr6N-Regular", "NotoSansCJKjp-Regular"),
    ("KozGoPr6N-Medium", "NotoSansCJKjp-Regular"),
    ("MS-Gothic", "NotoSansCJKjp-Regular"),
    ("MS-Gothic,Bold", "NotoSansCJKjp-Bold"),
    ("MS-Gothic,Italic", "NotoSansCJKjp-Regular"),
    ("MS-Gothic,BoldItalic", "NotoSansCJKjp-Bold"),
    ("MS-PGothic", "NotoSansCJKjp-Regular"),
    ("MS-PGothic,Bold", "NotoSansCJKjp-Bold"),
    ("MS-PGothic,Italic", "NotoSansCJKjp-Regular"),
    ("MS-PGothic,BoldItalic", "NotoSansCJKjp-Bold"),
    ("MS-Mincho", "NotoSansCJKjp-Regular"),
    ("MS-Mincho,Bold", "NotoSansCJKjp-Bold"),
    ("MS-Mincho,Italic", "NotoSansCJKjp-Regular"),
    ("MS-Mincho,BoldItalic", "NotoSansCJKjp-Bold"),
    ("MS-PMincho", "NotoSansCJKjp-Regular"),
    ("MS-PMincho,Bold", "NotoSansCJKjp-Bold"),
    ("MS-PMincho,Italic", "NotoSansCJKjp-Regular"),
    ("MS-PMincho,BoldItalic", "NotoSansCJKjp-Bold"),
    ("MSGothic", "NotoSansCJKjp-Regular"),
    ("MSPGothic", "NotoSansCJKjp-Regular"),
    ("MSMincho", "NotoSansCJKjp-Regular"),
    ("MSPMincho", "NotoSansCJKjp-Regular"),
    // Korean CJK fonts
    ("Batang", "NotoSansCJKkr-Regular"),
    ("BatangChe", "NotoSansCJKkr-Regular"),
    ("Dotum", "NotoSansCJKkr-Regular"),
    ("DotumChe", "NotoSansCJKkr-Regular"),
    ("Gulim", "NotoSansCJKkr-Regular"),
    ("GulimChe", "NotoSansCJKkr-Regular"),
    // Chinese Simplified CJK fonts (Adobe standard CID fonts)
    // NotoSerifCJKjp contains all CJK glyphs including SC/TC
    ("STSongStd-Light", "NotoSerifCJKjp-Regular"),
    ("STSong-Light", "NotoSerifCJKjp-Regular"),
    ("AdobeSongStd-Light", "NotoSerifCJKjp-Regular"),
    ("STFangsong-Light", "NotoSerifCJKjp-Regular"),
    ("STHeiti-Regular", "NotoSansCJKjp-Regular"),
    ("STKaiti-Regular", "NotoSansCJKjp-Regular"),
    ("SimSun", "NotoSerifCJKjp-Regular"),
    ("SimSunBold", "NotoSerifCJKjp-Bold"),
    ("SimHei", "NotoSansCJKjp-Regular"),
    ("FangSong", "NotoSerifCJKjp-Regular"),
    ("KaiTi", "NotoSansCJKjp-Regular"),
    // Chinese Traditional CJK fonts (Adobe standard CID fonts)
    ("MSungStd-Light", "NotoSerifCJKjp-Regular"),
    ("MSung-Light", "NotoSerifCJKjp-Regular"),
    ("AdobeMingStd-Light", "NotoSerifCJKjp-Regular"),
    ("MHei-Medium", "NotoSansCJKjp-Regular"),
    ("MingLiU", "NotoSerifCJKjp-Regular"),
    ("PMingLiU", "NotoSerifCJKjp-Regular"),
];

/// Check if an OpenType/CFF font contains a CID-keyed CFF (has ROS operator).
fn is_cff_cid_keyed(otf_data: &[u8]) -> bool {
    use stet_fonts::truetype::find_table;
    let Some((cff_off, cff_len)) = find_table(otf_data, b"CFF ") else {
        return false;
    };
    let cff_data = &otf_data[cff_off..cff_off + cff_len];
    match parse_cff(cff_data) {
        Ok(fonts) => fonts.first().map_or(false, |f| f.is_cid),
        Err(_) => false,
    }
}

/// Create a CidCff font from an OpenType/CFF system font (OTTO magic).
/// Extracts the CFF table and builds a CidCffPdfFont.
fn create_cid_cff_from_otf(
    otf_data: &[u8],
    default_width: f64,
    cid_widths: HashMap<u16, f64>,
    ordering: &[u8],
    pdf_cid_to_gid: Option<Vec<u16>>,
    identity_cid_to_gid: bool,
    code_lengths: [u8; 256],
    code_to_cid: HashMap<u32, u32>,
    wmode: u8,
    dw2: [f64; 2],
    w2: HashMap<u16, [f64; 3]>,
) -> Result<PdfFont, PdfError> {
    use stet_fonts::truetype::find_table;

    // Extract CFF table from OpenType font
    let (cff_off, cff_len) = find_table(otf_data, b"CFF ")
        .ok_or(PdfError::Other("OpenType font has no CFF table".into()))?;
    let cff_data = &otf_data[cff_off..cff_off + cff_len];
    let fonts =
        parse_cff(cff_data).map_err(|e| PdfError::Other(format!("CFF parse error: {e}")))?;
    let font = fonts
        .into_iter()
        .next()
        .ok_or(PdfError::Other("CFF contains no fonts".into()))?;
    let fm = font.font_matrix;
    let font_matrix = Matrix::new(fm[0], fm[1], fm[2], fm[3], fm[4], fm[5]);

    // Parse cmap from OTF for Unicode→GID lookup (substitute fonts)
    let otf_cmap = parse_cmap(otf_data);
    let cmap = if otf_cmap.is_empty() {
        None
    } else {
        Some(otf_cmap)
    };

    Ok(PdfFont::CidCff(CidCffPdfFont {
        font,
        default_width,
        cid_widths,
        font_matrix,
        cmap,
        pdf_cid_to_gid,
        identity_cid_to_gid,
        ordering: ordering.to_vec(),
        code_lengths,
        code_to_cid,
        wmode,
        dw2,
        w2,
    }))
}

/// Detect raw CFF font data (not wrapped in an OpenType container).
/// CFF starts with: major=1, minor=0, hdrSize>=4, offSize in 1..=4.
fn is_raw_cff(data: &[u8]) -> bool {
    data.len() > 4 && data[0] == 1 && data[1] == 0 && data[2] >= 4 && (1..=4).contains(&data[3])
}

/// Create a CidCff font from raw CFF data (no OpenType wrapper).
fn create_cid_cff_from_raw(
    cff_data: &[u8],
    default_width: f64,
    cid_widths: HashMap<u16, f64>,
    ordering: &[u8],
    pdf_cid_to_gid: Option<Vec<u16>>,
    identity_cid_to_gid: bool,
    code_lengths: [u8; 256],
    code_to_cid: HashMap<u32, u32>,
    wmode: u8,
    dw2: [f64; 2],
    w2: HashMap<u16, [f64; 3]>,
) -> Result<PdfFont, PdfError> {
    let fonts =
        parse_cff(cff_data).map_err(|e| PdfError::Other(format!("CFF parse error: {e}")))?;
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
        cmap: None, // Raw CFF has no OTF cmap table
        pdf_cid_to_gid,
        identity_cid_to_gid,
        ordering: ordering.to_vec(),
        code_lengths,
        code_to_cid,
        wmode,
        dw2,
        w2,
    }))
}

/// Fix malformed `head.indexToLocFormat` in embedded TrueType fonts.
///
/// Some PDF generators write invalid values (e.g. 256 instead of 0 or 1).
/// Skrifa checks `== 1` for long format, so any value other than 0 or 1
/// causes it to use short format incorrectly. Determine the correct format
/// from the loca table size and patch the head table in-place.
fn sanitize_index_to_loc_format(font_data: &mut [u8]) {
    use stet_fonts::truetype::{find_table, read_i16, read_u16};

    let head = find_table(font_data, b"head");
    let loca = find_table(font_data, b"loca");
    let maxp = find_table(font_data, b"maxp");
    let (head_off, _) = match head {
        Some(h) => h,
        None => return,
    };
    if head_off + 52 > font_data.len() {
        return;
    }
    let format = read_i16(font_data, head_off + 50);
    if format == 0 || format == 1 {
        return; // already valid
    }
    // Determine correct format from loca table size vs numGlyphs
    let correct = if let (Some((_, loca_len)), Some((maxp_off, _))) = (loca, maxp) {
        if maxp_off + 6 <= font_data.len() {
            let num_glyphs = read_u16(font_data, maxp_off + 4) as usize;
            // Long format: (numGlyphs + 1) * 4 bytes
            // Short format: (numGlyphs + 1) * 2 bytes
            if loca_len == (num_glyphs + 1) * 4 {
                1i16 // long
            } else {
                0i16 // short
            }
        } else {
            if format != 0 { 1 } else { 0 }
        }
    } else {
        if format != 0 { 1 } else { 0 }
    };
    font_data[head_off + 50] = (correct >> 8) as u8;
    font_data[head_off + 51] = correct as u8;
}

/// Try to load a TrueType font from the system font cache.
///
/// Used when a CIDFontType2 font is not embedded in the PDF (missing FontFile2).
/// Falls back to substitution table and fuzzy name matching.
fn load_system_truetype_font(base_font: &str) -> Result<Vec<u8>, PdfError> {
    use stet_fonts::system_fonts::get_system_font_cache;

    let cache = get_system_font_cache();

    // Strip subset prefix (e.g. "ABCDEF+Calibri,Bold" → "Calibri,Bold")
    let mut clean_name = base_font;
    if clean_name.len() > 7 && clean_name.as_bytes().get(6) == Some(&b'+') {
        clean_name = &clean_name[7..];
    }

    // Try exact match first
    if let Some(path) = cache.get_font_path(clean_name)
        && let Ok(data) = read_font_file(path, clean_name)
    {
        return Ok(data);
    }

    // Try known substitutions
    for &(from, to) in CID_FONT_SUBSTITUTIONS {
        if from == clean_name
            && let Some(path) = cache.get_font_path(to)
            && let Ok(data) = read_font_file(path, to)
        {
            return Ok(data);
        }
    }

    // Fuzzy family match — split on '-' or ',' to extract family name
    let lower = clean_name.to_ascii_lowercase();
    let is_bold = lower.contains("bold") || lower.contains("demi");
    let is_italic = lower.contains("italic") || lower.contains("oblique");

    for (ps_name, path) in cache.iter() {
        let ps_lower = ps_name.to_ascii_lowercase();
        let family = lower.split(&['-', ','][..]).next().unwrap_or(&lower);
        if ps_lower.contains(family) || family.contains(ps_lower.split('-').next().unwrap_or("")) {
            let name_bold = ps_lower.contains("bold") || ps_lower.contains("demi");
            let name_italic = ps_lower.contains("italic") || ps_lower.contains("oblique");
            if name_bold == is_bold
                && name_italic == is_italic
                && let Ok(data) = read_font_file(path, ps_name)
            {
                return Ok(data);
            }
        }
    }

    Err(PdfError::Other(format!(
        "font '{}' not found on system",
        clean_name
    )))
}

/// Fallback for CID fonts whose names can't be resolved (e.g. GBK-encoded
/// native names like 黑体). Uses the CIDSystemInfo Ordering to select
/// the appropriate Noto CJK regional variant.
fn load_cjk_fallback_font(ordering: &[u8], base_font: &str) -> Result<Vec<u8>, PdfError> {
    use stet_fonts::system_fonts::get_system_font_cache;

    if ordering.is_empty() {
        return Err(PdfError::Other("no CJK ordering for fallback".into()));
    }

    let cache = get_system_font_cache();
    let lower = base_font.to_ascii_lowercase();
    let is_bold = lower.contains("bold") || lower.contains("demi");

    // Noto CJK .ttc files contain JP/SC/TC/HK/KR sub-fonts; the system font
    // cache typically indexes only the first (JP). The JP variant includes
    // all CJK unified ideographs, so it works for all orderings.
    let target = if is_bold {
        "NotoSansCJKjp-Bold"
    } else {
        "NotoSansCJKjp-Regular"
    };
    if let Some(path) = cache.get_font_path(target)
        && let Ok(data) = read_font_file(path, target)
    {
        return Ok(data);
    }

    Err(PdfError::Other(format!(
        "CJK fallback font '{}' not found on system",
        target
    )))
}

/// Embedded Type 1 substitute fonts (URW families).
/// Compiled into the binary so the PDF reader works from any directory.
const EMBEDDED_FONTS: &[(&str, &[u8])] = &[
    // NimbusRoman (Times)
    ("NimbusRoman-Regular", include_bytes!("../../../../resources/Font/NimbusRoman-Regular.t1")),
    ("NimbusRoman-Bold", include_bytes!("../../../../resources/Font/NimbusRoman-Bold.t1")),
    ("NimbusRoman-Italic", include_bytes!("../../../../resources/Font/NimbusRoman-Italic.t1")),
    ("NimbusRoman-BoldItalic", include_bytes!("../../../../resources/Font/NimbusRoman-BoldItalic.t1")),
    // NimbusSans (Helvetica/Arial)
    ("NimbusSans-Regular", include_bytes!("../../../../resources/Font/NimbusSans-Regular.t1")),
    ("NimbusSans-Bold", include_bytes!("../../../../resources/Font/NimbusSans-Bold.t1")),
    ("NimbusSans-Italic", include_bytes!("../../../../resources/Font/NimbusSans-Italic.t1")),
    ("NimbusSans-BoldItalic", include_bytes!("../../../../resources/Font/NimbusSans-BoldItalic.t1")),
    // NimbusSansNarrow (Helvetica Narrow)
    ("NimbusSansNarrow-Regular", include_bytes!("../../../../resources/Font/NimbusSansNarrow-Regular.t1")),
    ("NimbusSansNarrow-Bold", include_bytes!("../../../../resources/Font/NimbusSansNarrow-Bold.t1")),
    ("NimbusSansNarrow-Oblique", include_bytes!("../../../../resources/Font/NimbusSansNarrow-Oblique.t1")),
    ("NimbusSansNarrow-BoldOblique", include_bytes!("../../../../resources/Font/NimbusSansNarrow-BoldOblique.t1")),
    // NimbusMonoPS (Courier)
    ("NimbusMonoPS-Regular", include_bytes!("../../../../resources/Font/NimbusMonoPS-Regular.t1")),
    ("NimbusMonoPS-Bold", include_bytes!("../../../../resources/Font/NimbusMonoPS-Bold.t1")),
    ("NimbusMonoPS-Italic", include_bytes!("../../../../resources/Font/NimbusMonoPS-Italic.t1")),
    ("NimbusMonoPS-BoldItalic", include_bytes!("../../../../resources/Font/NimbusMonoPS-BoldItalic.t1")),
    // P052 (Palatino)
    ("P052-Roman", include_bytes!("../../../../resources/Font/P052-Roman.t1")),
    ("P052-Bold", include_bytes!("../../../../resources/Font/P052-Bold.t1")),
    ("P052-Italic", include_bytes!("../../../../resources/Font/P052-Italic.t1")),
    ("P052-BoldItalic", include_bytes!("../../../../resources/Font/P052-BoldItalic.t1")),
    // C059 (New Century Schoolbook)
    ("C059-Roman", include_bytes!("../../../../resources/Font/C059-Roman.t1")),
    ("C059-Bold", include_bytes!("../../../../resources/Font/C059-Bold.t1")),
    ("C059-Italic", include_bytes!("../../../../resources/Font/C059-Italic.t1")),
    ("C059-BdIta", include_bytes!("../../../../resources/Font/C059-BdIta.t1")),
    // URWBookman (Bookman)
    ("URWBookman-Light", include_bytes!("../../../../resources/Font/URWBookman-Light.t1")),
    ("URWBookman-Demi", include_bytes!("../../../../resources/Font/URWBookman-Demi.t1")),
    ("URWBookman-LightItalic", include_bytes!("../../../../resources/Font/URWBookman-LightItalic.t1")),
    ("URWBookman-DemiItalic", include_bytes!("../../../../resources/Font/URWBookman-DemiItalic.t1")),
    // URWGothic (AvantGarde)
    ("URWGothic-Book", include_bytes!("../../../../resources/Font/URWGothic-Book.t1")),
    ("URWGothic-Demi", include_bytes!("../../../../resources/Font/URWGothic-Demi.t1")),
    ("URWGothic-BookOblique", include_bytes!("../../../../resources/Font/URWGothic-BookOblique.t1")),
    ("URWGothic-DemiOblique", include_bytes!("../../../../resources/Font/URWGothic-DemiOblique.t1")),
    // Symbol fonts
    ("StandardSymbolsPS", include_bytes!("../../../../resources/Font/StandardSymbolsPS.t1")),
    ("D050000L", include_bytes!("../../../../resources/Font/D050000L.t1")),
    ("Z003-MediumItalic", include_bytes!("../../../../resources/Font/Z003-MediumItalic.t1")),
];

/// Look up an embedded Type 1 substitute font by name.
fn embedded_font(name: &str) -> Option<Vec<u8>> {
    EMBEDDED_FONTS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, data)| data.to_vec())
}

/// Read a font file, handling TrueType Collection (.ttc) files by extracting
/// the sub-font matching `ps_name` (or the first font if no match found).
fn read_font_file(path: &std::path::Path, ps_name: &str) -> std::io::Result<Vec<u8>> {
    let data = std::fs::read(path)?;
    if data.len() > 12 && &data[0..4] == b"ttcf" {
        // TTC: extract the sub-font at the correct offset
        let num_fonts = u32::from_be_bytes([data[8], data[9], data[10], data[11]]) as usize;
        // Try to find the font matching ps_name by checking each font's name table
        let mut best_offset = if num_fonts > 0 {
            u32::from_be_bytes([data[12], data[13], data[14], data[15]]) as usize
        } else {
            0
        };
        for i in 0..num_fonts {
            let off_pos = 12 + i * 4;
            if off_pos + 4 > data.len() {
                break;
            }
            let font_offset = u32::from_be_bytes([
                data[off_pos],
                data[off_pos + 1],
                data[off_pos + 2],
                data[off_pos + 3],
            ]) as usize;
            // Check PostScript name in the name table of this sub-font
            if let Some(name) = extract_ps_name_at_offset(&data, font_offset)
                && name == ps_name
            {
                best_offset = font_offset;
                break;
            }
        }
        // Build a standalone TTF by rewriting the header to point to tables
        // at their absolute offsets within the TTC
        extract_ttf_from_ttc(&data, best_offset)
    } else {
        Ok(data)
    }
}

/// Extract the PostScript name from a font at a given offset within TTC data.
fn extract_ps_name_at_offset(data: &[u8], offset: usize) -> Option<String> {
    use stet_fonts::truetype::read_u16;
    // Manually find the 'name' table from the sub-font's table directory
    if offset + 12 > data.len() {
        return None;
    }
    let num_tables = read_u16(data, offset + 4) as usize;
    let mut name_off = 0usize;
    let mut name_len = 0usize;
    for i in 0..num_tables {
        let entry = offset + 12 + i * 16;
        if entry + 16 > data.len() {
            break;
        }
        if &data[entry..entry + 4] == b"name" {
            name_off = u32::from_be_bytes([
                data[entry + 8],
                data[entry + 9],
                data[entry + 10],
                data[entry + 11],
            ]) as usize;
            name_len = u32::from_be_bytes([
                data[entry + 12],
                data[entry + 13],
                data[entry + 14],
                data[entry + 15],
            ]) as usize;
            break;
        }
    }
    if name_off == 0 || name_off + name_len > data.len() {
        return None;
    }
    let nd = &data[name_off..name_off + name_len];
    let count = read_u16(nd, 2) as usize;
    let string_offset = read_u16(nd, 4) as usize;
    for i in 0..count {
        let rec = 6 + i * 12;
        if rec + 12 > nd.len() {
            break;
        }
        let pid = read_u16(nd, rec);
        let name_id = read_u16(nd, rec + 6);
        let length = read_u16(nd, rec + 8) as usize;
        let str_off = read_u16(nd, rec + 10) as usize;
        if name_id == 6 {
            let start = string_offset + str_off;
            if start + length <= nd.len() {
                let raw = &nd[start..start + length];
                if pid == 3 {
                    let s: String = raw
                        .chunks(2)
                        .filter_map(|c| {
                            if c.len() == 2 {
                                Some(u16::from_be_bytes([c[0], c[1]]) as u8 as char)
                            } else {
                                None
                            }
                        })
                        .collect();
                    return Some(s);
                } else {
                    return Some(String::from_utf8_lossy(raw).to_string());
                }
            }
        }
    }
    None
}

/// Extract a single TTF from a TTC by building a standalone font file.
/// The sub-font header at `font_offset` contains a table directory with
/// offsets that are absolute within the TTC. We copy the header + directory
/// and then append all referenced table data, adjusting offsets accordingly.
fn extract_ttf_from_ttc(ttc_data: &[u8], font_offset: usize) -> std::io::Result<Vec<u8>> {
    use stet_fonts::truetype::{read_u16, read_u32};

    if font_offset + 12 > ttc_data.len() {
        return Err(std::io::Error::other("TTC font offset out of range"));
    }

    let num_tables = read_u16(ttc_data, font_offset + 4) as usize;
    let header_size = 12 + num_tables * 16;

    // Collect table info: (tag, ttc_offset, length)
    let mut tables = Vec::with_capacity(num_tables);
    for i in 0..num_tables {
        let entry = font_offset + 12 + i * 16;
        if entry + 16 > ttc_data.len() {
            break;
        }
        let tag = &ttc_data[entry..entry + 4];
        let offset = read_u32(ttc_data, entry + 8) as usize;
        let length = read_u32(ttc_data, entry + 12) as usize;
        tables.push((tag.to_vec(), offset, length));
    }

    // Build standalone TTF: header + directory + table data
    let mut result = Vec::with_capacity(
        header_size + tables.iter().map(|(_, _, l)| (l + 3) & !3).sum::<usize>(),
    );

    // Copy the 12-byte sfnt header
    result.extend_from_slice(&ttc_data[font_offset..font_offset + 12]);

    // First pass: compute new offsets (tables follow directory)
    let mut data_offset = header_size as u32;
    let mut new_offsets = Vec::with_capacity(num_tables);
    for (_, _, length) in &tables {
        new_offsets.push(data_offset);
        data_offset += ((*length as u32) + 3) & !3; // 4-byte aligned
    }

    // Write table directory with new offsets
    for (i, (tag, _, length)) in tables.iter().enumerate() {
        let entry = font_offset + 12 + i * 16;
        result.extend_from_slice(tag); // tag
        result.extend_from_slice(&ttc_data[entry + 4..entry + 8]); // checksum
        result.extend_from_slice(&new_offsets[i].to_be_bytes()); // new offset
        result.extend_from_slice(&(*length as u32).to_be_bytes()); // length
    }

    // Copy table data
    for (_, ttc_offset, length) in &tables {
        let end = (*ttc_offset + *length).min(ttc_data.len());
        if *ttc_offset < ttc_data.len() {
            result.extend_from_slice(&ttc_data[*ttc_offset..end]);
            // Pad to 4-byte alignment
            let pad = (4 - (length % 4)) % 4;
            result.extend(std::iter::repeat_n(0u8, pad));
        }
    }

    Ok(result)
}

/// Resolve a Type 3 font: glyphs defined as content streams.
fn resolve_type3(resolver: &Resolver, font_dict: &PdfDict) -> Result<PdfFont, PdfError> {
    let first_char = font_dict.get_int(b"FirstChar").unwrap_or(0) as usize;

    // Parse widths array (already in glyph space — Type 3 FontMatrix maps to text space).
    // /Widths may be an indirect reference — resolve before accessing.
    let mut widths = [0.0f64; 256];
    let widths_resolved = font_dict
        .get(b"Widths")
        .and_then(|obj| resolver.deref(obj).ok());
    if let Some(ref w_obj) = widths_resolved
        && let Some(w_arr) = w_obj.as_array()
    {
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
    let (encoding, _, _) = resolve_encoding(font_dict, resolver)?;

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
        if let Some(glyph_name) = &encoding[code as usize]
            && let Some(proc_ref) = char_procs_dict.get(glyph_name.as_bytes())
            && let Ok(data) = resolver.stream_data_from_obj(proc_ref)
        {
            char_procs.insert(code as u8, data);
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
    has_explicit_encoding: bool,
    differences: &[(usize, String)],
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
                use stet_fonts::truetype::find_table;
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

    // PDF spec 9.6.6.1: encoding base depends on whether the font is embedded.
    let encoding = if !differences.is_empty() && font.encoding.len() == 256 {
        // Encoding dict had Differences but no BaseEncoding. For embedded fonts,
        // the base is the font's built-in encoding, not StandardEncoding.
        let mut builtin: [Option<String>; 256] = std::array::from_fn(|_| None);
        for (i, name) in font.encoding.iter().enumerate() {
            if name != ".notdef" {
                builtin[i] = Some(name.clone());
            }
        }
        for (code, name) in differences {
            if *code < 256 {
                builtin[*code] = Some(name.clone());
            }
        }
        builtin
    } else if !has_explicit_encoding {
        let flags = desc.get_int(b"Flags").unwrap_or(0) as u32;
        let is_symbolic = flags & 4 != 0;
        if is_symbolic && font.encoding.len() == 256 {
            let mut builtin: [Option<String>; 256] = std::array::from_fn(|_| None);
            for (i, name) in font.encoding.iter().enumerate() {
                if name != ".notdef" {
                    builtin[i] = Some(name.clone());
                }
            }
            builtin
        } else {
            encoding
        }
    } else {
        encoding
    };

    // Check if the encoding's glyph names are completely incompatible with the
    // font's CharStrings (e.g. StandardEncoding "A","B" vs custom "G41","G42").
    // If so, enable fallback to the font's built-in encoding at glyph lookup.
    let builtin_fallback = {
        let flags = desc.get_int(b"Flags").unwrap_or(0) as u32;
        let is_sym = flags & 4 != 0;
        let builtin_useful = is_sym
            && font.encoding.len() == 256
            && font.encoding.iter().any(|n| n != ".notdef");
        if builtin_useful {
            !encoding[32..127].iter().any(|slot| {
                slot.as_ref()
                    .is_some_and(|name| font.charstrings.contains_key(name.as_str()))
            })
        } else {
            false
        }
    };

    let fm = font.font_matrix;
    let font_matrix = Matrix::new(fm[0], fm[1], fm[2], fm[3], fm[4], fm[5]);

    let weight_vector = font.weight_vector.clone();
    Ok(PdfFont::Type1(Type1PdfFont {
        font,
        encoding,
        widths,
        font_matrix,
        weight_vector,
        builtin_fallback,
    }))
}

/// Resolve a TrueType font from its FontDescriptor.
/// If the font data has table directory entries pointing past the data,
/// try re-decompressing with raw deflate (skipping the zlib header).
/// Some fonts have corrupt zlib headers (CINFO < 7) that cause truncation.
fn try_raw_deflate_if_truncated(
    resolver: &Resolver,
    ff_ref: &PdfObj,
    data: Vec<u8>,
) -> Vec<u8> {
    // Check if any table extends past the data
    if data.len() < 12 {
        return data;
    }
    let num_tables = u16::from_be_bytes([data[4], data[5]]) as usize;
    let mut max_end = 0usize;
    for i in 0..num_tables {
        let e = 12 + i * 16;
        if e + 16 > data.len() {
            break;
        }
        let off = u32::from_be_bytes([data[e + 8], data[e + 9], data[e + 10], data[e + 11]])
            as usize;
        let len = u32::from_be_bytes([data[e + 12], data[e + 13], data[e + 14], data[e + 15]])
            as usize;
        max_end = max_end.max(off.saturating_add(len));
    }
    if max_end <= data.len() {
        return data; // all tables fit, no truncation
    }
    // Tables extend past the data — try raw deflate on the stream
    let raw_bytes = match resolver.raw_stream_bytes(ff_ref) {
        Some(b) if b.len() > 2 => b,
        _ => return data,
    };
    // Only retry if the zlib header has CINFO < 7 (suspect window size)
    let cinfo = raw_bytes[0] >> 4;
    let cm = raw_bytes[0] & 0xF;
    if cm != 8 || cinfo >= 7 {
        return data;
    }
    // Decompress with raw deflate (skip 2-byte zlib header)
    let mut decoder = flate2::Decompress::new(false);
    let mut output = Vec::with_capacity(data.len() * 2);
    let mut buf = [0u8; 8192];
    let input = &raw_bytes[2..];
    let mut input_offset = 0;
    loop {
        let before_in = decoder.total_in() as usize;
        let before_out = decoder.total_out() as usize;
        let result = decoder.decompress(
            &input[input_offset..],
            &mut buf,
            flate2::FlushDecompress::None,
        );
        let consumed = decoder.total_in() as usize - before_in;
        let produced = decoder.total_out() as usize - before_out;
        input_offset += consumed;
        output.extend_from_slice(&buf[..produced]);
        match result {
            Ok(flate2::Status::StreamEnd) => break,
            Ok(_) => {
                if consumed == 0 && produced == 0 {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    if output.len() <= data.len() {
        return data;
    }
    // Verify ALL tables fit in the raw output (reject if still truncated)
    let mut raw_max_end = 0usize;
    for i in 0..num_tables {
        let e = 12 + i * 16;
        if e + 16 > output.len() {
            return data;
        }
        let off = u32::from_be_bytes([output[e + 8], output[e + 9], output[e + 10], output[e + 11]])
            as usize;
        let len = u32::from_be_bytes([output[e + 12], output[e + 13], output[e + 14], output[e + 15]])
            as usize;
        raw_max_end = raw_max_end.max(off.saturating_add(len));
    }
    if raw_max_end > output.len() {
        return data; // raw output still truncated, don't use it
    }
    // Validate the head table has a plausible unitsPerEm (detects shifted data
    // where the head bytes are misaligned and read as zero)
    if stet_fonts::truetype::get_units_per_em(&output) == 0 {
        return data;
    }
    output
}

fn resolve_truetype(
    resolver: &Resolver,
    descriptor: &Option<PdfDict>,
    encoding: [Option<String>; 256],
    widths: [f64; 256],
    font_dict: &PdfDict,
) -> Result<PdfFont, PdfError> {
    let desc = descriptor.as_ref().ok_or(PdfError::Other(
        "TrueType font missing FontDescriptor".into(),
    ))?;
    let ff_ref = desc
        .get(b"FontFile2")
        .ok_or(PdfError::Other("TrueType font missing FontFile2".into()))?;
    let data = resolver.stream_data_from_obj(ff_ref)?;

    // Some fonts have corrupt zlib headers (CINFO < 7) that cause the zlib
    // decompressor to truncate. If key tables are out of bounds, try raw
    // deflate decompression which ignores the header.
    let data = try_raw_deflate_if_truncated(resolver, ff_ref, data);

    // Validate that glyph outline data is actually present
    use stet_fonts::truetype::find_table;
    let has_glyf = find_table(&data, b"glyf").is_some();
    let has_usable_glyx = if let Some((off, len)) = find_table(&data, b"glyx") {
        off + len <= data.len()
    } else {
        false
    };
    if !has_glyf && !has_usable_glyx {
        // Some PDFs store CFF/OpenType fonts as FontFile2 (malformed but common).
        // Detect and route to CFF parsing instead of failing.
        let is_otf = data.starts_with(b"OTTO");
        let is_cff = is_raw_cff(&data);
        if is_otf || is_cff {
            let has_explicit_encoding = font_dict.get(b"Encoding").is_some();
            let has_pdf_widths = font_dict.get(b"Widths").is_some();
            return build_cff_font(data, encoding, widths, has_explicit_encoding, has_pdf_widths, &[]);
        }
        return Err(PdfError::Other(
            "TrueType font has no usable glyph outline data".into(),
        ));
    }

    // Validate essential tables are within bounds. Truncated font data
    // (e.g. from corrupt zlib headers) may have table directory entries
    // pointing past the decompressed data.
    if let Some((off, _)) = find_table(&data, b"head") {
        if off + 54 > data.len() {
            return Err(PdfError::Other(
                "TrueType font head table is out of bounds (truncated data)".into(),
            ));
        }
    }

    let units_per_em = get_units_per_em(&data) as f64;
    let (cmap, cmap_is_unicode) = parse_cmap_with_info(&data);

    // Parse post table (GID → name) and invert to name → GID for fallback lookup
    let post_name_to_gid = stet_fonts::system_fonts::parse_post_table(&data)
        .map(|gid_to_name| {
            gid_to_name
                .into_iter()
                .map(|(gid, name)| (name, gid))
                .collect()
        })
        .unwrap_or_default();

    // Symbolic TrueType fonts without explicit /Encoding use identity mapping
    // (char_code = GID). The cmap is often misleading for re-encoded fonts
    // (e.g. Tamil glyphs at Latin cmap positions).
    let flags = desc.get_int(b"Flags").unwrap_or(0) as u32;
    let is_symbolic = flags & 4 != 0;
    let has_encoding = font_dict.get(b"Encoding").is_some();
    let identity_gid = is_symbolic && !has_encoding && cmap_is_unicode;
    let gid_hex = TrueTypePdfFont::detect_gid_hex(&encoding);

    Ok(PdfFont::TrueType(TrueTypePdfFont {
        data,
        encoding,
        widths,
        cmap,
        cmap_is_unicode,
        post_name_to_gid,
        units_per_em,
        to_unicode: if let Some(tu_obj) = font_dict.get(b"ToUnicode") {
            resolver.stream_data_from_obj(tu_obj)
                .map(|d| parse_to_unicode(&d))
                .unwrap_or_default()
        } else {
            HashMap::new()
        },
        identity_gid,
        gid_hex,
    }))
}

/// Resolve a CFF (Type1C) font from its FontDescriptor.
fn resolve_cff(
    resolver: &Resolver,
    descriptor: &Option<PdfDict>,
    encoding: [Option<String>; 256],
    widths: [f64; 256],
    has_explicit_encoding: bool,
    has_pdf_widths: bool,
    differences: &[(usize, String)],
) -> Result<PdfFont, PdfError> {
    let desc = descriptor
        .as_ref()
        .ok_or(PdfError::Other("CFF font missing FontDescriptor".into()))?;
    let ff_ref = desc
        .get(b"FontFile3")
        .ok_or(PdfError::Other("CFF font missing FontFile3".into()))?;
    let raw_data = resolver.stream_data_from_obj(ff_ref)?;
    build_cff_font(raw_data, encoding, widths, has_explicit_encoding, has_pdf_widths, differences)
}

/// Build a CFF font from raw font data (may be OpenType/CFF or raw CFF).
fn build_cff_font(
    raw_data: Vec<u8>,
    encoding: [Option<String>; 256],
    widths: [f64; 256],
    has_explicit_encoding: bool,
    has_pdf_widths: bool,
    differences: &[(usize, String)],
) -> Result<PdfFont, PdfError> {
    // If data starts with "OTTO" it's an OpenType container — extract CFF table
    let font_data = if raw_data.starts_with(b"OTTO") {
        use stet_fonts::truetype::find_table;
        let (offset, length) = find_table(&raw_data, b"CFF ")
            .ok_or(PdfError::Other("OpenType font has no CFF table".into()))?;
        raw_data[offset..offset + length].to_vec()
    } else {
        raw_data
    };

    let fonts =
        parse_cff(&font_data).map_err(|e| PdfError::Other(format!("CFF parse error: {e}")))?;
    let font = fonts
        .into_iter()
        .next()
        .ok_or(PdfError::Other("CFF contains no fonts".into()))?;

    // PDF spec 9.6.6.1: encoding base depends on whether the font is embedded.
    let encoding = if !differences.is_empty() {
        // Encoding dict had Differences but no BaseEncoding — use CFF built-in
        // encoding as base, then apply Differences.
        let mut enc: [Option<String>; 256] = std::array::from_fn(|_| None);
        #[allow(clippy::needless_range_loop)]
        for code in 0..256 {
            let gid = font.encoding[code] as usize;
            if gid > 0 && gid < font.charset.len() && font.charset[gid] != ".notdef" {
                enc[code] = Some(font.charset[gid].clone());
            }
        }
        for (code, name) in differences {
            if *code < 256 {
                enc[*code] = Some(name.clone());
            }
        }
        enc
    } else if !has_explicit_encoding {
        // No /Encoding at all — use CFF built-in encoding directly.
        let mut enc: [Option<String>; 256] = std::array::from_fn(|_| None);
        #[allow(clippy::needless_range_loop)]
        for code in 0..256 {
            let gid = font.encoding[code] as usize;
            if gid > 0 && gid < font.charset.len() && font.charset[gid] != ".notdef" {
                enc[code] = Some(font.charset[gid].clone());
            }
        }
        enc
    } else {
        encoding
    };

    let fm = font.font_matrix;
    let font_matrix = Matrix::new(fm[0], fm[1], fm[2], fm[3], fm[4], fm[5]);

    // When the PDF has no /Widths array, derive widths from the CFF charstrings.
    let widths = if !has_pdf_widths {
        use stet_fonts::type2_charstring::execute_type2_charstring;
        let mut derived = [0.0f64; 256];
        for code in 0..256usize {
            let glyph_name = encoding[code].as_deref().unwrap_or(".notdef");
            let gid = font
                .charset
                .iter()
                .position(|name| name == glyph_name)
                .unwrap_or(0);
            if gid > 0 && gid < font.char_strings.len() {
                if let Ok(result) = execute_type2_charstring(
                    &font.char_strings[gid],
                    &font.local_subrs,
                    &font.global_subrs,
                    font.default_width_x,
                    font.nominal_width_x,
                    true, // width_only
                ) {
                    derived[code] = result.width_x * fm[0];
                }
            }
        }
        derived
    } else {
        widths
    };

    Ok(PdfFont::Cff(CffPdfFont {
        font,
        encoding,
        widths,
        font_matrix,
    }))
}

/// Resolve a Type 0 composite font (CIDFontType2 descendant with TrueType outlines).
fn resolve_type0(resolver: &Resolver, font_dict: &PdfDict) -> Result<PdfFont, PdfError> {
    // Check if encoding is UCS2-based (character codes are Unicode, not CIDs).
    let encoding_obj = font_dict.get(b"Encoding");
    let encoding_name = font_dict.get_name(b"Encoding").unwrap_or(b"");
    let ucs2_encoding = encoding_name.windows(4).any(|w| w == b"UCS2");

    // Parse the encoding CMap's codespace ranges to determine byte widths,
    // and the code-to-CID mapping for non-identity encodings.
    // The encoding can be:
    //   - a stream containing a custom CMap
    //   - a name like "Identity-H" (identity mapping, 2-byte codes)
    //   - a predefined CMap name like "GBK-EUC-H" (load from system)
    let (code_lengths, code_to_cid, mut wmode) = if let Some(enc_obj) = encoding_obj {
        if let Ok(cmap_data) = resolver.stream_data_from_obj(enc_obj) {
            // Embedded CMap stream
            let cmap = super::cmap::CMap::parse_with_loader(
                &cmap_data,
                Some(&|name| load_predefined_cmap(name)),
            );
            (cmap.code_lengths, cmap.code_to_cid, cmap.wmode)
        } else if !encoding_name.is_empty()
            && !encoding_name.starts_with(b"Identity")
        {
            // Predefined CMap name (e.g. GBK-EUC-H) — load from system
            if let Some(cmap_data) = load_predefined_cmap(encoding_name) {
                let cmap = super::cmap::CMap::parse_with_loader(
                    &cmap_data,
                    Some(&|name| load_predefined_cmap(name)),
                );
                (cmap.code_lengths, cmap.code_to_cid, cmap.wmode)
            } else {
                eprintln!(
                    "warning: predefined CMap '{}' not found; \
                     set STET_CMAP_DIR or install poppler-data for CJK support",
                    String::from_utf8_lossy(encoding_name)
                );
                ([2u8; 256], HashMap::new(), 0)
            }
        } else {
            ([2u8; 256], HashMap::new(), 0) // Identity-H/V or fallback
        }
    } else {
        ([2u8; 256], HashMap::new(), 0)
    };
    // Encoding name suffix overrides CMap WMode: -V = vertical, -H = horizontal
    if encoding_name.ends_with(b"-V") {
        wmode = 1;
    } else if encoding_name.ends_with(b"-H") {
        wmode = 0;
    }

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

    // Parse /DW (default width) — may be int or real
    let default_width = cid_font_dict.get_f64(b"DW").unwrap_or(1000.0) / 1000.0;

    // Parse /DW2 (default vertical metrics: [v_y w1])
    // Default: [880, -1000] per PDF spec Table 117
    let dw2 = cid_font_dict
        .get_array(b"DW2")
        .and_then(|arr| {
            let v: Vec<f64> = arr.iter().filter_map(|o| o.as_f64()).collect();
            if v.len() >= 2 {
                Some([v[0], v[1]])
            } else {
                None
            }
        })
        .unwrap_or([880.0, -1000.0]);

    // Parse /W array (CID-specific widths)
    let cid_widths = parse_cid_widths(cid_font_dict, resolver);

    // Parse /W2 array (per-CID vertical metrics)
    let w2 = parse_cid_w2(cid_font_dict, resolver);

    // When a UCS2-based CMap (e.g. UniJIS-UCS2-H) couldn't be loaded from
    // disk, build a basic Latin fallback mapping. All Adobe CID collections
    // (Japan1, GB1, CNS1, Korea1) map Unicode basic Latin to:
    //   CID 1 = U+0020 (space), CID 2..95 = U+0021..U+007E
    // This handles the common case of CJK-font PDFs containing English text
    // when CMap resource files aren't installed.
    let code_to_cid = if code_to_cid.is_empty()
        && code_lengths[0] == 2
        && encoding_name.windows(4).any(|w| w == b"UCS2")
    {
        let mut map = HashMap::new();
        for unicode in 0x0020u32..=0x007Eu32 {
            let cid = unicode - 0x001F;
            map.insert(unicode, cid);
        }
        map
    } else {
        code_to_cid
    };

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
                let mut font_data = resolver.stream_data_from_obj(ff_ref)?;
                sanitize_index_to_loc_format(&mut font_data);
                // Some PDFs store CFF fonts as FontFile2 instead of FontFile3.
                // Detect OpenType/CFF (OTTO magic) or raw CFF and route accordingly.
                let is_otf_cff = font_data.len() > 4 && &font_data[0..4] == b"OTTO";
                let is_raw = is_raw_cff(&font_data);
                if is_otf_cff || is_raw {
                    // Parse CIDToGIDMap from the PDF before creating the CFF font
                    let cid_to_gid_map = if let Some(map_obj) = cid_font_dict.get(b"CIDToGIDMap") {
                        if cid_font_dict.get_name(b"CIDToGIDMap") != Some(b"Identity") {
                            resolver.stream_data_from_obj(map_obj).ok().map(|d| {
                                d.chunks_exact(2)
                                    .map(|p| u16::from_be_bytes([p[0], p[1]]))
                                    .collect()
                            })
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    let identity = cid_to_gid_map.is_none();
                    if is_otf_cff {
                        return create_cid_cff_from_otf(
                            &font_data,
                            default_width,
                            cid_widths,
                            &ordering,
                            cid_to_gid_map,
                            identity,
                            code_lengths,
                            code_to_cid.clone(),
                            wmode,
                            dw2,
                            w2.clone(),
                        );
                    } else {
                        return create_cid_cff_from_raw(
                            &font_data,
                            default_width,
                            cid_widths,
                            &ordering,
                            cid_to_gid_map,
                            identity,
                            code_lengths,
                            code_to_cid.clone(),
                            wmode,
                            dw2,
                            w2.clone(),
                        );
                    }
                }
                font_data
            } else {
                // Font not embedded — try system font lookup
                substituted = true;
                let base_font = cid_font_dict
                    .get_name(b"BaseFont")
                    .map(|n| {
                        let s = String::from_utf8_lossy(n);
                        if s.len() > 7 && s.as_bytes().get(6) == Some(&b'+') {
                            s[7..].to_string()
                        } else {
                            s.to_string()
                        }
                    })
                    .unwrap_or_default();
                let sys_data = load_system_truetype_font(&base_font)
                    .or_else(|_| load_cjk_fallback_font(&ordering, &base_font))?;
                // If the system font is OpenType/CFF, use CFF rendering path
                if sys_data.len() > 4 && &sys_data[0..4] == b"OTTO" {
                    return create_cid_cff_from_otf(
                        &sys_data,
                        default_width,
                        cid_widths,
                        &ordering,
                        None,
                        false, // substituted: use cmap, not identity
                        code_lengths,
                        code_to_cid.clone(),
                        wmode,
                        dw2,
                        w2.clone(),
                    );
                }
                sys_data
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
                            let mut gid_map = Vec::with_capacity(stream_data.len() / 2);
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
                ucs2_encoding,
                code_lengths,
                code_to_cid: code_to_cid.clone(),
                wmode,
                dw2,
                w2: w2.clone(),
            }))
        }
        b"CIDFontType0" => {
            // CFF-based CID font: FontFile3 with /Subtype /CIDFontType0C
            // Some PDFs use /FontFile instead of /FontFile3 — accept both.
            if let Some(ff_ref) = desc.get(b"FontFile3").or_else(|| desc.get(b"FontFile")) {
                let font_data = resolver.stream_data_from_obj(ff_ref)?;
                // FontFile3 may be raw CFF or OpenType/CFF (OTTO wrapper)
                if font_data.len() > 4 && &font_data[0..4] == b"OTTO" {
                    // Parse CIDToGIDMap for OpenType-wrapped CFF
                    let pdf_cid_to_gid =
                        if let Some(map_obj) = cid_font_dict.get(b"CIDToGIDMap") {
                            match resolver.stream_data_from_obj(map_obj) {
                                Ok(stream_data) => {
                                    let mut gid_map =
                                        Vec::with_capacity(stream_data.len() / 2);
                                    for pair in stream_data.chunks_exact(2) {
                                        gid_map
                                            .push(u16::from_be_bytes([pair[0], pair[1]]));
                                    }
                                    Some(gid_map)
                                }
                                Err(_) => None,
                            }
                        } else {
                            None
                        };
                    // For non-CID CFF fonts used as CIDFontType0, the CID IS the
                    // charstring index (identity mapping). For true CID-keyed CFF fonts,
                    // the CFF charset provides the CID→GID mapping, or the OTF cmap is used.
                    let cff_is_cid = is_cff_cid_keyed(&font_data);
                    return create_cid_cff_from_otf(
                        &font_data,
                        default_width,
                        cid_widths,
                        &ordering,
                        pdf_cid_to_gid,
                        !cff_is_cid,
                        code_lengths,
                        code_to_cid.clone(),
                        wmode,
                        dw2,
                        w2.clone(),
                    );
                }
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
                    cmap: None,
                    pdf_cid_to_gid: None,
                    identity_cid_to_gid: false,
                    ordering: ordering.clone(),
                    code_lengths,
                    code_to_cid: code_to_cid.clone(),
                    wmode,
                    dw2,
                    w2: w2.clone(),
                }))
            } else {
                // Not embedded — substitute with a system font
                let base_font = cid_font_dict
                    .get_name(b"BaseFont")
                    .map(|n| String::from_utf8_lossy(n).to_string())
                    .unwrap_or_default();
                let sys_data = if ucs2_encoding {
                    // UCS2-encoded CID fonts: use a substitute TrueType font so
                    // the text stays on the composite CID rendering path (correct
                    // code_lengths and CID width advancement). Without this, the
                    // simple fallback font treats 2-byte UCS-2 codes as individual
                    // bytes, producing doubled character spacing.
                    load_system_truetype_font(&base_font)
                        .or_else(|_| load_system_truetype_font("DejaVuSans"))
                        .or_else(|_| load_system_truetype_font("LiberationSans"))
                        .or_else(|_| load_system_truetype_font("NimbusSans"))?
                } else {
                    load_system_truetype_font(&base_font)
                        .or_else(|_| load_cjk_fallback_font(&ordering, &base_font))?
                };
                // If the system font is OpenType/CFF, use CFF rendering path
                if sys_data.len() > 4 && &sys_data[0..4] == b"OTTO" {
                    return create_cid_cff_from_otf(
                        &sys_data,
                        default_width,
                        cid_widths,
                        &ordering,
                        None,
                        false, // substituted: use cmap, not identity
                        code_lengths,
                        code_to_cid.clone(),
                        wmode,
                        dw2,
                        w2.clone(),
                    );
                }
                let data = sys_data;
                let units_per_em = get_units_per_em(&data) as f64;
                let cmap = parse_cmap(&data);
                Ok(PdfFont::CidTrueType(CidTrueTypePdfFont {
                    data,
                    default_width,
                    cid_widths,
                    cmap,
                    units_per_em,
                    identity_cid_to_gid: false,
                    substituted: true,
                    cid_to_gid_map: None,
                    to_unicode,
                    ordering: ordering.clone(),
                    ucs2_encoding,
                    code_lengths,
                    code_to_cid: code_to_cid.clone(),
                    wmode,
                    dw2,
                    w2,
                }))
            }
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
            "00660066" => Some(0xFB00),     // ff
            "00660069" => Some(0xFB01),     // fi
            "0066006C" => Some(0xFB02),     // fl
            "006600660069" => Some(0xFB03), // ffi
            "00660066006C" => Some(0xFB04), // ffl
            "017F0074" => Some(0xFB05),     // ſt (long s + t)
            "00730074" => Some(0xFB06),     // st
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
            if tokens.len() >= 2
                && let Ok(cid) = u32::from_str_radix(tokens[0], 16)
                && let Some(unicode) = hex_to_unicode(tokens[1])
            {
                map.insert(cid as u16, unicode);
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
                if all_before_bracket.len() >= 2
                    && let (Some(start), Some(end)) = (
                        u32::from_str_radix(all_before_bracket[0], 16).ok(),
                        u32::from_str_radix(all_before_bracket[1], 16).ok(),
                    )
                {
                    for (j, cid) in (start..=end).enumerate() {
                        if j < in_bracket.len()
                            && let Some(u) = hex_to_unicode(in_bracket[j])
                        {
                            map.insert(cid as u16, u);
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
    // /W may be an indirect reference — resolve it before accessing as array
    let w_obj = match cid_font_dict.get(b"W") {
        Some(obj) => match resolver.deref(obj) {
            Ok(resolved) => resolved,
            Err(_) => return widths,
        },
        None => return widths,
    };
    let w_arr = match w_obj.as_array() {
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

/// Parse /W2 array from CIDFont dict into CID → vertical metrics map.
///
/// Format mirrors /W but each entry has 3 values: w1 (vertical advance),
/// v_x and v_y (position vector from horizontal to vertical origin).
/// All values are in 1/1000 em units (NOT divided by 1000).
fn parse_cid_w2(cid_font_dict: &PdfDict, resolver: &Resolver) -> HashMap<u16, [f64; 3]> {
    let mut metrics = HashMap::new();
    let w2_obj = match cid_font_dict.get(b"W2") {
        Some(obj) => match resolver.deref(obj) {
            Ok(resolved) => resolved,
            Err(_) => return metrics,
        },
        None => return metrics,
    };
    let arr = match w2_obj.as_array() {
        Some(a) => a,
        None => return metrics,
    };
    let mut i = 0;
    while i < arr.len() {
        let first_cid = match &arr[i] {
            PdfObj::Int(n) => *n as u16,
            _ => break,
        };
        i += 1;
        if i >= arr.len() {
            break;
        }
        let next = resolver.deref(&arr[i]).unwrap_or(arr[i].clone());
        match &next {
            PdfObj::Array(sub) => {
                // [ cid_first [w1_1 v_x1 v_y1 w1_2 v_x2 v_y2 ...] ]
                let vals: Vec<f64> = sub.iter().filter_map(|o| o.as_f64()).collect();
                for (j, chunk) in vals.chunks(3).enumerate() {
                    if chunk.len() == 3 {
                        metrics.insert(first_cid + j as u16, [chunk[0], chunk[1], chunk[2]]);
                    }
                }
                i += 1;
            }
            _ => {
                // [ cid_first cid_last w1 v_x v_y ]
                let last_cid = match &next {
                    PdfObj::Int(n) => *n as u16,
                    _ => first_cid,
                };
                i += 1;
                if i + 2 < arr.len() {
                    let w1 = arr[i].as_f64().unwrap_or(-1000.0);
                    let vx = arr[i + 1].as_f64().unwrap_or(0.0);
                    let vy = arr[i + 2].as_f64().unwrap_or(880.0);
                    i += 3;
                    for cid in first_cid..=last_cid {
                        metrics.insert(cid, [w1, vx, vy]);
                    }
                } else {
                    break;
                }
            }
        }
    }
    metrics
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

    /// Get glyph path for a Unicode code point, bypassing CID machinery.
    /// Used for malformed PDFs that mix WinAnsi literal strings in CID fonts.
    pub fn glyph_path_unicode(&self, unicode: u16) -> Option<PsPath> {
        match self {
            PdfFont::CidTrueType(f) => f.glyph_path_unicode(unicode),
            PdfFont::CidCff(f) => f.glyph_path_unicode(unicode),
            _ => None,
        }
    }

    /// Get width for a Unicode code point from hmtx, bypassing CID widths.
    pub fn glyph_width_unicode(&self, unicode: u16) -> f64 {
        match self {
            PdfFont::CidTrueType(f) => f.glyph_width_unicode(unicode),
            _ => 0.0,
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
    ///
    /// Returns identity for CidCff because the full matrix (including per-FD
    /// composition) is applied inside `glyph_path_cid()`.
    pub fn font_matrix(&self) -> Matrix {
        match self {
            PdfFont::Type1(f) => f.font_matrix,
            PdfFont::TrueType(_) | PdfFont::CidTrueType(_) => Matrix::identity(),
            PdfFont::Cff(f) => f.font_matrix,
            PdfFont::CidCff(_) => Matrix::identity(),
            PdfFont::Type3(f) => f.font_matrix,
        }
    }

    /// Whether this is a composite (CID) font that uses multi-byte character codes.
    pub fn is_composite(&self) -> bool {
        matches!(self, PdfFont::CidTrueType(_) | PdfFont::CidCff(_))
    }

    /// Writing mode: 0 = horizontal, 1 = vertical.
    pub fn wmode(&self) -> u8 {
        match self {
            PdfFont::CidTrueType(f) => f.wmode,
            PdfFont::CidCff(f) => f.wmode,
            _ => 0,
        }
    }

    /// Default vertical metrics [v_y, w1] for vertical writing mode.
    /// v_y = vertical origin y offset (in 1/1000 em), w1 = vertical advance.
    pub fn dw2(&self) -> [f64; 2] {
        match self {
            PdfFont::CidTrueType(f) => f.dw2,
            PdfFont::CidCff(f) => f.dw2,
            _ => [880.0, -1000.0],
        }
    }

    /// Get per-CID vertical metrics (w1, v_x, v_y), falling back to DW2.
    /// Returns values in 1/1000 em units.
    pub fn vertical_metrics_cid(&self, cid: u16) -> [f64; 3] {
        match self {
            PdfFont::CidTrueType(f) => {
                if let Some(&m) = f.w2.get(&cid) {
                    m
                } else {
                    // DW2 = [v_y, w1]; v_x defaults to half the horizontal width
                    let w0 = f.cid_widths.get(&cid).copied().unwrap_or(f.default_width) * 1000.0;
                    [f.dw2[1], w0 / 2.0, f.dw2[0]]
                }
            }
            PdfFont::CidCff(f) => {
                if let Some(&m) = f.w2.get(&cid) {
                    m
                } else {
                    let w0 = f.cid_widths.get(&cid).copied().unwrap_or(f.default_width) * 1000.0;
                    [f.dw2[1], w0 / 2.0, f.dw2[0]]
                }
            }
            _ => [-1000.0, 500.0, 880.0],
        }
    }

    /// Whether a CID maps to a GID that exists in the font.
    /// Used to distinguish valid 2-byte CID codes from misinterpreted WinAnsi
    /// bytes in malformed PDFs that mix 1-byte literal text with CID fonts.
    pub fn has_cid_glyph(&self, cid: u16) -> bool {
        match self {
            PdfFont::CidTrueType(f) => f.has_glyph(cid),
            PdfFont::CidCff(_) => true, // CFF handles this differently
            _ => false,
        }
    }

    /// Map a raw character code to a CID using the encoding CMap.
    /// Returns the code unchanged if no mapping exists (identity encoding).
    pub fn resolve_code_to_cid(&self, code: u32) -> u32 {
        match self {
            PdfFont::CidTrueType(f) => {
                f.code_to_cid.get(&code).copied().unwrap_or(code)
            }
            PdfFont::CidCff(f) => {
                f.code_to_cid.get(&code).copied().unwrap_or(code)
            }
            _ => code,
        }
    }

    /// Get the byte width of a character code starting with the given byte.
    /// Only meaningful for composite fonts; returns 1 for simple fonts.
    pub fn code_width(&self, first_byte: u8) -> usize {
        match self {
            PdfFont::CidTrueType(f) => {
                let w = f.code_lengths[first_byte as usize];
                if w == 0 { 2 } else { w as usize }
            }
            PdfFont::CidCff(f) => {
                let w = f.code_lengths[first_byte as usize];
                if w == 0 { 2 } else { w as usize }
            }
            _ => 1,
        }
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
        let glyph_name = self.encoding[char_code as usize].as_deref();
        let charstring = glyph_name
            .and_then(|name| self.font.charstrings.get(name))
            .or_else(|| {
                if !self.builtin_fallback { return None; }
                let builtin = self.font.encoding.get(char_code as usize)?;
                if builtin != ".notdef"
                    && glyph_name.map_or(true, |n| n != builtin)
                {
                    self.font.charstrings.get(builtin.as_str())
                } else {
                    None
                }
            })?;
        // Provide charstring lookup for seac (accented character composition)
        let cs_lookup =
            |name: &str| -> Option<Vec<u8>> { self.font.charstrings.get(name).cloned() };
        let result = execute_charstring_mm(
            charstring,
            &self.font.subrs,
            self.font.len_iv,
            false,
            Some(&cs_lookup),
            self.weight_vector.as_deref(),
        )
        .ok()?;
        Some(result.path)
    }
}

impl TrueTypePdfFont {
    /// Check if any gNNNN glyph name in the encoding contains hex letters (a-f),
    /// indicating the subsetting tool used hexadecimal GIDs.
    fn detect_gid_hex(encoding: &[Option<String>; 256]) -> bool {
        encoding.iter().any(|name| {
            if let Some(n) = name {
                n.starts_with('g')
                    && n.len() > 1
                    && n[1..].bytes().all(|b| b.is_ascii_hexdigit())
                    && n[1..].bytes().any(|b| b.is_ascii_hexdigit() && !b.is_ascii_digit())
            } else {
                false
            }
        })
    }

    fn glyph_path(&self, char_code: u8) -> Option<PsPath> {
        let gid = self.char_code_to_gid(char_code)?;
        let path = skrifa_glyph_path(&self.data, gid, self.units_per_em).or_else(|| {
            // Fallback for locx/glyx PDF-subset fonts that skrifa can't parse
            let glyf_data = get_glyf_data(&self.data, gid)?;
            let data_ref = &self.data;
            let p = parse_glyf_to_path(&glyf_data, &|cid| get_glyf_data(data_ref, cid));
            if p.is_empty() { None } else { Some(p) }
        })?;
        let scale = 1.0 / self.units_per_em;
        let m = Matrix::scale(scale, scale);
        Some(path.transform(&m))
    }

    fn char_code_to_gid(&self, char_code: u8) -> Option<u16> {
        // Symbolic re-encoded fonts: skip the encoding→AGL→cmap path, which maps
        // StandardEncoding names (e.g. "circumflex") to wrong Unicode→GID values.
        // Go directly to the cmap lookup by char code.
        if self.identity_gid {
            if let Some(&gid) = self.cmap.get(&(char_code as u32)) {
                return Some(gid);
            }
            if let Some(&gid) = self.cmap.get(&(0xF000 + char_code as u32)) {
                return Some(gid);
            }
            return Some(char_code as u16);
        }
        if let Some(glyph_name) = &self.encoding[char_code as usize] {
            // Only use encoding → glyph name → Unicode → cmap when the cmap
            // is Unicode-keyed ((3,1), (3,10), or (0,*)).  Non-Unicode cmaps
            // ((1,0) Mac Roman, (3,0) Symbol) in subset fonts map re-encoded
            // char codes directly — looking up Unicode values gives wrong GIDs.
            if self.cmap_is_unicode {
                if let Some(unicode) = stet_fonts::agl::glyph_name_to_unicode(glyph_name)
                    && let Some(&gid) = self.cmap.get(&(unicode as u32))
                {
                    return Some(gid);
                }
            }
        }
        // ToUnicode CMap → Unicode → cmap GID.  Tried before gNNNN because
        // gNNNN GIDs are font-specific — they're wrong for substitute fonts
        // (e.g. SimSun g18331 ≠ NotoSerif GID 18331).
        // Only use when cmap is Unicode-keyed — non-Unicode cmaps map
        // re-encoded char codes, not Unicode values.
        if self.cmap_is_unicode {
            if let Some(&unicode) = self.to_unicode.get(&(char_code as u16))
                && let Some(&gid) = self.cmap.get(&unicode)
            {
                return Some(gid);
            }
        }
        if let Some(glyph_name) = &self.encoding[char_code as usize] {
            // Try gNNNN pattern → direct GID (for embedded fonts where GIDs match).
            // Some subsetting tools use hex (g003a = GID 58), others decimal (g1863).
            // detect_gid_hex() checks if any name in this font has hex letters (a-f).
            if glyph_name.starts_with('g')
                && glyph_name.len() > 1
                && glyph_name[1..].bytes().all(|b| b.is_ascii_hexdigit())
            {
                let suffix = &glyph_name[1..];
                let gid = if self.gid_hex {
                    u16::from_str_radix(suffix, 16).ok()
                } else {
                    suffix.parse::<u16>().ok()
                };
                if let Some(gid) = gid {
                    return Some(gid);
                }
            }
        }
        // Direct cmap lookup by char code — preferred over post table for subset
        // TrueType fonts where glyph names may not match character code positions.
        if let Some(&gid) = self.cmap.get(&(char_code as u32)) {
            return Some(gid);
        }
        if let Some(glyph_name) = &self.encoding[char_code as usize] {
            // Post table fallback (handles ligatures like fl/fi)
            if let Some(&gid) = self.post_name_to_gid.get(glyph_name.as_str()) {
                return Some(gid);
            }
        }
        // Windows Symbol encoding (U+F0XX range, common in subset fonts)
        if let Some(&gid) = self.cmap.get(&(0xF000 + char_code as u32)) {
            return Some(gid);
        }
        if self.cmap.is_empty() {
            // No cmap table: use char code as GID directly (PDF subset identity mapping)
            Some(char_code as u16)
        } else {
            None
        }
    }
}

impl CidTrueTypePdfFont {
    /// For UCS2 encodings, convert Unicode code point to CID for width lookup.
    fn resolve_cid(&self, code: u16) -> u16 {
        // Only remap when code_to_cid is empty (no CMap loaded) — in that case
        // the code IS a raw Unicode code point that needs mapping to a CID.
        // When a CMap IS loaded, it has already mapped to the correct CID.
        if self.ucs2_encoding && !self.ordering.is_empty() && self.code_to_cid.is_empty() {
            super::cid_unicode::unicode_to_cid(&self.ordering, code as u32).unwrap_or(code)
        } else {
            code
        }
    }

    fn glyph_path_cid(&self, cid: u16) -> Option<PsPath> {
        let gid = if self.ucs2_encoding && !self.cmap.is_empty() && self.code_to_cid.is_empty() {
            // UCS2 encoding with no CMap: cid is a raw Unicode code point, map via cmap.
            if let Some(&g) = self.cmap.get(&(cid as u32)) {
                g
            } else {
                return None;
            }
        } else if self.ucs2_encoding && self.substituted && !self.ordering.is_empty() {
            // CMap was loaded: cid is an Adobe CID, convert back to Unicode for glyph lookup
            let unicode = super::cid_unicode::cid_to_unicode(&self.ordering, cid)?;
            *self.cmap.get(&unicode)?
        } else if let Some(ref map) = self.cid_to_gid_map {
            // Explicit CIDToGIDMap stream: look up CID → GID
            *map.get(cid as usize).unwrap_or(&0)
        } else if self.substituted && !self.to_unicode.is_empty() {
            // Substituted font: CID → Unicode (via ToUnicode) → GID (via cmap)
            let unicode = *self.to_unicode.get(&cid)?;
            *self.cmap.get(&unicode)?
        } else if self.substituted && !self.ordering.is_empty() && self.ordering != b"Identity" {
            // Substituted font with Adobe CID registry (CJK): use CID→Unicode table
            let unicode = super::cid_unicode::cid_to_unicode(&self.ordering, cid)?;
            *self.cmap.get(&unicode)?
        } else if self.identity_cid_to_gid {
            // Identity CIDToGIDMap: CID = GID directly.
            // For substituted fonts without ToUnicode, the substitute (e.g.
            // Liberation Mono for Courier New) has compatible glyph ordering.
            cid
        } else if !self.cmap.is_empty() {
            // Non-Identity mapping: CID is Unicode, use cmap
            *self.cmap.get(&(cid as u32))?
        } else {
            cid
        };
        let path = skrifa_glyph_path(&self.data, gid, self.units_per_em).or_else(|| {
            // Fallback for fonts where skrifa can't render a glyph (e.g. locx/glyx
            // PDF-subset tables, or skrifa CFF rendering gaps).
            let glyf_data = get_glyf_data(&self.data, gid)?;
            let data_ref = &self.data;
            let p = parse_glyf_to_path(&glyf_data, &|cid| get_glyf_data(data_ref, cid));
            // Sanity check: real glyphs have at most a few thousand segments.
            // Bogus GIDs reading random glyf bytes can produce millions.
            if p.is_empty() || p.segments.len() > 10_000 { None } else { Some(p) }
        })?;
        let scale = 1.0 / self.units_per_em;
        let m = Matrix::scale(scale, scale);
        Some(path.transform(&m))
    }

    /// Check if a GID exists in the font (GID < numGlyphs from maxp table).
    /// Unlike glyph_path_cid, this returns true for space/whitespace GIDs
    /// that have no visible outline.
    fn has_glyph(&self, cid: u16) -> bool {
        // Resolve CID to GID using the same logic as glyph_path_cid
        let gid = if let Some(ref map) = self.cid_to_gid_map {
            *map.get(cid as usize).unwrap_or(&0)
        } else if self.identity_cid_to_gid {
            cid
        } else {
            return true; // non-identity: assume valid
        };
        // Check against font's glyph count
        let num_glyphs = stet_fonts::truetype::get_num_glyphs(&self.data);
        (gid as u32) < num_glyphs
    }

    fn glyph_width_cid(&self, cid: u16) -> f64 {
        let resolved = self.resolve_cid(cid);
        self.cid_widths
            .get(&resolved)
            .copied()
            .unwrap_or(self.default_width)
    }

    /// Get glyph path for a Unicode code point via cmap, bypassing CID mapping.
    /// Used when malformed PDFs embed WinAnsi literal strings in a CID font.
    fn glyph_path_unicode(&self, unicode: u16) -> Option<PsPath> {
        let &gid = self.cmap.get(&(unicode as u32))?;
        let path = skrifa_glyph_path(&self.data, gid, self.units_per_em)?;
        let scale = 1.0 / self.units_per_em;
        let m = Matrix::scale(scale, scale);
        Some(path.transform(&m))
    }

    /// Get width for a Unicode code point from hmtx via cmap, bypassing CID widths.
    /// Returns width in the same scale as glyph_width_cid (1/1000 of text space).
    fn glyph_width_unicode(&self, unicode: u16) -> f64 {
        if let Some(&gid) = self.cmap.get(&(unicode as u32)) {
            // hmtx_advance_width returns units in 1/1000 em; CID widths are stored
            // already divided by 1000, so divide here too for consistency.
            hmtx_advance_width(&self.data, gid, self.units_per_em)
                .map(|w| w / 1000.0)
                .unwrap_or(self.default_width)
        } else {
            self.default_width
        }
    }
}

impl CidCffPdfFont {
    /// Render the CFF charstring at the given GID.
    fn glyph_path_at_gid(&self, gid: usize) -> Option<PsPath> {
        if gid >= self.font.char_strings.len() {
            return None;
        }
        let (default_width_x, nominal_width_x, local_subrs, fd_font_matrix) = if self.font.is_cid
            && !self.font.fd_select.is_empty()
            && !self.font.fd_array.is_empty()
        {
            let fd_idx = *self.font.fd_select.get(gid).unwrap_or(&0) as usize;
            if let Some(fd) = self.font.fd_array.get(fd_idx) {
                (
                    fd.default_width_x,
                    fd.nominal_width_x,
                    &fd.local_subrs,
                    fd.font_matrix,
                )
            } else {
                (
                    self.font.default_width_x,
                    self.font.nominal_width_x,
                    &self.font.local_subrs,
                    None,
                )
            }
        } else {
            (
                self.font.default_width_x,
                self.font.nominal_width_x,
                &self.font.local_subrs,
                None,
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
        let effective_fm = if let Some(fd_fm) = fd_font_matrix {
            let fd = Matrix::new(fd_fm[0], fd_fm[1], fd_fm[2], fd_fm[3], fd_fm[4], fd_fm[5]);
            if fd.a.abs() < 0.01 || fd.d.abs() < 0.01 {
                fd
            } else {
                self.font_matrix.concat(&fd)
            }
        } else {
            self.font_matrix
        };
        Some(result.path.transform(&effective_fm))
    }

    /// Render a glyph by Unicode code point via the font's cmap table.
    fn glyph_path_unicode(&self, unicode: u16) -> Option<PsPath> {
        let cmap = self.cmap.as_ref()?;
        let &gid = cmap.get(&(unicode as u32))?;
        self.glyph_path_at_gid(gid as usize)
    }

    fn glyph_path_cid(&self, cid: u16) -> Option<PsPath> {
        // For embedded OTF/CFF with PDF CIDToGIDMap, use the PDF's mapping.
        // For OpenType/CFF substitutes with a cmap, map Unicode → GID directly.
        // For embedded CID-keyed CFF, use cid_to_gid mapping.
        let gid = if let Some(ref map) = self.pdf_cid_to_gid {
            // Embedded font with PDF-supplied CID→GID map
            *map.get(cid as usize).unwrap_or(&0) as usize
        } else if self.identity_cid_to_gid {
            // Identity CIDToGIDMap: CID = charstring index directly.
            // Common for CIDFontType2 fonts stored as OTTO/CFF in FontFile2.
            cid as usize
        } else if let Some(ref cmap) = self.cmap {
            // OTF font with Unicode cmap (substituted fonts, or non-CID fonts).
            // If this is a substituted font with an Adobe CID ordering
            // (e.g. Japan1), the CID is from the Adobe registry, not Unicode.
            // Convert CID → Unicode first, then look up in cmap.
            if !self.ordering.is_empty() && self.ordering != b"Identity" {
                let unicode =
                    super::cid_unicode::cid_to_unicode(&self.ordering, cid)?;
                *cmap.get(&unicode)? as usize
            } else {
                *cmap.get(&(cid as u32))? as usize
            }
        } else if !self.font.cid_to_gid.is_empty() {
            let g = *self.font.cid_to_gid.get(cid as usize)?;
            if g == 0xFFFF {
                return None;
            }
            g as usize
        } else {
            cid as usize
        };
        self.glyph_path_at_gid(gid)
    }

    fn glyph_width_cid(&self, cid: u16) -> f64 {
        // CID widths from the /W array are already keyed by CID — use directly.
        self.cid_widths
            .get(&cid)
            .copied()
            .unwrap_or(self.default_width)
    }
}

impl CffPdfFont {
    fn glyph_path(&self, char_code: u8) -> Option<PsPath> {
        let glyph_name = self.encoding[char_code as usize].as_deref()?;
        // The PDF /Encoding is authoritative: map char_code → glyph name,
        // then find that glyph in the CFF charset. This is essential for
        // subset fonts where the CFF internal encoding maps codes to
        // sequential GIDs that don't match the PDF encoding's glyph names.
        // Fall back to the CFF's built-in encoding only when the charset
        // lookup fails (e.g., fonts without a proper charset).
        let gid = self
            .font
            .charset
            .iter()
            .position(|name| name == glyph_name)
            .or_else(|| {
                let cff_gid = self.font.encoding.get(char_code as usize).copied().unwrap_or(0) as usize;
                if cff_gid > 0 && cff_gid < self.font.char_strings.len() {
                    Some(cff_gid)
                } else {
                    None
                }
            });
        let gid = gid?;
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
        use stet_fonts::encoding::STANDARD_ENCODING;

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
        combined
            .segments
            .extend_from_slice(&shifted_accent.segments);
        Some(combined)
    }
}

/// Pen adapter that converts skrifa outline callbacks into a `PsPath`.
struct PsPathPen {
    path: PsPath,
    cur_x: f64,
    cur_y: f64,
}

impl skrifa::outline::OutlinePen for PsPathPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.cur_x = x as f64;
        self.cur_y = y as f64;
        self.path
            .segments
            .push(PathSegment::MoveTo(self.cur_x, self.cur_y));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.cur_x = x as f64;
        self.cur_y = y as f64;
        self.path
            .segments
            .push(PathSegment::LineTo(self.cur_x, self.cur_y));
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let cx = cx as f64;
        let cy = cy as f64;
        let ex = x as f64;
        let ey = y as f64;
        // Quadratic → cubic degree elevation
        let cp1x = self.cur_x + 2.0 / 3.0 * (cx - self.cur_x);
        let cp1y = self.cur_y + 2.0 / 3.0 * (cy - self.cur_y);
        let cp2x = ex + 2.0 / 3.0 * (cx - ex);
        let cp2y = ey + 2.0 / 3.0 * (cy - ey);
        self.cur_x = ex;
        self.cur_y = ey;
        self.path.segments.push(PathSegment::CurveTo {
            x1: cp1x,
            y1: cp1y,
            x2: cp2x,
            y2: cp2y,
            x3: ex,
            y3: ey,
        });
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.cur_x = x as f64;
        self.cur_y = y as f64;
        self.path.segments.push(PathSegment::CurveTo {
            x1: cx0 as f64,
            y1: cy0 as f64,
            x2: cx1 as f64,
            y2: cy1 as f64,
            x3: self.cur_x,
            y3: self.cur_y,
        });
    }
    fn close(&mut self) {
        self.path.segments.push(PathSegment::ClosePath);
    }
}

/// Extract a TrueType glyph outline using skrifa with hinting enabled.
///
/// Hinting is needed for correct composite glyph assembly — some fonts have
/// TrueType instructions that adjust component positions. Falls back to the
/// hand-written parser for fonts skrifa can't handle (e.g., locx/glyx subsets).
/// Map a WinAnsiEncoding byte to its Unicode code point.
/// Bytes 0x00-0x7F and 0xA0-0xFF match Unicode (ISO 8859-1).
/// Bytes 0x80-0x9F differ — WinAnsi maps these to specific Unicode characters.
pub(crate) fn winansi_byte_to_unicode(byte: u8) -> u16 {
    match byte {
        0x80 => 0x20AC, // €
        0x82 => 0x201A, // ‚
        0x83 => 0x0192, // ƒ
        0x84 => 0x201E, // „
        0x85 => 0x2026, // …
        0x86 => 0x2020, // †
        0x87 => 0x2021, // ‡
        0x88 => 0x02C6, // ˆ
        0x89 => 0x2030, // ‰
        0x8A => 0x0160, // Š
        0x8B => 0x2039, // ‹
        0x8C => 0x0152, // Œ
        0x8E => 0x017D, // Ž
        0x91 => 0x2018, // '
        0x92 => 0x2019, // '
        0x93 => 0x201C, // "
        0x94 => 0x201D, // "
        0x95 => 0x2022, // •
        0x96 => 0x2013, // –
        0x97 => 0x2014, // —
        0x98 => 0x02DC, // ˜
        0x99 => 0x2122, // ™
        0x9A => 0x0161, // š
        0x9B => 0x203A, // ›
        0x9C => 0x0153, // œ
        0x9E => 0x017E, // ž
        0x9F => 0x0178, // Ÿ
        _ => byte as u16,
    }
}

/// Read the advance width for a GID from the hmtx table, returning the width
/// in text space (1/1000 em) for PDF CID width compatibility.
fn hmtx_advance_width(font_data: &[u8], gid: u16, units_per_em: f64) -> Option<f64> {
    use stet_fonts::truetype::{find_table, read_u16};
    let (hhea_off, _) = find_table(font_data, b"hhea")?;
    let (hmtx_off, _) = find_table(font_data, b"hmtx")?;
    if hhea_off + 36 > font_data.len() {
        return None;
    }
    let num_h_metrics = read_u16(font_data, hhea_off + 34) as usize;
    let gid = gid as usize;
    let advance = if gid < num_h_metrics {
        let offset = hmtx_off + gid * 4;
        if offset + 2 > font_data.len() {
            return None;
        }
        read_u16(font_data, offset)
    } else {
        // Use last metric for GIDs beyond num_h_metrics
        if num_h_metrics == 0 {
            return None;
        }
        let offset = hmtx_off + (num_h_metrics - 1) * 4;
        if offset + 2 > font_data.len() {
            return None;
        }
        read_u16(font_data, offset)
    };
    // Convert from font units to 1/1000 em (PDF text space)
    Some(advance as f64 / units_per_em * 1000.0)
}

fn skrifa_glyph_path(font_data: &[u8], gid: u16, units_per_em: f64) -> Option<PsPath> {
    let font_ref = skrifa::FontRef::new(font_data).ok()?;
    let outlines = font_ref.outline_glyphs();
    let glyph = outlines.get(skrifa::GlyphId::new(gid as u32))?;

    // Use TrueType bytecode interpreter with mono hinting for correct composite
    // glyph assembly. Some fonts have TT instructions that adjust component positions;
    // the auto-hinter doesn't handle these correctly.
    let hinting = skrifa::outline::HintingInstance::new(
        &outlines,
        skrifa::prelude::Size::new(units_per_em as f32),
        skrifa::instance::LocationRef::default(),
        skrifa::outline::HintingOptions {
            engine: skrifa::outline::Engine::Interpreter,
            target: skrifa::outline::Target::Mono,
        },
    )
    .ok();

    let mut pen = PsPathPen {
        path: PsPath::new(),
        cur_x: 0.0,
        cur_y: 0.0,
    };

    let result = if let Some(ref instance) = hinting {
        glyph.draw(instance, &mut pen)
    } else {
        glyph.draw(
            skrifa::outline::DrawSettings::unhinted(
                skrifa::prelude::Size::new(units_per_em as f32),
                skrifa::instance::LocationRef::default(),
            ),
            &mut pen,
        )
    };

    result.ok()?;
    if pen.path.is_empty() {
        None
    } else {
        Some(pen.path)
    }
}
