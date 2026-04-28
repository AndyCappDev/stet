// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Optional Content Group hierarchy and configurations.
//!
//! Defined in ISO 32000-2 §8.11.4. Each PDF carries one default
//! configuration (`/OCProperties /D`) and an optional array of
//! alternate configurations (`/OCProperties /Configs`); a configuration
//! says which layers are initially on/off, how they're presented in a
//! UI tree (`/Order`), how their visibility is grouped (`/RBGroups`),
//! and which layers cannot be toggled by the user (`/Locked`).
//!
//! This module turns each configuration dict into a typed
//! [`Configuration`] and exposes the hierarchy as a [`LayerTree`].

use crate::diagnostics::{ParsePhase, Severity, WarningSink};
use crate::metadata::pdf_string_to_rust_pub;
use crate::objects::{PdfDict, PdfObj};
use crate::resolver::Resolver;

use super::metadata::LayerIntent;

/// A presentation hierarchy for layers, parsed from `/Order`.
///
/// Empty when the configuration has no `/Order` entry — UI consumers
/// should fall back to a flat list of [`super::Layer`] in document
/// order.
#[derive(Debug, Clone, Default)]
pub struct LayerTree {
    /// Top-level nodes, in display order.
    pub nodes: Vec<LayerTreeNode>,
}

/// One node in the layer tree.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum LayerTreeNode {
    /// A leaf referencing a single layer by OCG object number.
    Layer(u32),
    /// A labelled section. The label and header layer are both
    /// optional:
    ///
    /// - `label = Some(_), header_layer = None` — anonymous section
    ///   with a string-literal heading (e.g. `(Backgrounds)` followed
    ///   by a child array).
    /// - `header_layer = Some(_), label = None` — section whose
    ///   heading is the layer immediately preceding a child array.
    /// - both `None` — anonymous section (a bare nested array).
    Section {
        label: Option<String>,
        header_layer: Option<u32>,
        children: Vec<LayerTreeNode>,
    },
}

/// One configuration of layer state and presentation.
///
/// `index = 0` is the default configuration (`/OCProperties /D`);
/// indices 1..N correspond to entries 0..N-1 in `/OCProperties /Configs`.
#[derive(Debug, Clone)]
pub struct Configuration {
    /// 0 = default `/D`; 1..N = `/Configs[i-1]`.
    pub index: usize,
    /// `/Name` — display label for this configuration.
    pub name: Option<String>,
    /// `/Creator` — application that authored this configuration.
    pub creator: Option<String>,
    /// `/BaseState` — starting visibility for every layer before
    /// `/ON` and `/OFF` overrides apply.
    pub base_state: BaseState,
    /// `/ON` — layers explicitly turned on.
    pub on: Vec<u32>,
    /// `/OFF` — layers explicitly turned off.
    pub off: Vec<u32>,
    /// `/Intent` — author's hint about the audiences this
    /// configuration is meant for.
    pub intent: LayerIntent,
    /// `/AS` — automatic-state rules that re-apply `/Usage` hints
    /// under render intents.
    pub auto_state: Vec<AutoStateRule>,
    /// `/Order` — display hierarchy.
    pub order: LayerTree,
    /// `/ListMode` — whether the layer panel should show all pages or
    /// only the visible page's layers.
    pub list_mode: ListMode,
    /// `/RBGroups` — radio-button groups: turning one layer on in a
    /// group implies turning the others in that group off.
    pub rb_groups: Vec<Vec<u32>>,
    /// `/Locked` — layers the user is not allowed to toggle from a
    /// layer panel.
    pub locked: Vec<u32>,
}

/// `/BaseState` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BaseState {
    /// `/ON` — all layers visible until `/OFF` flips them.
    On,
    /// `/OFF` — all layers hidden until `/ON` flips them.
    Off,
    /// `/Unchanged` — preserve the current visibility from a previous
    /// configuration. Only meaningful for alternate configurations.
    Unchanged,
}

impl BaseState {
    fn from_name(name: &[u8]) -> Option<Self> {
        match name {
            b"ON" => Some(BaseState::On),
            b"OFF" => Some(BaseState::Off),
            b"Unchanged" => Some(BaseState::Unchanged),
            _ => None,
        }
    }
}

/// `/ListMode` — layer-panel visibility scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ListMode {
    /// `/AllPages` — show every layer regardless of which pages use
    /// it. Default.
    #[default]
    AllPages,
    /// `/VisiblePages` — show only layers that appear on the
    /// currently displayed page.
    VisiblePages,
}

impl ListMode {
    fn from_name(name: &[u8]) -> Option<Self> {
        match name {
            b"AllPages" => Some(ListMode::AllPages),
            b"VisiblePages" => Some(ListMode::VisiblePages),
            _ => None,
        }
    }
}

/// One `/AS` automatic-state rule.
#[derive(Debug, Clone)]
pub struct AutoStateRule {
    /// `/Event` — render intent the rule applies to.
    pub event: AutoStateEvent,
    /// `/Category` — `/Usage` sub-dict names this rule consults.
    pub categories: Vec<String>,
    /// `/OCGs` — layers this rule applies to.
    pub ocgs: Vec<u32>,
}

/// `/Event` value on an `/AS` rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AutoStateEvent {
    View,
    Print,
    Export,
}

impl AutoStateEvent {
    fn from_name(name: &[u8]) -> Option<Self> {
        match name {
            b"View" => Some(AutoStateEvent::View),
            b"Print" => Some(AutoStateEvent::Print),
            b"Export" => Some(AutoStateEvent::Export),
            _ => None,
        }
    }
}

/// Walk `/OCProperties` and produce one [`Configuration`] for the
/// default `/D` plus one for each entry in `/Configs`.
///
/// Returns an empty `Vec` when the document has no `/OCProperties`.
/// The default configuration is always at index 0; alternate configs
/// follow at indices 1..N preserving the order in `/Configs`.
pub fn parse_configurations(resolver: &Resolver, sink: &WarningSink) -> Vec<Configuration> {
    let Some(catalog) = catalog_dict(resolver) else {
        return Vec::new();
    };
    let Some(oc_props_obj) = catalog.get(b"OCProperties") else {
        return Vec::new();
    };
    let Ok(oc_props) = resolver.deref(oc_props_obj) else {
        return Vec::new();
    };
    let Some(oc_dict) = oc_props.as_dict() else {
        return Vec::new();
    };

    let mut configs = Vec::new();

    // /D — default configuration. Required when /OCProperties exists.
    if let Some(d_obj) = oc_dict.get(b"D")
        && let Ok(d_resolved) = resolver.deref(d_obj)
        && let Some(d_dict) = d_resolved.as_dict()
    {
        configs.push(parse_configuration(resolver, d_dict, 0, sink));
    } else {
        sink.record(
            ParsePhase::Layers,
            None,
            Severity::Warning,
            "/OCProperties missing required /D configuration; skipped",
        );
    }

    // /Configs — alternate configurations.
    if let Some(configs_obj) = oc_dict.get(b"Configs")
        && let Ok(configs_resolved) = resolver.deref(configs_obj)
        && let Some(arr) = configs_resolved.as_array()
    {
        for (i, entry) in arr.iter().enumerate() {
            let Ok(resolved) = resolver.deref(entry) else {
                sink.record(
                    ParsePhase::Layers,
                    None,
                    Severity::Warning,
                    format!("/Configs[{i}] could not be resolved; skipped"),
                );
                continue;
            };
            let Some(dict) = resolved.as_dict() else {
                sink.record(
                    ParsePhase::Layers,
                    None,
                    Severity::Warning,
                    format!("/Configs[{i}] is not a dict; skipped"),
                );
                continue;
            };
            configs.push(parse_configuration(resolver, dict, i + 1, sink));
        }
    }

    configs
}

/// Parse a single configuration dict (the `/D` dict or one
/// `/Configs[i]`).
pub fn parse_configuration(
    resolver: &Resolver,
    dict: &PdfDict,
    index: usize,
    sink: &WarningSink,
) -> Configuration {
    let name = dict.get(b"Name").and_then(pdf_string_to_rust_pub);
    let creator = dict.get(b"Creator").and_then(pdf_string_to_rust_pub);

    let base_state = dict
        .get_name(b"BaseState")
        .and_then(BaseState::from_name)
        .unwrap_or(BaseState::On);

    let on = collect_ocg_refs(resolver, dict, b"ON");
    let off = collect_ocg_refs(resolver, dict, b"OFF");
    let locked = collect_ocg_refs(resolver, dict, b"Locked");

    let intent = dict
        .get(b"Intent")
        .map(|obj| LayerIntent::from_obj(resolver, obj))
        .unwrap_or(LayerIntent::View);

    let order = dict
        .get(b"Order")
        .and_then(|obj| resolver.deref(obj).ok())
        .map(|resolved| parse_order(resolver, &resolved, sink))
        .unwrap_or_default();

    let list_mode = dict
        .get_name(b"ListMode")
        .and_then(ListMode::from_name)
        .unwrap_or_default();

    let auto_state = dict
        .get(b"AS")
        .and_then(|obj| resolver.deref(obj).ok())
        .map(|resolved| parse_auto_state(resolver, &resolved, sink))
        .unwrap_or_default();

    let rb_groups = dict
        .get(b"RBGroups")
        .and_then(|obj| resolver.deref(obj).ok())
        .map(|resolved| parse_rb_groups(resolver, &resolved))
        .unwrap_or_default();

    Configuration {
        index,
        name,
        creator,
        base_state,
        on,
        off,
        intent,
        auto_state,
        order,
        list_mode,
        rb_groups,
        locked,
    }
}

/// Parse `/Order` according to ISO 32000-2 §8.11.4.3.
///
/// The array is walked left-to-right with one element of look-back
/// state to classify each item:
///
/// - **OCG ref** — leaf [`LayerTreeNode::Layer`].
/// - **OCG ref immediately followed by a nested array** — the array
///   is consumed as that layer's section (header-layer section).
/// - **String literal followed by a nested array** — labelled section.
/// - **Bare nested array** — anonymous section.
/// - **String not followed by an array** — dropped with a warning.
pub fn parse_order(resolver: &Resolver, obj: &PdfObj, sink: &WarningSink) -> LayerTree {
    let Some(arr) = obj.as_array() else {
        return LayerTree::default();
    };
    LayerTree {
        nodes: parse_order_nodes(resolver, arr, sink),
    }
}

fn parse_order_nodes(
    resolver: &Resolver,
    items: &[PdfObj],
    sink: &WarningSink,
) -> Vec<LayerTreeNode> {
    let mut nodes = Vec::new();
    let mut i = 0;
    while i < items.len() {
        let item = &items[i];

        // Resolve refs to inspect type, but keep the original ref's
        // (num, gen) for layer leaves.
        let resolved = resolver.deref(item).ok();
        let view = resolved.as_ref().unwrap_or(item);

        match view {
            // String literal: labelled section. The next item must
            // be a nested array; if it isn't, drop the string.
            PdfObj::Str(s) => {
                let label = crate::metadata::decode_pdf_text_string_pub(s);
                if let Some(next) = items.get(i + 1) {
                    let next_resolved = resolver.deref(next).ok();
                    let next_view = next_resolved.as_ref().unwrap_or(next);
                    if let PdfObj::Array(children_arr) = next_view {
                        let children = parse_order_nodes(resolver, children_arr, sink);
                        nodes.push(LayerTreeNode::Section {
                            label: Some(label),
                            header_layer: None,
                            children,
                        });
                        i += 2;
                        continue;
                    }
                }
                sink.record(
                    ParsePhase::Layers,
                    None,
                    Severity::Warning,
                    format!("/Order string label {label:?} not followed by a child array; dropped"),
                );
                i += 1;
            }

            // Bare nested array: anonymous section.
            PdfObj::Array(children_arr) => {
                let children = parse_order_nodes(resolver, children_arr, sink);
                nodes.push(LayerTreeNode::Section {
                    label: None,
                    header_layer: None,
                    children,
                });
                i += 1;
            }

            // OCG dict (we resolved a ref to a dict). Decide whether
            // the *next* item is a nested array, in which case this
            // layer is a section header.
            PdfObj::Dict(_) => {
                let Some((ocg_id, _gen)) = item.as_ref() else {
                    sink.record(
                        ParsePhase::Layers,
                        None,
                        Severity::Warning,
                        "/Order layer entry is an inline OCG dict (not a ref); skipped",
                    );
                    i += 1;
                    continue;
                };
                if let Some(next) = items.get(i + 1) {
                    let next_resolved = resolver.deref(next).ok();
                    let next_view = next_resolved.as_ref().unwrap_or(next);
                    if let PdfObj::Array(children_arr) = next_view {
                        let children = parse_order_nodes(resolver, children_arr, sink);
                        nodes.push(LayerTreeNode::Section {
                            label: None,
                            header_layer: Some(ocg_id),
                            children,
                        });
                        i += 2;
                        continue;
                    }
                }
                nodes.push(LayerTreeNode::Layer(ocg_id));
                i += 1;
            }

            _ => {
                sink.record(
                    ParsePhase::Layers,
                    None,
                    Severity::Warning,
                    "/Order item is neither layer ref, string, nor array; skipped",
                );
                i += 1;
            }
        }
    }
    nodes
}

/// Parse `/AS` — an array of rule dicts.
pub fn parse_auto_state(
    resolver: &Resolver,
    obj: &PdfObj,
    sink: &WarningSink,
) -> Vec<AutoStateRule> {
    let Some(arr) = obj.as_array() else {
        return Vec::new();
    };
    let mut rules = Vec::with_capacity(arr.len());
    for entry in arr {
        let Ok(resolved) = resolver.deref(entry) else {
            continue;
        };
        let Some(dict) = resolved.as_dict() else {
            continue;
        };
        let Some(event) = dict.get_name(b"Event").and_then(AutoStateEvent::from_name) else {
            sink.record(
                ParsePhase::Layers,
                None,
                Severity::Warning,
                "/AS rule missing or unknown /Event; skipped",
            );
            continue;
        };

        let categories = dict
            .get_array(b"Category")
            .map(|arr| {
                arr.iter()
                    .filter_map(|o| o.as_name())
                    .map(|n| String::from_utf8_lossy(n).into_owned())
                    .collect()
            })
            .unwrap_or_default();

        let ocgs = dict
            .get(b"OCGs")
            .and_then(|obj| resolver.deref(obj).ok())
            .and_then(|resolved| resolved.as_array().map(<[PdfObj]>::to_vec))
            .map(|arr| {
                arr.iter()
                    .filter_map(|o| o.as_ref().map(|(n, _)| n))
                    .collect()
            })
            .unwrap_or_default();

        rules.push(AutoStateRule {
            event,
            categories,
            ocgs,
        });
    }
    rules
}

/// Parse `/RBGroups` — an array of nested arrays, each containing
/// OCG refs.
pub fn parse_rb_groups(resolver: &Resolver, obj: &PdfObj) -> Vec<Vec<u32>> {
    let Some(arr) = obj.as_array() else {
        return Vec::new();
    };
    let mut groups = Vec::with_capacity(arr.len());
    for entry in arr {
        let Ok(resolved) = resolver.deref(entry) else {
            continue;
        };
        let Some(group_arr) = resolved.as_array() else {
            continue;
        };
        let group: Vec<u32> = group_arr
            .iter()
            .filter_map(|o| o.as_ref().map(|(n, _)| n))
            .collect();
        if !group.is_empty() {
            groups.push(group);
        }
    }
    groups
}

/// Read `<key>` on a configuration dict as a flat list of OCG object
/// numbers. The value can be an indirect-referenced array.
fn collect_ocg_refs(resolver: &Resolver, dict: &PdfDict, key: &[u8]) -> Vec<u32> {
    let Some(obj) = dict.get(key) else {
        return Vec::new();
    };
    let Ok(resolved) = resolver.deref(obj) else {
        return Vec::new();
    };
    let Some(arr) = resolved.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|o| o.as_ref().map(|(n, _)| n))
        .collect()
}

fn catalog_dict(resolver: &Resolver) -> Option<PdfDict> {
    if let Some((num, gen_num)) = resolver.trailer().get_ref(b"Root")
        && let Ok(obj) = resolver.resolve(num, gen_num)
        && let Some(dict) = obj.as_dict()
    {
        return Some(dict.clone());
    }
    crate::find_catalog(resolver).and_then(|obj| obj.as_dict().cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_state_names() {
        assert_eq!(BaseState::from_name(b"ON"), Some(BaseState::On));
        assert_eq!(BaseState::from_name(b"OFF"), Some(BaseState::Off));
        assert_eq!(
            BaseState::from_name(b"Unchanged"),
            Some(BaseState::Unchanged)
        );
        assert_eq!(BaseState::from_name(b"on"), None);
    }

    #[test]
    fn list_mode_names() {
        assert_eq!(ListMode::from_name(b"AllPages"), Some(ListMode::AllPages));
        assert_eq!(
            ListMode::from_name(b"VisiblePages"),
            Some(ListMode::VisiblePages)
        );
        assert_eq!(ListMode::from_name(b"Other"), None);
    }

    #[test]
    fn auto_state_event_names() {
        assert_eq!(
            AutoStateEvent::from_name(b"View"),
            Some(AutoStateEvent::View)
        );
        assert_eq!(
            AutoStateEvent::from_name(b"Print"),
            Some(AutoStateEvent::Print)
        );
        assert_eq!(
            AutoStateEvent::from_name(b"Export"),
            Some(AutoStateEvent::Export)
        );
        assert_eq!(AutoStateEvent::from_name(b"Save"), None);
    }
}
