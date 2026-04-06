// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! TrueType (Type 42) glyph parser and path converter.
//!
//! Parses TrueType `glyf` table data, converts quadratic B-spline contours
//! to cubic Bezier paths for rendering through the existing paint pipeline.

use crate::geometry::{PathSegment, PsPath};

/// Read a big-endian u16 from a byte slice at the given offset.
#[inline]
pub fn read_u16(data: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([data[offset], data[offset + 1]])
}

/// Read a big-endian i16 from a byte slice at the given offset.
#[inline]
pub fn read_i16(data: &[u8], offset: usize) -> i16 {
    i16::from_be_bytes([data[offset], data[offset + 1]])
}

/// Read a big-endian u32 from a byte slice at the given offset.
#[inline]
pub fn read_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

/// Concatenate sfnts array entries into a single font data buffer.
///
/// Per TrueType spec, each string in the sfnts array may have an extra
/// padding byte at the end if its length is odd. We strip that padding.
pub fn concatenate_sfnts(strings: &[&[u8]]) -> Vec<u8> {
    let total: usize = strings
        .iter()
        .map(|s| {
            // Strip trailing padding byte if length is odd
            if s.len() % 2 != 0 {
                s.len() - 1
            } else {
                s.len()
            }
        })
        .sum();
    let mut result = Vec::with_capacity(total);
    for s in strings {
        let effective_len = if s.len() % 2 != 0 {
            s.len() - 1
        } else {
            s.len()
        };
        result.extend_from_slice(&s[..effective_len]);
    }
    result
}

/// Find a table in the sfnt table directory, returning (offset, length).
pub fn find_table(font_data: &[u8], tag: &[u8; 4]) -> Option<(usize, usize)> {
    if font_data.len() < 12 {
        return None;
    }
    // Handle TrueType Collection (TTC): use the first font's offset table.
    // Table offsets within a TTC are absolute, so returned values work as-is.
    let base = if font_data.len() >= 16 && &font_data[0..4] == b"ttcf" {
        read_u32(font_data, 12) as usize
    } else {
        0
    };
    if base + 12 > font_data.len() {
        return None;
    }
    let num_tables = read_u16(font_data, base + 4) as usize;
    for i in 0..num_tables {
        let entry_offset = base + 12 + i * 16;
        if entry_offset + 16 > font_data.len() {
            break;
        }
        if &font_data[entry_offset..entry_offset + 4] == tag {
            let offset = read_u32(font_data, entry_offset + 8) as usize;
            let length = read_u32(font_data, entry_offset + 12) as usize;
            return Some((offset, length));
        }
    }
    None
}

/// Get units per em from the head table.
pub fn get_units_per_em(font_data: &[u8]) -> u16 {
    if let Some((offset, _len)) = find_table(font_data, b"head")
        && offset + 20 <= font_data.len()
    {
        return read_u16(font_data, offset + 18);
    }
    1000 // fallback
}

/// Get the number of glyphs from the maxp table.
pub fn get_num_glyphs(font_data: &[u8]) -> u32 {
    if let Some((offset, _len)) = find_table(font_data, b"maxp")
        && offset + 6 <= font_data.len()
    {
        return read_u16(font_data, offset + 4) as u32;
    }
    0
}

/// Get the advance width for a glyph ID from the hmtx table.
pub fn get_advance_width(font_data: &[u8], gid: u16) -> Option<u16> {
    let (hhea_off, _) = find_table(font_data, b"hhea")?;
    if hhea_off + 36 > font_data.len() {
        return None;
    }
    let num_h_metrics = read_u16(font_data, hhea_off + 34) as usize;

    let (hmtx_off, _) = find_table(font_data, b"hmtx")?;
    let gid = gid as usize;

    if gid < num_h_metrics {
        let entry_off = hmtx_off + gid * 4;
        if entry_off + 2 <= font_data.len() {
            return Some(read_u16(font_data, entry_off));
        }
    } else if num_h_metrics > 0 {
        // GIDs beyond numHMetrics use the last advance width
        let entry_off = hmtx_off + (num_h_metrics - 1) * 4;
        if entry_off + 2 <= font_data.len() {
            return Some(read_u16(font_data, entry_off));
        }
    }
    None
}

/// Extract raw glyf table bytes for a specific glyph ID.
pub fn get_glyf_data(font_data: &[u8], gid: u16) -> Option<Vec<u8>> {
    let (head_off, _) = find_table(font_data, b"head")?;
    if head_off + 52 > font_data.len() {
        return None;
    }
    let index_to_loc_format = read_i16(font_data, head_off + 50);

    // Try standard loca/glyf first, fall back to PDF-subset locx/glyx
    let (loca_off, _loca_len) =
        find_table(font_data, b"loca").or_else(|| find_table(font_data, b"locx"))?;
    let (glyf_off, _glyf_len) =
        find_table(font_data, b"glyf").or_else(|| find_table(font_data, b"glyx"))?;

    let gid = gid as usize;

    // locx always uses long (4-byte) offsets; standard loca uses head.indexToLocFormat
    let has_locx = find_table(font_data, b"loca").is_none();
    let use_long = has_locx || index_to_loc_format != 0;
    let (offset, next_offset) = if !use_long {
        // Short format: offsets are u16, multiply by 2
        let off_pos = loca_off + gid * 2;
        let next_pos = loca_off + (gid + 1) * 2;
        if next_pos + 2 > font_data.len() {
            return None;
        }
        let off = read_u16(font_data, off_pos) as usize * 2;
        let next = read_u16(font_data, next_pos) as usize * 2;
        (off, next)
    } else {
        // Long format: offsets are u32
        let off_pos = loca_off + gid * 4;
        let next_pos = loca_off + (gid + 1) * 4;
        if next_pos + 4 > font_data.len() {
            return None;
        }
        let off = read_u32(font_data, off_pos) as usize;
        let next = read_u32(font_data, next_pos) as usize;
        (off, next)
    };

    if offset == next_offset {
        return None; // Empty glyph (e.g., space)
    }

    // Handle non-monotonic loca tables (found in some PDF font subsets).
    // When loca[gid+1] < loca[gid], the glyph data is stored out of order in
    // the glyf table. Determine the glyph's end by finding the smallest loca
    // offset that is strictly greater than this glyph's start offset.
    let end_offset = if next_offset >= offset {
        next_offset
    } else {
        let num_glyphs = get_num_glyphs(font_data) as usize;
        let entry_size = if use_long { 4 } else { 2 };
        let mut best = if has_locx {
            font_data.len()
        } else {
            _glyf_len
        };
        for i in 0..=num_glyphs {
            let pos = loca_off + i * entry_size;
            let entry = if use_long {
                if pos + 4 <= font_data.len() {
                    read_u32(font_data, pos) as usize
                } else {
                    continue;
                }
            } else if pos + 2 <= font_data.len() {
                read_u16(font_data, pos) as usize * 2
            } else {
                continue;
            };
            if entry > offset && entry < best {
                best = entry;
            }
        }
        best
    };

    // For locx/glyx (PDF-subset), offsets are absolute within the font data
    let abs_offset = if has_locx { offset } else { glyf_off + offset };
    let abs_end = if has_locx {
        end_offset
    } else {
        glyf_off + end_offset
    };
    if abs_offset >= font_data.len() || abs_end > font_data.len() || abs_offset >= abs_end {
        return None;
    }

    Some(font_data[abs_offset..abs_end].to_vec())
}

/// Point from glyf contour parsing.
struct GlyfPoint {
    x: f64,
    y: f64,
    on_curve: bool,
}

/// Parse a simple glyph (num_contours > 0) into contour point lists.
fn parse_simple_glyph(glyf_data: &[u8], num_contours: i16) -> Option<Vec<Vec<GlyfPoint>>> {
    let nc = num_contours as usize;
    if glyf_data.len() < 10 + nc * 2 {
        return None;
    }

    // Read endPtsOfContours
    let mut end_pts = Vec::with_capacity(nc);
    let mut offset = 10; // skip header (numContours + xMin + yMin + xMax + yMax)
    for _ in 0..nc {
        end_pts.push(read_u16(glyf_data, offset) as usize);
        offset += 2;
    }

    // Skip instructions
    if offset + 2 > glyf_data.len() {
        return None;
    }
    let inst_len = read_u16(glyf_data, offset) as usize;
    offset += 2 + inst_len;

    let num_points = *end_pts.last()? + 1;

    // Parse flags with run-length encoding
    let mut flags = Vec::with_capacity(num_points);
    while flags.len() < num_points {
        if offset >= glyf_data.len() {
            return None;
        }
        let flag = glyf_data[offset];
        offset += 1;
        flags.push(flag);

        if flag & 0x08 != 0 {
            // Repeat flag
            if offset >= glyf_data.len() {
                return None;
            }
            let repeat_count = glyf_data[offset] as usize;
            offset += 1;
            for _ in 0..repeat_count {
                flags.push(flag);
            }
        }
    }

    // Parse x coordinates (cumulative deltas)
    let mut x_coords = Vec::with_capacity(num_points);
    let mut x: i32 = 0;
    for &flag in &flags[..num_points] {
        let x_short = flag & 0x02 != 0;
        let x_same_or_positive = flag & 0x10 != 0;

        if x_short {
            if offset >= glyf_data.len() {
                return None;
            }
            let delta = glyf_data[offset] as i32;
            offset += 1;
            x += if x_same_or_positive { delta } else { -delta };
        } else if !x_same_or_positive {
            if offset + 2 > glyf_data.len() {
                return None;
            }
            x += read_i16(glyf_data, offset) as i32;
            offset += 2;
        }
        // else: x_same_or_positive && !x_short → delta is 0, x unchanged
        x_coords.push(x);
    }

    // Parse y coordinates (cumulative deltas)
    let mut y_coords = Vec::with_capacity(num_points);
    let mut y: i32 = 0;
    for &flag in &flags[..num_points] {
        let y_short = flag & 0x04 != 0;
        let y_same_or_positive = flag & 0x20 != 0;

        if y_short {
            if offset >= glyf_data.len() {
                return None;
            }
            let delta = glyf_data[offset] as i32;
            offset += 1;
            y += if y_same_or_positive { delta } else { -delta };
        } else if !y_same_or_positive {
            if offset + 2 > glyf_data.len() {
                return None;
            }
            y += read_i16(glyf_data, offset) as i32;
            offset += 2;
        }
        y_coords.push(y);
    }

    // Split into contours
    let mut contours = Vec::with_capacity(nc);
    let mut start = 0;
    for &end in &end_pts {
        let mut contour = Vec::new();
        for i in start..=end {
            if i < num_points {
                contour.push(GlyfPoint {
                    x: x_coords[i] as f64,
                    y: y_coords[i] as f64,
                    on_curve: flags[i] & 0x01 != 0,
                });
            }
        }
        contours.push(contour);
        start = end + 1;
    }

    Some(contours)
}

/// Read a TrueType F2Dot14 fixed-point value.
#[inline]
fn read_f2dot14(data: &[u8], offset: usize) -> f64 {
    let raw = read_i16(data, offset);
    raw as f64 / 16384.0
}

/// Parse a composite glyph, recursively resolving components.
fn parse_composite_glyph(
    glyf_data: &[u8],
    resolver: &dyn Fn(u16) -> Option<Vec<u8>>,
) -> Vec<Vec<GlyfPoint>> {
    let mut all_contours = Vec::new();
    let mut offset = 10; // skip header

    loop {
        if offset + 4 > glyf_data.len() {
            break;
        }
        let flags = read_u16(glyf_data, offset);
        let glyph_index = read_u16(glyf_data, offset + 2);
        offset += 4;

        // Read x/y offsets
        let (dx, dy) = if flags & 0x0001 != 0 {
            // ARG_1_AND_2_ARE_WORDS
            if offset + 4 > glyf_data.len() {
                break;
            }
            let x = if flags & 0x0002 != 0 {
                read_i16(glyf_data, offset) as f64
            } else {
                read_u16(glyf_data, offset) as f64
            };
            let y = if flags & 0x0002 != 0 {
                read_i16(glyf_data, offset + 2) as f64
            } else {
                read_u16(glyf_data, offset + 2) as f64
            };
            offset += 4;
            (x, y)
        } else {
            if offset + 2 > glyf_data.len() {
                break;
            }
            let x = if flags & 0x0002 != 0 {
                glyf_data[offset] as i8 as f64
            } else {
                glyf_data[offset] as f64
            };
            let y = if flags & 0x0002 != 0 {
                glyf_data[offset + 1] as i8 as f64
            } else {
                glyf_data[offset + 1] as f64
            };
            offset += 2;
            (x, y)
        };

        // Read optional transform
        let (scale_x, scale_01, scale_10, scale_y) = if flags & 0x0008 != 0 {
            // WE_HAVE_A_SCALE
            if offset + 2 > glyf_data.len() {
                break;
            }
            let scale = read_f2dot14(glyf_data, offset);
            offset += 2;
            (scale, 0.0, 0.0, scale)
        } else if flags & 0x0040 != 0 {
            // WE_HAVE_AN_X_AND_Y_SCALE
            if offset + 4 > glyf_data.len() {
                break;
            }
            let sx = read_f2dot14(glyf_data, offset);
            let sy = read_f2dot14(glyf_data, offset + 2);
            offset += 4;
            (sx, 0.0, 0.0, sy)
        } else if flags & 0x0080 != 0 {
            // WE_HAVE_A_TWO_BY_TWO
            if offset + 8 > glyf_data.len() {
                break;
            }
            let a = read_f2dot14(glyf_data, offset);
            let b = read_f2dot14(glyf_data, offset + 2);
            let c = read_f2dot14(glyf_data, offset + 4);
            let d = read_f2dot14(glyf_data, offset + 6);
            offset += 8;
            (a, b, c, d)
        } else {
            (1.0, 0.0, 0.0, 1.0)
        };

        // Resolve component glyph
        if let Some(component_data) = resolver(glyph_index)
            && component_data.len() >= 2
        {
            let child_contours = parse_glyf_to_contours(&component_data, resolver);
            // Transform and merge
            for contour in child_contours {
                let transformed: Vec<GlyfPoint> = contour
                    .into_iter()
                    .map(|p| GlyfPoint {
                        x: p.x * scale_x + p.y * scale_10 + dx,
                        y: p.x * scale_01 + p.y * scale_y + dy,
                        on_curve: p.on_curve,
                    })
                    .collect();
                all_contours.push(transformed);
            }
        }

        // Check MORE_COMPONENTS flag
        if flags & 0x0020 == 0 {
            break;
        }
    }

    all_contours
}

/// Parse glyf data into contour point lists (handles simple and composite).
fn parse_glyf_to_contours(
    glyf_data: &[u8],
    resolver: &dyn Fn(u16) -> Option<Vec<u8>>,
) -> Vec<Vec<GlyfPoint>> {
    if glyf_data.len() < 10 {
        return Vec::new();
    }

    let num_contours = read_i16(glyf_data, 0);

    if num_contours > 0 {
        parse_simple_glyph(glyf_data, num_contours).unwrap_or_default()
    } else if num_contours < 0 {
        parse_composite_glyph(glyf_data, resolver)
    } else {
        Vec::new()
    }
}

/// Convert TrueType quadratic B-spline contours to cubic Bezier PsPath.
///
/// Handles:
/// - On-curve points → LineTo
/// - Off-curve quadratic control points → cubic Bezier approximation
/// - Two consecutive off-curve points → implicit on-curve midpoint
/// - Contours starting with off-curve points
fn contours_to_path(contours: Vec<Vec<GlyfPoint>>) -> PsPath {
    let mut path = PsPath::new();

    for contour in &contours {
        if contour.is_empty() {
            continue;
        }

        // Determine the starting on-curve point.
        // If the first point is on-curve, use it directly.
        // If the first point is off-curve:
        //   - If the last point is on-curve, start there
        //   - Otherwise, start at the midpoint of first and last
        let (start_x, start_y, first_idx) = if contour[0].on_curve {
            (contour[0].x, contour[0].y, 1)
        } else if contour.last().unwrap().on_curve {
            let last = contour.last().unwrap();
            (last.x, last.y, 0)
        } else {
            // Both first and last are off-curve: start at midpoint
            let last = contour.last().unwrap();
            let mx = (contour[0].x + last.x) / 2.0;
            let my = (contour[0].y + last.y) / 2.0;
            (mx, my, 0)
        };

        path.segments.push(PathSegment::MoveTo(start_x, start_y));

        let n = contour.len();
        let mut i = first_idx;
        let mut cur_x = start_x;
        let mut cur_y = start_y;
        let mut count = 0;

        while count < n {
            let idx = i % n;
            let pt = &contour[idx];

            if pt.on_curve {
                path.segments.push(PathSegment::LineTo(pt.x, pt.y));
                cur_x = pt.x;
                cur_y = pt.y;
                i += 1;
                count += 1;
            } else {
                // Off-curve control point — find the endpoint
                let next_idx = (i + 1) % n;
                let next = &contour[next_idx];

                let (end_x, end_y) = if next.on_curve {
                    i += 2;
                    count += 2;
                    (next.x, next.y)
                } else {
                    // Implicit on-curve midpoint between two off-curve
                    i += 1;
                    count += 1;
                    ((pt.x + next.x) / 2.0, (pt.y + next.y) / 2.0)
                };

                // Convert quadratic to cubic:
                // cp1 = start + 2/3 * (ctrl - start)
                // cp2 = end + 2/3 * (ctrl - end)
                let cp1x = cur_x + 2.0 / 3.0 * (pt.x - cur_x);
                let cp1y = cur_y + 2.0 / 3.0 * (pt.y - cur_y);
                let cp2x = end_x + 2.0 / 3.0 * (pt.x - end_x);
                let cp2y = end_y + 2.0 / 3.0 * (pt.y - end_y);

                path.segments.push(PathSegment::CurveTo {
                    x1: cp1x,
                    y1: cp1y,
                    x2: cp2x,
                    y2: cp2y,
                    x3: end_x,
                    y3: end_y,
                });

                cur_x = end_x;
                cur_y = end_y;
            }
        }

        path.segments.push(PathSegment::ClosePath);
    }

    path
}

/// Parse TrueType glyf data into a PsPath.
///
/// `resolver` is called to fetch raw glyf bytes for component glyph IDs
/// (used for composite glyphs). For simple glyphs it's not called.
pub fn parse_glyf_to_path(glyf_data: &[u8], resolver: &dyn Fn(u16) -> Option<Vec<u8>>) -> PsPath {
    let contours = parse_glyf_to_contours(glyf_data, resolver);
    contours_to_path(contours)
}

/// Parse TrueType cmap table, returning a mapping from character code to glyph index.
///
/// Supports Format 0 (byte encoding), Format 4 (segment mapping), and Format 6 (trimmed table).
pub fn parse_cmap(font_data: &[u8]) -> std::collections::HashMap<u32, u16> {
    parse_cmap_with_info(font_data).0
}

/// Parse the cmap table, returning the mapping and whether the selected
/// subtable is Unicode-keyed (platforms (3,1), (3,10), or (0,*)).
/// Non-Unicode cmaps ((1,0) Mac Roman, (3,0) Windows Symbol) map re-encoded
/// character codes or F0XX symbol codes — NOT Unicode values.
pub fn parse_cmap_with_info(font_data: &[u8]) -> (std::collections::HashMap<u32, u16>, bool) {
    let mut map = std::collections::HashMap::new();
    let (cmap_off, cmap_len) = match find_table(font_data, b"cmap") {
        Some(v) => v,
        None => return (map, false),
    };
    if cmap_off + 4 > font_data.len() {
        return (map, false);
    }
    let mut actual_cmap_off = cmap_off;
    let raw_num_subtables = read_u16(font_data, cmap_off + 2) as usize;
    // Cap to what fits in the cmap table to avoid reading past table bounds
    // into unrelated data (corrupted/malformed fonts may have wrong numTables).
    let max_subtables = cmap_len.saturating_sub(4) / 8;
    let mut num_subtables = raw_num_subtables.min(max_subtables);

    // If numTables from the header doesn't fit in the cmap table, the data may
    // have a 1-byte alignment error from decompression issues (e.g. corrupt zlib
    // window size in FlateDecode header). Try reading from offset-1.
    if raw_num_subtables > max_subtables && cmap_off > 0 {
        let alt_version = read_u16(font_data, cmap_off - 1);
        let alt_num = read_u16(font_data, cmap_off + 1) as usize;
        if alt_version == 0 && alt_num > 0 && alt_num <= max_subtables {
            actual_cmap_off = cmap_off - 1;
            num_subtables = alt_num;
        }
    }

    // Find best subtable: prefer (3,1) Windows Unicode, then (1,0) Mac Roman
    let mut best_offset = None;
    let mut best_priority = 0u8;
    for i in 0..num_subtables {
        let entry = actual_cmap_off + 4 + i * 8;
        if entry + 8 > font_data.len() {
            break;
        }
        let platform = read_u16(font_data, entry);
        let encoding = read_u16(font_data, entry + 2);
        let offset = read_u32(font_data, entry + 4) as usize;
        // Validate subtable offset is within the cmap table itself, not just
        // the overall font data. Some malformed PDFs declare a subtable whose
        // offset points past the end of the cmap table — into the next sfnt
        // table — which would otherwise be misinterpreted as a valid subtable.
        if offset + 2 > cmap_len || actual_cmap_off + offset + 2 > font_data.len() {
            continue;
        }
        let priority = match (platform, encoding) {
            (3, 10) => 5, // Windows Unicode Full (format 12)
            (3, 1) => 4,  // Windows Unicode BMP
            (0, _) => 3,  // Unicode
            (1, 0) => 2,  // Mac Roman
            (3, 0) => 1,  // Windows Symbol (U+F0XX range, common in subset fonts)
            _ => 0,
        };
        if priority > best_priority {
            best_priority = priority;
            best_offset = Some(actual_cmap_off + offset);
        }
    }
    // Unicode-keyed: (3,10), (3,1), or (0,*)  — priority 3+
    let cmap_is_unicode = best_priority >= 3;

    let subtable_off = match best_offset {
        Some(v) => v,
        None => return (map, cmap_is_unicode),
    };
    if subtable_off + 2 > font_data.len() {
        return (map, cmap_is_unicode);
    }
    let format = read_u16(font_data, subtable_off);

    match format {
        0 => {
            // Format 0: byte encoding table
            if subtable_off + 6 + 256 > font_data.len() {
                return (map, cmap_is_unicode);
            }
            for code in 0u32..256 {
                let gid = font_data[subtable_off + 6 + code as usize] as u16;
                if gid != 0 {
                    map.insert(code, gid);
                }
            }
        }
        2 => {
            // Format 2: high-byte mapping through table.
            // Used for mixed single/multi-byte encodings (e.g. Shift-JIS) and
            // some subset fonts that pack Unicode BMP into format 2.
            // Structure: subHeaderKeys[256] → subHeaders[] → glyphIndexArray[]
            let shk_off = subtable_off + 6; // 256 × u16 subHeaderKeys
            let sh_base = shk_off + 512; // subHeaders start here
            if sh_base + 8 > font_data.len() {
                return (map, cmap_is_unicode);
            }
            // Single-byte characters via subHeaders[0]
            {
                let first_code = read_u16(font_data, sh_base) as u32;
                let entry_count = read_u16(font_data, sh_base + 2) as u32;
                let id_delta = read_i16(font_data, sh_base + 4) as i32;
                let ro_addr = sh_base + 6;
                let range_off = read_u16(font_data, ro_addr) as usize;
                for j in 0..entry_count {
                    let addr = ro_addr + range_off + j as usize * 2;
                    if addr + 2 > font_data.len() {
                        break;
                    }
                    let gid_raw = read_u16(font_data, addr);
                    if gid_raw != 0 {
                        let gid = ((gid_raw as i32 + id_delta) & 0xFFFF) as u16;
                        if gid != 0 {
                            map.insert(first_code + j, gid);
                        }
                    }
                }
            }
            // Two-byte characters: high bytes with subHeaderKeys[h] > 0
            for high in 1u32..256 {
                let k = read_u16(font_data, shk_off + high as usize * 2) as usize / 8;
                if k == 0 {
                    continue; // maps to single-byte subHeader, already handled
                }
                let s = sh_base + k * 8;
                if s + 8 > font_data.len() {
                    continue;
                }
                let first_code = read_u16(font_data, s) as u32;
                let entry_count = read_u16(font_data, s + 2) as u32;
                let id_delta = read_i16(font_data, s + 4) as i32;
                let ro_addr = s + 6;
                let range_off = read_u16(font_data, ro_addr) as usize;
                for j in 0..entry_count {
                    let addr = ro_addr + range_off + j as usize * 2;
                    if addr + 2 > font_data.len() {
                        break;
                    }
                    let gid_raw = read_u16(font_data, addr);
                    if gid_raw != 0 {
                        let gid = ((gid_raw as i32 + id_delta) & 0xFFFF) as u16;
                        if gid != 0 {
                            map.insert((high << 8) | (first_code + j), gid);
                        }
                    }
                }
            }
        }
        4 => {
            // Format 4: segment mapping to delta values
            if subtable_off + 14 > font_data.len() {
                return (map, cmap_is_unicode);
            }
            let seg_count = read_u16(font_data, subtable_off + 6) as usize / 2;
            let end_codes_off = subtable_off + 14;
            let start_codes_off = end_codes_off + seg_count * 2 + 2; // +2 for reservedPad
            let deltas_off = start_codes_off + seg_count * 2;
            let range_offsets_off = deltas_off + seg_count * 2;

            if range_offsets_off + seg_count * 2 > font_data.len() {
                return (map, cmap_is_unicode);
            }

            for seg in 0..seg_count {
                let end_code = read_u16(font_data, end_codes_off + seg * 2) as u32;
                let start_code = read_u16(font_data, start_codes_off + seg * 2) as u32;
                let delta = read_i16(font_data, deltas_off + seg * 2) as i32;
                let range_offset_pos = range_offsets_off + seg * 2;
                let range_offset = read_u16(font_data, range_offset_pos) as usize;

                if start_code == 0xFFFF {
                    break;
                }

                for code in start_code..=end_code {
                    let gid = if range_offset == 0 {
                        ((code as i32 + delta) & 0xFFFF) as u16
                    } else {
                        let idx =
                            range_offset_pos + range_offset + (code - start_code) as usize * 2;
                        if idx + 2 > font_data.len() {
                            0
                        } else {
                            let gid = read_u16(font_data, idx);
                            if gid != 0 {
                                ((gid as i32 + delta) & 0xFFFF) as u16
                            } else {
                                0
                            }
                        }
                    };
                    if gid != 0 {
                        map.insert(code, gid);
                    }
                }
            }
        }
        6 => {
            // Format 6: trimmed table mapping
            if subtable_off + 10 > font_data.len() {
                return (map, cmap_is_unicode);
            }
            let first_code = read_u16(font_data, subtable_off + 6) as u32;
            let entry_count = read_u16(font_data, subtable_off + 8) as usize;
            let entries_off = subtable_off + 10;
            if entries_off + entry_count * 2 > font_data.len() {
                return (map, cmap_is_unicode);
            }
            for i in 0..entry_count {
                let gid = read_u16(font_data, entries_off + i * 2);
                if gid != 0 {
                    map.insert(first_code + i as u32, gid);
                }
            }
        }
        12 => {
            // Format 12: segmented coverage (full 32-bit Unicode)
            if subtable_off + 16 > font_data.len() {
                return (map, cmap_is_unicode);
            }
            let n_groups = read_u32(font_data, subtable_off + 12) as usize;
            let groups_off = subtable_off + 16;
            if groups_off + n_groups * 12 > font_data.len() {
                return (map, cmap_is_unicode);
            }
            for i in 0..n_groups {
                let g = groups_off + i * 12;
                let start_char = read_u32(font_data, g);
                let end_char = read_u32(font_data, g + 4);
                let start_gid = read_u32(font_data, g + 8);
                for code in start_char..=end_char {
                    let gid = start_gid + (code - start_char);
                    if gid != 0 && gid <= 0xFFFF {
                        map.insert(code, gid as u16);
                    }
                }
            }
        }
        _ => {} // Unsupported format
    }

    (map, cmap_is_unicode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_concatenate_sfnts_even() {
        let s1 = &[1u8, 2, 3, 4][..];
        let s2 = &[5u8, 6][..];
        let result = concatenate_sfnts(&[s1, s2]);
        assert_eq!(result, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn test_concatenate_sfnts_odd_padding() {
        // Odd-length string: last byte is padding
        let s1 = &[1u8, 2, 3][..]; // odd → strip last byte
        let s2 = &[4u8, 5][..];
        let result = concatenate_sfnts(&[s1, s2]);
        assert_eq!(result, vec![1, 2, 4, 5]);
    }

    #[test]
    fn test_find_table_not_found() {
        // Minimal sfnt header with 0 tables
        let data = [0u8, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert!(find_table(&data, b"glyf").is_none());
    }

    #[test]
    fn test_simple_path_square() {
        // Simple glyph: a unit square (4 on-curve points, 1 contour)
        let mut glyf = Vec::new();
        // numContours = 1
        glyf.extend_from_slice(&1i16.to_be_bytes());
        // xMin, yMin, xMax, yMax
        glyf.extend_from_slice(&0i16.to_be_bytes());
        glyf.extend_from_slice(&0i16.to_be_bytes());
        glyf.extend_from_slice(&100i16.to_be_bytes());
        glyf.extend_from_slice(&100i16.to_be_bytes());
        // endPtsOfContours[0] = 3 (4 points: 0,1,2,3)
        glyf.extend_from_slice(&3u16.to_be_bytes());
        // instructionLength = 0
        glyf.extend_from_slice(&0u16.to_be_bytes());
        // Flags: all on-curve (0x01), using short positive for first point,
        // short positive delta for rest
        // Point 0: (0, 0) — need x=0,y=0
        // Point 1: (100, 0)
        // Point 2: (100, 100)
        // Point 3: (0, 100)
        // Flags: 0x37 = ON_CURVE|X_SHORT|Y_SHORT|X_POSITIVE|Y_POSITIVE
        //        0x17 = ON_CURVE|X_SHORT|Y_POSITIVE (y=0 as short with same flag)
        // Let's use explicit coordinates:
        // All points on-curve (flag bit 0 = 1)
        // Point 0: x=0,y=0 → x_short+positive(0), y_short+positive(0)
        //   flag = 0x01 | 0x02 | 0x10 | 0x04 | 0x20 = 0x37
        // Point 1: dx=100, dy=0
        //   x: short positive (100), y: same as prev (flag 0x20 with !short)
        //   flag = 0x01 | 0x02 | 0x10 | 0x20 = 0x33
        // Point 2: dx=0, dy=100
        //   x: same as prev, y: short positive (100)
        //   flag = 0x01 | 0x10 | 0x04 | 0x20 = 0x35
        // Point 3: dx=-100, dy=0
        //   x: short negative (-100), y: same as prev
        //   flag = 0x01 | 0x02 | 0x20 = 0x23
        glyf.push(0x37); // pt0
        glyf.push(0x33); // pt1
        glyf.push(0x35); // pt2
        glyf.push(0x23); // pt3
        // x coordinates
        glyf.push(0); // pt0: x=0
        glyf.push(100); // pt1: dx=+100
        // pt2: x same (no bytes)
        glyf.push(100); // pt3: dx=100 (but negative since flag bit 0x10 not set)
        // y coordinates
        glyf.push(0); // pt0: y=0
        // pt1: y same (no bytes)
        glyf.push(100); // pt2: dy=+100
        // pt3: y same (no bytes)

        let no_resolve = |_: u16| -> Option<Vec<u8>> { None };
        let path = parse_glyf_to_path(&glyf, &no_resolve);

        // Should have: MoveTo + 4 LineTo (one per point, wrapping) + ClosePath = 6
        assert_eq!(path.segments.len(), 6);
        assert!(matches!(path.segments[0], PathSegment::MoveTo(0.0, 0.0)));
        assert!(matches!(path.segments[5], PathSegment::ClosePath));
    }

    #[test]
    fn test_empty_glyph() {
        let no_resolve = |_: u16| -> Option<Vec<u8>> { None };
        let path = parse_glyf_to_path(&[], &no_resolve);
        assert!(path.is_empty());
    }

    #[test]
    fn test_units_per_em_fallback() {
        assert_eq!(get_units_per_em(&[]), 1000);
    }
}
