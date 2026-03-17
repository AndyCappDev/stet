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
    // Get /Root -> Catalog
    let root_ref = resolver
        .trailer()
        .get_ref(b"Root")
        .ok_or(PdfError::MissingKey("Root"))?;
    let catalog = resolver.resolve(root_ref.0, root_ref.1)?;
    let catalog_dict = catalog.as_dict().ok_or(PdfError::MalformedTrailer)?;

    // Get /Pages
    let pages_obj = catalog_dict
        .get(b"Pages")
        .ok_or(PdfError::MissingKey("Pages"))?;
    let pages_obj = resolver.deref(pages_obj)?;
    let pages_dict = pages_obj
        .as_dict()
        .ok_or(PdfError::Other("catalog /Pages is not a dictionary".into()))?;

    let mut pages = Vec::new();
    let inherited = Inherited::default();
    collect_pages_recursive(resolver, pages_dict, 0, &inherited, &mut pages)?;

    Ok(pages)
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
    if let Some(mb) = parse_rect(node_dict, b"MediaBox") {
        inherited.media_box = Some(mb);
    }
    if let Some(cb) = parse_rect(node_dict, b"CropBox") {
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
            && let Some(d) = resolved.as_dict() {
                inherited.resources = Some(d.clone());
            }
    }

    // Determine node type
    let type_name = node_dict.get_name(b"Type");

    if matches!(type_name, Some(b"Page"))
        || (type_name.is_none() && node_dict.get(b"Kids").is_none())
    {
        // Leaf page node
        let media_box = inherited.media_box.unwrap_or([0.0, 0.0, 612.0, 792.0]); // Default US Letter
        // CropBox defaults to MediaBox; clamp to MediaBox if it extends beyond
        // (per PDF spec: "should be equal to or smaller than the media box").
        let crop_box = clamp_box_to_media(
            &inherited.crop_box.unwrap_or(media_box),
            &media_box,
        );
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

/// Parse a rectangle array [llx, lly, urx, ury] from a dict key.
fn parse_rect(dict: &PdfDict, key: &[u8]) -> Option<[f64; 4]> {
    let arr = dict.get_array(key)?;
    if arr.len() >= 4 {
        Some([
            arr[0].as_f64()?,
            arr[1].as_f64()?,
            arr[2].as_f64()?,
            arr[3].as_f64()?,
        ])
    } else {
        None
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
                && let PdfObj::Array(arr) = &resolved {
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

    #[test]
    fn parse_rect_valid() {
        let mut dict = PdfDict::new();
        dict.insert(
            b"MediaBox".to_vec(),
            PdfObj::Array(vec![
                PdfObj::Int(0),
                PdfObj::Int(0),
                PdfObj::Real(612.0),
                PdfObj::Real(792.0),
            ]),
        );
        let rect = parse_rect(&dict, b"MediaBox").unwrap();
        assert_eq!(rect, [0.0, 0.0, 612.0, 792.0]);
    }

    #[test]
    fn parse_rect_missing() {
        let dict = PdfDict::new();
        assert!(parse_rect(&dict, b"MediaBox").is_none());
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
