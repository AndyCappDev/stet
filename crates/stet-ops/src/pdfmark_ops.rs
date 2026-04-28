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
//! Phase 1 implements the dispatcher itself plus the `/DOCINFO`
//! type-tag. Subsequent phases add `/OUT`, `/ANN`, `/DEST`, `/PAGE` /
//! `/PAGES`, `/VIEWERPREFERENCES`, `/Metadata`, etc., as new arms in
//! the type-tag match.

use stet_core::context::Context;
use stet_core::dict::DictKey;
use stet_core::error::PsError;
use stet_core::object::{EntityId, PsObject, PsValue};
use stet_core::pdfmark::{
    DocDate, DocInfoRecord, GoToTarget, OutlineAction, OutlineDestination, OutlineRecord,
    PdfMarkRecord, TrappedState, ViewSpec,
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
