// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF viewer preferences from the catalog's `/ViewerPreferences` dict
//! (and the catalog-level `/PageLayout` and `/PageMode` entries that travel
//! with them).
//!
//! These preferences are hints PDF viewers should honor when displaying
//! the document — whether to hide chrome, fit the window to the first
//! page, which page mode to open in, etc. They are advisory, not
//! mandatory; consumers may override.

use crate::objects::{PdfDict, PdfObj};
use crate::resolver::Resolver;

/// PDF viewer-preference hints, gathered from the catalog's
/// `/ViewerPreferences` sub-dict plus the catalog-level `/PageLayout` and
/// `/PageMode`.
///
/// Every field has a spec-defined default (see ISO 32000-2 §12.2 Table
/// 147, §7.7.2 Table 28); a document with no `/ViewerPreferences` dict
/// gets all defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewerPreferences {
    /// `/HideToolbar` — hide viewer toolbar when document is active.
    pub hide_toolbar: bool,
    /// `/HideMenubar` — hide viewer menu bar.
    pub hide_menubar: bool,
    /// `/HideWindowUI` — hide UI elements like scroll bars.
    pub hide_window_ui: bool,
    /// `/FitWindow` — resize the window to fit the first page.
    pub fit_window: bool,
    /// `/CenterWindow` — center the window on the screen.
    pub center_window: bool,
    /// `/DisplayDocTitle` — display the document title in the title bar.
    pub display_doc_title: bool,
    /// `/NonFullScreenPageMode` — `/PageMode` to use when leaving full-screen.
    pub non_full_screen_page_mode: PageMode,
    /// `/Direction` — predominant reading order.
    pub direction: ReadingDirection,
    /// Catalog-level `/PageLayout`.
    pub page_layout: PageLayout,
    /// Catalog-level `/PageMode`.
    pub page_mode: PageMode,
    /// `/PrintScaling` — default print-scaling preference.
    pub print_scaling: PrintScaling,
    /// `/Duplex` — default print-duplex preference.
    pub duplex: Option<Duplex>,
    /// `/PickTrayByPDFSize` — choose paper tray based on PDF page size.
    pub pick_tray_by_pdf_size: Option<bool>,
    /// `/NumCopies` — default number of copies to print.
    pub num_copies: Option<u32>,
}

impl Default for ViewerPreferences {
    fn default() -> Self {
        Self {
            hide_toolbar: false,
            hide_menubar: false,
            hide_window_ui: false,
            fit_window: false,
            center_window: false,
            display_doc_title: false,
            non_full_screen_page_mode: PageMode::UseNone,
            direction: ReadingDirection::L2R,
            page_layout: PageLayout::SinglePage,
            page_mode: PageMode::UseNone,
            print_scaling: PrintScaling::AppDefault,
            duplex: None,
            pick_tray_by_pdf_size: None,
            num_copies: None,
        }
    }
}

/// `/PageLayout` — how pages should be displayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PageLayout {
    /// `/SinglePage` — display one page at a time (default).
    SinglePage,
    /// `/OneColumn` — display pages in one continuous column.
    OneColumn,
    /// `/TwoColumnLeft` — two columns, odd-numbered pages on the left.
    TwoColumnLeft,
    /// `/TwoColumnRight` — two columns, odd-numbered pages on the right.
    TwoColumnRight,
    /// `/TwoPageLeft` — two pages, odd-numbered pages on the left.
    TwoPageLeft,
    /// `/TwoPageRight` — two pages, odd-numbered pages on the right.
    TwoPageRight,
}

/// `/PageMode` — initial document presentation mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PageMode {
    /// `/UseNone` — neither outlines nor thumbnails visible (default).
    UseNone,
    /// `/UseOutlines` — outline panel visible.
    UseOutlines,
    /// `/UseThumbs` — thumbnails panel visible.
    UseThumbs,
    /// `/FullScreen` — full-screen mode, no menu/window/UI.
    FullScreen,
    /// `/UseOC` — optional content panel visible.
    UseOC,
    /// `/UseAttachments` — attachments panel visible.
    UseAttachments,
}

/// `/Direction` — predominant reading order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReadingDirection {
    /// `/L2R` — left-to-right (default).
    L2R,
    /// `/R2L` — right-to-left, e.g. Arabic, Hebrew.
    R2L,
}

/// `/PrintScaling` — default print-scaling preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PrintScaling {
    /// `/None` — no scaling.
    None,
    /// `/AppDefault` — let the print application decide (default).
    AppDefault,
}

/// `/Duplex` — default print-duplex preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Duplex {
    /// `/Simplex` — single-sided.
    Simplex,
    /// `/DuplexFlipShortEdge` — duplex, flip on short edge.
    DuplexFlipShortEdge,
    /// `/DuplexFlipLongEdge` — duplex, flip on long edge.
    DuplexFlipLongEdge,
}

/// Parse `/ViewerPreferences`, `/PageLayout`, and `/PageMode` from the
/// document catalog.
///
/// Always returns a value — missing or malformed entries leave their
/// fields at default.
pub fn parse_viewer_preferences(resolver: &Resolver) -> ViewerPreferences {
    let mut prefs = ViewerPreferences::default();

    let Some(catalog) = catalog_dict(resolver) else {
        return prefs;
    };

    if let Some(name) = catalog.get_name(b"PageLayout")
        && let Some(layout) = parse_page_layout(name)
    {
        prefs.page_layout = layout;
    }
    if let Some(name) = catalog.get_name(b"PageMode")
        && let Some(mode) = parse_page_mode(name)
    {
        prefs.page_mode = mode;
    }

    if let Some(vp_obj) = catalog.get(b"ViewerPreferences")
        && let Ok(vp) = resolver.deref(vp_obj)
        && let Some(vp_dict) = vp.as_dict()
    {
        fill_viewer_prefs(&mut prefs, vp_dict);
    }

    prefs
}

fn fill_viewer_prefs(prefs: &mut ViewerPreferences, dict: &PdfDict) {
    if let Some(b) = dict.get(b"HideToolbar").and_then(as_bool) {
        prefs.hide_toolbar = b;
    }
    if let Some(b) = dict.get(b"HideMenubar").and_then(as_bool) {
        prefs.hide_menubar = b;
    }
    if let Some(b) = dict.get(b"HideWindowUI").and_then(as_bool) {
        prefs.hide_window_ui = b;
    }
    if let Some(b) = dict.get(b"FitWindow").and_then(as_bool) {
        prefs.fit_window = b;
    }
    if let Some(b) = dict.get(b"CenterWindow").and_then(as_bool) {
        prefs.center_window = b;
    }
    if let Some(b) = dict.get(b"DisplayDocTitle").and_then(as_bool) {
        prefs.display_doc_title = b;
    }
    if let Some(name) = dict.get_name(b"NonFullScreenPageMode")
        && let Some(m) = parse_page_mode(name)
    {
        prefs.non_full_screen_page_mode = m;
    }
    if let Some(name) = dict.get_name(b"Direction")
        && let Some(d) = parse_direction(name)
    {
        prefs.direction = d;
    }
    if let Some(name) = dict.get_name(b"PrintScaling")
        && let Some(p) = parse_print_scaling(name)
    {
        prefs.print_scaling = p;
    }
    if let Some(name) = dict.get_name(b"Duplex") {
        prefs.duplex = parse_duplex(name);
    }
    if let Some(b) = dict.get(b"PickTrayByPDFSize").and_then(as_bool) {
        prefs.pick_tray_by_pdf_size = Some(b);
    }
    if let Some(n) = dict.get_int(b"NumCopies")
        && (1..=10_000).contains(&n)
    {
        prefs.num_copies = Some(n as u32);
    }
}

fn as_bool(obj: &PdfObj) -> Option<bool> {
    match obj {
        PdfObj::Bool(b) => Some(*b),
        _ => None,
    }
}

fn parse_page_layout(name: &[u8]) -> Option<PageLayout> {
    match name {
        b"SinglePage" => Some(PageLayout::SinglePage),
        b"OneColumn" => Some(PageLayout::OneColumn),
        b"TwoColumnLeft" => Some(PageLayout::TwoColumnLeft),
        b"TwoColumnRight" => Some(PageLayout::TwoColumnRight),
        b"TwoPageLeft" => Some(PageLayout::TwoPageLeft),
        b"TwoPageRight" => Some(PageLayout::TwoPageRight),
        _ => None,
    }
}

fn parse_page_mode(name: &[u8]) -> Option<PageMode> {
    match name {
        b"UseNone" => Some(PageMode::UseNone),
        b"UseOutlines" => Some(PageMode::UseOutlines),
        b"UseThumbs" => Some(PageMode::UseThumbs),
        b"FullScreen" => Some(PageMode::FullScreen),
        b"UseOC" => Some(PageMode::UseOC),
        b"UseAttachments" => Some(PageMode::UseAttachments),
        _ => None,
    }
}

fn parse_direction(name: &[u8]) -> Option<ReadingDirection> {
    match name {
        b"L2R" => Some(ReadingDirection::L2R),
        b"R2L" => Some(ReadingDirection::R2L),
        _ => None,
    }
}

fn parse_print_scaling(name: &[u8]) -> Option<PrintScaling> {
    match name {
        b"None" => Some(PrintScaling::None),
        b"AppDefault" => Some(PrintScaling::AppDefault),
        _ => None,
    }
}

fn parse_duplex(name: &[u8]) -> Option<Duplex> {
    match name {
        b"Simplex" => Some(Duplex::Simplex),
        b"DuplexFlipShortEdge" => Some(Duplex::DuplexFlipShortEdge),
        b"DuplexFlipLongEdge" => Some(Duplex::DuplexFlipLongEdge),
        _ => None,
    }
}

fn catalog_dict(resolver: &Resolver) -> Option<PdfDict> {
    if let Some((num, gen_num)) = resolver.trailer().get_ref(b"Root")
        && let Ok(obj) = resolver.resolve(num, gen_num)
        && let Some(dict) = obj.as_dict()
        && (dict.get_name(b"Type") == Some(b"Catalog")
            || dict.get(b"Pages").is_some()
            || dict.get(b"PageLayout").is_some()
            || dict.get(b"PageMode").is_some()
            || dict.get(b"ViewerPreferences").is_some())
    {
        return Some(dict.clone());
    }
    crate::find_catalog(resolver).and_then(|obj| obj.as_dict().cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let p = ViewerPreferences::default();
        assert!(!p.hide_toolbar);
        assert_eq!(p.page_layout, PageLayout::SinglePage);
        assert_eq!(p.page_mode, PageMode::UseNone);
        assert_eq!(p.print_scaling, PrintScaling::AppDefault);
        assert!(p.duplex.is_none());
    }

    #[test]
    fn fill_from_dict_sets_overrides() {
        let mut dict = PdfDict::new();
        dict.insert(b"HideToolbar".to_vec(), PdfObj::Bool(true));
        dict.insert(b"FitWindow".to_vec(), PdfObj::Bool(true));
        dict.insert(b"PrintScaling".to_vec(), PdfObj::Name(b"None".to_vec()));
        dict.insert(
            b"Duplex".to_vec(),
            PdfObj::Name(b"DuplexFlipLongEdge".to_vec()),
        );
        dict.insert(b"NumCopies".to_vec(), PdfObj::Int(3));
        dict.insert(
            b"NonFullScreenPageMode".to_vec(),
            PdfObj::Name(b"UseOutlines".to_vec()),
        );

        let mut prefs = ViewerPreferences::default();
        fill_viewer_prefs(&mut prefs, &dict);

        assert!(prefs.hide_toolbar);
        assert!(prefs.fit_window);
        assert_eq!(prefs.print_scaling, PrintScaling::None);
        assert_eq!(prefs.duplex, Some(Duplex::DuplexFlipLongEdge));
        assert_eq!(prefs.num_copies, Some(3));
        assert_eq!(prefs.non_full_screen_page_mode, PageMode::UseOutlines);
    }

    #[test]
    fn unknown_name_leaves_default() {
        let mut dict = PdfDict::new();
        dict.insert(b"PrintScaling".to_vec(), PdfObj::Name(b"Bogus".to_vec()));
        let mut prefs = ViewerPreferences::default();
        fill_viewer_prefs(&mut prefs, &dict);
        assert_eq!(prefs.print_scaling, PrintScaling::AppDefault);
    }

    #[test]
    fn num_copies_out_of_range_ignored() {
        let mut dict = PdfDict::new();
        dict.insert(b"NumCopies".to_vec(), PdfObj::Int(0));
        let mut prefs = ViewerPreferences::default();
        fill_viewer_prefs(&mut prefs, &dict);
        assert!(prefs.num_copies.is_none());

        let mut dict = PdfDict::new();
        dict.insert(b"NumCopies".to_vec(), PdfObj::Int(99_999));
        let mut prefs = ViewerPreferences::default();
        fill_viewer_prefs(&mut prefs, &dict);
        assert!(prefs.num_copies.is_none());
    }

    #[test]
    fn page_layout_round_trip() {
        for (name, expect) in [
            (&b"SinglePage"[..], PageLayout::SinglePage),
            (&b"OneColumn"[..], PageLayout::OneColumn),
            (&b"TwoColumnLeft"[..], PageLayout::TwoColumnLeft),
            (&b"TwoPageRight"[..], PageLayout::TwoPageRight),
        ] {
            assert_eq!(parse_page_layout(name), Some(expect));
        }
        assert!(parse_page_layout(b"unknown").is_none());
    }
}
