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
use stet_core::error::PsError;
use stet_core::object::{PsObject, PsValue};
use stet_core::pdfmark::{DocDate, DocInfoRecord, PdfMarkRecord, TrappedState};

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
    if tag_bytes.as_slice() == b"DOCINFO"
        && let Some(rec) = parse_docinfo(ctx, &payload)
    {
        ctx.pdfmark_buffer.push(PdfMarkRecord::DocInfo(rec));
    }
    // Unknown type-tags silently emit nothing (Adobe convention).
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
        let PdfMarkRecord::DocInfo(rec) = &ctx.pdfmark_buffer.records()[0];
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
        let PdfMarkRecord::DocInfo(rec) = &ctx.pdfmark_buffer.records()[0];
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
        let PdfMarkRecord::DocInfo(rec) = &ctx.pdfmark_buffer.records()[0];
        assert_eq!(rec.title.as_deref(), Some("T"));
        assert_eq!(rec.author.as_deref(), Some("A"));
        assert_eq!(rec.subject.as_deref(), Some("S"));
        assert_eq!(rec.keywords.as_deref(), Some("K"));
        assert_eq!(rec.creator.as_deref(), Some("C"));
        assert_eq!(rec.producer.as_deref(), Some("P"));
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
        let PdfMarkRecord::DocInfo(rec) = &ctx.pdfmark_buffer.records()[0];
        assert_eq!(rec.trapped, Some(TrappedState::True));
    }
}
