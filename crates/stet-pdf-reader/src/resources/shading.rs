// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shading (sh operator) → DisplayElement conversion.

use crate::content::color_space::{
    ResolvedColorSpace, components_to_device_color_icc_with_intent, painted_channels_for_cs,
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
    PatchShadingParams, RadialShadingParams, ShadingColorSpace, SimpleColorSpace, SpotTintFunction,
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
    handle_shading_cached(
        shading_obj,
        dict,
        gstate,
        resolver,
        display_list,
        icc_cache,
        &mut ShadingStops::default(),
    )
}

/// The colour stops of the axial and radial shadings a page has drawn so
/// far, by shading object and rendering intent.
///
/// Sampling a shading's function into stops means parsing the function,
/// evaluating it up to 1,024 times and converting each result through the
/// colour pipeline, and none of that depends on where the shading is
/// drawn. A page that builds a gradient out of clipped strips draws the
/// same few shadings thousands of times (pdf.js `bug1721218_reduced.pdf`:
/// 41 shadings, 3,531 uses), so the stops are kept for the page.
#[derive(Default)]
pub(crate) struct ShadingStops {
    stops: std::collections::HashMap<(u32, u16, u8), Vec<ColorStop>>,
}

/// Shadings past this many on one page are sampled each time, as before:
/// the cache is for the few drawn often, and each entry is up to 1,024
/// stops.
const MAX_CACHED_SHADINGS: usize = 256;

impl ShadingStops {
    /// The stops of `shading_obj` under `intent`, sampled by `sample` the
    /// first time. A shading written directly in a resource dictionary has
    /// no object number to know it by and is sampled every time.
    fn get_or_sample(
        &mut self,
        shading_obj: &PdfObj,
        intent: u8,
        sample: impl FnOnce() -> Result<Vec<ColorStop>, PdfError>,
    ) -> Result<Vec<ColorStop>, PdfError> {
        let PdfObj::Ref(num, generation) = shading_obj else {
            return sample();
        };
        let key = (*num, *generation, intent);
        if let Some(stops) = self.stops.get(&key) {
            return Ok(stops.clone());
        }
        let stops = sample()?;
        if self.stops.len() < MAX_CACHED_SHADINGS {
            self.stops.insert(key, stops.clone());
        }
        Ok(stops)
    }
}

/// [`handle_shading`], reusing the stops of shadings already drawn on the
/// page.
pub(crate) fn handle_shading_cached(
    shading_obj: &PdfObj,
    dict: &PdfDict,
    gstate: &PdfGraphicsState,
    resolver: &Resolver,
    display_list: &mut DisplayList,
    icc_cache: &mut IccCache,
    stops: &mut ShadingStops,
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
        let bg_color = components_to_device_color_icc_with_intent(
            &resolved_cs,
            &comps,
            Some(icc_cache),
            gstate.rendering_intent,
        );
        let mut params = gstate.fill_params(stet_graphics::color::FillRule::NonZeroWinding);
        params.color = bg_color;
        // Large rect in device space — the shading's clip constrains it
        let mut path = stet_fonts::geometry::PsPath::new();
        path.segments
            .push(stet_fonts::geometry::PathSegment::MoveTo(-1e6, -1e6));
        path.segments
            .push(stet_fonts::geometry::PathSegment::LineTo(1e6, -1e6));
        path.segments
            .push(stet_fonts::geometry::PathSegment::LineTo(1e6, 1e6));
        path.segments
            .push(stet_fonts::geometry::PathSegment::LineTo(-1e6, 1e6));
        path.segments
            .push(stet_fonts::geometry::PathSegment::ClosePath);
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
            shading_obj,
            dict,
            gstate,
            resolver,
            display_list,
            bbox,
            extend,
            &resolved_cs,
            icc_cache,
            stops,
        ),
        3 => handle_radial(
            shading_obj,
            dict,
            gstate,
            resolver,
            display_list,
            bbox,
            extend,
            &resolved_cs,
            icc_cache,
            stops,
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
            let color = components_to_device_color_icc_with_intent(
                resolved_cs,
                &components,
                Some(icc_cache),
                gstate.rendering_intent,
            );
            let idx = ((row * width + col) * 4) as usize;
            rgba[idx] = (color.r * 255.0 + 0.5) as u8;
            rgba[idx + 1] = (color.g * 255.0 + 0.5) as u8;
            rgba[idx + 2] = (color.b * 255.0 + 0.5) as u8;
        }
    }

    let image_matrix = Matrix::new(width as f64, 0.0, 0.0, -(height as f64), 0.0, height as f64);

    display_list.push(DisplayElement::Image {
        sample_data: std::sync::Arc::new(rgba),
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
            opm_paired: false,
            painted_channels: 0,
            alpha_is_shape: false,
            rendering_intent: gstate.rendering_intent,
            transfer: gstate.transfer.clone(),
        },
    });
    Ok(())
}

#[expect(clippy::too_many_arguments)]
fn handle_axial(
    shading_obj: &PdfObj,
    dict: &PdfDict,
    gstate: &PdfGraphicsState,
    resolver: &Resolver,
    display_list: &mut DisplayList,
    bbox: Option<[f64; 4]>,
    extend: (bool, bool),
    resolved_cs: &ResolvedColorSpace,
    icc_cache: &mut IccCache,
    stops: &mut ShadingStops,
) -> Result<(), PdfError> {
    let coords = dict
        .get_array(b"Coords")
        .ok_or(PdfError::Other("axial shading missing Coords".into()))?;
    let vals: Vec<f64> = coords.iter().filter_map(|o| o.as_f64()).collect();
    if vals.len() < 4 {
        return Err(PdfError::Other("axial Coords needs 4 values".into()));
    }

    let color_stops = stops.get_or_sample(shading_obj, gstate.rendering_intent, || {
        let function = parse_shading_function(dict, resolver)?;
        let n_stops = function.min_samples().clamp(64, 1024);
        Ok(sample_function_to_stops_icc(
            &function,
            n_stops,
            resolved_cs,
            icc_cache,
            gstate.rendering_intent,
        ))
    })?;

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
            overprint_mode: gstate.overprint_mode,
            painted_channels: painted_channels_for_cs(resolved_cs),
            alpha: gstate.fill_alpha,
            blend_mode: gstate.blend_mode,
            alpha_is_shape: gstate.alpha_is_shape,
            rendering_intent: gstate.rendering_intent,
            spot_tint_blend: cs_has_spot_with_cmyk_alt(resolved_cs),
            transfer: gstate.transfer.clone(),
        },
    });
    Ok(())
}

#[expect(clippy::too_many_arguments)]
fn handle_radial(
    shading_obj: &PdfObj,
    dict: &PdfDict,
    gstate: &PdfGraphicsState,
    resolver: &Resolver,
    display_list: &mut DisplayList,
    bbox: Option<[f64; 4]>,
    extend: (bool, bool),
    resolved_cs: &ResolvedColorSpace,
    icc_cache: &mut IccCache,
    stops: &mut ShadingStops,
) -> Result<(), PdfError> {
    let coords = dict
        .get_array(b"Coords")
        .ok_or(PdfError::Other("radial shading missing Coords".into()))?;
    let vals: Vec<f64> = coords.iter().filter_map(|o| o.as_f64()).collect();
    if vals.len() < 6 {
        return Err(PdfError::Other("radial Coords needs 6 values".into()));
    }

    let color_stops = stops.get_or_sample(shading_obj, gstate.rendering_intent, || {
        let function = parse_shading_function(dict, resolver)?;
        let n_stops = function.min_samples().clamp(64, 1024);
        Ok(sample_function_to_stops_icc(
            &function,
            n_stops,
            resolved_cs,
            icc_cache,
            gstate.rendering_intent,
        ))
    })?;

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
            overprint_mode: gstate.overprint_mode,
            painted_channels: painted_channels_for_cs(resolved_cs),
            alpha: gstate.fill_alpha,
            blend_mode: gstate.blend_mode,
            alpha_is_shape: gstate.alpha_is_shape,
            rendering_intent: gstate.rendering_intent,
            spot_tint_blend: cs_has_spot_with_cmyk_alt(resolved_cs),
            transfer: gstate.transfer.clone(),
        },
    });
    Ok(())
}

/// True when a resolved color space is Separation/DeviceN with a CMYK
/// alternate AND at least one non-process spot colorant.  See
/// [`AxialShadingParams::spot_tint_blend`].
fn cs_has_spot_with_cmyk_alt(cs: &ResolvedColorSpace) -> bool {
    use stet_graphics::device::cmyk_channel_for_name;
    match cs {
        ResolvedColorSpace::Separation { name, alt, .. } => {
            cmyk_channel_for_name(name) == 0
                && matches!(alt.as_ref(), ResolvedColorSpace::DeviceCMYK)
        }
        ResolvedColorSpace::DeviceN { names, alt, .. } => {
            matches!(alt.as_ref(), ResolvedColorSpace::DeviceCMYK)
                && names.iter().any(|n| cmyk_channel_for_name(n) == 0)
        }
        _ => false,
    }
}

/// The error for a mesh shading whose bit widths or `Decode` array cannot
/// describe a vertex.
fn unreadable_mesh_layout(
    bpc: usize,
    bpco: usize,
    bpfl: Option<usize>,
    decode: &[f64],
) -> PdfError {
    // A negative width in the file arrives here as a very large number.
    let bits = |b: usize| b as i64;
    let flag = bpfl.map_or(String::new(), |b| format!(", BitsPerFlag {}", bits(b)));
    PdfError::Other(format!(
        "mesh shading cannot be read: BitsPerCoordinate {}, BitsPerComponent {}{flag}, \
         Decode has {} entries",
        bits(bpc),
        bits(bpco),
        decode.len()
    ))
}

#[expect(clippy::too_many_arguments)]
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

    let flag_bits = (shading_type == 4).then_some(bpfl);
    if !stet_graphics::mesh_shading::mesh_layout_is_readable(bpc, bpco, flag_bits, &decode) {
        return Err(unreadable_mesh_layout(bpc, bpco, flag_bits, &decode));
    }

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
    let mut lut_components = None;
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
                let color = components_to_device_color_icc_with_intent(
                    resolved_cs,
                    &components,
                    Some(icc_cache),
                    gstate.rendering_intent,
                );
                lut.push(color);
            }

            lut_components = shading_lut_components(func, resolved_cs, &cs, d_min, d_max, &lut);

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
                    let color = components_to_device_color_icc_with_intent(
                        resolved_cs,
                        &expanded,
                        Some(icc_cache),
                        gstate.rendering_intent,
                    );
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
            t.v0.color = components_to_device_color_icc_with_intent(
                resolved_cs,
                &t.v0.raw_components,
                Some(icc_cache),
                gstate.rendering_intent,
            );
            t.v1.color = components_to_device_color_icc_with_intent(
                resolved_cs,
                &t.v1.raw_components,
                Some(icc_cache),
                gstate.rendering_intent,
            );
            t.v2.color = components_to_device_color_icc_with_intent(
                resolved_cs,
                &t.v2.raw_components,
                Some(icc_cache),
                gstate.rendering_intent,
            );
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
            overprint_mode: gstate.overprint_mode,
            painted_channels: painted_channels_for_cs(resolved_cs),
            color_lut,
            color_lut_components: lut_components,
            alpha: gstate.fill_alpha,
            blend_mode: gstate.blend_mode,
            alpha_is_shape: gstate.alpha_is_shape,
            rendering_intent: gstate.rendering_intent,
            transfer: gstate.transfer.clone(),
        },
    });
    Ok(())
}

#[expect(clippy::too_many_arguments)]
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

    if !stet_graphics::mesh_shading::mesh_layout_is_readable(bpc, bpco, Some(bpfl), &decode) {
        return Err(unreadable_mesh_layout(bpc, bpco, Some(bpfl), &decode));
    }

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
    let mut lut_components = None;
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
                let color = components_to_device_color_icc_with_intent(
                    resolved_cs,
                    &components,
                    Some(icc_cache),
                    gstate.rendering_intent,
                );
                lut.push(color);
            }

            lut_components = shading_lut_components(func, resolved_cs, &cs, d_min, d_max, &lut);

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
                    p.colors[i] = components_to_device_color_icc_with_intent(
                        resolved_cs,
                        &p.raw_colors[i],
                        Some(icc_cache),
                        gstate.rendering_intent,
                    );
                }
            }
            None
        }
    } else {
        // No function: convert direct corner colors through ICC
        for p in &mut patches {
            for i in 0..4 {
                p.colors[i] = components_to_device_color_icc_with_intent(
                    resolved_cs,
                    &p.raw_colors[i],
                    Some(icc_cache),
                    gstate.rendering_intent,
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
            overprint_mode: gstate.overprint_mode,
            painted_channels: painted_channels_for_cs(resolved_cs),
            color_lut,
            color_lut_components: lut_components,
            alpha: gstate.fill_alpha,
            blend_mode: gstate.blend_mode,
            alpha_is_shape: gstate.alpha_is_shape,
            rendering_intent: gstate.rendering_intent,
            transfer: gstate.transfer.clone(),
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
    intent: u8,
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
    let span = d_max - d_min;

    // Collect discontinuity positions and convert to normalized t in [0,1].
    // At each discontinuity we insert two samples (before and at) to produce a sharp edge.
    let disc_positions = function.discontinuity_positions();
    let mut disc_ts: Vec<f64> = disc_positions
        .iter()
        .filter_map(|&d| {
            if span.abs() < 1e-15 {
                return None;
            }
            let t = (d - d_min) / span;
            if t > 0.0 && t < 1.0 { Some(t) } else { None }
        })
        .collect();
    // `total_cmp`, not `partial_cmp().unwrap()`: the filter above happens to
    // exclude NaN today (a NaN `t` fails both `>` and `<`), so the unwrap is
    // not currently reachable — but that makes this function's safety depend
    // on a caller-side invariant, and `total_cmp` costs nothing to be right
    // unconditionally.
    disc_ts.sort_by(f64::total_cmp);
    disc_ts.dedup_by(|a, b| (*a - *b).abs() < 1e-12);

    // Build sample positions: uniform grid + discontinuity pairs.
    //
    // `n_samples - 1` is the divisor, so a caller passing 1 would produce
    // `0.0 / 0.0` — a NaN that then poisons the sort and every stop position.
    // Callers currently clamp to `[64, 1024]`; clamp here too so the
    // guarantee is local.
    let n_samples = n_samples.max(2);
    let mut sample_ts: Vec<f64> = (0..n_samples)
        .map(|i| i as f64 / (n_samples - 1) as f64)
        .collect();
    let eps = 1e-10;
    for &dt in &disc_ts {
        sample_ts.push((dt - eps).max(0.0));
        sample_ts.push(dt);
    }
    sample_ts.sort_by(f64::total_cmp);
    sample_ts.dedup_by(|a, b| (*a - *b).abs() < 1e-14);

    let is_spot_with_cmyk_alt = cmyk_tint_fn.is_some();

    let mut stops = Vec::with_capacity(sample_ts.len());
    for t in sample_ts {
        let input = d_min + t * span;
        let components = function.evaluate(&[input]);
        let color = components_to_device_color_icc_with_intent(
            resolved_cs,
            &components,
            Some(icc_cache),
            intent,
        );

        // For DeviceN/Separation with CMYK alternate, store the tint-transformed
        // 4-component CMYK values so the renderer can populate the CMYK tracking buffer.
        // Also retain the pre-transform `components` as `source_components` so the
        // PDF writer can emit a /Function whose output dimension matches the
        // shading's source color space (1 channel for Separation, N for DeviceN).
        let (raw_components, source_components) = if let Some(tint) = cmyk_tint_fn {
            let cmyk = tint.evaluate(&components);
            let raw = if cmyk.len() >= 4 {
                cmyk[..4].to_vec()
            } else {
                components.clone()
            };
            (raw, components)
        } else if is_spot_with_cmyk_alt {
            // unreachable but keeps the type checker happy
            (components.clone(), components)
        } else {
            (components, Vec::new())
        };

        stops.push(ColorStop {
            position: t,
            color,
            raw_components,
            source_components,
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
/// The shading function at each `color_lut` input, as components of the
/// display list's colour space `cs`, so a writer can emit the function
/// again: the function's own outputs where `cs` is the source's space,
/// the converted colours where `cs` is the DeviceRGB stand-in for one
/// stet does not carry. `None` when neither applies.
fn shading_lut_components(
    func: &PdfFunction,
    resolved_cs: &ResolvedColorSpace,
    cs: &ShadingColorSpace,
    d_min: f64,
    d_max: f64,
    lut: &[stet_graphics::color::DeviceColor],
) -> Option<Arc<Vec<Vec<f64>>>> {
    let n = cs.num_components();
    let last = lut.len().saturating_sub(1).max(1) as f64;
    let samples: Vec<Vec<f64>> = if shading_cs_is_source(resolved_cs) {
        (0..lut.len())
            .map(|i| func.evaluate(&[d_min + i as f64 / last * (d_max - d_min)]))
            .collect()
    } else if matches!(cs, ShadingColorSpace::DeviceRGB) {
        lut.iter().map(|c| vec![c.r, c.g, c.b]).collect()
    } else {
        return None;
    };
    if samples.iter().any(|s| s.len() < n) {
        return None;
    }
    Some(Arc::new(
        samples.into_iter().map(|s| s[..n].to_vec()).collect(),
    ))
}

/// Whether [`resolved_cs_to_shading_cs`] keeps `cs` itself — its
/// components mean the same in the display list's colour space — rather
/// than standing DeviceRGB or DeviceCMYK in for it.
fn shading_cs_is_source(cs: &ResolvedColorSpace) -> bool {
    match cs {
        ResolvedColorSpace::DeviceGray
        | ResolvedColorSpace::DeviceRGB
        | ResolvedColorSpace::DeviceCMYK
        | ResolvedColorSpace::ICCBased { .. } => true,
        ResolvedColorSpace::Separation { alt, tint_fn, .. }
        | ResolvedColorSpace::DeviceN { alt, tint_fn, .. } => {
            tint_fn.is_some() && matches!(**alt, ResolvedColorSpace::DeviceCMYK)
        }
        _ => false,
    }
}

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
            n: *n,
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
        // Separation/DeviceN with DeviceCMYK alternate: preserve the spot
        // identity so the PDF writer can round-trip the source's spot
        // /ColorSpace and the re-read display list re-acquires
        // `spot_tint_blend: true` for correct compositing.
        ResolvedColorSpace::Separation { name, alt, tint_fn }
            if matches!(**alt, ResolvedColorSpace::DeviceCMYK) =>
        {
            match tint_fn {
                Some(tint) => ShadingColorSpace::Separation {
                    name: name.clone(),
                    alternate: SimpleColorSpace::DeviceCMYK,
                    tint_function: sample_tint_function_1d(tint, 16),
                },
                None => ShadingColorSpace::DeviceCMYK,
            }
        }
        ResolvedColorSpace::DeviceN {
            names,
            alt,
            tint_fn,
        } if matches!(**alt, ResolvedColorSpace::DeviceCMYK) => {
            // With no tint transform, or more colourants than a sampled
            // grid can span, the shading is carried as the CMYK its stops
            // were already transformed to.
            match tint_fn
                .as_ref()
                .and_then(|tint| sample_tint_function_nd(tint, names.len(), 8))
            {
                Some(tint_function) => ShadingColorSpace::DeviceN {
                    names: names.clone(),
                    alternate: SimpleColorSpace::DeviceCMYK,
                    tint_function,
                },
                None => ShadingColorSpace::DeviceCMYK,
            }
        }
        _ => ShadingColorSpace::DeviceRGB,
    }
}

/// Sample a 1-component tint function on a uniform grid in `[0, 1]` and
/// pack the CMYK outputs into a `SpotTintFunction`. The writer emits this
/// back as a `FunctionType 0` sampled function inside the shading's
/// `/ColorSpace [/Separation … <tintFunc>]` array.
fn sample_tint_function_1d(tint: &PdfFunction, samples_per_dim: usize) -> SpotTintFunction {
    let mut cmyk_samples = Vec::with_capacity(samples_per_dim * 4);
    for i in 0..samples_per_dim {
        let t = i as f64 / (samples_per_dim - 1).max(1) as f64;
        let out = tint.evaluate(&[t]);
        for k in 0..4 {
            cmyk_samples.push(out.get(k).copied().unwrap_or(0.0));
        }
    }
    SpotTintFunction {
        input_dim: 1,
        samples_per_dim,
        cmyk_samples: Arc::new(cmyk_samples),
    }
}

/// Sample an N-component tint function on a uniform N-dimensional grid in
/// `[0, 1]^N`. Output ordering is row-major with the first input axis
/// varying slowest (matching PDF's SampledFunction convention).
///
/// `samples_per_dim` is what the caller would like; the grid gets fewer
/// when that many would exceed the ceiling every tint table shares, and
/// `None` when `input_dim` is more than a grid can span at all.
fn sample_tint_function_nd(
    tint: &PdfFunction,
    input_dim: usize,
    samples_per_dim: usize,
) -> Option<SpotTintFunction> {
    let samples_per_dim = stet_graphics::device::TintLookupTable::grid_samples(
        u32::try_from(input_dim).ok()?,
        u32::try_from(samples_per_dim).ok()?,
    )? as usize;
    let total = samples_per_dim.pow(input_dim as u32);
    let mut cmyk_samples = Vec::with_capacity(total * 4);
    let mut input = vec![0.0_f64; input_dim];
    for idx in 0..total {
        // Decompose `idx` into per-axis indices with first axis as slowest.
        let mut rem = idx;
        for axis in (0..input_dim).rev() {
            let i = rem % samples_per_dim;
            rem /= samples_per_dim;
            input[axis] = i as f64 / (samples_per_dim - 1).max(1) as f64;
        }
        let out = tint.evaluate(&input);
        for k in 0..4 {
            cmyk_samples.push(out.get(k).copied().unwrap_or(0.0));
        }
    }
    Some(SpotTintFunction {
        input_dim,
        samples_per_dim,
        cmyk_samples: Arc::new(cmyk_samples),
    })
}

#[cfg(test)]
mod shading_stops_tests {
    use super::*;
    use stet_graphics::color::DeviceColor;

    fn stops(n: usize) -> Vec<ColorStop> {
        (0..n)
            .map(|i| ColorStop {
                position: i as f64,
                color: DeviceColor::from_rgb(0.0, 0.0, 0.0),
                raw_components: Vec::new(),
                source_components: Vec::new(),
            })
            .collect()
    }

    /// How many stops the cache hands back, and whether it had to sample.
    fn ask(cache: &mut ShadingStops, obj: &PdfObj, intent: u8, n: usize) -> (usize, bool) {
        let mut sampled = false;
        let got = cache
            .get_or_sample(obj, intent, || {
                sampled = true;
                Ok(stops(n))
            })
            .unwrap();
        (got.len(), sampled)
    }

    #[test]
    fn a_shading_object_is_sampled_once_for_each_intent() {
        let mut cache = ShadingStops::default();
        let shading = PdfObj::Ref(7, 0);
        assert_eq!(ask(&mut cache, &shading, 1, 3), (3, true));
        // The second use gets the first sampling, not a new one.
        assert_eq!(ask(&mut cache, &shading, 1, 99), (3, false));
        // Another intent converts colours differently.
        assert_eq!(ask(&mut cache, &shading, 0, 5), (5, true));
        assert_eq!(ask(&mut cache, &shading, 0, 99), (5, false));
        // Another object, and another generation of the same number.
        assert_eq!(ask(&mut cache, &PdfObj::Ref(8, 0), 1, 4), (4, true));
        assert_eq!(ask(&mut cache, &PdfObj::Ref(7, 1), 1, 6), (6, true));
        assert_eq!(ask(&mut cache, &shading, 1, 99), (3, false));
    }

    #[test]
    fn a_shading_with_no_object_number_is_sampled_every_time() {
        let mut cache = ShadingStops::default();
        let inline = PdfObj::Dict(PdfDict::new());
        assert_eq!(ask(&mut cache, &inline, 1, 3), (3, true));
        assert_eq!(ask(&mut cache, &inline, 1, 4), (4, true));
    }

    #[test]
    fn a_failed_sampling_is_not_remembered() {
        let mut cache = ShadingStops::default();
        let shading = PdfObj::Ref(7, 0);
        let failed =
            cache.get_or_sample(&shading, 1, || Err(PdfError::Other("no function".into())));
        assert!(failed.is_err());
        assert_eq!(ask(&mut cache, &shading, 1, 3), (3, true));
    }

    #[test]
    fn the_cache_stops_growing_and_keeps_working() {
        let mut cache = ShadingStops::default();
        for n in 0..MAX_CACHED_SHADINGS as u32 + 10 {
            assert_eq!(ask(&mut cache, &PdfObj::Ref(n, 0), 1, 2), (2, true));
        }
        assert_eq!(cache.stops.len(), MAX_CACHED_SHADINGS);
        // The early ones are still served from it; the overflow is sampled
        // again each time, as every shading was before there was a cache.
        assert_eq!(ask(&mut cache, &PdfObj::Ref(0, 0), 1, 9), (2, false));
        let overflow = PdfObj::Ref(MAX_CACHED_SHADINGS as u32 + 5, 0);
        assert_eq!(ask(&mut cache, &overflow, 1, 9), (9, true));
    }
}
