// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Extraction-grade parser for PDF `/ToUnicode` CMaps.
//!
//! A ToUnicode CMap maps a font's character codes to the text they
//! represent. [`ToUnicodeMap`] keeps that text whole, which is what text
//! extraction needs:
//!
//! - destinations are decoded as full UTF-16BE, so a surrogate pair gives
//!   one supplementary-plane character;
//! - a multi-character destination stays multi-character — `<00660069>` is
//!   the two characters `fi`, not the ligature U+FB01;
//! - codes are `u32`, so three- and four-byte codes are kept distinct.
//!
//! The PDF reader keeps a separate, narrower parser for glyph selection
//! while rendering, where one code point per code is what it needs.
//!
//! Handled: `beginbfchar` / `endbfchar` and `beginbfrange` / `endbfrange`
//! with string or array destinations, hex and literal strings, comments,
//! and entries laid out any way across lines. A `bfchar` whose destination
//! is a name (not permitted in a ToUnicode CMap, but written by some
//! producers) is resolved through the Adobe Glyph List. `usecmap` is
//! ignored. Codes are looked up by value, so `<41>` and `<0041>` are the
//! same code; a well-formed CMap's codespace never contains both.
//!
//! Later definitions win over earlier ones for the same code, as in
//! PostScript CMap semantics.

use std::borrow::Cow;
use std::collections::HashMap;

use crate::agl::glyph_name_to_text;

/// Ranges with at most this many codes are expanded into individual
/// entries at parse time; larger ones are resolved on lookup. PDF requires
/// a `bfrange`'s two codes to differ only in their last byte, so every
/// conforming range fits; the lazy path exists for the non-conforming
/// `<0000> <FFFF> <0000>` shape, which would otherwise cost 65 536 entries,
/// and for adversarial ranges spanning the whole `u32` space.
const EXPAND_LIMIT: u32 = 256;

/// A parsed `/ToUnicode` CMap: character code → Unicode text.
#[derive(Debug, Clone, Default)]
pub struct ToUnicodeMap {
    /// Destination text of every expanded entry, concatenated. One
    /// allocation for the whole map rather than one per code.
    text: String,
    /// Code → byte range in `text`.
    entries: HashMap<u32, (u32, u32)>,
    /// `bfrange`s too large to expand, in definition order.
    wide_ranges: Vec<WideRange>,
}

/// A `bfrange` with a string destination, resolved on lookup.
#[derive(Debug, Clone)]
struct WideRange {
    lo: u32,
    hi: u32,
    /// UTF-16BE destination for `lo`; code `lo + n` maps to this string
    /// incremented by `n`.
    dst: Vec<u8>,
}

impl ToUnicodeMap {
    /// Parse a decoded ToUnicode CMap stream.
    ///
    /// Never fails: malformed entries are skipped, and a stream with no
    /// usable entries gives an empty map.
    pub fn parse(data: &[u8]) -> Self {
        let tokens = tokenize(data);
        let mut map = ToUnicodeMap::default();
        let mut pos = 0;
        while pos < tokens.len() {
            let tok = &tokens[pos];
            pos += 1;
            match tok {
                Token::Keyword(k) if k == b"beginbfchar" => {
                    pos = map.parse_bfchar(&tokens, pos);
                }
                Token::Keyword(k) if k == b"beginbfrange" => {
                    pos = map.parse_bfrange(&tokens, pos);
                }
                _ => {}
            }
        }
        map
    }

    /// The text for `code`, or `None` when the CMap does not map it.
    ///
    /// `Some("")` means the CMap maps the code to an empty string: the
    /// producer stated that the glyph carries no text.
    pub fn get(&self, code: u32) -> Option<Cow<'_, str>> {
        if let Some(&(start, end)) = self.entries.get(&code) {
            return Some(Cow::Borrowed(&self.text[start as usize..end as usize]));
        }
        let range = self
            .wide_ranges
            .iter()
            .rev()
            .find(|r| (r.lo..=r.hi).contains(&code))?;
        let mut text = String::new();
        decode_utf16be(&add_to_bytes(&range.dst, code - range.lo), &mut text);
        Some(Cow::Owned(text))
    }

    /// Whether the CMap maps no codes at all.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.wide_ranges.is_empty()
    }

    /// Entries of a `beginbfchar` section: `srcCode dstString` pairs.
    /// Returns the token index after the section.
    fn parse_bfchar(&mut self, tokens: &[Token], mut pos: usize) -> usize {
        while pos < tokens.len() {
            match &tokens[pos] {
                // `endbfchar`, or a missing one: leave any other operator
                // for the caller to see.
                Token::Keyword(k) if is_operator(k) => {
                    return if k == b"endbfchar" { pos + 1 } else { pos };
                }
                Token::Str(src) => {
                    let Some(dst) = tokens.get(pos + 1) else {
                        return pos + 1;
                    };
                    match dst {
                        Token::Str(dst) => {
                            if let Some(code) = code_value(src) {
                                let mut text = String::new();
                                decode_utf16be(dst, &mut text);
                                self.insert(code, &text);
                            }
                            pos += 2;
                        }
                        Token::Name(name) => {
                            if let Some(code) = code_value(src)
                                && let Ok(name) = std::str::from_utf8(name)
                                && let Some(text) = glyph_name_to_text(name)
                            {
                                self.insert(code, &text);
                            }
                            pos += 2;
                        }
                        // Not a destination: drop the source code and
                        // resynchronise on whatever follows it.
                        _ => pos += 1,
                    }
                }
                _ => pos += 1,
            }
        }
        pos
    }

    /// Entries of a `beginbfrange` section: `srcLo srcHi dst` triples,
    /// where `dst` is a string or an array of strings.
    /// Returns the token index after the section.
    fn parse_bfrange(&mut self, tokens: &[Token], mut pos: usize) -> usize {
        while pos < tokens.len() {
            let (Token::Str(lo), Some(Token::Str(hi))) = (&tokens[pos], tokens.get(pos + 1)) else {
                match &tokens[pos] {
                    Token::Keyword(k) if is_operator(k) => {
                        return if k == b"endbfrange" { pos + 1 } else { pos };
                    }
                    _ => {
                        pos += 1;
                        continue;
                    }
                }
            };
            let range = match (code_value(lo), code_value(hi)) {
                (Some(lo), Some(hi)) if lo <= hi => Some((lo, hi)),
                _ => None,
            };
            pos += 2;
            match tokens.get(pos) {
                Some(Token::Str(dst)) => {
                    if let Some((lo, hi)) = range {
                        self.insert_string_range(lo, hi, dst);
                    }
                    pos += 1;
                }
                Some(Token::ArrayOpen) => {
                    pos += 1;
                    let mut code = range.map(|(lo, _)| lo);
                    while let Some(tok) = tokens.get(pos) {
                        match tok {
                            Token::ArrayClose => {
                                pos += 1;
                                break;
                            }
                            // An unterminated array: stop at the operator.
                            Token::Keyword(k) if is_operator(k) => break,
                            _ => {}
                        }
                        if let (Some(c), Some((_, hi))) = (code, range)
                            && c <= hi
                        {
                            let mut text = String::new();
                            match tok {
                                Token::Str(dst) => decode_utf16be(dst, &mut text),
                                Token::Name(name) => {
                                    if let Some(t) =
                                        std::str::from_utf8(name).ok().and_then(glyph_name_to_text)
                                    {
                                        text = t;
                                    }
                                }
                                _ => {}
                            }
                            if matches!(tok, Token::Str(_) | Token::Name(_)) {
                                self.insert(c, &text);
                            }
                            code = c.checked_add(1);
                        }
                        pos += 1;
                    }
                }
                _ => {}
            }
        }
        pos
    }

    /// Map `lo..=hi` to `dst` incremented once per code.
    fn insert_string_range(&mut self, lo: u32, hi: u32, dst: &[u8]) {
        if hi - lo < EXPAND_LIMIT {
            let mut text = String::new();
            for offset in 0..=hi - lo {
                text.clear();
                decode_utf16be(&add_to_bytes(dst, offset), &mut text);
                self.insert(lo + offset, &text);
            }
        } else {
            // This range overrides every earlier entry it covers; later
            // entries go into `entries` and are checked first on lookup.
            self.entries.retain(|code, _| !(lo..=hi).contains(code));
            self.wide_ranges.push(WideRange {
                lo,
                hi,
                dst: dst.to_vec(),
            });
        }
    }

    fn insert(&mut self, code: u32, text: &str) {
        let start = self.text.len();
        let end = start + text.len();
        // Offsets are stored as u32; a CMap with 4 GiB of destination text
        // is not a real one.
        let (Ok(start), Ok(end)) = (u32::try_from(start), u32::try_from(end)) else {
            return;
        };
        self.text.push_str(text);
        self.entries.insert(code, (start, end));
    }
}

/// A source code's value: its bytes read big-endian. Codes longer than
/// four bytes do not occur in PDF and are rejected.
fn code_value(bytes: &[u8]) -> Option<u32> {
    if bytes.is_empty() || bytes.len() > 4 {
        return None;
    }
    Some(bytes.iter().fold(0u32, |acc, &b| (acc << 8) | b as u32))
}

/// `bytes` read as a big-endian integer, plus `n`, at the same width.
/// PDF increments only the last byte and leaves overflow undefined;
/// carrying into the preceding bytes is the reading other viewers share,
/// and is what makes a range cross into a new UTF-16 code unit correctly.
fn add_to_bytes(bytes: &[u8], n: u32) -> Vec<u8> {
    let mut out = bytes.to_vec();
    let mut carry = n as u64;
    for b in out.iter_mut().rev() {
        if carry == 0 {
            break;
        }
        let sum = *b as u64 + (carry & 0xFF);
        *b = sum as u8;
        carry = (carry >> 8) + (sum >> 8);
    }
    out
}

/// Append `bytes`, read as UTF-16BE, to `out`.
///
/// A lone surrogate becomes U+FFFD. A one-byte string is read as the code
/// point of that byte — not UTF-16, but written by enough producers for
/// ASCII that reading it any other way loses text. An odd trailing byte on
/// a longer string is read as a code unit on its own.
fn decode_utf16be(bytes: &[u8], out: &mut String) {
    if let [b] = bytes {
        out.push(char::from(*b));
        return;
    }
    let units = bytes.chunks(2).map(|c| match c {
        [hi, lo] => u16::from_be_bytes([*hi, *lo]),
        [b] => *b as u16,
        _ => unreachable!("chunks(2) yields one or two bytes"),
    });
    out.extend(char::decode_utf16(units).map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER)));
}

/// A lexical token of a CMap stream. Dictionaries, procedures and numbers
/// are not needed to read the mappings, so they are keywords here.
#[derive(Debug, Clone, PartialEq)]
enum Token {
    /// A hex `<…>` or literal `(…)` string, decoded to bytes.
    Str(Vec<u8>),
    /// `/name`, without the slash, `#xx` escapes decoded.
    Name(Vec<u8>),
    ArrayOpen,
    ArrayClose,
    /// Any other run of regular characters, or a `<<` / `>>` / `{` / `}`.
    Keyword(Vec<u8>),
}

/// Whether a keyword is an operator such as `endbfchar`, as opposed to a
/// number or a stray delimiter. Only operators end a mapping section, so a
/// garbage number inside one is skipped rather than truncating it.
fn is_operator(keyword: &[u8]) -> bool {
    keyword.first().is_some_and(u8::is_ascii_alphabetic)
}

fn is_whitespace(b: u8) -> bool {
    matches!(b, b'\0' | b'\t' | b'\n' | b'\x0C' | b'\r' | b' ')
}

fn is_delimiter(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

fn hex_digit(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}

fn tokenize(data: &[u8]) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let b = data[i];
        if is_whitespace(b) {
            i += 1;
            continue;
        }
        match b {
            b'%' => {
                while i < data.len() && data[i] != b'\n' && data[i] != b'\r' {
                    i += 1;
                }
            }
            b'<' if data.get(i + 1) == Some(&b'<') => {
                tokens.push(Token::Keyword(b"<<".to_vec()));
                i += 2;
            }
            b'>' if data.get(i + 1) == Some(&b'>') => {
                tokens.push(Token::Keyword(b">>".to_vec()));
                i += 2;
            }
            b'<' => {
                i += 1;
                let mut bytes = Vec::new();
                let mut high: Option<u8> = None;
                while i < data.len() && data[i] != b'>' {
                    if let Some(d) = hex_digit(data[i]) {
                        match high.take() {
                            Some(h) => bytes.push(h << 4 | d),
                            None => high = Some(d),
                        }
                    }
                    i += 1;
                }
                // An odd digit count implies a trailing 0.
                if let Some(h) = high {
                    bytes.push(h << 4);
                }
                i += 1;
                tokens.push(Token::Str(bytes));
            }
            b'(' => {
                let (bytes, next) = literal_string(data, i + 1);
                tokens.push(Token::Str(bytes));
                i = next;
            }
            b'[' => {
                tokens.push(Token::ArrayOpen);
                i += 1;
            }
            b']' => {
                tokens.push(Token::ArrayClose);
                i += 1;
            }
            b'{' | b'}' | b'>' | b')' => {
                tokens.push(Token::Keyword(vec![b]));
                i += 1;
            }
            b'/' => {
                i += 1;
                let mut name = Vec::new();
                while i < data.len() && !is_whitespace(data[i]) && !is_delimiter(data[i]) {
                    if data[i] == b'#'
                        && let (Some(h), Some(l)) = (
                            data.get(i + 1).copied().and_then(hex_digit),
                            data.get(i + 2).copied().and_then(hex_digit),
                        )
                    {
                        name.push(h << 4 | l);
                        i += 3;
                    } else {
                        name.push(data[i]);
                        i += 1;
                    }
                }
                tokens.push(Token::Name(name));
            }
            _ => {
                let start = i;
                while i < data.len() && !is_whitespace(data[i]) && !is_delimiter(data[i]) {
                    i += 1;
                }
                tokens.push(Token::Keyword(data[start..i].to_vec()));
            }
        }
    }
    tokens
}

/// Decode a literal string whose opening `(` precedes `start`. Returns the
/// bytes and the index after the closing `)` (or the end of the data).
fn literal_string(data: &[u8], start: usize) -> (Vec<u8>, usize) {
    let mut out = Vec::new();
    let mut depth = 1usize;
    let mut i = start;
    while i < data.len() {
        let b = data[i];
        i += 1;
        match b {
            b'(' => {
                depth += 1;
                out.push(b);
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return (out, i);
                }
                out.push(b);
            }
            b'\\' => {
                let Some(&e) = data.get(i) else { break };
                i += 1;
                match e {
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'b' => out.push(0x08),
                    b'f' => out.push(0x0C),
                    b'0'..=b'7' => {
                        let mut v = (e - b'0') as u32;
                        for _ in 0..2 {
                            match data.get(i) {
                                Some(&d @ b'0'..=b'7') => {
                                    v = v * 8 + (d - b'0') as u32;
                                    i += 1;
                                }
                                _ => break,
                            }
                        }
                        out.push(v as u8);
                    }
                    // Line continuation: the backslash and end-of-line vanish.
                    b'\r' => {
                        if data.get(i) == Some(&b'\n') {
                            i += 1;
                        }
                    }
                    b'\n' => {}
                    // `\(`, `\)`, `\\`, and any unknown escape: the character.
                    other => out.push(other),
                }
            }
            _ => out.push(b),
        }
    }
    (out, i)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(map: &ToUnicodeMap, code: u32) -> Option<String> {
        map.get(code).map(Cow::into_owned)
    }

    /// Wrap entries in the boilerplate a real ToUnicode CMap carries, so
    /// the parser is tested against the header it must skip.
    fn cmap(body: &str) -> Vec<u8> {
        format!(
            "/CIDInit /ProcSet findresource begin\n\
             12 dict begin\n\
             begincmap\n\
             /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
             /CMapName /Adobe-Identity-UCS def\n\
             /CMapType 2 def\n\
             1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n\
             {body}\n\
             endcmap\n\
             CMapName currentdict /CMap defineresource pop\n\
             end\nend\n"
        )
        .into_bytes()
    }

    #[test]
    fn bfchar_single_characters() {
        let map = ToUnicodeMap::parse(&cmap(
            "2 beginbfchar\n<0003> <0020>\n<0024> <0041>\nendbfchar",
        ));
        assert_eq!(text(&map, 3).as_deref(), Some(" "));
        assert_eq!(text(&map, 0x24).as_deref(), Some("A"));
        assert_eq!(text(&map, 0x25), None);
        // The codespace range is not a mapping.
        assert_eq!(text(&map, 0), None);
        assert_eq!(text(&map, 0xFFFF), None);
    }

    #[test]
    fn ligature_destinations_keep_every_character() {
        let map = ToUnicodeMap::parse(&cmap(
            "3 beginbfchar\n<01> <00660069>\n<02> <006600660069>\n<03> <017F0074>\nendbfchar",
        ));
        assert_eq!(text(&map, 1).as_deref(), Some("fi"));
        assert_eq!(text(&map, 2).as_deref(), Some("ffi"));
        assert_eq!(text(&map, 3).as_deref(), Some("\u{17F}t"));
    }

    #[test]
    fn surrogate_pairs_decode_to_one_character() {
        let map = ToUnicodeMap::parse(&cmap(
            "1 beginbfchar\n<0005> <D835DC00>\nendbfchar\n\
             1 beginbfrange\n<0010> <0012> <D835DC00>\nendbfrange",
        ));
        assert_eq!(text(&map, 5).as_deref(), Some("\u{1D400}"));
        assert_eq!(text(&map, 0x10).as_deref(), Some("\u{1D400}"));
        assert_eq!(text(&map, 0x12).as_deref(), Some("\u{1D402}"));
    }

    #[test]
    fn lone_surrogate_is_replaced_not_dropped() {
        let map = ToUnicodeMap::parse(&cmap("1 beginbfchar\n<01> <D835>\nendbfchar"));
        assert_eq!(text(&map, 1).as_deref(), Some("\u{FFFD}"));
    }

    #[test]
    fn bfrange_string_destination_increments() {
        let map = ToUnicodeMap::parse(&cmap("1 beginbfrange\n<0020> <0022> <0041>\nendbfrange"));
        assert_eq!(text(&map, 0x20).as_deref(), Some("A"));
        assert_eq!(text(&map, 0x21).as_deref(), Some("B"));
        assert_eq!(text(&map, 0x22).as_deref(), Some("C"));
        assert_eq!(text(&map, 0x23), None);
    }

    #[test]
    fn bfrange_multi_character_destination_increments_last_character() {
        let map = ToUnicodeMap::parse(&cmap(
            "1 beginbfrange\n<0001> <0003> <00660066>\nendbfrange",
        ));
        assert_eq!(text(&map, 1).as_deref(), Some("ff"));
        assert_eq!(text(&map, 2).as_deref(), Some("fg"));
        assert_eq!(text(&map, 3).as_deref(), Some("fh"));
    }

    #[test]
    fn bfrange_array_of_multi_character_strings() {
        let map = ToUnicodeMap::parse(&cmap(
            "1 beginbfrange\n<0010> <0012> [<00660069> <0066006C> <D83DDE00>]\nendbfrange",
        ));
        assert_eq!(text(&map, 0x10).as_deref(), Some("fi"));
        assert_eq!(text(&map, 0x11).as_deref(), Some("fl"));
        assert_eq!(text(&map, 0x12).as_deref(), Some("\u{1F600}"));
    }

    #[test]
    fn bfrange_array_shorter_than_range_maps_only_what_it_has() {
        let map = ToUnicodeMap::parse(&cmap(
            "1 beginbfrange\n<01> <05> [<0041> <0042>]\nendbfrange",
        ));
        assert_eq!(text(&map, 2).as_deref(), Some("B"));
        assert_eq!(text(&map, 3), None);
    }

    #[test]
    fn increment_carries_across_bytes() {
        let map = ToUnicodeMap::parse(&cmap("1 beginbfrange\n<01> <03> <00FF>\nendbfrange"));
        assert_eq!(text(&map, 1).as_deref(), Some("\u{FF}"));
        assert_eq!(text(&map, 2).as_deref(), Some("\u{100}"));
        assert_eq!(text(&map, 3).as_deref(), Some("\u{101}"));
    }

    #[test]
    fn entries_need_not_follow_line_structure() {
        // Several entries on one line, and one entry split across lines.
        let map = ToUnicodeMap::parse(&cmap(
            "3 beginbfchar <01> <0041> <02> <0042>\n<03>\n<0043> endbfchar\n\
             1 beginbfrange <10>\n<11>\n[<0061>\n<0062>] endbfrange",
        ));
        assert_eq!(text(&map, 1).as_deref(), Some("A"));
        assert_eq!(text(&map, 2).as_deref(), Some("B"));
        assert_eq!(text(&map, 3).as_deref(), Some("C"));
        assert_eq!(text(&map, 0x10).as_deref(), Some("a"));
        assert_eq!(text(&map, 0x11).as_deref(), Some("b"));
    }

    #[test]
    fn codes_wider_than_sixteen_bits_stay_distinct() {
        let map = ToUnicodeMap::parse(&cmap(
            "2 beginbfchar\n<010041> <0041>\n<020041> <0042>\nendbfchar",
        ));
        assert_eq!(text(&map, 0x01_0041).as_deref(), Some("A"));
        assert_eq!(text(&map, 0x02_0041).as_deref(), Some("B"));
        assert_eq!(text(&map, 0x41), None);
    }

    #[test]
    fn literal_string_codes_and_whitespace_inside_hex() {
        let map = ToUnicodeMap::parse(&cmap(
            "2 beginbfchar\n(\\001) <0041>\n(B) <00 4 2>\nendbfchar",
        ));
        assert_eq!(text(&map, 1).as_deref(), Some("A"));
        assert_eq!(text(&map, 0x42).as_deref(), Some("B"));
    }

    #[test]
    fn one_byte_destination_is_its_code_point() {
        let map = ToUnicodeMap::parse(&cmap("1 beginbfchar\n<01> <41>\nendbfchar"));
        assert_eq!(text(&map, 1).as_deref(), Some("A"));
    }

    #[test]
    fn empty_destination_is_an_explicit_no_text() {
        let map = ToUnicodeMap::parse(&cmap("1 beginbfchar\n<01> <>\nendbfchar"));
        assert_eq!(text(&map, 1).as_deref(), Some(""));
    }

    #[test]
    fn name_destination_resolves_through_the_glyph_list() {
        let map = ToUnicodeMap::parse(&cmap("2 beginbfchar\n<01> /Aacute\n<02> /f_f_i\nendbfchar"));
        assert_eq!(text(&map, 1).as_deref(), Some("\u{C1}"));
        assert_eq!(text(&map, 2).as_deref(), Some("ffi"));
    }

    #[test]
    fn later_definitions_win() {
        let map = ToUnicodeMap::parse(&cmap(
            "1 beginbfchar\n<0001> <0041>\nendbfchar\n\
             1 beginbfrange\n<0000> <0002> <0061>\nendbfrange\n\
             1 beginbfchar\n<0002> <005A>\nendbfchar",
        ));
        assert_eq!(text(&map, 1).as_deref(), Some("b"));
        assert_eq!(text(&map, 2).as_deref(), Some("Z"));
    }

    #[test]
    fn wide_range_resolves_lazily_and_keeps_precedence() {
        let map = ToUnicodeMap::parse(&cmap(
            "1 beginbfchar\n<0041> <0058>\nendbfchar\n\
             1 beginbfrange\n<0000> <FFFF> <0000>\nendbfrange\n\
             1 beginbfchar\n<0042> <0059>\nendbfchar",
        ));
        // Identity range overrode the earlier bfchar …
        assert_eq!(text(&map, 0x41).as_deref(), Some("A"));
        // … and the later bfchar overrides the range.
        assert_eq!(text(&map, 0x42).as_deref(), Some("Y"));
        assert_eq!(text(&map, 0x4E2D).as_deref(), Some("\u{4E2D}"));
        // Nothing was expanded for it.
        assert_eq!(map.entries.len(), 1);
    }

    #[test]
    fn range_spanning_all_codes_does_not_expand() {
        let map = ToUnicodeMap::parse(&cmap(
            "1 beginbfrange\n<00000000> <FFFFFFFF> <0041>\nendbfrange",
        ));
        assert_eq!(text(&map, 0).as_deref(), Some("A"));
        assert!(map.entries.is_empty());
    }

    #[test]
    fn missing_end_keywords_do_not_swallow_later_sections() {
        let map = ToUnicodeMap::parse(&cmap(
            "1 beginbfchar\n<01> <0041>\n\
             1 beginbfrange\n<02> <03> <0061>\nendbfrange",
        ));
        assert_eq!(text(&map, 1).as_deref(), Some("A"));
        assert_eq!(text(&map, 3).as_deref(), Some("b"));
    }

    #[test]
    fn malformed_entries_are_skipped() {
        let map = ToUnicodeMap::parse(&cmap(
            "4 beginbfchar\n<> <0041>\n<0102030405> <0042>\n<01> 7\n<02> <0043>\nendbfchar\n\
             3 beginbfrange\n<05> <04> <0041>\n<06> <07> 12\n<08> <08> <0044>\nendbfrange",
        ));
        // An empty and a five-byte source code are dropped, and a number
        // where a destination belongs drops its entry, not the section.
        assert_eq!(text(&map, 1), None);
        assert_eq!(text(&map, 2).as_deref(), Some("C"));
        // An inverted range and a range with a number destination.
        assert_eq!(text(&map, 4), None);
        assert_eq!(text(&map, 6), None);
        assert_eq!(text(&map, 8).as_deref(), Some("D"));
    }

    #[test]
    fn empty_and_garbage_input_give_empty_maps() {
        assert!(ToUnicodeMap::parse(b"").is_empty());
        assert!(ToUnicodeMap::parse(b"beginbfchar").is_empty());
        assert!(ToUnicodeMap::parse(b"beginbfrange <01").is_empty());
        assert!(ToUnicodeMap::parse(b"beginbfchar (\\").is_empty());
        assert!(ToUnicodeMap::parse(b"<<<<>>>>]]][[[///((((").is_empty());
    }
}
