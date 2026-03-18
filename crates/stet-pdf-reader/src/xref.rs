// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF cross-reference table and trailer parsing.

use crate::error::PdfError;
use crate::filters;
use crate::lexer::{Lexer, Token, parse_dict_body};
use crate::objects::PdfDict;

/// Location of an object in the PDF file.
#[derive(Debug, Clone, Copy)]
pub enum XrefEntry {
    /// Object at byte offset, with generation number.
    InFile { offset: usize, generation: u16 },
    /// Object compressed inside an object stream.
    InStream {
        stream_obj_num: u32,
        index_within: u16,
    },
    /// Free entry.
    Free,
}

/// Parsed cross-reference data + trailer.
#[derive(Clone)]
pub struct XrefTable {
    /// Map from object number to entry. Index = object number.
    entries: Vec<Option<XrefEntry>>,
    /// Trailer dictionary (from the most recent trailer/xref stream).
    pub trailer: PdfDict,
}

impl XrefTable {
    /// Look up an object's location by number.
    pub fn get(&self, obj_num: u32) -> Option<&XrefEntry> {
        self.entries.get(obj_num as usize).and_then(|e| e.as_ref())
    }

    /// Total number of entry slots (including None gaps).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Parse the complete xref structure from a PDF file.
pub fn parse_xref(data: &[u8]) -> Result<XrefTable, PdfError> {
    let startxref = find_startxref(data)?;

    // Follow the /Prev chain, collecting (entries, trailer) from oldest to newest
    let mut sections = Vec::new();
    let mut offset = startxref;
    let mut visited = std::collections::HashSet::new();

    let mut xref_failed = false;

    loop {
        if !visited.insert(offset) {
            break; // Circular /Prev chain
        }

        match parse_xref_section(data, offset) {
            Ok((entries, trailer)) => {
                let prev = trailer.get_int(b"Prev").map(|v| v as usize);
                sections.push((entries, trailer));
                match prev {
                    Some(p) => offset = p,
                    None => break,
                }
            }
            Err(_) => {
                xref_failed = true;
                break;
            }
        }
    }

    // If xref parsing failed completely, fall back to scanning the file for objects
    if xref_failed && sections.is_empty() {
        return rebuild_xref_from_scan(data);
    }

    // Build the combined table: oldest entries first, newest override
    sections.reverse();
    let mut combined_entries: Vec<Option<XrefEntry>> = Vec::new();
    let mut final_trailer = PdfDict::new();

    for (entries, trailer) in sections {
        for (num, entry) in entries {
            let idx = num as usize;
            if idx >= combined_entries.len() {
                combined_entries.resize(idx + 1, None);
            }
            combined_entries[idx] = Some(entry);
        }
        final_trailer = trailer; // Most recent trailer wins
    }

    Ok(XrefTable {
        entries: combined_entries,
        trailer: final_trailer,
    })
}

/// Rebuild the xref table by scanning the entire file for `N G obj` patterns.
/// Used as a fallback when the xref table/stream is damaged or missing.
fn rebuild_xref_from_scan(data: &[u8]) -> Result<XrefTable, PdfError> {
    let mut entries: Vec<Option<XrefEntry>> = Vec::new();

    // Scan for "N G obj" patterns at the start of lines
    let mut pos = 0;
    while pos < data.len() {
        // Skip to something that looks like a digit at start-of-line or start-of-file
        if pos > 0 && data[pos - 1] != b'\n' && data[pos - 1] != b'\r' {
            // Advance to next line
            while pos < data.len() && data[pos] != b'\n' {
                pos += 1;
            }
            if pos < data.len() {
                pos += 1; // skip \n
            }
            continue;
        }

        // Try to parse: <obj_num> <gen_num> obj
        if pos < data.len() && data[pos].is_ascii_digit()
            && let Some((obj_num, generation, obj_offset)) = try_parse_obj_header(data, pos) {
                let idx = obj_num as usize;
                if idx < 100_000 {
                    // sanity limit
                    if idx >= entries.len() {
                        entries.resize(idx + 1, None);
                    }
                    entries[idx] = Some(XrefEntry::InFile {
                        offset: obj_offset,
                        generation,
                    });
                }
            }

        // Advance to next line
        while pos < data.len() && data[pos] != b'\n' {
            pos += 1;
        }
        if pos < data.len() {
            pos += 1;
        }
    }

    // Parse the trailer dict (search for "trailer" keyword)
    let trailer = find_trailer_dict(data).unwrap_or_default();

    // If no trailer has /Root, scan objects for /Type /Catalog
    let trailer = if trailer.get(b"Root").is_none() {
        let mut t = trailer;
        if let Some(root_num) = find_catalog_obj(data, &entries) {
            t.insert(
                b"Root".to_vec(),
                crate::objects::PdfObj::Ref(root_num, 0),
            );
        }
        t
    } else {
        trailer
    };

    Ok(XrefTable { entries, trailer })
}

/// Try to parse an object header "N G obj" at the given position.
/// Returns (obj_num, generation, offset) if successful.
fn try_parse_obj_header(data: &[u8], pos: usize) -> Option<(u32, u16, usize)> {
    let mut p = pos;

    // Parse object number
    let num_start = p;
    while p < data.len() && data[p].is_ascii_digit() {
        p += 1;
    }
    if p == num_start || p >= data.len() {
        return None;
    }
    let obj_num: u32 = std::str::from_utf8(&data[num_start..p]).ok()?.parse().ok()?;

    // Skip spaces
    while p < data.len() && data[p] == b' ' {
        p += 1;
    }

    // Parse generation number
    let gen_start = p;
    while p < data.len() && data[p].is_ascii_digit() {
        p += 1;
    }
    if p == gen_start || p >= data.len() {
        return None;
    }
    let generation: u16 = std::str::from_utf8(&data[gen_start..p]).ok()?.parse().ok()?;

    // Skip spaces
    while p < data.len() && data[p] == b' ' {
        p += 1;
    }

    // Check for "obj" keyword
    if p + 3 > data.len() || &data[p..p + 3] != b"obj" {
        return None;
    }
    // "obj" must be followed by whitespace or << (not part of a longer word)
    let after_obj = p + 3;
    if after_obj < data.len()
        && !is_whitespace(data[after_obj])
        && data[after_obj] != b'<'
    {
        return None;
    }

    Some((obj_num, generation, pos))
}

/// Search for a `trailer << ... >>` dict in the file.
fn find_trailer_dict(data: &[u8]) -> Option<PdfDict> {
    // Search backwards from end for "trailer"
    let needle = b"trailer";
    let mut pos = data.len().saturating_sub(needle.len());
    while pos > 0 {
        if &data[pos..pos + needle.len()] == needle {
            let dict_start = pos + needle.len();
            let mut lexer = Lexer::at(data, dict_start);
            if let Ok(Token::DictBegin) = lexer.next_token()
                && let Ok(dict) = parse_dict_body(&mut lexer) {
                    return Some(dict);
                }
        }
        pos -= 1;
    }
    None
}

/// Scan resolved objects for one with /Type /Catalog and return its object number.
fn find_catalog_obj(data: &[u8], entries: &[Option<XrefEntry>]) -> Option<u32> {
    for (num, entry) in entries.iter().enumerate() {
        if let Some(XrefEntry::InFile { offset, .. }) = entry {
            // Quick byte-level check before full parsing
            let search_end = (*offset + 512).min(data.len());
            let slice = &data[*offset..search_end];
            if let Some(idx) = slice.windows(8).position(|w| w == b"/Catalog") {
                // Verify /Type precedes it
                if idx > 5 && slice[..idx].windows(5).any(|w| w == b"/Type") {
                    return Some(num as u32);
                }
            }
        }
    }
    None
}

/// Parse one xref section (classic table or xref stream) at the given offset.
fn parse_xref_section(
    data: &[u8],
    offset: usize,
) -> Result<(Vec<(u32, XrefEntry)>, PdfDict), PdfError> {
    // Peek at the data to determine if this is a classic xref or xref stream.
    // Some PDF generators have startxref offsets that are slightly off,
    // so scan forward (skipping whitespace) and backward to find "xref".
    let mut pos = offset;
    while pos < data.len() && is_whitespace(data[pos]) {
        pos += 1;
    }

    if pos + 4 <= data.len() && &data[pos..pos + 4] == b"xref" {
        parse_classic_xref(data, pos)
    } else {
        // Scan backward up to 20 bytes for "xref" (handles off-by-N offsets)
        let scan_start = offset.saturating_sub(20);
        let found = (scan_start..offset).rev().find(|&off| {
            off + 4 <= data.len() && &data[off..off + 4] == b"xref"
        });
        if let Some(xref_pos) = found {
            parse_classic_xref(data, xref_pos)
        } else {
            // Xref stream: an indirect object with /Type /XRef
            parse_xref_stream(data, offset)
        }
    }
}

/// Parse a classic xref table starting at `offset` (pointing to the `xref` keyword).
fn parse_classic_xref(
    data: &[u8],
    offset: usize,
) -> Result<(Vec<(u32, XrefEntry)>, PdfDict), PdfError> {
    let mut pos = offset + 4; // skip "xref"

    // Skip whitespace after "xref"
    while pos < data.len() && is_whitespace(data[pos]) {
        pos += 1;
    }

    let mut entries = Vec::new();

    // Parse subsections until we hit "trailer"
    loop {
        // Check for "trailer" keyword
        if pos + 7 <= data.len() && &data[pos..pos + 7] == b"trailer" {
            pos += 7;
            break;
        }

        // Parse subsection header: <first_obj_num> <count>
        let (first_obj, new_pos) = parse_int_at(data, pos)?;
        pos = new_pos;
        while pos < data.len() && (data[pos] == b' ' || data[pos] == b'\t') {
            pos += 1;
        }
        let (count, new_pos) = parse_int_at(data, pos)?;
        pos = new_pos;

        // Skip to start of entries
        while pos < data.len() && is_whitespace(data[pos]) {
            pos += 1;
        }

        // Parse entries: spec says 20 bytes each, but tolerate 21 (extra space before EOL)
        for i in 0..count as u32 {
            if pos + 18 > data.len() {
                break;
            }

            // Parse: OOOOOOOOOO GGGGG f/n + EOL (variable length)
            let off_str = std::str::from_utf8(&data[pos..pos + 10])
                .map_err(|_| PdfError::MalformedXref(offset))?;
            let off: usize = off_str
                .trim()
                .parse()
                .map_err(|_| PdfError::MalformedXref(offset))?;

            let gen_str = std::str::from_utf8(&data[pos + 11..pos + 16])
                .map_err(|_| PdfError::MalformedXref(offset))?;
            let generation: u16 = gen_str
                .trim()
                .parse()
                .map_err(|_| PdfError::MalformedXref(offset))?;

            let type_byte = data[pos + 17];
            let obj_num = first_obj as u32 + i;

            let entry = match type_byte {
                b'n' => XrefEntry::InFile {
                    offset: off,
                    generation,
                },
                b'f' => XrefEntry::Free,
                _ => XrefEntry::Free,
            };
            entries.push((obj_num, entry));

            // Advance past entry: skip to next line
            pos += 18;
            while pos < data.len()
                && (data[pos] == b' ' || data[pos] == b'\r' || data[pos] == b'\n')
            {
                pos += 1;
            }
        }

        // Skip any remaining whitespace
        while pos < data.len() && is_whitespace(data[pos]) {
            pos += 1;
        }
    }

    // Parse trailer dict
    let mut lexer = Lexer::at(data, pos);
    lexer.next_token()?; // skip DictBegin (<<)
    // Back up — we need to check if there's a << or if we're already at dict content
    lexer.set_pos(pos);
    let tok = lexer.next_token()?;
    let trailer = match tok {
        Token::DictBegin => parse_dict_body(&mut lexer)?,
        _ => return Err(PdfError::MalformedTrailer),
    };

    Ok((entries, trailer))
}

/// Parse an xref stream at the given offset.
fn parse_xref_stream(
    data: &[u8],
    offset: usize,
) -> Result<(Vec<(u32, XrefEntry)>, PdfDict), PdfError> {
    // Parse the indirect object header: N G obj
    let mut lexer = Lexer::at(data, offset);

    // Object number
    let _obj_num = match lexer.next_token()? {
        Token::Int(n) => n,
        t => {
            return Err(PdfError::UnexpectedToken {
                expected: "object number".into(),
                got: format!("{t:?}"),
            });
        }
    };

    // Generation number
    match lexer.next_token()? {
        Token::Int(_) => {}
        t => {
            return Err(PdfError::UnexpectedToken {
                expected: "generation number".into(),
                got: format!("{t:?}"),
            });
        }
    }

    // "obj" keyword
    match lexer.next_token()? {
        Token::Keyword(ref kw) if kw == b"obj" => {}
        t => {
            return Err(PdfError::UnexpectedToken {
                expected: "obj".into(),
                got: format!("{t:?}"),
            });
        }
    }

    // Parse the stream dict
    match lexer.next_token()? {
        Token::DictBegin => {}
        t => {
            return Err(PdfError::UnexpectedToken {
                expected: "<<".into(),
                got: format!("{t:?}"),
            });
        }
    }
    let dict = parse_dict_body(&mut lexer)?;

    // Find stream data
    let tok = lexer.next_token()?;
    if !matches!(tok, Token::Keyword(ref kw) if kw == b"stream") {
        return Err(PdfError::UnexpectedToken {
            expected: "stream".into(),
            got: format!("{tok:?}"),
        });
    }

    // Stream data starts after "stream" + EOL
    let mut data_start = lexer.pos();
    if data_start < data.len() && data[data_start] == b'\r' {
        data_start += 1;
    }
    if data_start < data.len() && data[data_start] == b'\n' {
        data_start += 1;
    }

    let length = dict
        .get_int(b"Length")
        .ok_or(PdfError::StreamMissingLength)? as usize;
    let raw_data = &data[data_start..std::cmp::min(data_start + length, data.len())];

    // Decompress the stream
    let (filter_list, parms) = filters::parse_filters(&dict)?;
    let stream_data = if filter_list.is_empty() {
        raw_data.to_vec()
    } else {
        filters::decode_stream(raw_data, &filter_list, &parms, None)?
    };

    // Parse xref stream entries
    let w = dict.get_array(b"W").ok_or(PdfError::MissingKey("W"))?;
    if w.len() != 3 {
        return Err(PdfError::Other(
            "xref stream /W must have 3 elements".into(),
        ));
    }
    let w1 = w[0].as_int().unwrap_or(0) as usize;
    let w2 = w[1].as_int().unwrap_or(0) as usize;
    let w3 = w[2].as_int().unwrap_or(0) as usize;
    let entry_size = w1 + w2 + w3;

    if entry_size == 0 {
        return Err(PdfError::Other("xref stream entry size is 0".into()));
    }

    // Parse /Index array (defaults to [0 Size])
    let size = dict.get_int(b"Size").ok_or(PdfError::MissingKey("Size"))? as u32;

    let index_pairs: Vec<(u32, u32)> = if let Some(index_arr) = dict.get_array(b"Index") {
        index_arr
            .chunks(2)
            .filter_map(|pair| {
                if pair.len() == 2 {
                    Some((pair[0].as_int()? as u32, pair[1].as_int()? as u32))
                } else {
                    None
                }
            })
            .collect()
    } else {
        vec![(0, size)]
    };

    let mut entries = Vec::new();
    let mut stream_pos = 0;

    for (first_obj, count) in &index_pairs {
        for i in 0..*count {
            if stream_pos + entry_size > stream_data.len() {
                break;
            }

            let field1 = read_field(&stream_data[stream_pos..], w1);
            let field2 = read_field(&stream_data[stream_pos + w1..], w2);
            let field3 = read_field(&stream_data[stream_pos + w1 + w2..], w3);
            stream_pos += entry_size;

            // Default type is 1 when w1 == 0
            let entry_type = if w1 == 0 { 1 } else { field1 };
            let obj_num = first_obj + i;

            let entry = match entry_type {
                0 => XrefEntry::Free,
                1 => XrefEntry::InFile {
                    offset: field2 as usize,
                    generation: field3 as u16,
                },
                2 => XrefEntry::InStream {
                    stream_obj_num: field2 as u32,
                    index_within: field3 as u16,
                },
                _ => XrefEntry::Free, // Unknown type, treat as free
            };

            entries.push((obj_num, entry));
        }
    }

    // The xref stream dict IS the trailer
    Ok((entries, dict))
}

/// Read a big-endian unsigned integer field of `width` bytes.
fn read_field(data: &[u8], width: usize) -> u64 {
    let mut val: u64 = 0;
    for i in 0..width {
        if i < data.len() {
            val = (val << 8) | data[i] as u64;
        }
    }
    val
}

/// Find the `startxref` offset near the end of the file.
///
/// Some PDFs have trailing garbage after `%%EOF` (e.g. embedded attachments or
/// corrupted downloads), so we first locate the last `%%EOF` and search backwards
/// from there. Falls back to searching the last 1024 bytes if no `%%EOF` is found.
fn find_startxref(data: &[u8]) -> Result<usize, PdfError> {
    let needle = b"startxref";

    // Strategy 1: Find the last %%EOF, then search backwards from it for startxref.
    // This handles PDFs with trailing garbage after the final %%EOF.
    let eof_marker = b"%%EOF";
    let mut eof_pos = None;
    for i in (0..data.len().saturating_sub(eof_marker.len())).rev() {
        if &data[i..i + eof_marker.len()] == eof_marker {
            eof_pos = Some(i);
            break;
        }
    }

    if let Some(eof) = eof_pos {
        // Search the 1024 bytes before %%EOF for the last "startxref"
        let search_start = eof.saturating_sub(1024);
        let region = &data[search_start..eof];
        let mut found = None;
        for i in 0..region.len().saturating_sub(needle.len()) {
            if &region[i..i + needle.len()] == needle {
                found = Some(search_start + i);
            }
        }
        if let Some(pos) = found {
            let mut p = pos + needle.len();
            while p < data.len() && is_whitespace(data[p]) {
                p += 1;
            }
            let (offset, _) = parse_int_at(data, p)?;
            return Ok(offset as usize);
        }
    }

    // Strategy 2: Fall back to searching the last 1024 bytes (no %%EOF found,
    // or startxref wasn't near the %%EOF).
    let search_start = data.len().saturating_sub(1024);
    let tail = &data[search_start..];
    let mut found = None;
    for i in 0..tail.len().saturating_sub(needle.len()) {
        if &tail[i..i + needle.len()] == needle {
            found = Some(search_start + i);
        }
    }

    let pos = found.ok_or(PdfError::NoStartXref)?;
    let mut p = pos + needle.len();
    while p < data.len() && is_whitespace(data[p]) {
        p += 1;
    }
    let (offset, _) = parse_int_at(data, p)?;
    Ok(offset as usize)
}

/// Parse an integer starting at `pos`, return (value, new_pos).
fn parse_int_at(data: &[u8], pos: usize) -> Result<(i64, usize), PdfError> {
    let mut p = pos;
    // Skip leading whitespace
    while p < data.len() && is_whitespace(data[p]) {
        p += 1;
    }
    let start = p;
    if p < data.len() && (data[p] == b'+' || data[p] == b'-') {
        p += 1;
    }
    while p < data.len() && data[p].is_ascii_digit() {
        p += 1;
    }
    if p == start {
        return Err(PdfError::Other(format!("expected integer at offset {pos}")));
    }
    let s = std::str::from_utf8(&data[start..p])
        .map_err(|_| PdfError::Other(format!("invalid integer at offset {pos}")))?;
    let n: i64 = s
        .parse()
        .map_err(|_| PdfError::Other(format!("invalid integer '{s}' at offset {pos}")))?;
    Ok((n, p))
}

fn is_whitespace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n' | 0x0C | 0x00)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_startxref_simple() {
        let data = b"%PDF-1.4\nstartxref\n1234\n%%EOF\n";
        let offset = find_startxref(data).unwrap();
        assert_eq!(offset, 1234);
    }

    #[test]
    fn find_startxref_trailing_garbage() {
        // Simulate a PDF with garbage data appended after %%EOF
        let mut data = b"%PDF-1.4\nstartxref\n5678\n%%EOF\n".to_vec();
        // Append 2000 bytes of garbage (more than the 1024-byte tail search)
        data.extend_from_slice(&[0xFFu8; 2000]);
        let offset = find_startxref(&data).unwrap();
        assert_eq!(offset, 5678);
    }

    #[test]
    fn parse_int_at_basic() {
        let (val, pos) = parse_int_at(b"  42 rest", 0).unwrap();
        assert_eq!(val, 42);
        assert_eq!(pos, 4);
    }

    #[test]
    fn read_field_sizes() {
        assert_eq!(read_field(&[0x01], 1), 1);
        assert_eq!(read_field(&[0x01, 0x00], 2), 256);
        assert_eq!(read_field(&[0x00, 0x01, 0x00], 3), 256);
    }
}
