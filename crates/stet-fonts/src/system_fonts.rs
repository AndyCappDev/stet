// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! System font discovery and caching.
//!
//! Scans platform-specific font directories for installed fonts, extracts
//! PostScript names, and caches the mapping to a JSON file for fast lookups.

use std::collections::HashMap;
use std::fs;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::SystemTime;

use crate::truetype::{find_table, read_u16, read_u32};

/// Global singleton for the system font cache.
static SYSTEM_FONT_CACHE: OnceLock<SystemFontCache> = OnceLock::new();

/// Get or initialize the global system font cache.
pub fn get_system_font_cache() -> &'static SystemFontCache {
    SYSTEM_FONT_CACHE.get_or_init(SystemFontCache::load_or_build)
}

/// Maps PostScript font names to filesystem paths.
pub struct SystemFontCache {
    fonts: HashMap<String, PathBuf>,
}

/// JSON-serializable cache format.
#[derive(serde::Serialize, serde::Deserialize)]
struct CacheData {
    version: u32,
    dir_mtimes: HashMap<String, u64>,
    fonts: HashMap<String, String>,
}

impl SystemFontCache {
    /// Look up a font by PostScript name, returning its file path.
    pub fn get_font_path(&self, ps_name: &str) -> Option<&Path> {
        self.fonts.get(ps_name).map(|p| p.as_path())
    }

    /// Iterate over all cached fonts (PostScript name, file path).
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Path)> {
        self.fonts.iter().map(|(k, v)| (k.as_str(), v.as_path()))
    }

    fn cache_path() -> Option<PathBuf> {
        dirs_cache().map(|d| d.join("stet").join("system_fonts.json"))
    }

    fn load_or_build() -> SystemFontCache {
        // Try loading from cache
        if let Some(cache_path) = Self::cache_path()
            && let Some(cache) = Self::try_load_cache(&cache_path)
        {
            return cache;
        }

        // Build from scratch
        let cache = Self::build();

        // Persist to disk
        if let Some(cache_path) = Self::cache_path() {
            let _ = cache.save(&cache_path);
        }

        cache
    }

    fn try_load_cache(path: &Path) -> Option<SystemFontCache> {
        let data = fs::read_to_string(path).ok()?;
        let cached: CacheData = serde_json::from_str(&data).ok()?;
        if cached.version != 1 {
            return None;
        }

        // Check staleness: compare directory mtimes
        let current_mtimes = get_font_dir_mtimes();
        for (dir, &cached_mtime) in &cached.dir_mtimes {
            match current_mtimes.get(dir.as_str()) {
                Some(&current) if current == cached_mtime => {}
                _ => return None, // stale
            }
        }
        // Also check if any new dirs appeared
        for dir in current_mtimes.keys() {
            if !cached.dir_mtimes.contains_key(*dir) {
                return None;
            }
        }

        let fonts = cached
            .fonts
            .into_iter()
            .map(|(k, v)| (k, PathBuf::from(v)))
            .collect();
        Some(SystemFontCache { fonts })
    }

    fn build() -> SystemFontCache {
        let mut fonts = HashMap::new();

        for dir in font_directories() {
            let dir_path = Path::new(dir);
            if dir_path.is_dir() {
                scan_directory(dir_path, &mut fonts);
            }
        }
        for dir_path in home_font_directories() {
            if dir_path.is_dir() {
                scan_directory(&dir_path, &mut fonts);
            }
        }

        SystemFontCache { fonts }
    }

    fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let dir_mtimes = get_font_dir_mtimes();
        let cache_data = CacheData {
            version: 1,
            dir_mtimes: dir_mtimes
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            fonts: self
                .fonts
                .iter()
                .map(|(k, v)| (k.clone(), v.to_string_lossy().to_string()))
                .collect(),
        };

        let json = serde_json::to_string_pretty(&cache_data)?;
        fs::write(path, json)
    }
}

/// Platform-specific font directories.
fn font_directories() -> Vec<&'static str> {
    if cfg!(target_os = "macos") {
        vec!["/System/Library/Fonts", "/Library/Fonts"]
    } else if cfg!(target_os = "windows") {
        vec!["C:\\Windows\\Fonts"]
    } else {
        // Linux
        vec!["/usr/share/fonts", "/usr/local/share/fonts"]
    }
}

/// Home-relative font directories (resolved at runtime).
fn home_font_directories() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        if cfg!(target_os = "macos") {
            dirs.push(home.join("Library/Fonts"));
        } else if !cfg!(target_os = "windows") {
            dirs.push(home.join(".local/share/fonts"));
            dirs.push(home.join(".fonts"));
        }
    }
    dirs
}

/// Get the cache directory (~/.cache on Linux/macOS).
fn dirs_cache() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache"))
}

/// Collect directory mtimes for staleness checking.
fn get_font_dir_mtimes() -> HashMap<&'static str, u64> {
    let mut mtimes = HashMap::new();
    for dir in font_directories() {
        if let Ok(meta) = fs::metadata(dir)
            && let Ok(mtime) = meta.modified()
            && let Ok(dur) = mtime.duration_since(SystemTime::UNIX_EPOCH)
        {
            mtimes.insert(dir, dur.as_secs());
        }
    }
    mtimes
}

/// Recursively scan a directory for font files.
fn scan_directory(dir: &Path, fonts: &mut HashMap<String, PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_directory(&path, fonts);
            continue;
        }

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());

        match ext.as_deref() {
            Some("ttf" | "otf") => {
                if let Some(name) = extract_ps_name_from_sfnt(&path) {
                    fonts.entry(name).or_insert_with(|| path.clone());
                }
            }
            Some("ttc") => {
                // TrueType Collection: index each sub-font
                if let Ok(data) = fs::read(&path)
                    && data.len() > 12
                    && &data[0..4] == b"ttcf"
                {
                    let num = read_u32(&data, 8) as usize;
                    for i in 0..num {
                        let off_pos = 12 + i * 4;
                        if off_pos + 4 > data.len() {
                            break;
                        }
                        let font_off = read_u32(&data, off_pos) as usize;
                        if let Some(name) = extract_ps_name_at_ttc_offset(&data, font_off) {
                            fonts.entry(name).or_insert_with(|| path.clone());
                        }
                    }
                }
            }
            Some("pfa" | "t1") => {
                if let Some(name) = extract_ps_name_from_pfa(&path) {
                    fonts.entry(name).or_insert_with(|| path.clone());
                }
            }
            Some("pfb") => {
                if let Some(name) = extract_ps_name_from_pfb(&path) {
                    fonts.entry(name).or_insert_with(|| path.clone());
                }
            }
            _ => {}
        }
    }
}

/// Extract PostScript name from a .pfa or .t1 file (first 4KB).
fn extract_ps_name_from_pfa(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file);
    let mut buf = String::new();
    let mut bytes_read = 0usize;

    while bytes_read < 4096 {
        buf.clear();
        let n = reader.read_line(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        bytes_read += n;
        if let Some(name) = parse_fontname_line(&buf) {
            return Some(name);
        }
    }
    None
}

/// Extract PostScript name from a .pfb file.
/// PFB segments: 0x80 <type:u8> <length:u32_le> <data...>
fn extract_ps_name_from_pfb(path: &Path) -> Option<String> {
    let data = fs::read(path).ok()?;
    let mut offset = 0;
    let mut ascii_data = Vec::new();

    // Collect ASCII segments (type 1) until we find the name
    while offset + 6 <= data.len() && data[offset] == 0x80 {
        let seg_type = data[offset + 1];
        let seg_len = u32::from_le_bytes([
            data[offset + 2],
            data[offset + 3],
            data[offset + 4],
            data[offset + 5],
        ]) as usize;
        offset += 6;

        if seg_type == 1 {
            // ASCII segment
            let end = (offset + seg_len).min(data.len());
            ascii_data.extend_from_slice(&data[offset..end]);

            // Check if we have enough data
            if let Some(name) = find_fontname_in_bytes(&ascii_data) {
                return Some(name);
            }
            if ascii_data.len() > 4096 {
                break;
            }
        } else if seg_type == 3 {
            break; // EOF marker
        }

        offset += seg_len;
    }
    None
}

/// Extract PostScript name from OTF/TTF using the `name` table (nameID 6).
/// Extract PostScript name from a sub-font within a TTC at a given offset.
fn extract_ps_name_at_ttc_offset(data: &[u8], font_offset: usize) -> Option<String> {
    if font_offset + 12 > data.len() {
        return None;
    }
    // Check for OTTO (CFF) or regular TrueType
    let is_otto = &data[font_offset..font_offset + 4] == b"OTTO";

    // Find the 'name' table in this sub-font's table directory
    let num_tables = read_u16(data, font_offset + 4) as usize;
    for i in 0..num_tables {
        let entry = font_offset + 12 + i * 16;
        if entry + 16 > data.len() {
            break;
        }
        let tag = &data[entry..entry + 4];
        if tag == b"name" {
            let tbl_off = read_u32(data, entry + 8) as usize;
            let tbl_len = read_u32(data, entry + 12) as usize;
            if tbl_off + tbl_len <= data.len() {
                return extract_ps_name_from_name_table_data(&data[tbl_off..tbl_off + tbl_len]);
            }
        }
        // For CFF fonts, also try the CFF Name INDEX
        if is_otto && tag == b"CFF " {
            let cff_off = read_u32(data, entry + 8) as usize;
            let cff_len = read_u32(data, entry + 12) as usize;
            if cff_off + cff_len <= data.len()
                && let Some(name) = extract_cff_name_from_data(&data[cff_off..cff_off + cff_len])
            {
                return Some(name);
            }
        }
    }
    None
}

/// Extract PostScript name from raw 'name' table data.
fn extract_ps_name_from_name_table_data(name_data: &[u8]) -> Option<String> {
    if name_data.len() < 6 {
        return None;
    }
    let count = read_u16(name_data, 2) as usize;
    let string_offset = read_u16(name_data, 4) as usize;
    for i in 0..count {
        let rec = 6 + i * 12;
        if rec + 12 > name_data.len() {
            break;
        }
        let platform_id = read_u16(name_data, rec);
        let name_id = read_u16(name_data, rec + 6);
        let length = read_u16(name_data, rec + 8) as usize;
        let str_off = read_u16(name_data, rec + 10) as usize;
        if name_id == 6 {
            let start = string_offset + str_off;
            if start + length <= name_data.len() {
                let raw = &name_data[start..start + length];
                if platform_id == 3 || platform_id == 0 {
                    // UTF-16BE
                    let s: String = raw
                        .chunks(2)
                        .filter_map(|c| {
                            if c.len() == 2 {
                                char::from_u32(u16::from_be_bytes([c[0], c[1]]) as u32)
                            } else {
                                None
                            }
                        })
                        .collect();
                    if !s.is_empty() {
                        return Some(s);
                    }
                } else {
                    let s = String::from_utf8_lossy(raw).to_string();
                    if !s.is_empty() {
                        return Some(s);
                    }
                }
            }
        }
    }
    None
}

/// Extract font name from raw CFF data's Name INDEX.
fn extract_cff_name_from_data(cff: &[u8]) -> Option<String> {
    if cff.len() < 4 {
        return None;
    }
    let hdr_size = cff[2] as usize;
    if hdr_size + 3 > cff.len() {
        return None;
    }
    let count = u16::from_be_bytes([cff[hdr_size], cff[hdr_size + 1]]) as usize;
    if count == 0 {
        return None;
    }
    let off_size = cff[hdr_size + 2] as usize;
    if off_size == 0 || off_size > 4 {
        return None;
    }
    let offsets_start = hdr_size + 3;
    let read_off = |idx: usize| -> usize {
        let pos = offsets_start + idx * off_size;
        let mut val = 0u32;
        for j in 0..off_size {
            if pos + j < cff.len() {
                val = (val << 8) | cff[pos + j] as u32;
            }
        }
        val as usize
    };
    let data_start = offsets_start + (count + 1) * off_size;
    let off1 = read_off(0);
    let off2 = read_off(1);
    let start = data_start + off1 - 1;
    let end = data_start + off2 - 1;
    if start < cff.len() && end <= cff.len() && end > start {
        return Some(String::from_utf8_lossy(&cff[start..end]).to_string());
    }
    None
}

fn extract_ps_name_from_sfnt(path: &Path) -> Option<String> {
    let data = fs::read(path).ok()?;
    if data.len() < 12 {
        return None;
    }

    // Check if this is an OTF with CFF — try CFF Name INDEX first
    if &data[0..4] == b"OTTO"
        && let Some(name) = extract_ps_name_from_cff_table(&data)
    {
        return Some(name);
    }

    // Fall back to name table
    extract_ps_name_from_name_table(&data)
}

/// Extract PostScript name from CFF Name INDEX (for OTF+CFF files).
fn extract_ps_name_from_cff_table(font_data: &[u8]) -> Option<String> {
    let (cff_offset, cff_length) = find_table(font_data, b"CFF ")?;
    if cff_offset + cff_length > font_data.len() {
        return None;
    }
    let cff = &font_data[cff_offset..cff_offset + cff_length];

    // CFF header: major(1) minor(1) hdrSize(1) offSize(1)
    if cff.len() < 4 {
        return None;
    }
    let hdr_size = cff[2] as usize;

    // Name INDEX starts right after header
    if hdr_size >= cff.len() {
        return None;
    }
    let name_idx_offset = hdr_size;

    // Parse INDEX: count(2) offSize(1) offset[count+1](offSize each) data...
    if name_idx_offset + 3 > cff.len() {
        return None;
    }
    let count = u16::from_be_bytes([cff[name_idx_offset], cff[name_idx_offset + 1]]) as usize;
    if count == 0 {
        return None;
    }
    let off_size = cff[name_idx_offset + 2] as usize;
    if off_size == 0 || off_size > 4 {
        return None;
    }

    // Read first two offsets to get the first name
    let offsets_start = name_idx_offset + 3;
    let read_offset = |idx: usize| -> Option<usize> {
        let pos = offsets_start + idx * off_size;
        if pos + off_size > cff.len() {
            return None;
        }
        let mut val = 0u32;
        for b in 0..off_size {
            val = (val << 8) | cff[pos + b] as u32;
        }
        Some(val as usize)
    };

    let off1 = read_offset(0)?;
    let off2 = read_offset(1)?;
    let data_start = offsets_start + (count + 1) * off_size;
    let start = data_start + off1 - 1; // offsets are 1-based
    let end = data_start + off2 - 1;

    if end > cff.len() || start >= end {
        return None;
    }

    String::from_utf8(cff[start..end].to_vec()).ok()
}

/// Extract PostScript name from the `name` table (nameID 6).
pub fn extract_ps_name_from_name_table(font_data: &[u8]) -> Option<String> {
    let (name_off, name_len) = find_table(font_data, b"name")?;
    if name_off + name_len > font_data.len() || name_len < 6 {
        return None;
    }
    let name_table = &font_data[name_off..name_off + name_len];

    let count = read_u16(name_table, 2) as usize;
    let string_offset = read_u16(name_table, 4) as usize;

    // Prefer platform 3 (Windows), then platform 1 (Mac)
    let mut win_result: Option<String> = None;
    let mut mac_result: Option<String> = None;

    for i in 0..count {
        let rec_off = 6 + i * 12;
        if rec_off + 12 > name_table.len() {
            break;
        }

        let platform_id = read_u16(name_table, rec_off);
        let encoding_id = read_u16(name_table, rec_off + 2);
        let name_id = read_u16(name_table, rec_off + 6);
        let length = read_u16(name_table, rec_off + 8) as usize;
        let offset = read_u16(name_table, rec_off + 10) as usize;

        if name_id != 6 {
            continue;
        }

        let data_start = string_offset + offset;
        if data_start + length > name_table.len() {
            continue;
        }
        let data = &name_table[data_start..data_start + length];

        if platform_id == 3 && encoding_id == 1 && win_result.is_none() {
            // Windows Unicode BMP — UTF-16BE
            win_result = decode_utf16be(data);
        } else if platform_id == 1 && encoding_id == 0 && mac_result.is_none() {
            // Mac Roman — treat as latin-1
            mac_result = Some(data.iter().map(|&b| b as char).collect());
        }
    }

    win_result.or(mac_result)
}

/// Decode UTF-16BE bytes to a String.
fn decode_utf16be(data: &[u8]) -> Option<String> {
    if !data.len().is_multiple_of(2) {
        return None;
    }
    let units: Vec<u16> = data
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16(&units).ok()
}

/// Parse a `/FontName /SomeName` line.
fn parse_fontname_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if let Some(rest) = trimmed.strip_prefix("/FontName") {
        let rest = rest.trim_start();
        if let Some(name) = rest.strip_prefix('/') {
            // Name ends at whitespace or special chars
            let end = name
                .find(|c: char| c.is_whitespace() || c == '/' || c == '{' || c == '(')
                .unwrap_or(name.len());
            if end > 0 {
                return Some(name[..end].to_string());
            }
        }
    }
    None
}

/// Find `/FontName /SomeName` in raw bytes.
fn find_fontname_in_bytes(data: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(data);
    for line in text.lines() {
        if let Some(name) = parse_fontname_line(line) {
            return Some(name);
        }
    }
    None
}

/// Parse the `cmap` table to build a Unicode→GID mapping.
///
/// Returns a map from Unicode codepoint to glyph ID.
/// Supports format 4 (BMP) and format 12 (full Unicode).
pub fn parse_cmap_table(font_data: &[u8]) -> Option<HashMap<u32, u16>> {
    let (cmap_off, cmap_len) = find_table(font_data, b"cmap")?;
    if cmap_off + cmap_len > font_data.len() {
        return None;
    }
    let cmap = &font_data[cmap_off..];

    let num_subtables = read_u16(cmap, 2) as usize;

    // Find best subtable: prefer format 12 (platform 3/encoding 10), then format 4
    let mut format4_offset: Option<usize> = None;
    let mut format12_offset: Option<usize> = None;

    for i in 0..num_subtables {
        let rec = 4 + i * 8;
        if rec + 8 > cmap_len {
            break;
        }
        let platform = read_u16(cmap, rec);
        let encoding = read_u16(cmap, rec + 2);
        let offset = read_u32(cmap, rec + 4) as usize;

        if offset + 2 > cmap_len {
            continue;
        }
        let format = read_u16(cmap, offset);

        if platform == 3 && encoding == 10 && format == 12 {
            format12_offset = Some(offset);
        } else if platform == 3 && encoding == 1 && format == 4 && format4_offset.is_none() {
            format4_offset = Some(offset);
        }
    }

    // Try format 12 first
    if let Some(off) = format12_offset
        && let Some(map) = parse_cmap_format12(cmap, off)
    {
        return Some(map);
    }

    // Fall back to format 4
    if let Some(off) = format4_offset {
        return parse_cmap_format4(cmap, off);
    }

    None
}

/// Parse cmap format 4 (BMP segmented mapping).
fn parse_cmap_format4(cmap: &[u8], offset: usize) -> Option<HashMap<u32, u16>> {
    if offset + 14 > cmap.len() {
        return None;
    }
    let seg_count_x2 = read_u16(cmap, offset + 6) as usize;
    let seg_count = seg_count_x2 / 2;

    let end_codes_off = offset + 14;
    let start_codes_off = end_codes_off + seg_count_x2 + 2; // +2 for reservedPad
    let id_delta_off = start_codes_off + seg_count_x2;
    let id_range_off = id_delta_off + seg_count_x2;

    if id_range_off + seg_count_x2 > cmap.len() {
        return None;
    }

    let mut map = HashMap::new();

    for i in 0..seg_count {
        let end_code = read_u16(cmap, end_codes_off + i * 2) as u32;
        let start_code = read_u16(cmap, start_codes_off + i * 2) as u32;
        let id_delta = read_u16(cmap, id_delta_off + i * 2) as i16;
        let id_range_offset = read_u16(cmap, id_range_off + i * 2) as usize;

        if start_code == 0xFFFF {
            break;
        }

        for code in start_code..=end_code {
            let gid = if id_range_offset == 0 {
                (code as i32 + id_delta as i32) as u16
            } else {
                let glyph_off =
                    id_range_off + i * 2 + id_range_offset + (code - start_code) as usize * 2;
                if glyph_off + 2 > cmap.len() {
                    continue;
                }
                let gid = read_u16(cmap, glyph_off);
                if gid == 0 {
                    0
                } else {
                    (gid as i32 + id_delta as i32) as u16
                }
            };
            if gid != 0 {
                map.insert(code, gid);
            }
        }
    }

    Some(map)
}

/// Parse cmap format 12 (full Unicode segmented coverage).
fn parse_cmap_format12(cmap: &[u8], offset: usize) -> Option<HashMap<u32, u16>> {
    if offset + 16 > cmap.len() {
        return None;
    }
    let num_groups = read_u32(cmap, offset + 12) as usize;
    let groups_off = offset + 16;

    if groups_off + num_groups * 12 > cmap.len() {
        return None;
    }

    let mut map = HashMap::new();

    for i in 0..num_groups {
        let g = groups_off + i * 12;
        let start_code = read_u32(cmap, g);
        let end_code = read_u32(cmap, g + 4);
        let start_gid = read_u32(cmap, g + 8);

        for code in start_code..=end_code {
            let gid = (start_gid + (code - start_code)) as u16;
            if gid != 0 {
                map.insert(code, gid);
            }
        }
    }

    Some(map)
}

/// Parse the `post` table to get GID→glyph name mapping.
///
/// Only handles format 2.0 (the common format with custom names).
/// Returns None for other formats (caller should use AGL fallback).
pub fn parse_post_table(font_data: &[u8]) -> Option<HashMap<u16, String>> {
    let (post_off, post_len) = find_table(font_data, b"post")?;
    if post_off + post_len > font_data.len() || post_len < 34 {
        return None;
    }
    let post = &font_data[post_off..post_off + post_len];

    // Format is a Fixed (16.16): check for 2.0
    let format_major = read_u16(post, 0);
    let format_minor = read_u16(post, 2);
    if format_major != 2 || format_minor != 0 {
        return None; // Only handle format 2.0
    }

    let num_glyphs = read_u16(post, 32) as usize;
    if 34 + num_glyphs * 2 > post.len() {
        return None;
    }

    // Read glyph name index array
    let mut name_indices = Vec::with_capacity(num_glyphs);
    for i in 0..num_glyphs {
        name_indices.push(read_u16(post, 34 + i * 2));
    }

    // Read Pascal strings for indices >= 258
    let mut extra_names = Vec::new();
    let mut offset = 34 + num_glyphs * 2;
    while offset < post.len() {
        let str_len = post[offset] as usize;
        offset += 1;
        if offset + str_len > post.len() {
            break;
        }
        let name = String::from_utf8_lossy(&post[offset..offset + str_len]).to_string();
        extra_names.push(name);
        offset += str_len;
    }

    let mut map = HashMap::new();
    for (gid, &idx) in name_indices.iter().enumerate() {
        let name = if (idx as usize) < MAC_GLYPH_NAMES.len() {
            MAC_GLYPH_NAMES[idx as usize].to_string()
        } else {
            let extra_idx = idx as usize - 258;
            if extra_idx < extra_names.len() {
                extra_names[extra_idx].clone()
            } else {
                continue;
            }
        };
        if name != ".notdef" {
            map.insert(gid as u16, name);
        }
    }

    Some(map)
}

/// Standard Macintosh glyph names (first 258 entries in post format 2.0).
static MAC_GLYPH_NAMES: &[&str] = &[
    ".notdef",
    ".null",
    "nonmarkingreturn",
    "space",
    "exclam",
    "quotedbl",
    "numbersign",
    "dollar",
    "percent",
    "ampersand",
    "quotesingle",
    "parenleft",
    "parenright",
    "asterisk",
    "plus",
    "comma",
    "hyphen",
    "period",
    "slash",
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "colon",
    "semicolon",
    "less",
    "equal",
    "greater",
    "question",
    "at",
    "A",
    "B",
    "C",
    "D",
    "E",
    "F",
    "G",
    "H",
    "I",
    "J",
    "K",
    "L",
    "M",
    "N",
    "O",
    "P",
    "Q",
    "R",
    "S",
    "T",
    "U",
    "V",
    "W",
    "X",
    "Y",
    "Z",
    "bracketleft",
    "backslash",
    "bracketright",
    "asciicircum",
    "underscore",
    "grave",
    "a",
    "b",
    "c",
    "d",
    "e",
    "f",
    "g",
    "h",
    "i",
    "j",
    "k",
    "l",
    "m",
    "n",
    "o",
    "p",
    "q",
    "r",
    "s",
    "t",
    "u",
    "v",
    "w",
    "x",
    "y",
    "z",
    "braceleft",
    "bar",
    "braceright",
    "asciitilde",
    "Adieresis",
    "Aring",
    "Ccedilla",
    "Eacute",
    "Ntilde",
    "Odieresis",
    "Udieresis",
    "aacute",
    "agrave",
    "acircumflex",
    "adieresis",
    "atilde",
    "aring",
    "ccedilla",
    "eacute",
    "egrave",
    "ecircumflex",
    "edieresis",
    "iacute",
    "igrave",
    "icircumflex",
    "idieresis",
    "ntilde",
    "oacute",
    "ograve",
    "ocircumflex",
    "odieresis",
    "otilde",
    "uacute",
    "ugrave",
    "ucircumflex",
    "udieresis",
    "dagger",
    "degree",
    "cent",
    "sterling",
    "section",
    "bullet",
    "paragraph",
    "germandbls",
    "registered",
    "copyright",
    "trademark",
    "acute",
    "dieresis",
    "notequal",
    "AE",
    "Oslash",
    "infinity",
    "plusminus",
    "lessequal",
    "greaterequal",
    "yen",
    "mu",
    "partialdiff",
    "summation",
    "product",
    "pi",
    "integral",
    "ordfeminine",
    "ordmasculine",
    "Omega",
    "ae",
    "oslash",
    "questiondown",
    "exclamdown",
    "logicalnot",
    "radical",
    "florin",
    "approxequal",
    "Delta",
    "guillemotleft",
    "guillemotright",
    "ellipsis",
    "nonbreakingspace",
    "Agrave",
    "Atilde",
    "Otilde",
    "OE",
    "oe",
    "endash",
    "emdash",
    "quotedblleft",
    "quotedblright",
    "quoteleft",
    "quoteright",
    "divide",
    "lozenge",
    "ydieresis",
    "Ydieresis",
    "fraction",
    "currency",
    "guilsinglleft",
    "guilsinglright",
    "fi",
    "fl",
    "daggerdbl",
    "periodcentered",
    "quotesinglbase",
    "quotedblbase",
    "perthousand",
    "Acircumflex",
    "Ecircumflex",
    "Aacute",
    "Edieresis",
    "Egrave",
    "Iacute",
    "Icircumflex",
    "Idieresis",
    "Igrave",
    "Oacute",
    "Ocircumflex",
    "apple",
    "Ograve",
    "Uacute",
    "Ucircumflex",
    "Ugrave",
    "dotlessi",
    "circumflex",
    "tilde",
    "macron",
    "breve",
    "dotaccent",
    "ring",
    "cedilla",
    "hungarumlaut",
    "ogonek",
    "caron",
    "Lslash",
    "lslash",
    "Scaron",
    "scaron",
    "Zcaron",
    "zcaron",
    "brokenbar",
    "Eth",
    "eth",
    "Yacute",
    "yacute",
    "Thorn",
    "thorn",
    "minus",
    "multiply",
    "onesuperior",
    "twosuperior",
    "threesuperior",
    "onehalf",
    "onequarter",
    "threequarters",
    "franc",
    "Gbreve",
    "gbreve",
    "Idotaccent",
    "Scedilla",
    "scedilla",
    "Cacute",
    "cacute",
    "Ccaron",
    "ccaron",
    "dcroat",
];

/// Adobe Glyph List: Unicode codepoint → PostScript glyph name.
/// Covers the most commonly needed Latin characters (0x0000–0x00FF range).
pub fn unicode_to_glyph_name(codepoint: u32) -> Option<&'static str> {
    AGL_MAP
        .binary_search_by_key(&codepoint, |&(cp, _)| cp)
        .ok()
        .map(|idx| AGL_MAP[idx].1)
}

/// Core AGL entries (Unicode → glyph name) for the 0x00–0xFF range.
/// Sorted by codepoint for binary search.
static AGL_MAP: &[(u32, &str)] = &[
    (0x0020, "space"),
    (0x0021, "exclam"),
    (0x0022, "quotedbl"),
    (0x0023, "numbersign"),
    (0x0024, "dollar"),
    (0x0025, "percent"),
    (0x0026, "ampersand"),
    (0x0027, "quotesingle"),
    (0x0028, "parenleft"),
    (0x0029, "parenright"),
    (0x002A, "asterisk"),
    (0x002B, "plus"),
    (0x002C, "comma"),
    (0x002D, "hyphen"),
    (0x002E, "period"),
    (0x002F, "slash"),
    (0x0030, "zero"),
    (0x0031, "one"),
    (0x0032, "two"),
    (0x0033, "three"),
    (0x0034, "four"),
    (0x0035, "five"),
    (0x0036, "six"),
    (0x0037, "seven"),
    (0x0038, "eight"),
    (0x0039, "nine"),
    (0x003A, "colon"),
    (0x003B, "semicolon"),
    (0x003C, "less"),
    (0x003D, "equal"),
    (0x003E, "greater"),
    (0x003F, "question"),
    (0x0040, "at"),
    (0x0041, "A"),
    (0x0042, "B"),
    (0x0043, "C"),
    (0x0044, "D"),
    (0x0045, "E"),
    (0x0046, "F"),
    (0x0047, "G"),
    (0x0048, "H"),
    (0x0049, "I"),
    (0x004A, "J"),
    (0x004B, "K"),
    (0x004C, "L"),
    (0x004D, "M"),
    (0x004E, "N"),
    (0x004F, "O"),
    (0x0050, "P"),
    (0x0051, "Q"),
    (0x0052, "R"),
    (0x0053, "S"),
    (0x0054, "T"),
    (0x0055, "U"),
    (0x0056, "V"),
    (0x0057, "W"),
    (0x0058, "X"),
    (0x0059, "Y"),
    (0x005A, "Z"),
    (0x005B, "bracketleft"),
    (0x005C, "backslash"),
    (0x005D, "bracketright"),
    (0x005E, "asciicircum"),
    (0x005F, "underscore"),
    (0x0060, "grave"),
    (0x0061, "a"),
    (0x0062, "b"),
    (0x0063, "c"),
    (0x0064, "d"),
    (0x0065, "e"),
    (0x0066, "f"),
    (0x0067, "g"),
    (0x0068, "h"),
    (0x0069, "i"),
    (0x006A, "j"),
    (0x006B, "k"),
    (0x006C, "l"),
    (0x006D, "m"),
    (0x006E, "n"),
    (0x006F, "o"),
    (0x0070, "p"),
    (0x0071, "q"),
    (0x0072, "r"),
    (0x0073, "s"),
    (0x0074, "t"),
    (0x0075, "u"),
    (0x0076, "v"),
    (0x0077, "w"),
    (0x0078, "x"),
    (0x0079, "y"),
    (0x007A, "z"),
    (0x007B, "braceleft"),
    (0x007C, "bar"),
    (0x007D, "braceright"),
    (0x007E, "asciitilde"),
    (0x00A0, "nonbreakingspace"),
    (0x00A1, "exclamdown"),
    (0x00A2, "cent"),
    (0x00A3, "sterling"),
    (0x00A4, "currency"),
    (0x00A5, "yen"),
    (0x00A6, "brokenbar"),
    (0x00A7, "section"),
    (0x00A8, "dieresis"),
    (0x00A9, "copyright"),
    (0x00AA, "ordfeminine"),
    (0x00AB, "guillemotleft"),
    (0x00AC, "logicalnot"),
    (0x00AD, "softhyphen"),
    (0x00AE, "registered"),
    (0x00AF, "macron"),
    (0x00B0, "degree"),
    (0x00B1, "plusminus"),
    (0x00B2, "twosuperior"),
    (0x00B3, "threesuperior"),
    (0x00B4, "acute"),
    (0x00B5, "mu"),
    (0x00B6, "paragraph"),
    (0x00B7, "periodcentered"),
    (0x00B8, "cedilla"),
    (0x00B9, "onesuperior"),
    (0x00BA, "ordmasculine"),
    (0x00BB, "guillemotright"),
    (0x00BC, "onequarter"),
    (0x00BD, "onehalf"),
    (0x00BE, "threequarters"),
    (0x00BF, "questiondown"),
    (0x00C0, "Agrave"),
    (0x00C1, "Aacute"),
    (0x00C2, "Acircumflex"),
    (0x00C3, "Atilde"),
    (0x00C4, "Adieresis"),
    (0x00C5, "Aring"),
    (0x00C6, "AE"),
    (0x00C7, "Ccedilla"),
    (0x00C8, "Egrave"),
    (0x00C9, "Eacute"),
    (0x00CA, "Ecircumflex"),
    (0x00CB, "Edieresis"),
    (0x00CC, "Igrave"),
    (0x00CD, "Iacute"),
    (0x00CE, "Icircumflex"),
    (0x00CF, "Idieresis"),
    (0x00D0, "Eth"),
    (0x00D1, "Ntilde"),
    (0x00D2, "Ograve"),
    (0x00D3, "Oacute"),
    (0x00D4, "Ocircumflex"),
    (0x00D5, "Otilde"),
    (0x00D6, "Odieresis"),
    (0x00D7, "multiply"),
    (0x00D8, "Oslash"),
    (0x00D9, "Ugrave"),
    (0x00DA, "Uacute"),
    (0x00DB, "Ucircumflex"),
    (0x00DC, "Udieresis"),
    (0x00DD, "Yacute"),
    (0x00DE, "Thorn"),
    (0x00DF, "germandbls"),
    (0x00E0, "agrave"),
    (0x00E1, "aacute"),
    (0x00E2, "acircumflex"),
    (0x00E3, "atilde"),
    (0x00E4, "adieresis"),
    (0x00E5, "aring"),
    (0x00E6, "ae"),
    (0x00E7, "ccedilla"),
    (0x00E8, "egrave"),
    (0x00E9, "eacute"),
    (0x00EA, "ecircumflex"),
    (0x00EB, "edieresis"),
    (0x00EC, "igrave"),
    (0x00ED, "iacute"),
    (0x00EE, "icircumflex"),
    (0x00EF, "idieresis"),
    (0x00F0, "eth"),
    (0x00F1, "ntilde"),
    (0x00F2, "ograve"),
    (0x00F3, "oacute"),
    (0x00F4, "ocircumflex"),
    (0x00F5, "otilde"),
    (0x00F6, "odieresis"),
    (0x00F7, "divide"),
    (0x00F8, "oslash"),
    (0x00F9, "ugrave"),
    (0x00FA, "uacute"),
    (0x00FB, "ucircumflex"),
    (0x00FC, "udieresis"),
    (0x00FD, "yacute"),
    (0x00FE, "thorn"),
    (0x00FF, "ydieresis"),
    // Common ligatures and extras
    (0x0131, "dotlessi"),
    (0x0141, "Lslash"),
    (0x0142, "lslash"),
    (0x0152, "OE"),
    (0x0153, "oe"),
    (0x0160, "Scaron"),
    (0x0161, "scaron"),
    (0x0178, "Ydieresis"),
    (0x017D, "Zcaron"),
    (0x017E, "zcaron"),
    (0x0192, "florin"),
    (0x02C6, "circumflex"),
    (0x02C7, "caron"),
    (0x02D8, "breve"),
    (0x02D9, "dotaccent"),
    (0x02DA, "ring"),
    (0x02DB, "ogonek"),
    (0x02DC, "tilde"),
    (0x02DD, "hungarumlaut"),
    (0x2013, "endash"),
    (0x2014, "emdash"),
    (0x2018, "quoteleft"),
    (0x2019, "quoteright"),
    (0x201A, "quotesinglbase"),
    (0x201C, "quotedblleft"),
    (0x201D, "quotedblright"),
    (0x201E, "quotedblbase"),
    (0x2020, "dagger"),
    (0x2021, "daggerdbl"),
    (0x2022, "bullet"),
    (0x2026, "ellipsis"),
    (0x2030, "perthousand"),
    (0x2039, "guilsinglleft"),
    (0x203A, "guilsinglright"),
    (0x2044, "fraction"),
    (0x20AC, "Euro"),
    (0x2122, "trademark"),
    (0x2202, "partialdiff"),
    (0x2206, "Delta"),
    (0x220F, "product"),
    (0x2211, "summation"),
    (0x221A, "radical"),
    (0x221E, "infinity"),
    (0x222B, "integral"),
    (0x2248, "approxequal"),
    (0x2260, "notequal"),
    (0x2264, "lessequal"),
    (0x2265, "greaterequal"),
    (0x25CA, "lozenge"),
    (0xF001, "fi"),
    (0xFB01, "fi"),
    (0xFB02, "fl"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_fontname_line() {
        assert_eq!(
            parse_fontname_line("/FontName /Helvetica def"),
            Some("Helvetica".to_string())
        );
        assert_eq!(
            parse_fontname_line("/FontName /NimbusSans-Regular def"),
            Some("NimbusSans-Regular".to_string())
        );
        assert_eq!(parse_fontname_line("/FontType 1 def"), None);
    }

    #[test]
    fn test_unicode_to_glyph_name() {
        assert_eq!(unicode_to_glyph_name(0x0041), Some("A"));
        assert_eq!(unicode_to_glyph_name(0x0020), Some("space"));
        assert_eq!(unicode_to_glyph_name(0x00C9), Some("Eacute"));
        assert_eq!(unicode_to_glyph_name(0x9999), None);
    }

    #[test]
    fn test_extract_ps_name_from_pfa() {
        let font_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../resources/Font/NimbusSans-Regular.t1");
        if !font_path.exists() {
            return;
        }
        let name = extract_ps_name_from_pfa(&font_path);
        assert_eq!(name, Some("NimbusSans-Regular".to_string()));
    }

    #[test]
    fn test_system_font_cache_builds() {
        let cache = SystemFontCache::build();
        // On a typical Linux system, should find some fonts
        assert!(
            !cache.fonts.is_empty(),
            "Expected to find system fonts, found none"
        );
    }

    #[test]
    fn test_cmap_format4_real_font() {
        // Test with a real system TTF if available
        let test_fonts = [
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
        ];
        for path in &test_fonts {
            if let Ok(data) = std::fs::read(path) {
                let map = parse_cmap_table(&data);
                assert!(map.is_some(), "Failed to parse cmap for {}", path);
                let map = map.unwrap();
                // Should at least map ASCII 'A' (0x41)
                assert!(map.contains_key(&0x41), "cmap missing 'A' for {}", path);
                return;
            }
        }
        eprintln!("Skipping cmap test — no test font found");
    }
}
