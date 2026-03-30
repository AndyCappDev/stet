// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! CMap parser for Type 0 (composite) font encoding.

use std::collections::HashMap;

/// Parsed CMap: maps character codes to CIDs.
pub struct CMap {
    /// Code-to-CID mapping (character code → CID).
    pub code_to_cid: HashMap<u32, u32>,
    /// Precomputed first-byte → code length table.
    /// 0 = not in any codespace range (treat as 2-byte default).
    pub code_lengths: [u8; 256],
    /// Writing mode: 0 = horizontal, 1 = vertical.
    pub wmode: u8,
}

impl CMap {
    /// Create an Identity CMap (code == CID, all 2-byte).
    pub fn identity() -> Self {
        Self {
            code_to_cid: HashMap::new(),
            code_lengths: [2; 256],
            wmode: 0,
        }
    }

    /// Decode a character code to a CID.
    pub fn decode(&self, code: u32) -> u32 {
        // Identity mapping: code == CID
        self.code_to_cid.get(&code).copied().unwrap_or(code)
    }

    /// Get the byte width of a character code starting with the given byte.
    pub fn code_width(&self, first_byte: u8) -> usize {
        let w = self.code_lengths[first_byte as usize];
        if w == 0 { 2 } else { w as usize }
    }

    /// Parse a CMap from stream data.
    pub fn parse(data: &[u8]) -> Self {
        Self::parse_with_loader(data, None)
    }

    /// Parse a CMap, optionally resolving `usecmap` with a loader function.
    pub fn parse_with_loader(
        data: &[u8],
        loader: Option<&dyn Fn(&[u8]) -> Option<Vec<u8>>>,
    ) -> Self {
        let mut code_to_cid = HashMap::new();
        let mut codespace_ranges: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        let mut wmode: u8 = 0;

        let text = String::from_utf8_lossy(data);
        #[allow(clippy::while_let_on_iterator)]
        let mut lines = text.lines();

        while let Some(line) = lines.next() {
            let line = line.trim();

            // Handle usecmap: inherit mappings from the referenced CMap
            if line.ends_with("usecmap") {
                let name = line.strip_suffix("usecmap").unwrap_or("").trim();
                let name = name.strip_prefix('/').unwrap_or(name);
                if !name.is_empty() {
                    if let Some(load_fn) = loader {
                        if let Some(base_data) = load_fn(name.as_bytes()) {
                            let base = Self::parse_with_loader(&base_data, loader);
                            // Inherit base mappings (current entries override)
                            for (k, v) in base.code_to_cid {
                                code_to_cid.entry(k).or_insert(v);
                            }
                            if codespace_ranges.is_empty() {
                                // Inherit codespace from base if not defined yet
                                for fb in 0..256u16 {
                                    let w = base.code_lengths[fb as usize];
                                    if w > 0 {
                                        let low = if w == 1 {
                                            vec![fb as u8]
                                        } else {
                                            vec![fb as u8, 0x00]
                                        };
                                        let high = if w == 1 {
                                            vec![fb as u8]
                                        } else {
                                            vec![fb as u8, 0xFF]
                                        };
                                        codespace_ranges.push((low, high));
                                    }
                                }
                            }
                            if wmode == 0 {
                                wmode = base.wmode;
                            }
                        }
                    }
                }
            }

            // Parse /WMode
            if let Some(rest) = line.strip_prefix("/WMode") {
                let rest = rest.trim();
                if let Some(rest) = rest.strip_prefix("def").or(Some(rest)) {
                    if let Ok(v) = rest.trim().parse::<u8>() {
                        wmode = v;
                    }
                }
            }
            // Also handle "N /WMode def" pattern
            if line.ends_with("/WMode def") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if let Some(v) = parts.first().and_then(|s| s.parse::<u8>().ok()) {
                    wmode = v;
                }
            }

            // Parse codespace ranges
            if line.ends_with("begincodespacerange") {
                while let Some(range_line) = lines.next() {
                    let range_line = range_line.trim();
                    if range_line == "endcodespacerange" {
                        break;
                    }
                    if let Some((low, high)) = parse_codespace_range(range_line) {
                        codespace_ranges.push((low, high));
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
                        code_to_cid.insert(code, cid);
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
                            code_to_cid.insert(code, cid_start + (code - start));
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
                        code_to_cid.insert(code, unicode);
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
                            code_to_cid.insert(code, cid_start + (code - start));
                        }
                    }
                }
            }
        }

        // Build first-byte → code-length table from codespace ranges.
        // For each first byte, find the shortest matching codespace range.
        let mut code_lengths = [0u8; 256];
        if codespace_ranges.is_empty() {
            // No codespace ranges → default all to 2-byte
            code_lengths = [2; 256];
        } else {
            for (low, high) in &codespace_ranges {
                let width = low.len() as u8;
                let first_lo = low[0];
                let first_hi = high[0];
                for byte in first_lo..=first_hi {
                    let cur = code_lengths[byte as usize];
                    // Prefer shorter (1-byte over 2-byte) or fill if unset
                    if cur == 0 || width < cur {
                        code_lengths[byte as usize] = width;
                    }
                }
            }
        }

        CMap {
            code_to_cid,
            code_lengths,
            wmode,
        }
    }
}

/// Parse a codespace range line like `<20> <20>` or `<0000> <19FF>`.
/// Returns (low_bytes, high_bytes).
fn parse_codespace_range(line: &str) -> Option<(Vec<u8>, Vec<u8>)> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() >= 2 {
        let low = parse_hex_bytes(parts[0])?;
        let high = parse_hex_bytes(parts[1])?;
        if low.len() == high.len() && !low.is_empty() {
            Some((low, high))
        } else {
            None
        }
    } else {
        None
    }
}

/// Parse a hex string like `<0041>` into raw bytes.
fn parse_hex_bytes(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.starts_with('<') && s.ends_with('>') {
        let hex = &s[1..s.len() - 1];
        let mut bytes = Vec::new();
        let mut i = 0;
        while i + 1 < hex.len() {
            bytes.push(u8::from_str_radix(&hex[i..i + 2], 16).ok()?);
            i += 2;
        }
        // Odd-length hex: pad last nibble
        if i < hex.len() {
            bytes.push(u8::from_str_radix(&format!("{}0", &hex[i..]), 16).ok()?);
        }
        Some(bytes)
    } else {
        None
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
