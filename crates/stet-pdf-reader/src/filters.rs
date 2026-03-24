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
    JPXDecode,
    JBIG2Decode,
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

    // Parse DecodeParms (single dict or array of dicts/refs)
    let parms = match dict.get(b"DecodeParms") {
        Some(crate::objects::PdfObj::Dict(d)) => vec![Some(d.clone())],
        Some(crate::objects::PdfObj::Ref(_, _)) => {
            // Single indirect reference to a dict
            vec![None] // Will be resolved below if resolver is available
        }
        Some(crate::objects::PdfObj::Array(arr)) => arr
            .iter()
            .map(|o| match o {
                crate::objects::PdfObj::Dict(d) => Some(d.clone()),
                _ => None, // null or unresolved refs handled below
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

/// Resolve indirect references in a DecodeParms array.
/// Call this after `parse_filters` when a Resolver is available.
pub fn resolve_decode_parms(
    dict: &PdfDict,
    parms: &mut [Option<PdfDict>],
    resolver: &crate::resolver::Resolver,
) {
    let dp = match dict.get(b"DecodeParms") {
        Some(obj) => obj,
        None => return,
    };

    match dp {
        crate::objects::PdfObj::Ref(_, _) => {
            // Single indirect ref — resolve and use as first entry
            if let Ok(resolved) = resolver.deref(dp) {
                if let Some(d) = resolved.as_dict() {
                    if let Some(slot) = parms.first_mut() {
                        *slot = Some(d.clone());
                    }
                }
            }
        }
        crate::objects::PdfObj::Array(arr) => {
            for (i, obj) in arr.iter().enumerate() {
                if let crate::objects::PdfObj::Ref(_, _) = obj {
                    if let Ok(resolved) = resolver.deref(obj) {
                        if let Some(d) = resolved.as_dict() {
                            if let Some(slot) = parms.get_mut(i) {
                                *slot = Some(d.clone());
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
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
        b"JPXDecode" | b"JPX" => Ok(Filter::JPXDecode),
        b"JBIG2Decode" | b"JBIG2" => Ok(Filter::JBIG2Decode),
        // Tolerate truncated filter names from malformed PDFs
        _ if name.starts_with(b"Flate") => Ok(Filter::FlateDecode),
        _ if name.starts_with(b"LZW") => Ok(Filter::LZWDecode),
        _ if name.starts_with(b"ASCIIHex") => Ok(Filter::ASCIIHexDecode),
        _ if name.starts_with(b"ASCII85") => Ok(Filter::ASCII85Decode),
        _ if name.starts_with(b"RunLength") => Ok(Filter::RunLengthDecode),
        _ if name.starts_with(b"CCITT") => Ok(Filter::CCITTFaxDecode),
        _ if name.starts_with(b"JPX") => Ok(Filter::JPXDecode),
        _ if name.starts_with(b"JBIG2") => Ok(Filter::JBIG2Decode),
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
    jbig2_globals: Option<&[u8]>,
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
            #[cfg(feature = "jpx")]
            Filter::JPXDecode => decode_jpx(&data)?,
            #[cfg(not(feature = "jpx"))]
            Filter::JPXDecode => {
                return Err(PdfError::UnsupportedFilter("JPXDecode (disabled)".into()));
            }
            Filter::JBIG2Decode => decode_jbig2(&data, jbig2_globals)?,
        };
    }

    Ok(data)
}

/// FlateDecode (zlib/deflate).
fn decode_flate(data: &[u8], parms: Option<&PdfDict>) -> Result<Vec<u8>, PdfError> {
    // Try zlib first. If it ends with an error (truncated output),
    // also try raw deflate (skip 2-byte zlib header) and pick the longer result.
    let (zlib_output, zlib_clean, _) = decode_flate_inner(data, true);
    let output = if zlib_clean {
        zlib_output?
    } else {
        // Zlib hit an error (corrupt checksum, etc).  Try raw deflate (skip
        // 2-byte zlib header) and prefer it only when zlib clearly truncated
        // mid-stream.  If zlib consumed (nearly) all input, the data is
        // complete — the error is just a bad trailing checksum, and raw
        // deflate may decode garbage past the stream boundary.
        let zlib_data = zlib_output.unwrap_or_default();
        if data.len() > 2 {
            let (raw_output, _, _) = decode_flate_inner(&data[2..], false);
            let raw_data = raw_output.unwrap_or_default();
            if raw_data.len() > zlib_data.len()
                && raw_data[..zlib_data.len()] == zlib_data[..]
                && looks_like_valid_continuation(&raw_data, zlib_data.len())
            {
                // Raw produced more data, the shared prefix matches, and the
                // extra bytes look like valid content — zlib truncated early
                // due to a checksum error; use the fuller raw output.
                raw_data
            } else if !zlib_data.is_empty() {
                zlib_data
            } else if !raw_data.is_empty() {
                raw_data
            } else {
                return Err(PdfError::DecompressionError(
                    "flate: decompression failed".into(),
                ));
            }
        } else if !zlib_data.is_empty() {
            zlib_data
        } else {
            return Err(PdfError::DecompressionError(
                "flate: decompression failed".into(),
            ));
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

/// Check whether the extra bytes (past `start`) in `data` look like valid
/// stream content rather than garbage from decoding past a stream boundary.
/// Checks a sample of bytes for printable ASCII / whitespace, which is typical
/// for PDF content streams but not for accidentally-decoded binary data.
fn looks_like_valid_continuation(data: &[u8], start: usize) -> bool {
    if start >= data.len() {
        return false;
    }
    // Sample the first 64 bytes of the continuation
    let sample = &data[start..data.len().min(start + 64)];
    let printable = sample
        .iter()
        .filter(|&&b| b.is_ascii_graphic() || b.is_ascii_whitespace())
        .count();
    // If >80% of sampled bytes are printable, it's likely valid content
    printable * 5 >= sample.len() * 4
}

/// Inner flate decompression. `zlib` = true uses zlib wrapper, false uses raw deflate.
/// Returns (Result<data>, clean) where clean=true means StreamEnd was reached normally.
/// Returns (decompressed_data, clean_finish, bytes_consumed).
fn decode_flate_inner(data: &[u8], zlib: bool) -> (Result<Vec<u8>, PdfError>, bool, usize) {
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
                flate2::Status::StreamEnd => return (Ok(output), true, input_offset),
                flate2::Status::Ok | flate2::Status::BufError => {
                    if consumed == 0 && produced == 0 {
                        return (Ok(output), true, input_offset);
                    }
                }
            },
            Err(_) if !output.is_empty() => {
                // Partial output — checksum/trailing data error.
                return (Ok(output), false, input_offset);
            }
            Err(e) => {
                return (
                    Err(PdfError::DecompressionError(format!("flate: {e}"))),
                    false,
                    input_offset,
                );
            }
        }
    }
}

/// LZWDecode — native PDF-compatible LZW decoder.
///
/// Handles EarlyChange correctly and tolerates premature EOF (missing EOD code),
/// which is common in real-world PDFs.
fn decode_lzw(data: &[u8], parms: Option<&PdfDict>) -> Result<Vec<u8>, PdfError> {
    let early_change = parms.and_then(|p| p.get_int(b"EarlyChange")).unwrap_or(1) != 0;

    let output = lzw_decode(data, early_change)
        .ok_or_else(|| PdfError::DecompressionError("lzw: decode failed".into()))?;

    // Apply predictor if specified
    if let Some(parms) = parms {
        let predictor = parms.get_int(b"Predictor").unwrap_or(1);
        if predictor > 1 {
            return apply_predictor(&output, parms, predictor);
        }
    }

    Ok(output)
}

// --- Native PDF LZW decoder ---

const LZW_CLEAR_TABLE: usize = 256;
const LZW_EOD: usize = 257;
const LZW_MAX_ENTRIES: usize = 4096;
const LZW_INITIAL_SIZE: usize = 258;

/// Decode an LZW-compressed byte stream per the PDF spec.
fn lzw_decode(data: &[u8], early_change: bool) -> Option<Vec<u8>> {
    let mut table = LzwTable::new(early_change);
    let mut bit_size = table.code_length();
    let mut reader = LzwBitReader::new(data);
    let mut decoded = Vec::new();
    let mut prev: Option<usize> = None;

    loop {
        let next = match reader.read(bit_size) {
            Some(code) => code as usize,
            None => {
                // Premature EOF — missing EOD code. Return what we have.
                return Some(decoded);
            }
        };

        match next {
            LZW_CLEAR_TABLE => {
                table.clear();
                prev = None;
                bit_size = table.code_length();
            }
            LZW_EOD => return Some(decoded),
            new => {
                if new > table.size() {
                    // Invalid code — return partial data if we have any
                    if decoded.is_empty() {
                        return None;
                    }
                    return Some(decoded);
                }

                if new < table.size() {
                    let entry = table.get(new)?;
                    let first_byte = entry[0];
                    decoded.extend_from_slice(entry);

                    if let Some(prev_code) = prev {
                        table.register(prev_code, first_byte);
                    }
                } else if new == table.size() && prev.is_some() {
                    // KwKwK case: code references the entry about to be created
                    let prev_code = prev.unwrap();
                    let prev_entry = table.get(prev_code)?;
                    let first_byte = prev_entry[0];

                    let new_entry = table.register(prev_code, first_byte)?;
                    decoded.extend_from_slice(new_entry);
                } else {
                    if decoded.is_empty() {
                        return None;
                    }
                    return Some(decoded);
                }

                bit_size = table.code_length();
                prev = Some(new);
            }
        }
    }
}

/// LZW string table.
struct LzwTable {
    early_change: bool,
    entries: Vec<Option<Vec<u8>>>,
}

impl LzwTable {
    fn new(early_change: bool) -> Self {
        let mut entries: Vec<_> = (0..=255u8).map(|b| Some(vec![b])).collect();
        entries.push(None); // 256 = CLEAR_TABLE
        entries.push(None); // 257 = EOD
        Self {
            early_change,
            entries,
        }
    }

    fn push(&mut self, entry: Vec<u8>) -> Option<&[u8]> {
        if self.entries.len() >= LZW_MAX_ENTRIES {
            None
        } else {
            self.entries.push(Some(entry));
            self.entries.last()?.as_deref()
        }
    }

    fn register(&mut self, prev: usize, new_byte: u8) -> Option<&[u8]> {
        let prev_entry = self.get(prev)?;
        let mut new_entry = Vec::with_capacity(prev_entry.len() + 1);
        new_entry.extend(prev_entry);
        new_entry.push(new_byte);
        self.push(new_entry)
    }

    fn get(&self, index: usize) -> Option<&[u8]> {
        self.entries.get(index)?.as_deref()
    }

    fn clear(&mut self) {
        self.entries.truncate(LZW_INITIAL_SIZE);
    }

    fn size(&self) -> usize {
        self.entries.len()
    }

    fn code_length(&self) -> u8 {
        let adjusted = self.entries.len() + if self.early_change { 1 } else { 0 };
        if adjusted >= 2048 {
            12
        } else if adjusted >= 1024 {
            11
        } else if adjusted >= 512 {
            10
        } else {
            9
        }
    }
}

/// MSB-first bit reader for LZW.
struct LzwBitReader<'a> {
    data: &'a [u8],
    bit_pos: usize,
}

impl<'a> LzwBitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, bit_pos: 0 }
    }

    fn read(&mut self, bit_size: u8) -> Option<u32> {
        let byte_pos = self.bit_pos / 8;
        if byte_pos >= self.data.len() {
            return None;
        }
        let bit_offset = self.bit_pos % 8;
        let end_byte = (self.bit_pos + bit_size as usize - 1) / 8;

        // Read up to 8 bytes into a u64 for extraction
        let mut buf = [0u8; 8];
        for (i, b) in buf.iter_mut().enumerate().take(end_byte - byte_pos + 1) {
            *b = *self.data.get(byte_pos + i)?;
        }
        let bits = u64::from_be_bytes(buf);
        let shift = 64 - bit_offset - bit_size as usize;
        let mask = (1u64 << bit_size) - 1;
        let value = ((bits >> shift) & mask) as u32;

        self.bit_pos += bit_size as usize;
        Some(value)
    }
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

    // Work around jpeg_decoder bug: it checks component IDs (1,2,3) → YCbCr
    // before checking Adobe APP14 ColorTransform. When ColorTransform=0 (raw
    // RGB) is present but component IDs are (1,2,3), the decoder incorrectly
    // applies YCbCr→RGB conversion to already-RGB data. Detect this case and
    // override with ColorTransform::RGB.
    if has_adobe_rgb_marker(data) {
        decoder.set_color_transform(jpeg_decoder::ColorTransform::RGB);
    } else if needs_ycck_override(data) {
        decoder.set_color_transform(jpeg_decoder::ColorTransform::YCCK);
    }

    let pixels = decoder
        .decode()
        .map_err(|e| PdfError::DecompressionError(format!("DCTDecode: {e}")))?;

    // For 4-component (CMYK) JPEG, the jpeg_decoder applies a CMYK color
    // transform that inverts all channels (255-x). However, for PDF streams the
    // raw JPEG data is already in the correct byte order for the PDF /Decode
    // array to process. Undo the decoder's inversion so the PDF renderer gets
    // the original sample values.
    if let Some(info) = decoder.info()
        && info.pixel_format == jpeg_decoder::PixelFormat::CMYK32
    {
        let mut result = pixels;
        for b in result.iter_mut() {
            *b = 255 - *b;
        }
        return Ok(result);
    }

    Ok(pixels)
}

/// Check if a JPEG has Adobe APP14 ColorTransform=0 AND uniform sampling factors,
/// confirming the data is truly raw RGB (not YCbCr mislabeled with ColorTransform=0).
/// YCbCr JPEGs use chroma subsampling (e.g., Y=2×2, Cb/Cr=1×1) while RGB JPEGs
/// use uniform sampling (all components 1×1).
fn has_adobe_rgb_marker(data: &[u8]) -> bool {
    let mut has_ct0 = false;
    let mut uniform_sampling = false;
    let mut i = 2; // skip SOI
    while i + 4 < data.len() {
        if data[i] != 0xFF {
            break;
        }
        let marker = data[i + 1];
        if marker == 0xDA {
            break; // SOS — done with headers
        }
        let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        if i + 2 + len > data.len() {
            break;
        }
        // APP14 (Adobe) marker: check ColorTransform
        // Segment layout: length(2) + "Adobe"(5) + version(2) + flags0(2) + flags1(2) + CT(1) = 14
        if marker == 0xEE && len >= 14 {
            let color_transform = data[i + 2 + 13];
            has_ct0 = color_transform == 0;
        }
        // SOF0/SOF2: check sampling factors
        if (marker == 0xC0 || marker == 0xC2) && i + 9 < data.len() {
            let ncomp = data[i + 9] as usize;
            if ncomp == 3 && i + 10 + ncomp * 3 <= data.len() {
                let s0 = data[i + 11]; // component 0 sampling
                let s1 = data[i + 14]; // component 1 sampling
                let s2 = data[i + 17]; // component 2 sampling
                uniform_sampling = s0 == s1 && s1 == s2;
            }
        }
        i += 2 + len;
    }
    has_ct0 && uniform_sampling
}

/// Work around jpeg_decoder bug: it checks `"Adobe\0"` (6 bytes) in APP14
/// but the spec defines only 5-byte `"Adobe"`. The 6th byte is the high byte
/// of the version field. When version >= 256 (high byte != 0), jpeg_decoder
/// misses the APP14 marker entirely and misidentifies YCCK as plain CMYK.
/// Returns true when the last APP14 has ColorTransform=2 (YCCK) and jpeg_decoder
/// would fail to detect it.
fn needs_ycck_override(data: &[u8]) -> bool {
    let mut last_ct = None;
    let mut decoder_would_miss = false;
    let mut n_components = 0u8;
    let mut i = 2; // skip SOI
    while i + 4 < data.len() {
        if data[i] != 0xFF {
            break;
        }
        let marker = data[i + 1];
        if marker == 0xDA {
            break; // SOS
        }
        let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        if i + 2 + len > data.len() {
            break;
        }
        // APP14 (Adobe): "Adobe"(5) + version(2) + flags0(2) + flags1(2) + CT(1)
        if marker == 0xEE && len >= 14 && &data[i + 4..i + 9] == b"Adobe" {
            let ct = data[i + 2 + 13];
            last_ct = Some(ct);
            // jpeg_decoder checks data[0..6] == "Adobe\0", so byte index 5
            // (= data[i+9], the version high byte) must be 0 for it to detect.
            decoder_would_miss = data[i + 9] != 0;
        }
        // SOF0/SOF2: get number of components
        if (marker == 0xC0 || marker == 0xC2) && i + 9 < data.len() {
            n_components = data[i + 9];
        }
        i += 2 + len;
    }
    // Only override when: last APP14 says YCCK, jpeg_decoder would miss it,
    // and the JPEG has 4 components (CMYK/YCCK domain).
    last_ct == Some(2) && decoder_would_miss && n_components == 4
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

    let encoded_byte_align = parms
        .and_then(|p| match p.get(b"EncodedByteAlign") {
            Some(PdfObj::Bool(b)) => Some(*b),
            _ => None,
        })
        .unwrap_or(false);

    let encoding = if k < 0 {
        hayro_ccitt::EncodingMode::Group4
    } else if k == 0 {
        hayro_ccitt::EncodingMode::Group3_1D
    } else {
        hayro_ccitt::EncodingMode::Group3_2D { k: k as u32 }
    };

    let settings = hayro_ccitt::DecodeSettings {
        columns: columns as u32,
        rows: if rows_limit > 0 { rows_limit } else { u32::MAX },
        end_of_block,
        end_of_line: false,
        rows_are_byte_aligned: encoded_byte_align,
        encoding,
        invert_black: false,
    };

    decode_ccitt_hayro(data, &settings, black_is1)
}

/// A byte-oriented CCITT pixel decoder used by hayro-ccitt.
/// Packs decoded pixels into bytes (MSB first), with `black_is1` polarity control.
struct CcittByteDecoder {
    output: Vec<u8>,
    current_byte: u8,
    bit_pos: u8,
    black_is1: bool,
}

impl CcittByteDecoder {
    fn new(black_is1: bool) -> Self {
        Self {
            output: Vec::new(),
            current_byte: 0,
            bit_pos: 0,
            black_is1,
        }
    }

    fn flush_byte(&mut self) {
        if self.bit_pos > 0 {
            // Shift remaining bits to MSB position and pad
            let remaining = 8 - self.bit_pos;
            self.current_byte <<= remaining;
            if !self.black_is1 {
                // Pad unfilled bits as white (1)
                self.current_byte |= (1u8 << remaining) - 1;
            }
            self.output.push(self.current_byte);
            self.current_byte = 0;
            self.bit_pos = 0;
        }
    }
}

impl hayro_ccitt::Decoder for CcittByteDecoder {
    fn push_pixel(&mut self, white: bool) {
        // black_is1=true: black=1, white=0
        // black_is1=false: black=0, white=1
        let bit = if self.black_is1 { !white } else { white };
        self.current_byte = (self.current_byte << 1) | (bit as u8);
        self.bit_pos += 1;
        if self.bit_pos == 8 {
            self.output.push(self.current_byte);
            self.current_byte = 0;
            self.bit_pos = 0;
        }
    }

    fn push_pixel_chunk(&mut self, white: bool, chunk_count: u32) {
        // If there are partial bits pending, we can't directly push bytes —
        // the bit boundary wouldn't align. Fall back to pixel-by-pixel.
        if self.bit_pos != 0 {
            for _ in 0..chunk_count * 8 {
                self.push_pixel(white);
            }
            return;
        }
        let byte = if (self.black_is1 && !white) || (!self.black_is1 && white) {
            0xFF
        } else {
            0x00
        };
        for _ in 0..chunk_count {
            self.output.push(byte);
        }
    }

    fn next_line(&mut self) {
        self.flush_byte();
    }
}

/// Decode CCITT data using hayro-ccitt (supports Group 3 and Group 4).
fn decode_ccitt_hayro(
    data: &[u8],
    settings: &hayro_ccitt::DecodeSettings,
    black_is1: bool,
) -> Result<Vec<u8>, PdfError> {
    let mut decoder = CcittByteDecoder::new(black_is1);
    if let Err(e) = hayro_ccitt::decode(data, &mut decoder, settings) {
        // UnexpectedEof is normal for inline images and streams without EOFB markers.
        // Only warn about genuine decoding errors (InvalidCode, LineLengthMismatch, etc.)
        if e != hayro_ccitt::DecodeError::UnexpectedEof {
            eprintln!("[CCITT] decode warning: {} (using partial data)", e);
        }
    }
    Ok(decoder.output)
}

/// JBIG2Decode.
fn decode_jbig2(data: &[u8], globals: Option<&[u8]>) -> Result<Vec<u8>, PdfError> {
    let image = hayro_jbig2::decode_embedded(data, globals)
        .map_err(|e| PdfError::DecompressionError(format!("JBIG2: {e}")))?;
    // Convert Vec<bool> to packed bytes (8 pixels/byte, MSB first)
    // JBIG2: true = black, false = white
    // PDF DeviceGray: 0 = black, 1 = white
    // So: start all-white (0xFF), clear bits for black pixels
    let row_bytes = (image.width as usize).div_ceil(8);
    let mut packed = vec![0xFFu8; row_bytes * image.height as usize];
    for y in 0..image.height as usize {
        for x in 0..image.width as usize {
            if image.data[y * image.width as usize + x] {
                packed[y * row_bytes + x / 8] &= !(0x80 >> (x % 8));
            }
        }
    }
    Ok(packed)
}

/// JPXDecode (JPEG 2000).
///
/// Uses hayro-jpeg2000 to decode JP2 or raw J2K codestreams into interleaved pixel data.
#[cfg(feature = "jpx")]
fn decode_jpx(data: &[u8]) -> Result<Vec<u8>, PdfError> {
    if data.is_empty() {
        return Ok(Vec::new());
    }

    let image = hayro_jpeg2000::Image::new(data, &hayro_jpeg2000::DecodeSettings::default())
        .map_err(|e| PdfError::DecompressionError(format!("JPXDecode: {e}")))?;

    image
        .decode()
        .map_err(|e| PdfError::DecompressionError(format!("JPXDecode: {e}")))
}

/// Query the number of color channels (excluding alpha) and whether alpha is
/// present in a JPEG 2000 image, without fully decoding the pixel data.
/// Returns `(color_channels, has_alpha)`.
#[cfg(feature = "jpx")]
pub fn jpx_color_info(data: &[u8]) -> Option<(u8, bool)> {
    let image = hayro_jpeg2000::Image::new(data, &hayro_jpeg2000::DecodeSettings::default()).ok()?;
    Some((image.color_space().num_channels(), image.has_alpha()))
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
        if bpc < 8 {
            // Sub-byte samples: operate at sample level, not byte level
            apply_tiff_predictor_subbyte(data, columns, colors, bpc, row_bytes)
        } else {
            apply_tiff_predictor(data, row_bytes, bytes_per_pixel)
        }
    } else if predictor >= 10 {
        // PNG predictors
        apply_png_predictor(data, row_bytes, bytes_per_pixel)
    } else {
        Ok(data.to_vec())
    }
}

/// TIFF predictor 2 for sub-byte samples (BPC = 1, 2, or 4).
/// Operates at the individual sample level within packed bytes.
fn apply_tiff_predictor_subbyte(
    data: &[u8],
    columns: usize,
    colors: usize,
    bpc: usize,
    row_bytes: usize,
) -> Result<Vec<u8>, PdfError> {
    let samples_per_row = columns * colors;
    let mask = (1u8 << bpc) - 1; // e.g., 1 for bpc=1, 3 for bpc=2, 15 for bpc=4
    let mut result = Vec::with_capacity(data.len());

    for row in data.chunks(row_bytes) {
        let mut out_row = vec![0u8; row.len()];
        // Copy the raw bytes first, then undo differencing at sample level
        out_row[..row.len()].copy_from_slice(row);

        // Extract all samples, undo differencing, re-pack
        let mut prev = vec![0u8; colors];
        for col in 0..columns {
            for c in 0..colors {
                let sample_idx = col * colors + c;
                if sample_idx >= samples_per_row {
                    break;
                }
                let bit_offset = sample_idx * bpc;
                let byte_idx = bit_offset / 8;
                let bit_pos = 8 - bpc - (bit_offset % 8); // MSB-first packing
                if byte_idx >= row.len() {
                    break;
                }
                let encoded = (row[byte_idx] >> bit_pos) & mask;
                let decoded = (encoded.wrapping_add(prev[c])) & mask;
                prev[c] = decoded;
                // Write back
                out_row[byte_idx] = (out_row[byte_idx] & !(mask << bit_pos)) | (decoded << bit_pos);
            }
        }
        result.extend_from_slice(&out_row);
    }

    Ok(result)
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

    // Detect data that lacks predictor bytes despite DecodeParms claiming them.
    // If data divides evenly into row_bytes but NOT into stride, the stream
    // was written without per-row predictor prefixes — return as-is.
    if row_bytes > 0
        && !data.is_empty()
        && data.len().is_multiple_of(row_bytes)
        && !data.len().is_multiple_of(stride)
    {
        return Ok(data.to_vec());
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
        let decoded = decode_stream(hex.as_bytes(), &filters, &parms, None).unwrap();
        assert_eq!(&decoded, original);
    }
}
