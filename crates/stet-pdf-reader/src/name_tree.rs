// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF name tree traversal.
//!
//! Per ISO 32000-2 §7.9.6, several catalog entries (named destinations,
//! embedded files, JavaScript, AP appearance streams, etc.) live in
//! *name trees*: B-tree-like structures keyed by string. Each node is
//! either a leaf (has `/Names` — an array of alternating
//! `(string, value)` pairs in sorted order) or a branch (has `/Kids`
//! — references to child nodes). The root may have either or both.
//!
//! [`walk_name_tree`] is generic over the value type: callers supply
//! a parser that converts a `PdfObj` value into their typed
//! representation, and the walker traverses the tree producing a
//! `HashMap<String, T>` of every leaf entry.

use std::collections::{HashMap, HashSet};

use crate::objects::PdfObj;
use crate::resolver::Resolver;

/// Maximum total leaf entries to collect.
///
/// Real-world name trees rarely exceed a few thousand entries; this
/// cap stops pathological documents from running away.
const MAX_NAME_TREE_ENTRIES: usize = 1_000_000;

/// Maximum branch-node depth to follow.
///
/// Spec-conformant name trees are typically shallow (depth 1–3 even
/// for tens of thousands of entries). 64 is a generous upper bound.
const MAX_NAME_TREE_DEPTH: u32 = 64;

/// Walk a PDF name tree starting at `root`, producing a map of all
/// leaf entries.
///
/// `parse_value` is invoked for every leaf entry; if it returns
/// `None`, that entry is skipped (the rest of the tree continues to
/// be traversed normally). Cycles are detected via an object-number
/// visited set; depth is capped.
///
/// Returns an empty `HashMap` for empty/missing/invalid trees.
pub fn walk_name_tree<T, F>(
    resolver: &Resolver,
    root: &PdfObj,
    mut parse_value: F,
) -> HashMap<String, T>
where
    F: FnMut(&Resolver, &PdfObj) -> Option<T>,
{
    let mut out = HashMap::new();
    let mut visited = HashSet::new();
    walk_node(resolver, root, &mut visited, &mut out, &mut parse_value, 0);
    out
}

fn walk_node<T, F>(
    resolver: &Resolver,
    node_obj: &PdfObj,
    visited: &mut HashSet<u32>,
    out: &mut HashMap<String, T>,
    parse_value: &mut F,
    depth: u32,
) where
    F: FnMut(&Resolver, &PdfObj) -> Option<T>,
{
    if depth >= MAX_NAME_TREE_DEPTH {
        return;
    }
    if out.len() >= MAX_NAME_TREE_ENTRIES {
        return;
    }
    // If this is an indirect ref, dereference and record visit.
    if let Some((num, _gen)) = node_obj.as_ref()
        && !visited.insert(num)
    {
        return;
    }
    let Ok(node) = resolver.deref(node_obj) else {
        return;
    };
    let Some(dict) = node.as_dict() else {
        return;
    };

    // Leaf: /Names is an array of alternating (string, value) pairs.
    if let Some(names) = dict.get_array(b"Names") {
        let mut i = 0;
        while i + 1 < names.len() {
            if out.len() >= MAX_NAME_TREE_ENTRIES {
                return;
            }
            let key_obj = &names[i];
            let val_obj = &names[i + 1];
            if let Some(key_bytes) = resolver
                .deref(key_obj)
                .ok()
                .as_ref()
                .and_then(|o| o.as_str().map(<[u8]>::to_vec))
            {
                let key = String::from_utf8_lossy(&key_bytes).into_owned();
                if let Some(value) = parse_value(resolver, val_obj) {
                    out.insert(key, value);
                }
            }
            i += 2;
        }
    }

    // Branch: /Kids is an array of refs to child nodes. A node may
    // have both /Names (own leaves) and /Kids (further descent) — rare
    // but legal — so this is independent of the leaf check.
    if let Some(kids) = dict.get_array(b"Kids") {
        for kid in kids {
            if out.len() >= MAX_NAME_TREE_ENTRIES {
                return;
            }
            walk_node(resolver, kid, visited, out, parse_value, depth + 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::objects::{PdfDict, PdfObj};
    use crate::xref::XrefTable;

    /// Build a tiny in-memory resolver harness so we can run leaf-only
    /// trees without a real PDF. /Kids tests need indirect-ref support
    /// which requires xref entries; we cover those via lib.rs's
    /// synthetic-PDF tests.
    fn empty_resolver() -> (Vec<u8>, XrefTable) {
        // Minimal valid PDF stub so XrefTable construction is well-formed.
        let data = b"%PDF-1.4\nxref\n0 1\n0000000000 65535 f \ntrailer\n<< /Size 1 >>\nstartxref\n9\n%%EOF\n".to_vec();
        let xref = crate::xref::parse_xref(&data).unwrap();
        (data, xref)
    }

    #[test]
    fn flat_leaf_tree() {
        let (data, xref) = empty_resolver();
        let resolver = Resolver::with_encryption(&data, xref, None);

        // Build a leaf node: /Names [(A) 1 (B) 2 (C) 3]
        let mut leaf = PdfDict::new();
        leaf.insert(
            b"Names".to_vec(),
            PdfObj::Array(vec![
                PdfObj::Str(b"A".to_vec()),
                PdfObj::Int(1),
                PdfObj::Str(b"B".to_vec()),
                PdfObj::Int(2),
                PdfObj::Str(b"C".to_vec()),
                PdfObj::Int(3),
            ]),
        );
        let root = PdfObj::Dict(leaf);

        let map = walk_name_tree(&resolver, &root, |_r, v| v.as_int());

        assert_eq!(map.len(), 3);
        assert_eq!(map.get("A"), Some(&1));
        assert_eq!(map.get("B"), Some(&2));
        assert_eq!(map.get("C"), Some(&3));
    }

    #[test]
    fn parse_value_can_skip() {
        let (data, xref) = empty_resolver();
        let resolver = Resolver::with_encryption(&data, xref, None);

        let mut leaf = PdfDict::new();
        leaf.insert(
            b"Names".to_vec(),
            PdfObj::Array(vec![
                PdfObj::Str(b"keep".to_vec()),
                PdfObj::Int(42),
                PdfObj::Str(b"skip".to_vec()),
                PdfObj::Null,
            ]),
        );
        let root = PdfObj::Dict(leaf);

        let map = walk_name_tree(&resolver, &root, |_r, v| v.as_int());

        assert_eq!(map.len(), 1);
        assert_eq!(map.get("keep"), Some(&42));
        assert!(!map.contains_key("skip"));
    }

    #[test]
    fn empty_tree_yields_empty_map() {
        let (data, xref) = empty_resolver();
        let resolver = Resolver::with_encryption(&data, xref, None);

        // An empty dict — no /Names, no /Kids.
        let root = PdfObj::Dict(PdfDict::new());
        let map: HashMap<String, i64> = walk_name_tree(&resolver, &root, |_r, v| v.as_int());
        assert!(map.is_empty());
    }

    #[test]
    fn malformed_names_array_odd_length() {
        let (data, xref) = empty_resolver();
        let resolver = Resolver::with_encryption(&data, xref, None);

        // /Names with odd count — the trailing unmatched key is dropped.
        let mut leaf = PdfDict::new();
        leaf.insert(
            b"Names".to_vec(),
            PdfObj::Array(vec![
                PdfObj::Str(b"A".to_vec()),
                PdfObj::Int(1),
                PdfObj::Str(b"orphan".to_vec()),
            ]),
        );
        let root = PdfObj::Dict(leaf);

        let map = walk_name_tree(&resolver, &root, |_r, v| v.as_int());
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("A"), Some(&1));
    }
}
