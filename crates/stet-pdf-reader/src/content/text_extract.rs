// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What text extraction needs to know about a PDF font.
//!
//! Read from the font dictionary rather than from the resolved
//! [`PdfFont`], whose Unicode tables serve glyph selection and fold or
//! drop text that extraction must keep (see [`stet_fonts::to_unicode`]).
//! Built only when extraction is switched on.

use stet_fonts::cid_unicode::cid_to_text;
use stet_fonts::to_unicode::ToUnicodeMap;
use stet_graphics::device::UnicodeSource;

use super::font::{PdfFont, get_font_descriptor, resolve_encoding};
use crate::objects::{PdfDict, PdfObj};
use crate::resolver::Resolver;

/// Glyph-space ascent and descent for a font with no usable metrics, in
/// 1000-unit glyph space.
const DEFAULT_ASCENT: f64 = 800.0;
const DEFAULT_DESCENT: f64 = -200.0;

/// Half the em, either side of a vertical-writing glyph's origin.
const VERTICAL_HALF_EM: f64 = 500.0;

/// How a composite font's predefined `Uni…` encoding CMap spells Unicode in
/// its character codes, which then are the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnicodeCodes {
    /// `Uni…-UCS2-…` and `Uni…-UTF16-…`: UTF-16 code units, a surrogate
    /// pair forming one four-byte code.
    Utf16,
    /// `Uni…-UTF32-…`: the code is the scalar value.
    Utf32,
    /// `Uni…-UTF8-…`: the code's bytes are UTF-8.
    Utf8,
}

/// Text-extraction data for one font, read from its dictionary.
pub(crate) struct FontText {
    /// The font's `/ToUnicode` CMap, when it has a usable one.
    to_unicode: Option<ToUnicodeMap>,
    /// Glyph name for each code of a simple font.
    glyph_names: Option<Box<[Option<String>; 256]>>,
    /// The font is ZapfDingbats, whose glyph names (`a20`) have their own list.
    zapf_dingbats: bool,
    /// The font's numeric glyph names are hexadecimal: see
    /// [`numeric_glyph_name`].
    hex_glyph_names: bool,
    /// Adobe CJK ordering (`Japan1`, …) of a composite font whose CIDs
    /// follow it.
    cid_ordering: Option<Vec<u8>>,
    /// Set when a composite font's codes are Unicode themselves.
    unicode_codes: Option<UnicodeCodes>,
    /// `/BaseFont` (or a Type 3 font's `/Name`).
    pub font_name: String,
    /// Glyph-space ascent; for vertical writing, the extent right of the
    /// glyph origin.
    pub ascent: f64,
    /// Glyph-space descent; for vertical writing, the extent left of the
    /// glyph origin.
    pub descent: f64,
}

impl FontText {
    /// Read the text-extraction data of the font `font_dict`, which
    /// resolved to `font` — or, when resolution failed and a fallback font
    /// draws it, to nothing usable (`None`).
    pub(crate) fn new(resolver: &Resolver, font_dict: &PdfDict, font: Option<&PdfFont>) -> Self {
        let to_unicode = font_dict
            .get(b"ToUnicode")
            .and_then(|obj| resolver.stream_data_from_obj(obj).ok())
            .map(|data| ToUnicodeMap::parse(&data))
            .filter(|map| !map.is_empty());

        let base_font = font_dict
            .get_name(b"BaseFont")
            .or_else(|| font_dict.get_name(b"Name"))
            .unwrap_or(b"");
        let font_name = String::from_utf8_lossy(base_font).into_owned();
        let clean_name = strip_subset_prefix(base_font);

        let mut text = FontText {
            to_unicode,
            glyph_names: None,
            zapf_dingbats: clean_name == b"ZapfDingbats",
            hex_glyph_names: false,
            cid_ordering: None,
            unicode_codes: None,
            font_name,
            ascent: DEFAULT_ASCENT,
            descent: DEFAULT_DESCENT,
        };

        if font_dict.get_name(b"Subtype") == Some(b"Type0") {
            text.read_composite(resolver, font_dict, font);
        } else {
            text.read_simple(resolver, font_dict, font);
        }
        text
    }

    /// A simple font: glyph names and metrics.
    fn read_simple(&mut self, resolver: &Resolver, font_dict: &PdfDict, font: Option<&PdfFont>) {
        self.glyph_names = match font {
            Some(PdfFont::Type1(f)) => Some(Box::new(f.encoding.clone())),
            Some(PdfFont::Cff(f)) => Some(Box::new(f.encoding.clone())),
            // A symbolic TrueType font without `/Encoding` selects glyphs
            // by code through the font's own cmap: its codes carry no
            // names, and the encoding array holds only a placeholder.
            Some(PdfFont::TrueType(f)) if f.identity_gid => None,
            Some(PdfFont::TrueType(f)) => Some(Box::new(f.encoding.clone())),
            // A Type 3 font's names are those its `/Encoding` gives; with
            // no `/BaseEncoding`, only the `/Differences` name anything.
            Some(PdfFont::Type3(_)) => resolve_encoding(font_dict, resolver).ok().map(|enc| {
                if enc.no_base_encoding {
                    let mut names: [Option<String>; 256] = std::array::from_fn(|_| None);
                    for (code, name) in enc.differences {
                        names[code] = Some(name);
                    }
                    Box::new(names)
                } else {
                    Box::new(enc.encoding)
                }
            }),
            // Resolution failed and a fallback font stands in: trust only
            // an encoding the dictionary states.
            _ => resolve_encoding(font_dict, resolver)
                .ok()
                .filter(|enc| enc.has_valid_encoding)
                .map(|enc| Box::new(enc.encoding)),
        };

        self.hex_glyph_names = self
            .glyph_names
            .as_ref()
            .is_some_and(|names| names.iter().flatten().any(|n| looks_hex(n)));

        if let Some(PdfFont::Type3(f)) = font {
            // Type 3 glyph space is the font's own, and its `/FontBBox` is
            // in it; the descriptor's metrics are not reliably so.
            let [_, y0, _, y1] = f.font_bbox;
            let (lo, hi) = (y0.min(y1), y0.max(y1));
            if hi > lo {
                self.ascent = hi;
                self.descent = lo;
            } else {
                // 0.8 / -0.2 em, in this font's glyph space.
                let em = if f.font_matrix.d.abs() > 1e-12 {
                    1.0 / f.font_matrix.d.abs()
                } else {
                    1000.0
                };
                self.ascent = 0.8 * em;
                self.descent = -0.2 * em;
            }
        } else if let Ok(Some(desc)) = get_font_descriptor(font_dict, resolver) {
            self.read_descriptor_metrics(resolver, &desc);
        }
    }

    /// A Type 0 font: the CID ordering, how its codes relate to Unicode,
    /// and the descendant font's metrics.
    fn read_composite(&mut self, resolver: &Resolver, font_dict: &PdfDict, font: Option<&PdfFont>) {
        let encoding_name = font_dict.get_name(b"Encoding").unwrap_or(b"");
        self.unicode_codes = unicode_codes(encoding_name);

        let Some(cid_font) = deref_entry(resolver, font_dict, b"DescendantFonts")
            .and_then(|obj| obj.as_array().and_then(|a| a.first().cloned()))
            .and_then(|obj| resolver.deref(&obj).ok())
            .and_then(|obj| obj.as_dict().cloned())
        else {
            return;
        };

        let system_info = deref_entry(resolver, &cid_font, b"CIDSystemInfo");
        let system_info = system_info.as_ref().and_then(|obj| obj.as_dict());
        let string = |key: &[u8]| {
            system_info
                .and_then(|d| d.get(key))
                .and_then(|obj| obj.as_str().or_else(|| obj.as_name()))
                .map(<[u8]>::to_vec)
        };
        if string(b"Registry").as_deref() == Some(b"Adobe")
            && let Some(ordering) = string(b"Ordering")
            && matches!(&ordering[..], b"Japan1" | b"CNS1" | b"GB1" | b"Korea1")
            && cids_are_known(font_dict, font)
        {
            self.cid_ordering = Some(ordering);
        }

        if font.is_some_and(|f| f.wmode() == 1) {
            self.ascent = VERTICAL_HALF_EM;
            self.descent = -VERTICAL_HALF_EM;
        } else if let Ok(Some(desc)) = get_font_descriptor(&cid_font, resolver) {
            self.read_descriptor_metrics(resolver, &desc);
        }
    }

    /// Ascent and descent from a font descriptor, else its `/FontBBox`.
    fn read_descriptor_metrics(&mut self, resolver: &Resolver, desc: &PdfDict) {
        let ascent = desc.get_f64(b"Ascent").unwrap_or(0.0);
        // Some producers write the descent as a positive distance.
        let descent = -desc.get_f64(b"Descent").unwrap_or(0.0).abs();
        if ascent > 0.0 {
            self.ascent = ascent;
            self.descent = descent;
            return;
        }
        let bbox: Vec<f64> = deref_entry(resolver, desc, b"FontBBox")
            .and_then(|obj| {
                obj.as_array()
                    .map(|a| a.iter().filter_map(PdfObj::as_f64).collect())
            })
            .unwrap_or_default();
        if let [_, y0, _, y1, ..] = bbox[..]
            && y0.max(y1) > y0.min(y1)
        {
            self.ascent = y0.max(y1);
            self.descent = y0.min(y1);
        }
    }

    /// Append the text of the glyph shown for `code` to `out`, and say
    /// where it came from. `cid` is the CID a composite font drew.
    pub(crate) fn append_text(
        &self,
        code: u32,
        cid: Option<u16>,
        out: &mut String,
    ) -> UnicodeSource {
        if let Some(text) = self.to_unicode.as_ref().and_then(|m| m.get(code)) {
            out.push_str(&text);
            return UnicodeSource::ToUnicode;
        }
        if let Some(names) = &self.glyph_names
            && let Some(Some(name)) = names.get(code as usize)
        {
            let text = if self.zapf_dingbats {
                stet_fonts::agl::zapf_dingbats_glyph_name_to_text(name)
            } else {
                stet_fonts::agl::glyph_name_to_text(name)
            };
            if let Some(text) = text {
                out.push_str(&text);
                return UnicodeSource::GlyphName;
            }
            if let Some(ch) = numeric_glyph_name(name, self.hex_glyph_names) {
                out.push(ch);
                return UnicodeSource::GlyphName;
            }
        }
        if let Some(kind) = self.unicode_codes
            && push_unicode_code(kind, code, out)
        {
            return UnicodeSource::CidOrdering;
        }
        if let (Some(ordering), Some(cid)) = (&self.cid_ordering, cid)
            && let Some(text) = cid_to_text(ordering, cid)
        {
            out.push_str(text);
            return UnicodeSource::CidOrdering;
        }
        UnicodeSource::Unmapped
    }
}

/// The character a glyph name outside the Adobe Glyph List spells as a
/// number, by the rule Poppler (after xpdf) applies to simple fonts — for
/// the bitmap Type 3 fonts dvips writes (`a80` for code 80, `P`), and
/// fonts named like them: `nn` (two to four digits), `Ann` and `ABnn`
/// (decimal after one or two characters), and, when `hex`, `xx` and `Axx`
/// (two hex digits). The number is a Latin-1 code point; control
/// characters, which such a name gives for codes below 32 (`g12`), are
/// dropped rather than invented as text.
fn numeric_glyph_name(name: &str, hex: bool) -> Option<char> {
    let b = name.as_bytes();
    let n = b.len();
    let digit = |i: usize| b.get(i).is_some_and(u8::is_ascii_digit);
    // `atoi`: the leading decimal digits of `s`.
    let leading = |s: &str| -> Option<u32> {
        let end = s
            .bytes()
            .position(|c| !c.is_ascii_digit())
            .unwrap_or(s.len());
        s[..end].parse().ok()
    };
    let code = if hex
        && n == 3
        && b[0].is_ascii_alphabetic()
        && b[1..].iter().all(u8::is_ascii_hexdigit)
    {
        u32::from_str_radix(&name[1..], 16).ok()
    } else if hex && n == 2 && b.iter().all(u8::is_ascii_hexdigit) {
        u32::from_str_radix(name, 16).ok()
    } else if !hex && (2..=4).contains(&n) && digit(0) && digit(1) {
        leading(name)
    } else if (3..=5).contains(&n) && digit(1) && digit(2) && name.is_char_boundary(1) {
        leading(&name[1..])
    } else if (4..=6).contains(&n) && digit(2) && digit(3) && name.is_char_boundary(2) {
        leading(&name[2..])
    } else {
        None
    }?;
    char::from_u32(code)
        .filter(|_| code <= 0xFF)
        .filter(|c| !c.is_control())
}

/// Whether `name` is a letter and two hex digits, at least one of them a
/// letter (`a1f`) — Poppler's sign that a font's numeric glyph names are
/// hexadecimal.
fn looks_hex(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() == 3
        && b[0].is_ascii_alphabetic()
        && b[1..].iter().all(u8::is_ascii_hexdigit)
        && b[1..].iter().any(u8::is_ascii_alphabetic)
}

/// Whether the CIDs a composite font draws are the ones its encoding
/// CMap defines. A predefined CMap the reader could not load leaves every
/// code standing in for its own CID, and reading those through the
/// collection's table would invent text.
fn cids_are_known(font_dict: &PdfDict, font: Option<&PdfFont>) -> bool {
    let code_to_cid_empty = match font {
        Some(PdfFont::CidTrueType(f)) => f.code_to_cid.is_empty(),
        Some(PdfFont::CidCff(f)) => f.code_to_cid.is_empty(),
        _ => return false,
    };
    match font_dict.get(b"Encoding") {
        // Identity-H / Identity-V: the code is the CID.
        Some(PdfObj::Name(name)) if name.starts_with(b"Identity") => true,
        Some(PdfObj::Name(_)) => !code_to_cid_empty,
        // An embedded CMap stream, parsed by the reader.
        Some(_) => true,
        None => false,
    }
}

/// How the predefined CMap `name` spells Unicode, if it is one of the
/// `Uni…` CMaps whose codes are Unicode.
fn unicode_codes(name: &[u8]) -> Option<UnicodeCodes> {
    if !name.starts_with(b"Uni") {
        return None;
    }
    let has = |needle: &[u8]| name.windows(needle.len()).any(|w| w == needle);
    if has(b"-UCS2-") || has(b"-UTF16-") {
        Some(UnicodeCodes::Utf16)
    } else if has(b"-UTF32-") {
        Some(UnicodeCodes::Utf32)
    } else if has(b"-UTF8-") {
        Some(UnicodeCodes::Utf8)
    } else {
        None
    }
}

/// Append the text a `Uni…` CMap's `code` spells; false when it spells
/// nothing valid.
fn push_unicode_code(kind: UnicodeCodes, code: u32, out: &mut String) -> bool {
    let ch = match kind {
        UnicodeCodes::Utf16 if code > 0xFFFF => {
            let (hi, lo) = ((code >> 16) as u16, code as u16);
            match char::decode_utf16([hi, lo]).next() {
                Some(Ok(c)) => Some(c),
                _ => None,
            }
        }
        UnicodeCodes::Utf16 | UnicodeCodes::Utf32 => char::from_u32(code),
        UnicodeCodes::Utf8 => {
            let bytes = code.to_be_bytes();
            let start = bytes.iter().position(|&b| b != 0).unwrap_or(3);
            std::str::from_utf8(&bytes[start..])
                .ok()
                .and_then(|s| s.chars().next())
        }
    };
    match ch {
        Some(c) if !c.is_control() => {
            out.push(c);
            true
        }
        _ => false,
    }
}

/// `name` without a `ABCDEF+` subset prefix.
fn strip_subset_prefix(name: &[u8]) -> &[u8] {
    if name.len() > 7 && name[6] == b'+' && name[..6].iter().all(u8::is_ascii_uppercase) {
        &name[7..]
    } else {
        name
    }
}

/// `dict[key]`, following an indirect reference.
fn deref_entry(resolver: &Resolver, dict: &PdfDict, key: &[u8]) -> Option<PdfObj> {
    resolver.deref(dict.get(key)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_cmap_names() {
        assert_eq!(unicode_codes(b"UniJIS-UCS2-H"), Some(UnicodeCodes::Utf16));
        assert_eq!(unicode_codes(b"UniGB-UTF16-V"), Some(UnicodeCodes::Utf16));
        assert_eq!(unicode_codes(b"UniKS-UTF32-H"), Some(UnicodeCodes::Utf32));
        assert_eq!(unicode_codes(b"UniCNS-UTF8-H"), Some(UnicodeCodes::Utf8));
        assert_eq!(
            unicode_codes(b"UniJIS-UCS2-HW-H"),
            Some(UnicodeCodes::Utf16)
        );
        assert_eq!(unicode_codes(b"90ms-RKSJ-H"), None);
        assert_eq!(unicode_codes(b"Identity-H"), None);
    }

    #[test]
    fn unicode_codes_decode() {
        let text = |kind, code| {
            let mut s = String::new();
            push_unicode_code(kind, code, &mut s).then_some(s)
        };
        assert_eq!(text(UnicodeCodes::Utf16, 0x65E5).as_deref(), Some("日"));
        // U+20BB7 as a surrogate pair.
        assert_eq!(
            text(UnicodeCodes::Utf16, 0xD842_DFB7).as_deref(),
            Some("𠮷")
        );
        assert_eq!(text(UnicodeCodes::Utf16, 0xD842), None);
        assert_eq!(text(UnicodeCodes::Utf32, 0x20BB7).as_deref(), Some("𠮷"));
        assert_eq!(text(UnicodeCodes::Utf8, 0xE6_97_A5).as_deref(), Some("日"));
        assert_eq!(text(UnicodeCodes::Utf8, 0x41).as_deref(), Some("A"));
        assert_eq!(text(UnicodeCodes::Utf8, 0xFF), None);
        assert_eq!(text(UnicodeCodes::Utf32, 0x07), None);
    }

    #[test]
    fn numeric_glyph_names() {
        let dec = |name| numeric_glyph_name(name, false);
        // dvips bitmap fonts: `a` and the code.
        assert_eq!(dec("a80"), Some('P'));
        assert_eq!(dec("a116"), Some('t'));
        // `nn`, `Ann`, `ABnn`.
        assert_eq!(dec("65"), Some('A'));
        assert_eq!(dec("g65"), Some('A'));
        assert_eq!(dec("cs233"), Some('é'));
        assert_eq!(dec("G233x"), Some('é'));
        // Control characters and codes past Latin-1 give nothing.
        assert_eq!(dec("g12"), None);
        assert_eq!(dec("a300"), None);
        // Not numeric names.
        assert_eq!(dec("Aring"), None);
        assert_eq!(dec("cid12345"), None);
        assert_eq!(dec("a"), None);
        // Hex fonts: `xx` and `Axx`; decimal `nn` no longer applies.
        let hex = |name| numeric_glyph_name(name, true);
        assert_eq!(hex("a41"), Some('A'));
        assert_eq!(hex("41"), Some('A'));
        assert_eq!(hex("ce9"), Some('é'));
        assert_eq!(hex("123"), None);
        assert!(looks_hex("a1f"));
        assert!(!looks_hex("a80"));
        assert!(!looks_hex("a1g"));
    }

    #[test]
    fn subset_prefix() {
        assert_eq!(strip_subset_prefix(b"ABCDEF+ZapfDingbats"), b"ZapfDingbats");
        assert_eq!(strip_subset_prefix(b"ZapfDingbats"), b"ZapfDingbats");
        assert_eq!(strip_subset_prefix(b"abcdef+Font"), b"abcdef+Font");
    }
}
