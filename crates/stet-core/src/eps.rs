// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! EPS file support: DOS binary header stripping and BoundingBox parsing.

/// DOS EPS binary header magic bytes.
const DOS_EPS_MAGIC: [u8; 4] = [0xC5, 0xD0, 0xD3, 0xC6];

/// Strip a DOS EPS binary header if present, returning the PostScript portion.
///
/// DOS EPS files start with a 30-byte header containing magic bytes `C5 D0 D3 C6`,
/// followed by a little-endian u32 offset and u32 length pointing to the embedded
/// PostScript section. If the magic is not found, the data is returned unchanged.
pub fn strip_dos_eps_header(data: &[u8]) -> &[u8] {
    if data.len() < 12 || data[..4] != DOS_EPS_MAGIC {
        return data;
    }

    let ps_offset = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
    let ps_length = u32::from_le_bytes([data[8], data[9], data[10], data[11]]) as usize;

    let start = ps_offset.min(data.len());
    let end = (ps_offset + ps_length).min(data.len());
    &data[start..end]
}

/// Parse a `%%BoundingBox` or `%%HiResBoundingBox` DSC comment from EPS data.
///
/// Scans the first 4096 bytes for the bounding box comment. Prefers
/// `%%HiResBoundingBox` (float values) over `%%BoundingBox` (integer values).
/// Handles `%%BoundingBox: (atend)` by also scanning the last 4096 bytes.
///
/// Returns `Some((llx, lly, urx, ury))` or `None` if not found.
pub fn read_eps_bounding_box(data: &[u8]) -> Option<(f64, f64, f64, f64)> {
    // Scan the header portion first
    let header_end = data.len().min(4096);
    let header = &data[..header_end];

    let mut bbox = None;
    let mut hires_bbox = None;
    let mut need_atend = false;

    scan_for_bbox(header, &mut bbox, &mut hires_bbox, &mut need_atend);

    // If %%BoundingBox: (atend), scan the trailer too
    if need_atend && data.len() > 4096 {
        let trailer_start = data.len().saturating_sub(4096);
        let trailer = &data[trailer_start..];
        scan_for_bbox(trailer, &mut bbox, &mut hires_bbox, &mut need_atend);
    }

    hires_bbox.or(bbox)
}

/// Scan a byte slice for BoundingBox and HiResBoundingBox comments.
fn scan_for_bbox(
    data: &[u8],
    bbox: &mut Option<(f64, f64, f64, f64)>,
    hires_bbox: &mut Option<(f64, f64, f64, f64)>,
    need_atend: &mut bool,
) {
    for line in data.split(|&b| b == b'\n') {
        // Strip trailing \r
        let line = line.strip_suffix(b"\r").unwrap_or(line);

        if line.starts_with(b"%%HiResBoundingBox:") {
            let rest = &line[b"%%HiResBoundingBox:".len()..];
            if let Some(values) = parse_four_numbers(rest) {
                *hires_bbox = Some(values);
            }
        } else if line.starts_with(b"%%BoundingBox:") {
            let rest = &line[b"%%BoundingBox:".len()..];
            let trimmed = trim_ascii(rest);
            if trimmed == b"(atend)" {
                *need_atend = true;
            } else if let Some(values) = parse_four_numbers(rest) {
                *bbox = Some(values);
            }
        }
    }
}

/// Parse four whitespace-separated numbers from a byte slice.
fn parse_four_numbers(data: &[u8]) -> Option<(f64, f64, f64, f64)> {
    let s = std::str::from_utf8(data).ok()?;
    let mut nums = s.split_whitespace().filter_map(|w| w.parse::<f64>().ok());
    let llx = nums.next()?;
    let lly = nums.next()?;
    let urx = nums.next()?;
    let ury = nums.next()?;
    Some((llx, lly, urx, ury))
}

/// Trim leading and trailing ASCII whitespace from a byte slice.
fn trim_ascii(data: &[u8]) -> &[u8] {
    let start = data
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(data.len());
    let end = data
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(start, |p| p + 1);
    &data[start..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_dos_header() {
        // Build a synthetic DOS EPS file
        let ps_content = b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 100 100\n";
        let ps_offset: u32 = 30; // header is 30 bytes
        let ps_length = ps_content.len() as u32;

        let mut data = Vec::new();
        data.extend_from_slice(&DOS_EPS_MAGIC);
        data.extend_from_slice(&ps_offset.to_le_bytes());
        data.extend_from_slice(&ps_length.to_le_bytes());
        // Pad to 30 bytes (TIFF preview offset, length, and checksum)
        data.resize(30, 0xFF);
        data.extend_from_slice(ps_content);

        let result = strip_dos_eps_header(&data);
        assert_eq!(result, ps_content);
    }

    #[test]
    fn test_strip_no_header() {
        let ps_content = b"%!PS-Adobe-3.0\n%%BoundingBox: 0 0 200 300\n";
        let result = strip_dos_eps_header(ps_content);
        assert_eq!(result, ps_content);
    }

    #[test]
    fn test_bbox_integer() {
        let data = b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 200 300\n";
        let bbox = read_eps_bounding_box(data);
        assert_eq!(bbox, Some((0.0, 0.0, 200.0, 300.0)));
    }

    #[test]
    fn test_bbox_hires_preferred() {
        let data = b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 200 300\n%%HiResBoundingBox: 0.5 1.5 199.75 299.25\n";
        let bbox = read_eps_bounding_box(data);
        assert_eq!(bbox, Some((0.5, 1.5, 199.75, 299.25)));
    }

    #[test]
    fn test_bbox_not_found() {
        let data = b"%!PS-Adobe-3.0\n/Helvetica findfont 12 scalefont setfont\n";
        let bbox = read_eps_bounding_box(data);
        assert_eq!(bbox, None);
    }

    #[test]
    fn test_bbox_negative_coords() {
        let data = b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: -50 -25 150 275\n";
        let bbox = read_eps_bounding_box(data);
        assert_eq!(bbox, Some((-50.0, -25.0, 150.0, 275.0)));
    }

    #[test]
    fn test_bbox_atend() {
        // Build data larger than 4096 bytes with (atend) in header and bbox in trailer
        let mut data = Vec::new();
        data.extend_from_slice(b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: (atend)\n");
        // Pad to > 4096 bytes
        data.resize(5000, b' ');
        data.extend_from_slice(b"\n%%BoundingBox: 10 20 300 400\n%%EOF\n");

        let bbox = read_eps_bounding_box(&data);
        assert_eq!(bbox, Some((10.0, 20.0, 300.0, 400.0)));
    }
}
