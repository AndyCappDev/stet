// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Colorimetric A2B1 CLUT sampler.
//!
//! moxcms's `create_transform` routes CMYK profiles through an internal
//! Lab→sRGB pipeline that over-saturates light/midtone colours noticeably
//! relative to lcms2 / Acrobat / Ghostscript. This module bypasses moxcms
//! for v2 `lut16Type` CMYK profiles: each grid point goes through the
//! profile's `A2B1` (relative-colorimetric) CLUT, the legacy v2 PCS-Lab
//! decode, and a hand-tuned Lab → XYZ-D50 → sRGB pipeline. The output
//! matches lcms2's `cmsDoTransform(RelCol)` to ±1 RGB level on a 17⁴
//! sweep against ISO Coated v2 300% (ECI), which is what GS produces for
//! typical print imagery (light greens, neutrals, blacks). Out-of-gamut
//! colours clip to the sRGB boundary (also matching lcms2 / GS).
//!
//! Profiles whose tables are `mAB`/`mBA` (v4 multi-process elements) or
//! `lut8Type` (mft1) fall back to the moxcms-based bake; callers detect
//! the `None` return and use the existing path.

use moxcms::{ColorProfile, DataColorSpace, Hypercube, Lab, LutStore, LutType, LutWarehouse};

use super::Clut4;
use super::bpc::{WP_D50, apply_bpc_rgb_u8, compute_bpc_params, lab_to_xyz_d50};

/// `0xFF00` — the legacy ICC v2 Lab denominator for L*. Stored values in
/// `[0, 0xFF00]` map linearly to L*∈[0, 100]; values above `0xFF00` are
/// reserved to allow encoders to round above the maximum without losing
/// the legal range.
const PCS_LAB_DENOM: f32 = 65280.0;

/// Sample a CMYK profile's `A2B1` (colorimetric) table into a [`Clut4`].
///
/// Out-of-gamut Lab values clip to the sRGB boundary, matching lcms2's
/// `cmsDoTransform(RelCol)` behaviour and avoiding the desaturation that
/// hue-preserving chroma compression produces on pure process primaries
/// (e.g. CMYK yellow → washed-out lemon).
///
/// Returns `None` when the profile lacks a colorimetric table or its
/// shape is one we currently defer (mAB / mft1 / non-Lab PCS / non-CMYK
/// input). In every `None` case the caller is expected to fall back to
/// the existing moxcms-driven [`super::bake_clut4`] path.
///
/// `grid_n` controls the output CLUT resolution (the existing path uses
/// 17). BPC is folded into every grid point so per-pixel runtime cost
/// stays at zero.
pub(super) fn bake_clut4_perceptual(
    profile: &ColorProfile,
    grid_n: usize,
    bpc_enabled: bool,
) -> Option<Clut4> {
    if profile.color_space != DataColorSpace::Cmyk || profile.pcs != DataColorSpace::Lab {
        return None;
    }
    if !(2..=33).contains(&grid_n) {
        return None;
    }

    let colorimetric = SampledLut::from_warehouse(profile.lut_a_to_b_colorimetric.as_ref()?)?;

    // BPC is computed against this sampler's own (1,1,1,1) output, not
    // moxcms's transform output, so the source black-point matches what
    // the bake actually produces. Computing it against moxcms's transform
    // miscalibrates the post-correction and leaves K-heavy CMYK ~13 RGB
    // levels lighter than baseline / GS.
    let bpc = if bpc_enabled {
        let lab_k = colorimetric.sample(1.0, 1.0, 1.0, 1.0);
        let l_star = (lab_k.l as f64 * 100.0).clamp(0.0, 100.0);
        let neutral = [l_star.min(50.0), 0.0, 0.0];
        let sbp = lab_to_xyz_d50(neutral);
        Some(compute_bpc_params(sbp, [0.0; 3], WP_D50))
    } else {
        None
    };
    let bpc = bpc.as_ref();

    let total = grid_n
        .checked_mul(grid_n)?
        .checked_mul(grid_n)?
        .checked_mul(grid_n)?;
    let mut data = vec![0u8; total * 3];

    let denom = (grid_n - 1) as f32;
    for k_i in 0..grid_n {
        for y_i in 0..grid_n {
            for m_i in 0..grid_n {
                for c_i in 0..grid_n {
                    let c = c_i as f32 / denom;
                    let m = m_i as f32 / denom;
                    let y = y_i as f32 / denom;
                    let k = k_i as f32 / denom;

                    let lab_c = colorimetric.sample(c, m, y, k);
                    let lin = lab_to_linear_srgb(lab_c);

                    let mut rgb = encode_linear_srgb(lin);
                    if let Some(p) = bpc {
                        rgb = apply_bpc_rgb_u8(rgb, p);
                    }

                    let off = (((k_i * grid_n + y_i) * grid_n + m_i) * grid_n + c_i) * 3;
                    data[off] = rgb[0];
                    data[off + 1] = rgb[1];
                    data[off + 2] = rgb[2];
                }
            }
        }
    }

    Some(Clut4::from_baked(grid_n as u8, data))
}

/// A profile A2B table prepared for sampling at arbitrary CMYK points.
struct SampledLut<'a> {
    input_table: &'a [u16],
    output_table: &'a [u16],
    n_in_entries: usize,
    n_out_entries: usize,
    cube_data: Vec<f32>,
    cube_grid: usize,
}

impl<'a> SampledLut<'a> {
    fn from_warehouse(warehouse: &'a LutWarehouse) -> Option<Self> {
        let lut = match warehouse {
            LutWarehouse::Lut(l) => l,
            // mAB (v4 multi-process elements) deferred — Phase 1.1.
            LutWarehouse::Multidimensional(_) => return None,
        };
        if lut.lut_type != LutType::Lut16 {
            // mft1 (lut8Type) deferred.
            return None;
        }
        if lut.num_input_channels != 4 || lut.num_output_channels != 3 {
            return None;
        }
        let input_table = match &lut.input_table {
            LutStore::Store16(v) => v.as_slice(),
            LutStore::Store8(_) => return None,
        };
        let output_table = match &lut.output_table {
            LutStore::Store16(v) => v.as_slice(),
            LutStore::Store8(_) => return None,
        };
        let clut_table = match &lut.clut_table {
            LutStore::Store16(v) => v.as_slice(),
            LutStore::Store8(_) => return None,
        };
        let n_in_entries = lut.num_input_table_entries as usize;
        let n_out_entries = lut.num_output_table_entries as usize;
        let cube_grid = lut.num_clut_grid_points as usize;
        if cube_grid < 2 || n_in_entries < 2 || n_out_entries < 2 {
            return None;
        }
        if input_table.len() < n_in_entries.checked_mul(4)? {
            return None;
        }
        if output_table.len() < n_out_entries.checked_mul(3)? {
            return None;
        }
        let cube_total = cube_grid
            .checked_mul(cube_grid)?
            .checked_mul(cube_grid)?
            .checked_mul(cube_grid)?
            .checked_mul(3)?;
        if clut_table.len() < cube_total {
            return None;
        }
        let cube_data: Vec<f32> = clut_table[..cube_total]
            .iter()
            .map(|&v| v as f32 / 65535.0)
            .collect();
        Some(SampledLut {
            input_table,
            output_table,
            n_in_entries,
            n_out_entries,
            cube_data,
            cube_grid,
        })
    }

    /// Run a CMYK input through the table's input curves, 4D CLUT, and
    /// output curves; return moxcms-normalised Lab.
    fn sample(&self, c: f32, m: f32, y: f32, k: f32) -> Lab {
        // Per-channel input curves.
        let c_in = sample_curve(self.input_table, 0, self.n_in_entries, c);
        let m_in = sample_curve(self.input_table, 1, self.n_in_entries, m);
        let y_in = sample_curve(self.input_table, 2, self.n_in_entries, y);
        let k_in = sample_curve(self.input_table, 3, self.n_in_entries, k);

        // 4D quadlinear CLUT lookup. Hypercube has (x, y, z, w) with w
        // varying fastest. ICC mft2 stores "the last input channel varies
        // most rapidly" — for CMYK that's K. So we hand x=C, y=M, z=Y, w=K.
        // `Hypercube::new` is cheap (no allocation; just stride bookkeeping)
        // so we can build it per-call without measurable overhead.
        let hypercube = match Hypercube::new(&self.cube_data, self.cube_grid, 3) {
            Ok(h) => h,
            Err(_) => return Lab::new(1.0, 0.5, 0.5),
        };
        let pcs = hypercube.quadlinear_vec3(c_in, m_in, y_in, k_in);

        // Per-channel output curves.
        let l_post = sample_curve(self.output_table, 0, self.n_out_entries, pcs.v[0]);
        let a_post = sample_curve(self.output_table, 1, self.n_out_entries, pcs.v[1]);
        let b_post = sample_curve(self.output_table, 2, self.n_out_entries, pcs.v[2]);

        // The CLUT and output curves operate on `raw / 65535`. Convert to
        // moxcms-normalised Lab via the legacy v2 mft2 encoding:
        //   L* = raw_L * 100 / 0xFF00 → l_norm = post_L * 65535 / 0xFF00
        //   a* = raw_a / 256 - 128    → a_norm = post_a * 65535 / 0xFF00
        //   b* = raw_b / 256 - 128    → b_norm = post_b * 65535 / 0xFF00
        let scale = 65535.0 / PCS_LAB_DENOM;
        Lab::new(
            (l_post * scale).clamp(0.0, 1.0),
            (a_post * scale).clamp(0.0, 1.0),
            (b_post * scale).clamp(0.0, 1.0),
        )
    }
}

/// 1D curve lookup with linear interpolation. `table` packs all channels
/// sequentially: channel `ch`'s `n_entries` values start at offset
/// `ch * n_entries`. `x` is in `[0, 1]`; the result is in `[0, 1]`.
#[inline]
fn sample_curve(table: &[u16], ch: usize, n_entries: usize, x: f32) -> f32 {
    let base = ch * n_entries;
    let scale = (n_entries - 1) as f32;
    let pos = x.clamp(0.0, 1.0) * scale;
    let i0 = pos.floor() as usize;
    let i1 = (i0 + 1).min(n_entries - 1);
    let t = pos - i0 as f32;
    let v0 = table[base + i0] as f32 / 65535.0;
    let v1 = table[base + i1] as f32 / 65535.0;
    v0 + (v1 - v0) * t
}

/// Decode normalised Lab (moxcms encoding) to absolute linear sRGB-D65.
/// Out-of-gamut values are returned as-is; the caller clamps via
/// [`encode_linear_srgb`].
fn lab_to_linear_srgb(lab: Lab) -> [f64; 3] {
    // moxcms's `to_pcs_xyz` divides by `(1 + 32767/32768)` to land in the
    // ICC PCS XYZ encoding where the reference white maps to ≈0.5. Undo
    // that here so the matrix sees absolute XYZ (Y_white = 1.0).
    let xyz = lab.to_pcs_xyz();
    const PCS_UNDO: f64 = 1.0 + 32767.0 / 32768.0;
    let xyz = [
        xyz.x as f64 * PCS_UNDO,
        xyz.y as f64 * PCS_UNDO,
        xyz.z as f64 * PCS_UNDO,
    ];
    xyz_d50_to_linear_srgb_d65(xyz)
}

/// Encode linear sRGB to packed gamma-encoded u8, clipping to `[0, 1]`.
fn encode_linear_srgb(lin: [f64; 3]) -> [u8; 3] {
    let r = linear_to_srgb(lin[0].clamp(0.0, 1.0));
    let g = linear_to_srgb(lin[1].clamp(0.0, 1.0));
    let b = linear_to_srgb(lin[2].clamp(0.0, 1.0));
    [
        (r * 255.0).round().clamp(0.0, 255.0) as u8,
        (g * 255.0).round().clamp(0.0, 255.0) as u8,
        (b * 255.0).round().clamp(0.0, 255.0) as u8,
    ]
}

/// XYZ-D50 → linear sRGB-D65 via the combined Bradford-CAT × sRGB inverse
/// matrix. Coefficients lifted from the ICC reference: identical to what
/// lcms2 produces under default settings.
#[inline]
fn xyz_d50_to_linear_srgb_d65(xyz: [f64; 3]) -> [f64; 3] {
    const M: [[f64; 3]; 3] = [
        [3.133_856_1, -1.616_866_7, -0.490_614_6],
        [-0.978_768_4, 1.916_141_5, 0.033_454_0],
        [0.071_945_3, -0.228_991_4, 1.405_242_7],
    ];
    [
        M[0][0] * xyz[0] + M[0][1] * xyz[1] + M[0][2] * xyz[2],
        M[1][0] * xyz[0] + M[1][1] * xyz[1] + M[1][2] * xyz[2],
        M[2][0] * xyz[0] + M[2][1] * xyz[1] + M[2][2] * xyz[2],
    ]
}

/// Linear → gamma-encoded sRGB. Standard sRGB EOTF.
#[inline]
fn linear_to_srgb(v: f64) -> f64 {
    if v <= 0.003_130_8 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}
