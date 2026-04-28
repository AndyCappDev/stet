// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! AcroForm writer + field-tree builder.
//!
//! Converts the flat list of `/Widget` annotation records on
//! [`stet_core::pdfmark::PdfMarkBuffer`] into the PDF AcroForm
//! structure: a tree of field dicts keyed by dotted field name,
//! merged with their widget annotation when the field has exactly one
//! widget, and a top-level `/AcroForm` dict listing the root fields.
//!
//! Field-tree shape rules (Adobe-pdfmark conventions stet matches):
//!
//! - **Single-widget leaf field** (one widget with a unique dotted
//!   name): the widget annotation dict carries both annotation keys
//!   (`/Type /Annot /Subtype /Widget /Rect …`) **and** field keys
//!   (`/T`, `/FT`, `/V`, `/Ff`, `/MaxLen`, …). PDF readers treat the
//!   merged dict as both an annotation (via the page's `/Annots`) and
//!   a field (via `/AcroForm /Fields` resolution).
//!
//! - **Radio group** (multiple widgets sharing the same dotted name):
//!   the producer's per-widget field-level keys (`/FT`, `/Ff`, `/V`,
//!   `/Q`, `/DA`, `/MaxLen`, `/Opt`, `/DV`) are lifted to a synthetic
//!   parent field; each widget keeps only its annotation keys plus
//!   `/Parent`. The parent dict carries `/Kids` referencing every
//!   widget.
//!
//! - **Dotted field name** (`order.shipping.street`): every prefix
//!   becomes an implicit container field with `/T` set to that
//!   segment, no `/FT`, and `/Kids` referencing the next level. The
//!   widget itself sits at the leaf with `/T = street` and
//!   `/Parent = order.shipping`.
//!
//! `/AcroForm /Fields` lists exactly the top-level (root) field refs.
//! PDF readers walk down via `/Kids` to find inner fields.

use std::collections::BTreeMap;

use stet_core::pdfmark::{
    AnnotationRecord, AnnotationSubtype, ChoiceOption, FieldType, FieldValue, FormRecord,
    WidgetAnnotation,
};

use crate::annotations::widget_annotation_dict;
use crate::pdf_objects::PdfObj;
use crate::pdf_writer::PdfWriter;

/// Per-widget output of the field-tree build: which annotation indirect
/// ref to attach to the page's `/Annots`, plus the role the widget
/// plays in the field tree (used by the writer to decide which keys
/// land on the widget vs. its parent).
struct WidgetPlacement {
    /// Pre-allocated indirect-object number for this widget's
    /// annotation dict.
    obj_ref: u32,
    /// Parent field's indirect ref. `None` means the widget itself is
    /// a top-level field.
    parent_ref: Option<u32>,
    /// True when this widget is a kid of a radio-group parent. Radio
    /// kids omit field-level keys (those live on the parent).
    is_radio_kid: bool,
}

/// Result of writing the AcroForm structure: the `/AcroForm` indirect
/// ref (pushed onto `/Catalog`) and the per-page `/Annots` arrays
/// (widget refs grouped by 1-based page minus one).
pub struct AcroFormOutput {
    pub acroform_ref: u32,
    pub per_page_widget_refs: Vec<Vec<u32>>,
}

/// Build the AcroForm structure and emit it into `writer`. Returns
/// `None` when there are no widgets and no `/FORM` record (so
/// `/Catalog` stays free of `/AcroForm`).
pub fn write_form(
    writer: &mut PdfWriter,
    widgets: &[(usize, AnnotationRecord)],
    form_record: Option<&FormRecord>,
    page_count: usize,
) -> Option<AcroFormOutput> {
    if widgets.is_empty() && form_record.is_none() {
        return None;
    }
    let mut per_page_widget_refs: Vec<Vec<u32>> = vec![Vec::new(); page_count];

    if widgets.is_empty() {
        // Form record only — emit a minimal /AcroForm with no fields.
        let acroform_ref = writer.alloc_obj();
        writer.set_object(
            acroform_ref,
            &PdfObj::Dict(build_acroform_entries(form_record, &[])),
        );
        return Some(AcroFormOutput {
            acroform_ref,
            per_page_widget_refs,
        });
    }

    // Group widgets by canonical (full-dotted) field name so radio
    // groups (multiple widgets sharing a name) are visible up front.
    let mut by_name: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (slot, (_orig_idx, record)) in widgets.iter().enumerate() {
        if let AnnotationSubtype::Widget(w) = &record.subtype {
            by_name.entry(w.field_name.clone()).or_default().push(slot);
        }
    }

    // Pre-allocate the widget annotation object refs.
    let widget_obj_refs: Vec<u32> = (0..widgets.len()).map(|_| writer.alloc_obj()).collect();

    // Build the parent-field tree. Every prefix of every dotted name
    // (everything but the last segment) becomes a non-leaf parent.
    // Radio groups also need a parent at the leaf segment.
    let mut node_refs: BTreeMap<String, u32> = BTreeMap::new();
    let mut placements: Vec<Option<WidgetPlacement>> = (0..widgets.len()).map(|_| None).collect();

    for (full_name, slots) in &by_name {
        let segments: Vec<&str> = full_name.split('.').filter(|s| !s.is_empty()).collect();
        if segments.is_empty() {
            // Pathological name like "" or ".." — drop.
            continue;
        }
        // Allocate refs for every container prefix (everything but the
        // last segment).
        for depth in 1..segments.len() {
            let prefix = segments[..depth].join(".");
            node_refs
                .entry(prefix)
                .or_insert_with(|| writer.alloc_obj());
        }

        let parent_path = if segments.len() > 1 {
            Some(segments[..segments.len() - 1].join("."))
        } else {
            None
        };

        if slots.len() == 1 {
            // Single-widget leaf field: the widget annot dict IS the
            // leaf field. /Parent points at the deepest container
            // (when one exists).
            let slot = slots[0];
            placements[slot] = Some(WidgetPlacement {
                obj_ref: widget_obj_refs[slot],
                parent_ref: parent_path
                    .as_deref()
                    .and_then(|p| node_refs.get(p).copied()),
                is_radio_kid: false,
            });
        } else {
            // Multiple widgets sharing a name → radio group. The leaf
            // segment becomes a synthetic parent field; each widget
            // becomes a /Kids entry of that parent with its own
            // annotation keys but no field-level keys.
            let parent_ref = *node_refs
                .entry(full_name.clone())
                .or_insert_with(|| writer.alloc_obj());
            for &slot in slots {
                placements[slot] = Some(WidgetPlacement {
                    obj_ref: widget_obj_refs[slot],
                    parent_ref: Some(parent_ref),
                    is_radio_kid: true,
                });
            }
        }
    }

    // `widget_obj_refs[slot]` stays indexed by the original slot
    // throughout — slots without a placement (e.g. widgets dropped
    // because of an empty field name, which the parser already filters)
    // simply never get consumed.

    // Emit each widget annotation dict with its merged-field keys.
    for (slot, placement_opt) in placements.iter().enumerate() {
        let Some(placement) = placement_opt else {
            continue;
        };
        let (_orig_idx, record) = &widgets[slot];
        let widget_data = match &record.subtype {
            AnnotationSubtype::Widget(w) => w,
            _ => continue,
        };
        let leaf_segment = leaf_segment_of(&widget_data.field_name);
        let merge_field_keys = !placement.is_radio_kid;
        let dict = widget_annotation_dict(
            record,
            widget_data,
            leaf_segment,
            placement.parent_ref,
            merge_field_keys,
        );
        writer.set_object(placement.obj_ref, &PdfObj::Dict(dict));

        if record.page > 0 && (record.page as usize) <= page_count {
            per_page_widget_refs[record.page as usize - 1].push(placement.obj_ref);
        }
    }

    // Emit each parent field dict (radio-group parents and dotted-name
    // containers).
    for (full_name, slots_for_name) in &by_name {
        let segments: Vec<&str> = full_name.split('.').filter(|s| !s.is_empty()).collect();
        if segments.is_empty() {
            continue;
        }
        // Container parents.
        for depth in 1..segments.len() {
            let prefix = segments[..depth].join(".");
            let Some(&parent_ref) = node_refs.get(&prefix) else {
                continue;
            };
            // Skip if we've already emitted this prefix.
            if writer.is_object_set(parent_ref) {
                continue;
            }
            let parent_segment = segments[depth - 1];
            let grandparent_path = if depth > 1 {
                Some(segments[..depth - 1].join("."))
            } else {
                None
            };
            let grandparent_ref = grandparent_path.and_then(|p| node_refs.get(&p).copied());
            let kids = collect_kids_for_prefix(&prefix, &node_refs, &by_name, &widget_obj_refs);
            let entries = container_field_dict(parent_segment, grandparent_ref, &kids);
            writer.set_object(parent_ref, &PdfObj::Dict(entries));
        }
        // Radio-group leaf parent (only when slots_for_name.len() > 1).
        if slots_for_name.len() > 1 {
            let Some(&parent_ref) = node_refs.get(full_name) else {
                continue;
            };
            if writer.is_object_set(parent_ref) {
                continue;
            }
            let parent_segment = segments[segments.len() - 1];
            let grandparent_path = if segments.len() > 1 {
                Some(segments[..segments.len() - 1].join("."))
            } else {
                None
            };
            let grandparent_ref = grandparent_path.and_then(|p| node_refs.get(&p).copied());
            // Gather widget refs for this radio group in declaration
            // order (index ascending matches declaration order).
            let mut sorted_slots = slots_for_name.clone();
            sorted_slots.sort_by_key(|&s| widgets[s].0);
            let kid_refs: Vec<u32> = sorted_slots.iter().map(|&s| widget_obj_refs[s]).collect();
            // Lift field-level keys from the first widget onto the
            // parent. Per PDF spec radio groups must have /FT /Btn on
            // the parent; the user is responsible for setting the
            // Radio flag bit in /Ff.
            let template_widget = match &widgets[sorted_slots[0]].1.subtype {
                AnnotationSubtype::Widget(w) => w,
                _ => continue,
            };
            let entries =
                radio_parent_dict(parent_segment, grandparent_ref, &kid_refs, template_widget);
            writer.set_object(parent_ref, &PdfObj::Dict(entries));
        }
    }

    // Top-level /AcroForm /Fields = roots of the field tree. Roots are
    // exactly the names with no '.' that are still present in node_refs
    // (radio-group parents) or that map directly to a single widget.
    let mut root_refs: Vec<u32> = Vec::new();
    let mut seen_roots: BTreeMap<String, ()> = BTreeMap::new();
    for (full_name, slots_for_name) in &by_name {
        let root_segment = match full_name.split('.').find(|s| !s.is_empty()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        if seen_roots.contains_key(&root_segment) {
            continue;
        }
        seen_roots.insert(root_segment.clone(), ());
        // Resolve the root segment to a ref: node_refs entry if a
        // container/radio-group parent exists, otherwise the widget
        // ref directly (single-widget root field).
        if let Some(&r) = node_refs.get(&root_segment) {
            root_refs.push(r);
        } else if slots_for_name.len() == 1 && full_name == &root_segment {
            root_refs.push(widget_obj_refs[slots_for_name[0]]);
        }
    }

    let acroform_ref = writer.alloc_obj();
    writer.set_object(
        acroform_ref,
        &PdfObj::Dict(build_acroform_entries(form_record, &root_refs)),
    );

    Some(AcroFormOutput {
        acroform_ref,
        per_page_widget_refs,
    })
}

/// Build the dict entries for the document-level `/AcroForm` object.
fn build_acroform_entries(
    form_record: Option<&FormRecord>,
    root_refs: &[u32],
) -> Vec<(Vec<u8>, PdfObj)> {
    let mut entries: Vec<(Vec<u8>, PdfObj)> = vec![(
        b"Fields".to_vec(),
        PdfObj::Array(root_refs.iter().map(|&r| PdfObj::Ref(r)).collect()),
    )];
    // /NeedAppearances defaults to true so viewers regenerate
    // appearance streams; stet doesn't author them.
    let need_appearances = form_record.and_then(|f| f.need_appearances).unwrap_or(true);
    entries.push((b"NeedAppearances".to_vec(), PdfObj::Bool(need_appearances)));
    if let Some(form) = form_record {
        if let Some(flags) = form.sig_flags {
            entries.push((b"SigFlags".to_vec(), PdfObj::Int(flags as i64)));
        }
        if let Some(co) = &form.calc_order {
            entries.push((
                b"CO".to_vec(),
                PdfObj::Array(
                    co.iter()
                        .map(|n| PdfObj::LitString(n.clone().into_bytes()))
                        .collect(),
                ),
            ));
        }
        if let Some(da) = &form.default_appearance {
            entries.push((b"DA".to_vec(), PdfObj::LitString(da.clone().into_bytes())));
        }
        if let Some(q) = form.quadding {
            entries.push((b"Q".to_vec(), PdfObj::Int(q as i64)));
        }
    }
    entries
}

/// Build a container-only parent field dict (no /FT, no /V) — the
/// shape used for dotted-name implicit parents like the `order` and
/// `order.shipping` levels of `order.shipping.street`.
fn container_field_dict(
    segment: &str,
    parent_ref: Option<u32>,
    kids: &[u32],
) -> Vec<(Vec<u8>, PdfObj)> {
    let mut entries: Vec<(Vec<u8>, PdfObj)> = vec![(
        b"T".to_vec(),
        PdfObj::LitString(segment.as_bytes().to_vec()),
    )];
    if let Some(p) = parent_ref {
        entries.push((b"Parent".to_vec(), PdfObj::Ref(p)));
    }
    entries.push((
        b"Kids".to_vec(),
        PdfObj::Array(kids.iter().map(|&r| PdfObj::Ref(r)).collect()),
    ));
    entries
}

/// Build a radio-group parent field dict — lifts the field-level keys
/// (`/FT`, `/Ff`, `/V`, `/Q`, `/DA`, `/MaxLen`, `/Opt`, `/DV`) from
/// the first kid widget onto the parent. The parent owns the field
/// state; the kids are visual widgets only.
fn radio_parent_dict(
    segment: &str,
    parent_ref: Option<u32>,
    kid_refs: &[u32],
    template: &WidgetAnnotation,
) -> Vec<(Vec<u8>, PdfObj)> {
    let mut entries: Vec<(Vec<u8>, PdfObj)> = vec![(
        b"T".to_vec(),
        PdfObj::LitString(segment.as_bytes().to_vec()),
    )];
    if let Some(p) = parent_ref {
        entries.push((b"Parent".to_vec(), PdfObj::Ref(p)));
    }
    push_field_level_keys(&mut entries, template);
    entries.push((
        b"Kids".to_vec(),
        PdfObj::Array(kid_refs.iter().map(|&r| PdfObj::Ref(r)).collect()),
    ));
    entries
}

/// Append every recognised field-level key that the widget carries.
/// Shared by the merged-leaf-field path (in `annotations.rs`) and the
/// radio-group parent path here so the encoding stays consistent.
pub(crate) fn push_field_level_keys(
    entries: &mut Vec<(Vec<u8>, PdfObj)>,
    widget: &WidgetAnnotation,
) {
    if let Some(ft) = widget.field_type {
        entries.push((b"FT".to_vec(), PdfObj::name(field_type_name(ft))));
    }
    if let Some(flags) = widget.flags {
        entries.push((b"Ff".to_vec(), PdfObj::Int(flags as i64)));
    }
    if let Some(value) = &widget.value {
        entries.push((b"V".to_vec(), encode_field_value(value)));
    }
    if let Some(default) = &widget.default_value {
        entries.push((b"DV".to_vec(), encode_field_value(default)));
    }
    if let Some(max_len) = widget.max_len {
        entries.push((b"MaxLen".to_vec(), PdfObj::Int(max_len as i64)));
    }
    if let Some(opts) = &widget.options {
        entries.push((b"Opt".to_vec(), encode_options(opts)));
    }
    if let Some(q) = widget.quadding {
        entries.push((b"Q".to_vec(), PdfObj::Int(q as i64)));
    }
    if let Some(da) = &widget.default_appearance {
        entries.push((b"DA".to_vec(), PdfObj::LitString(da.clone().into_bytes())));
    }
}

/// PDF /FT name for a [`FieldType`].
fn field_type_name(ft: FieldType) -> &'static str {
    match ft {
        FieldType::Btn => "Btn",
        FieldType::Tx => "Tx",
        FieldType::Ch => "Ch",
        FieldType::Sig => "Sig",
    }
}

/// Encode a `/V` or `/DV` field value into the PDF object kind that
/// matches the PDF spec for its field type. Text → string, Name →
/// /Name, TextArray → array-of-strings.
fn encode_field_value(value: &FieldValue) -> PdfObj {
    match value {
        FieldValue::Text(s) => PdfObj::LitString(s.clone().into_bytes()),
        FieldValue::Name(n) => PdfObj::name(n),
        FieldValue::TextArray(vs) => PdfObj::Array(
            vs.iter()
                .map(|v| PdfObj::LitString(v.clone().into_bytes()))
                .collect(),
        ),
    }
}

/// Encode a choice-field /Opt array. Single-string options collapse
/// to `(string)`; export-vs-display pairs become `[(export) (display)]`.
fn encode_options(opts: &[ChoiceOption]) -> PdfObj {
    PdfObj::Array(
        opts.iter()
            .map(|o| {
                if o.export == o.display {
                    PdfObj::LitString(o.export.clone().into_bytes())
                } else {
                    PdfObj::Array(vec![
                        PdfObj::LitString(o.export.clone().into_bytes()),
                        PdfObj::LitString(o.display.clone().into_bytes()),
                    ])
                }
            })
            .collect(),
    )
}

/// Last dot-segment of a dotted field name. `""` for empty input.
fn leaf_segment_of(field_name: &str) -> &str {
    field_name.rsplit('.').find(|s| !s.is_empty()).unwrap_or("")
}

/// Collect the kids list for a given dotted prefix. Walks every full
/// field name in `by_name`, picks those whose path is exactly one
/// segment deeper than `prefix`, and resolves each to its
/// container-parent ref (multi-segment depth) or widget ref
/// (single-widget leaf at that depth).
fn collect_kids_for_prefix(
    prefix: &str,
    node_refs: &BTreeMap<String, u32>,
    by_name: &BTreeMap<String, Vec<usize>>,
    widget_refs_by_slot: &[u32],
) -> Vec<u32> {
    let prefix_depth = prefix.split('.').filter(|s| !s.is_empty()).count();
    let mut seen: BTreeMap<String, u32> = BTreeMap::new();
    for (full_name, slots) in by_name {
        let segments: Vec<&str> = full_name.split('.').filter(|s| !s.is_empty()).collect();
        if segments.len() <= prefix_depth {
            continue;
        }
        let candidate_prefix = segments[..prefix_depth].join(".");
        if candidate_prefix != prefix {
            continue;
        }
        let child_path = segments[..prefix_depth + 1].join(".");
        if seen.contains_key(&child_path) {
            continue;
        }
        // Container or radio-group parent → use node_refs entry.
        if let Some(&r) = node_refs.get(&child_path) {
            seen.insert(child_path, r);
            continue;
        }
        // Single-widget leaf at this depth — use the widget's annot
        // ref directly. The leaf is the case where `child_path ==
        // full_name` and `slots.len() == 1`.
        if &child_path == full_name && slots.len() == 1 {
            let slot = slots[0];
            if let Some(&r) = widget_refs_by_slot.get(slot) {
                seen.insert(child_path, r);
            }
        }
    }
    seen.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use stet_core::pdfmark::{AnnotationRecord, AnnotationSubtype, FieldType};

    fn make_widget(name: &str, ft: FieldType) -> AnnotationRecord {
        AnnotationRecord {
            page: 1,
            rect: [0.0, 0.0, 100.0, 20.0],
            color: None,
            border: None,
            title: None,
            contents: None,
            subtype: AnnotationSubtype::Widget(WidgetAnnotation {
                field_name: name.to_string(),
                field_type: Some(ft),
                ..WidgetAnnotation::default()
            }),
        }
    }

    #[test]
    fn leaf_segment_simple() {
        assert_eq!(leaf_segment_of("firstname"), "firstname");
    }

    #[test]
    fn leaf_segment_dotted() {
        assert_eq!(leaf_segment_of("order.shipping.street"), "street");
    }

    #[test]
    fn leaf_segment_trailing_dot() {
        // Trailing dots are dropped; the actual leaf wins.
        assert_eq!(leaf_segment_of("a.b."), "b");
    }

    #[test]
    fn write_form_emits_acroform_and_widget_per_page_refs() {
        let mut writer = PdfWriter::new();
        let widgets = vec![
            (0, make_widget("firstname", FieldType::Tx)),
            (1, make_widget("lastname", FieldType::Tx)),
        ];
        let out = write_form(&mut writer, &widgets, None, 1).expect("acroform emitted");
        // Two widgets on page 1 → both refs land in per_page_widget_refs[0].
        assert_eq!(out.per_page_widget_refs.len(), 1);
        assert_eq!(out.per_page_widget_refs[0].len(), 2);
        assert!(writer.is_object_set(out.acroform_ref));
    }

    #[test]
    fn write_form_dotted_name_creates_container_parents() {
        let mut writer = PdfWriter::new();
        let widgets = vec![(0, make_widget("order.shipping.street", FieldType::Tx))];
        let out = write_form(&mut writer, &widgets, None, 1).expect("acroform emitted");
        // /AcroForm /Fields should have one root (the "order" container).
        // Three field-related objects were allocated: order, shipping, street(=widget).
        assert!(writer.is_object_set(out.acroform_ref));
        assert_eq!(out.per_page_widget_refs[0].len(), 1);
    }

    #[test]
    fn write_form_radio_group_collects_three_kids() {
        let mut writer = PdfWriter::new();
        let widgets = vec![
            (0, make_widget("answer", FieldType::Btn)),
            (1, make_widget("answer", FieldType::Btn)),
            (2, make_widget("answer", FieldType::Btn)),
        ];
        let out = write_form(&mut writer, &widgets, None, 1).expect("acroform emitted");
        // Three widget refs in per-page even though they share a name.
        assert_eq!(out.per_page_widget_refs[0].len(), 3);
    }

    #[test]
    fn write_form_no_widgets_no_form_returns_none() {
        let mut writer = PdfWriter::new();
        assert!(write_form(&mut writer, &[], None, 1).is_none());
    }

    #[test]
    fn write_form_form_only_emits_minimal_acroform() {
        let mut writer = PdfWriter::new();
        let form = FormRecord {
            sig_flags: Some(3),
            ..FormRecord::default()
        };
        let out = write_form(&mut writer, &[], Some(&form), 1).expect("acroform emitted");
        assert!(writer.is_object_set(out.acroform_ref));
        assert!(out.per_page_widget_refs[0].is_empty());
    }
}
