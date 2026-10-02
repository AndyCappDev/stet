// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Hand-rolled samplers for ICC `lut16Type` tables.
//!
//! moxcms's `create_transform` routes CMYK profiles through an internal
//! Lab→sRGB pipeline that over-saturates light/midtone colours noticeably
//! relative to lcms2 / Acrobat / Ghostscript. This module bypasses moxcms
//! for v2 `lut16Type` CMYK profiles: each grid point goes through one of
//! the profile's A2B CLUTs (chosen per rendering intent), the legacy v2
//! PCS-Lab decode, and a hand-tuned Lab → XYZ-D50 → sRGB pipeline. Through
//! `A2B1` the output matches lcms2's `cmsDoTransform(RelCol)` to ±1 RGB
//! level on a 17⁴ sweep against ISO Coated v2 300% (ECI), which is what GS
//! produces for
//! typical print imagery (light greens, neutrals, blacks). Out-of-gamut
//! colours clip to the sRGB boundary (also matching lcms2 / GS).
//!
//! The same building blocks are reused for the PDF/X **proofing chain**
//! (`source → OutputIntent CMYK`, the first leg of the source-through-OI
//! chain in `register_profile_with_n`). [`HandRolledChainStage1Rgb`]
//! composes a `SourceA2BSampler` and a `LabToCmykSampler` per pixel;
//! [`HandRolledChainStage1Cmyk`] composes a CMYK source's A2B table and the
//! OutputIntent's B2A table with lcms2's own interpolation, and
//! [`HandRolledChainStage1Gray`] a Gray source's tone curve, black-point
//! compensation and the OutputIntent's B2A table. All three implement
//! [`moxcms::TransformExecutor`] for `u8` and `f64` so they slot directly
//! into the `ChainedTransform` stage-1 slot.
//!
//! Profiles whose tables are `mAB`/`mBA` (v4 multi-process elements) fall
//! back to moxcms; callers detect the `None` return and use the existing
//! path. `lut8Type` (mft1) tables are read only by the evaluators that
//! reproduce lcms2 exactly (see [`OwnedLutSampler::lcms_exact`]): the
//! black points lcms2 would detect, the CMYK and Gray chain stage 1, and an
//! RGB source's table in the RGB chain stage 1. Widening the bake, or the
//! RGB chain's output-intent side, to them would move every colour through
//! those profiles.
//!
//! Tone curves — an RGB source's and a Gray source's — evaluate as lcms2
//! does (`LcmsCurve`), not through moxcms's evaluator, whose pure gamma is
//! garbage at 0.

use moxcms::{
    CmsError, ColorProfile, Cube, DataColorSpace, Hypercube, Lab, LutStore, LutType, LutWarehouse,
    Matrix3d, RenderingIntent, ToneReprCurve, TransformExecutor, Xyz,
};

use super::Clut4;
use super::black_point::SourceBlack;
use super::bpc::{
    BpcParams, WP_D50, apply_bpc_xyz_d50, compute_bpc_params, lab_to_xyz_d50, xyz_d50_to_lab,
};

/// `0xFF00` — the legacy ICC v2 Lab denominator for L*. Stored values in
/// `[0, 0xFF00]` map linearly to L*∈[0, 100]; values above `0xFF00` are
/// reserved to allow encoders to round above the maximum without losing
/// the legal range.
const PCS_LAB_DENOM: f32 = 65280.0;

/// Sample one of a CMYK profile's A2B tables into a [`Clut4`].
///
/// `table` is the profile's `A2B0`, `A2B1` or `A2B2` — the caller picks it
/// for the rendering intent (see [`super::cmyk_tables`]). Out-of-gamut Lab
/// values clip to the sRGB boundary, matching lcms2's `cmsDoTransform` and
/// avoiding the desaturation that hue-preserving chroma compression
/// produces on pure process primaries (e.g. CMYK yellow → washed-out
/// lemon).
///
/// Returns `None` when the profile or table is a shape we currently defer
/// (mAB / mft1 / non-Lab PCS / non-CMYK input). In every `None` case the
/// caller is expected to fall back to the moxcms-driven
/// [`super::bake_clut4`] path.
///
/// `grid_n` controls the output CLUT resolution (the existing path uses
/// 17). With a `black` point, black-point compensation is folded into
/// every grid point, so per-pixel runtime cost stays at zero.
pub(super) fn bake_clut4_hand_rolled(
    profile: &ColorProfile,
    table: &LutWarehouse,
    grid_n: usize,
    black: Option<SourceBlack>,
) -> Option<Clut4> {
    if profile.color_space != DataColorSpace::Cmyk || profile.pcs != DataColorSpace::Lab {
        return None;
    }
    if !(2..=33).contains(&grid_n) {
        return None;
    }

    let lut = OwnedLutSampler::from_warehouse(table, 4, 3)?;

    // The darker colorant is taken from this sampler's own (1,1,1,1)
    // output, not moxcms's transform output, so the source black point
    // matches what the bake actually produces. Computing it against
    // moxcms's transform miscalibrates the post-correction and leaves
    // K-heavy CMYK ~13 RGB levels lighter than baseline / GS.
    let bpc = black.map(|black| {
        let sbp = match black {
            SourceBlack::DarkerColorant => {
                let lab_k = lut.sample_cmyk_to_lab(1.0, 1.0, 1.0, 1.0);
                let l_star = (lab_k.l as f64 * 100.0).clamp(0.0, 100.0);
                lab_to_xyz_d50([l_star.min(50.0), 0.0, 0.0])
            }
            SourceBlack::Xyz(xyz) => xyz,
        };
        compute_bpc_params(sbp, [0.0; 3], WP_D50)
    });
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

                    // Black-point compensation scales XYZ before the sRGB
                    // gamut clip, as lcms2 does. Scaling the clipped sRGB
                    // instead moves out-of-gamut colours: one channel is
                    // already pinned, so the others shift (6 levels on a
                    // saturated cyan).
                    let mut xyz = lab_to_xyz_d50_abs(lut.sample_cmyk_to_lab(c, m, y, k));
                    if let Some(p) = bpc {
                        xyz = apply_bpc_xyz_d50(xyz, p);
                    }
                    let rgb = encode_linear_srgb(xyz_d50_to_linear_srgb_d65(xyz));

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

/// lcms2's black-point round trip for a CMYK output profile: Lab `start`
/// through the perceptual `B2A0`, then back through the colorimetric
/// `A2B1` (`A2B0` when there is none, as lcms2 reads it). Returns the ink
/// it passed through and the Lab it landed on; `None` when the profile is
/// not CMYK with a Lab PCS, or a table is one these evaluators cannot read
/// (v4 `mAB`/`mBA`).
pub(super) fn perceptual_round_trip(
    profile: &ColorProfile,
    start: [f64; 3],
) -> Option<([f64; 4], [f64; 3])> {
    Some(RoundTrip::new(profile, RenderingIntent::Perceptual)?.run(start))
}

/// lcms2's round trip through a CMYK output profile
/// (`CreateRoundtripXForm`): Lab through the profile's B2A table for an
/// intent, then back through its colorimetric A2B table, each as lcms2
/// reads it (see [`lcms_table`]).
pub(super) struct RoundTrip {
    b2a: OwnedLutSampler,
    a2b: OwnedLutSampler,
}

impl RoundTrip {
    /// `None` when the profile is not CMYK with a Lab PCS, lacks a table
    /// the round trip needs, or carries one these evaluators cannot read
    /// (v4 `mAB`/`mBA`).
    pub(super) fn new(profile: &ColorProfile, intent: RenderingIntent) -> Option<Self> {
        if profile.color_space != DataColorSpace::Cmyk || profile.pcs != DataColorSpace::Lab {
            return None;
        }
        let b2a = lcms_table(
            [
                profile.lut_b_to_a_perceptual.as_ref(),
                profile.lut_b_to_a_colorimetric.as_ref(),
                profile.lut_b_to_a_saturation.as_ref(),
            ],
            intent,
        )?;
        let a2b = lcms_table(
            [
                profile.lut_a_to_b_perceptual.as_ref(),
                profile.lut_a_to_b_colorimetric.as_ref(),
                profile.lut_a_to_b_saturation.as_ref(),
            ],
            RenderingIntent::RelativeColorimetric,
        )?;
        Some(Self {
            b2a: OwnedLutSampler::lcms_exact(b2a, 3, 4)?,
            a2b: OwnedLutSampler::lcms_exact(a2b, 4, 3)?,
        })
    }

    /// Lab → the ink it passes through and the Lab it lands on.
    pub(super) fn run(&self, lab: [f64; 3]) -> ([f64; 4], [f64; 3]) {
        let ink = self.b2a.lab_to_ink(lab);
        (ink, self.a2b.ink_to_lab(ink))
    }
}

/// The tone curve of a TRC-only Gray profile with an XYZ PCS — the usual
/// kind — which lcms2 treats as a matrix-shaper: gray → Y relative to the
/// white, the colour neutral.
pub(super) struct GrayTrc(LcmsCurve);

impl GrayTrc {
    /// `None` for anything else: a Gray profile with a LUT or a Lab PCS.
    pub(super) fn new(profile: &ColorProfile) -> Option<Self> {
        if profile.color_space != DataColorSpace::Gray
            || profile.pcs != DataColorSpace::Xyz
            || profile.lut_a_to_b_perceptual.is_some()
            || profile.lut_a_to_b_colorimetric.is_some()
            || profile.lut_a_to_b_saturation.is_some()
        {
            return None;
        }
        Some(Self(LcmsCurve::new(profile.gray_trc.as_ref()?)?))
    }

    /// Y of `gray` (`[0, 1]`), relative to the white.
    pub(super) fn y(&self, gray: f64) -> f64 {
        self.0.eval(gray.clamp(0.0, 1.0)).clamp(0.0, 1.0)
    }
}

/// A profile's tone curve as lcms2 evaluates it in a floating-point
/// transform (`cmsEvalToneCurveFloat`). moxcms's own evaluator computes a
/// pure gamma with an approximate power that is garbage at 0 — about
/// −1.7 × 10⁸ for γ 1.8 — which, through an RGB profile's matrix, wrecks
/// every colour with a zero channel.
enum LcmsCurve {
    /// A `curv` with one entry, `x^γ`: lcms2 builds it as a parametric
    /// curve. No entries is γ 1.
    Gamma(f64),
    /// A `curv` table, which lcms2 evaluates with 16-bit integer
    /// interpolation (`LinLerp1D`) even in a floating-point transform.
    Table(Vec<u16>),
    /// A `para` curve: lcms2's type (1–5, the ICC's function type + 1) and
    /// its parameters `[g, a, b, c, d, e, f]`.
    Parametric(u8, [f64; 7]),
}

impl LcmsCurve {
    /// `None` for a parametric curve of an unknown type.
    fn new(curve: &ToneReprCurve) -> Option<Self> {
        Some(match curve {
            ToneReprCurve::Lut(table) => match table.as_slice() {
                [] => LcmsCurve::Gamma(1.0),
                [gamma] => LcmsCurve::Gamma(f64::from(*gamma) / 256.0),
                _ => LcmsCurve::Table(table.clone()),
            },
            ToneReprCurve::Parametric(params) => {
                let kind = match params.len() {
                    1 => 1,
                    3 => 2,
                    4 => 3,
                    5 => 4,
                    7 => 5,
                    _ => return None,
                };
                let mut p = [0.0; 7];
                for (dst, &src) in p.iter_mut().zip(params) {
                    *dst = f64::from(src);
                }
                LcmsCurve::Parametric(kind, p)
            }
        })
    }

    /// The curve at `x`, as lcms2's `DefaultEvalParametricFn` and
    /// `LinLerp1D` compute it.
    fn eval(&self, x: f64) -> f64 {
        // lcms2's MATRIX_DET_TOLERANCE.
        const TOLERANCE: f64 = 0.0001;
        match self {
            LcmsCurve::Gamma(g) => Self::gamma(*g, x),
            LcmsCurve::Table(table) => Self::table(table, x),
            &LcmsCurve::Parametric(kind, [g, a, b, c, d, e, f]) => match kind {
                1 => Self::gamma(g, x),
                // (ax + b)^g for x ≥ −b/a, else 0.
                2 if a.abs() < TOLERANCE => 0.0,
                2 if x >= -b / a && a * x + b > 0.0 => (a * x + b).powf(g),
                2 => 0.0,
                // (ax + b)^g + c for x ≥ −b/a (at least 0), else c.
                3 if a.abs() < TOLERANCE => 0.0,
                3 if x >= (-b / a).max(0.0) => {
                    let base = a * x + b;
                    if base > 0.0 { base.powf(g) + c } else { 0.0 }
                }
                3 => c,
                // (ax + b)^g for x ≥ d, else cx.
                4 if x >= d && a * x + b > 0.0 => (a * x + b).powf(g),
                4 if x >= d => 0.0,
                4 => c * x,
                // (ax + b)^g + e for x ≥ d, else cx + f.
                _ if x >= d && a * x + b > 0.0 => (a * x + b).powf(g) + e,
                _ if x >= d => e,
                _ => c * x + f,
            },
        }
    }

    fn gamma(g: f64, x: f64) -> f64 {
        if x >= 0.0 {
            x.powf(g)
        } else if (g - 1.0).abs() < 0.0001 {
            x
        } else {
            0.0
        }
    }

    /// lcms2's 16-bit table lookup: the input saturated to a 16-bit word,
    /// then interpolated in 16.16 fixed point.
    fn table(table: &[u16], x: f64) -> f64 {
        let input = (x * 65535.0 + 0.5).floor().clamp(0.0, 65535.0) as u64;
        let domain = (table.len() - 1) as u64;
        if input == 0xFFFF || domain == 0 {
            return f64::from(table[domain as usize]) / 65535.0;
        }
        let v = domain * input;
        let v = v + (v + 0x7FFF) / 0xFFFF;
        let cell = (v >> 16) as usize;
        let rest = (v & 0xFFFF) as i64;
        let (y0, y1) = (i64::from(table[cell]), i64::from(table[cell + 1]));
        let out = (((y1 - y0) * rest + 0x8000) >> 16) + y0;
        out as f64 / 65535.0
    }
}

/// Lab of 400% ink through a CMYK A2B `table`, as lcms2 evaluates it.
pub(super) fn table_black(table: &LutWarehouse) -> Option<[f64; 3]> {
    Some(OwnedLutSampler::lcms_exact(table, 4, 3)?.ink_to_lab([1.0; 4]))
}

/// Whether [`bake_clut4_hand_rolled`] can read `table` of `profile`.
pub(super) fn can_sample(profile: &ColorProfile, table: &LutWarehouse) -> bool {
    profile.color_space == DataColorSpace::Cmyk
        && profile.pcs == DataColorSpace::Lab
        && OwnedLutSampler::from_warehouse(table, 4, 3).is_some()
}

/// Decode normalised Lab (moxcms encoding) to absolute XYZ-D50
/// (`Y_white = 1.0`).
fn lab_to_xyz_d50_abs(lab: Lab) -> [f64; 3] {
    // moxcms's `to_pcs_xyz` divides by `(1 + 32767/32768)` to land in the
    // ICC PCS XYZ encoding where the reference white maps to ≈0.5. Undo
    // that here so the matrix sees absolute XYZ (Y_white = 1.0).
    let xyz = lab.to_pcs_xyz();
    const PCS_UNDO: f64 = 1.0 + 32767.0 / 32768.0;
    [
        xyz.x as f64 * PCS_UNDO,
        xyz.y as f64 * PCS_UNDO,
        xyz.z as f64 * PCS_UNDO,
    ]
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
//   1. Source RGB → PCS Lab through the profile's A2B table for the intent,
//      else its A2B0, interpolated tetrahedrally as lcms2 does; only a
//      profile with neither goes through its tone curves and colorant
//      matrix, the curves evaluated as lcms2 evaluates them.
//   2. That Lab, in mft2 PCS-Lab encoding, passes directly into the
//      OutputIntent's B2A `lut16Type` input curves.
//   3. CLUT trilinear → output curves on the OI side → CMYK in `[0, 1]`.
//
// The composition is evaluated **per pixel** rather than baked into an
// intermediate CLUT. A pre-bake at the typical `17^3` grid resolution
// introduces a quantization layer between the two profile CLUTs that
// drifts the GWG 13.0 BG-vs-X match by ~6 RGB levels even though each leg
// is locally exact. moxcms's chain composes the same two CLUTs per pixel,
// and to match its byte-level accuracy we do the same.
//
// Both legs take the rendering intent and read that intent's tables
// (A2B0/B2A0 perceptual, A2B1/B2A1 colorimetric, A2B2/B2A2 saturation).
// The source falls back as lcms2 reads it (see step 1); the output intent
// to whichever B2A table the profile carries. Absolute colorimetric reads
// the colorimetric tables; its white-point adaptation is not applied.

/// Source RGB profile A2B sampler: either an owned `lut16Type` or
/// `lut8Type` table, or a shaper-matrix (TRC + colorant matrix). Held by
/// value inside
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
        // lcms2 reads the intent's A2B table, else A2B0, and only when the
        // profile has neither its tone curves and colorant matrix, which
        // give the same XYZ under every intent. A table these evaluators
        // cannot read — v4 `mAB`, an XYZ PCS, a non-identity matrix — has
        // no hand-rolled stage: the caller falls back to moxcms rather than
        // to a matrix lcms2 would not use.
        let table = lcms_table(
            [
                profile.lut_a_to_b_perceptual.as_ref(),
                profile.lut_a_to_b_colorimetric.as_ref(),
                profile.lut_a_to_b_saturation.as_ref(),
            ],
            intent,
        );
        match table {
            Some(table) if profile.pcs == DataColorSpace::Lab => {
                OwnedLutSampler::lcms_exact(table, 3, 3).map(SourceA2BSampler::Lut)
            }
            Some(_) => None,
            None => ShaperMatrix::new(profile).map(SourceA2BSampler::Shaper),
        }
    }

    /// Returns the source profile's A2B output as `[L, a, b]` in mft2
    /// PCS-Lab encoding (each component in `[0, 1]`, `0xFF00`-denominated).
    /// LUT-based profiles return their post-output-curve values, re-encoded
    /// from a `lut8Type` table's full scale; shaper-matrix profiles compute
    /// Lab via TRC + colorant matrix and re-encode to mft2.
    fn sample_pcs_lab(&self, r: f32, g: f32, b: f32) -> [f32; 3] {
        match self {
            SourceA2BSampler::Lut(lut) => lut
                .rgb_to_pcs_lab([r, g, b].map(f64::from))
                .map(|v| v as f32),
            SourceA2BSampler::Shaper(sm) => sm.sample_pcs_lab(r, g, b),
        }
    }
}

/// Shaper-matrix RGB profile sampler. Linearises with the per-channel tone
/// curves as lcms2 evaluates them, multiplies through the colorant matrix,
/// and converts the resulting XYZ-D50 to moxcms-encoded Lab.
///
/// `Lab::from_pcs_xyz` expects PCS-encoded XYZ (the ICC PCS encoding
/// where the white point lands at ≈ 0.5, equal to absolute XYZ divided
/// by `1 + 32767/32768` ≈ 2.0). The colorant matrix
/// (`ColorProfile::colorant_matrix`) returns absolute XYZ-D50, so we
/// fold the encoding factor into the matrix once at construction time
/// rather than dividing per pixel.
struct ShaperMatrix {
    trc_r: LcmsCurve,
    trc_g: LcmsCurve,
    trc_b: LcmsCurve,
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

        let trc_r = LcmsCurve::new(red_trc)?;
        let trc_g = LcmsCurve::new(green_trc)?;
        let trc_b = LcmsCurve::new(blue_trc)?;

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
        let lin_r = self.trc_r.eval(f64::from(r));
        let lin_g = self.trc_g.eval(f64::from(g));
        let lin_b = self.trc_b.eval(f64::from(b));

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
pub struct LabToCmykSampler {
    lut: OwnedLutSampler,
}

impl LabToCmykSampler {
    pub(super) fn new(profile: &ColorProfile, intent: RenderingIntent) -> Option<Self> {
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

    pub(super) fn build(
        profile: &ColorProfile,
        intent: RenderingIntent,
    ) -> Option<LabToCmykSampler> {
        Self::new(profile, intent)
    }

    fn sample_pcs_lab(&self, pcs_lab: [f32; 3]) -> [f32; 4] {
        self.lut.sample_pcs_lab_to_cmyk(pcs_lab)
    }

    /// Convert a PDF Lab triplet (L\* ∈ [0, 100], a\*/b\* ∈ [-128, 127]) to
    /// OutputIntent CMYK by encoding to the mft2 PCS-Lab grid the OI's B2A
    /// curves expect, then sampling the LUT. Used by `IccCache` to populate
    /// `DeviceColor::native_cmyk` for Lab fills, so the parallel CMYK buffer
    /// holds the same direct Lab→OI value Acrobat's ACE produces (rather
    /// than stet's Lab→sRGB→ICC-reverse approximation, which drifts under
    /// CMYK-group blends — GWG 22.1's ColorBurn form).
    pub fn sample_pdf_lab(&self, l_star: f64, a_star: f64, b_star: f64) -> [f64; 4] {
        let l_norm = (l_star / 100.0).clamp(0.0, 1.0) as f32;
        let a_norm = ((a_star + 128.0) / 255.0).clamp(0.0, 1.0) as f32;
        let b_norm = ((b_star + 128.0) / 255.0).clamp(0.0, 1.0) as f32;
        // Re-encode moxcms-Lab to mft2 PCS-Lab format (denom 65280) so the
        // value lines up with the OI B2A input curves' grid axis. Mirrors
        // the encoding stage 1's `Shaper`/`OwnedLutSampler` path uses.
        let scale = PCS_LAB_DENOM / 65535.0;
        let pcs = [
            (l_norm * scale).clamp(0.0, 1.0),
            (a_norm * scale).clamp(0.0, 1.0),
            (b_norm * scale).clamp(0.0, 1.0),
        ];
        let cmyk = self.sample_pcs_lab(pcs);
        [
            cmyk[0] as f64,
            cmyk[1] as f64,
            cmyk[2] as f64,
            cmyk[3] as f64,
        ]
    }
}

/// How a `lut8Type` or `lut16Type` table encodes Lab on its PCS side, as
/// lcms2 reads it. `_cmsReadInputLUT` and `_cmsReadOutputLUT` treat a
/// `lut16Type` Lab table as the legacy ICC v2 encoding (L\* = 100 at
/// `0xFF00`) and convert it; a `lut8Type` one they read as v4 (L\* = 100 at
/// `0xFF`), which is also what the 8-bit v2 encoding is.
#[derive(Clone, Copy, Debug, PartialEq)]
enum LabEncoding {
    /// `lut16Type`: `0xFF00`-denominated.
    V2,
    /// `lut8Type`: full-scale.
    V4,
}

impl LabEncoding {
    /// Lab (L\* 0–100, a\*/b\* −128–127) → the table's `[0, 1]` axes.
    fn encode(self, lab: [f64; 3]) -> [f64; 3] {
        let full = [
            lab[0] / 100.0,
            (lab[1] + 128.0) / 255.0,
            (lab[2] + 128.0) / 255.0,
        ];
        match self {
            LabEncoding::V4 => full,
            LabEncoding::V2 => full.map(|v| v * f64::from(PCS_LAB_DENOM) / 65535.0),
        }
    }

    /// The table's `[0, 1]` outputs → Lab.
    fn decode(self, v: [f64; 3]) -> [f64; 3] {
        let full = match self {
            LabEncoding::V4 => v,
            LabEncoding::V2 => v.map(|x| x * 65535.0 / f64::from(PCS_LAB_DENOM)),
        };
        [
            full[0] * 100.0,
            full[1] * 255.0 - 128.0,
            full[2] * 255.0 - 128.0,
        ]
    }
}

/// Owned sampler for a profile's `lut16Type` table — and, for the
/// lcms2-exact evaluators only, its `lut8Type` one — in the `(n_in, n_out)`
/// shapes stet
/// reads: `(4, 3)` for a CMYK A2B → Lab, `(3, 3)` for an RGB-source A2B →
/// Lab and `(3, 4)` for an OutputIntent B2A → CMYK. Stores its own copies
/// of the table data so the sampler outlives the source `ColorProfile`.
///
/// Curves and grid are pre-converted to `f32` in `[0, 1]` once at
/// construction so the per-sample curve lookups don't repeat the division.
struct OwnedLutSampler {
    input_table: Vec<f32>,
    output_table: Vec<f32>,
    n_in_entries: usize,
    n_out_entries: usize,
    cube_data: Vec<f32>,
    cube_grid: usize,
    encoding: LabEncoding,
}

impl OwnedLutSampler {
    /// A `lut16Type` table of shape `(expected_n_in, expected_n_out)`.
    fn from_warehouse(
        warehouse: &LutWarehouse,
        expected_n_in: usize,
        expected_n_out: usize,
    ) -> Option<Self> {
        Self::load(warehouse, expected_n_in, expected_n_out, false)
    }

    /// A `lut8Type` or `lut16Type` table, for evaluating it exactly as
    /// lcms2 does — see [`Self::lab_to_ink`], [`Self::ink_to_lab`] and
    /// [`Self::rgb_to_pcs_lab`]: the black point lcms2 detects, and the
    /// CMYK, Gray and RGB-source chain stage 1. A
    /// three-input table must carry the identity matrix, as the ICC
    /// requires of a Lab-indexed one; lcms2 would apply any other, and these
    /// evaluators do not.
    fn lcms_exact(
        warehouse: &LutWarehouse,
        expected_n_in: usize,
        expected_n_out: usize,
    ) -> Option<Self> {
        if let LutWarehouse::Lut(lut) = warehouse
            && expected_n_in == 3
            && lut.matrix != Matrix3d::IDENTITY
        {
            return None;
        }
        Self::load(warehouse, expected_n_in, expected_n_out, true)
    }

    fn load(
        warehouse: &LutWarehouse,
        expected_n_in: usize,
        expected_n_out: usize,
        allow_lut8: bool,
    ) -> Option<Self> {
        let lut = match warehouse {
            LutWarehouse::Lut(l) => l,
            // mAB/mBA (v4 multi-process elements) deferred.
            LutWarehouse::Multidimensional(_) => return None,
        };
        let encoding = match lut.lut_type {
            LutType::Lut16 => LabEncoding::V2,
            LutType::Lut8 if allow_lut8 => LabEncoding::V4,
            _ => return None,
        };
        if lut.num_input_channels as usize != expected_n_in
            || lut.num_output_channels as usize != expected_n_out
        {
            return None;
        }
        let n_in_entries = lut.num_input_table_entries as usize;
        let n_out_entries = lut.num_output_table_entries as usize;
        let cube_grid = lut.num_clut_grid_points as usize;
        if cube_grid < 2 || n_in_entries < 2 || n_out_entries < 2 {
            return None;
        }
        let in_total = n_in_entries.checked_mul(expected_n_in)?;
        let out_total = n_out_entries.checked_mul(expected_n_out)?;
        let mut cube_total: usize = expected_n_out;
        for _ in 0..expected_n_in {
            cube_total = cube_total.checked_mul(cube_grid)?;
        }
        Some(OwnedLutSampler {
            input_table: normalised(&lut.input_table, in_total)?,
            output_table: normalised(&lut.output_table, out_total)?,
            n_in_entries,
            n_out_entries,
            cube_data: normalised(&lut.clut_table, cube_total)?,
            cube_grid,
            encoding,
        })
    }

    /// 4-in / 3-out: CMYK → moxcms-normalised Lab, through the input
    /// curves, a quadlinear lookup and the output curves. The bake's
    /// sampler.
    fn sample_cmyk_to_lab(&self, c: f32, m: f32, y: f32, k: f32) -> Lab {
        let c_in = sample_curve_f32(&self.input_table, 0, self.n_in_entries, c);
        let m_in = sample_curve_f32(&self.input_table, 1, self.n_in_entries, m);
        let y_in = sample_curve_f32(&self.input_table, 2, self.n_in_entries, y);
        let k_in = sample_curve_f32(&self.input_table, 3, self.n_in_entries, k);

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

        let l_post = sample_curve_f32(&self.output_table, 0, self.n_out_entries, pcs.v[0]);
        let a_post = sample_curve_f32(&self.output_table, 1, self.n_out_entries, pcs.v[1]);
        let b_post = sample_curve_f32(&self.output_table, 2, self.n_out_entries, pcs.v[2]);

        // moxcms normalises Lab as L*/100 and (a* + 128)/255. For the legacy
        // v2 encoding that is the raw value × 65535 / 0xFF00:
        //   L* = raw_L * 100 / 0xFF00 → l_norm = post_L * 65535 / 0xFF00
        //   a* = raw_a / 256 - 128    → a_norm = post_a * 65535 / 0xFF00
        // and for v4 the raw value itself.
        let scale = match self.encoding {
            LabEncoding::V2 => 65535.0 / PCS_LAB_DENOM,
            LabEncoding::V4 => 1.0,
        };
        Lab::new(
            (l_post * scale).clamp(0.0, 1.0),
            (a_post * scale).clamp(0.0, 1.0),
            (b_post * scale).clamp(0.0, 1.0),
        )
    }

    /// 4-in / 3-out: CMYK ink (each `[0, 1]`) → Lab, interpolated as lcms2
    /// interpolates a four-input table.
    fn ink_to_lab(&self, ink: [f64; 4]) -> [f64; 3] {
        let curved: [f64; 4] = std::array::from_fn(|ch| {
            sample_curve_f32(&self.input_table, ch, self.n_in_entries, ink[ch] as f32) as f64
        });
        let mut pcs = [0.0; 3];
        eval4_lcms(&self.cube_data, self.cube_grid, curved, &mut pcs);
        let post: [f64; 3] = std::array::from_fn(|ch| {
            sample_curve_f32(&self.output_table, ch, self.n_out_entries, pcs[ch] as f32) as f64
        });
        self.encoding.decode(post)
    }

    /// 3-in / 4-out: Lab → CMYK ink (each `[0, 1]`), trilinear as lcms2
    /// reads a Lab-indexed output table.
    fn lab_to_ink(&self, lab: [f64; 3]) -> [f64; 4] {
        let pcs = self.encoding.encode(lab).map(|v| v as f32);
        self.sample_pcs_lab_to_cmyk(pcs).map(f64::from)
    }

    /// 3-in / 3-out: an RGB source's ink → its PCS Lab in the legacy v2
    /// `lut16Type` encoding (each in `[0, 1]`, `0xFF00`-denominated), the
    /// form an OutputIntent's B2A input curves take: through the input
    /// curves, lcms2's tetrahedral interpolation and the output curves.
    fn rgb_to_pcs_lab(&self, rgb: [f64; 3]) -> [f64; 3] {
        let curved: [f64; 3] = std::array::from_fn(|ch| {
            sample_curve_f32(&self.input_table, ch, self.n_in_entries, rgb[ch] as f32) as f64
        });
        let mut pcs = [0.0; 3];
        eval3_lcms(&self.cube_data, self.cube_grid, curved, &mut pcs);
        let post: [f64; 3] = std::array::from_fn(|ch| {
            sample_curve_f32(&self.output_table, ch, self.n_out_entries, pcs[ch] as f32) as f64
        });
        let v2 = match self.encoding {
            LabEncoding::V2 => post,
            LabEncoding::V4 => post.map(|v| v * f64::from(PCS_LAB_DENOM) / 65535.0),
        };
        v2.map(|v| v.clamp(0.0, 1.0))
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

/// A table's first `len` entries, normalised to `[0, 1]`; `None` when it
/// holds fewer.
fn normalised(store: &LutStore, len: usize) -> Option<Vec<f32>> {
    match store {
        LutStore::Store16(v) => Some(v.get(..len)?.iter().map(|&x| x as f32 / 65535.0).collect()),
        LutStore::Store8(v) => Some(v.get(..len)?.iter().map(|&x| x as f32 / 255.0).collect()),
    }
}

/// lcms2's interpolation of a four-input table (`Eval4Inputs`): linear in
/// the first input, between tetrahedral interpolations over the other
/// three in the two grid planes either side of it. `cube` holds the grid
/// with the last input varying fastest and three outputs per point.
fn eval4_lcms(cube: &[f32], grid: usize, input: [f64; 4], out: &mut [f64; 3]) {
    let axis = |v, stride| grid_axis(v, grid, stride);
    let n_out = 3;
    let (k0, rk, k_step) = axis(input[0], n_out * grid * grid * grid);
    let x = axis(input[1], n_out * grid * grid);
    let y = axis(input[2], n_out * grid);
    let z = axis(input[3], n_out);
    let mut lo = [0.0; 3];
    let mut hi = [0.0; 3];
    tetrahedral3(cube, k0, x, y, z, &mut lo);
    tetrahedral3(cube, k0 + k_step, x, y, z, &mut hi);
    for ch in 0..n_out {
        out[ch] = lo[ch] + (hi[ch] - lo[ch]) * rk;
    }
}

/// lcms2's interpolation of a three-input table (`TetrahedralInterp16`):
/// tetrahedral. `cube` holds the grid with the last input varying fastest
/// and three outputs per point.
fn eval3_lcms(cube: &[f32], grid: usize, input: [f64; 3], out: &mut [f64; 3]) {
    let n_out = 3;
    let x = grid_axis(input[0], grid, n_out * grid * grid);
    let y = grid_axis(input[1], grid, n_out * grid);
    let z = grid_axis(input[2], grid, n_out);
    tetrahedral3(cube, 0, x, y, z, out);
}

/// One input's grid offset, fraction and the offset to the next grid
/// point, for an input in `[0, 1]` on a `grid`-point axis whose points are
/// `stride` apart.
fn grid_axis(v: f64, grid: usize, stride: usize) -> (usize, f64, usize) {
    let v = v.clamp(0.0, 1.0);
    let p = v * (grid - 1) as f64;
    let i = (p.floor() as usize).min(grid - 1);
    let step = if v >= 1.0 { 0 } else { stride };
    (i * stride, p - i as f64, step)
}

/// lcms2's `TetrahedralInterpFloat` over one three-input slice of a grid
/// starting at `base`; each axis is `(offset, fraction, step)`.
fn tetrahedral3(
    cube: &[f32],
    base: usize,
    (x0, rx, sx): (usize, f64, usize),
    (y0, ry, sy): (usize, f64, usize),
    (z0, rz, sz): (usize, f64, usize),
    out: &mut [f64; 3],
) {
    let (x1, y1, z1) = (x0 + sx, y0 + sy, z0 + sz);
    for (ch, o) in out.iter_mut().enumerate() {
        let d = |x: usize, y: usize, z: usize| f64::from(cube[base + x + y + z + ch]);
        let c0 = d(x0, y0, z0);
        let (c1, c2, c3) = if rx >= ry && ry >= rz {
            (
                d(x1, y0, z0) - c0,
                d(x1, y1, z0) - d(x1, y0, z0),
                d(x1, y1, z1) - d(x1, y1, z0),
            )
        } else if rx >= rz && rz >= ry {
            (
                d(x1, y0, z0) - c0,
                d(x1, y1, z1) - d(x1, y0, z1),
                d(x1, y0, z1) - d(x1, y0, z0),
            )
        } else if rz >= rx && rx >= ry {
            (
                d(x1, y0, z1) - d(x0, y0, z1),
                d(x1, y1, z1) - d(x1, y0, z1),
                d(x0, y0, z1) - c0,
            )
        } else if ry >= rx && rx >= rz {
            (
                d(x1, y1, z0) - d(x0, y1, z0),
                d(x0, y1, z0) - c0,
                d(x1, y1, z1) - d(x1, y1, z0),
            )
        } else if ry >= rz && rz >= rx {
            (
                d(x1, y1, z1) - d(x0, y1, z1),
                d(x0, y1, z0) - c0,
                d(x0, y1, z1) - d(x0, y1, z0),
            )
        } else if rz >= ry && ry >= rx {
            (
                d(x1, y1, z1) - d(x0, y1, z1),
                d(x0, y1, z1) - d(x0, y0, z1),
                d(x0, y0, z1) - c0,
            )
        } else {
            (0.0, 0.0, 0.0)
        };
        *o = c0 + c1 * rx + c2 * ry + c3 * rz;
    }
}

/// 1D curve lookup over a pre-`/65535`-converted `f32` table, so per-pixel
/// runtime cost stays low.
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
/// composing the source's A2B table (or its tone curves and colorant
/// matrix) with the OI's B2A `lut16Type` table. Wraps cleanly into the
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
    /// `None` when either side has a table these evaluators cannot read
    /// (v4 `mAB`/`mBA`, an XYZ PCS, a `lut8Type` B2A) or the source has
    /// neither table nor matrix — the caller falls back to the
    /// moxcms-driven chain in that case.
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

/// The table lcms2 reads for `intent` from a profile's three LUT tags,
/// `[perceptual, colorimetric, saturation]`: `_cmsReadInputLUT` and
/// `_cmsReadOutputLUT` take the intent's tag, or tag 0 when the profile has
/// none, each profile on its own. Absolute colorimetric reads the
/// colorimetric table; its white-point adaptation is not applied, as on
/// every other CMYK path.
fn lcms_table(
    tables: [Option<&LutWarehouse>; 3],
    intent: RenderingIntent,
) -> Option<&LutWarehouse> {
    let i = match intent {
        RenderingIntent::Perceptual => 0,
        RenderingIntent::RelativeColorimetric | RenderingIntent::AbsoluteColorimetric => 1,
        RenderingIntent::Saturation => 2,
    };
    tables[i].or(tables[0])
}

/// A copy of `profile` whose missing colorimetric and saturation LUT tags
/// are its perceptual ones, as [`lcms_table`] reads them. moxcms refuses an
/// intent whose tag is missing, so a transform it builds from these copies
/// reads the tables lcms2 would, even where source and output intent each
/// fall back differently.
pub(super) fn with_lcms_tables(profile: &ColorProfile) -> ColorProfile {
    let mut p = profile.clone();
    for table in [&mut p.lut_a_to_b_colorimetric, &mut p.lut_a_to_b_saturation] {
        if table.is_none() {
            table.clone_from(&profile.lut_a_to_b_perceptual);
        }
    }
    for table in [&mut p.lut_b_to_a_colorimetric, &mut p.lut_b_to_a_saturation] {
        if table.is_none() {
            table.clone_from(&profile.lut_b_to_a_perceptual);
        }
    }
    p
}

/// Source-CMYK → OutputIntent-CMYK chain stage 1 for one rendering intent,
/// as lcms2 converts without black-point compensation: the source's A2B
/// table for the intent to Lab, then the OutputIntent's B2A table for the
/// intent, composed per pixel so no intermediate table quantises it.
pub(super) struct HandRolledChainStage1Cmyk {
    a2b: OwnedLutSampler,
    b2a: OwnedLutSampler,
}

impl HandRolledChainStage1Cmyk {
    /// `None` when either profile is not CMYK with a Lab PCS, or carries
    /// tables these evaluators cannot read (v4 `mAB`/`mBA`); the caller
    /// then builds moxcms's transform for the same intent.
    pub(super) fn new(
        source: &ColorProfile,
        output_intent: &ColorProfile,
        intent: RenderingIntent,
    ) -> Option<Self> {
        let cmyk_lab = |p: &ColorProfile| {
            p.color_space == DataColorSpace::Cmyk && p.pcs == DataColorSpace::Lab
        };
        if !cmyk_lab(source) || !cmyk_lab(output_intent) {
            return None;
        }
        let a2b = lcms_table(
            [
                source.lut_a_to_b_perceptual.as_ref(),
                source.lut_a_to_b_colorimetric.as_ref(),
                source.lut_a_to_b_saturation.as_ref(),
            ],
            intent,
        )?;
        let b2a = lcms_table(
            [
                output_intent.lut_b_to_a_perceptual.as_ref(),
                output_intent.lut_b_to_a_colorimetric.as_ref(),
                output_intent.lut_b_to_a_saturation.as_ref(),
            ],
            intent,
        )?;
        Some(Self {
            a2b: OwnedLutSampler::lcms_exact(a2b, 4, 3)?,
            b2a: OwnedLutSampler::lcms_exact(b2a, 3, 4)?,
        })
    }

    /// Source ink (each `[0, 1]`) → OutputIntent ink.
    pub(super) fn sample(&self, ink: [f64; 4]) -> [f64; 4] {
        self.b2a.lab_to_ink(self.a2b.ink_to_lab(ink))
    }
}

impl TransformExecutor<u8> for HandRolledChainStage1Cmyk {
    fn transform(&self, src: &[u8], dst: &mut [u8]) -> Result<(), CmsError> {
        for (s, d) in src
            .as_chunks::<4>()
            .0
            .iter()
            .zip(dst.as_chunks_mut::<4>().0)
        {
            let ink = self.sample(s.map(|v| f64::from(v) / 255.0));
            *d = ink.map(|v| (v * 255.0).round().clamp(0.0, 255.0) as u8);
        }
        Ok(())
    }
}

impl TransformExecutor<f64> for HandRolledChainStage1Cmyk {
    fn transform(&self, src: &[f64], dst: &mut [f64]) -> Result<(), CmsError> {
        for (s, d) in src
            .as_chunks::<4>()
            .0
            .iter()
            .zip(dst.as_chunks_mut::<4>().0)
        {
            *d = self.sample(*s);
        }
        Ok(())
    }
}

/// Source-Gray → OutputIntent-CMYK chain stage 1 for one rendering intent,
/// as lcms2 converts it: the TRC's Y as neutral XYZ, black-point
/// compensation in XYZ when it applies, Lab, then the OutputIntent's B2A
/// table for the intent.
pub(super) struct HandRolledChainStage1Gray {
    trc: GrayTrc,
    bpc: Option<BpcParams>,
    b2a: OwnedLutSampler,
}

impl HandRolledChainStage1Gray {
    /// `None` when the source is not a TRC-only Gray ([`GrayTrc`]), or the
    /// output intent not CMYK with a Lab PCS and a B2A table these
    /// evaluators read.
    pub(super) fn new(
        source: &ColorProfile,
        output_intent: &ColorProfile,
        intent: RenderingIntent,
        bpc: Option<BpcParams>,
    ) -> Option<Self> {
        if output_intent.color_space != DataColorSpace::Cmyk
            || output_intent.pcs != DataColorSpace::Lab
        {
            return None;
        }
        let b2a = lcms_table(
            [
                output_intent.lut_b_to_a_perceptual.as_ref(),
                output_intent.lut_b_to_a_colorimetric.as_ref(),
                output_intent.lut_b_to_a_saturation.as_ref(),
            ],
            intent,
        )?;
        Some(Self {
            trc: GrayTrc::new(source)?,
            bpc,
            b2a: OwnedLutSampler::lcms_exact(b2a, 3, 4)?,
        })
    }

    /// Gray (`[0, 1]`) → OutputIntent ink.
    pub(super) fn sample(&self, gray: f64) -> [f64; 4] {
        let y = self.trc.y(gray);
        let mut xyz = [WP_D50[0] * y, y, WP_D50[2] * y];
        if let Some(p) = &self.bpc {
            xyz = apply_bpc_xyz_d50(xyz, p);
        }
        self.b2a.lab_to_ink(xyz_d50_to_lab(xyz))
    }
}

impl TransformExecutor<u8> for HandRolledChainStage1Gray {
    fn transform(&self, src: &[u8], dst: &mut [u8]) -> Result<(), CmsError> {
        for (&g, d) in src.iter().zip(dst.as_chunks_mut::<4>().0) {
            let ink = self.sample(f64::from(g) / 255.0);
            *d = ink.map(|v| (v * 255.0).round().clamp(0.0, 255.0) as u8);
        }
        Ok(())
    }
}

impl TransformExecutor<f64> for HandRolledChainStage1Gray {
    fn transform(&self, src: &[f64], dst: &mut [f64]) -> Result<(), CmsError> {
        for (&g, d) in src.iter().zip(dst.as_chunks_mut::<4>().0) {
            *d = self.sample(g);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // lcms2's answers for the generated test profiles; this module reads the
    // round-trip legs.
    #[allow(dead_code)]
    mod reference {
        include!("../../tests/data/cmyk_intent/reference.rs");
    }

    const INKLIMIT: &[u8] = include_bytes!("../../tests/data/cmyk_intent/inklimit.icc");
    const INKLIMIT_LUT8: &[u8] = include_bytes!("../../tests/data/cmyk_intent/inklimit_lut8.icc");
    const SPLIT: &[u8] = include_bytes!("../../tests/data/cmyk_intent/split.icc");
    const SPLIT_SAT: &[u8] = include_bytes!("../../tests/data/cmyk_intent/split_sat.icc");
    const SPLIT_LUT8: &[u8] = include_bytes!("../../tests/data/cmyk_intent/split_lut8.icc");
    const SPLIT_XYZ: &[u8] = include_bytes!("../../tests/data/cmyk_intent/split_xyz.icc");
    const RGB_GAMMA: &[u8] = include_bytes!("../../tests/data/cmyk_intent/rgb_gamma.icc");
    const RGB_LUT: &[u8] = include_bytes!("../../tests/data/cmyk_intent/rgb_lut.icc");

    /// The two legs of lcms2's black-point round trip: `B2A0` and `A2B1`.
    fn legs(icc: &[u8]) -> (OwnedLutSampler, OwnedLutSampler) {
        let profile = ColorProfile::new_from_slice(icc).unwrap();
        let b2a0 = profile.lut_b_to_a_perceptual.as_ref().unwrap();
        let a2b1 = profile.lut_a_to_b_colorimetric.as_ref().unwrap();
        (
            OwnedLutSampler::lcms_exact(b2a0, 3, 4).unwrap(),
            OwnedLutSampler::lcms_exact(a2b1, 4, 3).unwrap(),
        )
    }

    fn assert_near<const N: usize>(what: &str, got: [f64; N], want: [f64; N], tolerance: f64) {
        for (g, w) in got.iter().zip(want) {
            assert!(
                (g - w).abs() <= tolerance,
                "{what}: got {got:?}, lcms2 {want:?}"
            );
        }
    }

    // lcms2 evaluates these tables at 16 bits a stage: 1/65535 ink, and
    // 100/65280 L* on a `lut16Type` table.
    const INK_TOLERANCE: f64 = 1e-4;
    const LAB_TOLERANCE: f64 = 0.005;

    #[test]
    fn perceptual_output_table_matches_lcms() {
        for (name, icc, want) in [
            ("inklimit", INKLIMIT, &reference::INKLIMIT_B2A0),
            (
                "inklimit_lut8",
                INKLIMIT_LUT8,
                &reference::INKLIMIT_LUT8_B2A0,
            ),
        ] {
            let (b2a0, _) = legs(icc);
            for (lab, want) in reference::LAB_SAMPLES.iter().zip(want) {
                let got = b2a0.lab_to_ink(*lab);
                assert_near(
                    &format!("{name} B2A0 at {lab:?}"),
                    got,
                    *want,
                    INK_TOLERANCE,
                );
            }
        }
    }

    #[test]
    fn colorimetric_input_table_matches_lcms() {
        for (name, icc, want) in [
            ("inklimit", INKLIMIT, &reference::INKLIMIT_A2B1),
            (
                "inklimit_lut8",
                INKLIMIT_LUT8,
                &reference::INKLIMIT_LUT8_A2B1,
            ),
        ] {
            let (_, a2b1) = legs(icc);
            for (cmyk, want) in reference::SAMPLES.iter().zip(want) {
                let got = a2b1.ink_to_lab(cmyk.map(|v| f64::from(v) / 255.0));
                assert_near(
                    &format!("{name} A2B1 at {cmyk:?}"),
                    got,
                    *want,
                    LAB_TOLERANCE,
                );
            }
        }
    }

    /// Lab 0/0/0 through `B2A0` then `A2B1` lands where lcms2's does —
    /// about 310% ink, L* 19, not 400% ink's L* 8. That needs trilinear
    /// interpolation of `B2A0`: tetrahedral would give 250% ink.
    #[test]
    fn round_trip_matches_lcms() {
        for (name, icc, want) in [
            ("inklimit", INKLIMIT, reference::INKLIMIT_ROUND_TRIP),
            (
                "inklimit_lut8",
                INKLIMIT_LUT8,
                reference::INKLIMIT_LUT8_ROUND_TRIP,
            ),
        ] {
            let (b2a0, a2b1) = legs(icc);
            let got = a2b1.ink_to_lab(b2a0.lab_to_ink([0.0; 3]));
            assert_near(&format!("{name} round trip"), got, want, LAB_TOLERANCE);
        }
    }

    /// lcms2 interpolates a four-input table linearly in the first input
    /// and tetrahedrally in the other three — not quadlinearly, which the
    /// generated profiles' affine tables cannot tell apart.
    #[test]
    fn four_input_interpolation_is_lcms2s() {
        // A 2⁴ grid that is zero except at C=0, M=Y=K=1.
        let mut cube = vec![0.0f32; 16 * 3];
        let corner = 0b0111;
        cube[corner * 3..corner * 3 + 3].copy_from_slice(&[1.0, 1.0, 1.0]);
        let mut out = [0.0; 3];
        // At the C=0 plane's centre, tetrahedral weights the corner by one
        // half (the M=Y=K diagonal's ends); trilinear would by one eighth.
        eval4_lcms(&cube, 2, [0.0, 0.5, 0.5, 0.5], &mut out);
        assert_near("C=0", out, [0.5; 3], 1e-12);
        // Halfway to the C=1 plane, where the grid is zero: linear in C.
        eval4_lcms(&cube, 2, [0.5, 0.5, 0.5, 0.5], &mut out);
        assert_near("C=0.5", out, [0.25; 3], 1e-12);
        // Off the diagonal, M > Y > K: the M, MY, MYK tetrahedron.
        eval4_lcms(&cube, 2, [0.0, 0.9, 0.6, 0.3], &mut out);
        assert_near("M>Y>K", out, [0.3; 3], 1e-12);
        // The grid's far edge, where lcms2 does not step past the end.
        eval4_lcms(&cube, 2, [0.0, 1.0, 1.0, 1.0], &mut out);
        assert_near("corner", out, [1.0; 3], 1e-12);
    }

    /// `lut8Type` tables are read only by the lcms2-exact evaluators (the
    /// black point and the chain stage 1's source side): the bake and the
    /// RGB chain's output-intent side still pass them to moxcms.
    #[test]
    fn lut8_tables_are_for_the_lcms_exact_evaluators_only() {
        let profile = ColorProfile::new_from_slice(INKLIMIT_LUT8).unwrap();
        let a2b1 = profile.lut_a_to_b_colorimetric.as_ref().unwrap();
        let b2a0 = profile.lut_b_to_a_perceptual.as_ref().unwrap();
        assert!(OwnedLutSampler::from_warehouse(a2b1, 4, 3).is_none());
        assert!(OwnedLutSampler::from_warehouse(b2a0, 3, 4).is_none());
        assert!(!can_sample(&profile, a2b1));
        assert!(LabToCmykSampler::new(&profile, RenderingIntent::Perceptual).is_none());
        assert!(OwnedLutSampler::lcms_exact(a2b1, 4, 3).is_some());
        assert!(OwnedLutSampler::lcms_exact(b2a0, 3, 4).is_some());
    }

    const INTENTS: [RenderingIntent; 3] = [
        RenderingIntent::Perceptual,
        RenderingIntent::RelativeColorimetric,
        RenderingIntent::Saturation,
    ];

    /// The CMYK chain stage 1 is lcms2's, ink for ink, under each intent:
    /// the intent's table on each side, or table 0 where a profile has
    /// none (`split.icc` has no `A2B2`, `inklimit.icc` no `B2A2`), and
    /// `lut8Type` tables too.
    #[test]
    fn cmyk_chain_stage1_matches_lcms() {
        for (name, source, oi, want) in [
            ("split", SPLIT, INKLIMIT, &reference::CHAIN_SPLIT_INKLIMIT),
            (
                "split_sat",
                SPLIT_SAT,
                INKLIMIT,
                &reference::CHAIN_SPLIT_SAT_INKLIMIT,
            ),
            (
                "lut8",
                SPLIT_LUT8,
                INKLIMIT_LUT8,
                &reference::CHAIN_SPLIT_LUT8_INKLIMIT_LUT8,
            ),
        ] {
            let source = ColorProfile::new_from_slice(source).unwrap();
            let oi = ColorProfile::new_from_slice(oi).unwrap();
            for (intent, want) in INTENTS.into_iter().zip(want) {
                let stage1 = HandRolledChainStage1Cmyk::new(&source, &oi, intent).unwrap();
                for (cmyk, want) in reference::SAMPLES.iter().zip(want) {
                    let got = stage1.sample(cmyk.map(|v| f64::from(v) / 255.0));
                    assert_near(
                        &format!("{name} {intent:?} at {cmyk:?}"),
                        got,
                        *want,
                        INK_TOLERANCE,
                    );
                }
            }
        }
    }

    /// Every kind of tone curve evaluates as lcms2's
    /// `cmsEvalToneCurveFloat` does, to its `f32` precision: the five
    /// parametric types, a table, and a pure gamma — which moxcms's own
    /// evaluator gets wrong at 0.
    #[test]
    fn tone_curves_are_lcms2s() {
        let check = |what: &str, curve: ToneReprCurve, want: &[f64]| {
            let curve = LcmsCurve::new(&curve).unwrap();
            for (x, want) in reference::CURVE_X.iter().zip(want) {
                let got = curve.eval(*x);
                assert!(
                    (got - want).abs() < 1e-6,
                    "{what} at {x}: {got} vs lcms2 {want}"
                );
            }
        };
        for (params, want) in reference::CURVE_PARAMETRIC {
            check(
                &format!("parametric {params:?}"),
                ToneReprCurve::Parametric(params.to_vec()),
                &want,
            );
        }
        check(
            "table",
            ToneReprCurve::Lut(reference::CURVE_TABLE.to_vec()),
            &reference::CURVE_TABLE_EVAL,
        );
        check(
            "pure gamma",
            ToneReprCurve::Lut(vec![reference::CURVE_GAMMA]),
            &reference::CURVE_GAMMA_EVAL,
        );
        check(
            "no entries",
            ToneReprCurve::Lut(Vec::new()),
            &reference::CURVE_X,
        );
    }

    /// The RGB chain stage 1 is lcms2's: a matrix profile with pure-gamma
    /// curves, whose zero channels moxcms's evaluator wrecks, and a LUT
    /// profile, interpolated tetrahedrally, whose missing `A2B0` sends
    /// perceptual to its matrix as lcms2 does.
    #[test]
    fn rgb_chain_stage1_matches_lcms() {
        let oi = ColorProfile::new_from_slice(INKLIMIT).unwrap();
        for (name, source, want) in [
            ("rgb_gamma", RGB_GAMMA, &reference::CHAIN_RGB_GAMMA_INKLIMIT),
            ("rgb_lut", RGB_LUT, &reference::CHAIN_RGB_LUT_INKLIMIT),
        ] {
            let source = ColorProfile::new_from_slice(source).unwrap();
            for (intent, want) in INTENTS.into_iter().zip(want) {
                let stage1 = HandRolledChainStage1Rgb::new(&source, &oi, intent).unwrap();
                for (rgb, want) in reference::RGB_SAMPLES.iter().zip(want) {
                    let [r, g, b] = rgb.map(|v| f64::from(v) / 255.0);
                    assert_near(
                        &format!("{name} {intent:?} at {rgb:?}"),
                        stage1.sample_cmyk_f64(r, g, b),
                        *want,
                        INK_TOLERANCE,
                    );
                }
            }
        }
    }

    /// A source with an XYZ PCS is left to moxcms.
    #[test]
    fn cmyk_chain_stage1_needs_lab_tables() {
        let source = ColorProfile::new_from_slice(SPLIT_XYZ).unwrap();
        let oi = ColorProfile::new_from_slice(INKLIMIT).unwrap();
        for intent in INTENTS {
            assert!(HandRolledChainStage1Cmyk::new(&source, &oi, intent).is_none());
            assert!(HandRolledChainStage1Cmyk::new(&oi, &source, intent).is_none());
        }
    }
}
