// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF embedded files (file attachments).
//!
//! PDFs can carry arbitrary file attachments via the catalog's
//! `/Names /EmbeddedFiles` name tree (PDF 1.4+) or as the `/FS`
//! target of a `/FileAttachment` annotation. This module exposes the
//! document-level table; the per-annotation form is reachable via
//! [`Annotation`]'s [`FileAttachmentAnnotation`].
//!
//! Each entry is a *file specification* describing the attachment
//! (name, description, relationship hint, MIME type, modification
//! dates, byte size) plus a reference to the embedded-file stream.
//! The bytes themselves load on demand via
//! [`PdfDocument::embedded_file_bytes`].
//!
//! [`Annotation`]: crate::Annotation
//! [`FileAttachmentAnnotation`]: crate::FileAttachmentAnnotation
//! [`PdfDocument::embedded_file_bytes`]: crate::PdfDocument::embedded_file_bytes

use std::collections::HashMap;

use crate::metadata::{PdfDate, pdf_string_to_rust_pub};
use crate::name_tree::walk_name_tree;
use crate::objects::{PdfDict, PdfObj};
use crate::resolver::Resolver;

/// One embedded file.
#[derive(Debug, Clone)]
pub struct EmbeddedFile {
    /// Display name (the key from the embedded-files name tree).
    pub name: String,
    /// `/F` filename (legacy ASCII).
    pub filename: Option<String>,
    /// `/UF` unicode filename.
    pub unicode_filename: Option<String>,
    /// `/Desc` description.
    pub description: Option<String>,
    /// `/AFRelationship` — author's hint about how this attachment
    /// relates to the host document.
    pub relationship: Option<AfRelationship>,
    /// `/Subtype` on the embedded-file stream — typically a MIME
    /// type encoded as a PDF name (e.g. `text/csv` → `/text#2Fcsv`).
    pub mime_type: Option<String>,
    /// `/Params /Size` — original byte length of the file.
    pub size: Option<u64>,
    /// `/Params /CreationDate`.
    pub creation_date: Option<PdfDate>,
    /// `/Params /ModDate`.
    pub mod_date: Option<PdfDate>,
    /// `/Params /CheckSum` — typically MD5 of the original content.
    pub checksum: Option<Vec<u8>>,
    /// Embedded-file stream object number.
    pub stream_obj_num: u32,
    /// Embedded-file stream generation number.
    pub stream_gen_num: u16,
}

/// `/AFRelationship` — relationship of an associated file to the host
/// document, from PDF 2.0 §14.13.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AfRelationship {
    Source,
    Data,
    Alternative,
    Supplement,
    EncryptedPayload,
    FormData,
    Schema,
    Unspecified,
    /// An unknown name; preserved verbatim.
    Other(String),
}

impl AfRelationship {
    fn from_name(name: &[u8]) -> Self {
        match name {
            b"Source" => AfRelationship::Source,
            b"Data" => AfRelationship::Data,
            b"Alternative" => AfRelationship::Alternative,
            b"Supplement" => AfRelationship::Supplement,
            b"EncryptedPayload" => AfRelationship::EncryptedPayload,
            b"FormData" => AfRelationship::FormData,
            b"Schema" => AfRelationship::Schema,
            b"Unspecified" => AfRelationship::Unspecified,
            other => AfRelationship::Other(String::from_utf8_lossy(other).into_owned()),
        }
    }
}

/// Walk the catalog's `/Names /EmbeddedFiles` name tree and produce a
/// map of every attachment, keyed by the tree's name.
///
/// Returns an empty map when the document has no embedded files.
pub fn parse_embedded_files(resolver: &Resolver) -> HashMap<String, EmbeddedFile> {
    let mut map = HashMap::new();

    let Some(catalog) = catalog_dict(resolver) else {
        return map;
    };
    let Some(names_obj) = catalog.get(b"Names") else {
        return map;
    };
    let Ok(names) = resolver.deref(names_obj) else {
        return map;
    };
    let Some(names_dict) = names.as_dict() else {
        return map;
    };
    let Some(ef_root) = names_dict.get(b"EmbeddedFiles") else {
        return map;
    };

    map = walk_name_tree(resolver, ef_root, parse_filespec);
    // The walker stamps the tree key into the map's key; we still
    // need to populate `EmbeddedFile::name` from it so the value
    // self-describes. Fix that up here.
    for (key, value) in map.iter_mut() {
        value.name = key.clone();
    }
    map
}

fn parse_filespec(resolver: &Resolver, obj: &PdfObj) -> Option<EmbeddedFile> {
    let resolved = resolver.deref(obj).ok()?;
    let dict = resolved.as_dict()?;

    // /EF subdict pointing to the embedded-file stream.
    let ef_obj = dict.get(b"EF")?;
    let ef = resolver.deref(ef_obj).ok()?;
    let ef_dict = ef.as_dict()?;
    // Prefer /UF, fall back to /F.
    let stream_ref = ef_dict.get_ref(b"UF").or_else(|| ef_dict.get_ref(b"F"))?;

    // Pull the embedded-file stream's dict for size/dates/checksum/mime.
    let mut size = None;
    let mut creation_date = None;
    let mut mod_date = None;
    let mut checksum = None;
    let mut mime_type = None;

    if let Ok(stream) = resolver.resolve(stream_ref.0, stream_ref.1)
        && let Some(stream_dict) = stream.as_dict()
    {
        if let Some(subtype) = stream_dict.get_name(b"Subtype") {
            mime_type = Some(decode_mime_name(subtype));
        }
        if let Some(params_obj) = stream_dict.get(b"Params")
            && let Ok(params_resolved) = resolver.deref(params_obj)
            && let Some(params) = params_resolved.as_dict()
        {
            size = params.get_int(b"Size").and_then(|n| u64::try_from(n).ok());
            creation_date = params
                .get(b"CreationDate")
                .and_then(|o| o.as_str())
                .and_then(PdfDate::parse);
            mod_date = params
                .get(b"ModDate")
                .and_then(|o| o.as_str())
                .and_then(PdfDate::parse);
            checksum = params
                .get(b"CheckSum")
                .and_then(|o| o.as_str())
                .map(<[u8]>::to_vec);
        }
    }

    let filename = dict.get(b"F").and_then(pdf_string_to_rust_pub);
    let unicode_filename = dict.get(b"UF").and_then(pdf_string_to_rust_pub);
    let description = dict.get(b"Desc").and_then(pdf_string_to_rust_pub);
    let relationship = dict
        .get_name(b"AFRelationship")
        .map(AfRelationship::from_name);

    Some(EmbeddedFile {
        name: String::new(), // filled in by caller from the tree key
        filename,
        unicode_filename,
        description,
        relationship,
        mime_type,
        size,
        creation_date,
        mod_date,
        checksum,
        stream_obj_num: stream_ref.0,
        stream_gen_num: stream_ref.1,
    })
}

/// PDF `/Subtype` for an embedded-file stream is a MIME type encoded
/// as a PDF name, with `/` replaced by `#2F`. Decode it.
fn decode_mime_name(name: &[u8]) -> String {
    let s = String::from_utf8_lossy(name);
    // Hex-decode `#XX` escapes.
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'#'
            && i + 2 < bytes.len()
            && let Ok(b) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
        {
            out.push(b as char);
            i += 3;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn catalog_dict(resolver: &Resolver) -> Option<PdfDict> {
    if let Some((num, gen_num)) = resolver.trailer().get_ref(b"Root")
        && let Ok(obj) = resolver.resolve(num, gen_num)
        && let Some(dict) = obj.as_dict()
    {
        return Some(dict.clone());
    }
    crate::find_catalog(resolver).and_then(|obj| obj.as_dict().cloned())
}

/// Decode the bytes of an embedded-file stream by `(obj_num, gen_num)`.
///
/// This is a thin wrapper around `Resolver::stream_data` for callers
/// who already hold an [`EmbeddedFile`] and want to pull the bytes
/// without going through the higher-level
/// `PdfDocument::embedded_file_bytes`.
pub fn decode_embedded_file_stream(
    resolver: &Resolver,
    obj_num: u32,
    gen_num: u16,
) -> Result<Vec<u8>, crate::PdfError> {
    resolver.stream_data(obj_num, gen_num)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn af_relationship_from_name() {
        assert_eq!(AfRelationship::from_name(b"Source"), AfRelationship::Source);
        assert_eq!(AfRelationship::from_name(b"Data"), AfRelationship::Data);
        assert_eq!(
            AfRelationship::from_name(b"Custom"),
            AfRelationship::Other("Custom".to_string())
        );
    }

    #[test]
    fn decode_mime_name_with_hex_escape() {
        assert_eq!(decode_mime_name(b"text#2Fcsv"), "text/csv");
        assert_eq!(decode_mime_name(b"application#2Fpdf"), "application/pdf");
        assert_eq!(decode_mime_name(b"plain"), "plain");
    }
}
