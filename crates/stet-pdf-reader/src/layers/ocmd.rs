// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Optional Content Membership Dictionary (OCMD) parsing.
//!
//! Defined in ISO 32000-2 §8.11.2.2. An OCMD wraps a set of OCGs in a
//! visibility predicate that's more expressive than a single OCG ref:
//!
//! - `/P` membership policy over `/OCGs` — `AllOn` / `AnyOn` /
//!   `AllOff` / `AnyOff`. Default policy is `AnyOn`.
//! - `/VE` visibility expression — a nested array using `/And` /
//!   `/Or` / `/Not` operators over OCG refs. Available since
//!   PDF 1.6.
//!
//! `/VE` takes precedence over `/P` when both are present.
//!
//! This module produces [`OcgVisibility`] values for the display
//! list. The matching evaluator lives in
//! `stet_graphics::layer_set::LayerSet`.

use stet_graphics::display_list::{MembershipPolicy, OcgVisibility, VisibilityExpr};

use crate::diagnostics::{ParsePhase, Severity, WarningSink};
use crate::objects::{PdfDict, PdfObj};
use crate::resolver::Resolver;

/// Maximum nesting depth for `/VE` expressions.
///
/// Real-world OCMDs nest 1–3 levels deep; this cap stops pathological
/// or maliciously cyclic expressions from blowing the stack.
const MAX_VE_DEPTH: u32 = 64;

/// Build an [`OcgVisibility`] for an OCMD dict.
///
/// The caller has already determined that the dict's `/Type` is
/// `/OCMD`. `default_visible` is the variant-level fallback returned
/// by [`stet_graphics::layer_set::LayerSet::evaluate`] when no leaf
/// has an explicit override; pass the result of statically evaluating
/// the OCMD against the document's default-config OCG state.
pub fn build_ocmd_visibility(
    resolver: &Resolver,
    ocmd: &PdfDict,
    default_visible: bool,
    sink: &WarningSink,
) -> OcgVisibility {
    if let Some(ve_obj) = ocmd.get(b"VE") {
        let resolved = resolver.deref(ve_obj).ok();
        let view = resolved.as_ref().unwrap_or(ve_obj);
        if let Some(arr) = view.as_array() {
            if let Some(expr) = parse_visibility_expression(resolver, arr, 0, sink) {
                return OcgVisibility::Expression {
                    expr,
                    default_visible,
                };
            }
            // Unparseable /VE — fall through to /P / /AnyOn membership.
        } else {
            sink.record(
                ParsePhase::Layers,
                None,
                Severity::Warning,
                "/VE expression on OCMD is not an array; ignored",
            );
        }
    }

    let mut ocg_ids = Vec::new();
    if let Some(ocgs_obj) = ocmd.get(b"OCGs") {
        match ocgs_obj {
            PdfObj::Ref(num, _) => ocg_ids.push(*num),
            PdfObj::Array(arr) => {
                for item in arr {
                    if let Some((num, _)) = item.as_ref() {
                        ocg_ids.push(num);
                    }
                }
            }
            _ => {}
        }
    }

    let policy = match ocmd.get_name(b"P") {
        Some(b"AllOn") => MembershipPolicy::AllOn,
        Some(b"AllOff") => MembershipPolicy::AllOff,
        Some(b"AnyOff") => MembershipPolicy::AnyOff,
        _ => MembershipPolicy::AnyOn,
    };

    OcgVisibility::Membership {
        ocg_ids,
        policy,
        default_visible,
    }
}

/// Parse an OCMD `/VE` array into a [`VisibilityExpr`].
///
/// The grammar from ISO 32000-2 §8.11.2.2:
///
/// ```text
/// expr     := array-form | ocg-ref
/// array-form := [ /And operand+ ]
///             | [ /Or  operand+ ]
///             | [ /Not operand   ]   (exactly one operand)
/// operand  := expr
/// ocg-ref  := indirect ref to an OCG dict
/// ```
///
/// Returns `None` and emits a warning when the array is malformed
/// (unknown leading name, wrong arity on `/Not`, missing operands,
/// nested non-array non-ref leaves).
pub fn parse_visibility_expression(
    resolver: &Resolver,
    arr: &[PdfObj],
    depth: u32,
    sink: &WarningSink,
) -> Option<VisibilityExpr> {
    if depth > MAX_VE_DEPTH {
        sink.record(
            ParsePhase::Layers,
            None,
            Severity::Error,
            format!("/VE depth limit {MAX_VE_DEPTH} reached; expression truncated"),
        );
        return None;
    }

    // Empty array — never valid.
    let head = arr.first()?;
    let head_name = head.as_name()?;

    let parse_operand = |obj: &PdfObj| -> Option<VisibilityExpr> {
        // Operands are either arrays (sub-expressions) or OCG refs.
        if let Some((ocg_id, _)) = obj.as_ref() {
            // Resolve once to confirm it points at an OCG, but emit
            // even if it doesn't (keeps the structural shape).
            return Some(VisibilityExpr::Layer(ocg_id));
        }
        let resolved = resolver.deref(obj).ok();
        let view = resolved.as_ref().unwrap_or(obj);
        if let Some(sub_arr) = view.as_array() {
            return parse_visibility_expression(resolver, sub_arr, depth + 1, sink);
        }
        sink.record(
            ParsePhase::Layers,
            None,
            Severity::Warning,
            "/VE operand is neither an OCG ref nor a nested array; dropped",
        );
        None
    };

    match head_name {
        b"And" | b"Or" => {
            let mut operands = Vec::new();
            for item in &arr[1..] {
                if let Some(o) = parse_operand(item) {
                    operands.push(o);
                }
            }
            if operands.is_empty() {
                sink.record(
                    ParsePhase::Layers,
                    None,
                    Severity::Warning,
                    format!(
                        "/VE /{} has no usable operands; dropped",
                        std::str::from_utf8(head_name).unwrap_or("?")
                    ),
                );
                return None;
            }
            if head_name == b"And" {
                Some(VisibilityExpr::And(operands))
            } else {
                Some(VisibilityExpr::Or(operands))
            }
        }
        b"Not" => {
            if arr.len() != 2 {
                sink.record(
                    ParsePhase::Layers,
                    None,
                    Severity::Warning,
                    format!("/VE /Not expects 1 operand, got {}; dropped", arr.len() - 1),
                );
                return None;
            }
            let inner = parse_operand(&arr[1])?;
            Some(VisibilityExpr::Not(Box::new(inner)))
        }
        other => {
            sink.record(
                ParsePhase::Layers,
                None,
                Severity::Warning,
                format!(
                    "/VE has unknown leading operator /{}; dropped",
                    String::from_utf8_lossy(other)
                ),
            );
            None
        }
    }
}
