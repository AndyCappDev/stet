// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF annotation (`/Annot`) writer.
//!
//! Consumes [`stet_core::pdfmark::AnnotationRecord`] entries from
//! `Context::pdfmark_buffer` and emits one PDF indirect object per
//! annotation. Per-page annotations are collected into the page dict's
//! `/Annots` array at page-build time.

use stet_core::pdfmark::{
    AnnotationRecord, AnnotationSubtype, AnnotationTarget, Border, LinkHighlight,
    TextAnnotationIcon, ViewSpec, WidgetAnnotation,
};

use crate::form_fields::push_field_level_keys;
use crate::outline::encode_action;
use crate::pdf_objects::PdfObj;
use crate::pdf_writer::PdfWriter;

/// Write a single annotation indirect object into `writer`. Returns
/// the object number of the new annotation. `Widget` records are
/// **skipped** here — the AcroForm writer in `form_fields.rs` owns
/// widget emission so it can pre-allocate the refs that the field
/// tree's `/Parent` and `/Kids` entries depend on.
pub fn write_annotation(
    writer: &mut PdfWriter,
    record: &AnnotationRecord,
    page_refs: &[u32],
) -> u32 {
    let mut entries: Vec<(Vec<u8>, PdfObj)> = vec![
        (b"Type".to_vec(), PdfObj::name("Annot")),
        (
            b"Subtype".to_vec(),
            PdfObj::name(subtype_name(&record.subtype)),
        ),
        (b"Rect".to_vec(), rect_array(&record.rect)),
    ];
    push_shared_keys(&mut entries, record);

    match &record.subtype {
        AnnotationSubtype::Link { target, highlight } => {
            if let Some(t) = target {
                attach_target(&mut entries, t, page_refs);
            }
            if let Some(h) = highlight {
                entries.push((b"H".to_vec(), PdfObj::name(highlight_name(*h))));
            }
        }
        AnnotationSubtype::Text { open, icon } => {
            entries.push((b"Open".to_vec(), PdfObj::Bool(*open)));
            entries.push((b"Name".to_vec(), PdfObj::name(text_icon_name(*icon))));
        }
        AnnotationSubtype::FreeText {
            default_appearance,
            quadding,
        } => {
            let da = default_appearance
                .clone()
                .unwrap_or_else(|| "0 0 0 rg /Helv 10 Tf".to_string());
            entries.push((b"DA".to_vec(), PdfObj::LitString(da.into_bytes())));
            if let Some(q) = quadding {
                entries.push((b"Q".to_vec(), PdfObj::Int(*q as i64)));
            }
        }
        AnnotationSubtype::Widget(_) => {
            // Widgets are owned by the form-fields writer.
            unreachable!("widget annotations are emitted by form_fields::write_form");
        }
        _ => {}
    }

    writer.add_object(&PdfObj::Dict(entries))
}

/// Build the dict body for a `/Widget` annotation, optionally merged
/// with its leaf field. Used by the AcroForm writer (which has
/// already pre-allocated this widget's object number) so the returned
/// entry list can be passed to `writer.set_object`. When
/// `merge_field_keys` is true, the field-level keys (`/T`, `/FT`,
/// `/V`, `/Ff`, …) are appended; radio kids set it to false because
/// those keys live on the synthetic radio-group parent instead.
pub fn widget_annotation_dict(
    record: &AnnotationRecord,
    widget: &WidgetAnnotation,
    leaf_segment: &str,
    parent_ref: Option<u32>,
    merge_field_keys: bool,
) -> Vec<(Vec<u8>, PdfObj)> {
    let mut entries: Vec<(Vec<u8>, PdfObj)> = vec![
        (b"Type".to_vec(), PdfObj::name("Annot")),
        (b"Subtype".to_vec(), PdfObj::name("Widget")),
        (b"Rect".to_vec(), rect_array(&record.rect)),
    ];
    push_shared_keys(&mut entries, record);

    if let Some(p) = parent_ref {
        entries.push((b"Parent".to_vec(), PdfObj::Ref(p)));
    }
    if merge_field_keys {
        // The widget IS the leaf field — emit its name segment plus
        // every field-level key the widget carries.
        if !leaf_segment.is_empty() {
            entries.push((
                b"T".to_vec(),
                PdfObj::LitString(leaf_segment.as_bytes().to_vec()),
            ));
        }
        push_field_level_keys(&mut entries, widget);
    }
    entries
}

/// Append the keys that every annotation subtype shares — color,
/// border, title, contents — onto an in-progress dict. Pulled out so
/// the widget-merged path and the conventional path stay in sync.
fn push_shared_keys(entries: &mut Vec<(Vec<u8>, PdfObj)>, record: &AnnotationRecord) {
    if let Some([r, g, b]) = record.color {
        entries.push((
            b"C".to_vec(),
            PdfObj::Array(vec![PdfObj::Real(r), PdfObj::Real(g), PdfObj::Real(b)]),
        ));
    }

    if let Some(border) = &record.border {
        entries.push((b"Border".to_vec(), border_array(border)));
    }

    if let Some(title) = &record.title {
        entries.push((b"T".to_vec(), PdfObj::LitString(title.clone().into_bytes())));
    }
    if let Some(contents) = &record.contents {
        entries.push((
            b"Contents".to_vec(),
            PdfObj::LitString(contents.clone().into_bytes()),
        ));
    }
}

/// Group annotations by 1-based page number so the page-builder can
/// inline a `/Annots` array per page in one pass. Returns a map from
/// `page_num - 1` (zero-based index into `page_refs`) to the list of
/// annotation indirect refs. **Widget annotations are skipped** —
/// `form_fields::write_form` returns the per-page widget refs which
/// the caller merges into this output before assembling the page
/// dicts.
pub fn collect_per_page(
    writer: &mut PdfWriter,
    records: &[AnnotationRecord],
    page_refs: &[u32],
) -> Vec<Vec<u32>> {
    let mut per_page: Vec<Vec<u32>> = vec![Vec::new(); page_refs.len()];
    for record in records {
        if record.page == 0 || record.page as usize > page_refs.len() {
            continue;
        }
        if matches!(record.subtype, AnnotationSubtype::Widget(_)) {
            continue;
        }
        let obj_ref = write_annotation(writer, record, page_refs);
        per_page[record.page as usize - 1].push(obj_ref);
    }
    per_page
}

fn subtype_name(subtype: &AnnotationSubtype) -> &'static str {
    match subtype {
        AnnotationSubtype::Link { .. } => "Link",
        AnnotationSubtype::Text { .. } => "Text",
        AnnotationSubtype::FreeText { .. } => "FreeText",
        AnnotationSubtype::Widget(_) => "Widget",
        _ => "Unknown",
    }
}

fn highlight_name(h: LinkHighlight) -> &'static str {
    match h {
        LinkHighlight::None => "N",
        LinkHighlight::Invert => "I",
        LinkHighlight::Outline => "O",
        LinkHighlight::Push => "P",
        _ => "I",
    }
}

fn text_icon_name(icon: TextAnnotationIcon) -> &'static str {
    match icon {
        TextAnnotationIcon::Comment => "Comment",
        TextAnnotationIcon::Note => "Note",
        TextAnnotationIcon::Key => "Key",
        TextAnnotationIcon::Help => "Help",
        TextAnnotationIcon::NewParagraph => "NewParagraph",
        TextAnnotationIcon::Paragraph => "Paragraph",
        TextAnnotationIcon::Insert => "Insert",
        _ => "Note",
    }
}

fn rect_array(rect: &[f64; 4]) -> PdfObj {
    PdfObj::Array(vec![
        PdfObj::Real(rect[0]),
        PdfObj::Real(rect[1]),
        PdfObj::Real(rect[2]),
        PdfObj::Real(rect[3]),
    ])
}

fn border_array(border: &Border) -> PdfObj {
    let mut elems = vec![
        PdfObj::Real(border.h_radius),
        PdfObj::Real(border.v_radius),
        PdfObj::Real(border.width),
    ];
    if let Some(dash) = &border.dash {
        elems.push(PdfObj::Array(
            dash.iter().map(|d| PdfObj::Real(*d)).collect(),
        ));
    }
    PdfObj::Array(elems)
}

fn attach_target(
    entries: &mut Vec<(Vec<u8>, PdfObj)>,
    target: &AnnotationTarget,
    page_refs: &[u32],
) {
    match target {
        AnnotationTarget::PageView { page, view } => {
            if let Some(page_ref) = page_to_ref(*page, page_refs) {
                entries.push((b"Dest".to_vec(), page_view_dest_array(page_ref, view)));
            }
        }
        AnnotationTarget::NamedDest(name) => {
            entries.push((
                b"Dest".to_vec(),
                PdfObj::LitString(name.clone().into_bytes()),
            ));
        }
        AnnotationTarget::Action(action) => {
            if let Some(dict) = encode_action(action, page_refs) {
                entries.push((b"A".to_vec(), dict));
            }
        }
        _ => {}
    }
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
        _ => elems.push(PdfObj::name("Fit")),
    }
    PdfObj::Array(elems)
}

fn opt_real(v: Option<f64>) -> PdfObj {
    match v {
        Some(v) => PdfObj::Real(v),
        None => PdfObj::Null,
    }
}
