// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shading (sh operator) → DisplayElement conversion.

use crate::content::color_space::{
    ResolvedColorSpace, components_to_device_color_icc, painted_channels_for_cs,
    resolve_color_space_obj,
};
use crate::content::graphics_state::PdfGraphicsState;
use crate::error::PdfError;
use crate::objects::{PdfDict, PdfObj};
use crate::resolver::Resolver;
use crate::resources::function::PdfFunction;
use std::sync::Arc;

use stet_fonts::geometry::Matrix;
use stet_graphics::device::{
    AxialShadingParams, ColorStop, ImageColorSpace, ImageParams, MeshShadingParams,
    PatchShadingParams, RadialShadingParams, ShadingColorSpace,
};
use stet_graphics::display_list::{DisplayElement, DisplayList};
use stet_graphics::icc::IccCache;

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

    // Background color: fill the entire paint area before the gradient (PDF spec 8.7.4.5.2).
    // The caller clips the shading to the fill path, so a large rect is fine.
    if let Some(bg_arr) = dict.get_array(b"Background") {
        let comps: Vec<f64> = bg_arr.iter().filter_map(|o| o.as_f64()).collect();
        let bg_color = components_to_device_color_icc(&resolved_cs, &comps, Some(icc_cache));
        let mut params = gstate.fill_params(stet_graphics::color::FillRule::NonZeroWinding);
        params.color = bg_color;
        // Large rect in device space — the shading's clip constrains it
        let mut path = stet_fonts::geometry::PsPath::new();
        path.segments.push(stet_fonts::geometry::PathSegment::MoveTo(-1e6, -1e6));
        path.segments.push(stet_fonts::geometry::PathSegment::LineTo(1e6, -1e6));
        path.segments.push(stet_fonts::geometry::PathSegment::LineTo(1e6, 1e6));
        path.segments.push(stet_fonts::geometry::PathSegment::LineTo(-1e6, 1e6));
        path.segments.push(stet_fonts::geometry::PathSegment::ClosePath);
        display_list.push(DisplayElement::Fill { path, params });
    }

    match shading_type {
        1 => handle_function_based(
            dict,
            gstate,
            resolver,
            display_list,
            &resolved_cs,
            icc_cache,
        ),
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

    let domain_w = domain[1] - domain[0];
    let domain_h = domain[3] - domain[2];
    let domain_matrix = Matrix::new(domain_w, 0.0, 0.0, domain_h, domain[0], domain[2]);
    let combined = gstate.ctm.concat(&shading_matrix).concat(&domain_matrix);

    // Compute rasterization resolution from device-space dimensions.
    // The combined matrix column vectors give the device extent of the
    // unit square.  Match that so each rasterized pixel ≈ 1 device pixel.
    let dev_w = (combined.a * combined.a + combined.b * combined.b).sqrt();
    let dev_h = (combined.c * combined.c + combined.d * combined.d).sqrt();
    let width = (dev_w.ceil() as u32).clamp(2, 2048);
    let height = (dev_h.ceil() as u32).clamp(2, 2048);

    let mut rgba = vec![255u8; (width * height * 4) as usize];

    for row in 0..height {
        for col in 0..width {
            let x = domain[0] + (col as f64 + 0.5) / width as f64 * (domain[1] - domain[0]);
            let y = domain[3] - (row as f64 + 0.5) / height as f64 * (domain[3] - domain[2]);
            let components = function.evaluate(&[x, y]);
            let color = components_to_device_color_icc(resolved_cs, &components, Some(icc_cache));
            let idx = ((row * width + col) * 4) as usize;
            rgba[idx] = (color.r * 255.0 + 0.5) as u8;
            rgba[idx + 1] = (color.g * 255.0 + 0.5) as u8;
            rgba[idx + 2] = (color.b * 255.0 + 0.5) as u8;
        }
    }

    let image_matrix = Matrix::new(width as f64, 0.0, 0.0, -(height as f64), 0.0, height as f64);

    display_list.push(DisplayElement::Image {
        sample_data: rgba,
        params: ImageParams {
            width,
            height,
            color_space: ImageColorSpace::PreconvertedRGBA,
            bits_per_component: 8,
            ctm: combined,
            image_matrix,
            interpolate: true,
            mask_color: None,
            alpha: 1.0,
            blend_mode: 0,
            overprint: false,
            overprint_mode: 0,
            painted_channels: 0,
        },
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
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

    // Keep coordinates in shading/user space, pass the CTM to the renderer.
    // The renderer inverse-transforms device pixels to evaluate the gradient,
    // correctly handling non-uniform scaling, rotation, and Y-flips.
    let cs = resolved_cs_to_shading_cs(resolved_cs);

    display_list.push(DisplayElement::AxialShading {
        params: AxialShadingParams {
            x0: vals[0],
            y0: vals[1],
            x1: vals[2],
            y1: vals[3],
            color_stops,
            extend_start: extend.0,
            extend_end: extend.1,
            ctm: gstate.ctm,
            bbox,
            color_space: cs,
            overprint: gstate.overprint,
            painted_channels: painted_channels_for_cs(resolved_cs),
        },
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
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
            overprint: gstate.overprint,
            painted_channels: painted_channels_for_cs(resolved_cs),
        },
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
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
    // Use the resolved color space's component count for parsing vertex data.
    // For Indexed this is 1 (the palette index), even though the shading CS
    // (after palette expansion) may have 3 or 4 components.
    let cs_comps = resolved_cs.num_components();

    // When a Function is present, vertex data has fewer color components per vertex —
    // the function's input dimension, not the color space dimension. Parse with the
    // function input count, then apply the function to expand to full color values.
    let function = if dict.get(b"Function").is_some() {
        parse_shading_function(dict, resolver).ok()
    } else {
        None
    };
    let n_comps = if function.is_some() {
        // Function input dimension: inferred from Decode array (entries beyond the 4
        // coordinate entries, each pair is one component)
        let color_entries = decode.len().saturating_sub(4);
        (color_entries / 2).max(1)
    } else {
        cs_comps
    };

    let data = resolver.stream_data_from_obj(shading_obj)?;

    let mut triangles = match shading_type {
        4 => {
            stet_graphics::mesh_shading::parse_type4_mesh(&data, bpc, bpco, bpfl, &decode, n_comps)
        }
        5 => {
            let vpr = dict.get_int(b"VerticesPerRow").unwrap_or(2) as usize;
            stet_graphics::mesh_shading::parse_type5_mesh(&data, bpc, bpco, &decode, n_comps, vpr)
        }
        _ => return Ok(()),
    };

    // Build per-pixel color LUT for single-input function-based meshes.
    // PDF spec says vertex colors are linearly interpolated, but for non-linear
    // functions (e.g., stitching with thresholds), interpolating raw function
    // inputs per-pixel then applying the function produces correct results.
    let color_lut = if let Some(ref func) = function {
        if n_comps == 1 {
            // Get the color decode range (the last pair in the Decode array)
            let d_min = decode.get(4).copied().unwrap_or(0.0);
            let d_max = decode.get(5).copied().unwrap_or(1.0);
            let d_range = (d_max - d_min).abs().max(1e-10);

            // Sample the function at 256 evenly-spaced points
            let lut_size = 256;
            let mut lut = Vec::with_capacity(lut_size);
            for i in 0..lut_size {
                let t = i as f64 / (lut_size - 1) as f64;
                let input = d_min + t * (d_max - d_min);
                let components = func.evaluate(&[input]);
                let color =
                    components_to_device_color_icc(resolved_cs, &components, Some(icc_cache));
                lut.push(color);
            }

            // Normalize vertex raw values to [0, 1] for LUT indexing
            for t in &mut triangles {
                for v in [&mut t.v0, &mut t.v1, &mut t.v2] {
                    let raw = v.raw_components[0];
                    let normalized = ((raw - d_min) / d_range).clamp(0.0, 1.0);
                    v.raw_components = vec![normalized];
                }
            }

            Some(std::sync::Arc::new(lut))
        } else {
            None
        }
    } else {
        None
    };

    // Apply shading function to expand vertex colors (for vertex DeviceColor
    // and for renderers that don't use the LUT path)
    if let Some(ref func) = function {
        if color_lut.is_some() {
            // LUT path: evaluate function at each vertex's normalized raw value
            // to populate vertex colors (needed by PDF output device)
            let d_min = decode.get(4).copied().unwrap_or(0.0);
            let d_max = decode.get(5).copied().unwrap_or(1.0);
            for t in &mut triangles {
                for v in [&mut t.v0, &mut t.v1, &mut t.v2] {
                    let input = d_min + v.raw_components[0] * (d_max - d_min);
                    let expanded = func.evaluate(&[input]);
                    let color =
                        components_to_device_color_icc(resolved_cs, &expanded, Some(icc_cache));
                    v.color = color;
                }
            }
        } else {
            for t in &mut triangles {
                t.v0.raw_components = func.evaluate(&t.v0.raw_components);
                t.v1.raw_components = func.evaluate(&t.v1.raw_components);
                t.v2.raw_components = func.evaluate(&t.v2.raw_components);
            }
        }
    }

    if color_lut.is_none() {
        // Convert vertex colors through ICC profile (non-LUT path)
        for t in &mut triangles {
            t.v0.color =
                components_to_device_color_icc(resolved_cs, &t.v0.raw_components, Some(icc_cache));
            t.v1.color =
                components_to_device_color_icc(resolved_cs, &t.v1.raw_components, Some(icc_cache));
            t.v2.color =
                components_to_device_color_icc(resolved_cs, &t.v2.raw_components, Some(icc_cache));
        }
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
            overprint: gstate.overprint,
            painted_channels: painted_channels_for_cs(resolved_cs),
            color_lut,
        },
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
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
    // Use the resolved color space's component count for parsing vertex data.
    // For Indexed this is 1 (the palette index), even though the shading CS
    // (after palette expansion) may have 3 or 4 components.
    let cs_comps = resolved_cs.num_components();

    let function = if dict.get(b"Function").is_some() {
        parse_shading_function(dict, resolver).ok()
    } else {
        None
    };
    let n_comps = if function.is_some() {
        let color_entries = decode.len().saturating_sub(4);
        (color_entries / 2).max(1)
    } else {
        cs_comps
    };

    let data = resolver.stream_data_from_obj(shading_obj)?;

    let mut patches = match shading_type {
        6 => stet_graphics::mesh_shading::parse_type6_patches(
            &data, bpc, bpco, bpfl, &decode, n_comps,
        ),
        7 => stet_graphics::mesh_shading::parse_type7_patches(
            &data, bpc, bpco, bpfl, &decode, n_comps,
        ),
        _ => return Ok(()),
    };

    // For single-input function-based patches, build a per-pixel LUT
    // (matching the mesh shading approach) so non-linear functions (e.g. N=3)
    // produce correct color transitions.  Corner raw_colors keep the
    // normalized function input for bilinear interpolation in the renderer.
    let color_lut = if let Some(ref func) = function {
        if n_comps == 1 {
            let d_min = decode.get(4).copied().unwrap_or(0.0);
            let d_max = decode.get(5).copied().unwrap_or(1.0);
            let d_range = (d_max - d_min).abs().max(1e-10);

            let lut_size = 256;
            let mut lut = Vec::with_capacity(lut_size);
            for i in 0..lut_size {
                let t = i as f64 / (lut_size - 1) as f64;
                let input = d_min + t * (d_max - d_min);
                let components = func.evaluate(&[input]);
                let color =
                    components_to_device_color_icc(resolved_cs, &components, Some(icc_cache));
                lut.push(color);
            }

            // Normalize vertex raw values to [0, 1] for LUT indexing
            for p in &mut patches {
                for i in 0..4 {
                    let raw = p.raw_colors[i][0];
                    let normalized = ((raw - d_min) / d_range).clamp(0.0, 1.0);
                    p.raw_colors[i] = vec![normalized];
                    // Set corner color from LUT for fallback rendering
                    let idx = (normalized * 255.0).round() as usize;
                    p.colors[i] = lut[idx.min(255)].clone();
                }
            }
            Some(std::sync::Arc::new(lut))
        } else {
            // Multi-input function: apply at corners only
            for p in &mut patches {
                for i in 0..4 {
                    p.raw_colors[i] = func.evaluate(&p.raw_colors[i]);
                }
            }
            for p in &mut patches {
                for i in 0..4 {
                    p.colors[i] = components_to_device_color_icc(
                        resolved_cs,
                        &p.raw_colors[i],
                        Some(icc_cache),
                    );
                }
            }
            None
        }
    } else {
        // No function: convert direct corner colors through ICC
        for p in &mut patches {
            for i in 0..4 {
                p.colors[i] = components_to_device_color_icc(
                    resolved_cs,
                    &p.raw_colors[i],
                    Some(icc_cache),
                );
            }
        }
        None
    };

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
            overprint: gstate.overprint,
            painted_channels: painted_channels_for_cs(resolved_cs),
            color_lut,
        },
    });
    Ok(())
}

fn parse_shading_function(dict: &PdfDict, resolver: &Resolver) -> Result<PdfFunction, PdfError> {
    let fn_obj = dict
        .get(b"Function")
        .ok_or(PdfError::Other("shading missing Function".into()))?;
    let fn_obj = resolver.deref(fn_obj)?;
    // Handle /Function null (invalid but seen in the wild)
    if matches!(fn_obj, PdfObj::Null) {
        return Err(PdfError::Other("shading Function is null".into()));
    }
    if let PdfObj::Array(arr) = &fn_obj {
        if arr.len() == 1 {
            return PdfFunction::parse(&arr[0], resolver);
        }
        // Array of N functions: each produces 1 output component.
        // Combine into a composite that concatenates all outputs.
        // This is common for DeviceCMYK shadings (4 functions → 4 components).
        if arr.len() > 1 {
            let mut funcs = Vec::with_capacity(arr.len());
            for item in arr {
                funcs.push(PdfFunction::parse(item, resolver)?);
            }
            return Ok(PdfFunction::composite(funcs));
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
    // For Separation/DeviceN with DeviceCMYK alternate, extract the tint function
    // so we can store tint-transformed CMYK values in raw_components (needed for
    // overprint CMYK buffer tracking).
    let cmyk_tint_fn = match resolved_cs {
        ResolvedColorSpace::Separation { alt, tint_fn, .. }
        | ResolvedColorSpace::DeviceN { alt, tint_fn, .. }
            if matches!(**alt, ResolvedColorSpace::DeviceCMYK) =>
        {
            tint_fn.as_ref()
        }
        _ => None,
    };

    let [d_min, d_max] = function.domain_0();
    let mut stops = Vec::with_capacity(n_samples);
    for i in 0..n_samples {
        let t = i as f64 / (n_samples - 1) as f64;
        let input = d_min + t * (d_max - d_min);
        let components = function.evaluate(&[input]);
        let color = components_to_device_color_icc(resolved_cs, &components, Some(icc_cache));

        // For DeviceN/Separation with CMYK alternate, store the tint-transformed
        // 4-component CMYK values so the renderer can populate the CMYK tracking buffer.
        let raw_components = if let Some(tint) = cmyk_tint_fn {
            let cmyk = tint.evaluate(&components);
            // Ensure we have exactly 4 CMYK components
            if cmyk.len() >= 4 {
                cmyk[..4].to_vec()
            } else {
                components
            }
        } else {
            components
        };

        stops.push(ColorStop {
            position: t,
            color,
            raw_components,
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
    if let Some(cs_obj) = dict.get(b"ColorSpace")
        && let Ok(resolved) = resolve_color_space_obj(cs_obj, resolver)
    {
        return resolved;
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
        // ICCBased: preserve profile info so the renderer can convert
        // interpolated colors per-grid-point for accurate patch shading.
        ResolvedColorSpace::ICCBased {
            n,
            profile_hash: Some(hash),
            profile_data: Some(data),
            ..
        } if *n != 1 && *n != 4 => ShadingColorSpace::ICCBased {
            n: *n as u32,
            profile_hash: *hash,
            profile_data: Arc::clone(data),
        },
        ResolvedColorSpace::ICCBased { n, .. } => match n {
            1 => ShadingColorSpace::DeviceGray,
            4 => ShadingColorSpace::DeviceCMYK,
            _ => ShadingColorSpace::DeviceRGB,
        },
        // Indexed: use the base color space for the display list element
        ResolvedColorSpace::Indexed { base, .. } => resolved_cs_to_shading_cs(base),
        // Separation/DeviceN with DeviceCMYK alternate: treat as CMYK for overprint
        ResolvedColorSpace::Separation { alt, .. } | ResolvedColorSpace::DeviceN { alt, .. }
            if matches!(**alt, ResolvedColorSpace::DeviceCMYK) =>
        {
            ShadingColorSpace::DeviceCMYK
        }
        _ => ShadingColorSpace::DeviceRGB,
    }
}
