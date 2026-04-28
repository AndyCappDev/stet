// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `pdfmark` operator and per-type-tag dispatch.
//!
//! `pdfmark` is the standard Adobe / GhostScript bridge for emitting PDF
//! structural features from PostScript code. The operator scans the
//! operand stack down to a `[` mark, treats the topmost item below
//! `pdfmark` as the type-tag name, and treats everything between the
//! mark and the type-tag as alternating key/value pairs. The dispatch
//! routes the parsed payload into [`stet_core::pdfmark::PdfMarkBuffer`]
//! on `Context`. The PDF output device drains that buffer at end-of-job;
//! non-PDF devices simply discard it.
//!
//! Currently recognised type-tags: `/DOCINFO` (document Info dict),
//! `/OUT` (outline / bookmark entry), `/ANN` (Link / Text / FreeText /
//! Widget annotations), `/DEST` (named destination), `/PAGE` and
//! `/PAGES` (per-page and document-wide page-box / rotate overrides),
//! `/VIEWERPREFERENCES` (catalog viewer preferences plus `/PageLayout`
//! and `/PageMode`), `/Metadata` (XMP stream), `/FORM` (document-level
//! AcroForm dict — `/Fields` is implicit, built from `/Widget`
//! annotations at write time). Subsequent phases add `/EMBED` and the
//! Tagged-PDF structure-tree set as new arms in the type-tag match.
//! Unknown type-tags silently no-op (Adobe convention) so PS code
//! targeting newer features stays runnable on older interpreters.

use stet_core::context::Context;
use stet_core::dict::DictKey;
use stet_core::error::PsError;
use stet_core::object::{EntityId, PsObject, PsValue};
use stet_core::pdfmark::{
    AnnotationRecord, AnnotationSubtype, AnnotationTarget, Border, ChoiceOption, DestRecord,
    DocDate, DocInfoRecord, FieldType, FieldValue, FormRecord, GoToTarget, LinkHighlight,
    MetadataRecord, OutlineAction, OutlineDestination, OutlineRecord, PageBoxes,
    PageOverrideRecord, PageOverrideScope, PdfMarkRecord, TextAnnotationIcon, TrappedState,
    ViewSpec, ViewerPrefsRecord, WidgetAnnotation,
};

/// `pdfmark`: mark arg1 ... argN typetag → —
///
/// Adobe-flavoured one-operator dispatcher. Locates the most recent `[`
/// mark, reads the type-tag (topmost name below the operator), and
/// hands everything between mark and type-tag to the per-type handler.
/// Adobe pdfmark is deliberately permissive: each type-tag has its own
/// payload shape (some are alternating key/value dicts, others — like
/// `/PUT`, `/OBJ`, `/EMBED` — interleave positional arguments with
/// dict-style pairs), so structural validation belongs in the
/// type-specific handler, not here.
///
/// Behaviour by error class:
///
/// - **No mark on stack** → `unmatchedmark`.
/// - **Type-tag missing or not a name** → `typecheck`.
/// - **Unknown type-tag** → silent no-op (Adobe convention so PS code
///   that targets newer PDF features stays runnable on older
///   interpreters; AI's `/PUT`, `/OBJ`, `/StRoleMap`, etc. land here
///   in stet until their respective phases ship).
/// - **Malformed payload inside a known type-tag** → handler-specific.
///   Most handlers ignore unrecognised entries silently; required-key
///   failures emit a diagnostic to stderr and skip the record.
///
/// Stack effect: pops everything from the mark up through the
/// type-tag (inclusive); pushes nothing.
pub fn op_pdfmark(ctx: &mut Context) -> Result<(), PsError> {
    let slice = ctx.o_stack.as_slice();
    let mark_pos = slice
        .iter()
        .rposition(|o| matches!(o.value, PsValue::Mark))
        .ok_or(PsError::UnmatchedMark)?;
    if slice.len() == mark_pos + 1 {
        return Err(PsError::TypeCheck);
    }
    let tag_name = match slice[slice.len() - 1].value {
        PsValue::Name(n) => n,
        _ => return Err(PsError::TypeCheck),
    };

    // Snapshot the payload before popping so handler helpers can read
    // strings / dicts via &Context without conflicting borrows.
    let payload: Vec<PsObject> = slice[mark_pos + 1..slice.len() - 1].to_vec();
    ctx.o_stack.truncate(mark_pos);

    let tag_bytes = ctx.names.get_bytes(tag_name).to_vec();
    match tag_bytes.as_slice() {
        b"DOCINFO" => {
            if let Some(rec) = parse_docinfo(ctx, &payload) {
                ctx.pdfmark_buffer.push(PdfMarkRecord::DocInfo(rec));
            }
        }
        b"OUT" => {
            if let Some(rec) = parse_outline(ctx, &payload) {
                ctx.pdfmark_buffer.push(PdfMarkRecord::Outline(rec));
            }
        }
        b"ANN" => {
            if let Some(rec) = parse_annotation(ctx, &payload) {
                ctx.pdfmark_buffer.push(PdfMarkRecord::Annotation(rec));
            }
        }
        b"DEST" => {
            if let Some(rec) = parse_dest(ctx, &payload) {
                ctx.pdfmark_buffer.push(PdfMarkRecord::Dest(rec));
            }
        }
        b"PAGE" => {
            if let Some(rec) = parse_page_override(ctx, &payload, false) {
                ctx.pdfmark_buffer.push(PdfMarkRecord::PageOverride(rec));
            }
        }
        b"PAGES" => {
            if let Some(rec) = parse_page_override(ctx, &payload, true) {
                ctx.pdfmark_buffer.push(PdfMarkRecord::PageOverride(rec));
            }
        }
        b"VIEWERPREFERENCES" => {
            if let Some(rec) = parse_viewer_prefs(ctx, &payload) {
                ctx.pdfmark_buffer.push(PdfMarkRecord::ViewerPrefs(rec));
            }
        }
        b"Metadata" => {
            if let Some(rec) = parse_metadata(ctx, &payload) {
                ctx.pdfmark_buffer.push(PdfMarkRecord::Metadata(rec));
            }
        }
        b"FORM" => {
            if let Some(rec) = parse_form(ctx, &payload) {
                ctx.pdfmark_buffer.push(PdfMarkRecord::Form(rec));
            }
        }
        // Unknown type-tags silently emit nothing (Adobe convention).
        _ => {}
    }
    Ok(())
}

/// Walk a pdfmark payload as alternating /Name value pairs. Stops at the
/// first item that doesn't fit (non-name where a key is expected, or a
/// missing trailing value). Used by type-tags whose payload is
/// dict-shaped (e.g. `/DOCINFO`, `/VIEWERPREFERENCES`).
fn pairs_iter<'a>(
    payload: &'a [PsObject],
) -> impl Iterator<Item = (stet_core::object::NameId, &'a PsObject)> + 'a {
    let mut i = 0;
    std::iter::from_fn(move || {
        while i + 1 < payload.len() {
            let key = match payload[i].value {
                PsValue::Name(n) => n,
                _ => {
                    i += 1;
                    continue;
                }
            };
            let value = &payload[i + 1];
            i += 2;
            return Some((key, value));
        }
        None
    })
}

/// Pull a `/Key (string)` entry out of a pdfmark payload as a Rust
/// `String`. Returns `None` when the key is absent or the value is not
/// a string. PostScript strings are arbitrary bytes; non-UTF-8 input is
/// replaced lossily so the writer always has a printable form.
fn extract_string(ctx: &Context, payload: &[PsObject], key: &[u8]) -> Option<String> {
    let key_id = ctx.names.find(key)?;
    for (k, v) in pairs_iter(payload) {
        if k != key_id {
            continue;
        }
        if let PsValue::String { entity, start, len } = v.value {
            let bytes = ctx.strings.get(entity, start, len);
            return Some(String::from_utf8_lossy(bytes).into_owned());
        }
    }
    None
}

/// Pull a `/Key /Name` entry. Returns the bytes of the name when
/// present, otherwise `None`.
fn extract_name<'a>(ctx: &'a Context, payload: &[PsObject], key: &[u8]) -> Option<&'a [u8]> {
    let key_id = ctx.names.find(key)?;
    for (k, v) in pairs_iter(payload) {
        if k != key_id {
            continue;
        }
        if let PsValue::Name(n) = v.value {
            return Some(ctx.names.get_bytes(n));
        }
    }
    None
}

/// Parse a `/DOCINFO` payload into a [`DocInfoRecord`]. Unknown keys
/// are ignored (Adobe convention). All recognised keys are optional;
/// the record is always produced — even when empty — so a bare
/// `[ /DOCINFO pdfmark` still records "the producer wanted to set Info
/// to empty".
fn parse_docinfo(ctx: &Context, payload: &[PsObject]) -> Option<DocInfoRecord> {
    Some(DocInfoRecord {
        title: extract_string(ctx, payload, b"Title"),
        author: extract_string(ctx, payload, b"Author"),
        subject: extract_string(ctx, payload, b"Subject"),
        keywords: extract_string(ctx, payload, b"Keywords"),
        creator: extract_string(ctx, payload, b"Creator"),
        producer: extract_string(ctx, payload, b"Producer"),
        creation_date: extract_string(ctx, payload, b"CreationDate").map(DocDate::Raw),
        mod_date: extract_string(ctx, payload, b"ModDate").map(DocDate::Raw),
        trapped: extract_name(ctx, payload, b"Trapped").and_then(|n| match n {
            b"True" => Some(TrappedState::True),
            b"False" => Some(TrappedState::False),
            b"Unknown" => Some(TrappedState::Unknown),
            _ => None,
        }),
    })
}

/// Pull a `/Key <int>` entry. Reals that round-trip cleanly count as
/// integers (`1.0` → `1`).
fn extract_i32(ctx: &Context, payload: &[PsObject], key: &[u8]) -> Option<i32> {
    let key_id = ctx.names.find(key)?;
    for (k, v) in pairs_iter(payload) {
        if k == key_id {
            return v.as_i32();
        }
    }
    None
}

/// Pull a `/Key <array>` entry, returning the array's elements as a
/// `Vec<PsObject>`. Returns `None` when the key is absent or the value
/// is not an array.
fn extract_array(ctx: &Context, payload: &[PsObject], key: &[u8]) -> Option<Vec<PsObject>> {
    let key_id = ctx.names.find(key)?;
    for (k, v) in pairs_iter(payload) {
        if k != key_id {
            continue;
        }
        let (entity, start, len) = match v.value {
            PsValue::Array { entity, start, len } | PsValue::PackedArray { entity, start, len } => {
                (entity, start, len)
            }
            _ => return None,
        };
        let mut out = Vec::with_capacity(len as usize);
        for i in 0..len {
            out.push(ctx.arrays.get_element(entity, start + i));
        }
        return Some(out);
    }
    None
}

/// Pull a `/Key <dict>` entry's `EntityId` so a handler can probe its
/// own keys directly.
fn extract_dict_entity(ctx: &Context, payload: &[PsObject], key: &[u8]) -> Option<EntityId> {
    let key_id = ctx.names.find(key)?;
    for (k, v) in pairs_iter(payload) {
        if k == key_id {
            return match v.value {
                PsValue::Dict(e) => Some(e),
                _ => None,
            };
        }
    }
    None
}

/// Treat a `PsObject` as a number (int or real) for `[/XYZ ...]` style
/// view specs. PostScript `null` becomes `None` (PDF spec: null in a
/// /Dest array means "keep current value").
fn obj_as_opt_f64(obj: &PsObject) -> Option<f64> {
    if matches!(obj.value, PsValue::Null) {
        return None;
    }
    obj.as_f64()
}

/// Decode a PDF view-spec array (`[/XYZ left top zoom]`, `[/Fit]`,
/// `[/FitR …]`, etc.) into a [`ViewSpec`]. Returns `None` for
/// malformed input — the caller can fall back to
/// [`ViewSpec::default()`].
fn parse_view_spec_with_ctx(ctx: &Context, elements: &[PsObject]) -> Option<ViewSpec> {
    let leading = match elements.first().map(|e| e.value) {
        Some(PsValue::Name(n)) => n,
        _ => return None,
    };
    let rest = &elements[1..];
    let kind = ctx.names.get_bytes(leading);
    match kind {
        b"XYZ" => Some(ViewSpec::Xyz {
            left: rest.first().and_then(obj_as_opt_f64),
            top: rest.get(1).and_then(obj_as_opt_f64),
            zoom: rest.get(2).and_then(obj_as_opt_f64),
        }),
        b"Fit" => Some(ViewSpec::Fit),
        b"FitH" => Some(ViewSpec::FitH(rest.first().and_then(obj_as_opt_f64))),
        b"FitV" => Some(ViewSpec::FitV(rest.first().and_then(obj_as_opt_f64))),
        b"FitR" if rest.len() >= 4 => Some(ViewSpec::FitR {
            left: rest[0].as_f64()?,
            bottom: rest[1].as_f64()?,
            right: rest[2].as_f64()?,
            top: rest[3].as_f64()?,
        }),
        b"FitB" => Some(ViewSpec::FitB),
        b"FitBH" => Some(ViewSpec::FitBH(rest.first().and_then(obj_as_opt_f64))),
        b"FitBV" => Some(ViewSpec::FitBV(rest.first().and_then(obj_as_opt_f64))),
        _ => None,
    }
}

/// Decode an `/Action <<...>>` dict into an [`OutlineAction`]. Currently
/// recognises `/S /URI` and `/S /GoTo` (with both named and explicit
/// destinations).
fn parse_action_dict(ctx: &Context, dict: EntityId) -> Option<OutlineAction> {
    let s_id = ctx.names.find(b"S")?;
    let s_obj = ctx.dicts.get(dict, &DictKey::Name(s_id))?;
    let s_name = match s_obj.value {
        PsValue::Name(n) => n,
        _ => return None,
    };
    match ctx.names.get_bytes(s_name) {
        b"URI" => {
            let uri_id = ctx.names.find(b"URI")?;
            let uri_obj = ctx.dicts.get(dict, &DictKey::Name(uri_id))?;
            let (entity, start, len) = match uri_obj.value {
                PsValue::String { entity, start, len } => (entity, start, len),
                _ => return None,
            };
            let bytes = ctx.strings.get(entity, start, len);
            Some(OutlineAction::Uri(
                String::from_utf8_lossy(bytes).into_owned(),
            ))
        }
        b"GoTo" => {
            let d_id = ctx.names.find(b"D")?;
            let d_obj = ctx.dicts.get(dict, &DictKey::Name(d_id))?;
            match d_obj.value {
                PsValue::Name(n) => Some(OutlineAction::GoTo(GoToTarget::Named(
                    String::from_utf8_lossy(ctx.names.get_bytes(n)).into_owned(),
                ))),
                PsValue::String { entity, start, len } => {
                    let bytes = ctx.strings.get(entity, start, len);
                    Some(OutlineAction::GoTo(GoToTarget::Named(
                        String::from_utf8_lossy(bytes).into_owned(),
                    )))
                }
                PsValue::Array { entity, start, len }
                | PsValue::PackedArray { entity, start, len } => {
                    if len < 2 {
                        return None;
                    }
                    let page = ctx.arrays.get_element(entity, start).as_i32()?;
                    let page = u32::try_from(page).ok()?;
                    let mut rest = Vec::with_capacity(len as usize - 1);
                    for i in 1..len {
                        rest.push(ctx.arrays.get_element(entity, start + i));
                    }
                    let view = parse_view_spec_with_ctx(ctx, &rest).unwrap_or_default();
                    Some(OutlineAction::GoTo(GoToTarget::Explicit { page, view }))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Resolve `/Page` + optional `/View`, `/Dest`, or `/Action` keys into
/// an [`OutlineDestination`]. Returns `None` when none of the three is
/// present (the bookmark is a non-navigable label). `/Action` wins over
/// `/Dest` wins over `/Page`, matching GhostScript pdfwrite precedence.
fn extract_outline_destination(ctx: &Context, payload: &[PsObject]) -> Option<OutlineDestination> {
    if let Some(dict) = extract_dict_entity(ctx, payload, b"Action")
        && let Some(action) = parse_action_dict(ctx, dict)
    {
        return Some(OutlineDestination::Action(action));
    }
    if let Some(name) = extract_name(ctx, payload, b"Dest") {
        return Some(OutlineDestination::NamedDest(
            String::from_utf8_lossy(name).into_owned(),
        ));
    }
    if let Some(name) = extract_string(ctx, payload, b"Dest") {
        return Some(OutlineDestination::NamedDest(name));
    }
    if let Some(page) = extract_i32(ctx, payload, b"Page")
        && let Ok(page) = u32::try_from(page)
    {
        let view = extract_array(ctx, payload, b"View")
            .as_deref()
            .and_then(|el| parse_view_spec_with_ctx(ctx, el))
            .unwrap_or_default();
        return Some(OutlineDestination::PageView { page, view });
    }
    None
}

/// Parse a `/OUT pdfmark` payload into an [`OutlineRecord`]. Records
/// without a `/Title` are dropped silently — there is nothing useful to
/// emit for a titleless bookmark, and Adobe pdfwrite behaves the same.
fn parse_outline(ctx: &Context, payload: &[PsObject]) -> Option<OutlineRecord> {
    let title = extract_string(ctx, payload, b"Title")?;
    let destination = extract_outline_destination(ctx, payload);
    let count = extract_i32(ctx, payload, b"Count");
    let outline_level = extract_i32(ctx, payload, b"OutlineLevel")
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 0);
    let color = extract_array(ctx, payload, b"Color").and_then(|el| {
        if el.len() < 3 {
            return None;
        }
        Some([el[0].as_f64()?, el[1].as_f64()?, el[2].as_f64()?])
    });
    let flags = extract_i32(ctx, payload, b"F").and_then(|n| u32::try_from(n).ok());
    Some(OutlineRecord {
        title,
        destination,
        count,
        outline_level,
        color,
        flags,
    })
}

// ----- Annotation parsing (Phase 3) ----------------------------------------

/// Decode a PDF view-spec array but accept any length sub-slice. Used
/// by both the bookmark and the link-annotation paths.
fn parse_view_spec_or_default(ctx: &Context, elements: &[PsObject]) -> ViewSpec {
    parse_view_spec_with_ctx(ctx, elements).unwrap_or_default()
}

/// Resolve `/Page` + optional `/View`, `/Dest`, or `/Action` keys into
/// an [`AnnotationTarget`] for `/Link` annotations. Mirrors
/// [`extract_outline_destination`] but emits the annotation-flavoured
/// enum so the writer can dispatch differently when needed.
fn extract_annotation_target(ctx: &Context, payload: &[PsObject]) -> Option<AnnotationTarget> {
    if let Some(dict) = extract_dict_entity(ctx, payload, b"Action")
        && let Some(action) = parse_action_dict(ctx, dict)
    {
        return Some(AnnotationTarget::Action(action));
    }
    if let Some(name) = extract_name(ctx, payload, b"Dest") {
        return Some(AnnotationTarget::NamedDest(
            String::from_utf8_lossy(name).into_owned(),
        ));
    }
    if let Some(name) = extract_string(ctx, payload, b"Dest") {
        return Some(AnnotationTarget::NamedDest(name));
    }
    if let Some(page) = extract_i32(ctx, payload, b"Page")
        && let Ok(page) = u32::try_from(page)
    {
        let view = extract_array(ctx, payload, b"View")
            .as_deref()
            .map(|el| parse_view_spec_or_default(ctx, el))
            .unwrap_or_default();
        return Some(AnnotationTarget::PageView { page, view });
    }
    None
}

/// Pull a `/Border [Hradius Vradius Width [dash...]]` array.
fn extract_border(ctx: &Context, payload: &[PsObject]) -> Option<Border> {
    let elements = extract_array(ctx, payload, b"Border")?;
    if elements.len() < 3 {
        return None;
    }
    let mut border = Border {
        h_radius: elements[0].as_f64()?,
        v_radius: elements[1].as_f64()?,
        width: elements[2].as_f64()?,
        dash: None,
    };
    if elements.len() >= 4 {
        let (entity, start, len) = match elements[3].value {
            PsValue::Array { entity, start, len } | PsValue::PackedArray { entity, start, len } => {
                (entity, start, len)
            }
            _ => return Some(border),
        };
        let mut dash = Vec::with_capacity(len as usize);
        for i in 0..len {
            if let Some(v) = ctx.arrays.get_element(entity, start + i).as_f64() {
                dash.push(v);
            } else {
                return Some(border); // malformed dash → keep base border, drop dash
            }
        }
        border.dash = Some(dash);
    }
    Some(border)
}

fn link_highlight_from_name(name: &[u8]) -> Option<LinkHighlight> {
    Some(match name {
        b"N" => LinkHighlight::None,
        b"I" => LinkHighlight::Invert,
        b"O" => LinkHighlight::Outline,
        b"P" => LinkHighlight::Push,
        _ => return None,
    })
}

fn text_icon_from_name(name: &[u8]) -> TextAnnotationIcon {
    match name {
        b"Comment" => TextAnnotationIcon::Comment,
        b"Note" => TextAnnotationIcon::Note,
        b"Key" => TextAnnotationIcon::Key,
        b"Help" => TextAnnotationIcon::Help,
        b"NewParagraph" => TextAnnotationIcon::NewParagraph,
        b"Paragraph" => TextAnnotationIcon::Paragraph,
        b"Insert" => TextAnnotationIcon::Insert,
        // Anything else falls back to /Note per Adobe convention.
        _ => TextAnnotationIcon::Note,
    }
}

/// Pull `/Subtype` and dispatch to the right [`AnnotationSubtype`]
/// variant.
fn parse_annotation_subtype(ctx: &Context, payload: &[PsObject]) -> Option<AnnotationSubtype> {
    let subtype = extract_name(ctx, payload, b"Subtype")?;
    match subtype {
        b"Link" => {
            let target = extract_annotation_target(ctx, payload);
            let highlight = extract_name(ctx, payload, b"H").and_then(link_highlight_from_name);
            Some(AnnotationSubtype::Link { target, highlight })
        }
        b"Text" => {
            let open = matches!(extract_name(ctx, payload, b"Open"), Some(b"true"))
                || extract_bool(ctx, payload, b"Open").unwrap_or(false);
            let icon = extract_name(ctx, payload, b"Name")
                .map(text_icon_from_name)
                .unwrap_or_default();
            Some(AnnotationSubtype::Text { open, icon })
        }
        b"FreeText" => {
            let default_appearance = extract_string(ctx, payload, b"DA");
            let quadding = extract_i32(ctx, payload, b"Q").and_then(|n| u32::try_from(n).ok());
            Some(AnnotationSubtype::FreeText {
                default_appearance,
                quadding,
            })
        }
        b"Widget" => parse_widget_payload(ctx, payload).map(AnnotationSubtype::Widget),
        // Other subtypes (Stamp, …) land in later phases; for now treat
        // them as "no record" so unknown subtypes don't litter the
        // output PDF.
        _ => None,
    }
}

/// Pull a `/Key` value, accepting either a literal name or a string and
/// returning the bytes as a UTF-8 lossy `String`. Used wherever the PDF
/// spec is loose about whether a key is a name or a text string —
/// notably field names (`/T`), action target names (`/D`), and choice
/// option strings (`/Opt` entries).
fn extract_name_or_string(ctx: &Context, payload: &[PsObject], key: &[u8]) -> Option<String> {
    if let Some(name) = extract_name(ctx, payload, key) {
        return Some(String::from_utf8_lossy(name).into_owned());
    }
    extract_string(ctx, payload, key)
}

/// Decode an `/FT` name into a [`FieldType`].
fn parse_field_type(name: &[u8]) -> Option<FieldType> {
    Some(match name {
        b"Btn" => FieldType::Btn,
        b"Tx" => FieldType::Tx,
        b"Ch" => FieldType::Ch,
        b"Sig" => FieldType::Sig,
        _ => return None,
    })
}

/// Read a field value (`/V` or `/DV`). PDF accepts strings (text fields,
/// single-select choice), names (button checkboxes / radio appearance
/// states), and arrays of strings (multi-select choice). Returns `None`
/// when the key is missing or the value isn't one of those kinds.
fn extract_field_value(ctx: &Context, payload: &[PsObject], key: &[u8]) -> Option<FieldValue> {
    let key_id = ctx.names.find(key)?;
    for (k, v) in pairs_iter(payload) {
        if k != key_id {
            continue;
        }
        match v.value {
            PsValue::String { entity, start, len } => {
                let bytes = ctx.strings.get(entity, start, len);
                return Some(FieldValue::Text(
                    String::from_utf8_lossy(bytes).into_owned(),
                ));
            }
            PsValue::Name(n) => {
                return Some(FieldValue::Name(
                    String::from_utf8_lossy(ctx.names.get_bytes(n)).into_owned(),
                ));
            }
            PsValue::Array { entity, start, len } | PsValue::PackedArray { entity, start, len } => {
                let mut items = Vec::with_capacity(len as usize);
                for i in 0..len {
                    let elem = ctx.arrays.get_element(entity, start + i);
                    match elem.value {
                        PsValue::String {
                            entity: se,
                            start: ss,
                            len: sl,
                        } => {
                            let bytes = ctx.strings.get(se, ss, sl);
                            items.push(String::from_utf8_lossy(bytes).into_owned());
                        }
                        _ => return None,
                    }
                }
                return Some(FieldValue::TextArray(items));
            }
            _ => return None,
        }
    }
    None
}

/// Read a `/Opt` array. Each entry is either a single string (display =
/// export) or `[export display]`. Anything else is dropped.
fn extract_options(ctx: &Context, payload: &[PsObject]) -> Option<Vec<ChoiceOption>> {
    let elements = extract_array(ctx, payload, b"Opt")?;
    let mut out = Vec::with_capacity(elements.len());
    for elem in &elements {
        match elem.value {
            PsValue::String { entity, start, len } => {
                let bytes = ctx.strings.get(entity, start, len);
                let s = String::from_utf8_lossy(bytes).into_owned();
                out.push(ChoiceOption {
                    export: s.clone(),
                    display: s,
                });
            }
            PsValue::Array { entity, start, len } | PsValue::PackedArray { entity, start, len }
                if len >= 2 =>
            {
                let take_str = |idx: u32| -> Option<String> {
                    let e = ctx.arrays.get_element(entity, start + idx);
                    match e.value {
                        PsValue::String {
                            entity: se,
                            start: ss,
                            len: sl,
                        } => {
                            Some(String::from_utf8_lossy(ctx.strings.get(se, ss, sl)).into_owned())
                        }
                        _ => None,
                    }
                };
                if let (Some(export), Some(display)) = (take_str(0), take_str(1)) {
                    out.push(ChoiceOption { export, display });
                }
            }
            _ => {}
        }
    }
    if out.is_empty() {
        return None;
    }
    Some(out)
}

/// Parse a `/Subtype /Widget` payload into a [`WidgetAnnotation`].
/// Returns `None` when `/T` is missing — every form field needs a name
/// to live under in the field tree, and Adobe pdfwrite drops widgets
/// that lack one.
fn parse_widget_payload(ctx: &Context, payload: &[PsObject]) -> Option<WidgetAnnotation> {
    let field_name = extract_name_or_string(ctx, payload, b"T")?;
    if field_name.is_empty() {
        return None;
    }
    let field_type = extract_name(ctx, payload, b"FT").and_then(parse_field_type);
    let value = extract_field_value(ctx, payload, b"V");
    let default_value = extract_field_value(ctx, payload, b"DV");
    let flags = extract_i32(ctx, payload, b"Ff");
    let max_len = extract_i32(ctx, payload, b"MaxLen");
    let options = extract_options(ctx, payload);
    let quadding = extract_i32(ctx, payload, b"Q");
    let default_appearance = extract_string(ctx, payload, b"DA");
    Some(WidgetAnnotation {
        field_name,
        field_type,
        value,
        default_value,
        flags,
        max_len,
        options,
        quadding,
        default_appearance,
    })
}

/// Parse a `/FORM pdfmark` payload into a [`FormRecord`]. All keys are
/// optional. Returns `None` when the payload sets no recognised key —
/// stray pdfmarks shouldn't pollute /AcroForm.
fn parse_form(ctx: &Context, payload: &[PsObject]) -> Option<FormRecord> {
    let need_appearances = extract_bool(ctx, payload, b"NeedAppearances");
    let sig_flags = extract_i32(ctx, payload, b"SigFlags");
    let calc_order = extract_array(ctx, payload, b"CO").and_then(|elements| {
        let mut names = Vec::with_capacity(elements.len());
        for elem in &elements {
            match elem.value {
                PsValue::String { entity, start, len } => {
                    names.push(
                        String::from_utf8_lossy(ctx.strings.get(entity, start, len)).into_owned(),
                    );
                }
                PsValue::Name(n) => {
                    names.push(String::from_utf8_lossy(ctx.names.get_bytes(n)).into_owned());
                }
                _ => return None,
            }
        }
        Some(names)
    });
    let default_appearance = extract_string(ctx, payload, b"DA");
    let quadding = extract_i32(ctx, payload, b"Q");

    let any = need_appearances.is_some()
        || sig_flags.is_some()
        || calc_order.is_some()
        || default_appearance.is_some()
        || quadding.is_some();
    if !any {
        return None;
    }
    Some(FormRecord {
        need_appearances,
        sig_flags,
        calc_order,
        default_appearance,
        quadding,
    })
}

/// Pull a `/Key true|false` entry. PostScript also accepts `/Open
/// /true` (a name) — that path is handled separately by the caller.
fn extract_bool(ctx: &Context, payload: &[PsObject], key: &[u8]) -> Option<bool> {
    let key_id = ctx.names.find(key)?;
    for (k, v) in pairs_iter(payload) {
        if k == key_id
            && let PsValue::Bool(b) = v.value
        {
            return Some(b);
        }
    }
    None
}

/// Pull a `/Rect [llx lly urx ury]`. Returns `None` only when the
/// array is missing entirely; malformed arrays default to the empty
/// rect so the writer still produces *some* annotation rather than
/// silently dropping the record.
fn extract_rect(ctx: &Context, payload: &[PsObject]) -> Option<[f64; 4]> {
    let elements = extract_array(ctx, payload, b"Rect")?;
    if elements.len() < 4 {
        return Some([0.0; 4]);
    }
    Some([
        elements[0].as_f64().unwrap_or(0.0),
        elements[1].as_f64().unwrap_or(0.0),
        elements[2].as_f64().unwrap_or(0.0),
        elements[3].as_f64().unwrap_or(0.0),
    ])
}

/// Resolve the `/Page` (or `/SrcPg`) key, falling back to the page
/// currently being assembled (`current_page + 1`).
fn resolve_annotation_page(ctx: &Context, payload: &[PsObject]) -> u32 {
    if let Some(page) = extract_i32(ctx, payload, b"Page")
        && let Ok(p) = u32::try_from(page)
        && p > 0
    {
        return p;
    }
    if let Some(page) = extract_i32(ctx, payload, b"SrcPg")
        && let Ok(p) = u32::try_from(page)
        && p > 0
    {
        return p;
    }
    ctx.pdfmark_buffer.current_page + 1
}

/// Parse an `/ANN pdfmark` payload. Records without a recognised
/// `/Subtype` (or without a `/Rect`) are dropped silently — Adobe
/// pdfwrite behaves the same way and silently dropping is the only
/// reasonable behaviour for an annotation that has no on-page footprint.
fn parse_annotation(ctx: &Context, payload: &[PsObject]) -> Option<AnnotationRecord> {
    let subtype = parse_annotation_subtype(ctx, payload)?;
    let rect = extract_rect(ctx, payload)?;
    let page = resolve_annotation_page(ctx, payload);
    let color = extract_array(ctx, payload, b"Color").and_then(|el| {
        if el.len() < 3 {
            return None;
        }
        Some([el[0].as_f64()?, el[1].as_f64()?, el[2].as_f64()?])
    });
    let border = extract_border(ctx, payload);
    let title = extract_string(ctx, payload, b"Title");
    let contents = extract_string(ctx, payload, b"Contents");
    Some(AnnotationRecord {
        page,
        rect,
        color,
        border,
        title,
        contents,
        subtype,
    })
}

// ----- Named destinations + page overrides (Phase 4) ----------------------

/// Resolve `/Dest` to a destination name. Accepts both literal names
/// (`/myname`) and string forms (`(myname)`).
fn extract_dest_name(ctx: &Context, payload: &[PsObject]) -> Option<String> {
    if let Some(name) = extract_name(ctx, payload, b"Dest") {
        return Some(String::from_utf8_lossy(name).into_owned());
    }
    extract_string(ctx, payload, b"Dest")
}

/// Parse a `/DEST pdfmark` payload. Required keys: `/Dest` (name) and
/// `/Page` (positive integer). `/View` is optional; when absent the
/// PDF spec default `[/XYZ null null null]` applies.
fn parse_dest(ctx: &Context, payload: &[PsObject]) -> Option<DestRecord> {
    let name = extract_dest_name(ctx, payload)?;
    let page = extract_i32(ctx, payload, b"Page").and_then(|n| u32::try_from(n).ok())?;
    if page == 0 {
        return None;
    }
    let view = extract_array(ctx, payload, b"View")
        .as_deref()
        .map(|el| parse_view_spec_or_default(ctx, el))
        .unwrap_or_default();
    Some(DestRecord { name, page, view })
}

/// Pull a `/Key [llx lly urx ury]` rectangle.
fn extract_rect_array(ctx: &Context, payload: &[PsObject], key: &[u8]) -> Option<[f64; 4]> {
    let elements = extract_array(ctx, payload, key)?;
    if elements.len() < 4 {
        return None;
    }
    Some([
        elements[0].as_f64()?,
        elements[1].as_f64()?,
        elements[2].as_f64()?,
        elements[3].as_f64()?,
    ])
}

/// Parse a `/PAGE` (per-page) or `/PAGES` (document-wide) pdfmark
/// payload. `is_pages` switches the scope. `/PAGE` records that omit
/// `/Page` (or `/SrcPg`) target the page being assembled — same
/// implicit-scoping rule as `/ANN`.
fn parse_page_override(
    ctx: &Context,
    payload: &[PsObject],
    is_pages: bool,
) -> Option<PageOverrideRecord> {
    let scope = if is_pages {
        PageOverrideScope::All
    } else {
        let page = if let Some(n) = extract_i32(ctx, payload, b"Page") {
            u32::try_from(n).ok().filter(|p| *p > 0)
        } else if let Some(n) = extract_i32(ctx, payload, b"SrcPg") {
            u32::try_from(n).ok().filter(|p| *p > 0)
        } else {
            None
        }
        .unwrap_or(ctx.pdfmark_buffer.current_page + 1);
        PageOverrideScope::Single(page)
    };
    let boxes = PageBoxes {
        crop_box: extract_rect_array(ctx, payload, b"CropBox"),
        bleed_box: extract_rect_array(ctx, payload, b"BleedBox"),
        trim_box: extract_rect_array(ctx, payload, b"TrimBox"),
        art_box: extract_rect_array(ctx, payload, b"ArtBox"),
    };
    let rotate = extract_i32(ctx, payload, b"Rotate");
    if boxes.is_empty() && rotate.is_none() {
        return None;
    }
    Some(PageOverrideRecord {
        scope,
        boxes,
        rotate,
    })
}

// ----- Viewer prefs + metadata (Phase 5) -----------------------------------

/// Pull a `/Key /Name` and return its bytes as a UTF-8 lossy `String`.
/// Convenience for viewer-prefs entries that store name-typed values
/// (`/PageMode`, `/PageLayout`, `/Direction`, …).
fn extract_name_string(ctx: &Context, payload: &[PsObject], key: &[u8]) -> Option<String> {
    extract_name(ctx, payload, key).map(|n| String::from_utf8_lossy(n).into_owned())
}

/// Parse a `/VIEWERPREFERENCES pdfmark` payload. Every recognised key
/// is optional; the record is dropped if no recognised key is set so
/// stray pdfmarks don't pollute the catalog.
fn parse_viewer_prefs(ctx: &Context, payload: &[PsObject]) -> Option<ViewerPrefsRecord> {
    let rec = ViewerPrefsRecord {
        hide_toolbar: extract_bool(ctx, payload, b"HideToolbar"),
        hide_menubar: extract_bool(ctx, payload, b"HideMenubar"),
        hide_window_ui: extract_bool(ctx, payload, b"HideWindowUI"),
        fit_window: extract_bool(ctx, payload, b"FitWindow"),
        center_window: extract_bool(ctx, payload, b"CenterWindow"),
        display_doc_title: extract_bool(ctx, payload, b"DisplayDocTitle"),
        non_full_screen_page_mode: extract_name_string(ctx, payload, b"NonFullScreenPageMode"),
        direction: extract_name_string(ctx, payload, b"Direction"),
        page_layout: extract_name_string(ctx, payload, b"PageLayout"),
        page_mode: extract_name_string(ctx, payload, b"PageMode"),
    };
    if rec.nested_is_empty() && rec.page_layout.is_none() && rec.page_mode.is_none() {
        return None;
    }
    Some(rec)
}

/// Parse a `/Metadata pdfmark` payload. The `/Metadata` value is the
/// raw XMP XML; pass through bytes verbatim so producers that hand-
/// craft their XMP keep byte-for-byte fidelity. Returns `None` when
/// the value is missing or not a string.
fn parse_metadata(ctx: &Context, payload: &[PsObject]) -> Option<MetadataRecord> {
    let key_id = ctx.names.find(b"Metadata")?;
    for (k, v) in pairs_iter(payload) {
        if k != key_id {
            continue;
        }
        if let PsValue::String { entity, start, len } = v.value {
            let bytes = ctx.strings.get(entity, start, len).to_vec();
            return Some(MetadataRecord { xmp_bytes: bytes });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use stet_core::object::PsObject;

    /// Build a minimal context with the `pdfmark` operator registered.
    fn make_ctx() -> Context {
        let mut ctx = Context::new();
        crate::build_system_dict(&mut ctx);
        crate::register_pdf_authoring_ops(&mut ctx);
        ctx
    }

    /// Push `[ /Title (Hello) /DOCINFO` onto the operand stack of `ctx`.
    fn push_docinfo_simple(ctx: &mut Context) {
        let title_id = ctx.names.intern(b"Title");
        let docinfo_id = ctx.names.intern(b"DOCINFO");
        let title_str = ctx.strings.allocate_from(b"Hello");
        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::name_lit(title_id)).unwrap();
        ctx.o_stack.push(PsObject::string(title_str, 5)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(docinfo_id)).unwrap();
    }

    #[test]
    fn docinfo_round_trip() {
        let mut ctx = make_ctx();
        push_docinfo_simple(&mut ctx);
        op_pdfmark(&mut ctx).unwrap();
        assert!(ctx.o_stack.is_empty());
        assert_eq!(ctx.pdfmark_buffer.records().len(), 1);
        let PdfMarkRecord::DocInfo(rec) = &ctx.pdfmark_buffer.records()[0] else {
            panic!("expected DocInfo record");
        };
        assert_eq!(rec.title.as_deref(), Some("Hello"));
        assert!(rec.author.is_none());
    }

    #[test]
    fn unmatched_mark_errors() {
        let mut ctx = make_ctx();
        let docinfo_id = ctx.names.intern(b"DOCINFO");
        ctx.o_stack.push(PsObject::name_lit(docinfo_id)).unwrap();
        assert!(matches!(op_pdfmark(&mut ctx), Err(PsError::UnmatchedMark)));
    }

    #[test]
    fn unknown_tag_silent_noop() {
        let mut ctx = make_ctx();
        let unknown_id = ctx.names.intern(b"NEWFEATURE");
        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::name_lit(unknown_id)).unwrap();
        op_pdfmark(&mut ctx).unwrap();
        assert!(ctx.o_stack.is_empty());
        assert!(ctx.pdfmark_buffer.is_empty());
    }

    #[test]
    fn missing_typetag_errors() {
        let mut ctx = make_ctx();
        ctx.o_stack.push(PsObject::mark()).unwrap();
        assert!(matches!(op_pdfmark(&mut ctx), Err(PsError::TypeCheck)));
    }

    #[test]
    fn typetag_must_be_name() {
        let mut ctx = make_ctx();
        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::int(42)).unwrap();
        assert!(matches!(op_pdfmark(&mut ctx), Err(PsError::TypeCheck)));
    }

    #[test]
    fn odd_payload_silently_ignored() {
        // Adobe pdfmark is permissive: an odd payload (key without value)
        // doesn't error, the dispatcher just no-ops the dangling key when
        // the type-tag's handler walks pairs. /DOCINFO with a bare /Title
        // produces an empty record.
        let mut ctx = make_ctx();
        let title_id = ctx.names.intern(b"Title");
        let docinfo_id = ctx.names.intern(b"DOCINFO");
        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::name_lit(title_id)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(docinfo_id)).unwrap();
        op_pdfmark(&mut ctx).unwrap();
        assert!(ctx.o_stack.is_empty());
        assert_eq!(ctx.pdfmark_buffer.records().len(), 1);
        let PdfMarkRecord::DocInfo(rec) = &ctx.pdfmark_buffer.records()[0] else {
            panic!("expected DocInfo record");
        };
        assert!(rec.title.is_none());
    }

    #[test]
    fn non_name_payload_silently_skipped() {
        // Adobe Illustrator's `/PUT` pattern is `[ <stream> -file- /PUT
        // pdfmark` — neither item between the mark and the type-tag is
        // a name. With `/PUT` unimplemented, the dispatcher should just
        // clear the stack without raising.
        let mut ctx = make_ctx();
        let put_id = ctx.names.intern(b"PUT");
        let str_e = ctx.strings.allocate_from(b"stream-bytes");
        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::int(7)).unwrap();
        ctx.o_stack.push(PsObject::string(str_e, 12)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(put_id)).unwrap();
        op_pdfmark(&mut ctx).unwrap();
        assert!(ctx.o_stack.is_empty());
        assert!(ctx.pdfmark_buffer.is_empty());
    }

    #[test]
    fn docinfo_all_keys() {
        let mut ctx = make_ctx();
        let names: Vec<_> = [
            "Title", "Author", "Subject", "Keywords", "Creator", "Producer",
        ]
        .iter()
        .map(|n| ctx.names.intern(n.as_bytes()))
        .collect();
        let strs: Vec<_> = [
            (b"T".as_slice(), 1u32),
            (b"A".as_slice(), 1),
            (b"S".as_slice(), 1),
            (b"K".as_slice(), 1),
            (b"C".as_slice(), 1),
            (b"P".as_slice(), 1),
        ]
        .iter()
        .map(|(s, l)| {
            let e = ctx.strings.allocate_from(s);
            PsObject::string(e, *l)
        })
        .collect();
        let docinfo_id = ctx.names.intern(b"DOCINFO");
        ctx.o_stack.push(PsObject::mark()).unwrap();
        for (n, v) in names.iter().zip(strs.iter()) {
            ctx.o_stack.push(PsObject::name_lit(*n)).unwrap();
            ctx.o_stack.push(*v).unwrap();
        }
        ctx.o_stack.push(PsObject::name_lit(docinfo_id)).unwrap();

        op_pdfmark(&mut ctx).unwrap();
        let PdfMarkRecord::DocInfo(rec) = &ctx.pdfmark_buffer.records()[0] else {
            panic!("expected DocInfo record");
        };
        assert_eq!(rec.title.as_deref(), Some("T"));
        assert_eq!(rec.author.as_deref(), Some("A"));
        assert_eq!(rec.subject.as_deref(), Some("S"));
        assert_eq!(rec.keywords.as_deref(), Some("K"));
        assert_eq!(rec.creator.as_deref(), Some("C"));
        assert_eq!(rec.producer.as_deref(), Some("P"));
    }

    /// Push `[ /Title (Hello) /Page 5 /OUT` so the dispatcher records
    /// one outline entry that targets page 5.
    fn push_outline_simple(ctx: &mut Context) {
        let title_id = ctx.names.intern(b"Title");
        let page_id = ctx.names.intern(b"Page");
        let out_id = ctx.names.intern(b"OUT");
        let title_str = ctx.strings.allocate_from(b"Hello");
        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::name_lit(title_id)).unwrap();
        ctx.o_stack.push(PsObject::string(title_str, 5)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(page_id)).unwrap();
        ctx.o_stack.push(PsObject::int(5)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(out_id)).unwrap();
    }

    #[test]
    fn outline_simple_page_target() {
        let mut ctx = make_ctx();
        push_outline_simple(&mut ctx);
        op_pdfmark(&mut ctx).unwrap();
        assert!(ctx.o_stack.is_empty());
        assert_eq!(ctx.pdfmark_buffer.records().len(), 1);
        let PdfMarkRecord::Outline(rec) = &ctx.pdfmark_buffer.records()[0] else {
            panic!("expected Outline record");
        };
        assert_eq!(rec.title, "Hello");
        match &rec.destination {
            Some(OutlineDestination::PageView { page, .. }) => assert_eq!(*page, 5),
            other => panic!("expected PageView destination, got {other:?}"),
        }
    }

    #[test]
    fn outline_titleless_dropped() {
        let mut ctx = make_ctx();
        let page_id = ctx.names.intern(b"Page");
        let out_id = ctx.names.intern(b"OUT");
        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::name_lit(page_id)).unwrap();
        ctx.o_stack.push(PsObject::int(2)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(out_id)).unwrap();
        op_pdfmark(&mut ctx).unwrap();
        assert!(ctx.pdfmark_buffer.is_empty());
    }

    #[test]
    fn outline_count_recorded() {
        let mut ctx = make_ctx();
        let title_id = ctx.names.intern(b"Title");
        let count_id = ctx.names.intern(b"Count");
        let out_id = ctx.names.intern(b"OUT");
        let s = ctx.strings.allocate_from(b"Parent");
        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::name_lit(title_id)).unwrap();
        ctx.o_stack.push(PsObject::string(s, 6)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(count_id)).unwrap();
        ctx.o_stack.push(PsObject::int(-3)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(out_id)).unwrap();
        op_pdfmark(&mut ctx).unwrap();
        let PdfMarkRecord::Outline(rec) = &ctx.pdfmark_buffer.records()[0] else {
            panic!("expected Outline record");
        };
        assert_eq!(rec.count, Some(-3));
    }

    #[test]
    fn outline_outline_level_normalises_negative_to_none() {
        let mut ctx = make_ctx();
        let title_id = ctx.names.intern(b"Title");
        let level_id = ctx.names.intern(b"OutlineLevel");
        let out_id = ctx.names.intern(b"OUT");
        let s = ctx.strings.allocate_from(b"Sub");
        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::name_lit(title_id)).unwrap();
        ctx.o_stack.push(PsObject::string(s, 3)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(level_id)).unwrap();
        ctx.o_stack.push(PsObject::int(-1)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(out_id)).unwrap();
        op_pdfmark(&mut ctx).unwrap();
        let PdfMarkRecord::Outline(rec) = &ctx.pdfmark_buffer.records()[0] else {
            panic!("expected Outline record");
        };
        assert!(rec.outline_level.is_none());
    }

    #[test]
    fn outline_view_xyz_round_trip() {
        let mut ctx = make_ctx();
        let title_id = ctx.names.intern(b"Title");
        let page_id = ctx.names.intern(b"Page");
        let view_id = ctx.names.intern(b"View");
        let xyz_id = ctx.names.intern(b"XYZ");
        let out_id = ctx.names.intern(b"OUT");
        let s = ctx.strings.allocate_from(b"WithView");
        // Build the View array [/XYZ 100 700 1.5]
        let view_entity = ctx.arrays.allocate(4);
        let elements = vec![
            PsObject::name_lit(xyz_id),
            PsObject::int(100),
            PsObject::int(700),
            PsObject::real(1.5),
        ];
        let dest = ctx.arrays.get_mut(view_entity, 0, 4);
        dest.copy_from_slice(&elements);
        let view_obj = PsObject::array(view_entity, 4);

        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::name_lit(title_id)).unwrap();
        ctx.o_stack.push(PsObject::string(s, 8)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(page_id)).unwrap();
        ctx.o_stack.push(PsObject::int(2)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(view_id)).unwrap();
        ctx.o_stack.push(view_obj).unwrap();
        ctx.o_stack.push(PsObject::name_lit(out_id)).unwrap();
        op_pdfmark(&mut ctx).unwrap();
        let PdfMarkRecord::Outline(rec) = &ctx.pdfmark_buffer.records()[0] else {
            panic!("expected Outline record");
        };
        match &rec.destination {
            Some(OutlineDestination::PageView { page, view }) => {
                assert_eq!(*page, 2);
                match view {
                    ViewSpec::Xyz { left, top, zoom } => {
                        assert_eq!(*left, Some(100.0));
                        assert_eq!(*top, Some(700.0));
                        assert_eq!(*zoom, Some(1.5));
                    }
                    other => panic!("expected XYZ view, got {other:?}"),
                }
            }
            other => panic!("expected PageView, got {other:?}"),
        }
    }

    #[test]
    fn outline_action_uri() {
        let mut ctx = make_ctx();
        let title_id = ctx.names.intern(b"Title");
        let action_id = ctx.names.intern(b"Action");
        let s_id = ctx.names.intern(b"S");
        let uri_name_id = ctx.names.intern(b"URI");
        let out_id = ctx.names.intern(b"OUT");
        let title_s = ctx.strings.allocate_from(b"GoSomewhere");
        let url_s = ctx.strings.allocate_from(b"https://example.org");
        // Action dict: << /S /URI /URI (https://...) >>
        let action_dict = ctx.dicts.allocate(4, b"action");
        ctx.dicts.put(
            action_dict,
            DictKey::Name(s_id),
            PsObject::name_lit(uri_name_id),
        );
        ctx.dicts.put(
            action_dict,
            DictKey::Name(uri_name_id),
            PsObject::string(url_s, 19),
        );
        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::name_lit(title_id)).unwrap();
        ctx.o_stack.push(PsObject::string(title_s, 11)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(action_id)).unwrap();
        ctx.o_stack.push(PsObject::dict(action_dict)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(out_id)).unwrap();
        op_pdfmark(&mut ctx).unwrap();
        let PdfMarkRecord::Outline(rec) = &ctx.pdfmark_buffer.records()[0] else {
            panic!("expected Outline record");
        };
        match &rec.destination {
            Some(OutlineDestination::Action(OutlineAction::Uri(uri))) => {
                assert_eq!(uri, "https://example.org");
            }
            other => panic!("expected URI action, got {other:?}"),
        }
    }

    #[test]
    fn outline_named_dest() {
        let mut ctx = make_ctx();
        let title_id = ctx.names.intern(b"Title");
        let dest_id = ctx.names.intern(b"Dest");
        let target_name_id = ctx.names.intern(b"chapter1");
        let out_id = ctx.names.intern(b"OUT");
        let s = ctx.strings.allocate_from(b"NamedJump");
        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::name_lit(title_id)).unwrap();
        ctx.o_stack.push(PsObject::string(s, 9)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(dest_id)).unwrap();
        ctx.o_stack
            .push(PsObject::name_lit(target_name_id))
            .unwrap();
        ctx.o_stack.push(PsObject::name_lit(out_id)).unwrap();
        op_pdfmark(&mut ctx).unwrap();
        let PdfMarkRecord::Outline(rec) = &ctx.pdfmark_buffer.records()[0] else {
            panic!("expected Outline record");
        };
        match &rec.destination {
            Some(OutlineDestination::NamedDest(name)) => assert_eq!(name, "chapter1"),
            other => panic!("expected NamedDest, got {other:?}"),
        }
    }

    /// Build a Rect array entity for tests.
    fn alloc_rect(ctx: &mut Context, llx: f64, lly: f64, urx: f64, ury: f64) -> PsObject {
        let entity = ctx.arrays.allocate(4);
        let elements = vec![
            PsObject::real(llx),
            PsObject::real(lly),
            PsObject::real(urx),
            PsObject::real(ury),
        ];
        let dest = ctx.arrays.get_mut(entity, 0, 4);
        dest.copy_from_slice(&elements);
        PsObject::array(entity, 4)
    }

    #[test]
    fn annotation_link_to_uri() {
        let mut ctx = make_ctx();
        let title_id = ctx.names.intern(b"Title");
        let rect_id = ctx.names.intern(b"Rect");
        let subtype_id = ctx.names.intern(b"Subtype");
        let link_id = ctx.names.intern(b"Link");
        let action_id = ctx.names.intern(b"Action");
        let s_id = ctx.names.intern(b"S");
        let uri_name_id = ctx.names.intern(b"URI");
        let ann_id = ctx.names.intern(b"ANN");

        let title_str = ctx.strings.allocate_from(b"website");
        let url_str = ctx.strings.allocate_from(b"https://example.com");
        let action_dict = ctx.dicts.allocate(4, b"action");
        ctx.dicts.put(
            action_dict,
            DictKey::Name(s_id),
            PsObject::name_lit(uri_name_id),
        );
        ctx.dicts.put(
            action_dict,
            DictKey::Name(uri_name_id),
            PsObject::string(url_str, 19),
        );

        let rect = alloc_rect(&mut ctx, 72.0, 720.0, 540.0, 750.0);
        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::name_lit(title_id)).unwrap();
        ctx.o_stack.push(PsObject::string(title_str, 7)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(rect_id)).unwrap();
        ctx.o_stack.push(rect).unwrap();
        ctx.o_stack.push(PsObject::name_lit(subtype_id)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(link_id)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(action_id)).unwrap();
        ctx.o_stack.push(PsObject::dict(action_dict)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(ann_id)).unwrap();
        op_pdfmark(&mut ctx).unwrap();

        let PdfMarkRecord::Annotation(rec) = &ctx.pdfmark_buffer.records()[0] else {
            panic!("expected Annotation record");
        };
        assert_eq!(rec.title.as_deref(), Some("website"));
        assert_eq!(rec.rect, [72.0, 720.0, 540.0, 750.0]);
        match &rec.subtype {
            AnnotationSubtype::Link { target, .. } => match target {
                Some(AnnotationTarget::Action(OutlineAction::Uri(uri))) => {
                    assert_eq!(uri, "https://example.com")
                }
                other => panic!("expected URI action target, got {other:?}"),
            },
            other => panic!("expected Link subtype, got {other:?}"),
        }
    }

    #[test]
    fn annotation_text_default_icon_is_note() {
        let mut ctx = make_ctx();
        let rect_id = ctx.names.intern(b"Rect");
        let subtype_id = ctx.names.intern(b"Subtype");
        let text_id = ctx.names.intern(b"Text");
        let contents_id = ctx.names.intern(b"Contents");
        let ann_id = ctx.names.intern(b"ANN");
        let body_str = ctx.strings.allocate_from(b"A comment");

        let rect = alloc_rect(&mut ctx, 100.0, 100.0, 130.0, 130.0);
        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::name_lit(rect_id)).unwrap();
        ctx.o_stack.push(rect).unwrap();
        ctx.o_stack.push(PsObject::name_lit(subtype_id)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(text_id)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(contents_id)).unwrap();
        ctx.o_stack.push(PsObject::string(body_str, 9)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(ann_id)).unwrap();
        op_pdfmark(&mut ctx).unwrap();

        let PdfMarkRecord::Annotation(rec) = &ctx.pdfmark_buffer.records()[0] else {
            panic!("expected Annotation record");
        };
        assert_eq!(rec.contents.as_deref(), Some("A comment"));
        match &rec.subtype {
            AnnotationSubtype::Text { open, icon } => {
                assert!(!*open);
                assert_eq!(*icon, TextAnnotationIcon::Note);
            }
            other => panic!("expected Text subtype, got {other:?}"),
        }
    }

    #[test]
    fn annotation_freetext_default_appearance_filled_in_at_write() {
        let mut ctx = make_ctx();
        let rect_id = ctx.names.intern(b"Rect");
        let subtype_id = ctx.names.intern(b"Subtype");
        let freetext_id = ctx.names.intern(b"FreeText");
        let q_id = ctx.names.intern(b"Q");
        let ann_id = ctx.names.intern(b"ANN");

        let rect = alloc_rect(&mut ctx, 50.0, 50.0, 250.0, 100.0);
        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::name_lit(rect_id)).unwrap();
        ctx.o_stack.push(rect).unwrap();
        ctx.o_stack.push(PsObject::name_lit(subtype_id)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(freetext_id)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(q_id)).unwrap();
        ctx.o_stack.push(PsObject::int(1)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(ann_id)).unwrap();
        op_pdfmark(&mut ctx).unwrap();

        let PdfMarkRecord::Annotation(rec) = &ctx.pdfmark_buffer.records()[0] else {
            panic!("expected Annotation record");
        };
        match &rec.subtype {
            AnnotationSubtype::FreeText {
                default_appearance,
                quadding,
            } => {
                assert!(default_appearance.is_none());
                assert_eq!(*quadding, Some(1));
            }
            other => panic!("expected FreeText subtype, got {other:?}"),
        }
    }

    #[test]
    fn annotation_page_defaults_to_current_page_plus_one() {
        // No /Page → record's page = pdfmark_buffer.current_page + 1.
        // After zero showpages, the page being assembled is page 1.
        let mut ctx = make_ctx();
        let rect_id = ctx.names.intern(b"Rect");
        let subtype_id = ctx.names.intern(b"Subtype");
        let text_id = ctx.names.intern(b"Text");
        let ann_id = ctx.names.intern(b"ANN");
        let rect = alloc_rect(&mut ctx, 0.0, 0.0, 10.0, 10.0);

        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::name_lit(rect_id)).unwrap();
        ctx.o_stack.push(rect).unwrap();
        ctx.o_stack.push(PsObject::name_lit(subtype_id)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(text_id)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(ann_id)).unwrap();
        op_pdfmark(&mut ctx).unwrap();

        let PdfMarkRecord::Annotation(rec) = &ctx.pdfmark_buffer.records()[0] else {
            panic!("expected Annotation record");
        };
        assert_eq!(rec.page, 1);

        // Simulate "showpage finished" → next /ANN scopes to page 2.
        ctx.pdfmark_buffer.current_page = 1;
        let rect2 = alloc_rect(&mut ctx, 0.0, 0.0, 10.0, 10.0);
        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::name_lit(rect_id)).unwrap();
        ctx.o_stack.push(rect2).unwrap();
        ctx.o_stack.push(PsObject::name_lit(subtype_id)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(text_id)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(ann_id)).unwrap();
        op_pdfmark(&mut ctx).unwrap();
        let PdfMarkRecord::Annotation(rec2) = &ctx.pdfmark_buffer.records()[1] else {
            panic!("expected Annotation record");
        };
        assert_eq!(rec2.page, 2);
    }

    #[test]
    fn annotation_unknown_subtype_dropped() {
        let mut ctx = make_ctx();
        let rect_id = ctx.names.intern(b"Rect");
        let subtype_id = ctx.names.intern(b"Subtype");
        let weird_id = ctx.names.intern(b"NotARealSubtype");
        let ann_id = ctx.names.intern(b"ANN");
        let rect = alloc_rect(&mut ctx, 0.0, 0.0, 10.0, 10.0);

        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::name_lit(rect_id)).unwrap();
        ctx.o_stack.push(rect).unwrap();
        ctx.o_stack.push(PsObject::name_lit(subtype_id)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(weird_id)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(ann_id)).unwrap();
        op_pdfmark(&mut ctx).unwrap();
        assert!(ctx.pdfmark_buffer.is_empty());
    }

    #[test]
    fn dest_simple_page() {
        let mut ctx = make_ctx();
        let dest_id = ctx.names.intern(b"Dest");
        let chap_id = ctx.names.intern(b"chapter1");
        let page_id = ctx.names.intern(b"Page");
        let dest_tag_id = ctx.names.intern(b"DEST");
        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::name_lit(dest_id)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(chap_id)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(page_id)).unwrap();
        ctx.o_stack.push(PsObject::int(7)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(dest_tag_id)).unwrap();
        op_pdfmark(&mut ctx).unwrap();
        let PdfMarkRecord::Dest(rec) = &ctx.pdfmark_buffer.records()[0] else {
            panic!("expected Dest record");
        };
        assert_eq!(rec.name, "chapter1");
        assert_eq!(rec.page, 7);
    }

    #[test]
    fn dest_requires_name_and_page() {
        // Missing /Page → drop.
        let mut ctx = make_ctx();
        let dest_id = ctx.names.intern(b"Dest");
        let n = ctx.names.intern(b"x");
        let dest_tag_id = ctx.names.intern(b"DEST");
        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::name_lit(dest_id)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(n)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(dest_tag_id)).unwrap();
        op_pdfmark(&mut ctx).unwrap();
        assert!(ctx.pdfmark_buffer.is_empty());

        // Missing /Dest → drop.
        let page_id = ctx.names.intern(b"Page");
        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::name_lit(page_id)).unwrap();
        ctx.o_stack.push(PsObject::int(1)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(dest_tag_id)).unwrap();
        op_pdfmark(&mut ctx).unwrap();
        assert!(ctx.pdfmark_buffer.is_empty());
    }

    fn alloc_box(ctx: &mut Context, llx: f64, lly: f64, urx: f64, ury: f64) -> PsObject {
        let entity = ctx.arrays.allocate(4);
        let elements = vec![
            PsObject::real(llx),
            PsObject::real(lly),
            PsObject::real(urx),
            PsObject::real(ury),
        ];
        let dest = ctx.arrays.get_mut(entity, 0, 4);
        dest.copy_from_slice(&elements);
        PsObject::array(entity, 4)
    }

    #[test]
    fn page_single_with_cropbox() {
        let mut ctx = make_ctx();
        let crop_id = ctx.names.intern(b"CropBox");
        let page_id = ctx.names.intern(b"Page");
        let page_tag_id = ctx.names.intern(b"PAGE");
        let bx = alloc_box(&mut ctx, 36.0, 36.0, 576.0, 756.0);
        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::name_lit(crop_id)).unwrap();
        ctx.o_stack.push(bx).unwrap();
        ctx.o_stack.push(PsObject::name_lit(page_id)).unwrap();
        ctx.o_stack.push(PsObject::int(2)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(page_tag_id)).unwrap();
        op_pdfmark(&mut ctx).unwrap();
        let PdfMarkRecord::PageOverride(rec) = &ctx.pdfmark_buffer.records()[0] else {
            panic!("expected PageOverride record");
        };
        assert!(matches!(rec.scope, PageOverrideScope::Single(2)));
        assert_eq!(rec.boxes.crop_box, Some([36.0, 36.0, 576.0, 756.0]));
    }

    #[test]
    fn page_implicit_scoping_to_current_page() {
        // /PAGE without /Page targets the page being assembled
        // (current_page + 1). Bumping current_page simulates a
        // completed showpage.
        let mut ctx = make_ctx();
        ctx.pdfmark_buffer.current_page = 3;
        let trim_id = ctx.names.intern(b"TrimBox");
        let page_tag_id = ctx.names.intern(b"PAGE");
        let bx = alloc_box(&mut ctx, 0.0, 0.0, 612.0, 792.0);
        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::name_lit(trim_id)).unwrap();
        ctx.o_stack.push(bx).unwrap();
        ctx.o_stack.push(PsObject::name_lit(page_tag_id)).unwrap();
        op_pdfmark(&mut ctx).unwrap();
        let PdfMarkRecord::PageOverride(rec) = &ctx.pdfmark_buffer.records()[0] else {
            panic!("expected PageOverride record");
        };
        assert!(matches!(rec.scope, PageOverrideScope::Single(4)));
        assert!(rec.boxes.trim_box.is_some());
    }

    #[test]
    fn pages_global_scope() {
        let mut ctx = make_ctx();
        let crop_id = ctx.names.intern(b"CropBox");
        let pages_tag_id = ctx.names.intern(b"PAGES");
        let bx = alloc_box(&mut ctx, 0.0, 0.0, 100.0, 100.0);
        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::name_lit(crop_id)).unwrap();
        ctx.o_stack.push(bx).unwrap();
        ctx.o_stack.push(PsObject::name_lit(pages_tag_id)).unwrap();
        op_pdfmark(&mut ctx).unwrap();
        let PdfMarkRecord::PageOverride(rec) = &ctx.pdfmark_buffer.records()[0] else {
            panic!("expected PageOverride record");
        };
        assert_eq!(rec.scope, PageOverrideScope::All);
        assert_eq!(rec.boxes.crop_box, Some([0.0, 0.0, 100.0, 100.0]));
    }

    #[test]
    fn page_record_with_no_boxes_or_rotate_is_dropped() {
        let mut ctx = make_ctx();
        let page_id = ctx.names.intern(b"Page");
        let page_tag_id = ctx.names.intern(b"PAGE");
        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::name_lit(page_id)).unwrap();
        ctx.o_stack.push(PsObject::int(1)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(page_tag_id)).unwrap();
        op_pdfmark(&mut ctx).unwrap();
        assert!(ctx.pdfmark_buffer.is_empty());
    }

    #[test]
    fn page_rotate_recorded() {
        let mut ctx = make_ctx();
        let rot_id = ctx.names.intern(b"Rotate");
        let page_tag_id = ctx.names.intern(b"PAGE");
        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::name_lit(rot_id)).unwrap();
        ctx.o_stack.push(PsObject::int(90)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(page_tag_id)).unwrap();
        op_pdfmark(&mut ctx).unwrap();
        let PdfMarkRecord::PageOverride(rec) = &ctx.pdfmark_buffer.records()[0] else {
            panic!("expected PageOverride record");
        };
        assert_eq!(rec.rotate, Some(90));
    }

    #[test]
    fn viewer_prefs_full_set() {
        let mut ctx = make_ctx();
        let hide_id = ctx.names.intern(b"HideToolbar");
        let fit_id = ctx.names.intern(b"FitWindow");
        let pm_id = ctx.names.intern(b"PageMode");
        let fs_id = ctx.names.intern(b"FullScreen");
        let pl_id = ctx.names.intern(b"PageLayout");
        let tcl_id = ctx.names.intern(b"TwoColumnLeft");
        let tag_id = ctx.names.intern(b"VIEWERPREFERENCES");
        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::name_lit(hide_id)).unwrap();
        ctx.o_stack.push(PsObject::bool(true)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(fit_id)).unwrap();
        ctx.o_stack.push(PsObject::bool(true)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(pm_id)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(fs_id)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(pl_id)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(tcl_id)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(tag_id)).unwrap();
        op_pdfmark(&mut ctx).unwrap();
        let PdfMarkRecord::ViewerPrefs(rec) = &ctx.pdfmark_buffer.records()[0] else {
            panic!("expected ViewerPrefs record");
        };
        assert_eq!(rec.hide_toolbar, Some(true));
        assert_eq!(rec.fit_window, Some(true));
        assert_eq!(rec.page_mode.as_deref(), Some("FullScreen"));
        assert_eq!(rec.page_layout.as_deref(), Some("TwoColumnLeft"));
    }

    #[test]
    fn viewer_prefs_empty_dropped() {
        let mut ctx = make_ctx();
        let tag_id = ctx.names.intern(b"VIEWERPREFERENCES");
        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::name_lit(tag_id)).unwrap();
        op_pdfmark(&mut ctx).unwrap();
        assert!(ctx.pdfmark_buffer.is_empty());
    }

    #[test]
    fn metadata_string_passthrough() {
        let mut ctx = make_ctx();
        let meta_key_id = ctx.names.intern(b"Metadata");
        let tag_id = ctx.names.intern(b"Metadata");
        let xmp = b"<?xpacket begin='\xef\xbb\xbf'?><x:xmpmeta/>";
        let s = ctx.strings.allocate_from(xmp);
        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::name_lit(meta_key_id)).unwrap();
        ctx.o_stack
            .push(PsObject::string(s, xmp.len() as u32))
            .unwrap();
        ctx.o_stack.push(PsObject::name_lit(tag_id)).unwrap();
        op_pdfmark(&mut ctx).unwrap();
        let PdfMarkRecord::Metadata(rec) = &ctx.pdfmark_buffer.records()[0] else {
            panic!("expected Metadata record");
        };
        assert_eq!(rec.xmp_bytes, xmp);
    }

    #[test]
    fn metadata_missing_value_dropped() {
        let mut ctx = make_ctx();
        let tag_id = ctx.names.intern(b"Metadata");
        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::name_lit(tag_id)).unwrap();
        op_pdfmark(&mut ctx).unwrap();
        assert!(ctx.pdfmark_buffer.is_empty());
    }

    #[test]
    fn docinfo_trapped() {
        let mut ctx = make_ctx();
        let trapped_id = ctx.names.intern(b"Trapped");
        let true_id = ctx.names.intern(b"True");
        let docinfo_id = ctx.names.intern(b"DOCINFO");
        ctx.o_stack.push(PsObject::mark()).unwrap();
        ctx.o_stack.push(PsObject::name_lit(trapped_id)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(true_id)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(docinfo_id)).unwrap();
        op_pdfmark(&mut ctx).unwrap();
        let PdfMarkRecord::DocInfo(rec) = &ctx.pdfmark_buffer.records()[0] else {
            panic!("expected DocInfo record");
        };
        assert_eq!(rec.trapped, Some(TrappedState::True));
    }
}
