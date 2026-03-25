// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF object model for reading.

use std::fmt;

/// A PDF object parsed from file data.
#[derive(Clone, PartialEq)]
pub enum PdfObj {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    /// Name object (without leading `/`).
    Name(Vec<u8>),
    /// String object (literal or hex, already decoded).
    Str(Vec<u8>),
    Array(Vec<PdfObj>),
    Dict(PdfDict),
    /// Stream: dictionary + location of raw data in source.
    Stream {
        dict: PdfDict,
        data_offset: usize,
        data_len: usize,
    },
    /// Indirect reference (object number, generation number).
    Ref(u32, u16),
}

impl PdfObj {
    /// Return the integer value, or None.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            PdfObj::Int(n) => Some(*n),
            _ => None,
        }
    }

    /// Return a numeric value as f64 (int or real), or None.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            PdfObj::Int(n) => Some(*n as f64),
            PdfObj::Real(f) => Some(*f),
            _ => None,
        }
    }

    /// Return the name bytes, or None.
    pub fn as_name(&self) -> Option<&[u8]> {
        match self {
            PdfObj::Name(n) => Some(n),
            _ => None,
        }
    }

    /// Return the string bytes, or None.
    pub fn as_str(&self) -> Option<&[u8]> {
        match self {
            PdfObj::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Return the array, or None.
    pub fn as_array(&self) -> Option<&[PdfObj]> {
        match self {
            PdfObj::Array(a) => Some(a),
            _ => None,
        }
    }

    /// Return the dict, or None.
    pub fn as_dict(&self) -> Option<&PdfDict> {
        match self {
            PdfObj::Dict(d) => Some(d),
            PdfObj::Stream { dict, .. } => Some(dict),
            _ => None,
        }
    }

    /// Return the indirect reference, or None.
    pub fn as_ref(&self) -> Option<(u32, u16)> {
        match self {
            PdfObj::Ref(n, g) => Some((*n, *g)),
            _ => None,
        }
    }

    /// Return the reference OR extract one from a Ref-typed object.
    /// For non-Ref objects, returns None.
    pub fn to_ref(&self) -> Option<(u32, u16)> {
        self.as_ref()
    }
}

impl fmt::Debug for PdfObj {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PdfObj::Null => write!(f, "null"),
            PdfObj::Bool(b) => write!(f, "{b}"),
            PdfObj::Int(n) => write!(f, "{n}"),
            PdfObj::Real(r) => write!(f, "{r}"),
            PdfObj::Name(n) => write!(f, "/{}", String::from_utf8_lossy(n)),
            PdfObj::Str(s) => write!(f, "({:?})", String::from_utf8_lossy(s)),
            PdfObj::Array(a) => f.debug_list().entries(a.iter()).finish(),
            PdfObj::Dict(d) => write!(f, "{d:?}"),
            PdfObj::Stream { dict, data_len, .. } => {
                write!(f, "{dict:?} stream({data_len} bytes)")
            }
            PdfObj::Ref(n, g) => write!(f, "{n} {g} R"),
        }
    }
}

/// A PDF dictionary: ordered list of (name, value) pairs.
#[derive(Clone, PartialEq, Default)]
pub struct PdfDict(Vec<(Vec<u8>, PdfObj)>);

impl PdfDict {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// Look up a key by name (without leading `/`).
    pub fn get(&self, key: &[u8]) -> Option<&PdfObj> {
        self.0.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// Get a name value for a key.
    pub fn get_name(&self, key: &[u8]) -> Option<&[u8]> {
        self.get(key).and_then(|v| v.as_name())
    }

    /// Get an integer value for a key.
    pub fn get_int(&self, key: &[u8]) -> Option<i64> {
        self.get(key).and_then(|v| v.as_int())
    }

    /// Get a numeric (int or real) value as f64.
    pub fn get_f64(&self, key: &[u8]) -> Option<f64> {
        self.get(key).and_then(|v| v.as_f64())
    }

    /// Get an array value for a key.
    pub fn get_array(&self, key: &[u8]) -> Option<&[PdfObj]> {
        self.get(key).and_then(|v| v.as_array())
    }

    /// Get a dict value for a key.
    pub fn get_dict(&self, key: &[u8]) -> Option<&PdfDict> {
        self.get(key).and_then(|v| v.as_dict())
    }

    /// Get an indirect reference for a key.
    pub fn get_ref(&self, key: &[u8]) -> Option<(u32, u16)> {
        self.get(key).and_then(|v| v.as_ref())
    }

    /// Get a reference or inline dict — returns the PdfObj for the caller to resolve.
    pub fn get_derefable(&self, key: &[u8]) -> Option<&PdfObj> {
        self.get(key)
    }

    /// Insert or replace a key-value pair.
    pub fn insert(&mut self, key: Vec<u8>, val: PdfObj) {
        // For small dicts, check for duplicates (PDF spec says keys should be
        // unique, but malformed files exist). For large dicts, skip the O(n)
        // scan — the duplicate check would be O(n²) for 74K+ entry dicts.
        if self.0.len() < 100 {
            if let Some(entry) = self.0.iter_mut().find(|(k, _)| k == &key) {
                entry.1 = val;
                return;
            }
        }
        self.0.push((key, val));
    }

    /// Get all entries.
    pub fn entries(&self) -> &[(Vec<u8>, PdfObj)] {
        &self.0
    }

    /// Consume the dict and return owned entries.
    pub fn into_entries(self) -> Vec<(Vec<u8>, PdfObj)> {
        self.0
    }

    /// Create a dict from owned entries.
    pub fn from_entries(entries: Vec<(Vec<u8>, PdfObj)>) -> Self {
        PdfDict(entries)
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the dict is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for PdfDict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<<")?;
        for (k, v) in &self.0 {
            write!(f, " /{} {v:?}", String::from_utf8_lossy(k))?;
        }
        write!(f, " >>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dict_get_and_insert() {
        let mut d = PdfDict::new();
        d.insert(b"Type".to_vec(), PdfObj::Name(b"Page".to_vec()));
        d.insert(b"Count".to_vec(), PdfObj::Int(5));

        assert_eq!(d.get_name(b"Type"), Some(b"Page".as_slice()));
        assert_eq!(d.get_int(b"Count"), Some(5));
        assert_eq!(d.get(b"Missing"), None);
        assert_eq!(d.len(), 2);
    }

    #[test]
    fn dict_insert_replaces() {
        let mut d = PdfDict::new();
        d.insert(b"Key".to_vec(), PdfObj::Int(1));
        d.insert(b"Key".to_vec(), PdfObj::Int(2));
        assert_eq!(d.get_int(b"Key"), Some(2));
        assert_eq!(d.len(), 1);
    }

    #[test]
    fn obj_as_f64_int_and_real() {
        assert_eq!(PdfObj::Int(42).as_f64(), Some(42.0));
        assert_eq!(PdfObj::Real(3.14).as_f64(), Some(3.14));
        assert_eq!(PdfObj::Null.as_f64(), None);
    }

    #[test]
    fn obj_as_dict_from_stream() {
        let mut d = PdfDict::new();
        d.insert(b"Length".to_vec(), PdfObj::Int(100));
        let obj = PdfObj::Stream {
            dict: d.clone(),
            data_offset: 0,
            data_len: 100,
        };
        assert!(obj.as_dict().is_some());
        assert_eq!(obj.as_dict().unwrap().get_int(b"Length"), Some(100));
    }
}
