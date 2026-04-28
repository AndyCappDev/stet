// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF `/Names /Dests` name-tree writer.
//!
//! Consumes [`stet_core::pdfmark::DestRecord`] entries from
//! `Context::pdfmark_buffer` and emits a single-leaf name tree that
//! the PDF reader resolves named destinations against. PDF's name-tree
//! shape is a B-tree-style hierarchy for efficient lookup at scale; for
//! MVP-sized documents (~hundreds of dests at most), one flat leaf
//! suffices and Acrobat / poppler / pdfium handle it correctly. If the
//! document grows past that, the writer can be extended to split into
//! intermediate /Kids nodes.

use stet_core::pdfmark::{DestRecord, ViewSpec};

use crate::pdf_objects::PdfObj;
use crate::pdf_writer::PdfWriter;

/// Build the `/Names /Dests` indirect object from a slice of
/// destination records. Returns the indirect object number of the
/// `/Names` catalog entry — i.e. a dict that contains
/// `/Dests <<...>>`. Returns `None` when no records were issued.
///
/// `page_refs` maps 1-based page numbers to PDF page-object refs.
/// Records that target an out-of-range page are dropped silently.
pub fn write_names_dict(
    writer: &mut PdfWriter,
    records: &[DestRecord],
    page_refs: &[u32],
) -> Option<u32> {
    if records.is_empty() {
        return None;
    }

    // Sort records by name — PDF name trees are required to be sorted.
    // Late entries with the same name win (matches "later record wins"
    // resolution we apply elsewhere).
    let mut by_name: std::collections::BTreeMap<String, &DestRecord> =
        std::collections::BTreeMap::new();
    for record in records {
        by_name.insert(record.name.clone(), record);
    }

    // Build /Names array: [(name1) [pageref /XYZ ...] (name2) [...] ...]
    let mut names_array: Vec<PdfObj> = Vec::with_capacity(by_name.len() * 2);
    for (name, record) in &by_name {
        let Some(page_ref) = page_to_ref(record.page, page_refs) else {
            continue;
        };
        names_array.push(PdfObj::LitString(name.clone().into_bytes()));
        names_array.push(page_view_dest_array(page_ref, &record.view));
    }
    if names_array.is_empty() {
        return None;
    }

    // Compute /Limits from the actual emitted entries (not the input,
    // since some records may have been dropped above).
    let limits_low = match &names_array[0] {
        PdfObj::LitString(s) => s.clone(),
        _ => return None,
    };
    let limits_high = match &names_array[names_array.len() - 2] {
        PdfObj::LitString(s) => s.clone(),
        _ => return None,
    };

    // Single leaf — no /Kids, just /Names + /Limits.
    let dests_leaf_ref = writer.add_object(&PdfObj::Dict(vec![
        (
            b"Limits".to_vec(),
            PdfObj::Array(vec![
                PdfObj::LitString(limits_low),
                PdfObj::LitString(limits_high),
            ]),
        ),
        (b"Names".to_vec(), PdfObj::Array(names_array)),
    ]));

    // /Names catalog entry: << /Dests <leaf> >>
    let names_root = writer.add_object(&PdfObj::Dict(vec![(
        b"Dests".to_vec(),
        PdfObj::Ref(dests_leaf_ref),
    )]));
    Some(names_root)
}

fn page_to_ref(page: u32, page_refs: &[u32]) -> Option<u32> {
    if page == 0 {
        return None;
    }
    page_refs.get(page as usize - 1).copied()
}

fn page_view_dest_array(page_ref: u32, view: &ViewSpec) -> PdfObj {
    let mut elems: Vec<PdfObj> = vec![PdfObj::Ref(page_ref)];
    match view {
        ViewSpec::Xyz { left, top, zoom } => {
            elems.push(PdfObj::name("XYZ"));
            elems.push(opt_real(*left));
            elems.push(opt_real(*top));
            elems.push(opt_real(*zoom));
        }
        ViewSpec::Fit => elems.push(PdfObj::name("Fit")),
        ViewSpec::FitH(top) => {
            elems.push(PdfObj::name("FitH"));
            elems.push(opt_real(*top));
        }
        ViewSpec::FitV(left) => {
            elems.push(PdfObj::name("FitV"));
            elems.push(opt_real(*left));
        }
        ViewSpec::FitR {
            left,
            bottom,
            right,
            top,
        } => {
            elems.push(PdfObj::name("FitR"));
            elems.push(PdfObj::Real(*left));
            elems.push(PdfObj::Real(*bottom));
            elems.push(PdfObj::Real(*right));
            elems.push(PdfObj::Real(*top));
        }
        ViewSpec::FitB => elems.push(PdfObj::name("FitB")),
        ViewSpec::FitBH(top) => {
            elems.push(PdfObj::name("FitBH"));
            elems.push(opt_real(*top));
        }
        ViewSpec::FitBV(left) => {
            elems.push(PdfObj::name("FitBV"));
            elems.push(opt_real(*left));
        }
    }
    PdfObj::Array(elems)
}

fn opt_real(v: Option<f64>) -> PdfObj {
    match v {
        Some(v) => PdfObj::Real(v),
        None => PdfObj::Null,
    }
}
