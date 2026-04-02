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
    /// Create an empty xref table (for tests).
    #[cfg(test)]
    pub(crate) fn empty() -> Self {
        Self {
            entries: Vec::new(),
            trailer: PdfDict::new(),
        }
    }

    /// Look up an object's location by number.
    pub fn get(&self, obj_num: u32) -> Option<&XrefEntry> {
        self.entries.get(obj_num as usize).and_then(|e| e.as_ref())
    }

    /// Iterate over all entries (including None gaps).
    pub fn entries(&self) -> &[Option<XrefEntry>] {
        &self.entries
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

    // If %PDF- header isn't at byte 0 (e.g., UTF-8 BOM or prepended garbage),
    // all internal offsets (startxref, /Prev) need adjustment. Compute this
    // before following the xref chain so /Prev pointers are corrected too.
    let header_offset = data
        .windows(5)
        .position(|w| w == b"%PDF-")
        .unwrap_or(0);

    // Detect concatenated PDFs (multiple %PDF- headers). When present, the
    // final section's offsets may be relative to a later header, not the first.
    let pdf_headers: Vec<usize> = find_all_pdf_headers(data);

    // Follow the /Prev chain, collecting (entries, trailer) from oldest to newest
    let mut sections = Vec::new();
    let mut offset = startxref + header_offset;
    let mut visited = std::collections::HashSet::new();

    let mut xref_failed = false;

    // If startxref + first header doesn't work, try with other headers
    // (concatenated PDFs have offsets relative to a later %PDF- header).
    if offset < data.len() && parse_xref_section(data, offset).is_err() && pdf_headers.len() > 1 {
        for &h in pdf_headers.iter().skip(1).rev() {
            let try_offset = startxref + h;
            if try_offset < data.len() {
                if let Ok((mut entries, trailer)) = parse_xref_section(data, try_offset) {
                    // Adjust entry offsets: they're relative to this header
                    shift_xref_entries(&mut entries, h);
                    visited.insert(try_offset);
                    let prev = trailer
                        .get_int(b"Prev")
                        .map(|v| v as usize + h);
                    sections.push((entries, trailer));
                    if let Some(p) = prev {
                        offset = p;
                    } else {
                        xref_failed = false;
                    }
                    break;
                }
            }
        }
    }

    loop {
        if !visited.insert(offset) {
            break; // Circular /Prev chain
        }

        match parse_xref_section(data, offset) {
            Ok((entries, trailer)) => {
                let prev = trailer
                    .get_int(b"Prev")
                    .map(|v| v as usize + header_offset);
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

    // If xref parsing failed completely, try discovering orphaned xref sections
    // (e.g., linearized PDFs where startxref points to byte 0 but the real xref
    // table is elsewhere in the file). Only fall back to full scan if that also
    // finds nothing.
    if xref_failed && sections.is_empty() {
        discover_orphaned_xref_sections(
            data,
            &mut sections,
            &mut visited,
            header_offset,
            &pdf_headers,
        );
        if sections.is_empty() {
            return rebuild_xref_from_scan(data);
        }
    }

    // Scan all trailer dicts and startxref values in the file for xref
    // sections not reached by the /Prev chain. This handles PDFs with broken
    // incremental updates where the chain dead-ends (missing trailer, circular
    // /Prev, or self-referencing /Prev) before reaching the original xref.
    discover_orphaned_xref_sections(
        data,
        &mut sections,
        &mut visited,
        header_offset,
        &pdf_headers,
    );

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
        // Merge trailer keys — later sections override earlier ones, but
        // empty trailers (e.g., from truncated xref sections in truncated
        // linearized PDFs) won't erase keys like /Root from earlier trailers
        for (key, val) in trailer.into_entries() {
            final_trailer.insert(key, val);
        }
    }

    // If the assembled trailer is missing /Root (e.g., corrupted xref where
    // the parser bailed out before reaching the trailer keyword), fall back to
    // a full object scan which can recover /Root from the catalog object.
    if final_trailer.get(b"Root").is_none() {
        return rebuild_xref_from_scan(data);
    }

    // If %PDF- header isn't at byte 0 (e.g., UTF-8 BOM or prepended garbage),
    // adjust all InFile xref offsets by the header position.
    if header_offset > 0 {
        for entry in combined_entries.iter_mut().flatten() {
            if let XrefEntry::InFile { offset, .. } = entry {
                *offset += header_offset;
            }
        }
    }

    // Supplement with a scan for objects not in any xref section.
    // Handles linearized PDFs with objects appended after %%EOF
    // or incremental updates without proper xref entries.
    supplement_xref_from_scan(data, &mut combined_entries);

    Ok(XrefTable {
        entries: combined_entries,
        trailer: final_trailer,
    })
}

/// Scan all trailer dictionaries and startxref values in the file for xref
/// sections not yet visited. Handles broken incremental updates where the
/// /Prev chain dead-ends before reaching the original xref.
fn discover_orphaned_xref_sections(
    data: &[u8],
    sections: &mut Vec<(Vec<(u32, XrefEntry)>, PdfDict)>,
    visited: &mut std::collections::HashSet<usize>,
    header_offset: usize,
    pdf_headers: &[usize],
) {
    let mut candidates = Vec::new();

    // Collect /Prev offsets from all trailers in the file
    let mut pos = 0;
    while pos + 7 < data.len() {
        if &data[pos..pos + 7] == b"trailer" {
            let end = (pos + 500).min(data.len());
            if let Some(prev_off) = find_int_after_key(&data[pos..end], b"/Prev") {
                // /Prev values are PDF-internal offsets, need BOM adjustment
                candidates.push(prev_off + header_offset);
            }
        }
        // Also collect startxref values (point to xref sections from older revisions)
        if pos + 9 < data.len() && &data[pos..pos + 9] == b"startxref" {
            let end = (pos + 50).min(data.len());
            if let Some(sx_off) = find_int_after_key(&data[pos..end], b"startxref") {
                if sx_off > 0 {
                    // startxref values are PDF-internal offsets, need BOM adjustment.
                    // For concatenated PDFs, also try each %PDF header as base.
                    candidates.push(sx_off + header_offset);
                    for &h in pdf_headers.iter().skip(1) {
                        let adjusted = sx_off + h;
                        if adjusted < data.len() {
                            candidates.push(adjusted);
                        }
                    }
                }
            }
        }
        // Also collect xref keyword positions directly — handles linearized PDFs
        // where the first-page xref section is never reached via /Prev chain
        if pos + 4 <= data.len() && &data[pos..pos + 4] == b"xref" {
            // Make sure this isn't "startxref"
            if pos == 0 || data[pos - 1] != b't' {
                // This is already an actual file position, no adjustment needed
                candidates.push(pos);
            }
        }
        pos += 1;
    }

    // Try each candidate xref offset we haven't visited yet
    for candidate in candidates {
        let mut offset = candidate;
        loop {
            if !visited.insert(offset) {
                break;
            }
            match parse_xref_section(data, offset) {
                Ok((mut entries, trailer)) => {
                    // If this xref section is under a secondary %PDF header,
                    // its offsets are relative to that header — shift them.
                    let section_header = owning_pdf_header(offset, pdf_headers);
                    if section_header > 0 {
                        shift_xref_entries(&mut entries, section_header);
                    }
                    let prev = trailer
                        .get_int(b"Prev")
                        .map(|v| v as usize + header_offset);
                    sections.push((entries, trailer));
                    match prev {
                        Some(p) => offset = p,
                        None => break,
                    }
                }
                Err(_) => break,
            }
        }
    }
}

/// Scan file for `N G obj` patterns and add entries for objects not already
/// in the xref table. This catches objects appended after %%EOF in
/// linearized PDFs or broken incremental updates.
fn supplement_xref_from_scan(data: &[u8], entries: &mut Vec<Option<XrefEntry>>) {
    let mut pos = 0;
    while pos < data.len() {
        // Advance to start-of-line
        if pos > 0 && data[pos - 1] != b'\n' && data[pos - 1] != b'\r' {
            while pos < data.len() && data[pos] != b'\n' && data[pos] != b'\r' {
                pos += 1;
            }
            if pos < data.len() {
                if data[pos] == b'\r' && pos + 1 < data.len() && data[pos + 1] == b'\n' {
                    pos += 2;
                } else {
                    pos += 1;
                }
            }
            continue;
        }

        if pos < data.len()
            && data[pos].is_ascii_digit()
            && let Some((obj_num, generation, obj_offset)) = try_parse_obj_header(data, pos)
        {
            let idx = obj_num as usize;
            if idx < 100_000 {
                // Add if not already in the table, or if the existing entry is
                // Free (malformed xref tables sometimes mark real objects as
                // free, e.g. when the subsection start number is off-by-one).
                let dominated_by_existing = entries.get(idx).is_some_and(|e| {
                    matches!(e, Some(XrefEntry::InFile { .. } | XrefEntry::InStream { .. }))
                });
                if !dominated_by_existing {
                    if idx >= entries.len() {
                        entries.resize(idx + 1, None);
                    }
                    entries[idx] = Some(XrefEntry::InFile {
                        offset: obj_offset,
                        generation,
                    });
                }
            }
        }

        while pos < data.len() && data[pos] != b'\n' && data[pos] != b'\r' {
            pos += 1;
        }
        if pos < data.len() {
            if data[pos] == b'\r' && pos + 1 < data.len() && data[pos + 1] == b'\n' {
                pos += 2;
            } else {
                pos += 1;
            }
        }
    }
}

/// Find an integer value after a keyword in a byte region.
/// Works for both `/Prev 12345` and `startxref\n12345`.
fn find_int_after_key(data: &[u8], key: &[u8]) -> Option<usize> {
    let s = std::str::from_utf8(data).ok()?;
    let key_str = std::str::from_utf8(key).ok()?;
    let idx = s.find(key_str)?;
    let after = &s[idx + key_str.len()..];
    let after = after.trim_start();
    let end = after
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(after.len());
    if end == 0 {
        return None;
    }
    after[..end].parse().ok()
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
            // Advance to next line (handle both \n and \r line endings)
            while pos < data.len() && data[pos] != b'\n' && data[pos] != b'\r' {
                pos += 1;
            }
            // Skip line ending character(s)
            if pos < data.len() {
                if data[pos] == b'\r' && pos + 1 < data.len() && data[pos + 1] == b'\n' {
                    pos += 2;
                } else {
                    pos += 1;
                }
            }
            continue;
        }

        // Try to parse: <obj_num> <gen_num> obj
        if pos < data.len()
            && data[pos].is_ascii_digit()
            && let Some((obj_num, generation, obj_offset)) = try_parse_obj_header(data, pos)
        {
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

        // Advance to next line (handle both \n and \r line endings)
        while pos < data.len() && data[pos] != b'\n' && data[pos] != b'\r' {
            pos += 1;
        }
        if pos < data.len() {
            if data[pos] == b'\r' && pos + 1 < data.len() && data[pos + 1] == b'\n' {
                pos += 2;
            } else {
                pos += 1;
            }
        }
    }

    // Try to parse any xref stream objects found during the scan.
    // These carry the trailer dict (with /Root) and may reference objects
    // inside object streams that aren't discoverable by byte-scanning alone.
    for entry in entries.iter().flatten() {
        if let XrefEntry::InFile { offset, .. } = entry {
            let search_end = (*offset + 512).min(data.len());
            let slice = &data[*offset..search_end];
            if slice.windows(10).any(|w| w == b"/Type/XRef")
                || slice.windows(11).any(|w| w == b"/Type /XRef")
            {
                if let Ok((xref_entries, xref_trailer)) = parse_xref_stream(data, *offset) {
                    // Merge entries from xref stream (includes ObjStm refs)
                    let mut merged = entries;
                    for (num, xentry) in xref_entries {
                        let idx = num as usize;
                        if idx < 100_000 {
                            if idx >= merged.len() {
                                merged.resize(idx + 1, None);
                            }
                            if merged[idx].is_none() {
                                merged[idx] = Some(xentry);
                            }
                        }
                    }
                    return Ok(XrefTable {
                        entries: merged,
                        trailer: xref_trailer,
                    });
                }
            }
        }
    }

    // Parse the trailer dict (search for "trailer" keyword)
    let trailer = find_trailer_dict(data).unwrap_or_default();

    // If no trailer has /Root, scan objects for /Type /Catalog
    let trailer = if trailer.get(b"Root").is_none() {
        let mut t = trailer;
        if let Some(root_num) = find_catalog_obj(data, &entries) {
            t.insert(b"Root".to_vec(), crate::objects::PdfObj::Ref(root_num, 0));
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
    let obj_num: u32 = std::str::from_utf8(&data[num_start..p])
        .ok()?
        .parse()
        .ok()?;

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
    let generation: u16 = std::str::from_utf8(&data[gen_start..p])
        .ok()?
        .parse()
        .ok()?;

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
    if after_obj < data.len() && !is_whitespace(data[after_obj]) && data[after_obj] != b'<' {
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
                && let Ok(dict) = parse_dict_body(&mut lexer)
            {
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
            // Also check xref stream objects which carry /Root in their dict
            // (the catalog itself may be inside an object stream, unreachable
            // by byte-scanning, but the xref stream dict has `/Root N 0 R`).
            if slice.windows(5).any(|w| w == b"/Root")
                && slice.windows(10).any(|w| w == b"/Type/XRef")
            {
                // Extract the /Root reference: parse the dict to find it
                let obj_start = slice
                    .windows(3)
                    .position(|w| w == b"obj")
                    .map(|p| p + 3)
                    .unwrap_or(0);
                let mut lexer = Lexer::at(slice, obj_start);
                if let Ok(Token::DictBegin) = lexer.next_token()
                    && let Ok(dict) = parse_dict_body(&mut lexer)
                {
                    if let Some((root_num, _)) = dict.get_ref(b"Root") {
                        return Some(root_num);
                    }
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
        let found = (scan_start..offset)
            .rev()
            .find(|&off| off + 4 <= data.len() && &data[off..off + 4] == b"xref");
        if let Some(xref_pos) = found {
            parse_classic_xref(data, xref_pos)
        } else {
            // Xref stream: an indirect object with /Type /XRef
            match parse_xref_stream(data, offset) {
                Ok(result) => Ok(result),
                Err(_) => {
                    // /Prev offset may point into the middle of an xref stream
                    // object (common in linearized PDFs). Search backwards for
                    // the actual object header.
                    let scan_start = offset.saturating_sub(256);
                    let scan_end = offset.min(data.len());
                    for search in (scan_start..scan_end).rev() {
                        if data[search].is_ascii_digit() {
                            if let Some((_, _, obj_offset)) =
                                try_parse_obj_header(data, search)
                            {
                                if obj_offset == search {
                                    if let Ok(result) =
                                        parse_xref_stream(data, search)
                                    {
                                        return Ok(result);
                                    }
                                }
                            }
                        }
                    }
                    parse_xref_stream(data, offset) // return original error
                }
            }
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

    // Parse subsections until we hit "trailer" (or data that isn't a valid
    // subsection header — some broken PDFs omit the trailer entirely).
    let mut has_trailer = false;
    loop {
        // Check for "trailer" keyword
        if pos + 7 <= data.len() && &data[pos..pos + 7] == b"trailer" {
            pos += 7;
            has_trailer = true;
            break;
        }

        // Parse subsection header: <first_obj_num> <count>
        // If this fails, the entries may have ended without a trailer.
        let Ok((first_obj, new_pos)) = parse_int_at(data, pos) else {
            break;
        };
        pos = new_pos;
        while pos < data.len() && (data[pos] == b' ' || data[pos] == b'\t') {
            pos += 1;
        }
        let Ok((count, new_pos)) = parse_int_at(data, pos) else {
            break;
        };
        pos = new_pos;

        // Skip to start of entries
        while pos < data.len() && is_whitespace(data[pos]) {
            pos += 1;
        }

        // Parse entries: spec says 20 bytes each, but tolerate 21 (extra space before EOL).
        // If an entry is malformed (e.g. we've run into binary stream data after
        // a trailer-less xref), stop parsing gracefully with what we have.
        let mut entry_error = false;
        for i in 0..count as u32 {
            if pos + 18 > data.len() {
                break;
            }

            // Parse: OOOOOOOOOO GGGGG f/n + EOL (variable length)
            let Ok(off_str) = std::str::from_utf8(&data[pos..pos + 10]) else {
                entry_error = true;
                break;
            };
            let Ok(off) = off_str.trim().parse::<usize>() else {
                entry_error = true;
                break;
            };

            let Ok(gen_str) = std::str::from_utf8(&data[pos + 11..pos + 16]) else {
                entry_error = true;
                break;
            };
            // Parse as u32 first, then clamp to u16 — some PDFs have
            // generation 65536 which overflows u16 but is otherwise valid.
            let Ok(gen_val) = gen_str.trim().parse::<u32>() else {
                entry_error = true;
                break;
            };
            let generation = gen_val.min(65535) as u16;

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

        // If an entry was malformed, stop parsing this xref section
        if entry_error {
            break;
        }

        // Skip any remaining whitespace
        while pos < data.len() && is_whitespace(data[pos]) {
            pos += 1;
        }
    }

    // Parse trailer dict (or return empty if the trailer is missing —
    // some broken incremental updates omit it entirely).
    let trailer = if has_trailer {
        let mut lexer = Lexer::at(data, pos);
        lexer.set_pos(pos);
        let tok = lexer.next_token()?;
        match tok {
            Token::DictBegin => parse_dict_body(&mut lexer)?,
            _ => return Err(PdfError::MalformedTrailer),
        }
    } else if entries.is_empty() {
        return Err(PdfError::MalformedXref(offset));
    } else {
        PdfDict::new()
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
    let (filter_list, parms) = filters::parse_filters(&dict, None)?;
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

/// Find all `%PDF-` header positions in the file.
///
/// Normal PDFs have one header at offset 0 (or after a BOM). Concatenated PDFs
/// (e.g., an encrypted revision appended to a plaintext revision) have multiple
/// headers. Offsets in later sections are relative to their own header.
fn find_all_pdf_headers(data: &[u8]) -> Vec<usize> {
    let mut headers = Vec::new();
    let mut pos = 0;
    while pos + 5 <= data.len() {
        if &data[pos..pos + 5] == b"%PDF-" {
            headers.push(pos);
            pos += 5;
        } else {
            pos += 1;
        }
    }
    headers
}

/// Determine which `%PDF-` header "owns" a given file position.
///
/// Returns the position of the last `%PDF-` header that appears before `pos`.
/// For positions under the first header (or before any header), returns 0.
fn owning_pdf_header(pos: usize, pdf_headers: &[usize]) -> usize {
    // Find the last header that is <= pos
    match pdf_headers.iter().rposition(|&h| h <= pos) {
        Some(idx) if idx > 0 => pdf_headers[idx],
        _ => 0,
    }
}

/// Shift all `InFile` xref entries by a base offset.
///
/// Used when a xref section's entry offsets are relative to a secondary
/// `%PDF-` header instead of the file start.
fn shift_xref_entries(entries: &mut [(u32, XrefEntry)], base: usize) {
    for (_, entry) in entries.iter_mut() {
        if let XrefEntry::InFile { offset, .. } = entry {
            *offset += base;
        }
    }
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

    if let Some(pos) = found {
        let mut p = pos + needle.len();
        while p < data.len() && is_whitespace(data[p]) {
            p += 1;
        }
        let (offset, _) = parse_int_at(data, p)?;
        return Ok(offset as usize);
    }

    // Strategy 3: No startxref at all (truncated file). Scan for the last
    // "xref" keyword and return its offset directly.
    let xref_kw = b"xref";
    let mut last_xref = None;
    for i in (0..data.len().saturating_sub(xref_kw.len())).rev() {
        if &data[i..i + xref_kw.len()] == xref_kw
            && (i == 0 || is_whitespace(data[i - 1]) || data[i - 1] == b'\n')
        {
            last_xref = Some(i);
            break;
        }
    }
    last_xref.ok_or(PdfError::NoStartXref)
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
