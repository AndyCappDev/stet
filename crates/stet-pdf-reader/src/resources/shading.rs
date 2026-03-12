// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shading (sh operator) → DisplayElement conversion.

use crate::content::color_space::{
    components_to_device_color_icc, resolve_color_space_obj, ResolvedColorSpace,
};
use crate::content::graphics_state::PdfGraphicsState;
use crate::error::PdfError;
use crate::objects::{PdfDict, PdfObj};
use crate::resolver::Resolver;
use crate::resources::function::PdfFunction;

use stet_core::device::{
    AxialShadingParams, ColorStop, ImageColorSpace, ImageParams, MeshShadingParams,
    PatchShadingParams, RadialShadingParams, ShadingColorSpace,
};
use stet_core::display_list::{DisplayElement, DisplayList};
use stet_core::graphics_state::Matrix;
use stet_core::icc::IccCache;

/// Handle the `sh` operator: parse shading dict and emit display element.
///
/// `shading_obj` is the original PdfObj (needed for stream access in types 4-7).
pub fn handle_shading(
    shading_obj: &PdfObj,
    dict: &PdfDict,
    gstate: &PdfGraphicsState,
    resolver: &Resolver,
    display_list: &mut DisplayList,
    icc_cache: &mut IccCache,
) -> Result<(), PdfError> {
    let shading_type =
        dict.get_int(b"ShadingType")
            .ok_or(PdfError::Other("shading missing ShadingType".into()))? as i32;

    let bbox = parse_bbox(dict);
    let extend = parse_extend(dict);

    // Resolve the color space once, used by all shading types
    let resolved_cs = resolve_shading_resolved_cs(dict, resolver);

    match shading_type {
        1 => handle_function_based(dict, gstate, resolver, display_list, &resolved_cs, icc_cache),
        2 => handle_axial(
            dict,
            gstate,
            resolver,
            display_list,
            bbox,
            extend,
            &resolved_cs,
            icc_cache,
        ),
        3 => handle_radial(
            dict,
            gstate,
            resolver,
            display_list,
            bbox,
            extend,
            &resolved_cs,
            icc_cache,
        ),
        4 | 5 => handle_mesh(
            shading_obj,
            dict,
            gstate,
            resolver,
            display_list,
            shading_type,
            &resolved_cs,
            icc_cache,
        ),
        6 | 7 => handle_patches(
            shading_obj,
            dict,
            gstate,
            resolver,
            display_list,
            shading_type,
            &resolved_cs,
            icc_cache,
        ),
        _ => Ok(()),
    }
}

fn handle_function_based(
    dict: &PdfDict,
    gstate: &PdfGraphicsState,
    resolver: &Resolver,
    display_list: &mut DisplayList,
    resolved_cs: &ResolvedColorSpace,
    icc_cache: &mut IccCache,
) -> Result<(), PdfError> {
    let function = parse_shading_function(dict, resolver)?;

    let domain = dict
        .get_array(b"Domain")
        .map(|a| {
            let v: Vec<f64> = a.iter().filter_map(|o| o.as_f64()).collect();
            if v.len() >= 4 {
                [v[0], v[1], v[2], v[3]]
            } else {
                [0.0, 1.0, 0.0, 1.0]
            }
        })
        .unwrap_or([0.0, 1.0, 0.0, 1.0]);

    let shading_matrix = dict
        .get_array(b"Matrix")
        .map(|a| {
            let v: Vec<f64> = a.iter().filter_map(|o| o.as_f64()).collect();
            if v.len() >= 6 {
                Matrix::new(v[0], v[1], v[2], v[3], v[4], v[5])
            } else {
                Matrix::identity()
            }
        })
        .unwrap_or_else(Matrix::identity);

    let width = 256u32;
    let height = 256u32;
    let mut rgba = vec![255u8; (width * height * 4) as usize];

    for row in 0..height {
        for col in 0..width {
            let x = domain[0] + (col as f64 + 0.5) / width as f64 * (domain[1] - domain[0]);
            let y = domain[2] + (row as f64 + 0.5) / height as f64 * (domain[3] - domain[2]);
            let components = function.evaluate(&[x, y]);
            let color =
                components_to_device_color_icc(resolved_cs, &components, Some(icc_cache));
            let idx = ((row * width + col) * 4) as usize;
            rgba[idx] = (color.r * 255.0 + 0.5) as u8;
            rgba[idx + 1] = (color.g * 255.0 + 0.5) as u8;
            rgba[idx + 2] = (color.b * 255.0 + 0.5) as u8;
        }
    }

    let domain_w = domain[1] - domain[0];
    let domain_h = domain[3] - domain[2];
    let domain_matrix = Matrix::new(domain_w, 0.0, 0.0, domain_h, domain[0], domain[2]);
    let combined = gstate.ctm.concat(&shading_matrix).concat(&domain_matrix);
    let image_matrix = Matrix::new(width as f64, 0.0, 0.0, -(height as f64), 0.0, height as f64);

    display_list.push(DisplayElement::Image {
        sample_data: rgba,
        params: ImageParams {
            width,
            height,
            color_space: ImageColorSpace::PreconvertedRGBA,
            ctm: combined,
            image_matrix,
            interpolate: true,
            mask_color: None,
            alpha: 1.0,
            blend_mode: 0,
        },
    });
    Ok(())
}

fn handle_axial(
    dict: &PdfDict,
    gstate: &PdfGraphicsState,
    resolver: &Resolver,
    display_list: &mut DisplayList,
    bbox: Option<[f64; 4]>,
    extend: (bool, bool),
    resolved_cs: &ResolvedColorSpace,
    icc_cache: &mut IccCache,
) -> Result<(), PdfError> {
    let coords = dict
        .get_array(b"Coords")
        .ok_or(PdfError::Other("axial shading missing Coords".into()))?;
    let vals: Vec<f64> = coords.iter().filter_map(|o| o.as_f64()).collect();
    if vals.len() < 4 {
        return Err(PdfError::Other("axial Coords needs 4 values".into()));
    }

    let function = parse_shading_function(dict, resolver)?;
    let color_stops = sample_function_to_stops_icc(&function, 64, resolved_cs, icc_cache);

    let (x0, y0) = gstate.ctm.transform_point(vals[0], vals[1]);
    let (x1, y1) = gstate.ctm.transform_point(vals[2], vals[3]);

    let cs = resolved_cs_to_shading_cs(resolved_cs);
    let device_bbox = transform_bbox(&bbox, &gstate.ctm);

    display_list.push(DisplayElement::AxialShading {
        params: AxialShadingParams {
            x0,
            y0,
            x1,
            y1,
            color_stops,
            extend_start: extend.0,
            extend_end: extend.1,
            ctm: Matrix::identity(),
            bbox: device_bbox,
            color_space: cs,
        },
    });
    Ok(())
}

fn handle_radial(
    dict: &PdfDict,
    gstate: &PdfGraphicsState,
    resolver: &Resolver,
    display_list: &mut DisplayList,
    bbox: Option<[f64; 4]>,
    extend: (bool, bool),
    resolved_cs: &ResolvedColorSpace,
    icc_cache: &mut IccCache,
) -> Result<(), PdfError> {
    let coords = dict
        .get_array(b"Coords")
        .ok_or(PdfError::Other("radial shading missing Coords".into()))?;
    let vals: Vec<f64> = coords.iter().filter_map(|o| o.as_f64()).collect();
    if vals.len() < 6 {
        return Err(PdfError::Other("radial Coords needs 6 values".into()));
    }

    let function = parse_shading_function(dict, resolver)?;
    let color_stops = sample_function_to_stops_icc(&function, 64, resolved_cs, icc_cache);

    // Keep coordinates in user space; pass the CTM to the renderer so it can
    // inverse-transform device pixels back to user space where circles are circular.
    // This correctly handles non-uniform scaling and shear (circles → ellipses).
    // BBox stays in user space too — the renderer transforms it via the CTM.
    let cs = resolved_cs_to_shading_cs(resolved_cs);

    display_list.push(DisplayElement::RadialShading {
        params: RadialShadingParams {
            x0: vals[0],
            y0: vals[1],
            r0: vals[2],
            x1: vals[3],
            y1: vals[4],
            r1: vals[5],
            color_stops,
            extend_start: extend.0,
            extend_end: extend.1,
            ctm: gstate.ctm,
            bbox,
            color_space: cs,
        },
    });
    Ok(())
}

fn handle_mesh(
    shading_obj: &PdfObj,
    dict: &PdfDict,
    gstate: &PdfGraphicsState,
    resolver: &Resolver,
    display_list: &mut DisplayList,
    shading_type: i32,
    resolved_cs: &ResolvedColorSpace,
    icc_cache: &mut IccCache,
) -> Result<(), PdfError> {
    let bpc = dict.get_int(b"BitsPerCoordinate").unwrap_or(8) as usize;
    let bpco = dict.get_int(b"BitsPerComponent").unwrap_or(8) as usize;
    let bpfl = dict.get_int(b"BitsPerFlag").unwrap_or(8) as usize;

    let decode = dict
        .get_array(b"Decode")
        .map(|a| a.iter().filter_map(|o| o.as_f64()).collect::<Vec<_>>())
        .unwrap_or_default();

    let cs = resolved_cs_to_shading_cs(resolved_cs);
    let n_comps = shading_cs_num_components(&cs);

    let data = resolver.stream_data_from_obj(shading_obj)?;

    let mut triangles = match shading_type {
        4 => stet_core::mesh_shading::parse_type4_mesh(&data, bpc, bpco, bpfl, &decode, n_comps),
        5 => {
            let vpr = dict.get_int(b"VerticesPerRow").unwrap_or(2) as usize;
            stet_core::mesh_shading::parse_type5_mesh(&data, bpc, bpco, &decode, n_comps, vpr)
        }
        _ => return Ok(()),
    };

    // Convert vertex colors through ICC profile
    for t in &mut triangles {
        t.v0.color = components_to_device_color_icc(resolved_cs, &t.v0.raw_components, Some(icc_cache));
        t.v1.color = components_to_device_color_icc(resolved_cs, &t.v1.raw_components, Some(icc_cache));
        t.v2.color = components_to_device_color_icc(resolved_cs, &t.v2.raw_components, Some(icc_cache));
    }

    // Transform vertices through CTM
    for t in &mut triangles {
        let (x, y) = gstate.ctm.transform_point(t.v0.x, t.v0.y);
        t.v0.x = x;
        t.v0.y = y;
        let (x, y) = gstate.ctm.transform_point(t.v1.x, t.v1.y);
        t.v1.x = x;
        t.v1.y = y;
        let (x, y) = gstate.ctm.transform_point(t.v2.x, t.v2.y);
        t.v2.x = x;
        t.v2.y = y;
    }

    let bbox = parse_bbox(dict);
    let device_bbox = transform_bbox(&bbox, &gstate.ctm);

    display_list.push(DisplayElement::MeshShading {
        params: MeshShadingParams {
            triangles,
            ctm: Matrix::identity(),
            bbox: device_bbox,
            color_space: cs,
        },
    });
    Ok(())
}

fn handle_patches(
    shading_obj: &PdfObj,
    dict: &PdfDict,
    gstate: &PdfGraphicsState,
    resolver: &Resolver,
    display_list: &mut DisplayList,
    shading_type: i32,
    resolved_cs: &ResolvedColorSpace,
    icc_cache: &mut IccCache,
) -> Result<(), PdfError> {
    let bpc = dict.get_int(b"BitsPerCoordinate").unwrap_or(8) as usize;
    let bpco = dict.get_int(b"BitsPerComponent").unwrap_or(8) as usize;
    let bpfl = dict.get_int(b"BitsPerFlag").unwrap_or(8) as usize;

    let decode = dict
        .get_array(b"Decode")
        .map(|a| a.iter().filter_map(|o| o.as_f64()).collect::<Vec<_>>())
        .unwrap_or_default();

    let cs = resolved_cs_to_shading_cs(resolved_cs);
    let n_comps = shading_cs_num_components(&cs);

    let data = resolver.stream_data_from_obj(shading_obj)?;

    let mut patches = match shading_type {
        6 => stet_core::mesh_shading::parse_type6_patches(&data, bpc, bpco, bpfl, &decode, n_comps),
        7 => stet_core::mesh_shading::parse_type7_patches(&data, bpc, bpco, bpfl, &decode, n_comps),
        _ => return Ok(()),
    };

    // Convert patch corner colors through ICC profile
    for p in &mut patches {
        for i in 0..4 {
            p.colors[i] = components_to_device_color_icc(resolved_cs, &p.raw_colors[i], Some(icc_cache));
        }
    }

    // Transform patch control points through CTM
    for p in &mut patches {
        for pt in &mut p.points {
            let (x, y) = gstate.ctm.transform_point(pt.0, pt.1);
            pt.0 = x;
            pt.1 = y;
        }
    }

    let bbox = parse_bbox(dict);
    let device_bbox = transform_bbox(&bbox, &gstate.ctm);

    display_list.push(DisplayElement::PatchShading {
        params: PatchShadingParams {
            patches,
            ctm: Matrix::identity(),
            bbox: device_bbox,
            color_space: cs,
        },
    });
    Ok(())
}

fn shading_cs_num_components(cs: &ShadingColorSpace) -> usize {
    match cs {
        ShadingColorSpace::DeviceGray => 1,
        ShadingColorSpace::DeviceRGB => 3,
        ShadingColorSpace::DeviceCMYK => 4,
        _ => 3,
    }
}

fn parse_shading_function(dict: &PdfDict, resolver: &Resolver) -> Result<PdfFunction, PdfError> {
    let fn_obj = dict
        .get(b"Function")
        .ok_or(PdfError::Other("shading missing Function".into()))?;
    let fn_obj = resolver.deref(fn_obj)?;
    if let PdfObj::Array(arr) = &fn_obj {
        if arr.len() == 1 {
            return PdfFunction::parse(&arr[0], resolver);
        }
        if let Some(first) = arr.first() {
            return PdfFunction::parse(first, resolver);
        }
    }
    PdfFunction::parse(&fn_obj, resolver)
}

fn sample_function_to_stops_icc(
    function: &PdfFunction,
    n_samples: usize,
    resolved_cs: &ResolvedColorSpace,
    icc_cache: &mut IccCache,
) -> Vec<ColorStop> {
    let mut stops = Vec::with_capacity(n_samples);
    for i in 0..n_samples {
        let t = i as f64 / (n_samples - 1) as f64;
        let components = function.evaluate(&[t]);
        let color =
            components_to_device_color_icc(resolved_cs, &components, Some(icc_cache));
        stops.push(ColorStop {
            position: t,
            color,
            raw_components: components,
        });
    }
    stops
}

/// Transform a user-space BBox to device space via CTM.
fn transform_bbox(bbox: &Option<[f64; 4]>, ctm: &Matrix) -> Option<[f64; 4]> {
    bbox.map(|b| {
        let corners = [
            ctm.transform_point(b[0], b[1]),
            ctm.transform_point(b[2], b[1]),
            ctm.transform_point(b[0], b[3]),
            ctm.transform_point(b[2], b[3]),
        ];
        let x_min = corners.iter().map(|c| c.0).fold(f64::INFINITY, f64::min);
        let y_min = corners.iter().map(|c| c.1).fold(f64::INFINITY, f64::min);
        let x_max = corners
            .iter()
            .map(|c| c.0)
            .fold(f64::NEG_INFINITY, f64::max);
        let y_max = corners
            .iter()
            .map(|c| c.1)
            .fold(f64::NEG_INFINITY, f64::max);
        [x_min, y_min, x_max, y_max]
    })
}

fn parse_bbox(dict: &PdfDict) -> Option<[f64; 4]> {
    dict.get_array(b"BBox").and_then(|arr| {
        let vals: Vec<f64> = arr.iter().filter_map(|o| o.as_f64()).collect();
        if vals.len() == 4 {
            Some([vals[0], vals[1], vals[2], vals[3]])
        } else {
            None
        }
    })
}

fn parse_extend(dict: &PdfDict) -> (bool, bool) {
    dict.get_array(b"Extend")
        .and_then(|arr| {
            if arr.len() == 2 {
                let a = matches!(arr[0], PdfObj::Bool(true));
                let b = matches!(arr[1], PdfObj::Bool(true));
                Some((a, b))
            } else {
                None
            }
        })
        .unwrap_or((false, false))
}

/// Resolve the shading's /ColorSpace to a ResolvedColorSpace.
fn resolve_shading_resolved_cs(dict: &PdfDict, resolver: &Resolver) -> ResolvedColorSpace {
    if let Some(cs_obj) = dict.get(b"ColorSpace") {
        if let Ok(resolved) = resolve_color_space_obj(cs_obj, resolver) {
            return resolved;
        }
    }
    ResolvedColorSpace::DeviceRGB
}

/// Convert ResolvedColorSpace to ShadingColorSpace for the display list.
/// ICCBased colors are already converted through the profile at stop/pixel level,
/// so we map them to the equivalent device space for the renderer.
fn resolved_cs_to_shading_cs(cs: &ResolvedColorSpace) -> ShadingColorSpace {
    match cs {
        ResolvedColorSpace::DeviceGray => ShadingColorSpace::DeviceGray,
        ResolvedColorSpace::DeviceRGB => ShadingColorSpace::DeviceRGB,
        ResolvedColorSpace::DeviceCMYK => ShadingColorSpace::DeviceCMYK,
        // ICCBased colors are converted to sRGB at sample time
        ResolvedColorSpace::ICCBased { n, .. } => match n {
            1 => ShadingColorSpace::DeviceGray,
            4 => ShadingColorSpace::DeviceCMYK,
            _ => ShadingColorSpace::DeviceRGB,
        },
        _ => ShadingColorSpace::DeviceRGB,
    }
}
