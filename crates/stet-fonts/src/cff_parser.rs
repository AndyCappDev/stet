// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! CFF (Compact Font Format) binary parser.
//!
//! Parses CFF binary data (Adobe TN#5176) into structured `CffFont` objects.
//! CFF is a compact binary encoding for Type 1-style fonts using Type 2 charstrings.
//!
//! CFF data appears in PostScript via the FontSet resource mechanism:
//!   FontSetInit /ProcSet findresource begin ... StartData
//!
//! This parser handles:
//! - CFF Header, Name INDEX, Top DICT, String INDEX, Global Subr INDEX
//! - CharStrings INDEX, charset, encoding, Private DICT, Local Subr INDEX
//! - Both name-keyed and CID-keyed fonts
//! - Predefined charsets (ISOAdobe, Expert, ExpertSubset) and encodings

/// A parsed CFF font.
pub struct CffFont {
    /// Font name from Name INDEX.
    pub name: String,
    /// Font transformation matrix (default [0.001, 0, 0, 0.001, 0, 0]).
    pub font_matrix: [f64; 6],
    /// Font bounding box.
    pub font_bbox: [f64; 4],
    /// GID-indexed raw Type 2 charstring bytes.
    pub char_strings: Vec<Vec<u8>>,
    /// GID → glyph name.
    pub charset: Vec<String>,
    /// char_code (0–255) → GID.
    pub encoding: Vec<u16>,
    /// Default advance width from Private DICT.
    pub default_width_x: f64,
    /// Nominal advance width from Private DICT.
    pub nominal_width_x: f64,
    /// Local subroutines from Private DICT.
    pub local_subrs: Vec<Vec<u8>>,
    /// Global subroutines (shared across all fonts in FontSet).
    pub global_subrs: Vec<Vec<u8>>,
    /// Whether this is a CID-keyed font (ROS operator present).
    pub is_cid: bool,
    /// Per-FD Private dicts + subrs (CID only).
    pub fd_array: Vec<FdEntry>,
    /// GID → FD index (CID only).
    pub fd_select: Vec<u8>,
    /// Registry-Ordering-Supplement (CID only).
    pub ros: Option<(String, String, i32)>,
    /// CID → GID mapping for CID-keyed fonts.
    /// In CID fonts, the charset encodes GID → CID; this is the reverse map.
    pub cid_to_gid: Vec<u16>,
}

/// Per-FD entry for CID fonts (from FDArray).
pub struct FdEntry {
    /// Default advance width for this FD.
    pub default_width_x: f64,
    /// Nominal advance width for this FD.
    pub nominal_width_x: f64,
    /// Local subroutines for this FD.
    pub local_subrs: Vec<Vec<u8>>,
    /// Per-FD FontMatrix (None = use top-level FontMatrix).
    pub font_matrix: Option<[f64; 6]>,
}

/// Parse CFF binary data into a list of `CffFont` objects.
pub fn parse_cff(data: &[u8]) -> Result<Vec<CffFont>, String> {
    if data.len() < 4 {
        return Err("CFF data too short for header".into());
    }

    // Header
    let major = data[0];
    if major != 1 {
        return Err(format!("Unsupported CFF major version: {major}"));
    }
    let hdr_size = data[2] as usize;
    let mut offset = hdr_size;

    // Name INDEX
    let (name_index, off) = parse_index(data, offset)?;
    offset = off;

    // Top DICT INDEX
    let (top_dict_index, off) = parse_index(data, offset)?;
    offset = off;

    // String INDEX
    let (string_index, off) = parse_index(data, offset)?;
    offset = off;

    // Global Subr INDEX
    let (global_subr_index, _off) = parse_index(data, offset)?;

    let mut fonts = Vec::new();
    for font_idx in 0..name_index.len() {
        let mut font = CffFont {
            name: String::from_utf8_lossy(&name_index[font_idx]).into_owned(),
            font_matrix: [0.001, 0.0, 0.0, 0.001, 0.0, 0.0],
            font_bbox: [0.0; 4],
            char_strings: Vec::new(),
            charset: Vec::new(),
            encoding: vec![0u16; 256],
            default_width_x: 0.0,
            nominal_width_x: 0.0,
            local_subrs: Vec::new(),
            global_subrs: global_subr_index.clone(),
            is_cid: false,
            fd_array: Vec::new(),
            fd_select: Vec::new(),
            ros: None,
            cid_to_gid: Vec::new(),
        };

        // Parse Top DICT
        let top_dict = if font_idx < top_dict_index.len() {
            parse_dict_data(&top_dict_index[font_idx])
        } else {
            Vec::new()
        };

        // FontMatrix (12,7)
        if let Some(vals) = dict_get(&top_dict, DictOp::TwoByte(12, 7))
            && vals.len() == 6
        {
            for (i, v) in vals.iter().enumerate() {
                font.font_matrix[i] = *v;
            }
        }

        // FontBBox (5)
        if let Some(vals) = dict_get(&top_dict, DictOp::OneByte(5))
            && vals.len() == 4
        {
            for (i, v) in vals.iter().enumerate() {
                font.font_bbox[i] = *v;
            }
        }

        // CID detection (ROS = 12,30)
        if let Some(ros_ops) = dict_get(&top_dict, DictOp::TwoByte(12, 30)) {
            font.is_cid = true;
            if ros_ops.len() >= 3 {
                let registry = get_sid_string(ros_ops[0] as u16, &string_index);
                let ordering = get_sid_string(ros_ops[1] as u16, &string_index);
                let supplement = ros_ops[2] as i32;
                font.ros = Some((registry, ordering, supplement));
            }
        }

        // CharStrings INDEX (op 17)
        if let Some(vals) = dict_get(&top_dict, DictOp::OneByte(17))
            && !vals.is_empty()
        {
            let cs_offset = vals[0] as usize;
            if cs_offset > 0 && cs_offset < data.len() {
                let (cs_items, _) = parse_index(data, cs_offset)?;
                font.char_strings = cs_items;
            }
        }

        let n_glyphs = font.char_strings.len();

        // Charset (op 15)
        let charset_val = dict_get(&top_dict, DictOp::OneByte(15))
            .and_then(|v| v.first().copied())
            .unwrap_or(0.0) as i32;
        if charset_val <= 2 {
            font.charset = get_predefined_charset(charset_val, n_glyphs, &string_index);
        } else {
            font.charset = parse_charset(data, charset_val as usize, n_glyphs, &string_index)?;
        }

        // Build CID→GID reverse mapping for CID-keyed fonts.
        // In CID fonts, charset values are CID values (not SIDs).
        if font.is_cid && charset_val > 2 {
            font.cid_to_gid = build_cid_to_gid(data, charset_val as usize, n_glyphs)?;
        }

        // Encoding (only for name-keyed fonts, op 16)
        if !font.is_cid {
            let enc_val = dict_get(&top_dict, DictOp::OneByte(16))
                .and_then(|v| v.first().copied())
                .unwrap_or(0.0) as i32;
            if enc_val <= 1 {
                font.encoding = get_predefined_encoding(enc_val, &font.charset, &string_index);
            } else {
                font.encoding =
                    parse_encoding(data, enc_val as usize, &font.charset, &string_index)?;
            }
        }

        // Private DICT (op 18: [size, offset])
        if let Some(priv_ops) = dict_get(&top_dict, DictOp::OneByte(18))
            && priv_ops.len() >= 2
        {
            let priv_size = priv_ops[0] as usize;
            let priv_offset = priv_ops[1] as usize;
            if priv_size > 0 && priv_offset > 0 && priv_offset + priv_size <= data.len() {
                let priv_data = &data[priv_offset..priv_offset + priv_size];
                let priv_dict = parse_dict_data(priv_data);

                // defaultWidthX (op 20)
                if let Some(vals) = dict_get(&priv_dict, DictOp::OneByte(20))
                    && let Some(&v) = vals.first()
                {
                    font.default_width_x = v;
                }

                // nominalWidthX (op 21)
                if let Some(vals) = dict_get(&priv_dict, DictOp::OneByte(21))
                    && let Some(&v) = vals.first()
                {
                    font.nominal_width_x = v;
                }

                // Local Subr INDEX (op 19, offset relative to Private DICT start)
                if let Some(vals) = dict_get(&priv_dict, DictOp::OneByte(19))
                    && let Some(&v) = vals.first()
                {
                    let subr_abs_offset = priv_offset + v as usize;
                    if subr_abs_offset < data.len() {
                        let (local_subrs, _) = parse_index(data, subr_abs_offset)?;
                        font.local_subrs = local_subrs;
                    }
                }
            }
        }

        // CID-specific: FDArray and FDSelect
        if font.is_cid {
            // FDArray (12,36)
            if let Some(vals) = dict_get(&top_dict, DictOp::TwoByte(12, 36))
                && let Some(&v) = vals.first()
            {
                let fda_offset = v as usize;
                if fda_offset < data.len() {
                    let (fd_dicts_raw, _) = parse_index(data, fda_offset)?;
                    for fd_raw in &fd_dicts_raw {
                        let fd_top = parse_dict_data(fd_raw);
                        let mut fd_entry = FdEntry {
                            default_width_x: 0.0,
                            nominal_width_x: 0.0,
                            local_subrs: Vec::new(),
                            font_matrix: None,
                        };

                        // Check for FD-level FontMatrix
                        if let Some(fm_vals) = dict_get(&fd_top, DictOp::TwoByte(12, 7))
                            && fm_vals.len() == 6
                        {
                            fd_entry.font_matrix = Some([
                                fm_vals[0], fm_vals[1], fm_vals[2],
                                fm_vals[3], fm_vals[4], fm_vals[5],
                            ]);
                        }

                        // Each FD has its own Private DICT
                        if let Some(fd_priv_ops) = dict_get(&fd_top, DictOp::OneByte(18))
                            && fd_priv_ops.len() >= 2
                        {
                            let fd_priv_size = fd_priv_ops[0] as usize;
                            let fd_priv_offset = fd_priv_ops[1] as usize;
                            if fd_priv_size > 0
                                && fd_priv_offset > 0
                                && fd_priv_offset + fd_priv_size <= data.len()
                            {
                                let fd_priv_data =
                                    &data[fd_priv_offset..fd_priv_offset + fd_priv_size];
                                let fd_priv_dict = parse_dict_data(fd_priv_data);

                                if let Some(vals) = dict_get(&fd_priv_dict, DictOp::OneByte(20))
                                    && let Some(&v) = vals.first()
                                {
                                    fd_entry.default_width_x = v;
                                }
                                if let Some(vals) = dict_get(&fd_priv_dict, DictOp::OneByte(21))
                                    && let Some(&v) = vals.first()
                                {
                                    fd_entry.nominal_width_x = v;
                                }

                                // FD-level local subrs
                                if let Some(vals) = dict_get(&fd_priv_dict, DictOp::OneByte(19))
                                    && let Some(&v) = vals.first()
                                {
                                    let subr_abs = fd_priv_offset + v as usize;
                                    if subr_abs < data.len() {
                                        let (fd_local, _) = parse_index(data, subr_abs)?;
                                        fd_entry.local_subrs = fd_local;
                                    }
                                }
                            }
                        }

                        font.fd_array.push(fd_entry);
                    }
                }
            }

            // FDSelect (12,37)
            if let Some(vals) = dict_get(&top_dict, DictOp::TwoByte(12, 37))
                && let Some(&v) = vals.first()
            {
                let fds_offset = v as usize;
                if fds_offset < data.len() {
                    font.fd_select = parse_fd_select(data, fds_offset, n_glyphs)?;
                }
            }
        }

        fonts.push(font);
    }

    Ok(fonts)
}

// ---------------------------------------------------------------------------
// INDEX Parsing
// ---------------------------------------------------------------------------

/// Parse a CFF INDEX structure. Returns (list of byte slices, offset after INDEX).
fn parse_index(data: &[u8], offset: usize) -> Result<(Vec<Vec<u8>>, usize), String> {
    if offset + 2 > data.len() {
        return Err("INDEX: truncated count".into());
    }
    let count = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
    let mut pos = offset + 2;

    if count == 0 {
        return Ok((Vec::new(), pos));
    }

    if pos >= data.len() {
        return Err("INDEX: truncated offSize".into());
    }
    let off_size = data[pos] as usize;
    pos += 1;

    if off_size == 0 || off_size > 4 {
        return Err(format!("INDEX: invalid offSize {off_size}"));
    }

    // Read count+1 offsets
    let mut offsets = Vec::with_capacity(count + 1);
    for _ in 0..=count {
        if pos + off_size > data.len() {
            return Err("INDEX: truncated offset".into());
        }
        let val = read_offset(data, pos, off_size);
        offsets.push(val);
        pos += off_size;
    }

    // Data starts at current pos; offsets are 1-based relative to byte before data
    let data_start = pos - 1; // offsets[0] == 1 means first byte of data region
    let mut items = Vec::with_capacity(count);
    for i in 0..count {
        let start = data_start + offsets[i];
        let end = data_start + offsets[i + 1];
        if end > data.len() || start > end {
            return Err("INDEX: data out of bounds".into());
        }
        items.push(data[start..end].to_vec());
    }

    let end_offset = data_start + offsets[count];
    Ok((items, end_offset))
}

/// Read an offset of `off_size` bytes (1–4), big-endian unsigned.
fn read_offset(data: &[u8], offset: usize, off_size: usize) -> usize {
    match off_size {
        1 => data[offset] as usize,
        2 => u16::from_be_bytes([data[offset], data[offset + 1]]) as usize,
        3 => {
            ((data[offset] as usize) << 16)
                | ((data[offset + 1] as usize) << 8)
                | (data[offset + 2] as usize)
        }
        4 => u32::from_be_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]) as usize,
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// DICT Parsing
// ---------------------------------------------------------------------------

/// DICT operator key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DictOp {
    OneByte(u8),
    TwoByte(u8, u8),
}

/// A DICT entry: operator key → operands.
struct DictEntry {
    op: DictOp,
    operands: Vec<f64>,
}

/// Look up an operator's operands in a parsed DICT.
fn dict_get(dict: &[DictEntry], op: DictOp) -> Option<&[f64]> {
    dict.iter()
        .find(|e| e.op == op)
        .map(|e| e.operands.as_slice())
}

/// Parse CFF DICT binary data into a list of entries.
fn parse_dict_data(data: &[u8]) -> Vec<DictEntry> {
    let mut result = Vec::new();
    let mut operands: Vec<f64> = Vec::new();
    let mut i = 0;
    let length = data.len();

    while i < length {
        let b0 = data[i];

        if b0 <= 21 {
            // Operator
            let op = if b0 == 12 {
                i += 1;
                if i >= length {
                    break;
                }
                DictOp::TwoByte(12, data[i])
            } else {
                DictOp::OneByte(b0)
            };
            result.push(DictEntry {
                op,
                operands: std::mem::take(&mut operands),
            });
            i += 1;
        } else if b0 == 28 {
            // 3-byte signed integer
            if i + 2 >= length {
                break;
            }
            let val = i16::from_be_bytes([data[i + 1], data[i + 2]]);
            operands.push(val as f64);
            i += 3;
        } else if b0 == 29 {
            // 5-byte signed integer
            if i + 4 >= length {
                break;
            }
            let val = i32::from_be_bytes([data[i + 1], data[i + 2], data[i + 3], data[i + 4]]);
            operands.push(val as f64);
            i += 5;
        } else if b0 == 30 {
            // BCD real
            i += 1;
            let mut chars = Vec::new();
            while i < length {
                let byte = data[i];
                i += 1;
                let n1 = (byte >> 4) & 0x0F;
                let n2 = byte & 0x0F;

                if !push_bcd_nibble(n1, &mut chars) {
                    break;
                }
                if !push_bcd_nibble(n2, &mut chars) {
                    break;
                }
            }
            let s: String = chars.into_iter().collect();
            operands.push(s.parse::<f64>().unwrap_or(0.0));
        } else if (32..=246).contains(&b0) {
            operands.push((b0 as i32 - 139) as f64);
            i += 1;
        } else if (247..=250).contains(&b0) {
            if i + 1 >= length {
                break;
            }
            let b1 = data[i + 1];
            operands.push(((b0 as i32 - 247) * 256 + b1 as i32 + 108) as f64);
            i += 2;
        } else if (251..=254).contains(&b0) {
            if i + 1 >= length {
                break;
            }
            let b1 = data[i + 1];
            operands.push((-(b0 as i32 - 251) * 256 - b1 as i32 - 108) as f64);
            i += 2;
        } else {
            // Skip unknown bytes (255 not used in DICT data)
            i += 1;
        }
    }

    result
}

/// Push a BCD nibble character. Returns false on end-of-number (0xF).
fn push_bcd_nibble(n: u8, chars: &mut Vec<char>) -> bool {
    match n {
        0..=9 => chars.push((b'0' + n) as char),
        0x0A => chars.push('.'),
        0x0B => chars.push('E'),
        0x0C => {
            chars.push('E');
            chars.push('-');
        }
        0x0E => chars.push('-'),
        0x0F => return false,
        _ => {} // 0x0D reserved
    }
    true
}

// ---------------------------------------------------------------------------
// SID Resolution
// ---------------------------------------------------------------------------

/// Resolve a String ID (SID) to its string.
/// SID 0–390 are predefined standard strings.
/// SID >= 391 indexes into the String INDEX (offset by 391).
pub fn get_sid_string(sid: u16, string_index: &[Vec<u8>]) -> String {
    if (sid as usize) < STANDARD_STRINGS.len() {
        return STANDARD_STRINGS[sid as usize].to_string();
    }
    let idx = sid as usize - STANDARD_STRINGS.len();
    if idx < string_index.len() {
        String::from_utf8_lossy(&string_index[idx]).into_owned()
    } else {
        format!(".sid{sid}")
    }
}

// ---------------------------------------------------------------------------
// Charset Parsing
// ---------------------------------------------------------------------------

/// Parse a charset structure. GID 0 is always `.notdef`.
fn parse_charset(
    data: &[u8],
    offset: usize,
    n_glyphs: usize,
    string_index: &[Vec<u8>],
) -> Result<Vec<String>, String> {
    let mut names = vec![".notdef".to_string()];
    if n_glyphs <= 1 {
        return Ok(names);
    }

    if offset >= data.len() {
        return Err("charset: offset out of bounds".into());
    }
    let fmt = data[offset];
    let mut pos = offset + 1;

    match fmt {
        0 => {
            // Format 0: array of SIDs
            for _ in 0..n_glyphs - 1 {
                if pos + 1 >= data.len() {
                    break;
                }
                let sid = u16::from_be_bytes([data[pos], data[pos + 1]]);
                pos += 2;
                names.push(get_sid_string(sid, string_index));
            }
        }
        1 => {
            // Format 1: ranges with u8 nLeft
            while names.len() < n_glyphs {
                if pos + 2 >= data.len() {
                    break;
                }
                let first_sid = u16::from_be_bytes([data[pos], data[pos + 1]]);
                let n_left = data[pos + 2] as u16;
                pos += 3;
                for sid in first_sid..=first_sid + n_left {
                    if names.len() >= n_glyphs {
                        break;
                    }
                    names.push(get_sid_string(sid, string_index));
                }
            }
        }
        2 => {
            // Format 2: ranges with u16 nLeft
            while names.len() < n_glyphs {
                if pos + 3 >= data.len() {
                    break;
                }
                let first_sid = u16::from_be_bytes([data[pos], data[pos + 1]]);
                let n_left = u16::from_be_bytes([data[pos + 2], data[pos + 3]]);
                pos += 4;
                for sid in first_sid..=first_sid + n_left {
                    if names.len() >= n_glyphs {
                        break;
                    }
                    names.push(get_sid_string(sid, string_index));
                }
            }
        }
        _ => return Err(format!("Unknown charset format: {fmt}")),
    }

    Ok(names)
}

/// Build a CID→GID reverse mapping from a CID-keyed CFF charset.
/// In CID fonts, charset values are CID values. GID 0 always maps to CID 0.
/// Returns a Vec where index = CID and value = GID.
fn build_cid_to_gid(data: &[u8], offset: usize, n_glyphs: usize) -> Result<Vec<u16>, String> {
    // Parse charset to get GID→CID pairs
    let mut gid_to_cid: Vec<u16> = vec![0]; // GID 0 → CID 0
    if n_glyphs <= 1 || offset >= data.len() {
        return Ok(Vec::new());
    }
    let fmt = data[offset];
    let mut pos = offset + 1;
    match fmt {
        0 => {
            for _ in 0..n_glyphs - 1 {
                if pos + 1 >= data.len() {
                    break;
                }
                let cid = u16::from_be_bytes([data[pos], data[pos + 1]]);
                pos += 2;
                gid_to_cid.push(cid);
            }
        }
        1 => {
            while gid_to_cid.len() < n_glyphs {
                if pos + 2 >= data.len() {
                    break;
                }
                let first = u16::from_be_bytes([data[pos], data[pos + 1]]);
                let n_left = data[pos + 2] as u16;
                pos += 3;
                for cid in first..=first + n_left {
                    if gid_to_cid.len() >= n_glyphs {
                        break;
                    }
                    gid_to_cid.push(cid);
                }
            }
        }
        2 => {
            while gid_to_cid.len() < n_glyphs {
                if pos + 3 >= data.len() {
                    break;
                }
                let first = u16::from_be_bytes([data[pos], data[pos + 1]]);
                let n_left = u16::from_be_bytes([data[pos + 2], data[pos + 3]]);
                pos += 4;
                for cid in first..=first + n_left {
                    if gid_to_cid.len() >= n_glyphs {
                        break;
                    }
                    gid_to_cid.push(cid);
                }
            }
        }
        _ => return Err(format!("Unknown charset format: {fmt}")),
    }

    // Find max CID to size the reverse map
    let max_cid = gid_to_cid.iter().copied().max().unwrap_or(0) as usize;
    let mut cid_to_gid = vec![0xFFFF_u16; max_cid + 1];
    for (gid, &cid) in gid_to_cid.iter().enumerate() {
        let cid_idx = cid as usize;
        if cid_idx < cid_to_gid.len() {
            cid_to_gid[cid_idx] = gid as u16;
        }
    }
    Ok(cid_to_gid)
}

/// Return glyph names for a predefined charset ID.
fn get_predefined_charset(
    charset_id: i32,
    n_glyphs: usize,
    string_index: &[Vec<u8>],
) -> Vec<String> {
    let sids: &[u16] = match charset_id {
        0 => &ISO_ADOBE_CHARSET,
        1 => &EXPERT_CHARSET,
        2 => &EXPERT_SUBSET_CHARSET,
        _ => {
            let mut names = vec![".notdef".to_string()];
            for i in 1..n_glyphs {
                names.push(format!(".gid{i}"));
            }
            return names;
        }
    };

    let mut names = vec![".notdef".to_string()];
    for &sid in sids.iter() {
        if names.len() >= n_glyphs {
            break;
        }
        names.push(get_sid_string(sid, string_index));
    }
    while names.len() < n_glyphs {
        names.push(format!(".gid{}", names.len()));
    }
    names
}

// ---------------------------------------------------------------------------
// Encoding Parsing
// ---------------------------------------------------------------------------

/// Parse an encoding structure. Returns 256-element Vec (code → GID).
fn parse_encoding(
    data: &[u8],
    offset: usize,
    charset: &[String],
    string_index: &[Vec<u8>],
) -> Result<Vec<u16>, String> {
    let mut encoding = vec![0u16; 256];

    // Build name→GID lookup
    let name_to_gid: std::collections::HashMap<&str, u16> = charset
        .iter()
        .enumerate()
        .map(|(gid, name)| (name.as_str(), gid as u16))
        .collect();

    if offset >= data.len() {
        return Err("encoding: offset out of bounds".into());
    }
    let raw_format = data[offset];
    let fmt = raw_format & 0x7F;
    let has_supplement = (raw_format & 0x80) != 0;
    let mut pos = offset + 1;

    match fmt {
        0 => {
            if pos >= data.len() {
                return Ok(encoding);
            }
            let n_codes = data[pos] as usize;
            pos += 1;
            for gid_minus_1 in 0..n_codes {
                if pos >= data.len() {
                    break;
                }
                let code = data[pos] as usize;
                pos += 1;
                let gid = (gid_minus_1 + 1) as u16;
                if code < 256 {
                    encoding[code] = gid;
                }
            }
        }
        1 => {
            if pos >= data.len() {
                return Ok(encoding);
            }
            let n_ranges = data[pos] as usize;
            pos += 1;
            let mut gid: u16 = 1;
            for _ in 0..n_ranges {
                if pos + 1 >= data.len() {
                    break;
                }
                let first_code = data[pos] as usize;
                let n_left = data[pos + 1] as usize;
                pos += 2;
                for off in 0..=n_left {
                    let code = first_code + off;
                    if code < 256 {
                        encoding[code] = gid;
                    }
                    gid += 1;
                }
            }
        }
        _ => return Err(format!("Unknown encoding format: {fmt}")),
    }

    // Supplemental encoding
    if has_supplement && pos < data.len() {
        let n_sups = data[pos] as usize;
        pos += 1;
        for _ in 0..n_sups {
            if pos + 2 >= data.len() {
                break;
            }
            let code = data[pos] as usize;
            let sid = u16::from_be_bytes([data[pos + 1], data[pos + 2]]);
            pos += 3;
            let name = get_sid_string(sid, string_index);
            let gid = name_to_gid.get(name.as_str()).copied().unwrap_or(0);
            if code < 256 {
                encoding[code] = gid;
            }
        }
    }

    Ok(encoding)
}

/// Build encoding for predefined encoding IDs (0=Standard, 1=Expert).
fn get_predefined_encoding(
    encoding_id: i32,
    charset: &[String],
    string_index: &[Vec<u8>],
) -> Vec<u16> {
    let mut encoding = vec![0u16; 256];

    let enc_map: &[(u8, u16)] = match encoding_id {
        0 => &STANDARD_ENCODING_MAP,
        1 => &EXPERT_ENCODING_MAP,
        _ => return encoding,
    };

    // Build name→GID from charset
    let name_to_gid: std::collections::HashMap<&str, u16> = charset
        .iter()
        .enumerate()
        .map(|(gid, name)| (name.as_str(), gid as u16))
        .collect();

    // Map: code → SID → name → GID
    for &(code, sid) in enc_map {
        let name = get_sid_string(sid, string_index);
        let gid = name_to_gid.get(name.as_str()).copied().unwrap_or(0);
        encoding[code as usize] = gid;
    }

    encoding
}

// ---------------------------------------------------------------------------
// FDSelect Parsing (CID fonts)
// ---------------------------------------------------------------------------

/// Parse FDSelect structure. Returns GID-indexed list of FD indices.
fn parse_fd_select(data: &[u8], offset: usize, n_glyphs: usize) -> Result<Vec<u8>, String> {
    if offset >= data.len() {
        return Err("FDSelect: offset out of bounds".into());
    }
    let fmt = data[offset];
    let mut pos = offset + 1;

    match fmt {
        0 => {
            // Format 0: one byte per glyph
            if pos + n_glyphs > data.len() {
                return Err("FDSelect format 0: truncated data".into());
            }
            Ok(data[pos..pos + n_glyphs].to_vec())
        }
        3 => {
            // Format 3: ranges
            if pos + 1 >= data.len() {
                return Err("FDSelect format 3: truncated".into());
            }
            let n_ranges = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
            pos += 2;
            let mut fd_select = vec![0u8; n_glyphs];

            for i in 0..n_ranges {
                if pos + 2 >= data.len() {
                    break;
                }
                let first_gid = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
                let fd = data[pos + 2];
                pos += 3;

                let next_first = if i + 1 < n_ranges && pos + 1 < data.len() {
                    u16::from_be_bytes([data[pos], data[pos + 1]]) as usize
                } else if pos + 1 < data.len() {
                    // Sentinel
                    u16::from_be_bytes([data[pos], data[pos + 1]]) as usize
                } else {
                    n_glyphs
                };

                for item in fd_select
                    .iter_mut()
                    .take(next_first.min(n_glyphs))
                    .skip(first_gid)
                {
                    *item = fd;
                }
            }

            Ok(fd_select)
        }
        _ => Err(format!("Unknown FDSelect format: {fmt}")),
    }
}

// ---------------------------------------------------------------------------
// Standard Strings (SID 0..390) — CFF Specification Appendix A
// ---------------------------------------------------------------------------

#[rustfmt::skip]
const STANDARD_STRINGS: [&str; 391] = [
    // SID 0-9
    ".notdef", "space", "exclam", "quotedbl", "numbersign",
    "dollar", "percent", "ampersand", "quoteright", "parenleft",
    // SID 10-19
    "parenright", "asterisk", "plus", "comma", "hyphen",
    "period", "slash", "zero", "one", "two",
    // SID 20-29
    "three", "four", "five", "six", "seven",
    "eight", "nine", "colon", "semicolon", "less",
    // SID 30-39
    "equal", "greater", "question", "at", "A",
    "B", "C", "D", "E", "F",
    // SID 40-49
    "G", "H", "I", "J", "K",
    "L", "M", "N", "O", "P",
    // SID 50-59
    "Q", "R", "S", "T", "U",
    "V", "W", "X", "Y", "Z",
    // SID 60-69
    "bracketleft", "backslash", "bracketright", "asciicircum", "underscore",
    "quoteleft", "a", "b", "c", "d",
    // SID 70-79
    "e", "f", "g", "h", "i",
    "j", "k", "l", "m", "n",
    // SID 80-89
    "o", "p", "q", "r", "s",
    "t", "u", "v", "w", "x",
    // SID 90-99
    "y", "z", "braceleft", "bar", "braceright",
    "asciitilde", "exclamdown", "cent", "sterling", "fraction",
    // SID 100-109
    "yen", "florin", "section", "currency", "quotesingle",
    "quotedblleft", "guillemotleft", "guilsinglleft", "guilsinglright", "fi",
    // SID 110-119
    "fl", "endash", "dagger", "daggerdbl", "periodcentered",
    "paragraph", "bullet", "quotesinglbase", "quotedblbase", "quotedblright",
    // SID 120-129
    "guillemotright", "ellipsis", "perthousand", "questiondown", "grave",
    "acute", "circumflex", "tilde", "macron", "breve",
    // SID 130-139
    "dotaccent", "dieresis", "ring", "cedilla", "hungarumlaut",
    "ogonek", "caron", "emdash", "AE", "ordfeminine",
    // SID 140-149
    "Lslash", "Oslash", "OE", "ordmasculine", "ae",
    "dotlessi", "lslash", "oslash", "oe", "germandbls",
    // SID 150-159
    "onesuperior", "logicalnot", "mu", "trademark", "Eth",
    "onehalf", "plusminus", "Thorn", "onequarter", "divide",
    // SID 160-169
    "brokenbar", "degree", "thorn", "threequarters", "twosuperior",
    "registered", "minus", "eth", "multiply", "threesuperior",
    // SID 170-179
    "copyright", "Aacute", "Acircumflex", "Adieresis", "Agrave",
    "Aring", "Atilde", "Ccedilla", "Eacute", "Ecircumflex",
    // SID 180-189
    "Edieresis", "Egrave", "Iacute", "Icircumflex", "Idieresis",
    "Igrave", "Ntilde", "Oacute", "Ocircumflex", "Odieresis",
    // SID 190-199
    "Ograve", "Otilde", "Scaron", "Uacute", "Ucircumflex",
    "Udieresis", "Ugrave", "Yacute", "Ydieresis", "Zcaron",
    // SID 200-209
    "aacute", "acircumflex", "adieresis", "agrave", "aring",
    "atilde", "ccedilla", "eacute", "ecircumflex", "edieresis",
    // SID 210-219
    "egrave", "iacute", "icircumflex", "idieresis", "igrave",
    "ntilde", "oacute", "ocircumflex", "odieresis", "ograve",
    // SID 220-229
    "otilde", "scaron", "uacute", "ucircumflex", "udieresis",
    "ugrave", "yacute", "ydieresis", "zcaron", "exclamsmall",
    // SID 230-239
    "Hungarumlautsmall", "dollaroldstyle", "dollarsuperior", "ampersandsmall",
    "Acutesmall", "parenleftsuperior", "parenrightsuperior", "twodotenleader",
    "onedotenleader", "zerooldstyle",
    // SID 240-249
    "oneoldstyle", "twooldstyle", "threeoldstyle", "fouroldstyle",
    "fiveoldstyle", "sixoldstyle", "sevenoldstyle", "eightoldstyle",
    "nineoldstyle", "commasuperior",
    // SID 250-259
    "threequartersemdash", "periodsuperior", "questionsmall", "asuperior",
    "bsuperior", "centsuperior", "dsuperior", "esuperior", "isuperior",
    "lsuperior",
    // SID 260-269
    "msuperior", "nsuperior", "osuperior", "rsuperior", "ssuperior",
    "tsuperior", "ff", "ffi", "ffl", "parenleftinferior",
    // SID 270-279
    "parenrightinferior", "Circumflexsmall", "hyphensuperior", "Gravesmall",
    "Asmall", "Bsmall", "Csmall", "Dsmall", "Esmall", "Fsmall",
    // SID 280-289
    "Gsmall", "Hsmall", "Ismall", "Jsmall", "Ksmall",
    "Lsmall", "Msmall", "Nsmall", "Osmall", "Psmall",
    // SID 290-299
    "Qsmall", "Rsmall", "Ssmall", "Tsmall", "Usmall",
    "Vsmall", "Wsmall", "Xsmall", "Ysmall", "Zsmall",
    // SID 300-309
    "colonmonetary", "onefitted", "rupiah", "Tildesmall", "exclamdownsmall",
    "centoldstyle", "Lslashsmall", "Scaronsmall", "Zcaronsmall", "Dieresissmall",
    // SID 310-319
    "Brevesmall", "Caronsmall", "Dotaccentsmall", "Macronsmall", "figuredash",
    "hypheninferior", "Ogoneksmall", "Ringsmall", "Cedillasmall", "questiondownsmall",
    // SID 320-329
    "oneeighth", "threeeighths", "fiveeighths", "seveneighths", "onethird",
    "twothirds", "zerosuperior", "foursuperior", "fivesuperior", "sixsuperior",
    // SID 330-339
    "sevensuperior", "eightsuperior", "ninesuperior", "zeroinferior", "oneinferior",
    "twoinferior", "threeinferior", "fourinferior", "fiveinferior", "sixinferior",
    // SID 340-349
    "seveninferior", "eightinferior", "nineinferior", "centinferior", "dollarinferior",
    "periodinferior", "commainferior", "Agravesmall", "Aacutesmall", "Acircumflexsmall",
    // SID 350-359
    "Atildesmall", "Adieresissmall", "Aringsmall", "AEsmall", "Ccedillasmall",
    "Egravesmall", "Eacutesmall", "Ecircumflexsmall", "Edieresissmall", "Igravesmall",
    // SID 360-369
    "Iacutesmall", "Icircumflexsmall", "Idieresissmall", "Ethsmall", "Ntildesmall",
    "Ogravesmall", "Oacutesmall", "Ocircumflexsmall", "Otildesmall", "Odieresissmall",
    // SID 370-379
    "OEsmall", "Oslashsmall", "Ugravesmall", "Uacutesmall", "Ucircumflexsmall",
    "Udieresissmall", "Yacutesmall", "Thornsmall", "Ydieresissmall",
    "001.000", "001.001",
    // SID 380-390
    "001.002", "001.003", "Black", "Bold", "Book",
    "Light", "Medium", "Regular", "Roman", "Semibold",
];

// ---------------------------------------------------------------------------
// Predefined Charsets
// ---------------------------------------------------------------------------

/// ISOAdobe charset (charset ID 0) — SIDs for GID 1..228
#[rustfmt::skip]
const ISO_ADOBE_CHARSET: [u16; 228] = [
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
    21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40,
    41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60,
    61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80,
    81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95, 96, 97, 98, 99, 100,
    101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112, 113, 114, 115, 116, 117, 118, 119, 120,
    121, 122, 123, 124, 125, 126, 127, 128, 129, 130, 131, 132, 133, 134, 135, 136, 137, 138, 139, 140,
    141, 142, 143, 144, 145, 146, 147, 148, 149, 150, 151, 152, 153, 154, 155, 156, 157, 158, 159, 160,
    161, 162, 163, 164, 165, 166, 167, 168, 169, 170, 171, 172, 173, 174, 175, 176, 177, 178, 179, 180,
    181, 182, 183, 184, 185, 186, 187, 188, 189, 190, 191, 192, 193, 194, 195, 196, 197, 198, 199, 200,
    201, 202, 203, 204, 205, 206, 207, 208, 209, 210, 211, 212, 213, 214, 215, 216, 217, 218, 219, 220,
    221, 222, 223, 224, 225, 226, 227, 228,
];

/// Expert charset (charset ID 1) — SIDs for GID 1..165
#[rustfmt::skip]
const EXPERT_CHARSET: [u16; 165] = [
    1, 229, 230, 231, 232, 233, 234, 235, 236, 237,
    238, 13, 14, 15, 99, 239, 240, 241, 242, 243,
    244, 245, 246, 247, 248, 27, 28, 249, 250, 251,
    252, 253, 254, 255, 256, 257, 258, 259, 260, 261,
    262, 263, 264, 265, 266, 109, 110, 267, 268, 269,
    270, 271, 272, 273, 274, 275, 276, 277, 278, 279,
    280, 281, 282, 283, 284, 285, 286, 287, 288, 289,
    290, 291, 292, 293, 294, 295, 296, 297, 298, 299,
    300, 301, 302, 303, 304, 305, 306, 307, 308, 309,
    310, 311, 312, 313, 314, 315, 316, 317, 318, 158,
    155, 163, 319, 320, 321, 322, 323, 324, 325, 326,
    150, 164, 169, 327, 328, 329, 330, 331, 332, 333,
    334, 335, 336, 337, 338, 339, 340, 341, 342, 343,
    344, 345, 346, 347, 348, 349, 350, 351, 352, 353,
    354, 355, 356, 357, 358, 359, 360, 361, 362, 363,
    364, 365, 366, 367, 368, 369, 370, 371, 372, 373,
    374, 375, 376, 377, 378,
];

/// ExpertSubset charset (charset ID 2) — SIDs for GID 1..86
#[rustfmt::skip]
const EXPERT_SUBSET_CHARSET: [u16; 86] = [
    1, 231, 232, 235, 236, 237, 238, 13, 14, 15,
    99, 239, 240, 241, 242, 243, 244, 245, 246, 247,
    248, 27, 28, 249, 250, 251, 253, 254, 255, 256,
    257, 258, 259, 260, 261, 262, 263, 264, 265, 266,
    109, 110, 267, 268, 269, 270, 272, 300, 301, 302,
    305, 314, 315, 158, 155, 163, 320, 321, 322, 323,
    324, 325, 326, 150, 164, 169, 327, 328, 329, 330,
    331, 332, 333, 334, 335, 336, 337, 338, 339, 340,
    341, 342, 343, 344, 345, 346,
];

// ---------------------------------------------------------------------------
// Predefined Encodings
// ---------------------------------------------------------------------------

/// Standard Encoding — (code, SID) pairs for non-zero entries.
#[rustfmt::skip]
const STANDARD_ENCODING_MAP: [(u8, u16); 149] = [
    (32, 1), (33, 2), (34, 3), (35, 4), (36, 5), (37, 6), (38, 7), (39, 8),
    (40, 9), (41, 10), (42, 11), (43, 12), (44, 13), (45, 14), (46, 15), (47, 16),
    (48, 17), (49, 18), (50, 19), (51, 20), (52, 21), (53, 22), (54, 23), (55, 24),
    (56, 25), (57, 26), (58, 27), (59, 28), (60, 29), (61, 30), (62, 31), (63, 32),
    (64, 33), (65, 34), (66, 35), (67, 36), (68, 37), (69, 38), (70, 39), (71, 40),
    (72, 41), (73, 42), (74, 43), (75, 44), (76, 45), (77, 46), (78, 47), (79, 48),
    (80, 49), (81, 50), (82, 51), (83, 52), (84, 53), (85, 54), (86, 55), (87, 56),
    (88, 57), (89, 58), (90, 59), (91, 60), (92, 61), (93, 62), (94, 63), (95, 64),
    (96, 65), (97, 66), (98, 67), (99, 68), (100, 69), (101, 70), (102, 71),
    (103, 72), (104, 73), (105, 74), (106, 75), (107, 76), (108, 77), (109, 78),
    (110, 79), (111, 80), (112, 81), (113, 82), (114, 83), (115, 84), (116, 85),
    (117, 86), (118, 87), (119, 88), (120, 89), (121, 90), (122, 91), (123, 92),
    (124, 93), (125, 94), (126, 95),
    (161, 96), (162, 97), (163, 98), (164, 99), (165, 100), (166, 101),
    (167, 102), (168, 103), (169, 104), (170, 105), (171, 106), (172, 107),
    (173, 108), (174, 109), (175, 110), (177, 111), (178, 112), (179, 113),
    (180, 114), (182, 115), (183, 116), (184, 117), (185, 118), (186, 119),
    (187, 120), (188, 121), (189, 122), (191, 123), (193, 124), (194, 125),
    (195, 126), (196, 127), (197, 128), (198, 129), (199, 130), (200, 131),
    (202, 132), (203, 133), (205, 134), (206, 135), (207, 136), (208, 137),
    (225, 138), (227, 139), (232, 140), (233, 141), (234, 142), (235, 143),
    (241, 144), (245, 145), (248, 146), (249, 147), (250, 148), (251, 149),
];

/// Expert Encoding — (code, SID) pairs for non-zero entries.
#[rustfmt::skip]
pub const EXPERT_ENCODING_MAP: [(u8, u16); 165] = [
    (32, 1), (33, 229), (34, 230), (36, 231), (37, 232), (38, 233), (39, 234),
    (40, 235), (41, 236), (42, 237), (43, 238), (44, 13), (45, 14), (46, 15),
    (47, 99), (48, 239), (49, 240), (50, 241), (51, 242), (52, 243), (53, 244),
    (54, 245), (55, 246), (56, 247), (57, 248), (58, 27), (59, 28), (60, 249),
    (61, 250), (62, 251), (63, 252), (64, 253), (65, 254), (66, 255), (67, 256),
    (68, 257), (69, 258), (70, 259), (71, 260), (72, 261), (73, 262), (74, 263),
    (75, 264), (76, 265), (77, 266), (78, 109), (79, 110), (80, 267), (81, 268),
    (82, 269), (83, 270), (84, 271), (85, 272), (86, 273), (87, 274), (88, 275),
    (89, 276), (90, 277), (91, 278), (92, 279), (93, 280), (94, 281), (95, 282),
    (96, 283), (97, 284), (98, 285), (99, 286), (100, 287), (101, 288), (102, 289),
    (103, 290), (104, 291), (105, 292), (106, 293), (107, 294), (108, 295),
    (109, 296), (110, 297), (111, 298), (112, 299), (113, 300), (114, 301),
    (115, 302), (116, 303), (117, 304), (118, 305), (119, 306), (120, 307),
    (121, 308), (122, 309), (123, 310), (124, 311), (125, 312), (126, 313),
    (161, 314), (162, 315), (163, 316), (164, 317), (165, 318), (166, 158),
    (167, 155), (168, 163), (169, 319), (170, 320), (171, 321), (172, 322),
    (173, 323), (174, 324), (175, 325), (176, 326), (177, 150), (178, 164),
    (179, 169), (180, 327), (181, 328), (182, 329), (183, 330), (184, 331),
    (185, 332), (186, 333), (187, 334), (188, 335), (189, 336), (190, 337),
    (191, 338), (192, 339), (193, 340), (194, 341), (195, 342), (196, 343),
    (197, 344), (198, 345), (199, 346), (200, 347), (201, 348), (202, 349),
    (203, 350), (204, 351), (205, 352), (206, 353), (207, 354), (208, 355),
    (209, 356), (210, 357), (211, 358), (212, 359), (213, 360), (214, 361),
    (215, 362), (216, 363), (217, 364), (218, 365), (219, 366), (220, 367),
    (221, 368), (222, 369), (223, 370), (224, 371), (225, 372), (226, 373),
    (227, 374), (228, 375), (229, 376), (230, 377), (231, 378),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sid_resolution() {
        let string_index = vec![b"CustomGlyph".to_vec()];
        assert_eq!(get_sid_string(0, &string_index), ".notdef");
        assert_eq!(get_sid_string(34, &string_index), "A");
        assert_eq!(get_sid_string(391, &string_index), "CustomGlyph");
        assert_eq!(get_sid_string(999, &string_index), ".sid999");
    }

    #[test]
    fn test_dict_number_encoding() {
        // 32-246 range: value = b0 - 139
        let data = [139u8, 15]; // operand 0, then operator 15 (charset)
        let entries = parse_dict_data(&data);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].operands, vec![0.0]);

        // 247-250 range
        let data = [247u8, 0, 15]; // (247-247)*256 + 0 + 108 = 108
        let entries = parse_dict_data(&data);
        assert_eq!(entries[0].operands, vec![108.0]);

        // 251-254 range
        let data = [251u8, 0, 15]; // -(251-251)*256 - 0 - 108 = -108
        let entries = parse_dict_data(&data);
        assert_eq!(entries[0].operands, vec![-108.0]);
    }

    #[test]
    fn test_empty_index() {
        // count = 0
        let data = [0u8, 0];
        let (items, off) = parse_index(&data, 0).unwrap();
        assert!(items.is_empty());
        assert_eq!(off, 2);
    }

    #[test]
    fn test_predefined_charset_iso_adobe() {
        let names = get_predefined_charset(0, 5, &[]);
        assert_eq!(names[0], ".notdef");
        assert_eq!(names[1], "space");
        assert_eq!(names[2], "exclam");
        assert_eq!(names.len(), 5);
    }
}
