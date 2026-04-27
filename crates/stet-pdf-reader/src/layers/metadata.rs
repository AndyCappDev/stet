// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Per-layer metadata (Optional Content Groups).
//!
//! Defined in ISO 32000-2 §8.11.2 (the OCG dictionary itself) and
//! §8.11.4.4 (the `/Usage` sub-dict, which describes the contexts —
//! view, print, export — in which the layer is meaningful).
//!
//! Phase 1 is **document-wide enumeration**: produce one [`Layer`]
//! record per OCG referenced from the catalog's
//! `/OCProperties /OCGs` array. Visibility defaults come from the
//! default configuration's `/OFF` array; locked state from `/Locked`.
//! Hierarchy (`/Order`) and alternate configurations live in Phase 2.

use std::collections::HashSet;

use crate::diagnostics::{LocationHint, ParsePhase, Severity, WarningSink};
use crate::metadata::{decode_pdf_text_string_pub, pdf_string_to_rust_pub};
use crate::objects::{PdfDict, PdfObj};
use crate::resolver::Resolver;

/// One Optional Content Group (PDF "layer").
#[derive(Debug, Clone)]
pub struct Layer {
    /// PDF object number of the OCG dict — stable across renders and
    /// the canonical key for matching layers to display-list
    /// `OcgGroup` elements.
    pub ocg_id: u32,
    /// `/Name` — UTF-8 decoded display label for layer panels.
    pub name: String,
    /// `/Intent` — author's hint about which contexts this layer is
    /// meaningful in. Default `View`.
    pub intent: LayerIntent,
    /// True when listed in `/OCProperties /D /Locked`. UI consumers
    /// should disable user-driven toggling for locked layers; the
    /// reader does not enforce this.
    pub locked: bool,
    /// `/Usage` sub-dict — context-specific hints (view/print/export
    /// state, zoom range, language, page-element role, etc.).
    pub usage: LayerUsage,
    /// `/CreatorInfo` on the OCG itself, if present. The same
    /// sub-dict can also appear under `/Usage`; Phase 1 captures
    /// both.
    pub creator_info: Option<CreatorInfo>,
    /// Initial visibility under the default configuration. Derived
    /// from membership in `/OCProperties /D /OFF` (false) or `/ON`
    /// (true); absent OCGs default to true per ISO 32000-2.
    pub default_visible: bool,
}

/// `/Intent` on an OCG, declaring which audiences the layer is for.
///
/// PDF allows a single name or an array of names; an array becomes
/// [`LayerIntent::Multiple`]. Names other than `View`/`Design`/`Export`
/// are preserved verbatim under [`LayerIntent::Other`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayerIntent {
    /// Default. Layer is meaningful for on-screen viewing.
    View,
    /// Layer represents structural design content (CAD drawings,
    /// engineering layers, etc.). Some viewers ignore non-`View`
    /// intents in interactive mode.
    Design,
    /// Layer is intended for export workflows (data extraction,
    /// archiving).
    Export,
    /// Multiple intents — array form.
    Multiple(Vec<String>),
    /// An intent name not covered by the spec's standard set.
    Other(String),
}

impl LayerIntent {
    /// Construct from the raw `/Intent` value.
    fn from_obj(resolver: &Resolver, obj: &PdfObj) -> Self {
        match resolver.deref(obj).ok().as_ref().unwrap_or(obj) {
            PdfObj::Name(name) => Self::from_single_name(name),
            PdfObj::Array(items) => {
                let names: Vec<String> = items
                    .iter()
                    .filter_map(|o| {
                        let resolved = resolver.deref(o).ok();
                        let bytes = resolved
                            .as_ref()
                            .and_then(|r| r.as_name())
                            .or_else(|| o.as_name())?;
                        Some(String::from_utf8_lossy(bytes).into_owned())
                    })
                    .collect();
                if names.len() == 1 {
                    Self::from_single_name(names[0].as_bytes())
                } else {
                    LayerIntent::Multiple(names)
                }
            }
            _ => LayerIntent::View,
        }
    }

    fn from_single_name(name: &[u8]) -> Self {
        match name {
            b"View" => LayerIntent::View,
            b"Design" => LayerIntent::Design,
            b"Export" => LayerIntent::Export,
            other => LayerIntent::Other(String::from_utf8_lossy(other).into_owned()),
        }
    }
}

/// Hints from the OCG `/Usage` sub-dict.
///
/// Every field is optional — a layer with no `/Usage` produces a
/// fully-`None` value. Phase 5 of the layer plan turns these hints
/// into automatic [`LayerSet`] adjustments under render intents
/// (View / Print / Export); Phase 1 just exposes the raw structure.
///
/// [`LayerSet`]: <crate-root>::LayerSet
#[derive(Debug, Clone, Default)]
pub struct LayerUsage {
    /// `/CreatorInfo` sub-dict — application that authored the layer.
    pub creator_info: Option<CreatorInfo>,
    /// `/Language` sub-dict — language tag and "preferred" flag.
    pub language: Option<LanguageUsage>,
    /// `/Export` sub-dict — visibility under export intent.
    pub export: Option<ExportUsage>,
    /// `/Zoom` sub-dict — min/max zoom range in which the layer is
    /// visible.
    pub zoom: Option<ZoomUsage>,
    /// `/Print` sub-dict — visibility and subtype hint under print
    /// intent.
    pub print: Option<PrintUsage>,
    /// `/View` sub-dict — visibility under interactive view intent.
    pub view: Option<ViewUsage>,
    /// `/User` sub-dict — user/group ownership.
    pub user: Option<UserUsage>,
    /// `/PageElement` sub-dict — role on the page (header, footer,
    /// foreground, background, logo).
    pub page_element: Option<PageElementSubtype>,
}

/// Usage state — `/ON` or `/OFF`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageState {
    On,
    Off,
}

impl UsageState {
    fn from_name(name: &[u8]) -> Option<Self> {
        match name {
            b"ON" => Some(UsageState::On),
            b"OFF" => Some(UsageState::Off),
            _ => None,
        }
    }
}

/// `/View` sub-dict.
#[derive(Debug, Clone, Copy)]
pub struct ViewUsage {
    /// `/ViewState` — visibility for interactive view.
    pub state: UsageState,
}

/// `/Print` sub-dict.
#[derive(Debug, Clone)]
pub struct PrintUsage {
    /// `/Subtype` — `Trapped`, `PrintersMarks`, `Watermark`, etc.
    /// Preserved verbatim; the reader does not interpret it.
    pub subtype: Option<String>,
    /// `/PrintState` — visibility for print intent.
    pub state: UsageState,
}

/// `/Export` sub-dict.
#[derive(Debug, Clone, Copy)]
pub struct ExportUsage {
    /// `/ExportState` — visibility for export intent.
    pub state: UsageState,
}

/// `/Zoom` sub-dict — visibility range.
#[derive(Debug, Clone, Copy, Default)]
pub struct ZoomUsage {
    /// `/min` — minimum magnification (inclusive). Below this the
    /// layer is hidden.
    pub min: Option<f64>,
    /// `/max` — maximum magnification (exclusive). At or above this
    /// the layer is hidden.
    pub max: Option<f64>,
}

/// `/Language` sub-dict.
#[derive(Debug, Clone)]
pub struct LanguageUsage {
    /// `/Lang` — BCP 47 language tag (e.g. `en-US`).
    pub lang: String,
    /// `/Preferred` — true when the layer is the preferred choice
    /// for its language.
    pub preferred: bool,
}

/// `/User` sub-dict — user/group ownership.
#[derive(Debug, Clone, Default)]
pub struct UserUsage {
    /// `/Type` — `Ind` (individual), `Ttl` (title), `Org`
    /// (organisation).
    pub user_type: Option<String>,
    /// `/Name` — list of user/group names. PDF allows either a
    /// single string or an array; both forms collapse to this `Vec`.
    pub names: Vec<String>,
}

/// `/PageElement /Subtype` value — role of the layer on the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageElementSubtype {
    /// `/HF` — headers and footers. PDF combines them under one name.
    HeaderFooter,
    /// `/FG` — foreground content.
    Foreground,
    /// `/BG` — background content.
    Background,
    /// `/L` — logo.
    Logo,
}

impl PageElementSubtype {
    fn from_name(name: &[u8]) -> Option<Self> {
        match name {
            b"HF" => Some(PageElementSubtype::HeaderFooter),
            b"FG" => Some(PageElementSubtype::Foreground),
            b"BG" => Some(PageElementSubtype::Background),
            b"L" => Some(PageElementSubtype::Logo),
            _ => None,
        }
    }
}

/// `/CreatorInfo` sub-dict — authoring application + subtype hint.
#[derive(Debug, Clone)]
pub struct CreatorInfo {
    /// `/Creator` — application name.
    pub creator: String,
    /// `/Subtype` — application-specific layer subtype hint
    /// (e.g. `Artwork`, `Technical`).
    pub subtype: Option<String>,
}

/// Walk the catalog's `/OCProperties /OCGs` array and produce one
/// [`Layer`] per OCG.
///
/// Returns an empty `Vec` when the document has no OCGs. Default
/// visibility comes from `/OCProperties /D /OFF`; locked state from
/// `/OCProperties /D /Locked`. Both arrays may be indirect-referenced.
///
/// Cycles cannot occur (the OCGs array is a flat list of refs), so no
/// visited-set guarding is needed; malformed entries (non-dict, no
/// `/Type /OCG`) are skipped with a warning.
pub fn parse_layers(resolver: &Resolver, sink: &WarningSink) -> Vec<Layer> {
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

    let off_set = collect_ocg_id_set(resolver, oc_dict, b"D", b"OFF");
    let locked_set = collect_ocg_id_set(resolver, oc_dict, b"D", b"Locked");

    // Resolve /OCGs — may be a direct array or an indirect ref.
    let Some(ocgs_obj) = oc_dict.get(b"OCGs") else {
        return Vec::new();
    };
    let ocgs_resolved = match resolver.deref(ocgs_obj) {
        Ok(o) => o,
        Err(_) => {
            sink.record(
                ParsePhase::Layers,
                None,
                Severity::Warning,
                "/OCProperties /OCGs could not be resolved",
            );
            return Vec::new();
        }
    };
    let Some(ocgs) = ocgs_resolved.as_array() else {
        sink.record(
            ParsePhase::Layers,
            None,
            Severity::Warning,
            "/OCProperties /OCGs is not an array",
        );
        return Vec::new();
    };

    let mut layers = Vec::with_capacity(ocgs.len());
    let mut seen = HashSet::new();
    for entry in ocgs {
        let Some((num, gen_num)) = entry.as_ref() else {
            sink.record(
                ParsePhase::Layers,
                None,
                Severity::Warning,
                "OCG entry is not an indirect reference; skipped",
            );
            continue;
        };
        if !seen.insert(num) {
            continue;
        }
        match resolver.resolve(num, gen_num) {
            Ok(obj) => {
                if let Some(dict) = obj.as_dict() {
                    if let Some(layer) =
                        parse_layer(resolver, dict, num, &off_set, &locked_set, sink)
                    {
                        layers.push(layer);
                    }
                } else {
                    sink.record(
                        ParsePhase::Layers,
                        Some(LocationHint::Object {
                            obj_num: num,
                            gen_num,
                        }),
                        Severity::Warning,
                        "OCG object is not a dict; skipped",
                    );
                }
            }
            Err(_) => {
                sink.record(
                    ParsePhase::Layers,
                    Some(LocationHint::Object {
                        obj_num: num,
                        gen_num,
                    }),
                    Severity::Warning,
                    "OCG object could not be resolved; skipped",
                );
            }
        }
    }
    layers
}

/// Parse one OCG dict into a [`Layer`].
///
/// Returns `None` only if `/Type` is present and is not `/OCG`; spec-
/// conformant generators always include `/Type /OCG`, but we tolerate
/// its absence for malformed PDFs that omit it.
pub fn parse_layer(
    resolver: &Resolver,
    dict: &PdfDict,
    ocg_id: u32,
    off_set: &HashSet<u32>,
    locked_set: &HashSet<u32>,
    sink: &WarningSink,
) -> Option<Layer> {
    if let Some(type_name) = dict.get_name(b"Type")
        && type_name != b"OCG"
    {
        sink.record(
            ParsePhase::Layers,
            Some(LocationHint::Object {
                obj_num: ocg_id,
                gen_num: 0,
            }),
            Severity::Warning,
            format!(
                "OCG dict has unexpected /Type {}; skipped",
                String::from_utf8_lossy(type_name)
            ),
        );
        return None;
    }

    let name = dict
        .get(b"Name")
        .and_then(pdf_string_to_rust_pub)
        .unwrap_or_default();

    let intent = dict
        .get(b"Intent")
        .map(|obj| LayerIntent::from_obj(resolver, obj))
        .unwrap_or(LayerIntent::View);

    let usage = dict
        .get(b"Usage")
        .and_then(|obj| resolver.deref(obj).ok())
        .and_then(|resolved| resolved.as_dict().cloned())
        .map(|d| parse_usage_dict(resolver, &d))
        .unwrap_or_default();

    let creator_info = dict
        .get(b"CreatorInfo")
        .and_then(|obj| resolver.deref(obj).ok())
        .and_then(|resolved| resolved.as_dict().cloned())
        .and_then(|d| parse_creator_info(&d));

    Some(Layer {
        ocg_id,
        name,
        intent,
        locked: locked_set.contains(&ocg_id),
        usage,
        creator_info,
        default_visible: !off_set.contains(&ocg_id),
    })
}

/// Parse an entire `/Usage` dict into a [`LayerUsage`].
pub fn parse_usage_dict(resolver: &Resolver, dict: &PdfDict) -> LayerUsage {
    LayerUsage {
        creator_info: dict
            .get(b"CreatorInfo")
            .and_then(|obj| resolver.deref(obj).ok())
            .and_then(|resolved| resolved.as_dict().cloned())
            .and_then(|d| parse_creator_info(&d)),
        language: dict
            .get(b"Language")
            .and_then(|obj| resolver.deref(obj).ok())
            .and_then(|resolved| resolved.as_dict().cloned())
            .and_then(|d| parse_language(&d)),
        export: dict
            .get(b"Export")
            .and_then(|obj| resolver.deref(obj).ok())
            .and_then(|resolved| resolved.as_dict().cloned())
            .and_then(|d| parse_export(&d)),
        zoom: dict
            .get(b"Zoom")
            .and_then(|obj| resolver.deref(obj).ok())
            .and_then(|resolved| resolved.as_dict().cloned())
            .map(|d| parse_zoom(&d)),
        print: dict
            .get(b"Print")
            .and_then(|obj| resolver.deref(obj).ok())
            .and_then(|resolved| resolved.as_dict().cloned())
            .and_then(|d| parse_print(&d)),
        view: dict
            .get(b"View")
            .and_then(|obj| resolver.deref(obj).ok())
            .and_then(|resolved| resolved.as_dict().cloned())
            .and_then(|d| parse_view(&d)),
        user: dict
            .get(b"User")
            .and_then(|obj| resolver.deref(obj).ok())
            .and_then(|resolved| resolved.as_dict().cloned())
            .map(|d| parse_user(&d)),
        page_element: dict
            .get(b"PageElement")
            .and_then(|obj| resolver.deref(obj).ok())
            .and_then(|resolved| resolved.as_dict().cloned())
            .and_then(|d| {
                d.get_name(b"Subtype")
                    .and_then(PageElementSubtype::from_name)
            }),
    }
}

fn parse_creator_info(dict: &PdfDict) -> Option<CreatorInfo> {
    let creator = dict.get(b"Creator").and_then(pdf_string_to_rust_pub)?;
    let subtype = dict
        .get_name(b"Subtype")
        .map(|n| String::from_utf8_lossy(n).into_owned());
    Some(CreatorInfo { creator, subtype })
}

fn parse_language(dict: &PdfDict) -> Option<LanguageUsage> {
    let lang = dict.get(b"Lang").and_then(pdf_string_to_rust_pub)?;
    let preferred = dict.get_name(b"Preferred") == Some(b"ON");
    Some(LanguageUsage { lang, preferred })
}

fn parse_export(dict: &PdfDict) -> Option<ExportUsage> {
    let state = dict
        .get_name(b"ExportState")
        .and_then(UsageState::from_name)?;
    Some(ExportUsage { state })
}

fn parse_zoom(dict: &PdfDict) -> ZoomUsage {
    ZoomUsage {
        min: dict.get_f64(b"min"),
        max: dict.get_f64(b"max"),
    }
}

fn parse_print(dict: &PdfDict) -> Option<PrintUsage> {
    let state = dict
        .get_name(b"PrintState")
        .and_then(UsageState::from_name)?;
    let subtype = dict
        .get_name(b"Subtype")
        .map(|n| String::from_utf8_lossy(n).into_owned());
    Some(PrintUsage { subtype, state })
}

fn parse_view(dict: &PdfDict) -> Option<ViewUsage> {
    let state = dict
        .get_name(b"ViewState")
        .and_then(UsageState::from_name)?;
    Some(ViewUsage { state })
}

fn parse_user(dict: &PdfDict) -> UserUsage {
    let user_type = dict
        .get_name(b"Type")
        .map(|n| String::from_utf8_lossy(n).into_owned());
    let names = match dict.get(b"Name") {
        Some(PdfObj::Str(s)) => vec![decode_pdf_text_string_pub(s)],
        Some(PdfObj::Array(items)) => items
            .iter()
            .filter_map(|o| match o {
                PdfObj::Str(s) => Some(decode_pdf_text_string_pub(s)),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    UserUsage { user_type, names }
}

/// Read `/OCProperties /<config_key> /<set_key>` as a flat set of OCG
/// object numbers. `config_key` is `b"D"` for the default config;
/// `set_key` is `b"OFF"`, `b"ON"`, or `b"Locked"`.
///
/// Tolerates an indirect-referenced array. Non-ref entries are
/// silently dropped (the OCG object number is the unique stable key,
/// and inline OCG dicts are spec-violating).
fn collect_ocg_id_set(
    resolver: &Resolver,
    oc_dict: &PdfDict,
    config_key: &[u8],
    set_key: &[u8],
) -> HashSet<u32> {
    let mut ids = HashSet::new();
    let Some(config_obj) = oc_dict.get(config_key) else {
        return ids;
    };
    let Ok(config_resolved) = resolver.deref(config_obj) else {
        return ids;
    };
    let Some(config_dict) = config_resolved.as_dict() else {
        return ids;
    };
    let Some(set_obj) = config_dict.get(set_key) else {
        return ids;
    };
    let Ok(set_resolved) = resolver.deref(set_obj) else {
        return ids;
    };
    if let Some(arr) = set_resolved.as_array() {
        for o in arr {
            if let Some((num, _gen)) = o.as_ref() {
                ids.insert(num);
            }
        }
    }
    ids
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
    fn intent_from_single_name() {
        assert_eq!(LayerIntent::from_single_name(b"View"), LayerIntent::View);
        assert_eq!(
            LayerIntent::from_single_name(b"Design"),
            LayerIntent::Design
        );
        assert_eq!(
            LayerIntent::from_single_name(b"Export"),
            LayerIntent::Export
        );
        match LayerIntent::from_single_name(b"Custom") {
            LayerIntent::Other(s) => assert_eq!(s, "Custom"),
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[test]
    fn page_element_names() {
        assert_eq!(
            PageElementSubtype::from_name(b"HF"),
            Some(PageElementSubtype::HeaderFooter)
        );
        assert_eq!(
            PageElementSubtype::from_name(b"FG"),
            Some(PageElementSubtype::Foreground)
        );
        assert_eq!(
            PageElementSubtype::from_name(b"BG"),
            Some(PageElementSubtype::Background)
        );
        assert_eq!(
            PageElementSubtype::from_name(b"L"),
            Some(PageElementSubtype::Logo)
        );
        assert_eq!(PageElementSubtype::from_name(b"Other"), None);
    }

    #[test]
    fn usage_state_names() {
        assert_eq!(UsageState::from_name(b"ON"), Some(UsageState::On));
        assert_eq!(UsageState::from_name(b"OFF"), Some(UsageState::Off));
        assert_eq!(UsageState::from_name(b"on"), None);
    }
}
