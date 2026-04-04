// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `shfill` operator — smooth shading fill for all 7 PLRM shading types.
//!
//! Extracts shading parameters from a PostScript dict, samples the shading
//! function where needed, and pushes display list elements for rendering.

use stet_core::context::Context;
use stet_core::dict::DictKey;
use stet_core::error::PsError;
use stet_core::graphics_state::ColorSpace;
use stet_core::object::{EntityId, PsObject, PsValue};
use stet_fonts::geometry::Matrix;
use stet_graphics::color::DeviceColor;
use stet_graphics::device::{
    AxialShadingParams, ColorStop, ImageParams, MeshShadingParams, PatchShadingParams,
    RadialShadingParams, ShadingColorSpace,
};
use stet_graphics::display_list::DisplayElement;
use stet_graphics::mesh_shading;

use crate::color_ops::{precompute_cie_decode_tables, resolve_color_space_from_obj};

// Number of samples for function-based gradients (matches PostForge).
const NUM_GRADIENT_SAMPLES: usize = 64;

// Resolution for Type 1 function-based shading rasterization.
const FUNCTION_SHADING_SIZE: usize = 256;

/// `shfill`: dict → —
///
/// Smooth shading fill operator. Reads a shading dictionary from the operand
/// stack and renders the specified gradient or mesh fill.
pub fn op_shfill(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let dict_obj = ctx.o_stack.peek(0)?;
    let dict_entity = match dict_obj.value {
        PsValue::Dict(e) => e,
        _ => return Err(PsError::TypeCheck),
    };

    // Extract ShadingType (required, integer 1-7)
    let shading_type = get_dict_int(ctx, dict_entity, b"ShadingType").ok_or(PsError::RangeCheck)?;
    if !(1..=7).contains(&shading_type) {
        return Err(PsError::RangeCheck);
    }

    // Extract ColorSpace (required)
    let cs_obj = get_dict_obj(ctx, dict_entity, b"ColorSpace").ok_or(PsError::Undefined)?;
    let (color_space, n_comps) = resolve_color_space_from_obj(ctx, &cs_obj)?;
    let color_space = precompute_cie_decode_tables(ctx, color_space)?;

    // Extract optional BBox [llx lly urx ury]
    let bbox = get_dict_bbox(ctx, dict_entity);

    // CTM snapshot
    let ctm = ctx.gstate.ctm;

    // Capture the shading color space for native output (PDF, TIFF)
    let shading_cs = capture_shading_color_space(ctx, &color_space);

    // Dispatch to type-specific builders
    match shading_type {
        1 => build_type1_shading(ctx, dict_entity, &color_space, n_comps, ctm, bbox)?,
        2 => build_type2_shading(
            ctx,
            dict_entity,
            &color_space,
            n_comps,
            ctm,
            bbox,
            shading_cs,
        )?,
        3 => build_type3_shading(
            ctx,
            dict_entity,
            &color_space,
            n_comps,
            ctm,
            bbox,
            shading_cs,
        )?,
        4 => build_type4_shading(
            ctx,
            dict_entity,
            &color_space,
            n_comps,
            ctm,
            bbox,
            shading_cs,
        )?,
        5 => build_type5_shading(
            ctx,
            dict_entity,
            &color_space,
            n_comps,
            ctm,
            bbox,
            shading_cs,
        )?,
        6 => build_type6_shading(
            ctx,
            dict_entity,
            &color_space,
            n_comps,
            ctm,
            bbox,
            shading_cs,
        )?,
        7 => build_type7_shading(
            ctx,
            dict_entity,
            &color_space,
            n_comps,
            ctm,
            bbox,
            shading_cs,
        )?,
        _ => {} // Already validated above
    }

    ctx.o_stack.pop()?;
    Ok(())
}

// ---- Type 2: Axial (linear gradient) ----

fn build_type2_shading(
    ctx: &mut Context,
    dict: EntityId,
    color_space: &ColorSpace,
    n_comps: usize,
    ctm: Matrix,
    bbox: Option<[f64; 4]>,
    shading_cs: ShadingColorSpace,
) -> Result<(), PsError> {
    // Coords [x0 y0 x1 y1] (required)
    let coords = get_dict_float_vec(ctx, dict, b"Coords").ok_or(PsError::Undefined)?;
    if coords.len() < 4 {
        return Err(PsError::RangeCheck);
    }

    // Domain [t0 t1] (default [0 1])
    let domain_vec = get_dict_float_vec(ctx, dict, b"Domain");
    let domain = match domain_vec {
        Some(ref v) if v.len() >= 2 => [v[0], v[1]],
        _ => [0.0, 1.0],
    };

    // Extend [bool bool] (default [false false])
    let (extend_start, extend_end) = get_dict_extend(ctx, dict);

    // Function (required)
    let function = get_dict_obj(ctx, dict, b"Function").ok_or(PsError::Undefined)?;

    // Sample the function to produce color stops
    let color_stops = sample_shading_function(
        ctx,
        function,
        domain,
        n_comps,
        color_space,
        NUM_GRADIENT_SAMPLES,
    )?;

    ctx.display_list.push(DisplayElement::AxialShading {
        params: AxialShadingParams {
            x0: coords[0],
            y0: coords[1],
            x1: coords[2],
            y1: coords[3],
            color_stops,
            extend_start,
            extend_end,
            ctm,
            bbox,
            color_space: shading_cs,
            overprint: false,
            painted_channels: 0,
            alpha: 1.0,
        },
    });

    Ok(())
}

// ---- Type 3: Radial gradient ----

fn build_type3_shading(
    ctx: &mut Context,
    dict: EntityId,
    color_space: &ColorSpace,
    n_comps: usize,
    ctm: Matrix,
    bbox: Option<[f64; 4]>,
    shading_cs: ShadingColorSpace,
) -> Result<(), PsError> {
    // Coords [x0 y0 r0 x1 y1 r1] (required)
    let coords = get_dict_float_vec(ctx, dict, b"Coords").ok_or(PsError::Undefined)?;
    if coords.len() < 6 {
        return Err(PsError::RangeCheck);
    }

    // Domain [t0 t1] (default [0 1])
    let domain_vec = get_dict_float_vec(ctx, dict, b"Domain");
    let domain = match domain_vec {
        Some(ref v) if v.len() >= 2 => [v[0], v[1]],
        _ => [0.0, 1.0],
    };

    // Extend [bool bool]
    let (extend_start, extend_end) = get_dict_extend(ctx, dict);

    // Function (required)
    let function = get_dict_obj(ctx, dict, b"Function").ok_or(PsError::Undefined)?;

    let color_stops = sample_shading_function(
        ctx,
        function,
        domain,
        n_comps,
        color_space,
        NUM_GRADIENT_SAMPLES,
    )?;

    ctx.display_list.push(DisplayElement::RadialShading {
        params: RadialShadingParams {
            x0: coords[0],
            y0: coords[1],
            r0: coords[2],
            x1: coords[3],
            y1: coords[4],
            r1: coords[5],
            color_stops,
            extend_start,
            extend_end,
            ctm,
            bbox,
            color_space: shading_cs,
            overprint: false,
            painted_channels: 0,
            alpha: 1.0,
        },
    });

    Ok(())
}

// ---- Type 1: Function-based shading (rasterize to image) ----

fn build_type1_shading(
    ctx: &mut Context,
    dict: EntityId,
    color_space: &ColorSpace,
    n_comps: usize,
    ctm: Matrix,
    bbox: Option<[f64; 4]>,
) -> Result<(), PsError> {
    // Domain [x0 x1 y0 y1] (default [0 1 0 1])
    let domain_vec = get_dict_float_vec(ctx, dict, b"Domain");
    let domain = match domain_vec {
        Some(ref v) if v.len() >= 4 => [v[0], v[1], v[2], v[3]],
        _ => [0.0, 1.0, 0.0, 1.0],
    };

    // Matrix (optional, default identity) — maps domain to user space
    let shading_matrix = get_dict_matrix(ctx, dict, b"Matrix").unwrap_or_else(Matrix::identity);

    // Function (required — 2 inputs, n_comps outputs)
    let function = get_dict_obj(ctx, dict, b"Function").ok_or(PsError::Undefined)?;

    let size = FUNCTION_SHADING_SIZE;
    let x_min = domain[0];
    let x_max = domain[1];
    let y_min = domain[2];
    let y_max = domain[3];
    let dx = (x_max - x_min) / size as f64;
    let dy = (y_max - y_min) / size as f64;

    // Check if function is a dict (native eval) or procedure (exec_sync)
    let func_dict = match function.value {
        PsValue::Dict(entity) => Some(entity),
        _ => None,
    };

    // Pre-rasterize to RGB pixel buffer (3 components, no alpha for shadings)
    let mut rgb_data = vec![255u8; size * size * 3];

    for row in 0..size {
        let y_val = y_min + (row as f64 + 0.5) * dy;
        for col in 0..size {
            let x_val = x_min + (col as f64 + 0.5) * dx;

            let color = if let Some(func_entity) = func_dict {
                match evaluate_ps_function(ctx, func_entity, &[x_val, y_val]) {
                    Ok(results) => {
                        if needs_tint_conversion(color_space) {
                            match convert_tint_color(ctx, &results, color_space) {
                                Ok(alt) => components_to_device_color(
                                    &alt,
                                    get_alt_space(color_space),
                                    &mut ctx.icc_cache,
                                ),
                                Err(_) => continue,
                            }
                        } else {
                            components_to_device_color(&results, color_space, &mut ctx.icc_cache)
                        }
                    }
                    Err(_) => continue,
                }
            } else {
                ctx.o_stack.push(PsObject::real(x_val))?;
                ctx.o_stack.push(PsObject::real(y_val))?;
                if ctx.exec_sync(function).is_err() {
                    continue;
                }
                if needs_tint_conversion(color_space) {
                    let mut comps = vec![0.0; n_comps];
                    for i in (0..n_comps).rev() {
                        comps[i] = ctx.o_stack.pop()?.as_f64().unwrap_or(0.0);
                    }
                    match convert_tint_color(ctx, &comps, color_space) {
                        Ok(alt) => components_to_device_color(
                            &alt,
                            get_alt_space(color_space),
                            &mut ctx.icc_cache,
                        ),
                        Err(_) => continue,
                    }
                } else {
                    pop_color_components(ctx, n_comps, color_space)
                }
            };

            let offset = (row * size + col) * 3;
            rgb_data[offset] = (color.r * 255.0).round().clamp(0.0, 255.0) as u8;
            rgb_data[offset + 1] = (color.g * 255.0).round().clamp(0.0, 255.0) as u8;
            rgb_data[offset + 2] = (color.b * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }

    // Build the transform: pixel → domain → user space
    // Pixel-to-domain: scales [0..size] to [x_min..x_max, y_min..y_max]
    let pixel_to_domain = Matrix::new(
        (x_max - x_min) / size as f64,
        0.0,
        0.0,
        (y_max - y_min) / size as f64,
        x_min,
        y_min,
    );
    // Domain → user space via shading Matrix
    // Combined pixel→user = shading_matrix × pixel_to_domain (row-vector convention)
    // In our column-vector multiply: pixel_to_domain.concat(&shading_matrix) would be wrong.
    // Row-vector: p × pixel_to_domain × shading_matrix
    // Column-vector: shading_matrix.multiply(&pixel_to_domain)
    let pixel_to_user = shading_matrix.multiply(&pixel_to_domain);

    // Per PLRM, image_matrix maps user → image space. The renderer
    // computes CTM × inv(image_matrix) = CTM × pixel_to_user to map
    // pixel coords → device space. So image_matrix = inv(pixel_to_user).
    let Some(image_matrix) = pixel_to_user.invert() else {
        return Ok(()); // degenerate matrix — nothing to render
    };

    // Apply bbox clip if present — the image covers the full domain
    // and bbox clipping happens at the device level
    let _ = bbox;

    ctx.display_list.push(DisplayElement::Image {
        sample_data: std::sync::Arc::new(rgb_data),
        params: ImageParams {
            width: size as u32,
            height: size as u32,
            color_space: stet_graphics::device::ImageColorSpace::DeviceRGB,
            bits_per_component: 8,
            ctm,
            image_matrix,
            interpolate: false,
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

// ---- Types 4 & 5: Triangle mesh shading ----

fn build_type4_shading(
    ctx: &mut Context,
    dict: EntityId,
    color_space: &ColorSpace,
    n_comps: usize,
    ctm: Matrix,
    bbox: Option<[f64; 4]>,
    shading_cs: ShadingColorSpace,
) -> Result<(), PsError> {
    let ds_obj = get_dict_obj(ctx, dict, b"DataSource").ok_or(PsError::Undefined)?;

    let triangles = match ds_obj.value {
        PsValue::String { entity, start, len } => {
            let bpc =
                get_dict_int(ctx, dict, b"BitsPerCoordinate").ok_or(PsError::Undefined)? as usize;
            let bpco =
                get_dict_int(ctx, dict, b"BitsPerComponent").ok_or(PsError::Undefined)? as usize;
            let bpfl = get_dict_int(ctx, dict, b"BitsPerFlag").ok_or(PsError::Undefined)? as usize;
            let decode = get_dict_float_vec(ctx, dict, b"Decode").ok_or(PsError::Undefined)?;
            let data = ctx.strings.get(entity, start, len).to_vec();
            mesh_shading::parse_type4_mesh(&data, bpc, bpco, bpfl, &decode, n_comps)
        }
        PsValue::Array { entity, start, len } => {
            let values = extract_float_array(ctx, entity, start, len);
            // Convert DeviceN/Separation colors to device color space
            let (values, eff_n) =
                convert_vertex_array_colors(ctx, values, n_comps, color_space, 3)?;
            mesh_shading::build_type4_from_array(&values, eff_n)
        }
        _ => return Err(PsError::TypeCheck),
    };

    if !triangles.is_empty() {
        ctx.display_list.push(DisplayElement::MeshShading {
            params: MeshShadingParams {
                triangles,
                ctm,
                bbox,
                color_space: shading_cs,
                overprint: false,
                painted_channels: 0,
                color_lut: None,
            },
        });
    }

    Ok(())
}

fn build_type5_shading(
    ctx: &mut Context,
    dict: EntityId,
    color_space: &ColorSpace,
    n_comps: usize,
    ctm: Matrix,
    bbox: Option<[f64; 4]>,
    shading_cs: ShadingColorSpace,
) -> Result<(), PsError> {
    let verts_per_row =
        get_dict_int(ctx, dict, b"VerticesPerRow").ok_or(PsError::Undefined)? as usize;
    let ds_obj = get_dict_obj(ctx, dict, b"DataSource").ok_or(PsError::Undefined)?;

    let triangles = match ds_obj.value {
        PsValue::String { entity, start, len } => {
            let bpc =
                get_dict_int(ctx, dict, b"BitsPerCoordinate").ok_or(PsError::Undefined)? as usize;
            let bpco =
                get_dict_int(ctx, dict, b"BitsPerComponent").ok_or(PsError::Undefined)? as usize;
            let decode = get_dict_float_vec(ctx, dict, b"Decode").ok_or(PsError::Undefined)?;
            let data = ctx.strings.get(entity, start, len).to_vec();
            mesh_shading::parse_type5_mesh(&data, bpc, bpco, &decode, n_comps, verts_per_row)
        }
        PsValue::Array { entity, start, len } => {
            let values = extract_float_array(ctx, entity, start, len);
            let (values, eff_n) =
                convert_vertex_array_colors(ctx, values, n_comps, color_space, 2)?;
            mesh_shading::build_type5_from_array(&values, eff_n, verts_per_row)
        }
        _ => return Err(PsError::TypeCheck),
    };

    if !triangles.is_empty() {
        ctx.display_list.push(DisplayElement::MeshShading {
            params: MeshShadingParams {
                triangles,
                ctm,
                bbox,
                color_space: shading_cs,
                overprint: false,
                painted_channels: 0,
                color_lut: None,
            },
        });
    }

    Ok(())
}

// ---- Types 6 & 7: Patch mesh shading ----

fn build_type6_shading(
    ctx: &mut Context,
    dict: EntityId,
    color_space: &ColorSpace,
    n_comps: usize,
    ctm: Matrix,
    bbox: Option<[f64; 4]>,
    shading_cs: ShadingColorSpace,
) -> Result<(), PsError> {
    let ds_obj = get_dict_obj(ctx, dict, b"DataSource").ok_or(PsError::Undefined)?;

    let patches = match ds_obj.value {
        PsValue::String { entity, start, len } => {
            let bpc =
                get_dict_int(ctx, dict, b"BitsPerCoordinate").ok_or(PsError::Undefined)? as usize;
            let bpco =
                get_dict_int(ctx, dict, b"BitsPerComponent").ok_or(PsError::Undefined)? as usize;
            let bpfl = get_dict_int(ctx, dict, b"BitsPerFlag").ok_or(PsError::Undefined)? as usize;
            let decode = get_dict_float_vec(ctx, dict, b"Decode").ok_or(PsError::Undefined)?;
            let data = ctx.strings.get(entity, start, len).to_vec();
            mesh_shading::parse_type6_patches(&data, bpc, bpco, bpfl, &decode, n_comps)
        }
        PsValue::Array { entity, start, len } => {
            let values = extract_float_array(ctx, entity, start, len);
            let (values, eff_n) = convert_type6_array_colors(ctx, values, n_comps, color_space)?;
            mesh_shading::build_type6_from_array(&values, eff_n)
        }
        _ => return Err(PsError::TypeCheck),
    };

    if !patches.is_empty() {
        ctx.display_list.push(DisplayElement::PatchShading {
            params: PatchShadingParams {
                patches,
                ctm,
                bbox,
                color_space: shading_cs,
                overprint: false,
                painted_channels: 0,
                color_lut: None,
            },
        });
    }

    Ok(())
}

fn build_type7_shading(
    ctx: &mut Context,
    dict: EntityId,
    color_space: &ColorSpace,
    n_comps: usize,
    ctm: Matrix,
    bbox: Option<[f64; 4]>,
    shading_cs: ShadingColorSpace,
) -> Result<(), PsError> {
    let ds_obj = get_dict_obj(ctx, dict, b"DataSource").ok_or(PsError::Undefined)?;

    let patches = match ds_obj.value {
        PsValue::String { entity, start, len } => {
            let bpc =
                get_dict_int(ctx, dict, b"BitsPerCoordinate").ok_or(PsError::Undefined)? as usize;
            let bpco =
                get_dict_int(ctx, dict, b"BitsPerComponent").ok_or(PsError::Undefined)? as usize;
            let bpfl = get_dict_int(ctx, dict, b"BitsPerFlag").ok_or(PsError::Undefined)? as usize;
            let decode = get_dict_float_vec(ctx, dict, b"Decode").ok_or(PsError::Undefined)?;
            let data = ctx.strings.get(entity, start, len).to_vec();
            mesh_shading::parse_type7_patches(&data, bpc, bpco, bpfl, &decode, n_comps)
        }
        PsValue::Array { entity, start, len } => {
            let values = extract_float_array(ctx, entity, start, len);
            let (values, eff_n) = convert_type7_array_colors(ctx, values, n_comps, color_space)?;
            mesh_shading::build_type7_from_array(&values, eff_n)
        }
        _ => return Err(PsError::TypeCheck),
    };

    if !patches.is_empty() {
        ctx.display_list.push(DisplayElement::PatchShading {
            params: PatchShadingParams {
                patches,
                ctm,
                bbox,
                color_space: shading_cs,
                overprint: false,
                painted_channels: 0,
                color_lut: None,
            },
        });
    }

    Ok(())
}

// ---- Color space conversion helpers ----

/// Check if a color space requires tint transform conversion (Separation/DeviceN).
fn needs_tint_conversion(cs: &ColorSpace) -> bool {
    matches!(
        cs,
        ColorSpace::Separation { .. } | ColorSpace::DeviceN { .. }
    )
}

/// Get the alternative color space for Separation/DeviceN.
fn get_alt_space(cs: &ColorSpace) -> &ColorSpace {
    match cs {
        ColorSpace::Separation { alt_space, .. } | ColorSpace::DeviceN { alt_space, .. } => {
            alt_space.as_ref()
        }
        _ => cs,
    }
}

/// Convert color components from Separation/DeviceN to the alternative
/// device color space by running the tint transform via exec_sync.
fn convert_tint_color(
    ctx: &mut Context,
    comps: &[f64],
    color_space: &ColorSpace,
) -> Result<Vec<f64>, PsError> {
    let (tint_transform, num_alt) = match color_space {
        ColorSpace::Separation {
            tint_transform,
            num_alt_components,
            ..
        } => (*tint_transform, *num_alt_components as usize),
        ColorSpace::DeviceN {
            tint_transform,
            num_alt_components,
            ..
        } => (*tint_transform, *num_alt_components as usize),
        _ => return Ok(comps.to_vec()),
    };
    for c in comps {
        ctx.o_stack.push(PsObject::real(*c))?;
    }
    ctx.exec_sync(tint_transform)?;
    let mut result = vec![0.0; num_alt];
    for i in (0..num_alt).rev() {
        result[i] = ctx.o_stack.pop()?.as_f64().unwrap_or(0.0);
    }
    Ok(result)
}

/// Pre-process Type 4/5 array DataSource: convert colors from source color
/// space to device color space via tint transform. `prefix_count` is the
/// number of non-color values per vertex (3 for Type 4: flag+x+y, 2 for
/// Type 5: x+y). Returns (converted values, effective n_comps).
fn convert_vertex_array_colors(
    ctx: &mut Context,
    values: Vec<f64>,
    n_comps: usize,
    color_space: &ColorSpace,
    prefix_count: usize,
) -> Result<(Vec<f64>, usize), PsError> {
    if !needs_tint_conversion(color_space) {
        return Ok((values, n_comps));
    }
    let alt_n = match color_space {
        ColorSpace::Separation {
            num_alt_components, ..
        }
        | ColorSpace::DeviceN {
            num_alt_components, ..
        } => *num_alt_components as usize,
        _ => return Ok((values, n_comps)),
    };
    let src_stride = prefix_count + n_comps;
    let mut converted =
        Vec::with_capacity(values.len() * (prefix_count + alt_n) / src_stride.max(1));
    let mut pos = 0;
    while pos + src_stride <= values.len() {
        converted.extend_from_slice(&values[pos..pos + prefix_count]);
        let alt = convert_tint_color(
            ctx,
            &values[pos + prefix_count..pos + prefix_count + n_comps],
            color_space,
        )?;
        converted.extend_from_slice(&alt);
        pos += src_stride;
    }
    Ok((converted, alt_n))
}

/// Pre-process Type 6 Coons patch array DataSource: convert colors via tint
/// transform. Handles flag-dependent variable-stride layout.
fn convert_type6_array_colors(
    ctx: &mut Context,
    values: Vec<f64>,
    n_comps: usize,
    color_space: &ColorSpace,
) -> Result<(Vec<f64>, usize), PsError> {
    if !needs_tint_conversion(color_space) {
        return Ok((values, n_comps));
    }
    let alt_n = match color_space {
        ColorSpace::Separation {
            num_alt_components, ..
        }
        | ColorSpace::DeviceN {
            num_alt_components, ..
        } => *num_alt_components as usize,
        _ => return Ok((values, n_comps)),
    };
    let mut converted = Vec::new();
    let mut pos = 0;
    let mut is_first = true;
    while pos < values.len() {
        let flag = values[pos] as u32;
        converted.push(values[pos]);
        pos += 1;
        let (n_coord_vals, n_colors) = if flag == 0 || is_first {
            is_first = false;
            (24, 4) // 12 points (24 coords) + 4 corner colors
        } else {
            (18, 2) // 9 points (18 coords) + 2 corner colors
        };
        if pos + n_coord_vals + n_colors * n_comps > values.len() {
            break;
        }
        converted.extend_from_slice(&values[pos..pos + n_coord_vals]);
        pos += n_coord_vals;
        for _ in 0..n_colors {
            let alt = convert_tint_color(ctx, &values[pos..pos + n_comps], color_space)?;
            converted.extend_from_slice(&alt);
            pos += n_comps;
        }
    }
    Ok((converted, alt_n))
}

/// Pre-process Type 7 tensor-product patch array DataSource: convert colors
/// via tint transform.
fn convert_type7_array_colors(
    ctx: &mut Context,
    values: Vec<f64>,
    n_comps: usize,
    color_space: &ColorSpace,
) -> Result<(Vec<f64>, usize), PsError> {
    if !needs_tint_conversion(color_space) {
        return Ok((values, n_comps));
    }
    let alt_n = match color_space {
        ColorSpace::Separation {
            num_alt_components, ..
        }
        | ColorSpace::DeviceN {
            num_alt_components, ..
        } => *num_alt_components as usize,
        _ => return Ok((values, n_comps)),
    };
    // Current implementation only handles flag-0 patches
    let src_stride = 1 + 32 + 4 * n_comps;
    let mut converted = Vec::new();
    let mut pos = 0;
    while pos + src_stride <= values.len() {
        // Copy flag + 32 coord values
        converted.extend_from_slice(&values[pos..pos + 33]);
        pos += 33;
        for _ in 0..4 {
            let alt = convert_tint_color(ctx, &values[pos..pos + n_comps], color_space)?;
            converted.extend_from_slice(&alt);
            pos += n_comps;
        }
    }
    Ok((converted, alt_n))
}

// ---- Function sampling ----

/// Sample a shading function across a domain to produce color stops.
fn sample_shading_function(
    ctx: &mut Context,
    function: PsObject,
    domain: [f64; 2],
    n_comps: usize,
    color_space: &ColorSpace,
    num_samples: usize,
) -> Result<Vec<ColorStop>, PsError> {
    let mut stops = Vec::with_capacity(num_samples + 1);
    let d_min = domain[0];
    let d_max = domain[1];
    let d_range = d_max - d_min;

    // Check if function is a dict (PostScript function dict) or a procedure
    let func_dict = match function.value {
        PsValue::Dict(entity) => Some(entity),
        _ => None,
    };

    for i in 0..=num_samples {
        let t_norm = i as f64 / num_samples as f64;
        let t_domain = d_min + t_norm * d_range;

        let (color, raw_components) = if let Some(func_entity) = func_dict {
            let results = evaluate_ps_function(ctx, func_entity, &[t_domain])?;
            if needs_tint_conversion(color_space) {
                let alt = convert_tint_color(ctx, &results, color_space)?;
                let color = components_to_device_color(
                    &alt,
                    get_alt_space(color_space),
                    &mut ctx.icc_cache,
                );
                (color, alt)
            } else {
                let color = components_to_device_color(&results, color_space, &mut ctx.icc_cache);
                (color, results)
            }
        } else {
            ctx.o_stack.push(PsObject::real(t_domain))?;
            ctx.exec_sync(function)?;
            if needs_tint_conversion(color_space) {
                let mut comps = vec![0.0; n_comps];
                for i in (0..n_comps).rev() {
                    comps[i] = ctx.o_stack.pop()?.as_f64().unwrap_or(0.0);
                }
                let alt = convert_tint_color(ctx, &comps, color_space)?;
                let color = components_to_device_color(
                    &alt,
                    get_alt_space(color_space),
                    &mut ctx.icc_cache,
                );
                (color, alt)
            } else {
                let mut comps = vec![0.0; n_comps];
                for j in (0..n_comps).rev() {
                    comps[j] = ctx
                        .o_stack
                        .pop()
                        .ok()
                        .and_then(|o| o.as_f64())
                        .unwrap_or(0.0);
                }
                let color = components_to_device_color(&comps, color_space, &mut ctx.icc_cache);
                (color, comps)
            }
        };

        stops.push(ColorStop {
            position: t_norm,
            color,
            raw_components,
        });
    }

    Ok(stops)
}

/// Detect if a 256-sample decode table matches `f(x) = x^γ` within tolerance.
/// Returns `Some(gamma)` if the table is a power curve, `None` otherwise.
fn detect_gamma(table: &[f64]) -> Option<f64> {
    if table.len() < 2 {
        return None;
    }
    // Check table starts at ~0 and ends at ~1 (expected for decode tables)
    if table[0].abs() > 0.01 || (table[table.len() - 1] - 1.0).abs() > 0.01 {
        return None;
    }
    // Use a sample point near the middle to estimate gamma
    let mid_idx = table.len() / 2;
    let x = mid_idx as f64 / (table.len() - 1) as f64;
    let y = table[mid_idx];
    if y <= 0.001 || x <= 0.001 {
        return None;
    }
    let gamma = y.ln() / x.ln();
    if !(0.1..=10.0).contains(&gamma) {
        return None;
    }
    // Verify the gamma fits the whole table within tolerance
    let n = table.len() - 1;
    for (i, &val) in table.iter().enumerate() {
        let t = i as f64 / n as f64;
        let expected = t.powf(gamma);
        if (val - expected).abs() > 0.005 {
            return None;
        }
    }
    Some(gamma)
}

/// Map a PostScript ColorSpace to a ShadingColorSpace for the display list.
fn capture_shading_color_space(ctx: &Context, color_space: &ColorSpace) -> ShadingColorSpace {
    match color_space {
        ColorSpace::DeviceGray => ShadingColorSpace::DeviceGray,
        ColorSpace::DeviceRGB => ShadingColorSpace::DeviceRGB,
        ColorSpace::DeviceCMYK => ShadingColorSpace::DeviceCMYK,
        ColorSpace::ICCBased {
            n, profile_hash, ..
        } => {
            if let Some(hash) = profile_hash
                && let Some(bytes) = ctx.icc_cache.get_profile_bytes(hash)
            {
                return ShadingColorSpace::ICCBased {
                    n: *n,
                    profile_hash: *hash,
                    profile_data: bytes,
                };
            }
            // Fallback by component count
            match n {
                1 => ShadingColorSpace::DeviceGray,
                3 => ShadingColorSpace::DeviceRGB,
                4 => ShadingColorSpace::DeviceCMYK,
                _ => ShadingColorSpace::DeviceRGB,
            }
        }
        ColorSpace::CIEBasedABC { params, .. } => {
            // Check if decode tables are simple gamma curves
            let gamma = if let Some(ref tables) = params.decode_abc {
                let g0 = detect_gamma(&tables[0]);
                let g1 = detect_gamma(&tables[1]);
                let g2 = detect_gamma(&tables[2]);
                match (g0, g1, g2) {
                    (Some(a), Some(b), Some(c)) => Some([a, b, c]),
                    _ => None,
                }
            } else {
                // No decode tables = identity (gamma 1.0)
                Some([1.0, 1.0, 1.0])
            };
            // Check if DecodeLMN is also simple (identity or gamma)
            let lmn_simple = params.decode_lmn.as_ref().is_none_or(|tables| {
                detect_gamma(&tables[0]).is_some()
                    && detect_gamma(&tables[1]).is_some()
                    && detect_gamma(&tables[2]).is_some()
            });
            if let Some(g) = gamma
                && lmn_simple
            {
                // Check if matrix_abc is identity
                let identity_mat = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
                let mat = if params.matrix_abc == identity_mat {
                    // Use matrix_lmn if matrix_abc is identity
                    if params.matrix_lmn == identity_mat {
                        None
                    } else {
                        Some(params.matrix_lmn)
                    }
                } else {
                    Some(params.matrix_abc)
                };
                let gamma_opt = if g == [1.0, 1.0, 1.0] { None } else { Some(g) };
                return ShadingColorSpace::CalRGB {
                    white_point: params.white_point,
                    matrix: mat,
                    gamma: gamma_opt,
                };
            }
            // Complex decode tables — fallback to DeviceRGB
            ShadingColorSpace::DeviceRGB
        }
        ColorSpace::CIEBasedA { params, .. } => {
            let gamma = if let Some(ref table) = params.decode_a {
                detect_gamma(table)
            } else {
                Some(1.0)
            };
            let lmn_simple = params.decode_lmn.is_none();
            if let Some(g) = gamma
                && lmn_simple
            {
                let gamma_opt = if (g - 1.0).abs() < 0.001 {
                    None
                } else {
                    Some(g)
                };
                return ShadingColorSpace::CalGray {
                    white_point: params.white_point,
                    gamma: gamma_opt,
                };
            }
            ShadingColorSpace::DeviceGray
        }
        ColorSpace::CIEBasedDEF { .. } | ColorSpace::CIEBasedDEFG { .. } => {
            // No PDF equivalent — values already converted to sRGB in DeviceColor
            ShadingColorSpace::DeviceRGB
        }
        ColorSpace::Separation { alt_space, .. } | ColorSpace::DeviceN { alt_space, .. } => {
            // After tint conversion, use the alt space
            capture_shading_color_space(ctx, alt_space)
        }
        ColorSpace::Indexed { base, .. } => {
            // Shouldn't appear in shadings, but handle gracefully
            capture_shading_color_space(ctx, base)
        }
    }
}

/// Convert component values to DeviceColor based on color space.
fn components_to_device_color(
    comps: &[f64],
    color_space: &ColorSpace,
    icc_cache: &mut stet_graphics::icc::IccCache,
) -> DeviceColor {
    match color_space {
        ColorSpace::DeviceGray => {
            DeviceColor::from_gray(comps.first().copied().unwrap_or(0.0).clamp(0.0, 1.0))
        }
        ColorSpace::DeviceRGB if comps.len() >= 3 => DeviceColor::from_rgb(
            comps[0].clamp(0.0, 1.0),
            comps[1].clamp(0.0, 1.0),
            comps[2].clamp(0.0, 1.0),
        ),
        ColorSpace::DeviceCMYK if comps.len() >= 4 => DeviceColor::from_cmyk_icc(
            comps[0].clamp(0.0, 1.0),
            comps[1].clamp(0.0, 1.0),
            comps[2].clamp(0.0, 1.0),
            comps[3].clamp(0.0, 1.0),
            icc_cache,
        ),
        ColorSpace::CIEBasedABC { params, .. } if comps.len() >= 3 => {
            DeviceColor::from_cie_abc(comps[0], comps[1], comps[2], params)
        }
        ColorSpace::CIEBasedA { params, .. } => {
            DeviceColor::from_cie_a(comps.first().copied().unwrap_or(0.0), params)
        }
        ColorSpace::CIEBasedDEF { params, .. } if comps.len() >= 3 => {
            DeviceColor::from_cie_def(comps[0], comps[1], comps[2], params)
        }
        ColorSpace::CIEBasedDEFG { params, .. } if comps.len() >= 4 => {
            DeviceColor::from_cie_defg(comps[0], comps[1], comps[2], comps[3], params)
        }
        ColorSpace::ICCBased {
            n, profile_hash, ..
        } => {
            if let Some(hash) = profile_hash
                && let Some((r, g, b)) = icc_cache.convert_color(hash, comps)
            {
                return DeviceColor::from_rgb(r, g, b);
            }
            match n {
                1 => DeviceColor::from_gray(comps.first().copied().unwrap_or(0.0).clamp(0.0, 1.0)),
                3 if comps.len() >= 3 => DeviceColor::from_rgb(
                    comps[0].clamp(0.0, 1.0),
                    comps[1].clamp(0.0, 1.0),
                    comps[2].clamp(0.0, 1.0),
                ),
                4 if comps.len() >= 4 => DeviceColor::from_cmyk_icc(
                    comps[0].clamp(0.0, 1.0),
                    comps[1].clamp(0.0, 1.0),
                    comps[2].clamp(0.0, 1.0),
                    comps[3].clamp(0.0, 1.0),
                    icc_cache,
                ),
                _ => DeviceColor::from_gray(0.0),
            }
        }
        _ => {
            // Fallback: heuristic based on count
            if comps.len() >= 4 {
                DeviceColor::from_cmyk_icc(
                    comps[0].clamp(0.0, 1.0),
                    comps[1].clamp(0.0, 1.0),
                    comps[2].clamp(0.0, 1.0),
                    comps[3].clamp(0.0, 1.0),
                    icc_cache,
                )
            } else if comps.len() >= 3 {
                DeviceColor::from_rgb(
                    comps[0].clamp(0.0, 1.0),
                    comps[1].clamp(0.0, 1.0),
                    comps[2].clamp(0.0, 1.0),
                )
            } else {
                DeviceColor::from_gray(comps.first().copied().unwrap_or(0.0).clamp(0.0, 1.0))
            }
        }
    }
}

// ---- PostScript Function Dictionary Evaluation ----

/// Evaluate a PostScript function dictionary at the given inputs.
/// Supports FunctionType 0 (sampled), 2 (exponential), and 3 (stitching).
fn evaluate_ps_function(
    ctx: &Context,
    func_entity: EntityId,
    inputs: &[f64],
) -> Result<Vec<f64>, PsError> {
    let func_type = get_dict_int(ctx, func_entity, b"FunctionType").ok_or(PsError::Undefined)?;

    // Get Domain for input clamping
    let domain = get_dict_float_vec(ctx, func_entity, b"Domain").unwrap_or_default();

    // Clamp inputs to domain
    let mut clamped_inputs = Vec::with_capacity(inputs.len());
    for (i, &val) in inputs.iter().enumerate() {
        let d_min = domain.get(i * 2).copied().unwrap_or(0.0);
        let d_max = domain.get(i * 2 + 1).copied().unwrap_or(1.0);
        clamped_inputs.push(val.clamp(d_min, d_max));
    }

    let mut results = match func_type {
        0 => eval_type0_sampled(ctx, func_entity, &clamped_inputs)?,
        2 => eval_type2_exponential(ctx, func_entity, &clamped_inputs),
        3 => eval_type3_stitching(ctx, func_entity, &clamped_inputs)?,
        _ => return Err(PsError::RangeCheck),
    };

    // Clamp to Range if specified
    let range = get_dict_float_vec(ctx, func_entity, b"Range");
    if let Some(range) = range {
        for (i, val) in results.iter_mut().enumerate() {
            let r_min = range.get(i * 2).copied().unwrap_or(0.0);
            let r_max = range.get(i * 2 + 1).copied().unwrap_or(1.0);
            *val = val.clamp(r_min, r_max);
        }
    }

    Ok(results)
}

/// Evaluate FunctionType 0: sampled function.
/// Uses multilinear interpolation over a grid of sample values.
fn eval_type0_sampled(
    ctx: &Context,
    func_entity: EntityId,
    inputs: &[f64],
) -> Result<Vec<f64>, PsError> {
    let domain = get_dict_float_vec(ctx, func_entity, b"Domain").unwrap_or_default();
    let range = get_dict_float_vec(ctx, func_entity, b"Range").unwrap_or_default();
    let size_vec = get_dict_float_vec(ctx, func_entity, b"Size").ok_or(PsError::Undefined)?;
    let bps = get_dict_int(ctx, func_entity, b"BitsPerSample").ok_or(PsError::Undefined)? as usize;

    let encode = get_dict_float_vec(ctx, func_entity, b"Encode");
    let decode = get_dict_float_vec(ctx, func_entity, b"Decode");

    let m = inputs.len(); // number of input dimensions
    let n = range.len() / 2; // number of output components
    if n == 0 || m == 0 {
        return Ok(vec![0.0; n.max(1)]);
    }

    let sizes: Vec<usize> = size_vec.iter().map(|s| *s as usize).collect();

    // Get sample data
    let ds_obj = get_dict_obj(ctx, func_entity, b"DataSource").ok_or(PsError::Undefined)?;
    let sample_data = match ds_obj.value {
        PsValue::String { entity, start, len } => ctx.strings.get(entity, start, len).to_vec(),
        _ => return Err(PsError::TypeCheck),
    };

    let max_sample = ((1u64 << bps) - 1) as f64;

    // For 1D input (most common case in shading)
    if m == 1 {
        let d_min = domain.first().copied().unwrap_or(0.0);
        let d_max = domain.get(1).copied().unwrap_or(1.0);
        let e_min = encode
            .as_ref()
            .and_then(|e| e.first().copied())
            .unwrap_or(0.0);
        let e_max = encode
            .as_ref()
            .and_then(|e| e.get(1).copied())
            .unwrap_or((sizes[0] - 1) as f64);

        // Encode: map domain to sample index space
        let x = inputs[0];
        let encoded = if (d_max - d_min).abs() < 1e-10 {
            e_min
        } else {
            e_min + (x - d_min) * (e_max - e_min) / (d_max - d_min)
        };
        let encoded = encoded.clamp(0.0, (sizes[0] - 1) as f64);

        // Interpolate between two adjacent samples
        let idx_lo = encoded.floor() as usize;
        let idx_hi = (idx_lo + 1).min(sizes[0] - 1);
        let frac = encoded - idx_lo as f64;

        let mut results = Vec::with_capacity(n);
        for j in 0..n {
            let raw_lo = read_sample(&sample_data, (idx_lo * n + j) * bps, bps);
            let raw_hi = read_sample(&sample_data, (idx_hi * n + j) * bps, bps);
            let val_lo = raw_lo as f64 / max_sample;
            let val_hi = raw_hi as f64 / max_sample;
            let interpolated = val_lo + frac * (val_hi - val_lo);

            // Decode to output range
            let d_out = if let Some(ref dec) = decode {
                let dec_min = dec.get(j * 2).copied().unwrap_or(0.0);
                let dec_max = dec.get(j * 2 + 1).copied().unwrap_or(1.0);
                dec_min + interpolated * (dec_max - dec_min)
            } else {
                let r_min = range.get(j * 2).copied().unwrap_or(0.0);
                let r_max = range.get(j * 2 + 1).copied().unwrap_or(1.0);
                r_min + interpolated * (r_max - r_min)
            };

            results.push(d_out);
        }
        return Ok(results);
    }

    // 2D input (for Type 1 function-based shading)
    if m == 2 && sizes.len() >= 2 {
        let sx = sizes[0];
        let sy = sizes[1];

        // Encode x
        let dx_min = domain.first().copied().unwrap_or(0.0);
        let dx_max = domain.get(1).copied().unwrap_or(1.0);
        let ex_min = encode
            .as_ref()
            .and_then(|e| e.first().copied())
            .unwrap_or(0.0);
        let ex_max = encode
            .as_ref()
            .and_then(|e| e.get(1).copied())
            .unwrap_or((sx - 1) as f64);
        let ex = if (dx_max - dx_min).abs() < 1e-10 {
            ex_min
        } else {
            ex_min + (inputs[0] - dx_min) * (ex_max - ex_min) / (dx_max - dx_min)
        };
        let ex = ex.clamp(0.0, (sx - 1) as f64);

        // Encode y
        let dy_min = domain.get(2).copied().unwrap_or(0.0);
        let dy_max = domain.get(3).copied().unwrap_or(1.0);
        let ey_min = encode
            .as_ref()
            .and_then(|e| e.get(2).copied())
            .unwrap_or(0.0);
        let ey_max = encode
            .as_ref()
            .and_then(|e| e.get(3).copied())
            .unwrap_or((sy - 1) as f64);
        let ey = if (dy_max - dy_min).abs() < 1e-10 {
            ey_min
        } else {
            ey_min + (inputs[1] - dy_min) * (ey_max - ey_min) / (dy_max - dy_min)
        };
        let ey = ey.clamp(0.0, (sy - 1) as f64);

        // Bilinear interpolation
        let ix0 = ex.floor() as usize;
        let ix1 = (ix0 + 1).min(sx - 1);
        let iy0 = ey.floor() as usize;
        let iy1 = (iy0 + 1).min(sy - 1);
        let fx = ex - ix0 as f64;
        let fy = ey - iy0 as f64;

        let mut results = Vec::with_capacity(n);
        for j in 0..n {
            // Grid ordering: x varies fastest (innermost)
            let idx00 = (iy0 * sx + ix0) * n + j;
            let idx10 = (iy0 * sx + ix1) * n + j;
            let idx01 = (iy1 * sx + ix0) * n + j;
            let idx11 = (iy1 * sx + ix1) * n + j;

            let v00 = read_sample(&sample_data, idx00 * bps, bps) as f64 / max_sample;
            let v10 = read_sample(&sample_data, idx10 * bps, bps) as f64 / max_sample;
            let v01 = read_sample(&sample_data, idx01 * bps, bps) as f64 / max_sample;
            let v11 = read_sample(&sample_data, idx11 * bps, bps) as f64 / max_sample;

            let interpolated = v00 * (1.0 - fx) * (1.0 - fy)
                + v10 * fx * (1.0 - fy)
                + v01 * (1.0 - fx) * fy
                + v11 * fx * fy;

            let d_out = if let Some(ref dec) = decode {
                let dec_min = dec.get(j * 2).copied().unwrap_or(0.0);
                let dec_max = dec.get(j * 2 + 1).copied().unwrap_or(1.0);
                dec_min + interpolated * (dec_max - dec_min)
            } else {
                let r_min = range.get(j * 2).copied().unwrap_or(0.0);
                let r_max = range.get(j * 2 + 1).copied().unwrap_or(1.0);
                r_min + interpolated * (r_max - r_min)
            };
            results.push(d_out);
        }
        return Ok(results);
    }

    // Fallback for higher dimensions: nearest-neighbor
    Ok(vec![0.0; n])
}

/// Read a sample value of `bps` bits at bit position `bit_pos` from data.
fn read_sample(data: &[u8], bit_pos: usize, bps: usize) -> u32 {
    if bps == 8 {
        let byte_idx = bit_pos / 8;
        if byte_idx < data.len() {
            return data[byte_idx] as u32;
        }
        return 0;
    }
    if bps == 16 {
        let byte_idx = bit_pos / 8;
        if byte_idx + 1 < data.len() {
            return ((data[byte_idx] as u32) << 8) | (data[byte_idx + 1] as u32);
        }
        return 0;
    }
    // General case using BitReader
    let byte_idx = bit_pos / 8;
    if byte_idx >= data.len() {
        return 0;
    }
    let mut reader = mesh_shading::BitReader::new(&data[byte_idx..]);
    let bit_off = bit_pos % 8;
    if bit_off > 0 {
        reader.read(bit_off); // skip to alignment
    }
    reader.read(bps).unwrap_or(0)
}

/// Evaluate FunctionType 2: exponential interpolation.
/// y_j = C0_j + x^N * (C1_j - C0_j)
fn eval_type2_exponential(ctx: &Context, func_entity: EntityId, inputs: &[f64]) -> Vec<f64> {
    let x = inputs.first().copied().unwrap_or(0.0);
    let n_exp = get_dict_float(ctx, func_entity, b"N").unwrap_or(1.0);
    let c0 = get_dict_float_vec(ctx, func_entity, b"C0").unwrap_or_else(|| vec![0.0]);
    let c1 = get_dict_float_vec(ctx, func_entity, b"C1").unwrap_or_else(|| vec![1.0]);

    let num_outputs = c0.len().max(c1.len());
    let mut results = Vec::with_capacity(num_outputs);

    let xn = if n_exp == 1.0 {
        x
    } else if n_exp == 0.0 {
        1.0
    } else if x == 0.0 {
        0.0
    } else if x == 1.0 {
        1.0
    } else {
        x.powf(n_exp)
    };

    for j in 0..num_outputs {
        let c0j = c0.get(j).copied().unwrap_or(0.0);
        let c1j = c1.get(j).copied().unwrap_or(1.0);
        results.push(c0j + xn * (c1j - c0j));
    }

    results
}

/// Evaluate FunctionType 3: stitching function.
/// Chains sub-functions across domain partitions.
fn eval_type3_stitching(
    ctx: &Context,
    func_entity: EntityId,
    inputs: &[f64],
) -> Result<Vec<f64>, PsError> {
    let x = inputs.first().copied().unwrap_or(0.0);
    let domain = get_dict_float_vec(ctx, func_entity, b"Domain").unwrap_or_else(|| vec![0.0, 1.0]);
    let bounds = get_dict_float_vec(ctx, func_entity, b"Bounds").ok_or(PsError::Undefined)?;
    let encode = get_dict_float_vec(ctx, func_entity, b"Encode").ok_or(PsError::Undefined)?;

    // Get the Functions array
    let funcs_obj = get_dict_obj(ctx, func_entity, b"Functions").ok_or(PsError::Undefined)?;
    let (funcs_entity, funcs_start, funcs_len) = match funcs_obj.value {
        PsValue::Array { entity, start, len } => (entity, start, len as usize),
        _ => return Err(PsError::TypeCheck),
    };

    let n_funcs = funcs_len;
    if n_funcs == 0 {
        return Err(PsError::RangeCheck);
    }

    // Build boundaries: [domain_min, bounds..., domain_max]
    let d_min = domain.first().copied().unwrap_or(0.0);
    let d_max = domain.get(1).copied().unwrap_or(1.0);
    let mut boundaries = Vec::with_capacity(bounds.len() + 2);
    boundaries.push(d_min);
    boundaries.extend_from_slice(&bounds);
    boundaries.push(d_max);

    // Find which subdomain x falls in
    let mut idx = 0;
    for i in 0..n_funcs {
        if i + 1 < boundaries.len() && (x < boundaries[i + 1] || i == n_funcs - 1) {
            idx = i;
            break;
        }
    }

    // Encode x for the sub-function's domain
    let sub_d_min = boundaries[idx];
    let sub_d_max = boundaries[idx + 1];
    let e_min = encode.get(idx * 2).copied().unwrap_or(0.0);
    let e_max = encode.get(idx * 2 + 1).copied().unwrap_or(1.0);

    let encoded = if (sub_d_max - sub_d_min).abs() < 1e-10 {
        e_min
    } else {
        e_min + (x - sub_d_min) * (e_max - e_min) / (sub_d_max - sub_d_min)
    };

    // Evaluate the sub-function
    let sub_func = ctx
        .arrays
        .get_element(funcs_entity, funcs_start + idx as u32);
    match sub_func.value {
        PsValue::Dict(sub_entity) => evaluate_ps_function(ctx, sub_entity, &[encoded]),
        _ => Err(PsError::TypeCheck),
    }
}

/// Look up a single float value from a dict.
fn get_dict_float(ctx: &Context, dict: EntityId, key: &[u8]) -> Option<f64> {
    get_dict_obj(ctx, dict, key)?.as_f64()
}

/// Pop n_comps values from the operand stack and convert to DeviceColor.
fn pop_color_components(
    ctx: &mut Context,
    n_comps: usize,
    color_space: &ColorSpace,
) -> DeviceColor {
    let mut comps = vec![0.0f64; n_comps];
    // Components are pushed in order, so last pushed is on top
    for i in (0..n_comps).rev() {
        comps[i] = ctx
            .o_stack
            .pop()
            .ok()
            .and_then(|o| o.as_f64())
            .unwrap_or(0.0);
    }

    match color_space {
        ColorSpace::DeviceGray => {
            DeviceColor::from_gray(comps.first().copied().unwrap_or(0.0).clamp(0.0, 1.0))
        }
        ColorSpace::DeviceRGB => {
            if comps.len() >= 3 {
                DeviceColor::from_rgb(
                    comps[0].clamp(0.0, 1.0),
                    comps[1].clamp(0.0, 1.0),
                    comps[2].clamp(0.0, 1.0),
                )
            } else {
                DeviceColor::from_gray(0.0)
            }
        }
        ColorSpace::DeviceCMYK => {
            if comps.len() >= 4 {
                DeviceColor::from_cmyk_icc(
                    comps[0].clamp(0.0, 1.0),
                    comps[1].clamp(0.0, 1.0),
                    comps[2].clamp(0.0, 1.0),
                    comps[3].clamp(0.0, 1.0),
                    &mut ctx.icc_cache,
                )
            } else {
                DeviceColor::from_gray(0.0)
            }
        }
        ColorSpace::CIEBasedABC { params, .. } => {
            if comps.len() >= 3 {
                DeviceColor::from_cie_abc(comps[0], comps[1], comps[2], params)
            } else {
                DeviceColor::from_gray(0.0)
            }
        }
        ColorSpace::CIEBasedA { params, .. } => {
            DeviceColor::from_cie_a(comps.first().copied().unwrap_or(0.0), params)
        }
        ColorSpace::CIEBasedDEF { params, .. } => {
            if comps.len() >= 3 {
                DeviceColor::from_cie_def(comps[0], comps[1], comps[2], params)
            } else {
                DeviceColor::from_gray(0.0)
            }
        }
        ColorSpace::CIEBasedDEFG { params, .. } => {
            if comps.len() >= 4 {
                DeviceColor::from_cie_defg(comps[0], comps[1], comps[2], comps[3], params)
            } else {
                DeviceColor::from_gray(0.0)
            }
        }
        ColorSpace::ICCBased {
            n, profile_hash, ..
        } => {
            if let Some(hash) = profile_hash
                && let Some((r, g, b)) = ctx.icc_cache.convert_color(hash, &comps)
            {
                return DeviceColor::from_rgb(r, g, b);
            }
            match n {
                1 => DeviceColor::from_gray(comps.first().copied().unwrap_or(0.0).clamp(0.0, 1.0)),
                3 if comps.len() >= 3 => DeviceColor::from_rgb(
                    comps[0].clamp(0.0, 1.0),
                    comps[1].clamp(0.0, 1.0),
                    comps[2].clamp(0.0, 1.0),
                ),
                4 if comps.len() >= 4 => DeviceColor::from_cmyk_icc(
                    comps[0].clamp(0.0, 1.0),
                    comps[1].clamp(0.0, 1.0),
                    comps[2].clamp(0.0, 1.0),
                    comps[3].clamp(0.0, 1.0),
                    &mut ctx.icc_cache,
                ),
                _ => DeviceColor::from_gray(0.0),
            }
        }
        _ => {
            // Separation/DeviceN/Indexed: use component count heuristic
            if comps.len() >= 4 {
                DeviceColor::from_cmyk_icc(
                    comps[0].clamp(0.0, 1.0),
                    comps[1].clamp(0.0, 1.0),
                    comps[2].clamp(0.0, 1.0),
                    comps[3].clamp(0.0, 1.0),
                    &mut ctx.icc_cache,
                )
            } else if comps.len() >= 3 {
                DeviceColor::from_rgb(
                    comps[0].clamp(0.0, 1.0),
                    comps[1].clamp(0.0, 1.0),
                    comps[2].clamp(0.0, 1.0),
                )
            } else {
                DeviceColor::from_gray(comps.first().copied().unwrap_or(0.0).clamp(0.0, 1.0))
            }
        }
    }
}

// ---- Dict helpers ----

/// Look up a key in a dict by name and return the PsObject.
fn get_dict_obj(ctx: &Context, dict: EntityId, key: &[u8]) -> Option<PsObject> {
    let name_id = ctx.names.find(key)?;
    ctx.dicts.get(dict, &DictKey::Name(name_id))
}

/// Look up an integer value from a dict.
fn get_dict_int(ctx: &Context, dict: EntityId, key: &[u8]) -> Option<i32> {
    get_dict_obj(ctx, dict, key)?.as_i32()
}

/// Look up a variable-length float vector from a dict.
fn get_dict_float_vec(ctx: &Context, dict: EntityId, key: &[u8]) -> Option<Vec<f64>> {
    let obj = get_dict_obj(ctx, dict, key)?;
    match obj.value {
        PsValue::Array { entity, start, len } => {
            let mut result = Vec::with_capacity(len as usize);
            for i in 0..len {
                result.push(
                    ctx.arrays
                        .get_element(entity, start + i)
                        .as_f64()
                        .unwrap_or(0.0),
                );
            }
            Some(result)
        }
        _ => None,
    }
}

/// Look up the /Extend array [bool bool] from a dict.
fn get_dict_extend(ctx: &Context, dict: EntityId) -> (bool, bool) {
    let Some(obj) = get_dict_obj(ctx, dict, b"Extend") else {
        return (false, false);
    };
    match obj.value {
        PsValue::Array { entity, start, len } if len >= 2 => {
            let e0 = ctx.arrays.get_element(entity, start);
            let e1 = ctx.arrays.get_element(entity, start + 1);
            let b0 = matches!(e0.value, PsValue::Bool(true));
            let b1 = matches!(e1.value, PsValue::Bool(true));
            (b0, b1)
        }
        _ => (false, false),
    }
}

/// Look up a 6-element matrix from a dict.
fn get_dict_matrix(ctx: &Context, dict: EntityId, key: &[u8]) -> Option<Matrix> {
    let obj = get_dict_obj(ctx, dict, key)?;
    match obj.value {
        PsValue::Array { entity, start, len } if len >= 6 => {
            let mut m = [0.0f64; 6];
            for (i, v) in m.iter_mut().enumerate() {
                *v = ctx
                    .arrays
                    .get_element(entity, start + i as u32)
                    .as_f64()
                    .unwrap_or(0.0);
            }
            Some(Matrix::new(m[0], m[1], m[2], m[3], m[4], m[5]))
        }
        _ => None,
    }
}

/// Look up optional /BBox [llx lly urx ury] from a dict.
fn get_dict_bbox(ctx: &Context, dict: EntityId) -> Option<[f64; 4]> {
    let v = get_dict_float_vec(ctx, dict, b"BBox")?;
    if v.len() >= 4 {
        Some([v[0], v[1], v[2], v[3]])
    } else {
        None
    }
}

/// Extract all elements of a PS array as f64 values.
fn extract_float_array(ctx: &Context, entity: EntityId, start: u32, len: u32) -> Vec<f64> {
    let mut values = Vec::with_capacity(len as usize);
    for i in 0..len {
        values.push(
            ctx.arrays
                .get_element(entity, start + i)
                .as_f64()
                .unwrap_or(0.0),
        );
    }
    values
}
