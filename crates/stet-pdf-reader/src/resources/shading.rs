// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shading (sh operator) → DisplayElement conversion.

use crate::content::color_space::resolve_color_space_obj;
use crate::content::graphics_state::PdfGraphicsState;
use crate::error::PdfError;
use crate::objects::{PdfDict, PdfObj};
use crate::resolver::Resolver;
use crate::resources::function::PdfFunction;

use stet_core::device::{AxialShadingParams, ColorStop, RadialShadingParams, ShadingColorSpace};
use stet_core::display_list::{DisplayElement, DisplayList};
use stet_core::graphics_state::DeviceColor;

/// Handle the `sh` operator: parse shading dict and emit display element.
pub fn handle_shading(
    dict: &PdfDict,
    gstate: &PdfGraphicsState,
    resolver: &Resolver,
    display_list: &mut DisplayList,
) -> Result<(), PdfError> {
    let shading_type =
        dict.get_int(b"ShadingType")
            .ok_or(PdfError::Other("shading missing ShadingType".into()))? as i32;

    let bbox = parse_bbox(dict);
    let extend = parse_extend(dict);

    match shading_type {
        2 => handle_axial(dict, gstate, resolver, display_list, bbox, extend),
        3 => handle_radial(dict, gstate, resolver, display_list, bbox, extend),
        // Types 1, 4-7 would go here
        _ => Ok(()), // Unsupported shading type — silently ignore
    }
}

fn handle_axial(
    dict: &PdfDict,
    gstate: &PdfGraphicsState,
    resolver: &Resolver,
    display_list: &mut DisplayList,
    bbox: Option<[f64; 4]>,
    extend: (bool, bool),
) -> Result<(), PdfError> {
    let coords = dict
        .get_array(b"Coords")
        .ok_or(PdfError::Other("axial shading missing Coords".into()))?;
    let vals: Vec<f64> = coords.iter().filter_map(|o| o.as_f64()).collect();
    if vals.len() < 4 {
        return Err(PdfError::Other("axial Coords needs 4 values".into()));
    }

    let function = parse_shading_function(dict, resolver)?;
    let color_stops = sample_function_to_stops(&function, 64);

    // Transform coords through CTM
    let (x0, y0) = gstate.ctm.transform_point(vals[0], vals[1]);
    let (x1, y1) = gstate.ctm.transform_point(vals[2], vals[3]);

    let cs = resolve_shading_color_space(dict, resolver);

    display_list.push(DisplayElement::AxialShading {
        params: AxialShadingParams {
            x0,
            y0,
            x1,
            y1,
            color_stops,
            extend_start: extend.0,
            extend_end: extend.1,
            ctm: stet_core::graphics_state::Matrix::identity(),
            bbox,
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
) -> Result<(), PdfError> {
    let coords = dict
        .get_array(b"Coords")
        .ok_or(PdfError::Other("radial shading missing Coords".into()))?;
    let vals: Vec<f64> = coords.iter().filter_map(|o| o.as_f64()).collect();
    if vals.len() < 6 {
        return Err(PdfError::Other("radial Coords needs 6 values".into()));
    }

    let function = parse_shading_function(dict, resolver)?;
    let color_stops = sample_function_to_stops(&function, 64);

    let (x0, y0) = gstate.ctm.transform_point(vals[0], vals[1]);
    let (x1, y1) = gstate.ctm.transform_point(vals[3], vals[4]);
    let scale = gstate.ctm_scale_factor();
    let r0 = vals[2] * scale;
    let r1 = vals[5] * scale;

    let cs = resolve_shading_color_space(dict, resolver);

    display_list.push(DisplayElement::RadialShading {
        params: RadialShadingParams {
            x0,
            y0,
            r0,
            x1,
            y1,
            r1,
            color_stops,
            extend_start: extend.0,
            extend_end: extend.1,
            ctm: stet_core::graphics_state::Matrix::identity(),
            bbox,
            color_space: cs,
        },
    });
    Ok(())
}

fn parse_shading_function(dict: &PdfDict, resolver: &Resolver) -> Result<PdfFunction, PdfError> {
    let fn_obj = dict
        .get(b"Function")
        .ok_or(PdfError::Other("shading missing Function".into()))?;
    // Function can be an array of functions or a single function
    let fn_obj = resolver.deref(fn_obj)?;
    if let PdfObj::Array(arr) = &fn_obj {
        if arr.len() == 1 {
            return PdfFunction::parse(&arr[0], resolver);
        }
        // Multiple functions: stitch them
        // For now, use just the first one
        if let Some(first) = arr.first() {
            return PdfFunction::parse(first, resolver);
        }
    }
    PdfFunction::parse(&fn_obj, resolver)
}

fn sample_function_to_stops(function: &PdfFunction, n_samples: usize) -> Vec<ColorStop> {
    let mut stops = Vec::with_capacity(n_samples);
    for i in 0..n_samples {
        let t = i as f64 / (n_samples - 1) as f64;
        let components = function.evaluate(&[t]);
        let color = match components.len() {
            1 => DeviceColor::from_gray(components[0]),
            3 => DeviceColor::from_rgb(components[0], components[1], components[2]),
            4 => DeviceColor::from_cmyk(components[0], components[1], components[2], components[3]),
            _ => DeviceColor::from_gray(0.0),
        };
        stops.push(ColorStop {
            position: t,
            color,
            raw_components: components,
        });
    }
    stops
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

fn resolve_shading_color_space(dict: &PdfDict, resolver: &Resolver) -> ShadingColorSpace {
    if let Some(cs_obj) = dict.get(b"ColorSpace")
        && let Ok(resolved) = resolve_color_space_obj(cs_obj, resolver)
    {
        return match resolved {
            crate::content::color_space::ResolvedColorSpace::DeviceGray => {
                ShadingColorSpace::DeviceGray
            }
            crate::content::color_space::ResolvedColorSpace::DeviceRGB => {
                ShadingColorSpace::DeviceRGB
            }
            crate::content::color_space::ResolvedColorSpace::DeviceCMYK => {
                ShadingColorSpace::DeviceCMYK
            }
            _ => ShadingColorSpace::DeviceRGB,
        };
    }
    ShadingColorSpace::DeviceRGB
}
