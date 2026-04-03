// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF page tree traversal with attribute inheritance.

use crate::error::PdfError;
use crate::objects::{PdfDict, PdfObj};
use crate::resolver::Resolver;

/// Resolved page information (after inheritance).
#[derive(Debug, Clone)]
pub struct PageInfo {
    /// Page object number.
    pub obj_num: u32,
    /// MediaBox [llx, lly, urx, ury] in points.
    pub media_box: [f64; 4],
    /// CropBox (defaults to MediaBox if absent).
    pub crop_box: [f64; 4],
    /// Rotation in degrees (0, 90, 180, 270).
    pub rotate: i32,
    /// Resources dictionary (may be inherited).
    pub resources: PdfDict,
    /// Content stream references.
    pub contents: Vec<(u32, u16)>,
    /// Annotation references (obj_num, gen_num).
    pub annots: Vec<(u32, u16)>,
}

/// Inherited attributes propagated down the page tree.
#[derive(Clone, Default)]
struct Inherited {
    media_box: Option<[f64; 4]>,
    crop_box: Option<[f64; 4]>,
    rotate: Option<i32>,
    resources: Option<PdfDict>,
}

/// Traverse the page tree and collect all leaf pages in order.
pub fn collect_pages(resolver: &Resolver) -> Result<Vec<PageInfo>, PdfError> {
    // Get /Root -> Catalog (may be an indirect reference or an inline dict)
    let catalog_owned;
    let catalog_dict = if let Some(root_ref) = resolver.trailer().get_ref(b"Root") {
        let catalog = match resolver.resolve(root_ref.0, root_ref.1) {
            Ok(c) => c,
            Err(_) => return collect_pages_by_scan(resolver),
        };
        catalog_owned = catalog;
        match catalog_owned.as_dict() {
            Some(d) => d,
            None => return collect_pages_by_scan(resolver),
        }
    } else if let Some(root_obj) = resolver.trailer().get(b"Root") {
        match root_obj.as_dict() {
            Some(d) => d,
            None => return collect_pages_by_scan(resolver),
        }
    } else {
        return collect_pages_by_scan(resolver);
    };

    // Get /Pages — if the page tree root is missing (truncated PDF),
    // fall back to scanning all objects for /Type /Page entries.
    let pages_ref = catalog_dict
        .get(b"Pages")
        .ok_or(PdfError::MissingKey("Pages"))?;
    let pages_obj = match resolver.deref(pages_ref) {
        Ok(obj) => obj,
        Err(_) => return collect_pages_by_scan(resolver),
    };
    let pages_dict = match pages_obj.as_dict() {
        Some(d) => d,
        None => return collect_pages_by_scan(resolver),
    };

    let mut pages = Vec::new();
    let inherited = Inherited::default();
    collect_pages_recursive(resolver, pages_dict, 0, &inherited, &mut pages)?;

    Ok(pages)
}

/// Fallback: scan all xref entries for `/Type /Page` objects.
/// Used when the page tree root is missing (e.g., truncated PDF).
fn collect_pages_by_scan(resolver: &Resolver) -> Result<Vec<PageInfo>, PdfError> {
    let mut pages = Vec::new();
    let xref_len = resolver.xref_len();

    for obj_num in 0..xref_len as u32 {
        if let Ok(obj) = resolver.resolve(obj_num, 0) {
            if let Some(dict) = obj.as_dict() {
                if dict.get_name(b"Type") == Some(b"Page") && dict.get(b"Kids").is_none() {
                    let media_box = parse_rect(dict, b"MediaBox", resolver)
                        .unwrap_or([0.0, 0.0, 612.0, 792.0]);
                    let crop_box = clamp_box_to_media(
                        &parse_rect(dict, b"CropBox", resolver).unwrap_or(media_box),
                        &media_box,
                    );
                    let rotate = dict.get_int(b"Rotate").unwrap_or(0) as i32;
                    let resources = resolve_resources(dict, resolver);
                    let contents = parse_contents(dict, resolver)?;
                    let annots = parse_annots(dict, resolver);

                    pages.push(PageInfo {
                        obj_num,
                        media_box,
                        crop_box,
                        rotate,
                        resources,
                        contents,
                        annots,
                    });
                }
            }
        }
    }

    if pages.is_empty() {
        return Err(PdfError::MissingKey("Pages"));
    }

    Ok(pages)
}

/// Resolve /Resources from a dict (direct or indirect ref).
fn resolve_resources(dict: &PdfDict, resolver: &Resolver) -> PdfDict {
    if let Some(res) = dict.get_dict(b"Resources") {
        return res.clone();
    }
    if let Some(PdfObj::Ref(n, g)) = dict.get(b"Resources") {
        if let Ok(resolved) = resolver.resolve(*n, *g) {
            if let Some(d) = resolved.as_dict() {
                return d.clone();
            }
        }
    }
    PdfDict::default()
}

fn collect_pages_recursive(
    resolver: &Resolver,
    node_dict: &PdfDict,
    obj_num: u32,
    parent_inherited: &Inherited,
    pages: &mut Vec<PageInfo>,
) -> Result<(), PdfError> {
    // Update inherited attributes from this node
    let mut inherited = parent_inherited.clone();
    if let Some(mb) = parse_rect(node_dict, b"MediaBox", resolver) {
        inherited.media_box = Some(mb);
    }
    if let Some(cb) = parse_rect(node_dict, b"CropBox", resolver) {
        inherited.crop_box = Some(cb);
    }
    if let Some(r) = node_dict.get_int(b"Rotate") {
        inherited.rotate = Some(r as i32);
    }
    if let Some(res) = node_dict.get_dict(b"Resources") {
        inherited.resources = Some(res.clone());
    } else if let Some(PdfObj::Ref(n, g)) = node_dict.get(b"Resources") {
        // /Resources may be an indirect reference — dereference it
        if let Ok(resolved) = resolver.resolve(*n, *g)
            && let Some(d) = resolved.as_dict()
        {
            inherited.resources = Some(d.clone());
        }
    }

    // Determine node type.
    // Use /Kids presence as the definitive indicator of an intermediate node —
    // some malformed PDFs have duplicate /Type keys (both /Pages and /Page)
    // in the same dict, where "last wins" parsing produces /Type /Page even
    // though the node is clearly an intermediate Pages node with /Kids.
    let has_kids = node_dict.get(b"Kids").is_some();
    let type_name = node_dict.get_name(b"Type");

    if !has_kids
        && (matches!(type_name, Some(b"Page"))
            || (type_name.is_none() && !has_kids))
    {
        // Leaf page node
        let media_box = inherited.media_box.unwrap_or([0.0, 0.0, 612.0, 792.0]); // Default US Letter
        // CropBox defaults to MediaBox; clamp to MediaBox if it extends beyond
        // (per PDF spec: "should be equal to or smaller than the media box").
        let crop_box = clamp_box_to_media(&inherited.crop_box.unwrap_or(media_box), &media_box);
        let rotate = inherited.rotate.unwrap_or(0);
        let resources = inherited.resources.clone().unwrap_or_default();

        // Parse /Contents (may be a ref to an array, not just a direct array)
        let contents = parse_contents(node_dict, resolver)?;

        // Parse /Annots (annotation references)
        let annots = parse_annots(node_dict, resolver);

        pages.push(PageInfo {
            obj_num,
            media_box,
            crop_box,
            rotate,
            resources,
            contents,
            annots,
        });
    } else {
        // Intermediate /Pages node — recurse into /Kids.
        // /Kids may be a direct array or an indirect reference to one.
        let kids_owned;
        let kids: &[PdfObj] = if let Some(arr) = node_dict.get_array(b"Kids") {
            arr
        } else if let Some(PdfObj::Ref(n, g)) = node_dict.get(b"Kids") {
            kids_owned = match resolver.resolve(*n, *g) {
                Ok(PdfObj::Array(arr)) => arr,
                _ => Vec::new(),
            };
            &kids_owned
        } else {
            &[]
        };
        for kid in kids {
            match kid {
                PdfObj::Ref(n, g) => {
                    let child = resolver.resolve(*n, *g)?;
                    if let Some(child_dict) = child.as_dict() {
                        collect_pages_recursive(resolver, child_dict, *n, &inherited, pages)?;
                    }
                }
                _ => {
                    // Inline dict (unusual but possible)
                    if let Some(child_dict) = kid.as_dict() {
                        collect_pages_recursive(resolver, child_dict, 0, &inherited, pages)?;
                    }
                }
            }
        }
    }

    Ok(())
}

/// Clamp a box to fit within the media box (intersection).
fn clamp_box_to_media(crop: &[f64; 4], media: &[f64; 4]) -> [f64; 4] {
    // Normalize both boxes so [0]<[2] and [1]<[3]
    let (c_llx, c_urx) = (crop[0].min(crop[2]), crop[0].max(crop[2]));
    let (c_lly, c_ury) = (crop[1].min(crop[3]), crop[1].max(crop[3]));
    let (m_llx, m_urx) = (media[0].min(media[2]), media[0].max(media[2]));
    let (m_lly, m_ury) = (media[1].min(media[3]), media[1].max(media[3]));
    [
        c_llx.max(m_llx),
        c_lly.max(m_lly),
        c_urx.min(m_urx),
        c_ury.min(m_ury),
    ]
}

/// Convert an array of PdfObj values to a rectangle [llx, lly, urx, ury].
fn arr_to_rect(arr: &[PdfObj], resolver: &Resolver) -> Option<[f64; 4]> {
    if arr.len() >= 4 {
        // Array elements may be indirect references (e.g. `4 0 R`)
        let resolve = |obj: &PdfObj| -> Option<f64> {
            obj.as_f64()
                .or_else(|| resolver.deref(obj).ok().and_then(|r| r.as_f64()))
        };
        Some([
            resolve(&arr[0])?,
            resolve(&arr[1])?,
            resolve(&arr[2])?,
            resolve(&arr[3])?,
        ])
    } else {
        None
    }
}

/// Parse a rectangle array [llx, lly, urx, ury] from a dict key.
/// Handles both direct arrays and indirect references to arrays.
fn parse_rect(dict: &PdfDict, key: &[u8], resolver: &Resolver) -> Option<[f64; 4]> {
    match dict.get(key)? {
        PdfObj::Array(a) => arr_to_rect(a, resolver),
        PdfObj::Ref(n, g) => match resolver.resolve(*n, *g).ok()? {
            PdfObj::Array(a) => arr_to_rect(&a, resolver),
            _ => None,
        },
        _ => None,
    }
}

/// Parse /Contents as a list of indirect references.
/// /Contents can be a single stream ref, a direct array of refs,
/// or an indirect ref to an array of refs (e.g., in linearized PDFs
/// where the array is stored in an object stream).
fn parse_contents(dict: &PdfDict, resolver: &Resolver) -> Result<Vec<(u32, u16)>, PdfError> {
    match dict.get(b"Contents") {
        None => Ok(Vec::new()), // Blank page
        Some(PdfObj::Ref(n, g)) => {
            // Could be a ref to a stream OR a ref to an array of refs.
            // Try resolving to check.
            if let Ok(resolved) = resolver.resolve(*n, *g)
                && let PdfObj::Array(arr) = &resolved
            {
                return collect_refs_from_array(arr);
            }
            // Single content stream reference
            Ok(vec![(*n, *g)])
        }
        Some(PdfObj::Array(arr)) => collect_refs_from_array(arr),
        _ => Ok(Vec::new()),
    }
}

/// Parse /Annots as a list of indirect references.
fn parse_annots(dict: &PdfDict, resolver: &Resolver) -> Vec<(u32, u16)> {
    let annot_arr = match dict.get(b"Annots") {
        Some(PdfObj::Array(arr)) => arr.clone(),
        Some(PdfObj::Ref(n, g)) => {
            // Indirect ref to array
            if let Ok(PdfObj::Array(arr)) = resolver.resolve(*n, *g) {
                arr
            } else {
                return Vec::new();
            }
        }
        _ => return Vec::new(),
    };

    let mut refs = Vec::new();
    for obj in &annot_arr {
        if let PdfObj::Ref(n, g) = obj {
            refs.push((*n, *g));
        }
    }
    refs
}

/// Extract (obj_num, gen_num) pairs from an array of Ref objects.
fn collect_refs_from_array(arr: &[PdfObj]) -> Result<Vec<(u32, u16)>, PdfError> {
    let mut refs = Vec::new();
    for obj in arr {
        if let PdfObj::Ref(n, g) = obj {
            refs.push((*n, *g));
        }
    }
    Ok(refs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xref::XrefTable;

    /// Create a dummy resolver for tests that only use direct values.
    fn dummy_resolver() -> Resolver<'static> {
        static EMPTY: &[u8] = b"";
        Resolver::new(EMPTY, &XrefTable::empty())
    }

    #[test]
    fn arr_to_rect_valid() {
        let r = dummy_resolver();
        let arr = vec![
            PdfObj::Int(0),
            PdfObj::Int(0),
            PdfObj::Real(612.0),
            PdfObj::Real(792.0),
        ];
        let rect = arr_to_rect(&arr, &r).unwrap();
        assert_eq!(rect, [0.0, 0.0, 612.0, 792.0]);
    }

    #[test]
    fn arr_to_rect_too_short() {
        let r = dummy_resolver();
        let arr = vec![PdfObj::Int(0), PdfObj::Int(0)];
        assert!(arr_to_rect(&arr, &r).is_none());
    }

    #[test]
    fn collect_refs_from_array_basic() {
        let arr = vec![PdfObj::Ref(5, 0), PdfObj::Ref(6, 0)];
        let refs = collect_refs_from_array(&arr).unwrap();
        assert_eq!(refs, vec![(5, 0), (6, 0)]);
    }

    #[test]
    fn collect_refs_from_array_empty() {
        let refs = collect_refs_from_array(&[]).unwrap();
        assert!(refs.is_empty());
    }
}
