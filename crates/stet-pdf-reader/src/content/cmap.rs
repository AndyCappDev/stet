// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! CMap parser for Type 0 (composite) font encoding.

use std::collections::HashMap;

/// Parsed CMap: maps character codes to CIDs.
pub struct CMap {
    /// Code-to-CID mapping (character code → CID).
    pub code_to_cid: HashMap<u32, u32>,
    /// Whether this is a 2-byte encoding (most common for CID fonts).
    pub is_two_byte: bool,
}

impl CMap {
    /// Create an Identity CMap (code == CID).
    pub fn identity() -> Self {
        Self {
            code_to_cid: HashMap::new(),
            is_two_byte: true,
        }
    }

    /// Decode a character code to a CID.
    pub fn decode(&self, code: u32) -> u32 {
        // Identity mapping: code == CID
        self.code_to_cid.get(&code).copied().unwrap_or(code)
    }

    /// Parse a CMap from stream data.
    pub fn parse(data: &[u8]) -> Self {
        let mut cmap = CMap {
            code_to_cid: HashMap::new(),
            is_two_byte: true,
        };

        let text = String::from_utf8_lossy(data);
        // We need `while let ... next()` rather than `for` because the inner loops
        // also advance the same iterator, which requires re-borrowing between calls.
        #[allow(clippy::while_let_on_iterator)]
        let mut lines = text.lines();

        while let Some(line) = lines.next() {
            let line = line.trim();

            // Detect codespace range to determine byte width
            if line.ends_with("begincodespacerange") {
                while let Some(range_line) = lines.next() {
                    let range_line = range_line.trim();
                    if range_line == "endcodespacerange" {
                        break;
                    }
                    // Parse <XX> or <XXXX> to determine byte width
                    if let Some(first_bracket) = range_line.find('<') {
                        if let Some(end_bracket) = range_line[first_bracket + 1..].find('>') {
                            cmap.is_two_byte = end_bracket > 2;
                        }
                    }
                }
            }

            // Parse cidchar mappings: <code> cid
            if line.ends_with("begincidchar") {
                while let Some(char_line) = lines.next() {
                    let char_line = char_line.trim();
                    if char_line == "endcidchar" {
                        break;
                    }
                    if let Some((code, cid)) = parse_cidchar_line(char_line) {
                        cmap.code_to_cid.insert(code, cid);
                    }
                }
            }

            // Parse cidrange mappings: <start> <end> cid_start
            if line.ends_with("begincidrange") {
                while let Some(range_line) = lines.next() {
                    let range_line = range_line.trim();
                    if range_line == "endcidrange" {
                        break;
                    }
                    if let Some((start, end, cid_start)) = parse_cidrange_line(range_line) {
                        for code in start..=end {
                            cmap.code_to_cid.insert(code, cid_start + (code - start));
                        }
                    }
                }
            }

            // Also parse bfchar/bfrange (some CMaps use these)
            if line.ends_with("beginbfchar") {
                while let Some(char_line) = lines.next() {
                    let char_line = char_line.trim();
                    if char_line == "endbfchar" {
                        break;
                    }
                    if let Some((code, unicode)) = parse_bfchar_line(char_line) {
                        cmap.code_to_cid.insert(code, unicode);
                    }
                }
            }

            if line.ends_with("beginbfrange") {
                while let Some(range_line) = lines.next() {
                    let range_line = range_line.trim();
                    if range_line == "endbfrange" {
                        break;
                    }
                    if let Some((start, end, cid_start)) = parse_cidrange_line(range_line) {
                        for code in start..=end {
                            cmap.code_to_cid.insert(code, cid_start + (code - start));
                        }
                    }
                }
            }
        }

        cmap
    }
}

/// Parse a hex string like `<0041>` into a u32.
fn parse_hex(s: &str) -> Option<u32> {
    let s = s.trim();
    if s.starts_with('<') && s.ends_with('>') {
        u32::from_str_radix(&s[1..s.len() - 1], 16).ok()
    } else {
        None
    }
}

/// Parse a cidchar line: `<code> cid`
fn parse_cidchar_line(line: &str) -> Option<(u32, u32)> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() >= 2 {
        let code = parse_hex(parts[0])?;
        let cid = parts[1].parse::<u32>().ok()?;
        Some((code, cid))
    } else {
        None
    }
}

/// Parse a cidrange line: `<start> <end> cid_start`
fn parse_cidrange_line(line: &str) -> Option<(u32, u32, u32)> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() >= 3 {
        let start = parse_hex(parts[0])?;
        let end = parse_hex(parts[1])?;
        let cid_start = if parts[2].starts_with('<') {
            parse_hex(parts[2])?
        } else {
            parts[2].parse::<u32>().ok()?
        };
        Some((start, end, cid_start))
    } else {
        None
    }
}

/// Parse a bfchar line: `<code> <unicode>`
fn parse_bfchar_line(line: &str) -> Option<(u32, u32)> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() >= 2 {
        let code = parse_hex(parts[0])?;
        let unicode = parse_hex(parts[1])?;
        Some((code, unicode))
    } else {
        None
    }
}
