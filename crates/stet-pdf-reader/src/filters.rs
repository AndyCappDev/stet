// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Stream decode filter chain for PDF streams.

use crate::error::PdfError;
use crate::objects::PdfDict;

/// A single decode filter.
#[derive(Debug, Clone, PartialEq)]
pub enum Filter {
    FlateDecode,
    LZWDecode,
    ASCIIHexDecode,
    ASCII85Decode,
    RunLengthDecode,
    DCTDecode,
    CCITTFaxDecode,
}

/// Parse the /Filter and /DecodeParms entries from a stream dict.
pub fn parse_filters(dict: &PdfDict) -> Result<(Vec<Filter>, Vec<Option<PdfDict>>), PdfError> {
    let filter_obj = match dict.get(b"Filter") {
        Some(obj) => obj,
        None => return Ok((Vec::new(), Vec::new())),
    };

    let filter_names: Vec<&[u8]> = match filter_obj {
        crate::objects::PdfObj::Name(n) => vec![n.as_slice()],
        crate::objects::PdfObj::Array(arr) => arr.iter().filter_map(|o| o.as_name()).collect(),
        _ => return Ok((Vec::new(), Vec::new())),
    };

    let mut filters = Vec::new();
    for name in &filter_names {
        filters.push(filter_from_name(name)?);
    }

    // Parse DecodeParms (single dict or array of dicts)
    let parms = match dict.get(b"DecodeParms") {
        Some(crate::objects::PdfObj::Dict(d)) => vec![Some(d.clone())],
        Some(crate::objects::PdfObj::Array(arr)) => arr
            .iter()
            .map(|o| match o {
                crate::objects::PdfObj::Dict(d) => Some(d.clone()),
                _ => None,
            })
            .collect(),
        _ => vec![None; filters.len()],
    };

    // Pad parms to match filters length
    let mut parms = parms;
    while parms.len() < filters.len() {
        parms.push(None);
    }

    Ok((filters, parms))
}

fn filter_from_name(name: &[u8]) -> Result<Filter, PdfError> {
    match name {
        b"FlateDecode" | b"Fl" => Ok(Filter::FlateDecode),
        b"LZWDecode" | b"LZW" => Ok(Filter::LZWDecode),
        b"ASCIIHexDecode" | b"AHx" => Ok(Filter::ASCIIHexDecode),
        b"ASCII85Decode" | b"A85" => Ok(Filter::ASCII85Decode),
        b"RunLengthDecode" | b"RL" => Ok(Filter::RunLengthDecode),
        b"DCTDecode" | b"DCT" => Ok(Filter::DCTDecode),
        b"CCITTFaxDecode" | b"CCF" => Ok(Filter::CCITTFaxDecode),
        // Tolerate truncated filter names from malformed PDFs
        _ if name.starts_with(b"Flate") => Ok(Filter::FlateDecode),
        _ if name.starts_with(b"LZW") => Ok(Filter::LZWDecode),
        _ if name.starts_with(b"ASCIIHex") => Ok(Filter::ASCIIHexDecode),
        _ if name.starts_with(b"ASCII85") => Ok(Filter::ASCII85Decode),
        _ if name.starts_with(b"RunLength") => Ok(Filter::RunLengthDecode),
        _ if name.starts_with(b"CCITT") => Ok(Filter::CCITTFaxDecode),
        _ => Err(PdfError::UnsupportedFilter(
            String::from_utf8_lossy(name).into(),
        )),
    }
}

/// Decode raw stream data through a chain of filters.
pub fn decode_stream(
    raw_data: &[u8],
    filters: &[Filter],
    decode_parms: &[Option<PdfDict>],
) -> Result<Vec<u8>, PdfError> {
    let mut data = raw_data.to_vec();

    for (i, filter) in filters.iter().enumerate() {
        let parms = decode_parms.get(i).and_then(|p| p.as_ref());
        data = match filter {
            Filter::FlateDecode => decode_flate(&data, parms)?,
            Filter::LZWDecode => decode_lzw(&data, parms)?,
            Filter::ASCIIHexDecode => decode_ascii_hex(&data)?,
            Filter::ASCII85Decode => decode_ascii85(&data)?,
            Filter::RunLengthDecode => decode_run_length(&data)?,
            Filter::DCTDecode => decode_dct(&data)?,
            Filter::CCITTFaxDecode => decode_ccittfax(&data, parms)?,
        };
    }

    Ok(data)
}

/// FlateDecode (zlib/deflate).
fn decode_flate(data: &[u8], parms: Option<&PdfDict>) -> Result<Vec<u8>, PdfError> {
    // Try zlib first. If it ends with an error (truncated output),
    // also try raw deflate (skip 2-byte zlib header) and pick the longer result.
    let (zlib_output, zlib_clean) = decode_flate_inner(data, true);
    let output = if zlib_clean {
        zlib_output?
    } else {
        // Zlib hit an error (corrupt checksum, etc). Use whatever it produced.
        // Don't try raw deflate — it may push past the corruption boundary
        // and decode garbage that renders as visual artifacts.
        let zlib_data = zlib_output.unwrap_or_default();
        if !zlib_data.is_empty() {
            zlib_data
        } else if data.len() > 2 {
            // Zlib produced nothing — try raw deflate as last resort
            let (raw_output, _) = decode_flate_inner(&data[2..], false);
            raw_output.map_err(|_| {
                PdfError::DecompressionError("flate: decompression failed".into())
            })?
        } else {
            return Err(PdfError::DecompressionError("flate: decompression failed".into()));
        }
    };

    // Apply predictor if specified
    if let Some(parms) = parms {
        let predictor = parms.get_int(b"Predictor").unwrap_or(1);
        if predictor > 1 {
            return Ok(apply_predictor(&output, parms, predictor)?);
        }
    }

    Ok(output)
}

/// Inner flate decompression. `zlib` = true uses zlib wrapper, false uses raw deflate.
/// Returns (Result<data>, clean) where clean=true means StreamEnd was reached normally.
fn decode_flate_inner(data: &[u8], zlib: bool) -> (Result<Vec<u8>, PdfError>, bool) {
    use flate2::Decompress;

    let mut decompressor = Decompress::new(zlib);
    let mut output = Vec::with_capacity(data.len() * 3);
    let mut buf = [0u8; 8192];
    let mut input_offset = 0;

    loop {
        let before_in = decompressor.total_in() as usize;
        let before_out = decompressor.total_out() as usize;
        let result = decompressor.decompress(
            &data[input_offset..],
            &mut buf,
            flate2::FlushDecompress::None,
        );

        let consumed = decompressor.total_in() as usize - before_in;
        let produced = decompressor.total_out() as usize - before_out;
        input_offset += consumed;
        output.extend_from_slice(&buf[..produced]);

        match result {
            Ok(status) => match status {
                flate2::Status::StreamEnd => return (Ok(output), true),
                flate2::Status::Ok | flate2::Status::BufError => {
                    if consumed == 0 && produced == 0 {
                        return (Ok(output), true);
                    }
                }
            },
            Err(_) if !output.is_empty() => {
                // Partial output — checksum/trailing data error.
                return (Ok(output), false);
            }
            Err(e) => {
                return (Err(PdfError::DecompressionError(format!("flate: {e}"))), false);
            }
        }
    }
}

/// LZWDecode.
fn decode_lzw(data: &[u8], parms: Option<&PdfDict>) -> Result<Vec<u8>, PdfError> {
    let early_change = parms.and_then(|p| p.get_int(b"EarlyChange")).unwrap_or(1);

    let mut decoder = if early_change == 0 {
        weezl::decode::Decoder::with_tiff_size_switch(weezl::BitOrder::Msb, 8)
    } else {
        weezl::decode::Decoder::new(weezl::BitOrder::Msb, 8)
    };

    let output = match decoder.decode(data) {
        Ok(out) => out,
        Err(e) => {
            // Try streaming decode to recover partial data on error
            let mut dec = if early_change == 0 {
                weezl::decode::Decoder::with_tiff_size_switch(weezl::BitOrder::Msb, 8)
            } else {
                weezl::decode::Decoder::new(weezl::BitOrder::Msb, 8)
            };
            let mut out = vec![0u8; data.len() * 4];
            let result = dec.decode_bytes(data, &mut out);
            let produced = result.consumed_out;
            if produced > 0 {
                out.truncate(produced);
                out
            } else {
                return Err(PdfError::DecompressionError(format!("lzw: {e}")));
            }
        }
    };

    // Apply predictor if specified
    if let Some(parms) = parms {
        let predictor = parms.get_int(b"Predictor").unwrap_or(1);
        if predictor > 1 {
            return apply_predictor(&output, parms, predictor);
        }
    }

    Ok(output)
}

/// ASCIIHexDecode.
fn decode_ascii_hex(data: &[u8]) -> Result<Vec<u8>, PdfError> {
    let mut result = Vec::with_capacity(data.len() / 2);
    let mut high: Option<u8> = None;

    for &b in data {
        if b == b'>' {
            break;
        }
        if b.is_ascii_whitespace() {
            continue;
        }
        let nibble = hex_digit(b)
            .ok_or_else(|| PdfError::DecompressionError(format!("invalid hex digit: 0x{b:02x}")))?;
        match high {
            None => high = Some(nibble),
            Some(h) => {
                result.push(h << 4 | nibble);
                high = None;
            }
        }
    }
    if let Some(h) = high {
        result.push(h << 4);
    }

    Ok(result)
}

/// ASCII85Decode.
fn decode_ascii85(data: &[u8]) -> Result<Vec<u8>, PdfError> {
    let mut result = Vec::with_capacity(data.len() * 4 / 5);
    let mut tuple: u64 = 0;
    let mut count = 0u8;

    for &b in data {
        if b == b'~' {
            break; // ~> end marker
        }
        if b.is_ascii_whitespace() {
            continue;
        }
        if b == b'z' && count == 0 {
            result.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        if !(b'!'..=b'u').contains(&b) {
            continue; // skip invalid
        }
        tuple = tuple * 85 + (b - b'!') as u64;
        count += 1;
        if count == 5 {
            result.push((tuple >> 24) as u8);
            result.push((tuple >> 16) as u8);
            result.push((tuple >> 8) as u8);
            result.push(tuple as u8);
            tuple = 0;
            count = 0;
        }
    }

    // Handle remainder
    if count > 0 {
        for _ in count..5 {
            tuple = tuple * 85 + 84; // pad with 'u'
        }
        for i in 0..(count - 1) {
            result.push((tuple >> (24 - i * 8)) as u8);
        }
    }

    Ok(result)
}

/// RunLengthDecode (PackBits).
fn decode_run_length(data: &[u8]) -> Result<Vec<u8>, PdfError> {
    let mut result = Vec::new();
    let mut i = 0;

    while i < data.len() {
        let length_byte = data[i];
        i += 1;
        if length_byte < 128 {
            // Copy next (length_byte + 1) bytes literally
            let count = length_byte as usize + 1;
            if i + count > data.len() {
                break;
            }
            result.extend_from_slice(&data[i..i + count]);
            i += count;
        } else if length_byte > 128 {
            // Repeat next byte (257 - length_byte) times
            if i >= data.len() {
                break;
            }
            let count = 257 - length_byte as usize;
            let val = data[i];
            i += 1;
            for _ in 0..count {
                result.push(val);
            }
        } else {
            // 128 = EOD
            break;
        }
    }

    Ok(result)
}

/// DCTDecode (JPEG).
/// For PDF image streams, DCTDecode returns raw pixel data.
/// However, when used as a filter in a filter chain, the JPEG data
/// is typically the final representation — return the raw JPEG bytes
/// since the image decoder will handle them. For standalone streams,
/// decode the JPEG to raw pixels.
fn decode_dct(data: &[u8]) -> Result<Vec<u8>, PdfError> {
    use jpeg_decoder::Decoder;

    let mut decoder = Decoder::new(data);
    let pixels = decoder
        .decode()
        .map_err(|e| PdfError::DecompressionError(format!("DCTDecode: {e}")))?;
    Ok(pixels)
}

/// CCITTFaxDecode (Group 3 / Group 4 fax compression).
fn decode_ccittfax(data: &[u8], parms: Option<&PdfDict>) -> Result<Vec<u8>, PdfError> {
    use crate::objects::PdfObj;

    let k = parms.and_then(|p| p.get_int(b"K")).unwrap_or(0) as i32;
    let columns = parms.and_then(|p| p.get_int(b"Columns")).unwrap_or(1728) as u16;
    let rows_limit = parms.and_then(|p| p.get_int(b"Rows")).unwrap_or(0) as u32;
    let end_of_block = parms
        .and_then(|p| match p.get(b"EndOfBlock") {
            Some(PdfObj::Bool(b)) => Some(*b),
            _ => None,
        })
        .unwrap_or(true);
    let black_is1 = parms
        .and_then(|p| match p.get(b"BlackIs1") {
            Some(PdfObj::Bool(b)) => Some(*b),
            _ => None,
        })
        .unwrap_or(false);

    let row_bytes = (columns as usize + 7) / 8;
    let mut output = Vec::new();
    let mut line_count: u32 = 0;

    let mut process_line = |transitions: &[u16]| {
        if !end_of_block && rows_limit > 0 && line_count >= rows_limit {
            return;
        }

        let line = fax::decoder::Line {
            transitions,
            width: columns,
        };
        let mut row = vec![0u8; row_bytes];
        for (i, color) in line.pels().enumerate() {
            if i >= columns as usize {
                break;
            }
            if matches!(color, fax::Color::Black) {
                row[i / 8] |= 0x80 >> (i % 8);
            }
        }

        if !black_is1 {
            for byte in &mut row {
                *byte = !*byte;
            }
        }

        output.extend_from_slice(&row);
        line_count += 1;
    };

    if k < 0 {
        let height = if rows_limit > 0 {
            Some(rows_limit as u16)
        } else {
            None
        };
        fax::decoder::decode_g4(data.iter().copied(), columns, height, |transitions| {
            process_line(transitions);
        });
    } else {
        fax::decoder::decode_g3(data.iter().copied(), |transitions| {
            process_line(transitions);
        });
    }

    Ok(output)
}

/// Apply PNG or TIFF predictor to decoded data.
fn apply_predictor(data: &[u8], parms: &PdfDict, predictor: i64) -> Result<Vec<u8>, PdfError> {
    let columns = parms.get_int(b"Columns").unwrap_or(1) as usize;
    let colors = parms.get_int(b"Colors").unwrap_or(1) as usize;
    let bpc = parms.get_int(b"BitsPerComponent").unwrap_or(8) as usize;

    let bytes_per_pixel = (colors * bpc).div_ceil(8);
    let row_bytes = (columns * colors * bpc).div_ceil(8);

    if predictor == 2 {
        // TIFF horizontal differencing
        apply_tiff_predictor(data, row_bytes, bytes_per_pixel)
    } else if predictor >= 10 {
        // PNG predictors
        apply_png_predictor(data, row_bytes, bytes_per_pixel)
    } else {
        Ok(data.to_vec())
    }
}

/// TIFF predictor 2: horizontal differencing.
fn apply_tiff_predictor(
    data: &[u8],
    row_bytes: usize,
    bytes_per_pixel: usize,
) -> Result<Vec<u8>, PdfError> {
    let mut result = Vec::with_capacity(data.len());

    for row in data.chunks(row_bytes) {
        let mut out_row = vec![0u8; row.len()];
        for i in 0..row.len() {
            let left = if i >= bytes_per_pixel {
                out_row[i - bytes_per_pixel]
            } else {
                0
            };
            out_row[i] = row[i].wrapping_add(left);
        }
        result.extend_from_slice(&out_row);
    }

    Ok(result)
}

/// PNG predictors (10-15): per-row predictor byte.
fn apply_png_predictor(
    data: &[u8],
    row_bytes: usize,
    bytes_per_pixel: usize,
) -> Result<Vec<u8>, PdfError> {
    // Each row has a leading predictor byte + row_bytes data bytes
    let stride = row_bytes + 1;
    if !data.len().is_multiple_of(stride) && !data.is_empty() {
        // Try to handle imperfect data
    }

    let num_rows = data.len() / stride;
    let mut result = Vec::with_capacity(num_rows * row_bytes);
    let mut prev_row = vec![0u8; row_bytes];

    for row_idx in 0..num_rows {
        let row_start = row_idx * stride;
        if row_start >= data.len() {
            break;
        }
        let filter_type = data[row_start];
        let row_data = &data[row_start + 1..std::cmp::min(row_start + stride, data.len())];
        let mut out_row = vec![0u8; row_data.len()];

        match filter_type {
            0 => {
                // None
                out_row.copy_from_slice(row_data);
            }
            1 => {
                // Sub
                for i in 0..row_data.len() {
                    let left = if i >= bytes_per_pixel {
                        out_row[i - bytes_per_pixel]
                    } else {
                        0
                    };
                    out_row[i] = row_data[i].wrapping_add(left);
                }
            }
            2 => {
                // Up
                for i in 0..row_data.len() {
                    let up = if i < prev_row.len() { prev_row[i] } else { 0 };
                    out_row[i] = row_data[i].wrapping_add(up);
                }
            }
            3 => {
                // Average
                for i in 0..row_data.len() {
                    let left = if i >= bytes_per_pixel {
                        out_row[i - bytes_per_pixel] as u16
                    } else {
                        0
                    };
                    let up = if i < prev_row.len() {
                        prev_row[i] as u16
                    } else {
                        0
                    };
                    out_row[i] = row_data[i].wrapping_add(((left + up) / 2) as u8);
                }
            }
            4 => {
                // Paeth
                for i in 0..row_data.len() {
                    let left = if i >= bytes_per_pixel {
                        out_row[i - bytes_per_pixel]
                    } else {
                        0
                    };
                    let up = if i < prev_row.len() { prev_row[i] } else { 0 };
                    let up_left = if i >= bytes_per_pixel && i - bytes_per_pixel < prev_row.len() {
                        prev_row[i - bytes_per_pixel]
                    } else {
                        0
                    };
                    out_row[i] = row_data[i].wrapping_add(paeth(left, up, up_left));
                }
            }
            _ => {
                // Unknown predictor type — pass through
                out_row.copy_from_slice(row_data);
            }
        }

        prev_row[..out_row.len()].copy_from_slice(&out_row);
        result.extend_from_slice(&out_row);
    }

    Ok(result)
}

/// Paeth predictor function.
fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let a = a as i16;
    let b = b as i16;
    let c = c as i16;
    let p = a + b - c;
    let pa = (p - a).abs();
    let pb = (p - b).abs();
    let pc = (p - c).abs();
    if pa <= pb && pa <= pc {
        a as u8
    } else if pb <= pc {
        b as u8
    } else {
        c as u8
    }
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flate_round_trip() {
        use flate2::Compression;
        use flate2::write::ZlibEncoder;
        use std::io::Write;

        let original = b"Hello, PDF world! This is a test of FlateDecode.";
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(original).unwrap();
        let compressed = enc.finish().unwrap();

        let decoded = decode_flate(&compressed, None).unwrap();
        assert_eq!(&decoded, original);
    }

    #[test]
    fn ascii_hex_decode() {
        let decoded = decode_ascii_hex(b"48656C6C6F>").unwrap();
        assert_eq!(&decoded, b"Hello");
    }

    #[test]
    fn ascii_hex_odd_digits() {
        let decoded = decode_ascii_hex(b"ABC>").unwrap();
        assert_eq!(decoded, vec![0xAB, 0xC0]);
    }

    #[test]
    fn ascii85_decode() {
        // "Hello" in ASCII85 = 87cURD]j7
        // Full encoding: <~87cURD]j7BEbo7~>  (for "Hello, World")
        // Simple test: encode "test" = FCfN8
        let decoded = decode_ascii85(b"FCfN8~>").unwrap();
        assert_eq!(&decoded, b"test");
    }

    #[test]
    fn ascii85_z_shortcut() {
        let decoded = decode_ascii85(b"z~>").unwrap();
        assert_eq!(decoded, vec![0, 0, 0, 0]);
    }

    #[test]
    fn run_length_decode() {
        // 2 = copy 3 bytes, then 253 = repeat next byte 4 times, then 128 = EOD
        let data = vec![2, b'A', b'B', b'C', 253, b'X', 128];
        let decoded = decode_run_length(&data).unwrap();
        assert_eq!(&decoded, b"ABCXXXX");
    }

    #[test]
    fn png_predictor_none() {
        // Row of 3 bytes, predictor type 0 (none)
        let data = vec![0, 10, 20, 30];
        let result = apply_png_predictor(&data, 3, 1).unwrap();
        assert_eq!(result, vec![10, 20, 30]);
    }

    #[test]
    fn png_predictor_sub() {
        // Row of 3 bytes, predictor type 1 (sub), bpp=1
        // input: [5, 3, 4] -> output: [5, 8, 12]
        let data = vec![1, 5, 3, 4];
        let result = apply_png_predictor(&data, 3, 1).unwrap();
        assert_eq!(result, vec![5, 8, 12]);
    }

    #[test]
    fn png_predictor_up() {
        // Two rows, predictor type 2 (up)
        // Row 0: [0, 10, 20, 30]  (type 0 = none)
        // Row 1: [2, 5, 5, 5]    (type 2 = up)
        let data = vec![0, 10, 20, 30, 2, 5, 5, 5];
        let result = apply_png_predictor(&data, 3, 1).unwrap();
        assert_eq!(result, vec![10, 20, 30, 15, 25, 35]);
    }

    #[test]
    fn filter_chain() {
        use flate2::Compression;
        use flate2::write::ZlibEncoder;
        use std::io::Write;

        let original = b"filter chain test data";
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(original).unwrap();
        let compressed = enc.finish().unwrap();

        // Encode as ASCII hex
        let mut hex = String::new();
        for b in &compressed {
            hex.push_str(&format!("{b:02X}"));
        }
        hex.push('>');

        let filters = vec![Filter::ASCIIHexDecode, Filter::FlateDecode];
        let parms = vec![None, None];
        let decoded = decode_stream(hex.as_bytes(), &filters, &parms).unwrap();
        assert_eq!(&decoded, original);
    }
}
