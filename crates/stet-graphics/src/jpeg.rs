// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! JPEG (`DCTDecode`) decoding, shared by the PostScript interpreter's
//! `DCTDecode` filter and the PDF reader, so that a JPEG decodes to the same
//! samples whichever of them reads it.
//!
//! It lived in the PDF reader, where it gathered the workarounds real files
//! need; the PostScript filter called `jpeg_decoder` bare and got a CMYK
//! JPEG's samples inverted, so an Adobe CMYK image `pdftops` wrote came out
//! as its negative — a white page black.

/// A JPEG that could not be decoded, carrying the decoder's message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JpegError(String);

impl std::fmt::Display for JpegError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for JpegError {}

/// Decode a JPEG (`DCTDecode`) to its samples: one byte per component, rows
/// top to bottom, components interleaved.
///
/// A CMYK JPEG's samples come back as the file stores them. Adobe's encoders
/// store CMYK inverted, and what a PostScript or PDF `/Decode` array is
/// written against is that stored value, so `jpeg_decoder`'s own inversion is
/// undone. Ghostscript's `DCTDecode` gives the same bytes. Past the plain
/// decode, this works around `jpeg_decoder`'s misreadings of Adobe's RGB and
/// YCCK markers, and recovers 2-component JPEGs, ones whose height comes in a
/// DNL marker, and truncated ones.
pub fn decode(data: &[u8]) -> Result<Vec<u8>, JpegError> {
    use jpeg_decoder::Decoder;

    // wasm32: prefer zune-jpeg over jpeg-decoder as the primary decoder.
    // jpeg-decoder's wasm SIMD IDCT path traps (raw `unreachable`, bypasses
    // std::panic::set_hook) on JPEGs whose bitstream ends early — the native
    // scalar reader returns a clean `Err("failed to fill whole buffer")`
    // which the fallback chain below catches, but on wasm32 the SIMD path
    // writes past the end of an intermediate buffer before the stream check
    // fires, tripping wasm's bounds check. Starting with zune avoids that
    // code path entirely for the most common JPEGs. jpeg-decoder is still
    // tried below as a secondary fallback (e.g. for Adobe YCCK streams
    // where zune's color transform would give wrong results).
    #[cfg(target_arch = "wasm32")]
    if let Some(pixels) = decode_dct_via_zune(data) {
        return Ok(pixels);
    }

    let mut decoder = Decoder::new(data);

    // Work around jpeg_decoder bug: it checks component IDs (1,2,3) → YCbCr
    // before checking Adobe APP14 ColorTransform. When ColorTransform=0 (raw
    // RGB) is present but component IDs are (1,2,3), the decoder incorrectly
    // applies YCbCr→RGB conversion to already-RGB data. Detect this case and
    // override with ColorTransform::RGB.
    if has_adobe_rgb_marker(data) || is_raw_rgb_jpeg(data) {
        decoder.set_color_transform(jpeg_decoder::ColorTransform::RGB);
    } else if needs_ycck_override(data) {
        decoder.set_color_transform(jpeg_decoder::ColorTransform::YCCK);
    }

    let pixels = match decoder.decode() {
        Ok(p) => p,
        Err(e) => {
            // jpeg_decoder doesn't support 2-component JPEGs (DeviceN spot
            // color images). Fall back to zune-jpeg which handles arbitrary
            // component counts.
            if let Some(pixels) = decode_dct_zune(data) {
                return Ok(pixels);
            }
            // Some JPEGs use DNL (Define Number of Lines) markers to specify
            // the height after encoding. Patch the SOF header with the DNL
            // height and retry.
            if let Some(patched) = patch_jpeg_dnl_height(data) {
                return decode(&patched);
            }
            // Truncated JPEGs: try tolerant decode that returns partial data.
            if let Some(pixels) = decode_dct_tolerant(data) {
                return Ok(pixels);
            }
            // Last resort: append EOI marker to truncated JPEG and retry.
            // jpeg-decoder may succeed when the stream is terminated properly.
            {
                let mut padded = data.to_vec();
                // Strip any partial marker at end, then add EOI
                if padded.last() == Some(&0xFF) {
                    padded.pop();
                }
                padded.extend_from_slice(&[0xFF, 0xD9]);
                let mut retry_dec = Decoder::new(&padded[..]);
                if has_adobe_rgb_marker(&padded) || is_raw_rgb_jpeg(&padded) {
                    retry_dec.set_color_transform(jpeg_decoder::ColorTransform::RGB);
                } else if needs_ycck_override(&padded) {
                    retry_dec.set_color_transform(jpeg_decoder::ColorTransform::YCCK);
                }
                if let Ok(pixels) = retry_dec.decode() {
                    // Apply same CMYK inversion as the normal path
                    if let Some(info) = retry_dec.info()
                        && info.pixel_format == jpeg_decoder::PixelFormat::CMYK32
                    {
                        let mut result = pixels;
                        for b in result.iter_mut() {
                            *b = 255 - *b;
                        }
                        return Ok(result);
                    }
                    return Ok(pixels);
                }
            }
            return Err(JpegError(e.to_string()));
        }
    };

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

/// Run a closure that may panic, suppressing the panic message and returning
/// `None` on panic.  Used for third-party JPEG decoders that can panic on
/// malformed input.
fn catch_silent<F, T>(f: F) -> Option<T>
where
    F: FnOnce() -> Option<T> + std::panic::UnwindSafe,
{
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(f).ok().flatten();
    std::panic::set_hook(prev);
    result
}

/// Primary JPEG decoder on wasm32. Uses zune-jpeg directly (no `catch_silent`
/// wrapper — `catch_unwind` is a no-op under `panic=abort` and the hook swap
/// would clobber the WASM panic hook). Picks the output colorspace from the
/// SOF component count so 1/3/4-component JPEGs all decode to their natural
/// format. Returns `None` if zune-jpeg can't decode — caller falls back to
/// jpeg-decoder.
#[cfg(target_arch = "wasm32")]
fn decode_dct_via_zune(data: &[u8]) -> Option<Vec<u8>> {
    use zune_jpeg::JpegDecoder;
    let n_comps = jpeg_dimensions_and_components(data)
        .map(|(_, _, n)| n)
        .unwrap_or(3);
    let out_cs = match n_comps {
        1 => zune_core::colorspace::ColorSpace::Luma,
        4 => zune_core::colorspace::ColorSpace::CMYK,
        _ => zune_core::colorspace::ColorSpace::RGB,
    };
    let options = zune_core::options::DecoderOptions::default().jpeg_set_out_colorspace(out_cs);
    let mut decoder = JpegDecoder::new_with_options(std::io::Cursor::new(data), options);
    decoder.decode().ok()
}

/// Fallback JPEG decoder using zune-jpeg for component counts that
/// jpeg_decoder doesn't support (e.g., 2-component DeviceN images).
fn decode_dct_zune(data: &[u8]) -> Option<Vec<u8>> {
    use zune_jpeg::JpegDecoder;
    // Request raw 2-component output (LumaA) to avoid unwanted color
    // conversion. PDF DeviceN images need the original channel values
    // for the tinting function.
    let data = data.to_vec();
    catch_silent(move || {
        let options = zune_core::options::DecoderOptions::default()
            .jpeg_set_out_colorspace(zune_core::colorspace::ColorSpace::LumaA);
        let mut decoder = JpegDecoder::new_with_options(std::io::Cursor::new(&data), options);
        decoder.decode().ok()
    })
}

/// Tolerant JPEG decoder for truncated streams.
/// Returns partial pixel data for whatever MCU rows decoded successfully.
/// Applies the same CMYK channel inversion as the primary decoder path.
fn decode_dct_tolerant(data: &[u8]) -> Option<Vec<u8>> {
    // Detect component count from SOF to set the right output colorspace.
    // Without this, zune-jpeg converts CMYK to RGB, producing wrong data.
    let n_comps = jpeg_dimensions_and_components(data)
        .map(|(_, _, n)| n)
        .unwrap_or(3);
    let data = data.to_vec();
    catch_silent(move || {
        use zune_jpeg::JpegDecoder;
        let out_cs = match n_comps {
            1 => zune_core::colorspace::ColorSpace::Luma,
            4 => zune_core::colorspace::ColorSpace::CMYK,
            _ => zune_core::colorspace::ColorSpace::RGB,
        };
        let options = zune_core::options::DecoderOptions::default()
            .set_strict_mode(false)
            .jpeg_set_out_colorspace(out_cs);
        let mut decoder = JpegDecoder::new_with_options(std::io::Cursor::new(&data), options);
        decoder.decode().ok()
    })
}

/// Patch a JPEG that uses DNL (Define Number of Lines, marker 0xFFDC) to specify
/// its height. Finds the DNL marker, extracts the height, writes it into the SOF
/// header, and strips the DNL marker from the scan data so standard decoders can
/// parse it.
fn patch_jpeg_dnl_height(data: &[u8]) -> Option<Vec<u8>> {
    // Find DNL marker (0xFF 0xDC) and extract height
    let dnl_height = {
        let mut pos = 0;
        let mut found = None;
        while pos + 4 < data.len() {
            if data[pos] == 0xFF && data[pos + 1] == 0xDC {
                // DNL: FF DC 00 04 <height_hi> <height_lo>
                if pos + 5 < data.len() {
                    let h = ((data[pos + 4] as u16) << 8) | data[pos + 5] as u16;
                    found = Some((pos, h));
                }
                break;
            }
            pos += 1;
        }
        found
    };
    let (dnl_pos, height) = dnl_height?;
    if height == 0 {
        return None;
    }

    // Find SOF marker (0xFFC0..0xFFC3) and patch height field
    let mut patched = data.to_vec();
    let mut pos = 2; // skip SOI
    while pos + 8 < patched.len() {
        if patched[pos] != 0xFF {
            pos += 1;
            continue;
        }
        let marker = patched[pos + 1];
        if (0xC0..=0xC3).contains(&marker) {
            // SOF: FF Cn LL LL PP HH HH WW WW ...
            // Height is at offset +5 (2 bytes, big-endian)
            patched[pos + 5] = (height >> 8) as u8;
            patched[pos + 6] = (height & 0xFF) as u8;
            break;
        }
        if marker == 0xDA {
            break; // SOS — stop before scan data
        }
        // Skip marker segment
        if pos + 3 < patched.len() {
            let seg_len = ((patched[pos + 2] as usize) << 8) | patched[pos + 3] as usize;
            pos += 2 + seg_len;
        } else {
            break;
        }
    }

    // Remove the DNL marker (6 bytes: FF DC 00 04 HH HH)
    if dnl_pos + 6 <= patched.len() {
        patched.drain(dnl_pos..dnl_pos + 6);
    }

    Some(patched)
}

/// Patch the SOF height field in raw JPEG data.
/// Used when the SOF header has a streaming-encoder placeholder height (e.g.
/// 60000) that exceeds the PDF dict's authoritative /Height value.
pub fn patch_jpeg_sof_height(data: &mut [u8], new_height: u16) {
    if data.len() < 2 || data[0] != 0xFF || data[1] != 0xD8 {
        return;
    }
    let mut pos = 2;
    while pos + 4 < data.len() {
        if data[pos] != 0xFF {
            pos += 1;
            continue;
        }
        let marker = data[pos + 1];
        if (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xC8 && marker != 0xCC {
            if pos + 6 < data.len() {
                data[pos + 5] = (new_height >> 8) as u8;
                data[pos + 6] = (new_height & 0xFF) as u8;
            }
            return;
        }
        if marker == 0xDA {
            return; // SOS — too late
        }
        let seg_len = if pos + 3 < data.len() {
            ((data[pos + 2] as usize) << 8) | data[pos + 3] as usize
        } else {
            return;
        };
        pos += 2 + seg_len;
    }
}

/// Extract width, height, and component count from a JPEG SOF header.
fn jpeg_dimensions_and_components(data: &[u8]) -> Option<(u32, u32, u8)> {
    if data.len() < 2 || data[0] != 0xFF || data[1] != 0xD8 {
        return None;
    }
    let mut pos = 2;
    while pos + 4 < data.len() {
        if data[pos] != 0xFF {
            pos += 1;
            continue;
        }
        let marker = data[pos + 1];
        if (0xC0..=0xCF).contains(&marker)
            && marker != 0xC4
            && marker != 0xC8
            && marker != 0xCC
            && pos + 9 < data.len()
        {
            let h = ((data[pos + 5] as u32) << 8) | data[pos + 6] as u32;
            let w = ((data[pos + 7] as u32) << 8) | data[pos + 8] as u32;
            let n = data[pos + 9];
            return Some((w, h, n));
        }
        if marker == 0xDA {
            break;
        }
        let seg_len = ((data[pos + 2] as usize) << 8) | data[pos + 3] as usize;
        pos += 2 + seg_len;
    }
    None
}

/// Extract image dimensions from a JPEG's SOF marker.
/// Returns `(width, height)` if found.
///
/// When the JPEG uses DNL (Define Number of Lines, marker 0xFFDC) — indicated by
/// a dummy SOF height of 0 or 0xFFFF — scans the bitstream for the DNL marker
/// and returns its height instead.
pub fn jpeg_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    if data.len() < 2 || data[0] != 0xFF || data[1] != 0xD8 {
        return None;
    }
    let mut pos = 2;
    while pos + 4 < data.len() {
        if data[pos] != 0xFF {
            pos += 1;
            continue;
        }
        let marker = data[pos + 1];
        // SOF markers: 0xC0-0xCF except 0xC4 (DHT), 0xC8 (JPG), 0xCC (DAC)
        if (0xC0..=0xCF).contains(&marker)
            && marker != 0xC4
            && marker != 0xC8
            && marker != 0xCC
            && pos + 9 < data.len()
        {
            let mut h = ((data[pos + 5] as u32) << 8) | data[pos + 6] as u32;
            let w = ((data[pos + 7] as u32) << 8) | data[pos + 8] as u32;
            // SOF height 0 or 0xFFFF means "defined by DNL marker later"
            if (h == 0 || h == 0xFFFF)
                && let Some(dnl_h) = find_dnl_height(data)
            {
                h = dnl_h as u32;
            }
            return Some((w, h));
        }
        if marker == 0xDA {
            break; // SOS — no more markers
        }
        let seg_len = ((data[pos + 2] as usize) << 8) | data[pos + 3] as usize;
        pos += 2 + seg_len;
    }
    None
}

/// Scan JPEG data for a DNL (Define Number of Lines) marker and return its height.
fn find_dnl_height(data: &[u8]) -> Option<u16> {
    let mut pos = 0;
    while pos + 5 < data.len() {
        if data[pos] == 0xFF && data[pos + 1] == 0xDC && pos + 5 < data.len() {
            return Some(((data[pos + 4] as u16) << 8) | data[pos + 5] as u16);
        }
        pos += 1;
    }
    None
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

/// Detect raw RGB JPEGs that have no APP14/JFIF markers and non-standard
/// component IDs (e.g. 0,1,2 instead of the YCbCr standard 1,2,3).
/// These JPEGs store raw RGB data — applying YCbCr→RGB conversion produces
/// completely wrong colors (e.g. blue → magenta).
fn is_raw_rgb_jpeg(data: &[u8]) -> bool {
    let mut has_jfif = false;
    let mut has_adobe = false;
    let mut non_standard_ids = false;
    let mut uniform_sampling = false;
    let mut n_components = 0u8;
    let mut i = 2; // skip SOI
    while i + 4 < data.len() {
        if data[i] != 0xFF {
            break;
        }
        let marker = data[i + 1];
        if marker == 0xDA {
            break;
        }
        let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        if i + 2 + len > data.len() {
            break;
        }
        if marker == 0xE0 && len >= 7 && &data[i + 4..i + 9] == b"JFIF\x00" {
            has_jfif = true;
        }
        if marker == 0xEE && len >= 7 && &data[i + 4..i + 9] == b"Adobe" {
            has_adobe = true;
        }
        if (marker == 0xC0 || marker == 0xC2) && i + 9 < data.len() {
            n_components = data[i + 9];
            if n_components == 3 && i + 10 + 9 <= data.len() {
                let id0 = data[i + 10];
                let id1 = data[i + 13];
                let id2 = data[i + 16];
                // Standard YCbCr uses IDs (1,2,3). Anything else suggests raw RGB.
                non_standard_ids = !(id0 == 1 && id1 == 2 && id2 == 3);
                let s0 = data[i + 11];
                let s1 = data[i + 14];
                let s2 = data[i + 17];
                uniform_sampling = s0 == s1 && s1 == s2;
            }
        }
        i += 2 + len;
    }
    // Raw RGB: 3 components, non-standard IDs, uniform sampling, no JFIF/Adobe markers
    n_components == 3 && non_standard_ids && uniform_sampling && !has_jfif && !has_adobe
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
