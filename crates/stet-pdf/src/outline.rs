// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF outline (`/Outlines`) tree writer.
//!
//! Consumes [`stet_core::pdfmark::OutlineNode`] trees produced from
//! `/OUT pdfmark` records by [`stet_core::pdfmark::build_outline_tree`]
//! and emits a PDF outline tree: one root `/Outlines` dict plus one
//! `/Outline` indirect object per node, linked through `/First`,
//! `/Last`, `/Next`, `/Prev`, `/Parent`, and `/Count`. Returns the root
//! `/Outlines` dict's object number so the caller can wire it into
//! `/Catalog /Outlines`.

use stet_core::pdfmark::{GoToTarget, OutlineAction, OutlineDestination, OutlineNode, ViewSpec};

use crate::pdf_objects::PdfObj;
use crate::pdf_writer::PdfWriter;

/// Emit an `/Outlines` tree from a slice of root nodes. Returns the
/// indirect object number of the root `/Outlines` dictionary, or
/// `None` when `roots` is empty (no outline → don't reference one
/// from `/Catalog`). `page_refs` maps 1-based page numbers to PDF
/// indirect refs of the page objects.
pub fn write_outline_tree(
    writer: &mut PdfWriter,
    roots: &[OutlineNode],
    page_refs: &[u32],
) -> Option<u32> {
    if roots.is_empty() {
        return None;
    }
    let root_ref = writer.alloc_obj();
    let total_visible = total_visible_descendants(roots);
    let child_refs = emit_children(writer, roots, root_ref, page_refs);
    let entries = vec![
        (b"Type".to_vec(), PdfObj::name("Outlines")),
        (b"First".to_vec(), PdfObj::Ref(*child_refs.first()?)),
        (b"Last".to_vec(), PdfObj::Ref(*child_refs.last()?)),
        (b"Count".to_vec(), PdfObj::Int(total_visible as i64)),
    ];
    writer.set_object(root_ref, &PdfObj::Dict(entries));
    Some(root_ref)
}

/// Pre-allocate object numbers for every node in `siblings`, then
/// recursively emit each one with proper sibling and parent links.
fn emit_children(
    writer: &mut PdfWriter,
    siblings: &[OutlineNode],
    parent_ref: u32,
    page_refs: &[u32],
) -> Vec<u32> {
    let refs: Vec<u32> = (0..siblings.len()).map(|_| writer.alloc_obj()).collect();
    for i in 0..siblings.len() {
        let prev = if i > 0 { Some(refs[i - 1]) } else { None };
        let next = if i + 1 < refs.len() {
            Some(refs[i + 1])
        } else {
            None
        };
        emit_node(
            writer,
            &siblings[i],
            refs[i],
            parent_ref,
            prev,
            next,
            page_refs,
        );
    }
    refs
}

/// Emit one /Outline indirect object.
fn emit_node(
    writer: &mut PdfWriter,
    node: &OutlineNode,
    self_ref: u32,
    parent_ref: u32,
    prev_ref: Option<u32>,
    next_ref: Option<u32>,
    page_refs: &[u32],
) {
    let child_refs = if node.children.is_empty() {
        Vec::new()
    } else {
        emit_children(writer, &node.children, self_ref, page_refs)
    };

    let mut entries: Vec<(Vec<u8>, PdfObj)> = vec![
        (
            b"Title".to_vec(),
            PdfObj::LitString(node.record.title.clone().into_bytes()),
        ),
        (b"Parent".to_vec(), PdfObj::Ref(parent_ref)),
    ];
    if let Some(p) = prev_ref {
        entries.push((b"Prev".to_vec(), PdfObj::Ref(p)));
    }
    if let Some(n) = next_ref {
        entries.push((b"Next".to_vec(), PdfObj::Ref(n)));
    }
    if !child_refs.is_empty() {
        entries.push((b"First".to_vec(), PdfObj::Ref(child_refs[0])));
        entries.push((b"Last".to_vec(), PdfObj::Ref(*child_refs.last().unwrap())));
    }

    // /Count: PDF convention is positive when the node is open
    // (children visible by default), negative when closed. The total
    // descendant count is what the spec stores; the sign comes from
    // the producer's intent (Adobe `/OUT` /Count > 0 = open,
    // /Count < 0 = closed). When no /Count was supplied we default to
    // closed (negative) so files don't auto-expand thousands of
    // bookmarks on open.
    let total = total_visible_descendants(&node.children);
    if total > 0 {
        let signed = match node.record.count {
            Some(n) if n > 0 => total as i64,
            Some(_) => -(total as i64),
            None => -(total as i64),
        };
        entries.push((b"Count".to_vec(), PdfObj::Int(signed)));
    }

    if let Some(dest) = &node.record.destination {
        match destination_to_pdf(dest, page_refs) {
            DestinationEmit::Dest(obj) => entries.push((b"Dest".to_vec(), obj)),
            DestinationEmit::Action(obj) => entries.push((b"A".to_vec(), obj)),
            DestinationEmit::None => {}
        }
    }

    if let Some([r, g, b]) = node.record.color {
        entries.push((
            b"C".to_vec(),
            PdfObj::Array(vec![PdfObj::Real(r), PdfObj::Real(g), PdfObj::Real(b)]),
        ));
    }

    if let Some(f) = node.record.flags {
        entries.push((b"F".to_vec(), PdfObj::Int(f as i64)));
    }

    writer.set_object(self_ref, &PdfObj::Dict(entries));
}

/// Total number of descendants that participate in the visible-count
/// tally — every node, regardless of whether it is open or closed.
/// Matches PDF spec: parent /Count holds the sum of all descendants
/// that *would* be visible if the parent were open.
fn total_visible_descendants(nodes: &[OutlineNode]) -> usize {
    let mut total = 0;
    for n in nodes {
        total += 1 + total_visible_descendants(&n.children);
    }
    total
}

/// What [`destination_to_pdf`] hands back — either a `/Dest` array, an
/// `/A` action dict, or nothing.
enum DestinationEmit {
    Dest(PdfObj),
    Action(PdfObj),
    None,
}

fn destination_to_pdf(dest: &OutlineDestination, page_refs: &[u32]) -> DestinationEmit {
    match dest {
        OutlineDestination::PageView { page, view } => match page_to_ref(*page, page_refs) {
            Some(page_ref) => DestinationEmit::Dest(page_view_dest_array(page_ref, view)),
            None => DestinationEmit::None,
        },
        OutlineDestination::NamedDest(name) => {
            DestinationEmit::Dest(PdfObj::LitString(name.clone().into_bytes()))
        }
        OutlineDestination::Action(action) => match encode_action(action, page_refs) {
            Some(dict) => DestinationEmit::Action(dict),
            None => DestinationEmit::None,
        },
        _ => DestinationEmit::None,
    }
}

/// Encode an action dict for either an outline or annotation. Returns
/// `None` when the action's target page is out of range and the
/// caller should drop the action altogether.
pub(crate) fn encode_action(action: &OutlineAction, page_refs: &[u32]) -> Option<PdfObj> {
    Some(PdfObj::Dict(match action {
        OutlineAction::Uri(uri) => vec![
            (b"Type".to_vec(), PdfObj::name("Action")),
            (b"S".to_vec(), PdfObj::name("URI")),
            (b"URI".to_vec(), PdfObj::LitString(uri.clone().into_bytes())),
        ],
        OutlineAction::GoTo(target) => {
            let d = match target {
                GoToTarget::Named(name) => PdfObj::LitString(name.clone().into_bytes()),
                GoToTarget::Explicit { page, view } => match page_to_ref(*page, page_refs) {
                    Some(page_ref) => page_view_dest_array(page_ref, view),
                    None => return None,
                },
                _ => return None,
            };
            vec![
                (b"Type".to_vec(), PdfObj::name("Action")),
                (b"S".to_vec(), PdfObj::name("GoTo")),
                (b"D".to_vec(), d),
            ]
        }
        OutlineAction::JavaScript(js) => vec![
            (b"Type".to_vec(), PdfObj::name("Action")),
            (b"S".to_vec(), PdfObj::name("JavaScript")),
            (b"JS".to_vec(), PdfObj::LitString(js.clone().into_bytes())),
        ],
        OutlineAction::Named(name) => vec![
            (b"Type".to_vec(), PdfObj::name("Action")),
            (b"S".to_vec(), PdfObj::name("Named")),
            (b"N".to_vec(), PdfObj::name(name)),
        ],
        _ => return None,
    }))
}

/// 1-based page index → PDF object ref. Out-of-range pages produce
/// `None`; the caller drops the destination entirely so we don't emit
/// a bookmark that points at nothing.
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
        ViewSpec::Fit => {
            elems.push(PdfObj::name("Fit"));
        }
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
        ViewSpec::FitB => {
            elems.push(PdfObj::name("FitB"));
        }
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
