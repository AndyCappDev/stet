// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF Optional Content (layers).
//!
//! Layers — formally Optional Content Groups (OCGs) in ISO 32000-2
//! §8.11 — let a PDF mark slices of its content for selective
//! visibility: CAD layers, watermarks, multilingual annotations,
//! print-only or screen-only overlays, and so on.
//!
//! This module exposes the read-only metadata side of the OCG model:
//!
//! - One [`Layer`] per OCG, carrying the layer's name, intent, lock
//!   state, default visibility, full `/Usage` sub-dict, and any
//!   `/CreatorInfo` hint.
//!
//! Hierarchy (`/Order`), alternate configurations, runtime visibility
//! overrides, OCMD policies, and `/AS` automatic-state rules land in
//! later phases. Phase 1 is just the per-layer record so consumers can
//! enumerate what a document contains.
//!
//! # Quick reference
//!
//! ```no_run
//! use stet_pdf_reader::PdfDocument;
//!
//! let data = std::fs::read("layered.pdf")?;
//! let doc = PdfDocument::from_bytes(&data)?;
//!
//! for layer in doc.layers() {
//!     println!(
//!         "{} (id={}, locked={}, default_visible={})",
//!         layer.name, layer.ocg_id, layer.locked, layer.default_visible
//!     );
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub mod configuration;
pub mod metadata;
pub mod ocmd;

pub use configuration::{
    AutoStateEvent, AutoStateRule, BaseState, Configuration, LayerTree, LayerTreeNode, ListMode,
};
pub use metadata::{
    CreatorInfo, ExportUsage, LanguageUsage, Layer, LayerIntent, LayerUsage, PageElementSubtype,
    PrintUsage, UsageState, UserUsage, ViewUsage, ZoomUsage,
};

// Re-export the underlying renderer types so consumers don't have to
// reach into `stet-graphics` to construct visibility predicates.
pub use stet_graphics::display_list::{MembershipPolicy, OcgVisibility, VisibilityExpr};
pub use stet_graphics::layer_set::LayerSet;

use crate::PdfDocument;

/// Which audience a render is for.
///
/// Drives [`PdfDocument::layer_set_for`]: the resulting [`LayerSet`]
/// has every `/AS` automatic-state rule whose `/Event` matches this
/// intent applied on top of the default-configuration starting point.
///
/// PDF authors use this to hide print-only watermarks during
/// interactive viewing, or to surface annotations only for export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RenderIntent {
    /// `/View` — interactive on-screen display.
    View,
    /// `/Print` — printing pipeline.
    Print,
    /// `/Export` — conversion or extraction.
    Export,
}

impl RenderIntent {
    fn matches(self, event: AutoStateEvent) -> bool {
        matches!(
            (self, event),
            (RenderIntent::View, AutoStateEvent::View)
                | (RenderIntent::Print, AutoStateEvent::Print)
                | (RenderIntent::Export, AutoStateEvent::Export)
        )
    }
}

/// Build a [`LayerSet`] that reflects the document's default
/// configuration with every `/AS` automatic-state rule for the given
/// [`RenderIntent`] applied.
///
/// Algorithm:
///
/// 1. Start from [`layer_set_from_document`].
/// 2. For each auto-state rule on the default configuration whose
///    `/Event` matches the requested intent:
///     - For each OCG listed in the rule's `/OCGs`:
///         - For each `/Category` in the rule:
///             - If that category's `/Usage` sub-dict on the OCG
///               carries an explicit ON/OFF state, override that
///               OCG's entry in the LayerSet.
///
/// PDF spec doesn't define precedence when multiple rules touch the
/// same OCG; this implementation is **last-wins** in the order rules
/// appear in `/AS`.
///
/// Layers carrying `/Usage` hints with **no** matching `/AS` rule are
/// untouched — by spec their hints are informational only, not
/// auto-applied. Some viewers heuristically apply them anyway; stet
/// does not.
pub fn layer_set_for(doc: &PdfDocument<'_>, intent: RenderIntent) -> LayerSet {
    let mut set = layer_set_from_document(doc);
    let Some(cfg) = doc.default_configuration() else {
        return set;
    };
    for rule in &cfg.auto_state {
        if !intent.matches(rule.event) {
            continue;
        }
        for &ocg_id in &rule.ocgs {
            let Some(layer) = doc.layer(ocg_id) else {
                continue;
            };
            for category in &rule.categories {
                if let Some(state) = usage_state_for_category(&layer.usage, category) {
                    set.set(ocg_id, matches!(state, UsageState::On));
                }
            }
        }
    }
    set
}

/// Look up an OCG's `/Usage` sub-dict for the given `/Category` name
/// and return its ON/OFF state, if it carries one. Categories that
/// don't carry a state (Zoom, Language, User, PageElement,
/// CreatorInfo) return `None` and are silently skipped by
/// [`layer_set_for`].
fn usage_state_for_category(usage: &LayerUsage, category: &str) -> Option<UsageState> {
    match category {
        "View" => usage.view.as_ref().map(|v| v.state),
        "Print" => usage.print.as_ref().map(|p| p.state),
        "Export" => usage.export.as_ref().map(|e| e.state),
        _ => None,
    }
}

/// Build a [`LayerSet`] populated from a [`PdfDocument`]'s default
/// configuration (`/OCProperties /D`).
///
/// Each layer gets an explicit override matching its `default_visible`
/// flag; the resulting set is therefore equivalent to "what the
/// document looks like with no user toggles applied" but lets a UI
/// toggle individual layers from a known starting point.
pub fn layer_set_from_document(doc: &PdfDocument<'_>) -> LayerSet {
    let mut set = LayerSet::new();
    for layer in doc.layers() {
        set.set(layer.ocg_id, layer.default_visible);
    }
    set
}

/// Build a [`LayerSet`] populated from one of the document's
/// alternate configurations (`/OCProperties /Configs`).
///
/// `index = 0` is the default configuration; `1..N` are the entries
/// of `/Configs`. Returns `None` for an out-of-range index.
///
/// `BaseState::On` starts every layer ON before applying the
/// configuration's `/OFF` overrides; `BaseState::Off` starts every
/// layer OFF before applying `/ON`; `BaseState::Unchanged` carries
/// each layer's metadata-level `default_visible` forward.
pub fn layer_set_from_configuration(doc: &PdfDocument<'_>, index: usize) -> Option<LayerSet> {
    let cfg = doc.configuration(index)?;
    let mut set = LayerSet::new();
    let initial = match cfg.base_state {
        BaseState::On => Some(true),
        BaseState::Off => Some(false),
        BaseState::Unchanged => None,
    };
    for layer in doc.layers() {
        let v = match initial {
            Some(b) => b,
            None => layer.default_visible,
        };
        set.set(layer.ocg_id, v);
    }
    for &id in &cfg.on {
        set.set(id, true);
    }
    for &id in &cfg.off {
        set.set(id, false);
    }
    Some(set)
}
