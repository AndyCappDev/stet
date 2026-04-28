// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Convert display list shading parameters to PDF shading dictionaries.

use stet_graphics::device::{
    AxialShadingParams, ColorStop, MeshShadingParams, PatchShadingParams, RadialShadingParams,
    ShadingColorSpace,
};

use crate::pdf_objects::PdfObj;
use crate::pdf_writer::PdfWriter;

/// Convert a ShadingColorSpace to a PDF color space object.
/// For ICCBased, emits the profile stream and returns an array reference.
fn shading_color_space_to_pdf(writer: &mut PdfWriter, cs: &ShadingColorSpace) -> PdfObj {
    match cs {
        ShadingColorSpace::DeviceGray => PdfObj::name("DeviceGray"),
        ShadingColorSpace::DeviceRGB => PdfObj::name("DeviceRGB"),
        ShadingColorSpace::DeviceCMYK => PdfObj::name("DeviceCMYK"),
        ShadingColorSpace::ICCBased {
            n, profile_data, ..
        } => {
            let stream_entries = vec![(b"N".to_vec(), PdfObj::Int(*n as i64))];
            let stream_ref = writer.add_stream(stream_entries, profile_data, true);
            PdfObj::Array(vec![PdfObj::name("ICCBased"), PdfObj::Ref(stream_ref)])
        }
        ShadingColorSpace::CalRGB {
            white_point,
            matrix,
            gamma,
        } => {
            let mut dict_entries = vec![(
                b"WhitePoint".to_vec(),
                PdfObj::Array(white_point.iter().map(|&v| PdfObj::Real(v)).collect()),
            )];
            if let Some(mat) = matrix {
                dict_entries.push((
                    b"Matrix".to_vec(),
                    PdfObj::Array(mat.iter().map(|&v| PdfObj::Real(v)).collect()),
                ));
            }
            if let Some(g) = gamma {
                dict_entries.push((
                    b"Gamma".to_vec(),
                    PdfObj::Array(g.iter().map(|&v| PdfObj::Real(v)).collect()),
                ));
            }
            PdfObj::Array(vec![PdfObj::name("CalRGB"), PdfObj::Dict(dict_entries)])
        }
        ShadingColorSpace::CalGray { white_point, gamma } => {
            let mut dict_entries = vec![(
                b"WhitePoint".to_vec(),
                PdfObj::Array(white_point.iter().map(|&v| PdfObj::Real(v)).collect()),
            )];
            if let Some(g) = gamma {
                dict_entries.push((b"Gamma".to_vec(), PdfObj::Real(*g)));
            }
            PdfObj::Array(vec![PdfObj::name("CalGray"), PdfObj::Dict(dict_entries)])
        }
        _ => PdfObj::name("DeviceRGB"),
    }
}

/// Build a PDF axial shading and add all needed objects to the writer.
/// Returns the shading dict object number.
pub fn build_axial_shading(writer: &mut PdfWriter, params: &AxialShadingParams) -> u32 {
    let n_comps = params.color_space.num_components();
    let func_ref = build_sampled_function(writer, &params.color_stops, n_comps);
    let cs_obj = shading_color_space_to_pdf(writer, &params.color_space);

    let mut entries = vec![
        (b"ShadingType".to_vec(), PdfObj::Int(2)),
        (b"ColorSpace".to_vec(), cs_obj),
        (
            b"Coords".to_vec(),
            PdfObj::Array(vec![
                PdfObj::Real(params.x0),
                PdfObj::Real(params.y0),
                PdfObj::Real(params.x1),
                PdfObj::Real(params.y1),
            ]),
        ),
        (b"Function".to_vec(), PdfObj::Ref(func_ref)),
        (
            b"Extend".to_vec(),
            PdfObj::Array(vec![
                PdfObj::Bool(params.extend_start),
                PdfObj::Bool(params.extend_end),
            ]),
        ),
    ];

    if let Some(bbox) = params.bbox {
        entries.push((
            b"BBox".to_vec(),
            PdfObj::Array(bbox.iter().map(|&v| PdfObj::Real(v)).collect()),
        ));
    }

    writer.add_object(&PdfObj::Dict(entries))
}

/// Build a PDF radial shading and add all needed objects to the writer.
/// Returns the shading dict object number.
pub fn build_radial_shading(writer: &mut PdfWriter, params: &RadialShadingParams) -> u32 {
    let n_comps = params.color_space.num_components();
    let func_ref = build_sampled_function(writer, &params.color_stops, n_comps);
    let cs_obj = shading_color_space_to_pdf(writer, &params.color_space);

    let mut entries = vec![
        (b"ShadingType".to_vec(), PdfObj::Int(3)),
        (b"ColorSpace".to_vec(), cs_obj),
        (
            b"Coords".to_vec(),
            PdfObj::Array(vec![
                PdfObj::Real(params.x0),
                PdfObj::Real(params.y0),
                PdfObj::Real(params.r0),
                PdfObj::Real(params.x1),
                PdfObj::Real(params.y1),
                PdfObj::Real(params.r1),
            ]),
        ),
        (b"Function".to_vec(), PdfObj::Ref(func_ref)),
        (
            b"Extend".to_vec(),
            PdfObj::Array(vec![
                PdfObj::Bool(params.extend_start),
                PdfObj::Bool(params.extend_end),
            ]),
        ),
    ];

    if let Some(bbox) = params.bbox {
        entries.push((
            b"BBox".to_vec(),
            PdfObj::Array(bbox.iter().map(|&v| PdfObj::Real(v)).collect()),
        ));
    }

    writer.add_object(&PdfObj::Dict(entries))
}

/// Build a PDF Type 4 free-form Gouraud-shaded triangle mesh.
/// Returns the shading stream object number.
pub fn build_mesh_shading(writer: &mut PdfWriter, params: &MeshShadingParams) -> u32 {
    let n_comps = params.color_space.num_components();
    let cs_obj = shading_color_space_to_pdf(writer, &params.color_space);

    if params.triangles.is_empty() {
        return writer.add_object(&PdfObj::Dict(vec![
            (b"ShadingType".to_vec(), PdfObj::Int(4)),
            (b"ColorSpace".to_vec(), cs_obj),
        ]));
    }

    // Compute bounding box of all vertices
    let mut x_min = f64::MAX;
    let mut x_max = f64::MIN;
    let mut y_min = f64::MAX;
    let mut y_max = f64::MIN;
    for tri in &params.triangles {
        for v in [&tri.v0, &tri.v1, &tri.v2] {
            x_min = x_min.min(v.x);
            x_max = x_max.max(v.x);
            y_min = y_min.min(v.y);
            y_max = y_max.max(v.y);
        }
    }
    // Expand slightly to avoid edge clipping
    let margin = ((x_max - x_min).max(y_max - y_min)) * 0.001 + 1.0;
    x_min -= margin;
    x_max += margin;
    y_min -= margin;
    y_max += margin;

    // Encode vertices as binary stream
    let mut data = Vec::new();
    for tri in &params.triangles {
        for v in [&tri.v0, &tri.v1, &tri.v2] {
            // Flag: 0 for each vertex of independent triangles
            data.push(0u8);
            // X coordinate (16-bit)
            data.extend(encode_coord_16(v.x, x_min, x_max).to_be_bytes());
            // Y coordinate (16-bit)
            data.extend(encode_coord_16(v.y, y_min, y_max).to_be_bytes());
            // Color components (8-bit each)
            encode_color_components(&v.raw_components, &v.color, n_comps, &mut data);
        }
    }

    // Build Decode array: [xmin xmax ymin ymax c0min c0max c1min c1max ...]
    let mut decode_array = vec![
        PdfObj::Real(x_min),
        PdfObj::Real(x_max),
        PdfObj::Real(y_min),
        PdfObj::Real(y_max),
    ];
    for _ in 0..n_comps {
        decode_array.push(PdfObj::Int(0));
        decode_array.push(PdfObj::Int(1));
    }

    let dict_entries = vec![
        (b"ShadingType".to_vec(), PdfObj::Int(4)),
        (b"ColorSpace".to_vec(), cs_obj),
        (b"BitsPerCoordinate".to_vec(), PdfObj::Int(16)),
        (b"BitsPerComponent".to_vec(), PdfObj::Int(8)),
        (b"BitsPerFlag".to_vec(), PdfObj::Int(8)),
        (b"Decode".to_vec(), PdfObj::Array(decode_array)),
    ];

    writer.add_stream(dict_entries, &data, true)
}

/// Build a PDF Type 6 (Coons) or Type 7 (tensor-product) patch mesh.
/// Returns the shading stream object number.
pub fn build_patch_shading(writer: &mut PdfWriter, params: &PatchShadingParams) -> u32 {
    let n_comps = params.color_space.num_components();
    let cs_obj = shading_color_space_to_pdf(writer, &params.color_space);

    if params.patches.is_empty() {
        return writer.add_object(&PdfObj::Dict(vec![
            (b"ShadingType".to_vec(), PdfObj::Int(6)),
            (b"ColorSpace".to_vec(), cs_obj),
        ]));
    }

    // Determine shading type from number of control points
    let shading_type = if params.patches[0].points.len() >= 16 {
        7 // tensor-product
    } else {
        6 // Coons
    };

    // Compute bounding box
    let mut x_min = f64::MAX;
    let mut x_max = f64::MIN;
    let mut y_min = f64::MAX;
    let mut y_max = f64::MIN;
    for patch in &params.patches {
        for &(x, y) in &patch.points {
            x_min = x_min.min(x);
            x_max = x_max.max(x);
            y_min = y_min.min(y);
            y_max = y_max.max(y);
        }
    }
    let margin = ((x_max - x_min).max(y_max - y_min)) * 0.001 + 1.0;
    x_min -= margin;
    x_max += margin;
    y_min -= margin;
    y_max += margin;

    // Encode patches as binary stream
    let mut data = Vec::new();
    for patch in &params.patches {
        // Flag: 0 = new independent patch
        data.push(0u8);
        // Control points
        for &(x, y) in &patch.points {
            data.extend(encode_coord_16(x, x_min, x_max).to_be_bytes());
            data.extend(encode_coord_16(y, y_min, y_max).to_be_bytes());
        }
        // Corner colors (4 corners × N components)
        for (i, color) in patch.colors.iter().enumerate() {
            encode_color_components(&patch.raw_colors[i], color, n_comps, &mut data);
        }
    }

    // Build Decode array
    let mut decode_array = vec![
        PdfObj::Real(x_min),
        PdfObj::Real(x_max),
        PdfObj::Real(y_min),
        PdfObj::Real(y_max),
    ];
    for _ in 0..n_comps {
        decode_array.push(PdfObj::Int(0));
        decode_array.push(PdfObj::Int(1));
    }

    let dict_entries = vec![
        (b"ShadingType".to_vec(), PdfObj::Int(shading_type)),
        (b"ColorSpace".to_vec(), cs_obj),
        (b"BitsPerCoordinate".to_vec(), PdfObj::Int(16)),
        (b"BitsPerComponent".to_vec(), PdfObj::Int(8)),
        (b"BitsPerFlag".to_vec(), PdfObj::Int(8)),
        (b"Decode".to_vec(), PdfObj::Array(decode_array)),
    ];

    writer.add_stream(dict_entries, &data, true)
}

/// Build a Type 0 (sampled) function from color stops.
/// Samples N-component values and returns the function stream object number.
fn build_sampled_function(writer: &mut PdfWriter, stops: &[ColorStop], n_comps: usize) -> u32 {
    let n_samples = 256;
    let mut samples = Vec::with_capacity(n_samples * n_comps);

    for i in 0..n_samples {
        let t = i as f64 / (n_samples - 1) as f64;
        let comps = sample_color_stops_raw(stops, t, n_comps);
        for c in comps {
            samples.push((c.clamp(0.0, 1.0) * 255.0) as u8);
        }
    }

    // Range array: N components × [0 1]
    let mut range = Vec::with_capacity(n_comps * 2);
    for _ in 0..n_comps {
        range.push(PdfObj::Int(0));
        range.push(PdfObj::Int(1));
    }

    let dict_entries = vec![
        (b"FunctionType".to_vec(), PdfObj::Int(0)),
        (
            b"Domain".to_vec(),
            PdfObj::Array(vec![PdfObj::Int(0), PdfObj::Int(1)]),
        ),
        (b"Range".to_vec(), PdfObj::Array(range)),
        (
            b"Size".to_vec(),
            PdfObj::Array(vec![PdfObj::Int(n_samples as i64)]),
        ),
        (b"BitsPerSample".to_vec(), PdfObj::Int(8)),
    ];

    writer.add_stream(dict_entries, &samples, true)
}

/// Interpolate color stops at position t (0..1), returning raw component values.
/// Falls back to RGB from DeviceColor if raw_components are empty.
fn sample_color_stops_raw(stops: &[ColorStop], t: f64, n_comps: usize) -> Vec<f64> {
    if stops.is_empty() {
        return vec![0.0; n_comps];
    }
    if stops.len() == 1 || t <= stops[0].position {
        return get_stop_components(&stops[0], n_comps);
    }
    if t >= stops[stops.len() - 1].position {
        return get_stop_components(&stops[stops.len() - 1], n_comps);
    }

    // Find the two stops bracketing t
    for i in 0..stops.len() - 1 {
        let s0 = &stops[i];
        let s1 = &stops[i + 1];
        if t >= s0.position && t <= s1.position {
            let range = s1.position - s0.position;
            if range < 1e-10 {
                return get_stop_components(s1, n_comps);
            }
            let f = (t - s0.position) / range;
            let c0 = get_stop_components(s0, n_comps);
            let c1 = get_stop_components(s1, n_comps);
            return c0
                .iter()
                .zip(c1.iter())
                .map(|(&a, &b)| a + (b - a) * f)
                .collect();
        }
    }

    get_stop_components(&stops[stops.len() - 1], n_comps)
}

/// Get raw components from a color stop, falling back to RGB if empty.
fn get_stop_components(stop: &ColorStop, n_comps: usize) -> Vec<f64> {
    if !stop.raw_components.is_empty() {
        return stop.raw_components.clone();
    }
    // Fallback: use DeviceColor RGB
    match n_comps {
        1 => vec![stop.color.r],
        4 => {
            if let Some((c, m, y, k)) = stop.color.native_cmyk {
                vec![c, m, y, k]
            } else {
                vec![stop.color.r, stop.color.g, stop.color.b, 0.0]
            }
        }
        _ => vec![stop.color.r, stop.color.g, stop.color.b],
    }
}

/// Encode color components as 8-bit values into the output buffer.
/// Uses raw_components if available, falls back to DeviceColor.
fn encode_color_components(
    raw: &[f64],
    color: &stet_graphics::color::DeviceColor,
    n_comps: usize,
    data: &mut Vec<u8>,
) {
    if !raw.is_empty() && raw.len() >= n_comps {
        for &c in &raw[..n_comps] {
            data.push((c.clamp(0.0, 1.0) * 255.0) as u8);
        }
    } else {
        // Fallback: use DeviceColor
        match n_comps {
            1 => {
                data.push((color.r.clamp(0.0, 1.0) * 255.0) as u8);
            }
            4 => {
                if let Some((c, m, y, k)) = color.native_cmyk {
                    data.push((c.clamp(0.0, 1.0) * 255.0) as u8);
                    data.push((m.clamp(0.0, 1.0) * 255.0) as u8);
                    data.push((y.clamp(0.0, 1.0) * 255.0) as u8);
                    data.push((k.clamp(0.0, 1.0) * 255.0) as u8);
                } else {
                    data.push((color.r.clamp(0.0, 1.0) * 255.0) as u8);
                    data.push((color.g.clamp(0.0, 1.0) * 255.0) as u8);
                    data.push((color.b.clamp(0.0, 1.0) * 255.0) as u8);
                    data.push(0u8);
                }
            }
            _ => {
                data.push((color.r.clamp(0.0, 1.0) * 255.0) as u8);
                data.push((color.g.clamp(0.0, 1.0) * 255.0) as u8);
                data.push((color.b.clamp(0.0, 1.0) * 255.0) as u8);
            }
        }
    }
}

/// Encode a coordinate as a 16-bit unsigned value within [min, max].
fn encode_coord_16(val: f64, min: f64, max: f64) -> u16 {
    let range = max - min;
    if range < 1e-10 {
        return 0;
    }
    let t = (val - min) / range;
    (t.clamp(0.0, 1.0) * 65535.0) as u16
}
