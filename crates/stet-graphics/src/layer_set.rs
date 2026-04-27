// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Per-render OCG visibility overrides.
//!
//! [`LayerSet`] is the renderer-side contract for "which OCGs are
//! currently on". The display list carries every OcgGroup's
//! per-variant `default_visible` fallback baked from the document's
//! default configuration, but a consumer building a layer panel needs
//! to flip a layer on or off without re-parsing the PDF or rebuilding
//! its display list.
//!
//! The flow:
//!
//! 1. The PDF reader (or any other producer) builds a display list
//!    whose [`crate::display_list::DisplayElement::OcgGroup`] elements
//!    each carry an [`crate::display_list::OcgVisibility`] predicate.
//! 2. The consumer constructs a `LayerSet` — empty (every OCG falls
//!    back to its `default_visible`) or populated from a particular
//!    PDF configuration.
//! 3. Mutate it as needed, e.g. via [`LayerSet::set`].
//! 4. Pass it into the renderer; rendering calls
//!    [`LayerSet::evaluate`] for each OcgGroup.
//!
//! Higher-level constructors that build a `LayerSet` from a parsed
//! document live in `stet-pdf-reader`'s `layers` module.

use std::collections::HashMap;

use crate::display_list::{MembershipPolicy, OcgVisibility, VisibilityExpr};

/// Per-render override of OCG visibility.
///
/// An OCG with no entry here falls back to its display-list-baked
/// `default_visible`. Entries are explicit `bool`s so a consumer can
/// override a layer in either direction.
#[derive(Clone, Debug, Default)]
pub struct LayerSet {
    states: HashMap<u32, bool>,
}

impl LayerSet {
    /// Construct an empty set — every OCG falls back to its
    /// `default_visible`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the visibility of an OCG.
    pub fn set(&mut self, ocg_id: u32, visible: bool) {
        self.states.insert(ocg_id, visible);
    }

    /// Get the explicit override for an OCG, if any. `None` means the
    /// renderer should fall back to `default_visible`.
    pub fn get(&self, ocg_id: u32) -> Option<bool> {
        self.states.get(&ocg_id).copied()
    }

    /// Drop an explicit override, restoring fallback behaviour.
    pub fn clear(&mut self, ocg_id: u32) {
        self.states.remove(&ocg_id);
    }

    /// Number of explicit overrides.
    pub fn len(&self) -> usize {
        self.states.len()
    }

    /// True when no explicit overrides exist.
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    /// Resolve a single OCG's visibility — explicit override if
    /// present, otherwise the supplied fallback.
    fn resolve(&self, ocg_id: u32, fallback: bool) -> bool {
        self.states.get(&ocg_id).copied().unwrap_or(fallback)
    }

    /// Evaluate an [`OcgVisibility`] predicate.
    ///
    /// `default_visible` on each variant supplies the per-OCG fallback
    /// when this `LayerSet` has no opinion. For the `Membership` and
    /// `Expression` variants, every leaf OCG's visibility is resolved
    /// the same way (explicit override → fallback `default_visible`).
    pub fn evaluate(&self, vis: &OcgVisibility) -> bool {
        match vis {
            OcgVisibility::Single {
                ocg_id,
                default_visible,
            } => self.resolve(*ocg_id, *default_visible),
            OcgVisibility::Membership {
                ocg_ids,
                policy,
                default_visible,
            } => {
                if ocg_ids.is_empty() {
                    // PDF spec: an OCMD with no /OCGs is always visible.
                    return true;
                }
                let on_count = ocg_ids
                    .iter()
                    .filter(|id| self.resolve(**id, *default_visible))
                    .count();
                let total = ocg_ids.len();
                match policy {
                    MembershipPolicy::AllOn => on_count == total,
                    MembershipPolicy::AnyOn => on_count > 0,
                    MembershipPolicy::AllOff => on_count == 0,
                    MembershipPolicy::AnyOff => on_count < total,
                }
            }
            OcgVisibility::Expression {
                expr,
                default_visible,
            } => self.evaluate_expr(expr, *default_visible),
        }
    }

    /// Recursively evaluate a `/VE` expression.
    fn evaluate_expr(&self, expr: &VisibilityExpr, default_visible: bool) -> bool {
        match expr {
            VisibilityExpr::Layer(id) => self.resolve(*id, default_visible),
            VisibilityExpr::Not(inner) => !self.evaluate_expr(inner, default_visible),
            VisibilityExpr::And(operands) => operands
                .iter()
                .all(|o| self.evaluate_expr(o, default_visible)),
            VisibilityExpr::Or(operands) => operands
                .iter()
                .any(|o| self.evaluate_expr(o, default_visible)),
        }
    }

    /// Apply a radio-button-group constraint: when one layer in the
    /// group is turned ON, all the others get explicitly turned OFF.
    /// `newly_on` is left ON.
    ///
    /// Layers in `group` other than `newly_on` are explicitly forced
    /// OFF (they get an entry in this set, not just a missing entry).
    pub fn enforce_rb_group(&mut self, group: &[u32], newly_on: u32) {
        for &id in group {
            if id == newly_on {
                self.states.insert(id, true);
            } else {
                self.states.insert(id, false);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn single(id: u32, default_visible: bool) -> OcgVisibility {
        OcgVisibility::Single {
            ocg_id: id,
            default_visible,
        }
    }

    #[test]
    fn empty_layer_set_uses_defaults() {
        let s = LayerSet::new();
        assert!(s.evaluate(&single(1, true)));
        assert!(!s.evaluate(&single(2, false)));
    }

    #[test]
    fn override_flips_visibility() {
        let mut s = LayerSet::new();
        s.set(1, false);
        assert!(!s.evaluate(&single(1, true)));
        s.set(1, true);
        assert!(s.evaluate(&single(1, true)));
        s.clear(1);
        assert!(s.evaluate(&single(1, true)));
        assert!(s.is_empty());
    }

    #[test]
    fn membership_any_on_default() {
        let s = LayerSet::new();
        let vis = OcgVisibility::Membership {
            ocg_ids: vec![1, 2],
            policy: MembershipPolicy::AnyOn,
            default_visible: false,
        };
        assert!(!s.evaluate(&vis), "both default off");

        let mut s2 = s;
        s2.set(2, true);
        assert!(s2.evaluate(&vis));
    }

    #[test]
    fn membership_all_on() {
        let mut s = LayerSet::new();
        let vis = OcgVisibility::Membership {
            ocg_ids: vec![1, 2],
            policy: MembershipPolicy::AllOn,
            default_visible: true,
        };
        assert!(s.evaluate(&vis));
        s.set(1, false);
        assert!(!s.evaluate(&vis));
    }

    #[test]
    fn membership_all_off_and_any_off() {
        let all_off_default_off = OcgVisibility::Membership {
            ocg_ids: vec![1, 2],
            policy: MembershipPolicy::AllOff,
            default_visible: false,
        };
        let any_off_default_on = OcgVisibility::Membership {
            ocg_ids: vec![1, 2],
            policy: MembershipPolicy::AnyOff,
            default_visible: true,
        };

        let s = LayerSet::new();
        // Empty set, default OFF: both leaves resolve OFF → all_off true.
        assert!(s.evaluate(&all_off_default_off));
        // Empty set, default ON: both leaves resolve ON → no any_off.
        assert!(!s.evaluate(&any_off_default_on));

        // Flip one leaf in the set with default OFF — no longer all_off.
        let mut s = LayerSet::new();
        s.set(1, true);
        assert!(!s.evaluate(&all_off_default_off));

        // Flip one leaf in the set with default ON — now one is off so any_off.
        let mut s = LayerSet::new();
        s.set(1, false);
        assert!(s.evaluate(&any_off_default_on));
    }

    #[test]
    fn membership_empty_ocgs_always_visible() {
        let s = LayerSet::new();
        let vis = OcgVisibility::Membership {
            ocg_ids: vec![],
            policy: MembershipPolicy::AllOff,
            default_visible: false,
        };
        assert!(s.evaluate(&vis));
    }

    #[test]
    fn expression_truth_table() {
        // /VE /And [/Layer 1] [/Or [/Layer 2] [/Not [/Layer 3]]]
        let expr = VisibilityExpr::And(vec![
            VisibilityExpr::Layer(1),
            VisibilityExpr::Or(vec![
                VisibilityExpr::Layer(2),
                VisibilityExpr::Not(Box::new(VisibilityExpr::Layer(3))),
            ]),
        ]);
        let vis = OcgVisibility::Expression {
            expr,
            default_visible: false,
        };

        for a in [false, true] {
            for b in [false, true] {
                for c in [false, true] {
                    let mut s = LayerSet::new();
                    s.set(1, a);
                    s.set(2, b);
                    s.set(3, c);
                    let expected = a && (b || !c);
                    assert_eq!(
                        s.evaluate(&vis),
                        expected,
                        "a={a} b={b} c={c} -> expected {expected}"
                    );
                }
            }
        }
    }

    #[test]
    fn rb_group_enforcement() {
        let mut s = LayerSet::new();
        s.enforce_rb_group(&[10, 20, 30], 20);
        assert_eq!(s.get(10), Some(false));
        assert_eq!(s.get(20), Some(true));
        assert_eq!(s.get(30), Some(false));
    }
}
