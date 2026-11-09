// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! CCITT Group 3 and Group 4 fax decoding, shared by the PostScript
//! `CCITTFaxDecode` filter and the PDF reader, so a fax image gives the same
//! samples in both.
//!
//! `hayro-ccitt` decodes first; when it rejects the data with a hard error,
//! the more lenient `fax` crate is tried, and whichever gives more output is
//! kept. Old Acrobat Distiller versions wrote Group 4 streams that hayro's
//! position checks stop on part-way although the image is still decodable.

/// A `CCITTFaxDecode` filter's parameters, as PLRM 3e Table 3.20 and ISO
/// 32000-1 Table 11 give them.
#[derive(Clone, Copy, Debug)]
pub struct CcittParams {
    /// `K`: below 0 for Group 4, 0 for Group 3 one-dimensional, above 0 for
    /// Group 3 mixed one- and two-dimensional.
    pub k: i32,
    /// `Columns`: the image's width in pixels.
    pub columns: u32,
    /// `Rows`: the image's height in pixels, 0 when not given.
    pub rows: u32,
    /// `EndOfBlock`: whether the data may end with an end-of-block code.
    pub end_of_block: bool,
    /// `EncodedByteAlign`: whether each row's data starts on a byte.
    pub encoded_byte_align: bool,
    /// `BlackIs1`: whether a black pixel is a 1 bit in the output.
    pub black_is1: bool,
}

/// How decoding ended.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CcittEnd {
    /// The image ended after this many bytes of the input: through its
    /// end-of-block code, or its last row, to the next whole byte. The bytes
    /// after it are not the image's.
    Finished(usize),
    /// The input ran out before the image ended.
    RanOut,
    /// The data is malformed; the rows decoded before the error are kept.
    Malformed,
}

/// Something decoding had to work around, for a caller that reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CcittProblem {
    /// Whether part of the image is missing, or only the second decoder
    /// was needed.
    pub lost_data: bool,
    /// What happened, for a warning.
    pub message: String,
}

/// A decoded fax image.
#[derive(Clone, Debug)]
pub struct CcittImage {
    /// One bit per pixel, most significant first, each row padded to a
    /// byte; a black pixel is 0 unless `BlackIs1` is true.
    pub data: Vec<u8>,
    /// How decoding ended.
    pub end: CcittEnd,
    /// What decoding had to work around, if anything.
    pub problem: Option<CcittProblem>,
}

/// Decode CCITT fax data.
pub fn decode(data: &[u8], params: &CcittParams) -> CcittImage {
    let encoding = if params.k < 0 {
        hayro_ccitt::EncodingMode::Group4
    } else if params.k == 0 {
        hayro_ccitt::EncodingMode::Group3_1D
    } else {
        hayro_ccitt::EncodingMode::Group3_2D { k: params.k as u32 }
    };
    let settings = hayro_ccitt::DecodeSettings {
        columns: params.columns,
        rows: if params.rows > 0 {
            params.rows
        } else {
            u32::MAX
        },
        end_of_block: params.end_of_block,
        // Producers are sloppy with `EndOfLine`; hayro reads an EOL
        // wherever one is.
        end_of_line: false,
        rows_are_byte_aligned: params.encoded_byte_align,
        encoding,
        invert_black: false,
    };

    let mut decoder = ByteDecoder::new(params.black_is1);
    let mut ctx = hayro_ccitt::DecoderContext::new(settings);
    let err = match hayro_ccitt::decode(data, &mut decoder, &mut ctx) {
        Ok(mut consumed) => {
            if params.k > 0 && params.end_of_block {
                consumed = skip_rest_of_rtc_2d(data, consumed);
            }
            return CcittImage {
                data: decoder.output,
                end: CcittEnd::Finished(consumed),
                problem: None,
            };
        }
        Err(e) => e,
    };
    if err == hayro_ccitt::DecodeError::UnexpectedEof {
        return CcittImage {
            data: decoder.output,
            end: CcittEnd::RanOut,
            problem: None,
        };
    }

    // A hard error: try `fax`, and keep whichever decoder gave more.
    let fallback = decode_with_fax(data, &settings, params.black_is1);
    if fallback.len() > decoder.output.len() {
        return CcittImage {
            data: fallback,
            end: CcittEnd::Malformed,
            problem: Some(CcittProblem {
                lost_data: false,
                message: format!("CCITT image: {err}; decoded by the more lenient decoder instead"),
            }),
        };
    }
    CcittImage {
        data: decoder.output,
        end: CcittEnd::Malformed,
        problem: Some(CcittProblem {
            lost_data: true,
            message: format!("CCITT image: {err}; only the part before the error is drawn"),
        }),
    }
}

/// Where a Group 3 two-dimensional image's end-of-block code ends, given
/// where hayro-ccitt stopped reading.
///
/// That code (RTC) is six EOLs each followed by a tag bit,
/// `0000000000011` six times. hayro-ccitt reads one EOL, takes the tag bit
/// after it for the start of something else, and stops, so `from`, rounded
/// up to a byte, falls inside the code. This finds the end of the 13-bit
/// units that follow, entered at any point of one, and asks for four whole
/// units so that bytes after the image are never taken for them.
fn skip_rest_of_rtc_2d(data: &[u8], from: usize) -> usize {
    const UNIT: [u8; 13] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1];
    const LEAST: usize = 4 * UNIT.len();
    let tail = &data[from.min(data.len())..data.len().min(from + 10)];
    let bit = |i: usize| (tail[i / 8] >> (7 - i % 8)) & 1;
    let mut end = 0;
    for phase in 0..UNIT.len() {
        let mut i = 0;
        while i < tail.len() * 8 && bit(i) == UNIT[(phase + i) % UNIT.len()] {
            i += 1;
            if (phase + i) % UNIT.len() == 0 {
                end = end.max(i);
            }
        }
    }
    if end < LEAST {
        return from;
    }
    from + end.div_ceil(8)
}

/// Packs hayro-ccitt's pixel runs into bytes, most significant bit first.
struct ByteDecoder {
    output: Vec<u8>,
    current_byte: u8,
    bit_pos: u8,
    black_is1: bool,
}

impl ByteDecoder {
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

    /// Append `count` bits of the same value, MSB first.
    fn push_bits(&mut self, bit: bool, mut count: u32) {
        // Finish the byte in progress.
        while self.bit_pos != 0 && count > 0 {
            self.current_byte = (self.current_byte << 1) | (bit as u8);
            self.bit_pos += 1;
            if self.bit_pos == 8 {
                self.output.push(self.current_byte);
                self.current_byte = 0;
                self.bit_pos = 0;
            }
            count -= 1;
        }
        // Whole bytes.
        let whole = (count / 8) as usize;
        if whole > 0 {
            let byte = if bit { 0xFF } else { 0x00 };
            self.output.resize(self.output.len() + whole, byte);
        }
        // Start the next byte with what is left.
        let rest = (count % 8) as u8;
        if rest > 0 {
            self.current_byte = if bit { (1u8 << rest) - 1 } else { 0 };
            self.bit_pos = rest;
        }
    }
}

impl hayro_ccitt::Decoder for ByteDecoder {
    fn push_pixels(&mut self, white: bool, count: u32) {
        // black_is1=true: black=1, white=0
        // black_is1=false: black=0, white=1
        let bit = if self.black_is1 { !white } else { white };
        self.push_bits(bit, count);
    }

    fn next_line(&mut self) {
        self.flush_byte();
    }
}

/// Decode CCITT data with the `fax` crate, in the same layout and
/// polarity as [`ByteDecoder`].
fn decode_with_fax(
    data: &[u8],
    settings: &hayro_ccitt::DecodeSettings,
    black_is1: bool,
) -> Vec<u8> {
    let width = settings.columns as u16;
    let row_bytes = settings.columns.div_ceil(8) as usize;
    let mut out: Vec<u8> = Vec::new();
    // Byte value for a full chunk of "white" and "black" pixels after polarity.
    // black_is1=false (the default): 0=black, 1=white → white row = 0xFF, black = 0x00
    // black_is1=true: 0=white, 1=black → white row = 0x00, black = 0xFF
    let white_byte: u8 = if black_is1 { 0x00 } else { 0xFF };
    let black_byte: u8 = !white_byte;

    let rows_limit = if settings.rows == u32::MAX || settings.rows == 0 {
        None
    } else {
        Some(settings.rows.min(u16::MAX as u32) as u16)
    };

    let mut emit_row = |transitions: &[u16]| {
        // Rebuild one packed row from the transition list.
        let mut row = vec![white_byte; row_bytes];
        // Row starts white; each transition flips color starting at that index.
        let mut color_white = true;
        let mut cursor: u16 = 0;
        // Add the sentinel `width` transition so we close the final run.
        let iter = transitions.iter().copied().chain(std::iter::once(width));
        for next in iter {
            let end = next.min(width);
            if !color_white && end > cursor {
                fill_bits(&mut row, cursor as usize, end as usize, black_byte != 0);
            }
            color_white = !color_white;
            cursor = end;
            if cursor >= width {
                break;
            }
        }
        out.extend_from_slice(&row);
    };

    match settings.encoding {
        hayro_ccitt::EncodingMode::Group4 => {
            let _ = fax::decoder::decode_g4(data.iter().copied(), width, rows_limit, &mut emit_row);
        }
        hayro_ccitt::EncodingMode::Group3_1D | hayro_ccitt::EncodingMode::Group3_2D { .. } => {
            let _ = fax::decoder::decode_g3(data.iter().copied(), &mut emit_row);
        }
    }

    // Pad truncated output with white scanlines so downstream image handling
    // sees the full-height buffer. Without this, a Group 4 stream that the
    // decoder can't finish would produce a buffer short by thousands of
    // bytes; the image code fills the missing rows with zeros, which lands as
    // a solid black rectangle covering part of the page.
    if let Some(target_rows) = rows_limit {
        let expected = row_bytes * target_rows as usize;
        if out.len() < expected {
            out.resize(expected, white_byte);
        }
    }

    out
}

/// Flip bits in a byte-packed (MSB-first) row between `[start, end)` to black.
/// `start`/`end` are pixel indices; the buffer is pre-filled with the "white"
/// polarity, so this routine only needs to set the black-colored runs.
fn fill_bits(row: &mut [u8], start: usize, end: usize, black_is_one: bool) {
    if end <= start {
        return;
    }
    for x in start..end {
        let byte = x / 8;
        let bit = 0x80u8 >> (x % 8);
        if black_is_one {
            row[byte] |= bit;
        } else {
            row[byte] &= !bit;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `bits` as bytes, padded with zeros.
    fn pack(bits: &str) -> Vec<u8> {
        let bits: Vec<u8> = bits.bytes().map(|b| b - b'0').collect();
        bits.chunks(8)
            .map(|c| c.iter().enumerate().fold(0, |v, (i, &b)| v | b << (7 - i)))
            .collect()
    }

    /// Wherever hayro-ccitt's stop falls inside the code, the rest of it is
    /// taken, and the bytes after it are left.
    #[test]
    fn the_rest_of_a_2d_rtc_is_skipped_from_any_point() {
        let rtc = "0000000000011".repeat(6);
        for skip in 0..14 {
            let mut bits = rtc[skip..].to_string();
            bits.push_str(&"0".repeat((8 - bits.len() % 8) % 8));
            let mut data = pack(&bits);
            let end = data.len();
            data.extend_from_slice(b"rest");
            assert_eq!(skip_rest_of_rtc_2d(&data, 0), end, "entered {skip} bits in");
        }
    }

    /// Bytes that are not most of an RTC are not taken for one.
    #[test]
    fn other_bytes_are_not_an_rtc() {
        for data in [
            &b"0grestore"[..],
            b"\ngrestore",
            &[0x00, 0x18, 0x00, 0xC0, 0x41],
        ] {
            assert_eq!(skip_rest_of_rtc_2d(data, 0), 0, "{data:?}");
        }
    }
}
