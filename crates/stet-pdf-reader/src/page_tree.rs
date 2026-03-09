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
    let pages_dict = pages_obj.as_dict().ok_or(PdfError::Other(
        "catalog /Pages is not a dictionary".into(),
    ))?;

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
    }

    // Determine node type
    let type_name = node_dict.get_name(b"Type");

    if matches!(type_name, Some(b"Page")) || (type_name.is_none() && node_dict.get(b"Kids").is_none())
    {
        // Leaf page node
        let media_box = inherited
            .media_box
            .unwrap_or([0.0, 0.0, 612.0, 792.0]); // Default US Letter
        let crop_box = inherited.crop_box.unwrap_or(media_box);
        let rotate = inherited.rotate.unwrap_or(0);
        let resources = inherited.resources.clone().unwrap_or_default();

        // Parse /Contents
        let contents = parse_contents(node_dict)?;

        pages.push(PageInfo {
            obj_num,
            media_box,
            crop_box,
            rotate,
            resources,
            contents,
        });
    } else {
        // Intermediate /Pages node — recurse into /Kids
        if let Some(kids) = node_dict.get_array(b"Kids") {
            for kid in kids {
                match kid {
                    PdfObj::Ref(n, g) => {
                        let child = resolver.resolve(*n, *g)?;
                        if let Some(child_dict) = child.as_dict() {
                            collect_pages_recursive(
                                resolver,
                                child_dict,
                                *n,
                                &inherited,
                                pages,
                            )?;
                        }
                    }
                    _ => {
                        // Inline dict (unusual but possible)
                        if let Some(child_dict) = kid.as_dict() {
                            collect_pages_recursive(
                                resolver, child_dict, 0, &inherited, pages,
                            )?;
                        }
                    }
                }
            }
        }
    }

    Ok(())
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
fn parse_contents(dict: &PdfDict) -> Result<Vec<(u32, u16)>, PdfError> {
    match dict.get(b"Contents") {
        None => Ok(Vec::new()), // Blank page
        Some(PdfObj::Ref(n, g)) => Ok(vec![(*n, *g)]),
        Some(PdfObj::Array(arr)) => {
            let mut refs = Vec::new();
            for obj in arr {
                if let PdfObj::Ref(n, g) = obj {
                    refs.push((*n, *g));
                }
            }
            Ok(refs)
        }
        _ => Ok(Vec::new()),
    }
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
    fn parse_contents_single_ref() {
        let mut dict = PdfDict::new();
        dict.insert(b"Contents".to_vec(), PdfObj::Ref(5, 0));
        let refs = parse_contents(&dict).unwrap();
        assert_eq!(refs, vec![(5, 0)]);
    }

    #[test]
    fn parse_contents_array() {
        let mut dict = PdfDict::new();
        dict.insert(
            b"Contents".to_vec(),
            PdfObj::Array(vec![PdfObj::Ref(5, 0), PdfObj::Ref(6, 0)]),
        );
        let refs = parse_contents(&dict).unwrap();
        assert_eq!(refs, vec![(5, 0), (6, 0)]);
    }

    #[test]
    fn parse_contents_absent() {
        let dict = PdfDict::new();
        let refs = parse_contents(&dict).unwrap();
        assert!(refs.is_empty());
    }
}
