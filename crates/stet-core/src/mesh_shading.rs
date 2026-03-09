// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Binary mesh and patch parsers for shading Types 4-7.
//!
//! Parses the binary DataSource format defined in PLRM §4.9.1. Supports
//! free-form Gouraud triangle meshes (Type 4), lattice-form Gouraud meshes
//! (Type 5), Coons patch meshes (Type 6), and tensor-product patch meshes (Type 7).

use crate::device::{ShadingPatch, ShadingTriangle, ShadingVertex};
use crate::graphics_state::DeviceColor;

/// Reads arbitrary bit-width unsigned integers from a byte buffer.
pub struct BitReader<'a> {
    data: &'a [u8],
    bit_pos: usize,
}

impl<'a> BitReader<'a> {
    /// Create a new BitReader over the given byte slice.
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, bit_pos: 0 }
    }

    /// Read `n` bits as an unsigned integer (big-endian, MSB first).
    pub fn read(&mut self, n: usize) -> Option<u32> {
        if n == 0 {
            return Some(0);
        }
        if n > 32 {
            return None;
        }
        let end_bit = self.bit_pos + n;
        if end_bit > self.data.len() * 8 {
            return None;
        }

        let bit_offset = self.bit_pos & 7;
        let byte_idx = self.bit_pos >> 3;

        // Fast paths for byte-aligned reads
        if bit_offset == 0 {
            let val = match n {
                8 => {
                    self.bit_pos = end_bit;
                    return Some(self.data[byte_idx] as u32);
                }
                16 => {
                    let v = ((self.data[byte_idx] as u32) << 8) | (self.data[byte_idx + 1] as u32);
                    self.bit_pos = end_bit;
                    return Some(v);
                }
                24 => {
                    let v = ((self.data[byte_idx] as u32) << 16)
                        | ((self.data[byte_idx + 1] as u32) << 8)
                        | (self.data[byte_idx + 2] as u32);
                    self.bit_pos = end_bit;
                    return Some(v);
                }
                32 => {
                    let v = ((self.data[byte_idx] as u32) << 24)
                        | ((self.data[byte_idx + 1] as u32) << 16)
                        | ((self.data[byte_idx + 2] as u32) << 8)
                        | (self.data[byte_idx + 3] as u32);
                    self.bit_pos = end_bit;
                    return Some(v);
                }
                _ => None,
            };
            if let Some(v) = val {
                return Some(v);
            }
        }

        // General case: accumulate bytes spanning the bit range
        let end_byte = (end_bit + 7) >> 3;
        let mut accum: u64 = 0;
        for &b in &self.data[byte_idx..end_byte] {
            accum = (accum << 8) | b as u64;
        }
        // Total bits in the accumulated range
        let total_bits = (end_byte - byte_idx) * 8;
        // We want n bits starting at bit_offset within the accumulated value
        let shift = total_bits - bit_offset - n;
        let mask = (1u64 << n) - 1;
        let result = ((accum >> shift) & mask) as u32;

        self.bit_pos = end_bit;
        Some(result)
    }

    /// Whether all data has been consumed.
    pub fn exhausted(&self) -> bool {
        self.bit_pos >> 3 >= self.data.len()
    }
}

/// Decode a raw integer to a float using the Decode array mapping.
/// `value = decode_min + raw * (decode_max - decode_min) / (2^bits - 1)`
#[inline]
fn decode_value(raw: u32, scale: f64, min: f64) -> f64 {
    min + raw as f64 * scale
}

/// Precompute decode scale: `(max - min) / ((1 << bits) - 1)`.
#[inline]
fn decode_scale(bits: usize, min: f64, max: f64) -> f64 {
    let max_val = ((1u64 << bits) - 1) as f64;
    if max_val == 0.0 {
        0.0
    } else {
        (max - min) / max_val
    }
}

/// Convert color components to DeviceColor based on component count.
/// Uses ICC CMYK profile when available for 4-component colors.
fn components_to_color(comps: &[f64]) -> (DeviceColor, Vec<f64>) {
    let raw = comps.to_vec();
    let color = match comps.len() {
        1 => DeviceColor::from_gray(comps[0].clamp(0.0, 1.0)),
        3 => DeviceColor::from_rgb(
            comps[0].clamp(0.0, 1.0),
            comps[1].clamp(0.0, 1.0),
            comps[2].clamp(0.0, 1.0),
        ),
        4 => DeviceColor::from_cmyk(
            comps[0].clamp(0.0, 1.0),
            comps[1].clamp(0.0, 1.0),
            comps[2].clamp(0.0, 1.0),
            comps[3].clamp(0.0, 1.0),
        ),
        _ => {
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
    };
    (color, raw)
}

/// Parse a Type 4 free-form Gouraud-shaded triangle mesh from binary data.
///
/// # Parameters
/// - `data`: Raw binary DataSource bytes
/// - `bpc`: BitsPerCoordinate
/// - `bpco`: BitsPerComponent (color)
/// - `bpfl`: BitsPerFlag
/// - `decode`: Decode array [xmin, xmax, ymin, ymax, c0min, c0max, ...]
/// - `n_comps`: Number of color components
pub fn parse_type4_mesh(
    data: &[u8],
    bpc: usize,
    bpco: usize,
    bpfl: usize,
    decode: &[f64],
    n_comps: usize,
) -> Vec<ShadingTriangle> {
    let mut reader = BitReader::new(data);
    let mut triangles = Vec::new();

    // Precompute decode scales
    let x_min = decode[0];
    let x_scale = decode_scale(bpc, decode[0], decode[1]);
    let y_min = decode[2];
    let y_scale = decode_scale(bpc, decode[2], decode[3]);

    let mut color_params: Vec<(f64, f64)> = Vec::with_capacity(n_comps);
    for i in 0..n_comps {
        let c_min = decode[4 + i * 2];
        let c_max = decode[4 + i * 2 + 1];
        color_params.push((c_min, decode_scale(bpco, c_min, c_max)));
    }

    let read_vertex = |reader: &mut BitReader| -> Option<(u32, ShadingVertex)> {
        let flag = reader.read(bpfl)?;
        let raw_x = reader.read(bpc)?;
        let raw_y = reader.read(bpc)?;
        let x = decode_value(raw_x, x_scale, x_min);
        let y = decode_value(raw_y, y_scale, y_min);
        let mut comps = vec![0.0f64; n_comps];
        for (i, comp) in comps.iter_mut().enumerate() {
            let raw = reader.read(bpco)?;
            *comp = decode_value(raw, color_params[i].1, color_params[i].0);
        }
        let (color, raw_components) = components_to_color(&comps);
        Some((flag, ShadingVertex { x, y, color, raw_components }))
    };

    // Running list of vertices for edge connectivity
    let mut vertices: Vec<ShadingVertex> = Vec::new();

    while !reader.exhausted() {
        let Some((flag, vertex)) = read_vertex(&mut reader) else {
            break;
        };

        match flag {
            0 => {
                // New independent triangle: need 2 more vertices
                vertices.clear();
                vertices.push(vertex);
                for _ in 0..2 {
                    let Some((_, v)) = read_vertex(&mut reader) else {
                        break;
                    };
                    vertices.push(v);
                }
                if vertices.len() == 3 {
                    triangles.push(ShadingTriangle {
                        v0: vertices[0].clone(),
                        v1: vertices[1].clone(),
                        v2: vertices[2].clone(),
                    });
                }
            }
            1 => {
                // Share edge BC of previous triangle
                if vertices.len() >= 2 {
                    let len = vertices.len();
                    let v0 = vertices[len - 2].clone();
                    let v1 = vertices[len - 1].clone();
                    vertices.push(vertex);
                    let v2 = vertices.last().unwrap().clone();
                    triangles.push(ShadingTriangle { v0, v1, v2 });
                }
            }
            2 => {
                // Share edge AC of previous triangle
                if vertices.len() >= 3 {
                    let len = vertices.len();
                    let v0 = vertices[len - 3].clone();
                    let v1 = vertices[len - 1].clone();
                    vertices.push(vertex);
                    let v2 = vertices.last().unwrap().clone();
                    triangles.push(ShadingTriangle { v0, v1, v2 });
                }
            }
            _ => {}
        }
    }

    triangles
}

/// Parse a Type 5 lattice-form Gouraud-shaded triangle mesh from binary data.
///
/// # Parameters
/// - `data`: Raw binary DataSource bytes
/// - `bpc`: BitsPerCoordinate
/// - `bpco`: BitsPerComponent (color)
/// - `decode`: Decode array [xmin, xmax, ymin, ymax, c0min, c0max, ...]
/// - `n_comps`: Number of color components
/// - `verts_per_row`: VerticesPerRow
pub fn parse_type5_mesh(
    data: &[u8],
    bpc: usize,
    bpco: usize,
    decode: &[f64],
    n_comps: usize,
    verts_per_row: usize,
) -> Vec<ShadingTriangle> {
    let mut reader = BitReader::new(data);
    let mut all_vertices = Vec::new();

    // Precompute decode scales
    let x_min = decode[0];
    let x_scale = decode_scale(bpc, decode[0], decode[1]);
    let y_min = decode[2];
    let y_scale = decode_scale(bpc, decode[2], decode[3]);

    let mut color_params: Vec<(f64, f64)> = Vec::with_capacity(n_comps);
    for i in 0..n_comps {
        let c_min = decode[4 + i * 2];
        let c_max = decode[4 + i * 2 + 1];
        color_params.push((c_min, decode_scale(bpco, c_min, c_max)));
    }

    // Read all vertices (no flags in Type 5)
    while !reader.exhausted() {
        let Some(raw_x) = reader.read(bpc) else {
            break;
        };
        let Some(raw_y) = reader.read(bpc) else {
            break;
        };
        let x = decode_value(raw_x, x_scale, x_min);
        let y = decode_value(raw_y, y_scale, y_min);
        let mut comps = vec![0.0f64; n_comps];
        let mut ok = true;
        for (i, comp) in comps.iter_mut().enumerate() {
            let Some(raw) = reader.read(bpco) else {
                ok = false;
                break;
            };
            *comp = decode_value(raw, color_params[i].1, color_params[i].0);
        }
        if !ok {
            break;
        }
        let (color, raw_components) = components_to_color(&comps);
        all_vertices.push(ShadingVertex {
            x,
            y,
            color,
            raw_components,
        });
    }

    // Convert lattice to triangles: each quad → 2 triangles
    let mut triangles = Vec::new();
    if verts_per_row < 2 {
        return triangles;
    }
    let num_rows = all_vertices.len() / verts_per_row;
    if num_rows < 2 {
        return triangles;
    }

    for row in 0..num_rows - 1 {
        for col in 0..verts_per_row - 1 {
            let i00 = row * verts_per_row + col;
            let i10 = i00 + 1;
            let i01 = i00 + verts_per_row;
            let i11 = i01 + 1;
            if i11 >= all_vertices.len() {
                break;
            }
            // Upper-left triangle
            triangles.push(ShadingTriangle {
                v0: all_vertices[i00].clone(),
                v1: all_vertices[i10].clone(),
                v2: all_vertices[i01].clone(),
            });
            // Lower-right triangle
            triangles.push(ShadingTriangle {
                v0: all_vertices[i10].clone(),
                v1: all_vertices[i11].clone(),
                v2: all_vertices[i01].clone(),
            });
        }
    }

    triangles
}

/// Parse a Type 6 Coons patch mesh from binary data.
pub fn parse_type6_patches(
    data: &[u8],
    bpc: usize,
    bpco: usize,
    bpfl: usize,
    decode: &[f64],
    n_comps: usize,
) -> Vec<ShadingPatch> {
    let mut reader = BitReader::new(data);
    let mut patches = Vec::new();

    let x_min = decode[0];
    let x_scale = decode_scale(bpc, decode[0], decode[1]);
    let y_min = decode[2];
    let y_scale = decode_scale(bpc, decode[2], decode[3]);

    let mut color_params: Vec<(f64, f64)> = Vec::with_capacity(n_comps);
    for i in 0..n_comps {
        let c_min = decode[4 + i * 2];
        let c_max = decode[4 + i * 2 + 1];
        color_params.push((c_min, decode_scale(bpco, c_min, c_max)));
    }

    let read_point = |reader: &mut BitReader| -> Option<(f64, f64)> {
        let raw_x = reader.read(bpc)?;
        let raw_y = reader.read(bpc)?;
        Some((
            decode_value(raw_x, x_scale, x_min),
            decode_value(raw_y, y_scale, y_min),
        ))
    };

    let read_color = |reader: &mut BitReader| -> Option<(DeviceColor, Vec<f64>)> {
        let mut comps = vec![0.0f64; n_comps];
        for (i, comp) in comps.iter_mut().enumerate() {
            let raw = reader.read(bpco)?;
            *comp = decode_value(raw, color_params[i].1, color_params[i].0);
        }
        Some(components_to_color(&comps))
    };

    while !reader.exhausted() {
        let Some(flag) = reader.read(bpfl) else {
            break;
        };

        match flag {
            0 => {
                // Independent patch: 12 points + 4 colors
                let mut points = Vec::with_capacity(12);
                let mut colors = Vec::with_capacity(4);
                let mut raw_colors_vec = Vec::with_capacity(4);
                let mut ok = true;
                for _ in 0..12 {
                    if let Some(pt) = read_point(&mut reader) {
                        points.push(pt);
                    } else {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    for _ in 0..4 {
                        if let Some((c, rc)) = read_color(&mut reader) {
                            colors.push(c);
                            raw_colors_vec.push(rc);
                        } else {
                            ok = false;
                            break;
                        }
                    }
                }
                if ok && points.len() == 12 && colors.len() == 4 {
                    patches.push(ShadingPatch {
                        points,
                        colors: [
                            colors[0].clone(),
                            colors[1].clone(),
                            colors[2].clone(),
                            colors[3].clone(),
                        ],
                        raw_colors: [
                            raw_colors_vec[0].clone(),
                            raw_colors_vec[1].clone(),
                            raw_colors_vec[2].clone(),
                            raw_colors_vec[3].clone(),
                        ],
                    });
                }
            }
            1..=3 => {
                // Inherit one side from previous patch
                if patches.is_empty() {
                    break;
                }
                let prev = patches.last().unwrap().clone();

                let (inherited_pts, inherited_colors, inherited_raw) = match flag {
                    1 => (
                        prev.points[3..6].to_vec(),
                        [prev.colors[1].clone(), prev.colors[2].clone()],
                        [prev.raw_colors[1].clone(), prev.raw_colors[2].clone()],
                    ),
                    2 => (
                        prev.points[6..9].to_vec(),
                        [prev.colors[2].clone(), prev.colors[3].clone()],
                        [prev.raw_colors[2].clone(), prev.raw_colors[3].clone()],
                    ),
                    3 => (
                        prev.points[9..12].to_vec(),
                        [prev.colors[3].clone(), prev.colors[0].clone()],
                        [prev.raw_colors[3].clone(), prev.raw_colors[0].clone()],
                    ),
                    _ => unreachable!(),
                };

                // Read remaining 9 points + 2 colors
                let mut points = inherited_pts;
                let mut ok = true;
                for _ in 0..9 {
                    if let Some(pt) = read_point(&mut reader) {
                        points.push(pt);
                    } else {
                        ok = false;
                        break;
                    }
                }
                let mut colors = vec![inherited_colors[0].clone(), inherited_colors[1].clone()];
                let mut raw_colors_vec = vec![inherited_raw[0].clone(), inherited_raw[1].clone()];
                if ok {
                    for _ in 0..2 {
                        if let Some((c, rc)) = read_color(&mut reader) {
                            colors.push(c);
                            raw_colors_vec.push(rc);
                        } else {
                            ok = false;
                            break;
                        }
                    }
                }
                if ok && points.len() == 12 && colors.len() == 4 {
                    patches.push(ShadingPatch {
                        points,
                        colors: [
                            colors[0].clone(),
                            colors[1].clone(),
                            colors[2].clone(),
                            colors[3].clone(),
                        ],
                        raw_colors: [
                            raw_colors_vec[0].clone(),
                            raw_colors_vec[1].clone(),
                            raw_colors_vec[2].clone(),
                            raw_colors_vec[3].clone(),
                        ],
                    });
                }
            }
            _ => break,
        }
    }

    patches
}

/// Parse a Type 7 tensor-product patch mesh from binary data.
pub fn parse_type7_patches(
    data: &[u8],
    bpc: usize,
    bpco: usize,
    bpfl: usize,
    decode: &[f64],
    n_comps: usize,
) -> Vec<ShadingPatch> {
    let mut reader = BitReader::new(data);
    let mut patches = Vec::new();

    let x_min = decode[0];
    let x_scale = decode_scale(bpc, decode[0], decode[1]);
    let y_min = decode[2];
    let y_scale = decode_scale(bpc, decode[2], decode[3]);

    let mut color_params: Vec<(f64, f64)> = Vec::with_capacity(n_comps);
    for i in 0..n_comps {
        let c_min = decode[4 + i * 2];
        let c_max = decode[4 + i * 2 + 1];
        color_params.push((c_min, decode_scale(bpco, c_min, c_max)));
    }

    let read_point = |reader: &mut BitReader| -> Option<(f64, f64)> {
        let raw_x = reader.read(bpc)?;
        let raw_y = reader.read(bpc)?;
        Some((
            decode_value(raw_x, x_scale, x_min),
            decode_value(raw_y, y_scale, y_min),
        ))
    };

    let read_color = |reader: &mut BitReader| -> Option<(DeviceColor, Vec<f64>)> {
        let mut comps = vec![0.0f64; n_comps];
        for (i, comp) in comps.iter_mut().enumerate() {
            let raw = reader.read(bpco)?;
            *comp = decode_value(raw, color_params[i].1, color_params[i].0);
        }
        Some(components_to_color(&comps))
    };

    while !reader.exhausted() {
        let Some(_flag) = reader.read(bpfl) else {
            break;
        };
        // Simplified: read all patches as independent (16 points + 4 colors)
        let mut points = Vec::with_capacity(16);
        let mut colors = Vec::with_capacity(4);
        let mut raw_colors_vec = Vec::with_capacity(4);
        let mut ok = true;
        for _ in 0..16 {
            if let Some(pt) = read_point(&mut reader) {
                points.push(pt);
            } else {
                ok = false;
                break;
            }
        }
        if ok {
            for _ in 0..4 {
                if let Some((c, rc)) = read_color(&mut reader) {
                    colors.push(c);
                    raw_colors_vec.push(rc);
                } else {
                    ok = false;
                    break;
                }
            }
        }
        if ok && points.len() == 16 && colors.len() == 4 {
            patches.push(ShadingPatch {
                points,
                colors: [
                    colors[0].clone(),
                    colors[1].clone(),
                    colors[2].clone(),
                    colors[3].clone(),
                ],
                raw_colors: [
                    raw_colors_vec[0].clone(),
                    raw_colors_vec[1].clone(),
                    raw_colors_vec[2].clone(),
                    raw_colors_vec[3].clone(),
                ],
            });
        }
    }

    patches
}

/// Build triangles from an array-based DataSource (flat list of numbers).
/// Type 4: [flag x y c0 c1 ... flag x y c0 c1 ...]
pub fn build_type4_from_array(values: &[f64], n_comps: usize) -> Vec<ShadingTriangle> {
    let stride = 3 + n_comps; // flag + x + y + n_comps colors
    let mut triangles = Vec::new();
    let mut vertices: Vec<ShadingVertex> = Vec::new();
    let mut pos = 0;

    while pos + stride <= values.len() {
        let flag = values[pos] as u32;
        let x = values[pos + 1];
        let y = values[pos + 2];
        let comps: Vec<f64> = values[pos + 3..pos + 3 + n_comps].to_vec();
        let (color, raw_components) = components_to_color(&comps);
        let vertex = ShadingVertex { x, y, color, raw_components };
        pos += stride;

        match flag {
            0 => {
                vertices.clear();
                vertices.push(vertex);
                for _ in 0..2 {
                    if pos + stride > values.len() {
                        break;
                    }
                    let x2 = values[pos + 1];
                    let y2 = values[pos + 2];
                    let comps2: Vec<f64> = values[pos + 3..pos + 3 + n_comps].to_vec();
                    let (color2, raw2) = components_to_color(&comps2);
                    vertices.push(ShadingVertex {
                        x: x2,
                        y: y2,
                        color: color2,
                        raw_components: raw2,
                    });
                    pos += stride;
                }
                if vertices.len() == 3 {
                    triangles.push(ShadingTriangle {
                        v0: vertices[0].clone(),
                        v1: vertices[1].clone(),
                        v2: vertices[2].clone(),
                    });
                }
            }
            1 => {
                if vertices.len() >= 2 {
                    let len = vertices.len();
                    let v0 = vertices[len - 2].clone();
                    let v1 = vertices[len - 1].clone();
                    vertices.push(vertex);
                    let v2 = vertices.last().unwrap().clone();
                    triangles.push(ShadingTriangle { v0, v1, v2 });
                }
            }
            2 => {
                if vertices.len() >= 3 {
                    let len = vertices.len();
                    let v0 = vertices[len - 3].clone();
                    let v1 = vertices[len - 1].clone();
                    vertices.push(vertex);
                    let v2 = vertices.last().unwrap().clone();
                    triangles.push(ShadingTriangle { v0, v1, v2 });
                }
            }
            _ => {}
        }
    }

    triangles
}

/// Build triangles from an array-based Type 5 lattice mesh.
/// Values: [x y c0 c1 ... x y c0 c1 ...] (no flags)
pub fn build_type5_from_array(
    values: &[f64],
    n_comps: usize,
    verts_per_row: usize,
) -> Vec<ShadingTriangle> {
    let stride = 2 + n_comps; // x + y + n_comps
    let mut all_vertices = Vec::new();

    let mut pos = 0;
    while pos + stride <= values.len() {
        let x = values[pos];
        let y = values[pos + 1];
        let comps: Vec<f64> = values[pos + 2..pos + 2 + n_comps].to_vec();
        let (color, raw_components) = components_to_color(&comps);
        all_vertices.push(ShadingVertex {
            x,
            y,
            color,
            raw_components,
        });
        pos += stride;
    }

    let mut triangles = Vec::new();
    if verts_per_row < 2 {
        return triangles;
    }
    let num_rows = all_vertices.len() / verts_per_row;
    if num_rows < 2 {
        return triangles;
    }

    for row in 0..num_rows - 1 {
        for col in 0..verts_per_row - 1 {
            let i00 = row * verts_per_row + col;
            let i10 = i00 + 1;
            let i01 = i00 + verts_per_row;
            let i11 = i01 + 1;
            if i11 >= all_vertices.len() {
                break;
            }
            triangles.push(ShadingTriangle {
                v0: all_vertices[i00].clone(),
                v1: all_vertices[i10].clone(),
                v2: all_vertices[i01].clone(),
            });
            triangles.push(ShadingTriangle {
                v0: all_vertices[i10].clone(),
                v1: all_vertices[i11].clone(),
                v2: all_vertices[i01].clone(),
            });
        }
    }

    triangles
}

/// Build patches from an array-based Type 6 Coons patch mesh.
/// Values: [flag x0 y0 x1 y1 ... c0_0 c0_1 ... c1_0 c1_1 ...]
pub fn build_type6_from_array(values: &[f64], n_comps: usize) -> Vec<ShadingPatch> {
    // flag + 12 points (24 values) + 4 colors (4 * n_comps)
    let full_stride = 1 + 24 + 4 * n_comps;
    // continuation: flag + 9 points (18 values) + 2 colors (2 * n_comps)
    let cont_stride = 1 + 18 + 2 * n_comps;
    let mut patches = Vec::new();
    let mut pos = 0;

    while pos < values.len() {
        let flag = values[pos] as u32;
        pos += 1;

        match flag {
            0 => {
                if pos + 24 + 4 * n_comps > values.len() {
                    break;
                }
                let mut points = Vec::with_capacity(12);
                for i in 0..12 {
                    points.push((values[pos + i * 2], values[pos + i * 2 + 1]));
                }
                pos += 24;
                let mut colors = Vec::with_capacity(4);
                let mut raw_colors_vec = Vec::with_capacity(4);
                for _ in 0..4 {
                    let comps: Vec<f64> = values[pos..pos + n_comps].to_vec();
                    let (c, rc) = components_to_color(&comps);
                    colors.push(c);
                    raw_colors_vec.push(rc);
                    pos += n_comps;
                }
                let _ = full_stride; // used for documentation
                patches.push(ShadingPatch {
                    points,
                    colors: [
                        colors[0].clone(),
                        colors[1].clone(),
                        colors[2].clone(),
                        colors[3].clone(),
                    ],
                    raw_colors: [
                        raw_colors_vec[0].clone(),
                        raw_colors_vec[1].clone(),
                        raw_colors_vec[2].clone(),
                        raw_colors_vec[3].clone(),
                    ],
                });
            }
            1..=3 => {
                if patches.is_empty() {
                    break;
                }
                let prev = patches.last().unwrap().clone();
                let (inherited_pts, inherited_colors, inherited_raw) = match flag {
                    1 => (
                        prev.points[3..6].to_vec(),
                        [prev.colors[1].clone(), prev.colors[2].clone()],
                        [prev.raw_colors[1].clone(), prev.raw_colors[2].clone()],
                    ),
                    2 => (
                        prev.points[6..9].to_vec(),
                        [prev.colors[2].clone(), prev.colors[3].clone()],
                        [prev.raw_colors[2].clone(), prev.raw_colors[3].clone()],
                    ),
                    3 => (
                        prev.points[9..12].to_vec(),
                        [prev.colors[3].clone(), prev.colors[0].clone()],
                        [prev.raw_colors[3].clone(), prev.raw_colors[0].clone()],
                    ),
                    _ => unreachable!(),
                };

                if pos + 18 + 2 * n_comps > values.len() {
                    break;
                }
                let mut points = inherited_pts;
                for i in 0..9 {
                    points.push((values[pos + i * 2], values[pos + i * 2 + 1]));
                }
                pos += 18;
                let mut colors = vec![inherited_colors[0].clone(), inherited_colors[1].clone()];
                let mut raw_colors_vec = vec![inherited_raw[0].clone(), inherited_raw[1].clone()];
                for _ in 0..2 {
                    let comps: Vec<f64> = values[pos..pos + n_comps].to_vec();
                    let (c, rc) = components_to_color(&comps);
                    colors.push(c);
                    raw_colors_vec.push(rc);
                    pos += n_comps;
                }
                let _ = cont_stride;
                patches.push(ShadingPatch {
                    points,
                    colors: [
                        colors[0].clone(),
                        colors[1].clone(),
                        colors[2].clone(),
                        colors[3].clone(),
                    ],
                    raw_colors: [
                        raw_colors_vec[0].clone(),
                        raw_colors_vec[1].clone(),
                        raw_colors_vec[2].clone(),
                        raw_colors_vec[3].clone(),
                    ],
                });
            }
            _ => break,
        }
    }

    patches
}

/// Build patches from an array-based Type 7 tensor-product patch mesh.
pub fn build_type7_from_array(values: &[f64], n_comps: usize) -> Vec<ShadingPatch> {
    // flag + 16 points (32 values) + 4 colors (4 * n_comps)
    let stride = 1 + 32 + 4 * n_comps;
    let mut patches = Vec::new();
    let mut pos = 0;

    while pos + stride <= values.len() {
        let _flag = values[pos] as u32;
        pos += 1;
        let mut points = Vec::with_capacity(16);
        for i in 0..16 {
            points.push((values[pos + i * 2], values[pos + i * 2 + 1]));
        }
        pos += 32;
        let mut colors = Vec::with_capacity(4);
        let mut raw_colors_vec = Vec::with_capacity(4);
        for _ in 0..4 {
            let comps: Vec<f64> = values[pos..pos + n_comps].to_vec();
            let (c, rc) = components_to_color(&comps);
            colors.push(c);
            raw_colors_vec.push(rc);
            pos += n_comps;
        }
        patches.push(ShadingPatch {
            points,
            colors: [
                colors[0].clone(),
                colors[1].clone(),
                colors[2].clone(),
                colors[3].clone(),
            ],
            raw_colors: [
                raw_colors_vec[0].clone(),
                raw_colors_vec[1].clone(),
                raw_colors_vec[2].clone(),
                raw_colors_vec[3].clone(),
            ],
        });
    }

    patches
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bit_reader_aligned() {
        let data = [0xAB, 0xCD, 0xEF, 0x01];
        let mut reader = BitReader::new(&data);
        assert_eq!(reader.read(8), Some(0xAB));
        assert_eq!(reader.read(8), Some(0xCD));
        assert_eq!(reader.read(16), Some(0xEF01));
        assert!(reader.exhausted());
    }

    #[test]
    fn test_bit_reader_unaligned() {
        let data = [0b10110100, 0b11010010];
        let mut reader = BitReader::new(&data);
        assert_eq!(reader.read(4), Some(0b1011));
        assert_eq!(reader.read(4), Some(0b0100));
        assert_eq!(reader.read(4), Some(0b1101));
        assert_eq!(reader.read(4), Some(0b0010));
        assert!(reader.exhausted());
    }

    #[test]
    fn test_bit_reader_cross_byte() {
        let data = [0b11110000, 0b10101010];
        let mut reader = BitReader::new(&data);
        assert_eq!(reader.read(6), Some(0b111100));
        assert_eq!(reader.read(6), Some(0b001010));
        assert_eq!(reader.read(4), Some(0b1010));
        assert!(reader.exhausted());
    }

    #[test]
    fn test_decode_scale_8bit() {
        let scale = decode_scale(8, 0.0, 1.0);
        assert!((scale - 1.0 / 255.0).abs() < 1e-10);
        let val = decode_value(128, scale, 0.0);
        assert!((val - 128.0 / 255.0).abs() < 1e-10);
    }

    #[test]
    fn test_type5_lattice_simple() {
        // 2x2 lattice with 1-component (gray) colors
        // 8 bits/coord, 8 bits/component
        // Decode: [0, 255, 0, 255, 0, 1]
        #[rustfmt::skip]
        let data = [
            0, 0, 0,      // (0,0) gray=0.0
            255, 0, 128,   // (255,0) gray≈0.5
            0, 255, 0,     // (0,255) gray=0.0
            255, 255, 255, // (255,255) gray=1.0
        ];
        let decode = [0.0, 255.0, 0.0, 255.0, 0.0, 1.0];
        let triangles = parse_type5_mesh(&data, 8, 8, &decode, 1, 2);
        assert_eq!(triangles.len(), 2);
    }
}
