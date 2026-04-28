// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `/EMBED` pdfmark writer.
//!
//! Consumes [`stet_core::pdfmark::EmbedRecord`] entries from
//! `Context::pdfmark_buffer` and emits one `/Filespec` dict + one
//! `/EmbeddedFile` stream per record. The writer assembles them into
//! a single-leaf `/EmbeddedFiles` name tree (sorted by filename) that
//! the caller plugs into `/Catalog /Names /EmbeddedFiles`.
//!
//! The single-leaf layout is fine for the document sizes pdfmark
//! authoring targets (handfuls of attachments). If a use case ever
//! needs deep trees, `build_embedded_files_leaf` can be split into a
//! `/Kids` hierarchy without changing the caller surface.

use stet_core::pdfmark::EmbedRecord;

use crate::pdf_objects::PdfObj;
use crate::pdf_writer::PdfWriter;

/// Build the `/EmbeddedFiles` leaf indirect ref. Returns `None` when
/// no records were issued. The result is meant to be passed to
/// [`crate::names::write_names_root`] alongside any `/Dests` leaf.
pub fn build_embedded_files_leaf(writer: &mut PdfWriter, records: &[EmbedRecord]) -> Option<u32> {
    if records.is_empty() {
        return None;
    }

    // Group by filename — PDF name trees require sorted entries, and
    // later records with the same name win (matches the "later wins"
    // resolution rule used elsewhere).
    let mut by_name: std::collections::BTreeMap<String, &EmbedRecord> =
        std::collections::BTreeMap::new();
    for record in records {
        let key = record
            .unicode_filename
            .clone()
            .unwrap_or_else(|| record.filename.clone());
        by_name.insert(key, record);
    }

    let mut names_array: Vec<PdfObj> = Vec::with_capacity(by_name.len() * 2);
    for (name, record) in &by_name {
        let filespec_ref = write_filespec(writer, record);
        names_array.push(PdfObj::LitString(name.clone().into_bytes()));
        names_array.push(PdfObj::Ref(filespec_ref));
    }

    let limits_low = match &names_array[0] {
        PdfObj::LitString(s) => s.clone(),
        _ => return None,
    };
    let limits_high = match &names_array[names_array.len() - 2] {
        PdfObj::LitString(s) => s.clone(),
        _ => return None,
    };

    Some(writer.add_object(&PdfObj::Dict(vec![
        (
            b"Limits".to_vec(),
            PdfObj::Array(vec![
                PdfObj::LitString(limits_low),
                PdfObj::LitString(limits_high),
            ]),
        ),
        (b"Names".to_vec(), PdfObj::Array(names_array)),
    ])))
}

/// Write the `/EmbeddedFile` stream for `record.data` and the
/// `/Filespec` dict that references it. Returns the `/Filespec` ref.
fn write_filespec(writer: &mut PdfWriter, record: &EmbedRecord) -> u32 {
    // Stream object — flate-compressed when it shrinks, with PDF 1.7
    // /Type /EmbeddedFile so viewers know the bytes are the raw
    // attachment payload.
    let mut stream_dict_entries: Vec<(Vec<u8>, PdfObj)> = vec![
        (b"Type".to_vec(), PdfObj::name("EmbeddedFile")),
        (
            b"Params".to_vec(),
            PdfObj::Dict(vec![(
                b"Size".to_vec(),
                PdfObj::Int(record.data.len() as i64),
            )]),
        ),
    ];
    if let Some(mime) = &record.mime_type {
        stream_dict_entries.push((b"Subtype".to_vec(), PdfObj::name(mime)));
    }
    let ef_ref = writer.add_stream(stream_dict_entries, &record.data, true);

    // Filespec dict — points at the embedded-file stream via /EF /F.
    let mut entries: Vec<(Vec<u8>, PdfObj)> = vec![
        (b"Type".to_vec(), PdfObj::name("Filespec")),
        (
            b"F".to_vec(),
            PdfObj::LitString(record.filename.clone().into_bytes()),
        ),
        (
            b"UF".to_vec(),
            PdfObj::LitString(
                record
                    .unicode_filename
                    .clone()
                    .unwrap_or_else(|| record.filename.clone())
                    .into_bytes(),
            ),
        ),
        (
            b"EF".to_vec(),
            PdfObj::Dict(vec![(b"F".to_vec(), PdfObj::Ref(ef_ref))]),
        ),
    ];
    if let Some(desc) = &record.description {
        entries.push((
            b"Desc".to_vec(),
            PdfObj::LitString(desc.clone().into_bytes()),
        ));
    }
    if let Some(rel) = record.af_relationship.as_deref().and_then(validated_af) {
        entries.push((b"AFRelationship".to_vec(), PdfObj::Name(rel.to_vec())));
    }
    writer.add_object(&PdfObj::Dict(entries))
}

/// Validate `/AFRelationship` against the PDF 2.0 / ISO 19005-3
/// allow-list. Unknown values are dropped silently — the spec defines
/// `/Unspecified` as the implicit default.
fn validated_af(name: &str) -> Option<&[u8]> {
    Some(match name {
        "Source" => b"Source".as_slice(),
        "Data" => b"Data",
        "Alternative" => b"Alternative",
        "Supplement" => b"Supplement",
        "EncryptedPayload" => b"EncryptedPayload",
        "Unspecified" => b"Unspecified",
        "FormData" => b"FormData",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_embed(name: &str, body: &[u8]) -> EmbedRecord {
        EmbedRecord {
            filename: name.to_string(),
            data: body.to_vec(),
            unicode_filename: None,
            description: None,
            af_relationship: None,
            mime_type: None,
        }
    }

    #[test]
    fn empty_records_returns_none() {
        let mut writer = PdfWriter::new();
        assert!(build_embedded_files_leaf(&mut writer, &[]).is_none());
    }

    #[test]
    fn single_record_emits_leaf() {
        let mut writer = PdfWriter::new();
        let leaf = build_embedded_files_leaf(
            &mut writer,
            &[make_embed("notes.txt", b"Hello, attachment.")],
        )
        .unwrap();
        assert!(writer.is_object_set(leaf));
    }

    #[test]
    fn af_validation_drops_unknown() {
        assert!(validated_af("Source").is_some());
        assert!(validated_af("BogusValue").is_none());
    }
}
