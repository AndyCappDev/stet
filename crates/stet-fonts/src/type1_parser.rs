// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Type 1 font file (.t1) parser.
//!
//! Parses ASCII header and eexec-encrypted binary section to extract
//! font metadata, encoding, charstrings, and subroutines.

use rustc_hash::FxHashMap as HashMap;

/// Parsed Type 1 font data.
pub struct Type1Font {
    pub font_name: String,
    pub font_matrix: [f64; 6],
    pub font_bbox: [f64; 4],
    pub paint_type: i32,
    pub encoding: Vec<String>,                 // 256 entries, glyph names
    pub charstrings: HashMap<String, Vec<u8>>, // glyph name → encrypted bytes
    pub subrs: Vec<Vec<u8>>,                   // subroutine charstrings (encrypted)
    pub len_iv: usize,                         // default 4
    /// Multiple Master weight vector (for blend interpolation).
    pub weight_vector: Option<Vec<f64>>,
}

/// Decrypt data using the Type 1 cipher.
///
/// Constants: C1=52845, C2=22719. The key `r` is the initial state
/// (55665 for eexec, 4330 for charstrings).
fn decrypt(data: &[u8], r: u16) -> Vec<u8> {
    let c1: u32 = 52845;
    let c2: u32 = 22719;
    let mut state = r as u32;
    let mut result = Vec::with_capacity(data.len());
    for &cipher in data {
        let plain = (cipher as u32 ^ (state >> 8)) as u8;
        result.push(plain);
        state = ((cipher as u32 + state) * c1 + c2) & 0xFFFF;
    }
    result
}

/// Decrypt the eexec section. Skips the first 4 random bytes after decryption.
pub fn decrypt_eexec(data: &[u8]) -> Vec<u8> {
    let decrypted = decrypt(data, 55665);
    if decrypted.len() > 4 {
        decrypted[4..].to_vec()
    } else {
        Vec::new()
    }
}

/// Check if data is hex-encoded eexec (vs binary).
pub fn is_hex_encoded(data: &[u8]) -> bool {
    // Check if the first non-whitespace bytes are hex characters
    let mut hex_count = 0;
    for &b in data.iter().take(40) {
        if b == b' ' || b == b'\n' || b == b'\r' || b == b'\t' {
            continue;
        }
        if b.is_ascii_hexdigit() {
            hex_count += 1;
        } else {
            return false;
        }
    }
    hex_count > 4
}

/// Decode hex-encoded binary data (strips whitespace).
pub fn decode_hex(data: &[u8]) -> Vec<u8> {
    let mut result = Vec::with_capacity(data.len() / 2);
    let mut nibble: Option<u8> = None;
    for &b in data {
        let val = match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => continue, // skip whitespace
        };
        if let Some(v) = val {
            match nibble {
                None => nibble = Some(v),
                Some(hi) => {
                    result.push((hi << 4) | v);
                    nibble = None;
                }
            }
        }
    }
    result
}

/// Parse a Type 1 font file (.t1 / .pfa format).
pub fn parse_type1(data: &[u8]) -> Result<Type1Font, String> {
    // Find "currentfile eexec" marker
    let eexec_marker = b"currentfile eexec";
    let eexec_pos = find_bytes(data, eexec_marker)
        .ok_or_else(|| "Missing 'currentfile eexec' marker".to_string())?;

    // Parse ASCII header (everything before eexec)
    let header = &data[..eexec_pos];
    let font_name = parse_font_name(header)?;
    let font_matrix = parse_font_matrix(header).unwrap_or([0.001, 0.0, 0.0, 0.001, 0.0, 0.0]);
    let font_bbox = parse_font_bbox(header).unwrap_or([0.0, 0.0, 1000.0, 1000.0]);
    let paint_type = parse_paint_type(header).unwrap_or(0);
    let encoding = parse_encoding(header);

    // Extract binary portion after the eexec marker.
    // Skip exactly one newline sequence after the marker — any bytes beyond
    // that are encrypted data, even if they look like whitespace (e.g. 0x0D).
    let after_marker = eexec_pos + eexec_marker.len();
    let mut binary_start = after_marker;
    // Skip spaces/tabs
    while binary_start < data.len() && (data[binary_start] == b' ' || data[binary_start] == b'\t') {
        binary_start += 1;
    }
    // Skip one newline: \r\n, \r, or \n
    if binary_start < data.len() {
        if data[binary_start] == b'\r' {
            binary_start += 1;
            if binary_start < data.len() && data[binary_start] == b'\n' {
                binary_start += 1;
            }
        } else if data[binary_start] == b'\n' {
            binary_start += 1;
        }
    }

    let binary_data = &data[binary_start..];

    // Detect hex vs binary encoding and decrypt
    let eexec_bytes = if is_hex_encoded(binary_data) {
        decode_hex(binary_data)
    } else {
        binary_data.to_vec()
    };

    let decrypted = decrypt_eexec(&eexec_bytes);

    // Parse decrypted section for Private dict, CharStrings, and Subrs
    let len_iv = parse_len_iv(&decrypted).unwrap_or(4);
    let charstrings = parse_charstrings(&decrypted);
    let subrs = parse_subrs(&decrypted);

    // Parse /WeightVector from header (Multiple Master fonts)
    let weight_vector = parse_weight_vector(header).or_else(|| parse_weight_vector(&decrypted));

    Ok(Type1Font {
        font_name,
        font_matrix,
        font_bbox,
        paint_type,
        encoding,
        charstrings,
        subrs,
        len_iv,
        weight_vector,
    })
}

/// Find a byte pattern in data.
fn find_bytes(data: &[u8], pattern: &[u8]) -> Option<usize> {
    data.windows(pattern.len()).position(|w| w == pattern)
}

/// Parse /FontName from ASCII header.
fn parse_font_name(header: &[u8]) -> Result<String, String> {
    let text = String::from_utf8_lossy(header);
    // Look for /FontName /SomeName
    if let Some(idx) = text.find("/FontName") {
        let after = &text[idx + 9..];
        // Skip whitespace
        let trimmed = after.trim_start();
        if let Some(rest) = trimmed.strip_prefix('/') {
            // Extract name until whitespace
            let name_end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
            return Ok(rest[..name_end].to_string());
        }
    }
    Err("Missing /FontName in font header".to_string())
}

/// Parse /WeightVector from font data (Multiple Master fonts).
fn parse_weight_vector(data: &[u8]) -> Option<Vec<f64>> {
    let text = String::from_utf8_lossy(data);
    let idx = text.find("/WeightVector")?;
    let after = &text[idx + "/WeightVector".len()..];
    let bracket_idx = after.find('[')?;
    let end_idx = after[bracket_idx..].find(']')?;
    let inner = &after[bracket_idx + 1..bracket_idx + end_idx];
    let vals: Vec<f64> = inner
        .split_whitespace()
        .filter_map(|s| s.parse().ok())
        .collect();
    if vals.is_empty() { None } else { Some(vals) }
}

/// Parse /FontMatrix [a b c d tx ty] from ASCII header.
fn parse_font_matrix(header: &[u8]) -> Option<[f64; 6]> {
    let text = String::from_utf8_lossy(header);
    parse_number_array(&text, "/FontMatrix", 6).map(|v| [v[0], v[1], v[2], v[3], v[4], v[5]])
}

/// Parse /FontBBox {llx lly urx ury} from ASCII header.
fn parse_font_bbox(header: &[u8]) -> Option<[f64; 4]> {
    let text = String::from_utf8_lossy(header);
    // FontBBox can use either [] or {} delimiters
    parse_number_array(&text, "/FontBBox", 4).map(|v| [v[0], v[1], v[2], v[3]])
}

/// Parse a number array from text, looking for a key followed by [ or { delimited numbers.
fn parse_number_array(text: &str, key: &str, count: usize) -> Option<Vec<f64>> {
    let idx = text.find(key)?;
    let after = &text[idx + key.len()..];
    // Find the start delimiter
    let bracket_idx = after.find(['[', '{', '('])?;
    let end_char = match after.as_bytes()[bracket_idx] {
        b'[' => ']',
        b'{' => '}',
        _ => ')',
    };
    let inner_start = bracket_idx + 1;
    let inner_end = after[inner_start..].find(end_char)? + inner_start;
    let inner = &after[inner_start..inner_end];

    let numbers: Vec<f64> = inner
        .split_whitespace()
        .filter_map(|s| s.parse().ok())
        .collect();

    if numbers.len() >= count {
        Some(numbers[..count].to_vec())
    } else {
        None
    }
}

/// Parse /PaintType from header.
fn parse_paint_type(header: &[u8]) -> Option<i32> {
    let text = String::from_utf8_lossy(header);
    parse_int_value(&text, "/PaintType")
}

/// Parse an integer value after a key: `/Key N def`
fn parse_int_value(text: &str, key: &str) -> Option<i32> {
    let idx = text.find(key)?;
    let after = &text[idx + key.len()..];
    let trimmed = after.trim_start();
    trimmed.split_whitespace().next()?.parse().ok()
}

/// Parse the Encoding from the ASCII header.
/// Returns a 256-entry vector of glyph names.
fn parse_encoding(header: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(header);
    let mut encoding = vec![".notdef".to_string(); 256];

    // Check for "StandardEncoding def" or "/Encoding StandardEncoding def"
    if text.contains("StandardEncoding") && !text.contains("256 array") {
        // Use StandardEncoding
        for (i, name) in crate::encoding::STANDARD_ENCODING.iter().enumerate() {
            encoding[i] = name.to_string();
        }
        return encoding;
    }

    // Check for "ISOLatin1Encoding def"
    if text.contains("ISOLatin1Encoding") && !text.contains("256 array") {
        for (i, name) in crate::encoding::ISO_LATIN1_ENCODING.iter().enumerate() {
            encoding[i] = name.to_string();
        }
        return encoding;
    }

    // Parse custom encoding: `dup N /name put` entries
    // Look for /Encoding followed by array definition
    if let Some(enc_idx) = text.find("/Encoding") {
        let enc_section = &text[enc_idx..];
        // Find all "dup N /name put" patterns
        for line in enc_section.lines() {
            let trimmed = line.trim();
            if !trimmed.starts_with("dup ") {
                continue;
            }
            // Parse: dup <index> /<name> put
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if let Ok(idx) = parts[1].parse::<usize>()
                && parts.len() >= 4
                && parts[0] == "dup"
                && parts[3] == "put"
                && idx < 256
                && let Some(name) = parts[2].strip_prefix('/')
            {
                encoding[idx] = name.to_string();
            }
        }
    }

    encoding
}

/// Parse /lenIV from the decrypted eexec section.
fn parse_len_iv(decrypted: &[u8]) -> Option<usize> {
    let text = String::from_utf8_lossy(decrypted);
    parse_int_value(&text, "/lenIV").map(|v| v as usize)
}

/// Parse CharStrings from decrypted eexec data.
/// Extracts `/glyphname N RD <bytes> ND` or `/glyphname N -| <bytes> |-` patterns.
fn parse_charstrings(decrypted: &[u8]) -> HashMap<String, Vec<u8>> {
    let mut charstrings = HashMap::default();

    // Find the CharStrings section
    let cs_marker = b"/CharStrings";
    let Some(cs_start) = find_bytes(decrypted, cs_marker) else {
        return charstrings;
    };

    // Parse entries: /glyphname N RD <N bytes> ND
    // or: /glyphname N -| <N bytes> |-
    let data = &decrypted[cs_start..];
    let mut pos = 0;

    while pos < data.len() {
        // Find next /name
        let slash_pos = match data[pos..].iter().position(|&b| b == b'/') {
            Some(p) => pos + p,
            None => break,
        };

        // Check if we've hit "end" or similar terminator
        if slash_pos + 1 >= data.len() {
            break;
        }

        // Extract glyph name
        let name_start = slash_pos + 1;
        let name_end = match data[name_start..]
            .iter()
            .position(|&b| b == b' ' || b == b'\t' || b == b'\n' || b == b'\r')
        {
            Some(p) => name_start + p,
            None => break,
        };

        let glyph_name = match std::str::from_utf8(&data[name_start..name_end]) {
            Ok(s) => s.to_string(),
            Err(_) => {
                pos = name_end;
                continue;
            }
        };

        // After name, skip whitespace to find the byte count
        let mut after_name = name_end;
        while after_name < data.len() && data[after_name].is_ascii_whitespace() {
            after_name += 1;
        }

        // Parse the byte count
        let count_end = match data[after_name..].iter().position(|&b| !b.is_ascii_digit()) {
            Some(p) => after_name + p,
            None => break,
        };

        let byte_count: usize = match std::str::from_utf8(&data[after_name..count_end]) {
            Ok(s) => match s.parse() {
                Ok(n) => n,
                Err(_) => {
                    pos = count_end;
                    continue;
                }
            },
            Err(_) => {
                pos = count_end;
                continue;
            }
        };

        // After the count, find "RD " or "-| " (the read-data marker)
        // Skip to after the marker: the next space after RD/-|
        let mut marker_pos = count_end;
        while marker_pos < data.len() && data[marker_pos].is_ascii_whitespace() {
            marker_pos += 1;
        }

        // The marker might be "RD" or "-|"
        if marker_pos + 2 >= data.len() {
            break;
        }

        // Skip the marker and exactly one space/newline after it
        let is_rd_marker = (data[marker_pos] == b'R' && data[marker_pos + 1] == b'D')
            || (data[marker_pos] == b'-' && data[marker_pos + 1] == b'|');
        if !is_rd_marker {
            pos = marker_pos + 1;
            continue;
        }
        let binary_start = marker_pos + 3; // marker (2 bytes) + one separator byte

        if binary_start + byte_count > data.len() {
            break;
        }

        let charstring_bytes = data[binary_start..binary_start + byte_count].to_vec();
        charstrings.insert(glyph_name, charstring_bytes);

        pos = binary_start + byte_count;
    }

    charstrings
}

/// Parse Subrs from decrypted eexec data.
/// Extracts `dup N M RD <M bytes> NP` patterns.
fn parse_subrs(decrypted: &[u8]) -> Vec<Vec<u8>> {
    let mut subrs: Vec<Vec<u8>> = Vec::new();

    // Find the Subrs section
    let subrs_marker = b"/Subrs";
    let Some(subrs_start) = find_bytes(decrypted, subrs_marker) else {
        return subrs;
    };

    // Find the count: /Subrs N array
    let after_marker = &decrypted[subrs_start + subrs_marker.len()..];
    let text = String::from_utf8_lossy(after_marker);
    let trimmed = text.trim_start();
    let count: usize = trimmed
        .split_whitespace()
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    subrs.resize(count, Vec::new());

    // Parse entries: dup N M RD <M bytes> NP
    let data = &decrypted[subrs_start..];
    let mut pos = 0;

    while pos < data.len() {
        // Find next "dup "
        let dup_pos = match find_bytes(&data[pos..], b"dup ") {
            Some(p) => pos + p,
            None => break,
        };

        let after_dup = dup_pos + 4;
        if after_dup >= data.len() {
            break;
        }

        // Check if we've reached a section terminator
        // Look for "ND" or "|-" or "def" following the subrs array
        // Or if we've moved past into CharStrings
        if find_bytes(
            &data[dup_pos..dup_pos + 20.min(data.len() - dup_pos)],
            b"/CharStrings",
        )
        .is_some()
        {
            break;
        }

        // Parse: dup <index> <byte_count> RD <bytes> NP
        let mut cursor = after_dup;
        while cursor < data.len() && data[cursor].is_ascii_whitespace() {
            cursor += 1;
        }

        // Parse index
        let idx_end = cursor
            + data[cursor..]
                .iter()
                .position(|&b| !b.is_ascii_digit())
                .unwrap_or(data.len() - cursor);

        let index: usize = match std::str::from_utf8(&data[cursor..idx_end]) {
            Ok(s) => match s.parse() {
                Ok(n) => n,
                Err(_) => {
                    pos = idx_end;
                    continue;
                }
            },
            Err(_) => {
                pos = idx_end;
                continue;
            }
        };

        cursor = idx_end;
        while cursor < data.len() && data[cursor].is_ascii_whitespace() {
            cursor += 1;
        }

        // Parse byte count
        let count_end = cursor
            + data[cursor..]
                .iter()
                .position(|&b| !b.is_ascii_digit())
                .unwrap_or(data.len() - cursor);

        let byte_count: usize = match std::str::from_utf8(&data[cursor..count_end]) {
            Ok(s) => match s.parse() {
                Ok(n) => n,
                Err(_) => {
                    pos = count_end;
                    continue;
                }
            },
            Err(_) => {
                pos = count_end;
                continue;
            }
        };

        cursor = count_end;
        while cursor < data.len() && data[cursor].is_ascii_whitespace() {
            cursor += 1;
        }

        // Skip RD or -| marker plus one separator byte
        if cursor + 2 >= data.len() {
            break;
        }

        let is_rd = (data[cursor] == b'R' && data[cursor + 1] == b'D')
            || (data[cursor] == b'-' && data[cursor + 1] == b'|');
        if !is_rd {
            pos = cursor + 1;
            continue;
        }
        let binary_start = cursor + 3;

        if binary_start + byte_count > data.len() {
            break;
        }

        let bytes = data[binary_start..binary_start + byte_count].to_vec();
        if index < subrs.len() {
            subrs[index] = bytes;
        }

        pos = binary_start + byte_count;
    }

    subrs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decrypt_eexec_constants() {
        // Verify the decrypt function works with known test vectors
        // Encrypt then decrypt should produce original (minus random prefix)
        let plain = b"Hello";
        let c1: u32 = 52845;
        let c2: u32 = 22719;
        let mut r: u32 = 55665;

        // Prepend 4 random bytes (zeros for simplicity)
        let mut to_encrypt = vec![0u8; 4];
        to_encrypt.extend_from_slice(plain);

        // Encrypt
        let mut encrypted = Vec::new();
        for &p in &to_encrypt {
            let c = (p as u32 ^ (r >> 8)) as u8;
            encrypted.push(c);
            r = ((c as u32 + r) * c1 + c2) & 0xFFFF;
        }

        // Decrypt
        let decrypted = decrypt_eexec(&encrypted);
        assert_eq!(&decrypted, plain);
    }

    #[test]
    fn test_is_hex_encoded() {
        assert!(is_hex_encoded(b"D9D66F633B846AB284BCF8B0411D1A34"));
        assert!(!is_hex_encoded(b"\x80\x01\x00\x00binary"));
    }

    #[test]
    fn test_decode_hex() {
        assert_eq!(decode_hex(b"48656C6C6F"), b"Hello");
        assert_eq!(decode_hex(b"48 65 6C\n6C 6F"), b"Hello");
    }

    #[test]
    fn test_parse_font_name() {
        let header = b"/FontName /NimbusSans-Regular def";
        assert_eq!(parse_font_name(header).unwrap(), "NimbusSans-Regular");
    }

    #[test]
    fn test_parse_font_matrix() {
        let header = b"/FontMatrix [0.001 0.0 0.0 0.001 0.0 0.0] readonly def";
        let m = parse_font_matrix(header).unwrap();
        assert!((m[0] - 0.001).abs() < 1e-10);
        assert!((m[3] - 0.001).abs() < 1e-10);
    }

    #[test]
    fn test_parse_font_bbox_braces() {
        let header = b"/FontBBox {-210 -299 1032 1075} readonly def";
        let bb = parse_font_bbox(header).unwrap();
        assert_eq!(bb[0], -210.0);
        assert_eq!(bb[3], 1075.0);
    }

    #[test]
    fn test_parse_encoding_standard() {
        let header = b"/Encoding StandardEncoding def";
        let enc = parse_encoding(header);
        assert_eq!(enc[0x41], "A");
        assert_eq!(enc[0x20], "space");
    }

    #[test]
    fn test_parse_real_font_file() {
        let font_path = std::path::Path::new(
            "/home/scott/Projects/postforge/postforge/resources/Font/NimbusSans-Regular.t1",
        );
        if !font_path.exists() {
            eprintln!("Skipping test — font file not found");
            return;
        }

        let data = std::fs::read(font_path).unwrap();
        let font = parse_type1(&data).unwrap();

        assert_eq!(font.font_name, "NimbusSans-Regular");
        assert!((font.font_matrix[0] - 0.001).abs() < 1e-10);
        assert_eq!(font.paint_type, 0);
        // NimbusSans-Regular should have many charstrings
        assert!(
            font.charstrings.len() > 100,
            "Expected >100 charstrings, got {}",
            font.charstrings.len()
        );
        assert!(font.charstrings.contains_key(".notdef"));
        assert!(font.charstrings.contains_key("A"));
        assert!(font.charstrings.contains_key("space"));
        // Should have subroutines
        assert!(!font.subrs.is_empty(), "Expected subroutines, got none");
    }
}
