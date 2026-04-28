// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Document-level metadata: the Info dict and the XMP metadata stream.
//!
//! PDF documents carry author/title/etc. in two places:
//!
//! 1. The trailer's `/Info` dict (legacy, present in nearly all PDFs).
//! 2. The catalog's `/Metadata` stream (XMP XML, PDF 1.4+, increasingly the
//!    canonical form).
//!
//! [`DocumentMetadata`] returns both: parsed Info-dict fields plus the raw
//! XMP XML for callers that want to consume it themselves.
//!
//! Date strings come in PDF's own format (`D:YYYYMMDDHHmmSSOHH'mm'`); we
//! parse them into a typed [`PdfDate`].

use std::collections::HashMap;

use crate::objects::{PdfDict, PdfObj};
use crate::resolver::Resolver;

/// Information about a PDF document, drawn from the trailer's `/Info` dict
/// and the catalog's `/Metadata` stream.
///
/// All fields are optional — a PDF may have no `/Info` dict, or a partial
/// one. `custom` collects any non-standard `/Info` keys for callers that
/// need to inspect them (e.g. preservation tools).
#[derive(Debug, Clone, Default)]
pub struct DocumentMetadata {
    /// `/Info /Title`.
    pub title: Option<String>,
    /// `/Info /Author`.
    pub author: Option<String>,
    /// `/Info /Subject`.
    pub subject: Option<String>,
    /// `/Info /Keywords`.
    pub keywords: Option<String>,
    /// `/Info /Creator` — the application that authored the source document.
    pub creator: Option<String>,
    /// `/Info /Producer` — the application that produced the PDF.
    pub producer: Option<String>,
    /// `/Info /CreationDate`, parsed from PDF date format.
    pub creation_date: Option<PdfDate>,
    /// `/Info /ModDate`, parsed from PDF date format.
    pub mod_date: Option<PdfDate>,
    /// `/Info /Trapped` — whether the document has been pre-trapped for press.
    pub trapped: Option<TrappedFlag>,
    /// Non-standard `/Info` entries, decoded as strings where possible.
    pub custom: HashMap<String, String>,
    /// Raw XMP metadata XML from the catalog's `/Metadata` stream, if present.
    pub xmp_xml: Option<String>,
}

/// `/Trapped` flag value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TrappedFlag {
    /// `/True` — document has been trapped.
    True,
    /// `/False` — document has not been trapped.
    False,
    /// `/Unknown` — trapping state is unknown (PDF default when `/Trapped` is absent).
    Unknown,
}

/// A parsed PDF date string.
///
/// PDF dates use the format `D:YYYYMMDDHHmmSSOHH'mm'` where each component
/// after the year is optional. Truncated forms (`D:2026`, `D:202612`,
/// `D:20261231`) are accepted and produce a [`PdfDate`] with the
/// missing-from-the-right components defaulted to spec-correct minima
/// (month → 1, day → 1, hour/minute/second → 0).
///
/// Timezone offset (`O`) is one of `+`, `-`, `Z`. `Z` and absent both
/// produce `tz_offset_minutes = None` (UTC / unspecified).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PdfDate {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    /// Timezone offset in minutes east of UTC. `None` = UTC or unspecified.
    pub tz_offset_minutes: Option<i16>,
}

impl PdfDate {
    /// Parse a PDF date string. Returns `None` if the input cannot be
    /// recognised even as a truncated form.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        parse_date_string(bytes)
    }
}

/// Parse the trailer's `/Info` dict and the catalog's `/Metadata` stream
/// into a [`DocumentMetadata`].
///
/// Always returns a value — missing or malformed inputs simply leave
/// fields as `None` / `Default`.
pub fn parse_document_metadata(resolver: &Resolver) -> DocumentMetadata {
    let mut meta = DocumentMetadata::default();

    if let Some(info_obj) = resolver.trailer().get(b"Info")
        && let Ok(info) = resolver.deref(info_obj)
        && let Some(dict) = info.as_dict()
    {
        fill_info_fields(&mut meta, dict);
    }

    meta.xmp_xml = parse_xmp_stream(resolver);

    meta
}

fn fill_info_fields(meta: &mut DocumentMetadata, dict: &PdfDict) {
    for (key, value) in dict.entries() {
        match key.as_slice() {
            b"Title" => meta.title = pdf_string_to_rust(value),
            b"Author" => meta.author = pdf_string_to_rust(value),
            b"Subject" => meta.subject = pdf_string_to_rust(value),
            b"Keywords" => meta.keywords = pdf_string_to_rust(value),
            b"Creator" => meta.creator = pdf_string_to_rust(value),
            b"Producer" => meta.producer = pdf_string_to_rust(value),
            b"CreationDate" => {
                if let Some(s) = value.as_str() {
                    meta.creation_date = PdfDate::parse(s);
                }
            }
            b"ModDate" => {
                if let Some(s) = value.as_str() {
                    meta.mod_date = PdfDate::parse(s);
                }
            }
            b"Trapped" => {
                meta.trapped = match value.as_name() {
                    Some(b"True") => Some(TrappedFlag::True),
                    Some(b"False") => Some(TrappedFlag::False),
                    Some(b"Unknown") => Some(TrappedFlag::Unknown),
                    _ => None,
                };
            }
            other => {
                if let Some(s) = pdf_string_to_rust(value) {
                    let key_str = String::from_utf8_lossy(other).into_owned();
                    meta.custom.insert(key_str, s);
                }
            }
        }
    }
}

/// Extract the catalog's `/Metadata` stream content as XMP XML text.
fn parse_xmp_stream(resolver: &Resolver) -> Option<String> {
    let catalog_dict = catalog_dict_for_metadata(resolver)?;
    let metadata_obj = catalog_dict.get(b"Metadata")?;
    let bytes = resolver.stream_data_from_obj(metadata_obj).ok()?;
    // XMP is XML; PDF 2.0 allows UTF-8/16/32 with BOM. Lossy-decode to keep
    // callers unburdened by encoding errors — they get the raw string and
    // can do strict parsing themselves.
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Resolve the catalog dict, with the same /Root-vs-find_catalog fallback
/// logic used elsewhere in the crate.
fn catalog_dict_for_metadata(resolver: &Resolver) -> Option<PdfDict> {
    let trailer_root = resolver.trailer().get_ref(b"Root");
    if let Some((num, gen_num)) = trailer_root
        && let Ok(obj) = resolver.resolve(num, gen_num)
        && let Some(dict) = obj.as_dict()
        && (dict.get_name(b"Type") == Some(b"Catalog")
            || dict.get(b"Pages").is_some()
            || dict.get(b"Metadata").is_some())
    {
        return Some(dict.clone());
    }
    crate::find_catalog(resolver).and_then(|obj| obj.as_dict().cloned())
}

// --- string decoding ---

/// Decode a PDF object that should be a textual string into a Rust `String`.
///
/// Handles:
///
/// - Direct PDF strings with UTF-16BE BOM (`FE FF`)
/// - Direct PDF strings with UTF-8 BOM (`EF BB BF`, PDF 2.0)
/// - Direct PDF strings without a BOM, decoded via PDFDocEncoding
/// - Names (rare in /Info but handled)
///
/// Returns `None` for non-string-like objects.
fn pdf_string_to_rust(obj: &PdfObj) -> Option<String> {
    pdf_string_to_rust_pub(obj)
}

/// Crate-internal alias for [`pdf_string_to_rust`], usable from sibling
/// modules that need the same Info-string decoding (destinations,
/// outline titles, etc.).
pub(crate) fn pdf_string_to_rust_pub(obj: &PdfObj) -> Option<String> {
    match obj {
        PdfObj::Str(bytes) => Some(decode_pdf_text_string(bytes)),
        PdfObj::Name(bytes) => Some(decode_pdf_text_string(bytes)),
        _ => None,
    }
}

/// Crate-internal alias for [`decode_pdf_text_string`].
pub(crate) fn decode_pdf_text_string_pub(bytes: &[u8]) -> String {
    decode_pdf_text_string(bytes)
}

fn decode_pdf_text_string(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        // UTF-16BE
        let chars: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&chars);
    }
    if bytes.len() >= 3 && bytes[0] == 0xEF && bytes[1] == 0xBB && bytes[2] == 0xBF {
        // UTF-8 BOM
        return String::from_utf8_lossy(&bytes[3..]).into_owned();
    }
    // PDFDocEncoding: ASCII range matches; high range mapped per Annex D.
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        out.push(pdfdoc_encoding_to_char(b));
    }
    out
}

/// Map a PDFDocEncoding byte (0–255) to its Unicode code point.
///
/// Per ISO 32000-2 Annex D.2. The 0x80..=0x9F range carries glyphs that in
/// most other 8-bit encodings are control characters; PDF assigns them
/// printable Unicode characters.
fn pdfdoc_encoding_to_char(b: u8) -> char {
    // Fast path: ASCII printable + common controls map identically.
    if b < 0x80 {
        return b as char;
    }
    // PDFDocEncoding 0x80..=0xFF table per ISO 32000-2 Annex D.2.
    const HIGH: [char; 128] = [
        // 0x80..=0x8F
        '\u{2022}', '\u{2020}', '\u{2021}', '\u{2026}', '\u{2014}', '\u{2013}', '\u{0192}',
        '\u{2044}', '\u{2039}', '\u{203A}', '\u{2212}', '\u{2030}', '\u{201E}', '\u{201C}',
        '\u{201D}', '\u{2018}', // 0x90..=0x9F
        '\u{2019}', '\u{201A}', '\u{2122}', '\u{FB01}', '\u{FB02}', '\u{0141}', '\u{0152}',
        '\u{0160}', '\u{0178}', '\u{017D}', '\u{0131}', '\u{0142}', '\u{0153}', '\u{0161}',
        '\u{017E}', '\u{FFFD}', // 0x9F undefined in PDFDocEncoding → REPLACEMENT
        // 0xA0..=0xAF
        '\u{20AC}', '\u{00A1}', '\u{00A2}', '\u{00A3}', '\u{00A4}', '\u{00A5}', '\u{00A6}',
        '\u{00A7}', '\u{00A8}', '\u{00A9}', '\u{00AA}', '\u{00AB}', '\u{00AC}', '\u{00AD}',
        '\u{00AE}', '\u{00AF}', // 0xB0..=0xBF
        '\u{00B0}', '\u{00B1}', '\u{00B2}', '\u{00B3}', '\u{00B4}', '\u{00B5}', '\u{00B6}',
        '\u{00B7}', '\u{00B8}', '\u{00B9}', '\u{00BA}', '\u{00BB}', '\u{00BC}', '\u{00BD}',
        '\u{00BE}', '\u{00BF}', // 0xC0..=0xCF
        '\u{00C0}', '\u{00C1}', '\u{00C2}', '\u{00C3}', '\u{00C4}', '\u{00C5}', '\u{00C6}',
        '\u{00C7}', '\u{00C8}', '\u{00C9}', '\u{00CA}', '\u{00CB}', '\u{00CC}', '\u{00CD}',
        '\u{00CE}', '\u{00CF}', // 0xD0..=0xDF
        '\u{00D0}', '\u{00D1}', '\u{00D2}', '\u{00D3}', '\u{00D4}', '\u{00D5}', '\u{00D6}',
        '\u{00D7}', '\u{00D8}', '\u{00D9}', '\u{00DA}', '\u{00DB}', '\u{00DC}', '\u{00DD}',
        '\u{00DE}', '\u{00DF}', // 0xE0..=0xEF
        '\u{00E0}', '\u{00E1}', '\u{00E2}', '\u{00E3}', '\u{00E4}', '\u{00E5}', '\u{00E6}',
        '\u{00E7}', '\u{00E8}', '\u{00E9}', '\u{00EA}', '\u{00EB}', '\u{00EC}', '\u{00ED}',
        '\u{00EE}', '\u{00EF}', // 0xF0..=0xFF
        '\u{00F0}', '\u{00F1}', '\u{00F2}', '\u{00F3}', '\u{00F4}', '\u{00F5}', '\u{00F6}',
        '\u{00F7}', '\u{00F8}', '\u{00F9}', '\u{00FA}', '\u{00FB}', '\u{00FC}', '\u{00FD}',
        '\u{00FE}', '\u{00FF}',
    ];
    HIGH[(b - 0x80) as usize]
}

// --- date parsing ---

/// Parse a PDF date string (`D:YYYYMMDDHHmmSSOHH'mm'`).
///
/// Tolerates truncated forms; rejects strings whose year cannot be parsed.
fn parse_date_string(input: &[u8]) -> Option<PdfDate> {
    // Strip optional "D:" prefix; PDF dates may omit it (some authoring
    // tools do, even though the spec requires it).
    let bytes = input.strip_prefix(b"D:").unwrap_or(input);

    if bytes.len() < 4 {
        return None;
    }

    let year_str = std::str::from_utf8(&bytes[0..4]).ok()?;
    let year: i32 = year_str.parse().ok()?;

    let read_2 = |off: usize, max: u8| -> Option<u8> {
        if bytes.len() < off + 2 {
            return None;
        }
        let s = std::str::from_utf8(&bytes[off..off + 2]).ok()?;
        let v: u8 = s.parse().ok()?;
        if v > max { None } else { Some(v) }
    };

    let month = read_2(4, 12).unwrap_or(1).max(1);
    let day = read_2(6, 31).unwrap_or(1).max(1);
    let hour = read_2(8, 23).unwrap_or(0);
    let minute = read_2(10, 59).unwrap_or(0);
    let second = read_2(12, 60).unwrap_or(0); // 60 to admit leap second

    // Timezone is at offset 14 if present.
    let tz_offset_minutes = if bytes.len() > 14 {
        match bytes[14] {
            b'Z' => None,
            sign @ (b'+' | b'-') => parse_tz(&bytes[15..], sign == b'-'),
            _ => None,
        }
    } else {
        None
    };

    Some(PdfDate {
        year,
        month,
        day,
        hour,
        minute,
        second,
        tz_offset_minutes,
    })
}

fn parse_tz(rest: &[u8], negative: bool) -> Option<i16> {
    if rest.len() < 2 {
        return None;
    }
    let h_str = std::str::from_utf8(&rest[0..2]).ok()?;
    let hours: i16 = h_str.parse().ok()?;
    // After hours, the spec calls for an apostrophe then minutes then a
    // trailing apostrophe. Real-world files vary: some skip the trailing
    // apostrophe, some skip both, some use a literal `'` byte (0x27).
    let mins = if rest.len() >= 5 && rest[2] == b'\'' {
        let m_str = std::str::from_utf8(&rest[3..5]).ok()?;
        m_str.parse::<i16>().ok().unwrap_or(0)
    } else if rest.len() >= 4 {
        // No apostrophe; some authors emit HHMM directly.
        let m_str = std::str::from_utf8(&rest[2..4]).ok()?;
        m_str.parse::<i16>().ok().unwrap_or(0)
    } else {
        0
    };
    let total = hours * 60 + mins;
    Some(if negative { -total } else { total })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_full_form() {
        let d = PdfDate::parse(b"D:20261231235959-05'00'").unwrap();
        assert_eq!(d.year, 2026);
        assert_eq!(d.month, 12);
        assert_eq!(d.day, 31);
        assert_eq!(d.hour, 23);
        assert_eq!(d.minute, 59);
        assert_eq!(d.second, 59);
        assert_eq!(d.tz_offset_minutes, Some(-300));
    }

    #[test]
    fn date_utc() {
        let d = PdfDate::parse(b"D:20260101120000Z").unwrap();
        assert_eq!(d.tz_offset_minutes, None);
        assert_eq!(d.hour, 12);
    }

    #[test]
    fn date_truncated_year_only() {
        let d = PdfDate::parse(b"D:2026").unwrap();
        assert_eq!(d.year, 2026);
        assert_eq!(d.month, 1);
        assert_eq!(d.day, 1);
        assert_eq!(d.hour, 0);
    }

    #[test]
    fn date_truncated_year_month() {
        let d = PdfDate::parse(b"D:202607").unwrap();
        assert_eq!(d.year, 2026);
        assert_eq!(d.month, 7);
        assert_eq!(d.day, 1);
    }

    #[test]
    fn date_no_d_prefix() {
        let d = PdfDate::parse(b"20260101000000").unwrap();
        assert_eq!(d.year, 2026);
    }

    #[test]
    fn date_positive_tz_no_trailing_apostrophe() {
        let d = PdfDate::parse(b"D:20260101000000+0530").unwrap();
        assert_eq!(d.tz_offset_minutes, Some(330));
    }

    #[test]
    fn date_invalid_year() {
        assert!(PdfDate::parse(b"D:abcd").is_none());
    }

    #[test]
    fn date_too_short() {
        assert!(PdfDate::parse(b"D:202").is_none());
    }

    #[test]
    fn date_invalid_month_clamps_to_jan() {
        // Month 13 is invalid; we default to 1 rather than failing.
        let d = PdfDate::parse(b"D:20261301").unwrap();
        assert_eq!(d.month, 1);
    }

    #[test]
    fn decode_utf16be_bom() {
        let bytes = [0xFE, 0xFF, 0x00, b'H', 0x00, b'i'];
        assert_eq!(decode_pdf_text_string(&bytes), "Hi");
    }

    #[test]
    fn decode_utf8_bom() {
        let bytes = [0xEF, 0xBB, 0xBF, b'H', b'i'];
        assert_eq!(decode_pdf_text_string(&bytes), "Hi");
    }

    #[test]
    fn decode_pdfdocencoding_ascii() {
        assert_eq!(decode_pdf_text_string(b"Hello"), "Hello");
    }

    #[test]
    fn decode_pdfdocencoding_high() {
        // 0x80 maps to U+2022 (BULLET).
        let s = decode_pdf_text_string(&[0x80]);
        assert_eq!(s, "\u{2022}");
    }

    #[test]
    fn decode_pdfdocencoding_a4_euro() {
        // 0xA0 maps to U+20AC (EURO SIGN).
        assert_eq!(decode_pdf_text_string(&[0xA0]), "\u{20AC}");
    }

    #[test]
    fn pdf_string_to_rust_handles_str_and_name() {
        let s = pdf_string_to_rust(&PdfObj::Str(b"Hi".to_vec())).unwrap();
        assert_eq!(s, "Hi");
        let n = pdf_string_to_rust(&PdfObj::Name(b"Foo".to_vec())).unwrap();
        assert_eq!(n, "Foo");
        assert!(pdf_string_to_rust(&PdfObj::Int(5)).is_none());
    }

    #[test]
    fn trapped_flag_parsing() {
        // Standalone test: build a small Info dict and run fill_info_fields.
        let mut dict = PdfDict::new();
        dict.insert(b"Trapped".to_vec(), PdfObj::Name(b"True".to_vec()));
        let mut meta = DocumentMetadata::default();
        fill_info_fields(&mut meta, &dict);
        assert_eq!(meta.trapped, Some(TrappedFlag::True));

        let mut dict = PdfDict::new();
        dict.insert(b"Trapped".to_vec(), PdfObj::Name(b"Unknown".to_vec()));
        let mut meta = DocumentMetadata::default();
        fill_info_fields(&mut meta, &dict);
        assert_eq!(meta.trapped, Some(TrappedFlag::Unknown));
    }

    #[test]
    fn custom_keys_collected() {
        let mut dict = PdfDict::new();
        dict.insert(b"MyCustom".to_vec(), PdfObj::Str(b"hello world".to_vec()));
        let mut meta = DocumentMetadata::default();
        fill_info_fields(&mut meta, &dict);
        assert_eq!(
            meta.custom.get("MyCustom").map(String::as_str),
            Some("hello world")
        );
    }

    #[test]
    fn standard_info_fields() {
        let mut dict = PdfDict::new();
        dict.insert(b"Title".to_vec(), PdfObj::Str(b"My Doc".to_vec()));
        dict.insert(b"Author".to_vec(), PdfObj::Str(b"Scott".to_vec()));
        dict.insert(b"Producer".to_vec(), PdfObj::Str(b"stet".to_vec()));
        dict.insert(
            b"CreationDate".to_vec(),
            PdfObj::Str(b"D:20260101000000Z".to_vec()),
        );
        let mut meta = DocumentMetadata::default();
        fill_info_fields(&mut meta, &dict);
        assert_eq!(meta.title.as_deref(), Some("My Doc"));
        assert_eq!(meta.author.as_deref(), Some("Scott"));
        assert_eq!(meta.producer.as_deref(), Some("stet"));
        assert_eq!(meta.creation_date.unwrap().year, 2026);
    }
}
