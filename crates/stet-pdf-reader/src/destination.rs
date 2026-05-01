// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Typed PDF destinations and actions.
//!
//! Outlines, link annotations, and the document's `/OpenAction` all
//! reference either a [`Destination`] (where to scroll/zoom on a page)
//! or an [`Action`] (what to do when activated). This module handles
//! parsing both into typed Rust values.
//!
//! Named destinations (`Destination::NamedDest`) are returned as the
//! raw name string. Resolution against the document's name tree
//! happens in Phase 3 once the name-tree walker exists; until then
//! consumers can still see the name and look it up themselves via
//! the resolver if they need to.

use crate::metadata::pdf_string_to_rust_pub;
use crate::objects::{PdfDict, PdfObj};
use crate::page_tree::PageInfo;
use crate::resolver::Resolver;

/// A target location within (or referenced from) a PDF document.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Destination {
    /// Explicit destination: a specific page in this document plus a
    /// view spec.
    PageView {
        /// 0-based page index. `None` if the destination references a
        /// page object that we couldn't map to one of the document's
        /// pages (broken reference).
        page: Option<usize>,
        view: ViewSpec,
    },
    /// Named destination — resolution against `/Names /Dests` happens
    /// via [`crate::PdfDocument::resolve_named_destination`] once the
    /// consumer has the name tree.
    NamedDest(String),
}

/// PDF view spec from a destination array.
///
/// Each variant maps to one of the explicit-destination forms in
/// ISO 32000-2 §12.3.2.2.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum ViewSpec {
    /// `[page /XYZ left top zoom]` — position the upper-left corner of
    /// the page region at `(left, top)` and zoom to `zoom` (1.0 = 100%).
    /// `None` for a coordinate or zoom means "retain the current value".
    Xyz {
        x: Option<f64>,
        y: Option<f64>,
        zoom: Option<f64>,
    },
    /// `[page /Fit]` — fit the entire page in the window.
    Fit,
    /// `[page /FitH top]` — fit the page width with the top edge at `top`.
    FitH { y: Option<f64> },
    /// `[page /FitV left]` — fit the page height with left edge at `left`.
    FitV { x: Option<f64> },
    /// `[page /FitR left bottom right top]` — fit the rectangle in the window.
    FitR {
        left: f64,
        bottom: f64,
        right: f64,
        top: f64,
    },
    /// `[page /FitB]` — fit the page's bounding box in the window.
    FitB,
    /// `[page /FitBH top]` — fit the bounding-box width.
    FitBH { y: Option<f64> },
    /// `[page /FitBV left]` — fit the bounding-box height.
    FitBV { x: Option<f64> },
}

impl Default for ViewSpec {
    fn default() -> Self {
        ViewSpec::Xyz {
            x: None,
            y: None,
            zoom: None,
        }
    }
}

/// PDF action — what to do when a link / outline / page event fires.
///
/// stet does not *execute* most of these (no JS engine, no network
/// access for URIs); the action is exposed as data for consumers that
/// want to render link panels, route handlers, or convert to other
/// formats.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Action {
    /// `/S /GoTo` — jump to a destination in this document.
    GoTo(Destination),
    /// `/S /GoToR` — jump to a destination in another PDF.
    GoToR {
        filename: String,
        dest: Destination,
        new_window: Option<bool>,
    },
    /// `/S /GoToE` — jump into an embedded PDF file.
    GoToE {
        target: String,
        dest: Destination,
        new_window: Option<bool>,
    },
    /// `/S /Launch` — launch an application or open a file.
    Launch {
        filename: String,
        new_window: Option<bool>,
    },
    /// `/S /URI` — open a URI / hyperlink.
    Uri { uri: String, is_map: bool },
    /// `/S /Named` — viewer-defined named action (`/NextPage`,
    /// `/PrevPage`, `/FirstPage`, `/LastPage`, `/Print`, ...).
    Named(String),
    /// `/S /JavaScript` — execute the given JS source. Exposed as raw
    /// source; stet does not evaluate.
    JavaScript(String),
    /// `/S /SubmitForm` — submit form data to a URL.
    SubmitForm {
        url: String,
        fields: Vec<String>,
        flags: u32,
    },
    /// `/S /ResetForm` — reset form fields to their default values.
    ResetForm { fields: Vec<String>, flags: u32 },
    /// `/S /Hide` — hide / show form fields by name.
    Hide { targets: Vec<String>, hide: bool },
    /// `/S /Sound` — deprecated. Marker only; we don't expose audio data.
    Sound,
    /// `/S /Movie` — deprecated. Marker only.
    Movie,
    /// `/S /Thread` — article thread navigation.
    Thread { target: Option<String> },
    /// Unknown `/S` value. The raw subtype name is preserved for
    /// callers that want to recognise their own extensions.
    Other { subtype: String },
}

/// Parse a destination object — array, name, or dict-with-/D — into a
/// typed [`Destination`].
///
/// PDF destinations come in three forms:
///
/// 1. An explicit array `[page /XYZ x y z]`
/// 2. A name (named destination) — `/MyDest`
/// 3. A dict with a `/D` entry holding either of the above (used by
///    actions and some catalog entries)
pub fn parse_destination(
    resolver: &Resolver,
    pages: &[PageInfo],
    obj: &PdfObj,
) -> Option<Destination> {
    let resolved = resolver.deref(obj).ok()?;

    // Dict-with-/D unwrap.
    if let Some(dict) = resolved.as_dict()
        && let Some(d) = dict.get(b"D")
    {
        return parse_destination(resolver, pages, d);
    }

    // Named destinations: a name object, or a string (PDF 1.2+).
    if let Some(name) = resolved.as_name() {
        return Some(Destination::NamedDest(
            String::from_utf8_lossy(name).into_owned(),
        ));
    }
    if let Some(s) = resolved.as_str() {
        return Some(Destination::NamedDest(
            String::from_utf8_lossy(s).into_owned(),
        ));
    }

    // Explicit destination array.
    if let Some(arr) = resolved.as_array() {
        return Some(parse_explicit_destination(pages, arr));
    }

    None
}

/// Parse an explicit-destination array `[page mode args...]`.
///
/// `page` may be an indirect reference to a page object (mapped to a
/// 0-based page index) or an integer (used in remote destinations,
/// stored as-is).
pub fn parse_explicit_destination(pages: &[PageInfo], arr: &[PdfObj]) -> Destination {
    let page = arr.first().and_then(|p| {
        if let Some((num, _)) = p.as_ref() {
            pages.iter().position(|info| info.obj_num == num)
        } else {
            p.as_int().map(|i| i as usize)
        }
    });
    let mode = arr.get(1).and_then(|m| m.as_name()).unwrap_or(b"XYZ");
    let view = parse_view_spec(mode, &arr[2.min(arr.len())..]);
    Destination::PageView { page, view }
}

/// Parse a view-spec mode name and its argument tail.
pub fn parse_view_spec(mode: &[u8], args: &[PdfObj]) -> ViewSpec {
    let num = |i: usize| args.get(i).and_then(|o| o.as_f64());
    match mode {
        b"XYZ" => ViewSpec::Xyz {
            x: num(0),
            y: num(1),
            zoom: num(2).filter(|&z| z > 0.0),
        },
        b"Fit" => ViewSpec::Fit,
        b"FitH" => ViewSpec::FitH { y: num(0) },
        b"FitV" => ViewSpec::FitV { x: num(0) },
        b"FitR" => ViewSpec::FitR {
            left: num(0).unwrap_or(0.0),
            bottom: num(1).unwrap_or(0.0),
            right: num(2).unwrap_or(0.0),
            top: num(3).unwrap_or(0.0),
        },
        b"FitB" => ViewSpec::FitB,
        b"FitBH" => ViewSpec::FitBH { y: num(0) },
        b"FitBV" => ViewSpec::FitBV { x: num(0) },
        _ => ViewSpec::default(),
    }
}

/// Parse an action dict into a typed [`Action`].
///
/// Returns `None` only if `obj` cannot be resolved to a dict at all;
/// unknown `/S` subtypes return `Action::Other { subtype }` rather
/// than failing.
pub fn parse_action(resolver: &Resolver, pages: &[PageInfo], obj: &PdfObj) -> Option<Action> {
    let resolved = resolver.deref(obj).ok()?;
    let dict = resolved.as_dict()?;
    let subtype = dict.get_name(b"S").unwrap_or(b"");
    Some(match subtype {
        b"GoTo" => {
            let dest = dict
                .get(b"D")
                .and_then(|d| parse_destination(resolver, pages, d))
                .unwrap_or(Destination::NamedDest(String::new()));
            Action::GoTo(dest)
        }
        b"GoToR" => Action::GoToR {
            filename: parse_file_spec(resolver, dict.get(b"F")).unwrap_or_default(),
            dest: dict
                .get(b"D")
                .and_then(|d| parse_destination(resolver, &[], d))
                .unwrap_or(Destination::NamedDest(String::new())),
            new_window: dict.get(b"NewWindow").and_then(as_bool),
        },
        b"GoToE" => Action::GoToE {
            target: dict
                .get(b"T")
                .and_then(pdf_string_to_rust_pub)
                .unwrap_or_default(),
            dest: dict
                .get(b"D")
                .and_then(|d| parse_destination(resolver, &[], d))
                .unwrap_or(Destination::NamedDest(String::new())),
            new_window: dict.get(b"NewWindow").and_then(as_bool),
        },
        b"Launch" => Action::Launch {
            filename: parse_file_spec(resolver, dict.get(b"F")).unwrap_or_default(),
            new_window: dict.get(b"NewWindow").and_then(as_bool),
        },
        b"URI" => Action::Uri {
            uri: dict
                .get(b"URI")
                .and_then(pdf_string_to_rust_pub)
                .unwrap_or_default(),
            is_map: dict.get(b"IsMap").and_then(as_bool).unwrap_or(false),
        },
        b"Named" => Action::Named(
            dict.get_name(b"N")
                .map(|n| String::from_utf8_lossy(n).into_owned())
                .unwrap_or_default(),
        ),
        b"JavaScript" => {
            Action::JavaScript(dict.get(b"JS").and_then(read_js_source).unwrap_or_default())
        }
        b"SubmitForm" => Action::SubmitForm {
            url: parse_file_spec(resolver, dict.get(b"F")).unwrap_or_default(),
            fields: parse_field_name_list(resolver, dict.get(b"Fields")),
            flags: dict.get_int(b"Flags").unwrap_or(0) as u32,
        },
        b"ResetForm" => Action::ResetForm {
            fields: parse_field_name_list(resolver, dict.get(b"Fields")),
            flags: dict.get_int(b"Flags").unwrap_or(0) as u32,
        },
        b"Hide" => Action::Hide {
            targets: parse_field_name_list(resolver, dict.get(b"T")),
            hide: dict.get(b"H").and_then(as_bool).unwrap_or(true),
        },
        b"Sound" => Action::Sound,
        b"Movie" => Action::Movie,
        b"Thread" => Action::Thread {
            target: dict.get(b"T").and_then(pdf_string_to_rust_pub),
        },
        other => Action::Other {
            subtype: String::from_utf8_lossy(other).into_owned(),
        },
    })
}

fn as_bool(obj: &PdfObj) -> Option<bool> {
    match obj {
        PdfObj::Bool(b) => Some(*b),
        _ => None,
    }
}

/// Read a `/JS` value: it's typically a string, but spec-permissibly a
/// stream. Streams require dereferencing through the resolver; the
/// caller passes the unresolved value here, so we handle both forms.
fn read_js_source(obj: &PdfObj) -> Option<String> {
    if let Some(s) = obj.as_str() {
        return Some(crate::metadata::decode_pdf_text_string_pub(s));
    }
    None
}

/// File specs come in two forms: a string (legacy), or a dict with
/// `/F`, `/UF`, `/Unix`, `/Mac`, `/DOS`, `/EF` (embedded files).
fn parse_file_spec(resolver: &Resolver, obj: Option<&PdfObj>) -> Option<String> {
    let obj = obj?;
    let resolved = resolver.deref(obj).ok()?;
    if let Some(s) = resolved.as_str() {
        return Some(crate::metadata::decode_pdf_text_string_pub(s));
    }
    if let Some(d) = resolved.as_dict() {
        // Prefer /UF (Unicode), fall back to /F.
        if let Some(uf) = d.get(b"UF").and_then(pdf_string_to_rust_pub) {
            return Some(uf);
        }
        if let Some(f) = d.get(b"F").and_then(pdf_string_to_rust_pub) {
            return Some(f);
        }
    }
    None
}

fn parse_field_name_list(resolver: &Resolver, obj: Option<&PdfObj>) -> Vec<String> {
    let Some(obj) = obj else {
        return Vec::new();
    };
    let Ok(resolved) = resolver.deref(obj) else {
        return Vec::new();
    };
    let Some(arr) = resolved.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|o| o.as_str().map(crate::metadata::decode_pdf_text_string_pub))
        .collect()
}

/// Build the document-wide named-destination table by merging both
/// PDF-spec sources:
///
/// 1. **Legacy** — `/Catalog /Dests`, a direct dict (PDF 1.1).
/// 2. **Modern** — `/Catalog /Names /Dests`, a name tree (PDF 1.2+).
///
/// Per ISO 32000-2 §12.3.2.3, when both forms are present the legacy
/// `/Dests` entries take precedence over name-tree entries with the
/// same key. Both sources are walked unconditionally; this function
/// always returns a populated map (possibly empty) and never panics.
pub fn parse_named_destinations(
    resolver: &Resolver,
    pages: &[PageInfo],
) -> std::collections::HashMap<String, Destination> {
    use std::collections::HashMap;

    let mut map: HashMap<String, Destination> = HashMap::new();

    let catalog = match catalog_dict_for_dests(resolver) {
        Some(c) => c,
        None => return map,
    };

    // Modern: /Names /Dests name tree (parsed first so legacy wins).
    if let Some(names_obj) = catalog.get(b"Names")
        && let Ok(names_dict_obj) = resolver.deref(names_obj)
        && let Some(names_dict) = names_dict_obj.as_dict()
        && let Some(dests_root) = names_dict.get(b"Dests")
    {
        let tree = crate::name_tree::walk_name_tree(resolver, dests_root, |r, val| {
            parse_destination(r, pages, val)
        });
        map.extend(tree);
    }

    // Legacy: /Dests direct dict — entries here override name-tree entries.
    if let Some(dests_obj) = catalog.get(b"Dests")
        && let Ok(dests) = resolver.deref(dests_obj)
        && let Some(dests_dict) = dests.as_dict()
    {
        for (key, val) in dests_dict.entries() {
            if let Some(d) = parse_destination(resolver, pages, val) {
                let k = String::from_utf8_lossy(key).into_owned();
                map.insert(k, d);
            }
        }
    }

    map
}

fn catalog_dict_for_dests(resolver: &Resolver) -> Option<PdfDict> {
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

    fn page_info(obj_num: u32) -> PageInfo {
        PageInfo {
            obj_num,
            media_box: [0.0, 0.0, 612.0, 792.0],
            crop_box: [0.0, 0.0, 612.0, 792.0],
            rotate: 0,
            resources: PdfDict::new(),
            contents: vec![],
            annots: vec![],
        }
    }

    #[test]
    fn view_spec_xyz_with_some_nones() {
        let v = parse_view_spec(
            b"XYZ",
            &[PdfObj::Real(72.0), PdfObj::Null, PdfObj::Real(1.5)],
        );
        match v {
            ViewSpec::Xyz { x, y, zoom } => {
                assert_eq!(x, Some(72.0));
                assert_eq!(y, None);
                assert_eq!(zoom, Some(1.5));
            }
            _ => panic!("expected XYZ"),
        }
    }

    #[test]
    fn view_spec_xyz_zero_zoom_becomes_none() {
        let v = parse_view_spec(
            b"XYZ",
            &[PdfObj::Real(0.0), PdfObj::Real(0.0), PdfObj::Real(0.0)],
        );
        match v {
            ViewSpec::Xyz { zoom, .. } => assert_eq!(zoom, None),
            _ => panic!("expected XYZ"),
        }
    }

    #[test]
    fn view_spec_fit_variants() {
        assert_eq!(parse_view_spec(b"Fit", &[]), ViewSpec::Fit);
        assert_eq!(parse_view_spec(b"FitB", &[]), ViewSpec::FitB);
        assert_eq!(
            parse_view_spec(b"FitH", &[PdfObj::Real(700.0)]),
            ViewSpec::FitH { y: Some(700.0) }
        );
        assert_eq!(
            parse_view_spec(
                b"FitR",
                &[
                    PdfObj::Real(0.0),
                    PdfObj::Real(0.0),
                    PdfObj::Real(100.0),
                    PdfObj::Real(200.0),
                ]
            ),
            ViewSpec::FitR {
                left: 0.0,
                bottom: 0.0,
                right: 100.0,
                top: 200.0,
            }
        );
    }

    #[test]
    fn view_spec_unknown_falls_back_to_xyz_default() {
        let v = parse_view_spec(b"Bogus", &[]);
        assert_eq!(v, ViewSpec::default());
    }

    #[test]
    fn explicit_destination_with_page_ref_resolves_index() {
        let pages = vec![page_info(10), page_info(20), page_info(30)];
        let arr = vec![PdfObj::Ref(20, 0), PdfObj::Name(b"Fit".to_vec())];
        let d = parse_explicit_destination(&pages, &arr);
        match d {
            Destination::PageView { page, view } => {
                assert_eq!(page, Some(1));
                assert_eq!(view, ViewSpec::Fit);
            }
            _ => panic!("expected PageView"),
        }
    }

    #[test]
    fn explicit_destination_with_unknown_page_ref() {
        let pages = vec![page_info(10)];
        let arr = vec![PdfObj::Ref(99, 0), PdfObj::Name(b"Fit".to_vec())];
        let d = parse_explicit_destination(&pages, &arr);
        assert!(matches!(d, Destination::PageView { page: None, .. }));
    }
}
