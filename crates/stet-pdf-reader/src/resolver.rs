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

/// Resolves indirect object references, caching parsed objects.
pub struct Resolver<'a> {
    data: &'a [u8],
    xref: XrefTable,
    /// Cache of parsed objects, populated on demand.
    cache: RefCell<HashMap<u32, PdfObj>>,
    /// Guard against circular references.
    resolving: RefCell<HashSet<u32>>,
}

impl<'a> Resolver<'a> {
    pub fn new(data: &'a [u8], xref: XrefTable) -> Self {
        Self {
            data,
            xref,
            cache: RefCell::new(HashMap::new()),
            resolving: RefCell::new(HashSet::new()),
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

        match *entry {
            XrefEntry::InFile { offset, .. } => self.parse_object_at(offset),
            XrefEntry::InStream {
                stream_obj_num,
                index_within,
            } => self.parse_object_from_stream(stream_obj_num, index_within),
            XrefEntry::Free => Err(PdfError::ObjectNotFound { obj_num, gen_num }),
        }
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
        let obj = self.resolve(obj_num, gen_num)?;
        match obj {
            PdfObj::Stream {
                dict,
                data_offset,
                data_len,
            } => {
                let raw = &self.data[data_offset..data_offset + data_len];
                let (filter_list, parms) = filters::parse_filters(&dict)?;
                if filter_list.is_empty() {
                    Ok(raw.to_vec())
                } else {
                    filters::decode_stream(raw, &filter_list, &parms)
                }
            }
            _ => Err(PdfError::Other(format!(
                "object {obj_num} {gen_num} is not a stream"
            ))),
        }
    }

    /// Resolve an object and return decompressed stream data, accepting a PdfObj directly.
    pub fn stream_data_from_obj(&self, obj: &PdfObj) -> Result<Vec<u8>, PdfError> {
        let obj = self.deref(obj)?;
        match obj {
            PdfObj::Stream {
                dict,
                data_offset,
                data_len,
            } => {
                let raw = &self.data[data_offset..data_offset + data_len];
                let (filter_list, parms) = filters::parse_filters(&dict)?;
                if filter_list.is_empty() {
                    Ok(raw.to_vec())
                } else {
                    filters::decode_stream(raw, &filter_list, &parms)
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

    /// Parse an indirect object at a file offset.
    fn parse_object_at(&self, offset: usize) -> Result<PdfObj, PdfError> {
        if offset >= self.data.len() {
            return Err(PdfError::InvalidObject(offset));
        }

        let mut lexer = Lexer::at(self.data, offset);

        // Expect: <obj_num> <gen_num> obj
        let tok1 = lexer.next_token()?;
        if !matches!(tok1, Token::Int(_)) {
            return Err(PdfError::InvalidObject(offset));
        }
        let tok2 = lexer.next_token()?;
        if !matches!(tok2, Token::Int(_)) {
            return Err(PdfError::InvalidObject(offset));
        }
        match lexer.next_token()? {
            Token::Keyword(ref kw) if kw == b"obj" => {}
            _ => return Err(PdfError::InvalidObject(offset)),
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

                // Get length (may be an indirect reference)
                let length = self.resolve_stream_length(&dict)?;
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

    /// Parse an object from inside an object stream (ObjStm).
    fn parse_object_from_stream(
        &self,
        stream_obj_num: u32,
        index_within: u16,
    ) -> Result<PdfObj, PdfError> {
        // Resolve the container object stream
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

        // Decompress the stream
        let raw = &self.data[data_offset..data_offset + data_len];
        let (filter_list, parms) = filters::parse_filters(&dict)?;
        let stream_data = if filter_list.is_empty() {
            raw.to_vec()
        } else {
            filters::decode_stream(raw, &filter_list, &parms)?
        };

        // Parse the header
        let n = dict.get_int(b"N").ok_or(PdfError::MissingKey("N"))? as usize;
        let first = dict
            .get_int(b"First")
            .ok_or(PdfError::MissingKey("First"))? as usize;

        // Parse obj_num/offset pairs from the header
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

        // Find and parse the target object
        let idx = index_within as usize;
        if idx >= obj_offsets.len() {
            return Err(PdfError::Other(format!(
                "index {idx} out of range in object stream {stream_obj_num}"
            )));
        }

        let (_target_num, target_offset) = obj_offsets[idx];
        let abs_offset = first + target_offset;
        if abs_offset >= stream_data.len() {
            return Err(PdfError::Other(format!(
                "object offset {abs_offset} out of range in stream {stream_obj_num}"
            )));
        }

        let mut obj_lexer = Lexer::new(&stream_data[abs_offset..]);
        let obj = parse_object(&mut obj_lexer)?;

        // Cache all objects from this stream while we have it decompressed
        for (i, &(num, off)) in obj_offsets.iter().enumerate() {
            if i == idx {
                // Already parsed this one
                self.cache.borrow_mut().insert(num, obj.clone());
                continue;
            }
            if !self.cache.borrow().contains_key(&num) {
                let abs = first + off;
                if abs < stream_data.len() {
                    let mut l = Lexer::new(&stream_data[abs..]);
                    if let Ok(o) = parse_object(&mut l) {
                        self.cache.borrow_mut().insert(num, o);
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
        let resolver = Resolver::new(&data, xref);

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
        let resolver = Resolver::new(&data, xref);

        let obj = PdfObj::Int(42);
        let result = resolver.deref(&obj).unwrap();
        assert_eq!(result, PdfObj::Int(42));
    }

    #[test]
    fn deref_resolves_ref() {
        let data = minimal_pdf();
        let xref = crate::xref::parse_xref(&data).unwrap();
        let resolver = Resolver::new(&data, xref);

        let obj = PdfObj::Ref(1, 0);
        let result = resolver.deref(&obj).unwrap();
        assert!(result.as_dict().is_some());
    }

    #[test]
    fn object_not_found() {
        let data = minimal_pdf();
        let xref = crate::xref::parse_xref(&data).unwrap();
        let resolver = Resolver::new(&data, xref);

        let result = resolver.resolve(999, 0);
        assert!(result.is_err());
    }
}
