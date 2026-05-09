// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Hand-rolled colorimetric CLUT samplers.
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
//! The same building blocks are reused for the PDF/X **proofing chain**
//! (`source RGB → OutputIntent CMYK`, used as the first leg of the
//! source-through-OI chain in `register_profile_with_n`).
//! [`HandRolledChainStage1Rgb`] composes a `SourceA2BSampler` and a
//! `LabToCmykSampler` per pixel and implements
//! [`moxcms::TransformExecutor`] for both `u8` and `f64` so it slots
//! directly into the `ChainedTransform` stage-1 slot.
//!
//! Profiles whose tables are `mAB`/`mBA` (v4 multi-process elements) or
//! `lut8Type` (mft1) fall back to the moxcms-based bake; callers detect
//! the `None` return and use the existing path.

use moxcms::{
    CmsError, ColorProfile, Cube, DataColorSpace, Hypercube, Lab, LutStore, LutType, LutWarehouse,
    RenderingIntent, ToneCurveEvaluator, TransformExecutor, Xyz,
};

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

// ============================================================================
// PDF/X proofing chain stage 1 (source RGB → OutputIntent CMYK)
// ============================================================================
//
// The `register_profile_with_n` chain in `super::IccCache` previously routed
// every source profile through `moxcms::ColorProfile::create_transform`, then
// through the OutputIntent's CMYK→sRGB transform. For sRGB-style RGB sources
// moxcms's first leg drifts ~5–15% off Adobe ACE / lcms2 — small enough to
// pass GWG 13.0 (whose tiny custom RGB profile happens to lie on a region
// where moxcms is exact) but large enough to fail GWG 16.1 (sRGB Saturation
// rendering intent through the OutputIntent's B2A0).
//
// To close that gap we re-build stage 1 from scratch using the same
// primitives moxcms exposes:
//   1. Source RGB → linear RGB via the profile's TRC evaluators (or a
//      LUT-based A2B `lut16Type` table when the profile carries one).
//   2. The post-output-curve values pass directly into the OutputIntent's
//      B2A `lut16Type` input curves — both are in mft2 PCS-Lab encoding.
//   3. CLUT trilinear → output curves on the OI side → CMYK in `[0, 1]`.
//
// The composition is evaluated **per pixel** rather than baked into an
// intermediate CLUT. A pre-bake at the typical `17^3` grid resolution
// introduces a quantization layer between the two profile CLUTs that
// drifts the GWG 13.0 BG-vs-X match by ~6 RGB levels even though each leg
// is locally exact. moxcms's chain composes the same two CLUTs per pixel,
// and to match its byte-level accuracy we do the same.
//
// This module presently uses the Perceptual tables only (A2B0 / B2A0,
// matching moxcms's intent default). Step 2 of the GWG 16.1 plan will
// expand to all four ICC rendering intents.

/// Source RGB profile A2B sampler: either an owned `lut16Type` table or a
/// shaper-matrix (TRC + colorant matrix) fallback. Held by value inside
/// [`HandRolledChainStage1Rgb`] so the chain can outlive the source
/// `ColorProfile` it was built from.
enum SourceA2BSampler {
    Lut(OwnedLutSampler),
    Shaper(ShaperMatrix),
}

impl SourceA2BSampler {
    fn new(profile: &ColorProfile, intent: RenderingIntent) -> Option<Self> {
        if profile.color_space != DataColorSpace::Rgb {
            return None;
        }
        // Pick the requested intent's A2B table; fall back to A2B0
        // (perceptual) and then to whichever table is available so
        // step 3's intent plumbing degrades gracefully on profiles
        // that ship only one table.
        let primary = match intent {
            RenderingIntent::Perceptual => profile.lut_a_to_b_perceptual.as_ref(),
            // AbsoluteColorimetric uses the colorimetric (A2B1) table —
            // the white-point shift specific to absolute colorimetric is
            // a runtime adjustment we defer to step 4 (BPC + AbsCol).
            RenderingIntent::RelativeColorimetric | RenderingIntent::AbsoluteColorimetric => {
                profile.lut_a_to_b_colorimetric.as_ref()
            }
            RenderingIntent::Saturation => profile.lut_a_to_b_saturation.as_ref(),
        };
        let warehouse = primary
            .or(profile.lut_a_to_b_perceptual.as_ref())
            .or(profile.lut_a_to_b_colorimetric.as_ref())
            .or(profile.lut_a_to_b_saturation.as_ref());
        if let Some(wh) = warehouse
            && profile.pcs == DataColorSpace::Lab
            && let Some(lut) = OwnedLutSampler::from_warehouse(wh, 3, 3)
        {
            return Some(SourceA2BSampler::Lut(lut));
        }

        // Shaper-matrix fallback for sRGB-style profiles (TRCs +
        // colorant matrix → XYZ-D50 → Lab). Shaper-matrix profiles
        // produce the same XYZ regardless of intent, so the same
        // sampler is reused for every requested intent.
        ShaperMatrix::new(profile).map(SourceA2BSampler::Shaper)
    }

    /// Returns the source profile's A2B output as `[L, a, b]` in mft2
    /// PCS-Lab encoding (each component in `[0, 1]`, `0xFF00`-denominated).
    /// LUT-based profiles return their post-output-curve values verbatim;
    /// shaper-matrix profiles compute Lab via TRC + colorant matrix and
    /// re-encode to mft2.
    fn sample_pcs_lab(&self, r: f32, g: f32, b: f32) -> [f32; 3] {
        match self {
            SourceA2BSampler::Lut(lut) => lut.sample_rgb_to_pcs_lab_raw(r, g, b),
            SourceA2BSampler::Shaper(sm) => sm.sample_pcs_lab(r, g, b),
        }
    }
}

/// Shaper-matrix RGB profile sampler. Linearises with the per-channel TRC
/// evaluators, multiplies through the colorant matrix, and converts the
/// resulting XYZ-D50 to moxcms-encoded Lab.
///
/// `Lab::from_pcs_xyz` expects PCS-encoded XYZ (the ICC PCS encoding
/// where the white point lands at ≈ 0.5, equal to absolute XYZ divided
/// by `1 + 32767/32768` ≈ 2.0). The colorant matrix
/// (`ColorProfile::colorant_matrix`) returns absolute XYZ-D50, so we
/// fold the encoding factor into the matrix once at construction time
/// rather than dividing per pixel.
struct ShaperMatrix {
    trc_r: Box<dyn ToneCurveEvaluator + Send + Sync>,
    trc_g: Box<dyn ToneCurveEvaluator + Send + Sync>,
    trc_b: Box<dyn ToneCurveEvaluator + Send + Sync>,
    /// 3×3 colorant matrix (linear-RGB → PCS-encoded XYZ-D50), row-major.
    /// Pre-scaled by `1 / (1 + 32767/32768)` so its output feeds straight
    /// into [`Lab::from_pcs_xyz`].
    matrix: [[f64; 3]; 3],
}

/// `1 + 32767/32768` — the PCS XYZ encoding scale moxcms's
/// [`Lab::to_pcs_xyz`] / [`Lab::from_pcs_xyz`] apply. PCS-encoded XYZ
/// equals absolute XYZ divided by this factor.
const PCS_XYZ_DENOM: f64 = 1.0 + 32767.0 / 32768.0;

impl ShaperMatrix {
    fn new(profile: &ColorProfile) -> Option<Self> {
        let red_trc = profile.red_trc.as_ref()?;
        let green_trc = profile.green_trc.as_ref()?;
        let blue_trc = profile.blue_trc.as_ref()?;

        let trc_r = red_trc.make_linear_evaluator().ok()?;
        let trc_g = green_trc.make_linear_evaluator().ok()?;
        let trc_b = blue_trc.make_linear_evaluator().ok()?;

        let m = profile.colorant_matrix();
        let s = 1.0 / PCS_XYZ_DENOM;
        let matrix = [
            [m.v[0][0] * s, m.v[0][1] * s, m.v[0][2] * s],
            [m.v[1][0] * s, m.v[1][1] * s, m.v[1][2] * s],
            [m.v[2][0] * s, m.v[2][1] * s, m.v[2][2] * s],
        ];

        Some(ShaperMatrix {
            trc_r,
            trc_g,
            trc_b,
            matrix,
        })
    }

    fn sample_pcs_lab(&self, r: f32, g: f32, b: f32) -> [f32; 3] {
        let lin_r = self.trc_r.evaluate_value(r) as f64;
        let lin_g = self.trc_g.evaluate_value(g) as f64;
        let lin_b = self.trc_b.evaluate_value(b) as f64;

        let x = self.matrix[0][0] * lin_r + self.matrix[0][1] * lin_g + self.matrix[0][2] * lin_b;
        let y = self.matrix[1][0] * lin_r + self.matrix[1][1] * lin_g + self.matrix[1][2] * lin_b;
        let z = self.matrix[2][0] * lin_r + self.matrix[2][1] * lin_g + self.matrix[2][2] * lin_b;

        let lab = Lab::from_pcs_xyz(Xyz::new(x as f32, y as f32, z as f32));
        // Re-encode moxcms-Lab to mft2 PCS-Lab format (denom 65280) so
        // the value lines up with the OI B2A input curves' grid axis.
        let scale = PCS_LAB_DENOM / 65535.0;
        [
            (lab.l * scale).clamp(0.0, 1.0),
            (lab.a * scale).clamp(0.0, 1.0),
            (lab.b * scale).clamp(0.0, 1.0),
        ]
    }
}

/// OutputIntent CMYK profile B2A sampler (Lab → CMYK), owned variant.
struct LabToCmykSampler {
    lut: OwnedLutSampler,
}

impl LabToCmykSampler {
    fn new(profile: &ColorProfile, intent: RenderingIntent) -> Option<Self> {
        if profile.color_space != DataColorSpace::Cmyk || profile.pcs != DataColorSpace::Lab {
            return None;
        }
        // Pick the requested intent's B2A table; fall back to B2A0 and
        // then to whichever is available. AbsoluteColorimetric uses the
        // same B2A1 table as RelativeColorimetric.
        let primary = match intent {
            RenderingIntent::Perceptual => profile.lut_b_to_a_perceptual.as_ref(),
            RenderingIntent::RelativeColorimetric | RenderingIntent::AbsoluteColorimetric => {
                profile.lut_b_to_a_colorimetric.as_ref()
            }
            RenderingIntent::Saturation => profile.lut_b_to_a_saturation.as_ref(),
        };
        let warehouse = primary
            .or(profile.lut_b_to_a_perceptual.as_ref())
            .or(profile.lut_b_to_a_colorimetric.as_ref())
            .or(profile.lut_b_to_a_saturation.as_ref())?;
        let lut = OwnedLutSampler::from_warehouse(warehouse, 3, 4)?;
        Some(LabToCmykSampler { lut })
    }

    fn sample_pcs_lab(&self, pcs_lab: [f32; 3]) -> [f32; 4] {
        self.lut.sample_pcs_lab_to_cmyk(pcs_lab)
    }
}

/// Owned v2 `lut16Type` sampler covering `(n_in, n_out)` shapes the
/// proofing chain needs: `(3, 3)` for an RGB-source A2B → Lab and `(3, 4)`
/// for an OutputIntent B2A → CMYK. Stores its own Vec copies of the
/// table data so the sampler outlives the source `ColorProfile`. The
/// (4, 3) CMYK A2B path keeps using the older [`SampledLut`] above
/// unchanged.
///
/// Curves are pre-converted from `u16/65535` to `f32` once at
/// construction so the per-pixel `sample_curve_owned` doesn't repeat the
/// division on every call.
struct OwnedLutSampler {
    input_table: Vec<f32>,
    output_table: Vec<f32>,
    n_in_entries: usize,
    n_out_entries: usize,
    cube_data: Vec<f32>,
    cube_grid: usize,
}

impl OwnedLutSampler {
    fn from_warehouse(
        warehouse: &LutWarehouse,
        expected_n_in: usize,
        expected_n_out: usize,
    ) -> Option<Self> {
        let lut = match warehouse {
            LutWarehouse::Lut(l) => l,
            // mAB/mBA (v4 multi-process elements) deferred.
            LutWarehouse::Multidimensional(_) => return None,
        };
        if lut.lut_type != LutType::Lut16 {
            // mft1 (lut8Type) deferred.
            return None;
        }
        if lut.num_input_channels as usize != expected_n_in
            || lut.num_output_channels as usize != expected_n_out
        {
            return None;
        }
        let input_table_u16 = match &lut.input_table {
            LutStore::Store16(v) => v.as_slice(),
            LutStore::Store8(_) => return None,
        };
        let output_table_u16 = match &lut.output_table {
            LutStore::Store16(v) => v.as_slice(),
            LutStore::Store8(_) => return None,
        };
        let clut_table_u16 = match &lut.clut_table {
            LutStore::Store16(v) => v.as_slice(),
            LutStore::Store8(_) => return None,
        };
        let n_in_entries = lut.num_input_table_entries as usize;
        let n_out_entries = lut.num_output_table_entries as usize;
        let cube_grid = lut.num_clut_grid_points as usize;
        if cube_grid < 2 || n_in_entries < 2 || n_out_entries < 2 {
            return None;
        }
        let in_total = n_in_entries.checked_mul(expected_n_in)?;
        if input_table_u16.len() < in_total {
            return None;
        }
        let out_total = n_out_entries.checked_mul(expected_n_out)?;
        if output_table_u16.len() < out_total {
            return None;
        }
        let mut cube_total: usize = expected_n_out;
        for _ in 0..expected_n_in {
            cube_total = cube_total.checked_mul(cube_grid)?;
        }
        if clut_table_u16.len() < cube_total {
            return None;
        }
        let input_table: Vec<f32> = input_table_u16[..in_total]
            .iter()
            .map(|&v| v as f32 / 65535.0)
            .collect();
        let output_table: Vec<f32> = output_table_u16[..out_total]
            .iter()
            .map(|&v| v as f32 / 65535.0)
            .collect();
        let cube_data: Vec<f32> = clut_table_u16[..cube_total]
            .iter()
            .map(|&v| v as f32 / 65535.0)
            .collect();
        Some(OwnedLutSampler {
            input_table,
            output_table,
            n_in_entries,
            n_out_entries,
            cube_data,
            cube_grid,
        })
    }

    /// 3-in / 3-out: RGB source → mft2 PCS-Lab encoded `[L, a, b]`.
    ///
    /// Returns the post-output-curve values directly (each in `[0, 1]`,
    /// 65280-denominated) so they slot straight into a downstream B2A
    /// LUT's input curves without an intervening Lab decode/encode
    /// round-trip.
    fn sample_rgb_to_pcs_lab_raw(&self, r: f32, g: f32, b: f32) -> [f32; 3] {
        let r_in = sample_curve_f32(&self.input_table, 0, self.n_in_entries, r);
        let g_in = sample_curve_f32(&self.input_table, 1, self.n_in_entries, g);
        let b_in = sample_curve_f32(&self.input_table, 2, self.n_in_entries, b);
        let cube = match Cube::new(&self.cube_data, self.cube_grid, 3) {
            Ok(c) => c,
            Err(_) => return [0.0, 0.5, 0.5],
        };
        let pcs = cube.trilinear_vec3(r_in, g_in, b_in);
        let l_post = sample_curve_f32(&self.output_table, 0, self.n_out_entries, pcs.v[0]);
        let a_post = sample_curve_f32(&self.output_table, 1, self.n_out_entries, pcs.v[1]);
        let b_post = sample_curve_f32(&self.output_table, 2, self.n_out_entries, pcs.v[2]);
        [
            l_post.clamp(0.0, 1.0),
            a_post.clamp(0.0, 1.0),
            b_post.clamp(0.0, 1.0),
        ]
    }

    /// 3-in / 4-out: mft2 PCS-Lab → CMYK in `[0, 1]`. Input is the raw
    /// 65280-denominated form so this composes byte-for-byte with the
    /// `lut16Type` upstream of it.
    fn sample_pcs_lab_to_cmyk(&self, pcs_lab: [f32; 3]) -> [f32; 4] {
        let l_in = sample_curve_f32(
            &self.input_table,
            0,
            self.n_in_entries,
            pcs_lab[0].clamp(0.0, 1.0),
        );
        let a_in = sample_curve_f32(
            &self.input_table,
            1,
            self.n_in_entries,
            pcs_lab[1].clamp(0.0, 1.0),
        );
        let b_in = sample_curve_f32(
            &self.input_table,
            2,
            self.n_in_entries,
            pcs_lab[2].clamp(0.0, 1.0),
        );
        let cube = match Cube::new(&self.cube_data, self.cube_grid, 4) {
            Ok(c) => c,
            Err(_) => return [0.0; 4],
        };
        let pcs = cube.trilinear_vec4(l_in, a_in, b_in);
        let c = sample_curve_f32(&self.output_table, 0, self.n_out_entries, pcs.v[0]);
        let m = sample_curve_f32(&self.output_table, 1, self.n_out_entries, pcs.v[1]);
        let y = sample_curve_f32(&self.output_table, 2, self.n_out_entries, pcs.v[2]);
        let k = sample_curve_f32(&self.output_table, 3, self.n_out_entries, pcs.v[3]);
        [
            c.clamp(0.0, 1.0),
            m.clamp(0.0, 1.0),
            y.clamp(0.0, 1.0),
            k.clamp(0.0, 1.0),
        ]
    }
}

/// 1D curve lookup over a pre-`/65535`-converted `f32` table. Mirrors
/// [`sample_curve`] above but skips the per-call integer-to-float
/// division so per-pixel runtime cost stays low.
#[inline]
fn sample_curve_f32(table: &[f32], ch: usize, n_entries: usize, x: f32) -> f32 {
    let base = ch * n_entries;
    let scale = (n_entries - 1) as f32;
    let pos = x.clamp(0.0, 1.0) * scale;
    let i0 = pos.floor() as usize;
    let i1 = (i0 + 1).min(n_entries - 1);
    let t = pos - i0 as f32;
    let v0 = table[base + i0];
    let v1 = table[base + i1];
    v0 + (v1 - v0) * t
}

/// Source-RGB → OutputIntent-CMYK chain stage 1, evaluated per pixel by
/// composing two ICC `lut16Type` tables (or a shaper-matrix source +
/// LUT-based OI). Wraps cleanly into the
/// `Arc<dyn TransformExecutor<{u8,f64}> + Send + Sync>` slot that the
/// `ChainedTransform` in `super::IccCache` expects.
pub(super) struct HandRolledChainStage1Rgb {
    src: SourceA2BSampler,
    oi: LabToCmykSampler,
}

impl HandRolledChainStage1Rgb {
    /// Build the chain stage-1 sampler from the source RGB profile and
    /// the OutputIntent CMYK profile, picking the source's A2B table
    /// and the OI's B2A table for the given rendering intent. Returns
    /// `None` when either side has an unsupported tag layout (mAB /
    /// mft1 / non-Lab PCS / no shaper-matrix fallback) — the caller
    /// falls back to the moxcms-driven chain in that case.
    pub(super) fn new(
        source: &ColorProfile,
        output_intent: &ColorProfile,
        intent: RenderingIntent,
    ) -> Option<Self> {
        if source.color_space != DataColorSpace::Rgb {
            return None;
        }
        let src = SourceA2BSampler::new(source, intent)?;
        let oi = LabToCmykSampler::new(output_intent, intent)?;
        Some(Self { src, oi })
    }

    #[inline]
    fn sample(&self, r: f32, g: f32, b: f32) -> [f32; 4] {
        let pcs = self.src.sample_pcs_lab(r, g, b);
        self.oi.sample_pcs_lab(pcs)
    }

    /// Run a single source-RGB sample through the chain and return the
    /// intermediate OutputIntent CMYK as `f64` in `[0, 1]`. Used by
    /// `IccCache::convert_to_oi_cmyk` so the PDF reader can record the
    /// chain's CMYK output as `DeviceColor::native_cmyk`; the renderer's
    /// CMYK-buffer composite path then has the same native ink values
    /// it would get from a `k`-operator paint.
    pub(super) fn sample_cmyk_f64(&self, r: f64, g: f64, b: f64) -> [f64; 4] {
        let cmyk = self.sample(r as f32, g as f32, b as f32);
        [
            cmyk[0] as f64,
            cmyk[1] as f64,
            cmyk[2] as f64,
            cmyk[3] as f64,
        ]
    }
}

impl TransformExecutor<u8> for HandRolledChainStage1Rgb {
    fn transform(&self, src: &[u8], dst: &mut [u8]) -> Result<(), CmsError> {
        let pixel_count = dst.len() / 4;
        for px in 0..pixel_count {
            let r = src[px * 3] as f32 / 255.0;
            let g = src[px * 3 + 1] as f32 / 255.0;
            let b = src[px * 3 + 2] as f32 / 255.0;
            let cmyk = self.sample(r, g, b);
            dst[px * 4] = (cmyk[0] * 255.0).round().clamp(0.0, 255.0) as u8;
            dst[px * 4 + 1] = (cmyk[1] * 255.0).round().clamp(0.0, 255.0) as u8;
            dst[px * 4 + 2] = (cmyk[2] * 255.0).round().clamp(0.0, 255.0) as u8;
            dst[px * 4 + 3] = (cmyk[3] * 255.0).round().clamp(0.0, 255.0) as u8;
        }
        Ok(())
    }
}

impl TransformExecutor<f64> for HandRolledChainStage1Rgb {
    fn transform(&self, src: &[f64], dst: &mut [f64]) -> Result<(), CmsError> {
        let pixel_count = dst.len() / 4;
        for px in 0..pixel_count {
            let r = src[px * 3] as f32;
            let g = src[px * 3 + 1] as f32;
            let b = src[px * 3 + 2] as f32;
            let cmyk = self.sample(r, g, b);
            dst[px * 4] = cmyk[0] as f64;
            dst[px * 4 + 1] = cmyk[1] as f64;
            dst[px * 4 + 2] = cmyk[2] as f64;
            dst[px * 4 + 3] = cmyk[3] as f64;
        }
        Ok(())
    }
}
