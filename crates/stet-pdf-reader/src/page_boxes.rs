// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Per-page geometry: the five PDF page boxes plus rotation, user
//! unit, and presentation hints.
//!
//! [`PageBoxes`] exposes [`PageInfo`]'s already-resolved `MediaBox`
//! and `CropBox` plus the page-only `BleedBox`, `TrimBox`, `ArtBox`,
//! `UserUnit`, `Dur`, `Trans`, and `AA` entries.
//!
//! Per ISO 32000-2 §14.8.2, MediaBox and CropBox are inheritable from
//! the page tree; the bleed, trim, and art boxes are page-local.
//! `parse_page_boxes` reads PageInfo for the inheritable fields and
//! re-resolves the page dict for the page-local entries.

use crate::error::PdfError;
use crate::objects::PdfObj;
use crate::page_tree::{PageInfo, clamp_box_to_media, parse_rect};
use crate::resolver::Resolver;

/// Which area of a PDF page is rendered as the page.
///
/// Set with [`PdfDocument::set_page_area`]; [`PdfDocument::page_size`] and
/// [`PdfDocument::render_page`] (and everything built on them) then treat
/// that area as the page: its size is the page size, its lower-left corner
/// the origin, and `/Rotate` applies to it as it does to the crop box. For
/// placed artwork, render the box the layout cropped it to.
///
/// Resolution follows PDF 32000-1: an absent BleedBox, TrimBox or ArtBox is
/// the crop box (Table 30), and every area, [`Rect`](Self::Rect) included,
/// is reduced to its intersection with the MediaBox (§14.11.2). The crop
/// box is not a limit, so a bleed area outside it renders.
///
/// Marked `#[non_exhaustive]`: other areas may be added, so `match` on it
/// with a wildcard arm.
///
/// [`PdfDocument::set_page_area`]: crate::PdfDocument::set_page_area
/// [`PdfDocument::page_size`]: crate::PdfDocument::page_size
/// [`PdfDocument::render_page`]: crate::PdfDocument::render_page
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[non_exhaustive]
pub enum PageArea {
    /// The crop box: what a viewer shows. The default.
    #[default]
    CropBox,
    /// The media box: the whole physical medium.
    MediaBox,
    /// The bleed box: the crop box when the page declares none.
    BleedBox,
    /// The trim box: the finished page after trimming; the crop box when
    /// the page declares none.
    TrimBox,
    /// The art box: the extent of the page's meaningful content; the crop
    /// box when the page declares none.
    ArtBox,
    /// A rectangle `[llx, lly, urx, ury]` in the page's default user space:
    /// unrotated, in points, the space `/MediaBox` and `/ArtBox` are given
    /// in. Corners may come in either order.
    Rect([f64; 4]),
}

/// The rectangle `area` resolves to on the page `info`, in default user
/// space, normalised so `[0] < [2]` and `[1] < [3]`.
///
/// [`PageArea::CropBox`] returns the page's crop box exactly as the page
/// tree resolved it, so the default renders as it always has. Any other
/// area that is empty after the MediaBox intersection, or not finite, is
/// [`PdfError::EmptyPageArea`].
pub(crate) fn resolve_page_area(
    resolver: &Resolver,
    info: &PageInfo,
    page: usize,
    area: PageArea,
) -> Result<[f64; 4], PdfError> {
    let declared = |key: &[u8]| {
        resolver
            .resolve(info.obj_num, 0)
            .ok()
            .and_then(|obj| obj.as_dict().and_then(|d| parse_rect(d, key, resolver)))
    };
    let wanted = match area {
        PageArea::CropBox => return Ok(info.crop_box),
        PageArea::MediaBox => info.media_box,
        PageArea::BleedBox => declared(b"BleedBox").unwrap_or(info.crop_box),
        PageArea::TrimBox => declared(b"TrimBox").unwrap_or(info.crop_box),
        PageArea::ArtBox => declared(b"ArtBox").unwrap_or(info.crop_box),
        PageArea::Rect(rect) => rect,
    };
    // Check before clamping: `f64::min`/`max` drop a NaN, which would turn a
    // malformed rectangle into a plausible one.
    if !wanted.iter().all(|v| v.is_finite()) {
        return Err(PdfError::EmptyPageArea { page });
    }
    let [llx, lly, urx, ury] = clamp_box_to_media(&wanted, &info.media_box);
    if urx > llx && ury > lly {
        Ok([llx, lly, urx, ury])
    } else {
        Err(PdfError::EmptyPageArea { page })
    }
}

/// Page geometry and presentation hints, drawn from the page dict
/// plus the inherited MediaBox/CropBox already resolved on
/// [`PageInfo`].
///
/// The optional boxes are reported as declared, as `Option<[f64; 4]>`,
/// so callers can tell "explicitly set" from "spec default": `Some`
/// means the box was declared in the page dict (or inherited, for
/// `crop_box`); `None` means it was not. The spec defaults (PDF
/// 32000-1, Table 30) are: an absent `crop_box` is the `media_box`, and
/// an absent `bleed_box`, `trim_box` or `art_box` is the crop box. Every
/// box is also reduced to its intersection with the `media_box`
/// (§14.11.2). These values are raw: neither the defaults nor the
/// intersection are applied. [`PdfDocument::page_area_rect`] applies both.
///
/// [`PdfDocument::page_area_rect`]: crate::PdfDocument::page_area_rect
#[derive(Debug, Clone, PartialEq)]
pub struct PageBoxes {
    /// `/MediaBox` — required; defines the boundaries of the
    /// physical medium.
    pub media_box: [f64; 4],
    /// `/CropBox` — visible area; `None` means not declared (the
    /// default is `media_box`). Inherited from the page tree if present
    /// on a parent.
    pub crop_box: Option<[f64; 4]>,
    /// `/BleedBox` — bounds of the area within which page contents
    /// may bleed when output in production. `None`: the crop box.
    pub bleed_box: Option<[f64; 4]>,
    /// `/TrimBox` — intended dimensions of the finished page after
    /// trimming. `None`: the crop box.
    pub trim_box: Option<[f64; 4]>,
    /// `/ArtBox` — extent of the page's meaningful content. `None`:
    /// the crop box.
    pub art_box: Option<[f64; 4]>,
    /// `/Rotate` — clockwise rotation in degrees (multiple of 90).
    pub rotate: u16,
    /// `/UserUnit` — multiplier for default user-space units.
    /// Default `1.0` per spec.
    pub user_unit: f64,
    /// `/Dur` — page display duration for presentation mode.
    pub duration: Option<f64>,
    /// `/Trans` — page-transition dict presence.
    pub has_transition: bool,
    /// `/AA` — additional-actions dict presence.
    pub has_additional_actions: bool,
}

/// Read the page-box and presentation-hint entries for a page.
///
/// `pages` is the document's page list (usually `PdfDocument::pages()`);
/// `page_index` is the 0-based page number.
///
/// The inherited `MediaBox` and `CropBox` come from [`PageInfo`] (the
/// page-tree walker resolved inheritance at document-load time). The
/// page-local `BleedBox`, `TrimBox`, `ArtBox`, `UserUnit`, `Dur`,
/// `Trans`, and `AA` are read fresh from the page dict via the
/// resolver.
///
/// Returns `None` if `page_index` is out of range; otherwise always
/// returns a populated value (missing entries default per spec).
pub fn parse_page_boxes(
    resolver: &Resolver,
    pages: &[PageInfo],
    page_index: usize,
) -> Option<PageBoxes> {
    let info = pages.get(page_index)?;

    let mut boxes = PageBoxes {
        media_box: info.media_box,
        crop_box: None,
        bleed_box: None,
        trim_box: None,
        art_box: None,
        rotate: rotate_from_info(info.rotate),
        user_unit: 1.0,
        duration: None,
        has_transition: false,
        has_additional_actions: false,
    };

    // /CropBox: PageInfo resolves it via inheritance and falls back
    // to MediaBox. Distinguishing explicit-vs-default requires a
    // direct read of the page dict and walking parents for the
    // inheritance chain. For now, treat `info.crop_box != info.media_box`
    // as a strong proxy for "explicitly set"; a stricter check would
    // walk the page-tree chain for /CropBox presence. Real PDFs
    // typically set crop_box explicitly when they want it different,
    // so this approximation matches common behaviour.
    if info.crop_box != info.media_box {
        boxes.crop_box = Some(info.crop_box);
    }

    // Read the page dict for page-local entries.
    if let Ok(obj) = resolver.resolve(info.obj_num, 0)
        && let Some(dict) = obj.as_dict()
    {
        boxes.bleed_box = dict.get_array(b"BleedBox").and_then(parse_box);
        boxes.trim_box = dict.get_array(b"TrimBox").and_then(parse_box);
        boxes.art_box = dict.get_array(b"ArtBox").and_then(parse_box);
        // CropBox: prefer an explicit page-level entry over the
        // proxy guess above.
        if let Some(cb) = dict.get_array(b"CropBox").and_then(parse_box) {
            boxes.crop_box = Some(cb);
        }
        if let Some(uu) = dict.get_f64(b"UserUnit")
            && uu > 0.0
        {
            boxes.user_unit = uu;
        }
        boxes.duration = dict.get_f64(b"Dur");
        boxes.has_transition = dict.get(b"Trans").is_some();
        boxes.has_additional_actions = dict.get(b"AA").is_some();
    }

    Some(boxes)
}

fn parse_box(arr: &[PdfObj]) -> Option<[f64; 4]> {
    if arr.len() < 4 {
        return None;
    }
    Some([
        arr[0].as_f64()?,
        arr[1].as_f64()?,
        arr[2].as_f64()?,
        arr[3].as_f64()?,
    ])
}

fn rotate_from_info(rot: i32) -> u16 {
    let mut r = rot.rem_euclid(360);
    if r % 90 != 0 {
        r = 0;
    }
    r as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotate_normalizes_negative() {
        assert_eq!(rotate_from_info(-90), 270);
        assert_eq!(rotate_from_info(0), 0);
        assert_eq!(rotate_from_info(90), 90);
        assert_eq!(rotate_from_info(360), 0);
        assert_eq!(rotate_from_info(450), 90);
    }

    #[test]
    fn rotate_drops_non_multiples_of_90() {
        // PDFs sometimes ship invalid /Rotate values; we coerce to 0
        // rather than propagate.
        assert_eq!(rotate_from_info(45), 0);
        assert_eq!(rotate_from_info(180 + 1), 0);
    }

    #[test]
    fn parse_box_requires_four_numbers() {
        let arr = vec![PdfObj::Real(0.0), PdfObj::Real(0.0), PdfObj::Real(100.0)];
        assert!(parse_box(&arr).is_none());
        let arr = vec![
            PdfObj::Int(0),
            PdfObj::Int(0),
            PdfObj::Int(612),
            PdfObj::Int(792),
        ];
        assert_eq!(parse_box(&arr), Some([0.0, 0.0, 612.0, 792.0]));
    }

    /// `parse_box` should accept mixed Int/Real entries (PDFs often
    /// emit integer literals for whole-number coordinates).
    #[test]
    fn parse_box_mixed_int_real() {
        let arr = vec![
            PdfObj::Int(0),
            PdfObj::Real(0.5),
            PdfObj::Int(612),
            PdfObj::Real(792.0),
        ];
        assert_eq!(parse_box(&arr), Some([0.0, 0.5, 612.0, 792.0]));
    }

    #[test]
    fn parse_box_rejects_non_numeric() {
        let arr = vec![
            PdfObj::Int(0),
            PdfObj::Int(0),
            PdfObj::Name(b"oops".to_vec()),
            PdfObj::Int(792),
        ];
        // Silent failure for the third entry → None.
        assert!(parse_box(&arr).is_none());
    }
}
