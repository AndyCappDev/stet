// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Convert display list shading parameters to PDF shading dictionaries.

use stet_core::device::{
    AxialShadingParams, ColorStop, MeshShadingParams, PatchShadingParams, RadialShadingParams,
};

use crate::pdf_objects::PdfObj;
use crate::pdf_writer::PdfWriter;

/// Build a PDF axial shading and add all needed objects to the writer.
/// Returns the shading dict object number.
pub fn build_axial_shading(writer: &mut PdfWriter, params: &AxialShadingParams) -> u32 {
    let func_ref = build_sampled_function(writer, &params.color_stops);

    let mut entries = vec![
        (b"ShadingType".to_vec(), PdfObj::Int(2)),
        (b"ColorSpace".to_vec(), PdfObj::name("DeviceRGB")),
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
    let func_ref = build_sampled_function(writer, &params.color_stops);

    let mut entries = vec![
        (b"ShadingType".to_vec(), PdfObj::Int(3)),
        (b"ColorSpace".to_vec(), PdfObj::name("DeviceRGB")),
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
    if params.triangles.is_empty() {
        // Empty mesh — return a minimal shading
        return writer.add_object(&PdfObj::Dict(vec![
            (b"ShadingType".to_vec(), PdfObj::Int(4)),
            (b"ColorSpace".to_vec(), PdfObj::name("DeviceRGB")),
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
            let xn = encode_coord_16(v.x, x_min, x_max);
            data.extend(xn.to_be_bytes());
            // Y coordinate (16-bit)
            let yn = encode_coord_16(v.y, y_min, y_max);
            data.extend(yn.to_be_bytes());
            // RGB (8-bit each)
            data.push((v.color.r.clamp(0.0, 1.0) * 255.0) as u8);
            data.push((v.color.g.clamp(0.0, 1.0) * 255.0) as u8);
            data.push((v.color.b.clamp(0.0, 1.0) * 255.0) as u8);
        }
    }

    let dict_entries = vec![
        (b"ShadingType".to_vec(), PdfObj::Int(4)),
        (b"ColorSpace".to_vec(), PdfObj::name("DeviceRGB")),
        (b"BitsPerCoordinate".to_vec(), PdfObj::Int(16)),
        (b"BitsPerComponent".to_vec(), PdfObj::Int(8)),
        (b"BitsPerFlag".to_vec(), PdfObj::Int(8)),
        (
            b"Decode".to_vec(),
            PdfObj::Array(vec![
                PdfObj::Real(x_min),
                PdfObj::Real(x_max),
                PdfObj::Real(y_min),
                PdfObj::Real(y_max),
                PdfObj::Int(0),
                PdfObj::Int(1),
                PdfObj::Int(0),
                PdfObj::Int(1),
                PdfObj::Int(0),
                PdfObj::Int(1),
            ]),
        ),
    ];

    writer.add_stream(dict_entries, &data, true)
}

/// Build a PDF Type 6 (Coons) or Type 7 (tensor-product) patch mesh.
/// Returns the shading stream object number.
pub fn build_patch_shading(writer: &mut PdfWriter, params: &PatchShadingParams) -> u32 {
    if params.patches.is_empty() {
        return writer.add_object(&PdfObj::Dict(vec![
            (b"ShadingType".to_vec(), PdfObj::Int(6)),
            (b"ColorSpace".to_vec(), PdfObj::name("DeviceRGB")),
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
        // Corner colors (4 corners × RGB)
        for color in &patch.colors {
            data.push((color.r.clamp(0.0, 1.0) * 255.0) as u8);
            data.push((color.g.clamp(0.0, 1.0) * 255.0) as u8);
            data.push((color.b.clamp(0.0, 1.0) * 255.0) as u8);
        }
    }

    let dict_entries = vec![
        (b"ShadingType".to_vec(), PdfObj::Int(shading_type)),
        (b"ColorSpace".to_vec(), PdfObj::name("DeviceRGB")),
        (b"BitsPerCoordinate".to_vec(), PdfObj::Int(16)),
        (b"BitsPerComponent".to_vec(), PdfObj::Int(8)),
        (b"BitsPerFlag".to_vec(), PdfObj::Int(8)),
        (
            b"Decode".to_vec(),
            PdfObj::Array(vec![
                PdfObj::Real(x_min),
                PdfObj::Real(x_max),
                PdfObj::Real(y_min),
                PdfObj::Real(y_max),
                PdfObj::Int(0),
                PdfObj::Int(1),
                PdfObj::Int(0),
                PdfObj::Int(1),
                PdfObj::Int(0),
                PdfObj::Int(1),
            ]),
        ),
    ];

    writer.add_stream(dict_entries, &data, true)
}

/// Build a Type 0 (sampled) function from color stops.
/// Samples 256 RGB values and returns the function stream object number.
fn build_sampled_function(writer: &mut PdfWriter, stops: &[ColorStop]) -> u32 {
    let n_samples = 256;
    let mut samples = Vec::with_capacity(n_samples * 3);

    for i in 0..n_samples {
        let t = i as f64 / (n_samples - 1) as f64;
        let (r, g, b) = sample_color_stops(stops, t);
        samples.push((r.clamp(0.0, 1.0) * 255.0) as u8);
        samples.push((g.clamp(0.0, 1.0) * 255.0) as u8);
        samples.push((b.clamp(0.0, 1.0) * 255.0) as u8);
    }

    let dict_entries = vec![
        (b"FunctionType".to_vec(), PdfObj::Int(0)),
        (
            b"Domain".to_vec(),
            PdfObj::Array(vec![PdfObj::Int(0), PdfObj::Int(1)]),
        ),
        (
            b"Range".to_vec(),
            PdfObj::Array(vec![
                PdfObj::Int(0),
                PdfObj::Int(1),
                PdfObj::Int(0),
                PdfObj::Int(1),
                PdfObj::Int(0),
                PdfObj::Int(1),
            ]),
        ),
        (
            b"Size".to_vec(),
            PdfObj::Array(vec![PdfObj::Int(n_samples as i64)]),
        ),
        (b"BitsPerSample".to_vec(), PdfObj::Int(8)),
    ];

    writer.add_stream(dict_entries, &samples, true)
}

/// Interpolate color stops at position t (0..1).
fn sample_color_stops(stops: &[ColorStop], t: f64) -> (f64, f64, f64) {
    if stops.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    if stops.len() == 1 || t <= stops[0].position {
        let c = &stops[0].color;
        return (c.r, c.g, c.b);
    }
    if t >= stops[stops.len() - 1].position {
        let c = &stops[stops.len() - 1].color;
        return (c.r, c.g, c.b);
    }

    // Find the two stops bracketing t
    for i in 0..stops.len() - 1 {
        let s0 = &stops[i];
        let s1 = &stops[i + 1];
        if t >= s0.position && t <= s1.position {
            let range = s1.position - s0.position;
            if range < 1e-10 {
                return (s1.color.r, s1.color.g, s1.color.b);
            }
            let f = (t - s0.position) / range;
            return (
                s0.color.r + (s1.color.r - s0.color.r) * f,
                s0.color.g + (s1.color.g - s0.color.g) * f,
                s0.color.b + (s1.color.b - s0.color.b) * f,
            );
        }
    }

    let c = &stops[stops.len() - 1].color;
    (c.r, c.g, c.b)
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
