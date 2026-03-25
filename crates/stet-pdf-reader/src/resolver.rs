// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Indirect object resolution with lazy caching and stream decompression.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use crate::error::PdfError;
use crate::filters;
use crate::lexer::{Lexer, Token, parse_object};
use crate::objects::{PdfDict, PdfObj};
use crate::xref::{XrefEntry, XrefTable};

/// Cached decompressed ObjStm data + parsed header.
struct ObjStmCache {
    data: Vec<u8>,
    first: usize,
    offsets: Vec<(u32, usize)>,
}

/// Resolves indirect object references, caching parsed objects.
pub struct Resolver<'a> {
    data: &'a [u8],
    xref: XrefTable,
    /// Cache of parsed objects, populated on demand.
    cache: RefCell<HashMap<u32, PdfObj>>,
    /// Guard against circular references.
    resolving: RefCell<HashSet<u32>>,
    /// Encryption state, if the PDF is encrypted.
    encryption: Option<crate::crypto::EncryptionState>,
    /// Cache of decompressed stream data by object number.
    /// Avoids re-decompressing the same stream (e.g., ICC profiles
    /// referenced by thousands of Indexed color spaces).
    stream_cache: RefCell<HashMap<u32, Vec<u8>>>,
    /// Cache of decompressed object streams (ObjStm).
    /// Avoids re-decompressing the same ObjStm for every object within it.
    objstm_cache: RefCell<HashMap<u32, ObjStmCache>>,
}

impl<'a> Resolver<'a> {
    /// Create a temporary resolver without encryption (for resolving the
    /// Encrypt dict before encryption state is known).
    pub(crate) fn new(data: &'a [u8], xref: &XrefTable) -> Self {
        Self {
            data,
            xref: xref.clone(),
            cache: RefCell::new(HashMap::new()),
            resolving: RefCell::new(HashSet::new()),
            encryption: None,
            stream_cache: RefCell::new(HashMap::new()),
            objstm_cache: RefCell::new(HashMap::new()),
        }
    }

    /// Create a resolver with optional encryption state.
    pub fn with_encryption(
        data: &'a [u8],
        xref: XrefTable,
        encryption: Option<crate::crypto::EncryptionState>,
    ) -> Self {
        Self {
            data,
            xref,
            cache: RefCell::new(HashMap::new()),
            resolving: RefCell::new(HashSet::new()),
            encryption,
            stream_cache: RefCell::new(HashMap::new()),
            objstm_cache: RefCell::new(HashMap::new()),
        }
    }

    /// Resolve an indirect reference to its parsed object.
    pub fn resolve(&self, obj_num: u32, _gen_num: u16) -> Result<PdfObj, PdfError> {
        // Check cache first
        if let Some(obj) = self.cache.borrow().get(&obj_num) {
            return Ok(obj.clone());
        }

        // Circular reference guard
        if !self.resolving.borrow_mut().insert(obj_num) {
            return Err(PdfError::CircularReference(obj_num, _gen_num));
        }

        let result = self.resolve_uncached(obj_num, _gen_num);

        self.resolving.borrow_mut().remove(&obj_num);

        let obj = result?;
        self.cache.borrow_mut().insert(obj_num, obj.clone());
        Ok(obj)
    }

    fn resolve_uncached(&self, obj_num: u32, gen_num: u16) -> Result<PdfObj, PdfError> {
        let entry = self
            .xref
            .get(obj_num)
            .ok_or(PdfError::ObjectNotFound { obj_num, gen_num })?;

        let obj = match *entry {
            XrefEntry::InFile { offset, .. } => {
                // Try the xref offset first; if it's corrupt (e.g. from a
                // broken incremental update), fall back to scanning the file
                // for the real "N G obj" header.
                match self.parse_object_at(offset, Some(obj_num)) {
                    Ok(obj) => obj,
                    Err(_) => self.scan_for_object(obj_num)?,
                }
            }
            XrefEntry::InStream {
                stream_obj_num,
                index_within,
            } => {
                // Objects in object streams are not individually encrypted
                return self.parse_object_from_stream(stream_obj_num, index_within);
            }
            XrefEntry::Free => return Err(PdfError::ObjectNotFound { obj_num, gen_num }),
        };

        // Decrypt strings in the parsed object (stream data is decrypted separately)
        Ok(self.decrypt_object(obj, obj_num, gen_num))
    }

    /// If obj is a Ref, resolve it. Otherwise return as-is.
    pub fn deref(&self, obj: &PdfObj) -> Result<PdfObj, PdfError> {
        match obj {
            PdfObj::Ref(n, g) => self.resolve(*n, *g),
            other => Ok(other.clone()),
        }
    }

    /// Resolve an object and return its decompressed stream data.
    pub fn stream_data(&self, obj_num: u32, gen_num: u16) -> Result<Vec<u8>, PdfError> {
        // Check stream cache first
        if let Some(cached) = self.stream_cache.borrow().get(&obj_num) {
            return Ok(cached.clone());
        }

        let obj = self.resolve(obj_num, gen_num)?;
        match obj {
            PdfObj::Stream {
                dict,
                data_offset,
                data_len,
            } => {
                let raw_slice = &self.data[data_offset..data_offset + data_len];
                // Decrypt stream data if encrypted
                let raw = if let Some(ref enc) = self.encryption {
                    enc.decrypt_stream(raw_slice, obj_num, gen_num)
                } else {
                    raw_slice.to_vec()
                };
                let (filter_list, mut parms) = filters::parse_filters(&dict)?;
                filters::resolve_decode_parms(&dict, &mut parms, self);
                let result = if filter_list.is_empty() {
                    Ok(raw)
                } else {
                    let jbig2_globals = self.resolve_jbig2_globals(&filter_list, &parms)?;
                    filters::decode_stream(&raw, &filter_list, &parms, jbig2_globals.as_deref())
                }?;

                // Cache small streams (ICC profiles, lookup tables, CMaps) to avoid
                // repeated decompression. Skip large streams (images, page content).
                if result.len() <= 1_048_576 {
                    self.stream_cache
                        .borrow_mut()
                        .insert(obj_num, result.clone());
                }
                Ok(result)
            }
            _ => Err(PdfError::Other(format!(
                "object {obj_num} {gen_num} is not a stream"
            ))),
        }
    }

    /// Get the raw (pre-filter) stream bytes and filter list for an object.
    /// Used for peeking at JPX metadata before full decode.
    pub fn raw_stream_and_filters(
        &self,
        obj: &PdfObj,
    ) -> Result<(Vec<u8>, Vec<filters::Filter>), PdfError> {
        let (obj_num, gen_num) = match obj {
            PdfObj::Ref(n, g) => (*n, *g),
            _ => return Err(PdfError::Other("expected Ref".into())),
        };
        let resolved = self.resolve(obj_num, gen_num)?;
        match resolved {
            PdfObj::Stream {
                dict,
                data_offset,
                data_len,
            } => {
                let raw_slice = &self.data[data_offset..data_offset + data_len];
                let raw = if let Some(ref enc) = self.encryption {
                    enc.decrypt_stream(raw_slice, obj_num, gen_num)
                } else {
                    raw_slice.to_vec()
                };
                let (filter_list, _parms) = filters::parse_filters(&dict)?;
                Ok((raw, filter_list))
            }
            _ => Err(PdfError::Other("not a stream".into())),
        }
    }

    /// Resolve an object and return decompressed stream data, accepting a PdfObj directly.
    pub fn stream_data_from_obj(&self, obj: &PdfObj) -> Result<Vec<u8>, PdfError> {
        // If it's a Ref, use stream_data() which handles encryption with obj_num/gen_num.
        if let PdfObj::Ref(n, g) = obj {
            return self.stream_data(*n, *g);
        }
        let obj = self.deref(obj)?;
        match obj {
            PdfObj::Stream {
                dict,
                data_offset,
                data_len,
            } => {
                // No encryption for inline stream objects (no obj_num to derive key from)
                let raw = &self.data[data_offset..data_offset + data_len];
                let (filter_list, mut parms) = filters::parse_filters(&dict)?;
                filters::resolve_decode_parms(&dict, &mut parms, self);
                if filter_list.is_empty() {
                    Ok(raw.to_vec())
                } else {
                    let jbig2_globals = self.resolve_jbig2_globals(&filter_list, &parms)?;
                    filters::decode_stream(raw, &filter_list, &parms, jbig2_globals.as_deref())
                }
            }
            _ => Err(PdfError::Other("expected a stream object".into())),
        }
    }

    /// Access the trailer dictionary.
    pub fn trailer(&self) -> &PdfDict {
        &self.xref.trailer
    }

    /// Access the raw file data.
    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    /// Decrypt all strings within a parsed object tree.
    /// Stream data is NOT decrypted here (handled in stream_data()).
    fn decrypt_object(&self, obj: PdfObj, obj_num: u32, gen_num: u16) -> PdfObj {
        let enc = match &self.encryption {
            Some(e) => e,
            None => return obj,
        };
        match obj {
            PdfObj::Str(s) => PdfObj::Str(enc.decrypt_string(&s, obj_num, gen_num)),
            PdfObj::Array(arr) => PdfObj::Array(
                arr.into_iter()
                    .map(|o| self.decrypt_object(o, obj_num, gen_num))
                    .collect(),
            ),
            PdfObj::Dict(dict) => {
                let entries: Vec<_> = dict
                    .into_entries()
                    .into_iter()
                    .map(|(k, v)| (k, self.decrypt_object(v, obj_num, gen_num)))
                    .collect();
                PdfObj::Dict(PdfDict::from_entries(entries))
            }
            PdfObj::Stream {
                dict,
                data_offset,
                data_len,
            } => {
                // Decrypt dict entries but NOT stream data (done in stream_data())
                let entries: Vec<_> = dict
                    .into_entries()
                    .into_iter()
                    .map(|(k, v)| (k, self.decrypt_object(v, obj_num, gen_num)))
                    .collect();
                PdfObj::Stream {
                    dict: PdfDict::from_entries(entries),
                    data_offset,
                    data_len,
                }
            }
            other => other,
        }
    }

    /// If the filter chain contains JBIG2Decode, resolve the /JBIG2Globals stream
    /// from the corresponding DecodeParms entry.
    fn resolve_jbig2_globals(
        &self,
        filters: &[filters::Filter],
        parms: &[Option<PdfDict>],
    ) -> Result<Option<Vec<u8>>, PdfError> {
        for (i, f) in filters.iter().enumerate() {
            if *f == filters::Filter::JBIG2Decode
                && let Some(Some(dp)) = parms.get(i)
                && let Some(globals_ref) = dp.get(b"JBIG2Globals")
            {
                return self.stream_data_from_obj(globals_ref).map(Some);
            }
        }
        Ok(None)
    }

    /// Scan the entire file for `obj_num G obj` and parse the last occurrence.
    /// Uses the last match because incremental updates append newer versions
    /// of objects later in the file; the latest definition should win.
    fn scan_for_object(&self, obj_num: u32) -> Result<PdfObj, PdfError> {
        let needle = format!("{} 0 obj", obj_num);
        let needle_bytes = needle.as_bytes();
        let mut last_match = None;
        let mut pos = 0;
        while pos + needle_bytes.len() < self.data.len() {
            if self.data[pos..].starts_with(needle_bytes) {
                // Verify it's at a line boundary (start of file or after whitespace)
                if pos == 0
                    || self.data[pos - 1] == b'\n'
                    || self.data[pos - 1] == b'\r'
                    || self.data[pos - 1] == b' '
                {
                    // Verify the char after "obj" is whitespace or '<'
                    let after = pos + needle_bytes.len();
                    if after < self.data.len()
                        && (self.data[after].is_ascii_whitespace() || self.data[after] == b'<')
                    {
                        last_match = Some(pos);
                    }
                }
            }
            pos += 1;
        }
        match last_match {
            Some(offset) => self.parse_object_at(offset, None),
            None => Err(PdfError::ObjectNotFound {
                obj_num,
                gen_num: 0,
            }),
        }
    }

    /// Parse an indirect object at a file offset.
    /// Handles xref offsets that are off by a few bytes (common in some PDF generators)
    /// by scanning backward up to 20 bytes to find the `N G obj` header.
    /// When `expected_obj_num` is Some, verifies the parsed object number matches;
    /// returns an error on mismatch so the caller can fall back to scanning.
    fn parse_object_at(
        &self,
        offset: usize,
        expected_obj_num: Option<u32>,
    ) -> Result<PdfObj, PdfError> {
        if offset >= self.data.len() {
            return Err(PdfError::InvalidObject(offset));
        }

        // Try the exact offset first (no word-boundary check — some PDFs omit
        // whitespace between `endobj` and the next object header), then scan
        // backward with stricter matching if that fails.
        let actual_offset = self
            .try_parse_obj_header_relaxed(offset)
            .or_else(|| {
                // Scan backward up to 20 bytes for the object header
                let start = offset.saturating_sub(20);
                (start..offset)
                    .rev()
                    .find_map(|off| self.try_parse_obj_header(off))
            })
            .ok_or(PdfError::InvalidObject(offset))?;

        let mut lexer = Lexer::at(self.data, actual_offset);

        // Parse the "N G obj" header and verify the object number
        let parsed_num = match lexer.next_token()? {
            Token::Int(n) => n as u32,
            _ => return Err(PdfError::InvalidObject(offset)),
        };
        let _gen_num = lexer.next_token()?; // Int
        let _obj_kw = lexer.next_token()?; // Keyword("obj")

        if let Some(expected) = expected_obj_num {
            if parsed_num != expected {
                return Err(PdfError::InvalidObject(offset));
            }
        }

        // Parse the object value
        let obj = parse_object(&mut lexer)?;

        // Check if this is a stream (dict followed by "stream" keyword)
        if let PdfObj::Dict(dict) = obj {
            let saved = lexer.pos();
            let tok = lexer.next_token()?;
            if matches!(tok, Token::Keyword(ref kw) if kw == b"stream") {
                // Stream data starts after "stream" + EOL
                let mut data_start = lexer.pos();
                if data_start < self.data.len() && self.data[data_start] == b'\r' {
                    data_start += 1;
                }
                if data_start < self.data.len() && self.data[data_start] == b'\n' {
                    data_start += 1;
                }

                // Get length (may be an indirect reference, or missing).
                // If /Length is present but wrong (doesn't end at endstream),
                // recover by scanning for endstream.
                let length = match self.resolve_stream_length(&dict) {
                    Ok(len) => {
                        // Validate: endstream should follow at data_start + len
                        let expected_end = data_start + len;
                        let valid = self.check_endstream_at(expected_end);
                        if valid {
                            len
                        } else {
                            // /Length is wrong — recover from endstream
                            self.recover_stream_length(data_start).unwrap_or(len)
                        }
                    }
                    Err(_) => self.recover_stream_length(data_start)?,
                };
                let data_end = std::cmp::min(data_start + length, self.data.len());

                return Ok(PdfObj::Stream {
                    dict,
                    data_offset: data_start,
                    data_len: data_end - data_start,
                });
            } else {
                lexer.set_pos(saved);
            }
            // endobj follows — we don't strictly require it
            Ok(PdfObj::Dict(dict))
        } else {
            // endobj follows — we don't strictly require it
            Ok(obj)
        }
    }

    /// Check if `offset` starts with `Int Int Keyword("obj")` at a word boundary.
    /// Returns `Some(offset)` on success, `None` on failure.
    /// Check for `N G obj` header without requiring whitespace before the offset.
    /// Used for the exact xref offset, where some PDFs omit whitespace after `endobj`.
    fn try_parse_obj_header_relaxed(&self, offset: usize) -> Option<usize> {
        if offset >= self.data.len() {
            return None;
        }
        let mut lexer = Lexer::at(self.data, offset);
        if !matches!(lexer.next_token().ok()?, Token::Int(_)) {
            return None;
        }
        if !matches!(lexer.next_token().ok()?, Token::Int(_)) {
            return None;
        }
        match lexer.next_token().ok()? {
            Token::Keyword(ref kw) if kw == b"obj" => Some(offset),
            _ => None,
        }
    }

    fn try_parse_obj_header(&self, offset: usize) -> Option<usize> {
        if offset >= self.data.len() {
            return None;
        }
        // Must be at a word boundary (start of file, or preceded by whitespace/newline)
        if offset > 0 && !matches!(self.data[offset - 1], b' ' | b'\t' | b'\r' | b'\n') {
            return None;
        }
        self.try_parse_obj_header_relaxed(offset)
    }

    /// Check if `endstream` keyword appears at or near the given offset.
    fn check_endstream_at(&self, offset: usize) -> bool {
        let needle = b"endstream";
        // Allow up to 2 bytes of whitespace between stream data and endstream
        for skip in 0..=2 {
            let pos = offset + skip;
            if pos + needle.len() <= self.data.len()
                && &self.data[pos..pos + needle.len()] == needle
            {
                return true;
            }
        }
        false
    }

    /// Recover stream length by scanning for `endstream` keyword.
    /// Used when /Length is missing from the stream dict.
    fn recover_stream_length(&self, data_start: usize) -> Result<usize, PdfError> {
        let needle = b"endstream";
        let search_end = self.data.len().saturating_sub(needle.len());
        let mut pos = data_start;
        while pos <= search_end {
            if &self.data[pos..pos + needle.len()] == needle {
                // Strip trailing whitespace before endstream
                let mut end = pos;
                while end > data_start && matches!(self.data[end - 1], b' ' | b'\r' | b'\n') {
                    end -= 1;
                }
                return Ok(end - data_start);
            }
            pos += 1;
        }
        Err(PdfError::StreamMissingLength)
    }

    /// Resolve the /Length of a stream dict (may be an indirect reference).
    fn resolve_stream_length(&self, dict: &PdfDict) -> Result<usize, PdfError> {
        match dict.get(b"Length") {
            Some(PdfObj::Int(n)) => Ok(*n as usize),
            Some(PdfObj::Ref(n, g)) => {
                let len_obj = self.resolve(*n, *g)?;
                match len_obj {
                    PdfObj::Int(n) => Ok(n as usize),
                    _ => Err(PdfError::StreamMissingLength),
                }
            }
            _ => Err(PdfError::StreamMissingLength),
        }
    }

    /// Ensure the decompressed ObjStm data + header are cached.
    /// Returns the index into `objstm_cache` for the given stream object.
    fn ensure_objstm_cached(&self, stream_obj_num: u32) -> Result<(), PdfError> {
        if self.objstm_cache.borrow().contains_key(&stream_obj_num) {
            return Ok(());
        }

        let stream_obj = self.resolve(stream_obj_num, 0)?;
        let (dict, data_offset, data_len) = match stream_obj {
            PdfObj::Stream {
                dict,
                data_offset,
                data_len,
            } => (dict, data_offset, data_len),
            _ => {
                return Err(PdfError::Other(format!(
                    "object stream {stream_obj_num} is not a stream"
                )));
            }
        };

        let raw_slice = &self.data[data_offset..data_offset + data_len];
        let raw = if let Some(ref enc) = self.encryption {
            enc.decrypt_stream(raw_slice, stream_obj_num, 0)
        } else {
            raw_slice.to_vec()
        };
        let (filter_list, mut parms) = filters::parse_filters(&dict)?;
        filters::resolve_decode_parms(&dict, &mut parms, self);
        let stream_data = if filter_list.is_empty() {
            raw
        } else {
            filters::decode_stream(&raw, &filter_list, &parms, None)?
        };

        let n = dict.get_int(b"N").ok_or(PdfError::MissingKey("N"))? as usize;
        let first = dict
            .get_int(b"First")
            .ok_or(PdfError::MissingKey("First"))? as usize;

        let mut header_lexer = Lexer::new(&stream_data[..first.min(stream_data.len())]);
        let mut obj_offsets: Vec<(u32, usize)> = Vec::with_capacity(n);
        for _ in 0..n {
            let num = match header_lexer.next_token()? {
                Token::Int(v) => v as u32,
                _ => break,
            };
            let off = match header_lexer.next_token()? {
                Token::Int(v) => v as usize,
                _ => break,
            };
            obj_offsets.push((num, off));
        }

        self.objstm_cache.borrow_mut().insert(
            stream_obj_num,
            ObjStmCache {
                data: stream_data,
                first,
                offsets: obj_offsets,
            },
        );
        Ok(())
    }

    /// Parse an object from inside an object stream (ObjStm).
    fn parse_object_from_stream(
        &self,
        stream_obj_num: u32,
        index_within: u16,
    ) -> Result<PdfObj, PdfError> {
        // Ensure decompressed data is cached
        self.ensure_objstm_cached(stream_obj_num)?;

        let cache = self.objstm_cache.borrow();
        let cached = cache.get(&stream_obj_num).ok_or_else(|| {
            PdfError::Other(format!("ObjStm {stream_obj_num} not in cache"))
        })?;

        // Find and parse the target object
        let idx = index_within as usize;
        if idx >= cached.offsets.len() {
            return Err(PdfError::Other(format!(
                "index {idx} out of range in object stream {stream_obj_num}"
            )));
        }

        let (_target_num, target_offset) = cached.offsets[idx];
        let abs_offset = cached.first + target_offset;
        if abs_offset >= cached.data.len() {
            return Err(PdfError::Other(format!(
                "object offset {abs_offset} out of range in stream {stream_obj_num}"
            )));
        }

        let mut obj_lexer = Lexer::new(&cached.data[abs_offset..]);
        let obj = parse_object(&mut obj_lexer)?;

        // Eagerly cache all objects from this stream while we have it
        let offsets = cached.offsets.clone();
        let first = cached.first;
        drop(cache); // release borrow before mutating self.cache

        for (i, &(num, off)) in offsets.iter().enumerate() {
            if i == idx {
                self.cache.borrow_mut().insert(num, obj.clone());
                continue;
            }
            if !self.cache.borrow().contains_key(&num) {
                let abs = first + off;
                let stm = self.objstm_cache.borrow();
                if let Some(c) = stm.get(&stream_obj_num) {
                    if abs < c.data.len() {
                        let mut l = Lexer::new(&c.data[abs..]);
                        if let Ok(o) = parse_object(&mut l) {
                            drop(stm);
                            self.cache.borrow_mut().insert(num, o);
                        }
                    }
                }
            }
        }

        Ok(obj)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid PDF with one object for testing.
    fn minimal_pdf() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend(b"%PDF-1.4\n");

        // Object 1: a simple dict
        let obj1_offset = pdf.len();
        pdf.extend(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        // Object 2: pages
        let obj2_offset = pdf.len();
        pdf.extend(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");

        // Xref
        let xref_offset = pdf.len();
        pdf.extend(b"xref\n");
        pdf.extend(b"0 3\n");
        pdf.extend(b"0000000000 65535 f\r\n");
        pdf.extend(format!("{:010} 00000 n\r\n", obj1_offset).as_bytes());
        pdf.extend(format!("{:010} 00000 n\r\n", obj2_offset).as_bytes());
        pdf.extend(b"trailer\n");
        pdf.extend(b"<< /Size 3 /Root 1 0 R >>\n");
        pdf.extend(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

        pdf
    }

    #[test]
    fn resolve_simple_objects() {
        let data = minimal_pdf();
        let xref = crate::xref::parse_xref(&data).unwrap();
        let resolver = Resolver::with_encryption(&data, xref, None);

        let obj1 = resolver.resolve(1, 0).unwrap();
        let dict = obj1.as_dict().unwrap();
        assert_eq!(dict.get_name(b"Type"), Some(b"Catalog".as_slice()));

        let obj2 = resolver.resolve(2, 0).unwrap();
        let dict = obj2.as_dict().unwrap();
        assert_eq!(dict.get_name(b"Type"), Some(b"Pages".as_slice()));
    }

    #[test]
    fn deref_passes_through_non_ref() {
        let data = minimal_pdf();
        let xref = crate::xref::parse_xref(&data).unwrap();
        let resolver = Resolver::with_encryption(&data, xref, None);

        let obj = PdfObj::Int(42);
        let result = resolver.deref(&obj).unwrap();
        assert_eq!(result, PdfObj::Int(42));
    }

    #[test]
    fn deref_resolves_ref() {
        let data = minimal_pdf();
        let xref = crate::xref::parse_xref(&data).unwrap();
        let resolver = Resolver::with_encryption(&data, xref, None);

        let obj = PdfObj::Ref(1, 0);
        let result = resolver.deref(&obj).unwrap();
        assert!(result.as_dict().is_some());
    }

    #[test]
    fn object_not_found() {
        let data = minimal_pdf();
        let xref = crate::xref::parse_xref(&data).unwrap();
        let resolver = Resolver::with_encryption(&data, xref, None);

        let result = resolver.resolve(999, 0);
        assert!(result.is_err());
    }
}
