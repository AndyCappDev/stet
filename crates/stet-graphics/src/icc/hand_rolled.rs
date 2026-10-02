// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Hand-rolled samplers for ICC LUT-based tables.
//!
//! moxcms's `create_transform` routes CMYK profiles through an internal
//! Lab→sRGB pipeline that over-saturates light/midtone colours noticeably
//! relative to lcms2 / Acrobat / Ghostscript. This module bypasses moxcms
//! for CMYK profiles: each grid point goes through one of the profile's
//! A2B CLUTs (chosen per rendering intent), the PCS decode, and a
//! hand-tuned XYZ-D50 → sRGB pipeline. A `lut16Type` table with a Lab PCS
//! has its own sampler ([`bake_clut4_hand_rolled`]); ICC v4 `lutAToBType`
//! and `lut8Type` tables, and any with an XYZ PCS, go through the evaluator
//! that reproduces lcms2 ([`bake_clut4_lcms`]). Through `A2B1` the output
//! matches lcms2's `cmsDoTransform(RelCol)` to ±1 RGB level on a 17⁴ sweep
//! against ISO Coated v2 300% (ECI), which is what GS produces for typical
//! print imagery (light greens, neutrals, blacks). Out-of-gamut colours
//! clip to the sRGB boundary (also matching lcms2 / GS).
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
//! The reverse transform, sRGB → the default CMYK profile, runs the display
//! backwards ([`display_srgb_to_lab`]) into a `LabToCmykSampler`, so it is
//! lcms2's built-in sRGB → profile.
//!
//! The evaluators that reproduce lcms2 exactly (`LcmsLut`) read a Lab or an
//! XYZ PCS, applying a three-input `lut8Type`/`lut16Type` table's matrix as
//! lcms2 does. They also read ICC v4 `lutAToBType`/`lutBToAType` tables,
//! taking their curves from the profile's own bytes: moxcms 0.8.1 misreads
//! a curve set holding an empty `curv`, and builds no working transform
//! from a four-input CLUT whose grid differs per input. `lut8Type` (mft1) tables are read only by those
//! evaluators (see [`OwnedLutSampler::lcms_exact`]), wherever stet reads a
//! table itself: the display bake, the black points lcms2 would detect, and
//! both sides of the proofing chain's stage 1.
//!
//! Tone curves — an RGB source's and a Gray source's — evaluate as lcms2
//! does (`LcmsCurve`), not through moxcms's evaluator, whose pure gamma is
//! garbage at 0.

use moxcms::{
    CmsError, ColorProfile, Cube, DataColorSpace, Hypercube, Lab, LutMultidimensionalType,
    LutStore, LutType, LutWarehouse, Matrix3d, RenderingIntent, ToneReprCurve, TransformExecutor,
};

use super::Clut4;
use super::black_point::SourceBlack;
use super::bpc::{
    BpcParams, LCMS_D50, WP_D50, apply_bpc_xyz_d50, compute_bpc_params, lab_to_xyz_d50,
    lab_to_xyz_white, matmul3, srgb_to_linear, xyz_d50_to_lab, xyz_to_lab_white,
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
/// Returns `None` when the table is not `lut16Type`, or the profile is not
/// CMYK with a Lab PCS. The caller then tries [`bake_clut4_lcms`], and
/// after it the moxcms-driven [`super::bake_clut4`].
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
    let denom = (grid_n - 1) as f32;
    bake_grid(grid_n, bpc.as_ref(), |[c_i, m_i, y_i, k_i]| {
        let [c, m, y, k] = [c_i, m_i, y_i, k_i].map(|i| i as f32 / denom);
        lab_to_xyz_d50_abs(lut.sample_cmyk_to_lab(c, m, y, k))
    })
}

/// Sample one of a CMYK profile's A2B tables into a [`Clut4`] through the
/// evaluator that reproduces lcms2 (`LcmsLut`): an ICC v4 `lutAToBType`
/// table, a `lut8Type` one, or any with an XYZ PCS. `table` is the tag the
/// caller picked for the rendering intent, with its signature; `icc` is the
/// profile's raw bytes, which hold the v4 curve sets. Otherwise as
/// [`bake_clut4_hand_rolled`], darker colorant included: it is this table's
/// own 400% ink.
///
/// `None` when the profile is not CMYK or the table is one the evaluator
/// cannot read; see [`can_bake_lcms`].
pub(super) fn bake_clut4_lcms(
    profile: &ColorProfile,
    icc: &[u8],
    table: Table,
    grid_n: usize,
    black: Option<SourceBlack>,
) -> Option<Clut4> {
    if profile.color_space != DataColorSpace::Cmyk {
        return None;
    }
    if !(2..=33).contains(&grid_n) {
        return None;
    }
    let lut = LcmsLut::to_pcs(table, icc, 4)?;
    let bpc = black.map(|black| {
        let sbp = match black {
            SourceBlack::DarkerColorant => {
                let l_star = lut.ink_to_lab([1.0; 4])[0].clamp(0.0, 100.0);
                lab_to_xyz_d50([l_star.min(50.0), 0.0, 0.0])
            }
            SourceBlack::Xyz(xyz) => xyz,
        };
        compute_bpc_params(sbp, [0.0; 3], WP_D50)
    });
    let denom = (grid_n - 1) as f64;
    bake_grid(grid_n, bpc.as_ref(), |ink| {
        lut.ink_to_xyz(ink.map(|i| i as f64 / denom))
    })
}

/// Whether [`bake_clut4_lcms`] can read `table` of `profile`, whose raw
/// bytes are `icc`.
pub(super) fn can_bake_lcms(profile: &ColorProfile, icc: &[u8], table: Table) -> bool {
    profile.color_space == DataColorSpace::Cmyk && LcmsLut::to_pcs(table, icc, 4).is_some()
}

/// A [`Clut4`] of `grid_n` points per axis, each the sRGB of the XYZ-D50
/// (`Y_white = 1.0`) that `xyz_at` gives for its grid indices `[c, m, y,
/// k]`, compensated by `bpc`.
fn bake_grid(
    grid_n: usize,
    bpc: Option<&BpcParams>,
    xyz_at: impl Fn([usize; 4]) -> [f64; 3],
) -> Option<Clut4> {
    let total = grid_n
        .checked_mul(grid_n)?
        .checked_mul(grid_n)?
        .checked_mul(grid_n)?;
    let mut data = vec![0u8; total * 3];
    for k_i in 0..grid_n {
        for y_i in 0..grid_n {
            for m_i in 0..grid_n {
                for c_i in 0..grid_n {
                    // Black-point compensation scales XYZ before the sRGB
                    // gamut clip, as lcms2 does. Scaling the clipped sRGB
                    // instead moves out-of-gamut colours: one channel is
                    // already pinned, so the others shift (6 levels on a
                    // saturated cyan).
                    let mut xyz = xyz_at([c_i, m_i, y_i, k_i]);
                    if let Some(p) = bpc {
                        xyz = apply_bpc_xyz_d50(xyz, p);
                    }
                    let rgb = encode_linear_srgb(xyz_d50_to_linear_srgb_d65(xyz));
                    let off = (((k_i * grid_n + y_i) * grid_n + m_i) * grid_n + c_i) * 3;
                    data[off..off + 3].copy_from_slice(&rgb);
                }
            }
        }
    }
    Some(Clut4::from_baked(grid_n as u8, data))
}

/// lcms2's black-point round trip for a CMYK output profile: Lab `start`
/// through the perceptual `B2A0`, then back through the colorimetric
/// `A2B1` (`A2B0` when there is none, as lcms2 reads it). Returns the ink
/// it passed through and the Lab it landed on; `None` when the profile,
/// whose raw bytes are `icc`, is not CMYK, or a table is one these
/// evaluators cannot read.
pub(super) fn perceptual_round_trip(
    profile: &ColorProfile,
    icc: &[u8],
    start: [f64; 3],
) -> Option<([f64; 4], [f64; 3])> {
    Some(RoundTrip::new(profile, icc, RenderingIntent::Perceptual)?.run(start))
}

/// lcms2's round trip through a CMYK output profile
/// (`CreateRoundtripXForm`): Lab through the profile's B2A table for an
/// intent, then back through its colorimetric A2B table, each as lcms2
/// reads it (see [`lcms_table`]).
pub(super) struct RoundTrip {
    b2a: LcmsLut,
    a2b: LcmsLut,
}

impl RoundTrip {
    /// `None` when the profile, whose raw bytes are `icc`, is not CMYK,
    /// lacks a table the round trip needs, or carries one these evaluators
    /// cannot read.
    pub(super) fn new(profile: &ColorProfile, icc: &[u8], intent: RenderingIntent) -> Option<Self> {
        if profile.color_space != DataColorSpace::Cmyk {
            return None;
        }
        let b2a = b2a_table(profile, intent)?;
        let a2b = a2b_table(profile, RenderingIntent::RelativeColorimetric)?;
        Some(Self {
            b2a: LcmsLut::from_pcs(b2a, icc, 4)?,
            a2b: LcmsLut::to_pcs(a2b, icc, 4)?,
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
    /// The `curv` or `para` curve at the start of `bytes`, and its length;
    /// `None` when it is neither or is cut short.
    fn read(bytes: &[u8]) -> Option<(Self, usize)> {
        let be16 = |at: usize| -> Option<u16> {
            Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
        };
        match bytes.get(..4)? {
            b"curv" => {
                let n = u32::from_be_bytes(bytes.get(8..12)?.try_into().ok()?) as usize;
                let len = n.checked_mul(2)?.checked_add(12)?;
                let table: Vec<u16> = (0..n).map(|i| be16(12 + 2 * i)).collect::<Option<_>>()?;
                let curve = match table.as_slice() {
                    [] => LcmsCurve::Gamma(1.0),
                    [gamma] => LcmsCurve::Gamma(f64::from(*gamma) / 256.0),
                    _ => LcmsCurve::Table(table),
                };
                Some((curve, len))
            }
            b"para" => {
                let function = be16(8)?;
                let n = [1, 3, 4, 5, 7].get(usize::from(function)).copied()?;
                let mut p = [0.0; 7];
                for (i, v) in p.iter_mut().take(n).enumerate() {
                    let raw =
                        i32::from_be_bytes(bytes.get(12 + 4 * i..16 + 4 * i)?.try_into().ok()?);
                    *v = f64::from(raw) / 65536.0;
                }
                Some((LcmsCurve::Parametric(function as u8 + 1, p), 12 + 4 * n))
            }
            _ => None,
        }
    }

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

/// Lab of 400% ink through the CMYK profile's A2B table for `intent` —
/// that intent's own, with no fallback — as lcms2 evaluates it; `icc` is
/// the profile's raw bytes.
pub(super) fn table_black(
    profile: &ColorProfile,
    icc: &[u8],
    intent: RenderingIntent,
) -> Option<[f64; 3]> {
    let (table, sig) = match intent {
        RenderingIntent::Perceptual => (profile.lut_a_to_b_perceptual.as_ref()?, b"A2B0"),
        RenderingIntent::Saturation => (profile.lut_a_to_b_saturation.as_ref()?, b"A2B2"),
        _ => (profile.lut_a_to_b_colorimetric.as_ref()?, b"A2B1"),
    };
    Some(LcmsLut::to_pcs((table, sig), icc, 4)?.ink_to_lab([1.0; 4]))
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

/// The combined Bradford-CAT × sRGB inverse matrix, XYZ-D50 → linear
/// sRGB-D65. Coefficients lifted from the ICC reference: identical to what
/// lcms2 produces under default settings.
const XYZ_D50_TO_LINEAR_SRGB_D65: [[f64; 3]; 3] = [
    [3.133_856_1, -1.616_866_7, -0.490_614_6],
    [-0.978_768_4, 1.916_141_5, 0.033_454_0],
    [0.071_945_3, -0.228_991_4, 1.405_242_7],
];

/// Linear sRGB-D65 → XYZ-D50: the inverse of
/// [`XYZ_D50_TO_LINEAR_SRGB_D65`] itself, not the textbook sRGB matrix, so
/// that [`display_srgb_to_lab`] undoes the display exactly.
const LINEAR_SRGB_D65_TO_XYZ_D50: [[f64; 3]; 3] = invert3(XYZ_D50_TO_LINEAR_SRGB_D65);

/// The inverse of a 3×3 matrix, by cofactors.
const fn invert3(a: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let c00 = a[1][1] * a[2][2] - a[1][2] * a[2][1];
    let c01 = a[1][2] * a[2][0] - a[1][0] * a[2][2];
    let c02 = a[1][0] * a[2][1] - a[1][1] * a[2][0];
    let det = a[0][0] * c00 + a[0][1] * c01 + a[0][2] * c02;
    [
        [
            c00 / det,
            (a[0][2] * a[2][1] - a[0][1] * a[2][2]) / det,
            (a[0][1] * a[1][2] - a[0][2] * a[1][1]) / det,
        ],
        [
            c01 / det,
            (a[0][0] * a[2][2] - a[0][2] * a[2][0]) / det,
            (a[0][2] * a[1][0] - a[0][0] * a[1][2]) / det,
        ],
        [
            c02 / det,
            (a[0][1] * a[2][0] - a[0][0] * a[2][1]) / det,
            (a[0][0] * a[1][1] - a[0][1] * a[1][0]) / det,
        ],
    ]
}

/// XYZ-D50 → linear sRGB-D65 through [`XYZ_D50_TO_LINEAR_SRGB_D65`].
#[inline]
fn xyz_d50_to_linear_srgb_d65(xyz: [f64; 3]) -> [f64; 3] {
    matmul3(&XYZ_D50_TO_LINEAR_SRGB_D65, xyz)
}

/// The Lab that stet displays as the sRGB colour `rgb` (each `[0, 1]`):
/// the sRGB transfer function decoded, then the display matrix inverted.
/// The display matrix is lcms2's built-in sRGB, so this is also lcms2's
/// sRGB → Lab. The reverse transform (`IccCache::convert_rgb_to_cmyk_readonly`)
/// takes it into a CMYK profile's `B2A1`, so that a colour stet displays
/// converts to the ink that displays as it.
pub(super) fn display_srgb_to_lab(rgb: [f64; 3]) -> [f64; 3] {
    let linear = rgb.map(srgb_to_linear);
    xyz_d50_to_lab(matmul3(&LINEAR_SRGB_D65_TO_XYZ_D50, linear))
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
    Lut(LcmsLut),
    Shaper(ShaperMatrix),
}

impl SourceA2BSampler {
    fn new(profile: &ColorProfile, icc: &[u8], intent: RenderingIntent) -> Option<Self> {
        if profile.color_space != DataColorSpace::Rgb {
            return None;
        }
        // lcms2 reads the intent's A2B table, else A2B0, and only when the
        // profile has neither its tone curves and colorant matrix, which
        // give the same XYZ under every intent. A table these evaluators
        // cannot read has no hand-rolled stage: the caller falls back to
        // moxcms rather than to a matrix lcms2 would not use.
        match a2b_table(profile, intent) {
            Some(table) => LcmsLut::to_pcs(table, icc, 3).map(SourceA2BSampler::Lut),
            None => ShaperMatrix::new(profile).map(SourceA2BSampler::Shaper),
        }
    }

    /// Returns the source profile's A2B output as `[L, a, b]` in mft2
    /// PCS-Lab encoding (each component in `[0, 1]`, `0xFF00`-denominated).
    /// LUT-based profiles return their post-output-curve values, re-encoded
    /// from a `lut8Type` table's full scale; shaper-matrix profiles compute
    /// Lab via TRC + colorant matrix and re-encode to mft2.
    /// The source's XYZ-D50 (white Y = 1) for RGB in `[0, 1]`, in `f64`:
    /// what black-point compensation works on.
    fn sample_xyz(&self, rgb: [f64; 3]) -> [f64; 3] {
        match self {
            SourceA2BSampler::Lut(lut) => lab_to_xyz_d50(lut.rgb_to_lab(rgb)),
            SourceA2BSampler::Shaper(sm) => sm.sample_xyz(rgb),
        }
    }

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
/// and takes the resulting XYZ-D50 to Lab relative to lcms2's own D50
/// ([`LCMS_D50`]), the frame the colorants are in.
struct ShaperMatrix {
    trc_r: LcmsCurve,
    trc_g: LcmsCurve,
    trc_b: LcmsCurve,
    /// 3×3 colorant matrix (linear-RGB → absolute XYZ-D50), row-major.
    matrix: [[f64; 3]; 3],
}

impl ShaperMatrix {
    /// `None` unless the profile has all three colorants and tone curves,
    /// as lcms2 requires of a matrix-shaper.
    fn new(profile: &ColorProfile) -> Option<Self> {
        if !profile.is_matrix_shaper() {
            return None;
        }
        let red_trc = profile.red_trc.as_ref()?;
        let green_trc = profile.green_trc.as_ref()?;
        let blue_trc = profile.blue_trc.as_ref()?;

        let trc_r = LcmsCurve::new(red_trc)?;
        let trc_g = LcmsCurve::new(green_trc)?;
        let trc_b = LcmsCurve::new(blue_trc)?;

        Some(ShaperMatrix {
            trc_r,
            trc_g,
            trc_b,
            matrix: profile.colorant_matrix().v,
        })
    }

    /// Absolute XYZ-D50 (white Y = 1) for RGB in `[0, 1]`, in `f64`, in
    /// stet's frame (see [`PcsCoding`]): the colorants are in lcms2's.
    fn sample_xyz(&self, rgb: [f64; 3]) -> [f64; 3] {
        let lin = [
            self.trc_r.eval(rgb[0]),
            self.trc_g.eval(rgb[1]),
            self.trc_b.eval(rgb[2]),
        ];
        let m = &self.matrix;
        std::array::from_fn(|i| {
            let xyz = m[i][0] * lin[0] + m[i][1] * lin[1] + m[i][2] * lin[2];
            xyz * WP_D50[i] / LCMS_D50[i]
        })
    }

    fn sample_pcs_lab(&self, r: f32, g: f32, b: f32) -> [f32; 3] {
        let lin_r = self.trc_r.eval(f64::from(r));
        let lin_g = self.trc_g.eval(f64::from(g));
        let lin_b = self.trc_b.eval(f64::from(b));

        let x = self.matrix[0][0] * lin_r + self.matrix[0][1] * lin_g + self.matrix[0][2] * lin_b;
        let y = self.matrix[1][0] * lin_r + self.matrix[1][1] * lin_g + self.matrix[1][2] * lin_b;
        let z = self.matrix[2][0] * lin_r + self.matrix[2][1] * lin_g + self.matrix[2][2] * lin_b;

        // The colorants' XYZ is in lcms2's frame, so Lab is relative to its
        // D50, encoded as mft2 PCS-Lab (denom 65280) so the value lines up
        // with the OI B2A input curves' grid axis.
        let lab = xyz_to_lab_white([x, y, z], LCMS_D50);
        LabEncoding::V2
            .encode(lab)
            .map(|v| v.clamp(0.0, 1.0) as f32)
    }
}

/// Lab of `rgb` (each `[0, 1]`) through the RGB profile `profile`, whose
/// raw bytes are `icc`, as the chain stage 1 reads it for `intent`; `None`
/// when it cannot.
pub(super) fn rgb_to_lab(
    profile: &ColorProfile,
    icc: &[u8],
    intent: RenderingIntent,
    rgb: [f64; 3],
) -> Option<[f64; 3]> {
    let source = SourceA2BSampler::new(profile, icc, intent)?;
    Some(xyz_d50_to_lab(source.sample_xyz(rgb)))
}

/// OutputIntent CMYK profile B2A sampler (Lab → CMYK), owned variant, with
/// the black-point compensation lcms2 applies to a Lab source.
pub struct LabToCmykSampler {
    lut: LcmsLut,
    bpc: Option<BpcParams>,
}

impl LabToCmykSampler {
    /// The output intent's B2A table for `intent` as lcms2 reads it (the
    /// intent's, else `B2A0`; absolute colorimetric reads `B2A1`), with
    /// `bpc` applied to the Lab that [`Self::sample_pdf_lab`] converts.
    /// `None` when the profile, whose raw bytes are `icc`, is not CMYK, or
    /// the table is missing or one these evaluators cannot read.
    pub(super) fn new(
        profile: &ColorProfile,
        icc: &[u8],
        intent: RenderingIntent,
        bpc: Option<BpcParams>,
    ) -> Option<Self> {
        if profile.color_space != DataColorSpace::Cmyk {
            return None;
        }
        let lut = LcmsLut::from_pcs(b2a_table(profile, intent)?, icc, 4)?;
        Some(LabToCmykSampler { lut, bpc })
    }

    /// Legacy v2-encoded PCS Lab (each `[0, 1]`) → ink. A `lut16Type`
    /// table takes it as it is; any other goes through Lab.
    fn sample_pcs_lab(&self, pcs_lab: [f32; 3]) -> [f32; 4] {
        match &self.lut {
            LcmsLut::Legacy(lut) if lut.coding == PcsCoding::Lab(LabEncoding::V2) => {
                lut.sample_pcs_lab_to_cmyk(pcs_lab)
            }
            lut => {
                let lab = LabEncoding::V2.decode(pcs_lab.map(f64::from));
                lut.lab_to_ink(lab).map(|v| v as f32)
            }
        }
    }

    /// Convert a PDF Lab triplet (L\* ∈ [0, 100], a\*/b\* ∈ [-128, 127]) to
    /// OutputIntent CMYK by encoding to the mft2 PCS-Lab grid the OI's B2A
    /// curves expect, then sampling the LUT. Used by `IccCache` to populate
    /// `DeviceColor::native_cmyk` for Lab fills, so the parallel CMYK buffer
    /// holds the same direct Lab→OI value Acrobat's ACE produces (rather
    /// than stet's Lab→sRGB→ICC-reverse approximation, which drifts under
    /// CMYK-group blends — GWG 22.1's ColorBurn form).
    ///
    /// With black-point compensation the Lab goes to XYZ and back around
    /// it, as lcms2 compensates: a Lab source's black is zero.
    pub fn sample_pdf_lab(&self, l_star: f64, a_star: f64, b_star: f64) -> [f64; 4] {
        if let Some(p) = &self.bpc {
            let lab = [
                l_star.clamp(0.0, 100.0),
                a_star.clamp(-128.0, 127.0),
                b_star.clamp(-128.0, 127.0),
            ];
            let xyz = apply_bpc_xyz_d50(lab_to_xyz_d50(lab), p);
            return self.lut.xyz_to_ink(xyz);
        }
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

/// The PCS of a profile, from its header.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Pcs {
    Lab,
    Xyz,
}

impl Pcs {
    /// The PCS of the profile whose raw bytes are `icc`; `None` when it is
    /// neither.
    fn of(icc: &[u8]) -> Option<Self> {
        match icc.get(20..24)? {
            b"Lab " => Some(Pcs::Lab),
            b"XYZ " => Some(Pcs::Xyz),
            _ => None,
        }
    }
}

/// lcms2's `MAX_ENCODEABLE_XYZ`: an XYZ PCS value of 1 in a table is this
/// much XYZ (white Y = 1), whatever the table type.
const XYZ_MAX: f64 = 1.0 + 32767.0 / 32768.0;

/// How a table encodes its PCS side, each value in `[0, 1]`, as lcms2 reads
/// it.
///
/// XYZ comes out in stet's frame, white [`WP_D50`], which its Lab, its
/// black points and its sRGB matrix all use. lcms2's frame has white
/// [`LCMS_D50`], and Lab is the same in both, so moving between them scales
/// each channel by the ratio of the whites.
#[derive(Clone, Copy, Debug, PartialEq)]
enum PcsCoding {
    Lab(LabEncoding),
    /// XYZ / [`XYZ_MAX`].
    Xyz,
}

impl PcsCoding {
    /// The coding of a table of type `lut_type`, or of an ICC v4 one when
    /// `None`, in a profile whose PCS is `pcs`.
    fn new(pcs: Pcs, lut_type: Option<LutType>) -> Self {
        match (pcs, lut_type) {
            (Pcs::Xyz, _) => PcsCoding::Xyz,
            (Pcs::Lab, Some(LutType::Lut16)) => PcsCoding::Lab(LabEncoding::V2),
            (Pcs::Lab, _) => PcsCoding::Lab(LabEncoding::V4),
        }
    }

    /// Lab (L\* 0–100, a\*/b\* −128–127) → the table's `[0, 1]` axes.
    /// Between Lab and XYZ the white is lcms2's own D50.
    fn encode_lab(self, lab: [f64; 3]) -> [f64; 3] {
        match self {
            PcsCoding::Lab(e) => e.encode(lab),
            PcsCoding::Xyz => lab_to_xyz_white(lab, LCMS_D50).map(|v| v / XYZ_MAX),
        }
    }

    /// The table's `[0, 1]` outputs → Lab.
    fn decode_lab(self, v: [f64; 3]) -> [f64; 3] {
        match self {
            PcsCoding::Lab(e) => e.decode(v),
            PcsCoding::Xyz => xyz_to_lab_white(v.map(|x| x * XYZ_MAX), LCMS_D50),
        }
    }

    /// The table's `[0, 1]` outputs → XYZ (D50, white Y = 1).
    fn decode_xyz(self, v: [f64; 3]) -> [f64; 3] {
        match self {
            PcsCoding::Lab(e) => lab_to_xyz_d50(e.decode(v)),
            PcsCoding::Xyz => std::array::from_fn(|i| v[i] * XYZ_MAX * WP_D50[i] / LCMS_D50[i]),
        }
    }

    /// XYZ (D50, white Y = 1) → the table's `[0, 1]` axes.
    fn encode_xyz(self, xyz: [f64; 3]) -> [f64; 3] {
        match self {
            PcsCoding::Lab(e) => e.encode(xyz_d50_to_lab(xyz)),
            PcsCoding::Xyz => std::array::from_fn(|i| xyz[i] * LCMS_D50[i] / WP_D50[i] / XYZ_MAX),
        }
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
    coding: PcsCoding,
    /// A three-input table's matrix, applied before its input curves as
    /// lcms2 does; `None` for the identity.
    matrix: Option<[[f64; 3]; 3]>,
}

impl OwnedLutSampler {
    /// A `lut16Type` table of shape `(expected_n_in, expected_n_out)` with
    /// a Lab PCS.
    fn from_warehouse(
        warehouse: &LutWarehouse,
        expected_n_in: usize,
        expected_n_out: usize,
    ) -> Option<Self> {
        Self::load(warehouse, expected_n_in, expected_n_out, false, Pcs::Lab)
    }

    /// A `lut8Type` or `lut16Type` table of a profile whose PCS is `pcs`,
    /// for evaluating it exactly as lcms2 does — see [`Self::lab_to_ink`],
    /// [`Self::ink_to_raw`] and [`Self::rgb_to_pcs_lab`]: the display bake,
    /// the black point lcms2 detects, and the CMYK, Gray and RGB-source
    /// chain stage 1. A three-input table's matrix is applied first, as
    /// lcms2 applies it (`Type_LUT16_Read`), whatever the PCS; the ICC
    /// expects the identity unless the input is XYZ.
    fn lcms_exact(
        warehouse: &LutWarehouse,
        expected_n_in: usize,
        expected_n_out: usize,
        pcs: Pcs,
    ) -> Option<Self> {
        let mut lut = Self::load(warehouse, expected_n_in, expected_n_out, true, pcs)?;
        if let LutWarehouse::Lut(table) = warehouse
            && expected_n_in == 3
            && table.matrix != Matrix3d::IDENTITY
        {
            lut.matrix = Some(table.matrix.v);
        }
        Some(lut)
    }

    fn load(
        warehouse: &LutWarehouse,
        expected_n_in: usize,
        expected_n_out: usize,
        allow_lut8: bool,
        pcs: Pcs,
    ) -> Option<Self> {
        let lut = match warehouse {
            LutWarehouse::Lut(l) => l,
            // v4 tables are `MultiLut`'s.
            LutWarehouse::Multidimensional(_) => return None,
        };
        if lut.lut_type == LutType::Lut8 && !allow_lut8 {
            return None;
        }
        let coding = PcsCoding::new(pcs, Some(lut.lut_type));
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
            coding,
            matrix: None,
        })
    }

    /// A three-input table's input, through its matrix when it has one.
    fn matrixed(&self, v: [f64; 3]) -> [f64; 3] {
        match &self.matrix {
            Some(m) => std::array::from_fn(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2]),
            None => v,
        }
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
        let scale = match self.coding {
            PcsCoding::Lab(LabEncoding::V2) => 65535.0 / PCS_LAB_DENOM,
            PcsCoding::Lab(LabEncoding::V4) => 1.0,
            PcsCoding::Xyz => unreachable!("the display sampler reads only a Lab PCS"),
        };
        Lab::new(
            (l_post * scale).clamp(0.0, 1.0),
            (a_post * scale).clamp(0.0, 1.0),
            (b_post * scale).clamp(0.0, 1.0),
        )
    }

    /// 4-in / 3-out: CMYK ink (each `[0, 1]`) → the table's `[0, 1]` PCS
    /// outputs, interpolated as lcms2 interpolates a four-input table.
    fn ink_to_raw(&self, ink: [f64; 4]) -> [f64; 3] {
        let curved: [f64; 4] = std::array::from_fn(|ch| {
            sample_curve_f32(&self.input_table, ch, self.n_in_entries, ink[ch] as f32) as f64
        });
        let mut pcs = [0.0; 3];
        eval4_lcms(&self.cube_data, self.cube_grid, curved, &mut pcs);
        let post: [f64; 3] = std::array::from_fn(|ch| {
            sample_curve_f32(&self.output_table, ch, self.n_out_entries, pcs[ch] as f32) as f64
        });
        post
    }

    /// 3-in / 4-out: Lab → CMYK ink (each `[0, 1]`): trilinear as lcms2
    /// reads a Lab-indexed output table, tetrahedral for an XYZ-indexed one.
    fn lab_to_ink(&self, lab: [f64; 3]) -> [f64; 4] {
        let pcs = self.coding.encode_lab(lab);
        if self.coding == PcsCoding::Xyz {
            return self.sample_pcs_xyz_to_cmyk(pcs);
        }
        self.sample_pcs_lab_to_cmyk(pcs.map(|v| v as f32))
            .map(f64::from)
    }

    /// 3-in / 4-out: an XYZ-indexed table's `[0, 1]` inputs → CMYK in
    /// `[0, 1]`: through its matrix, its input curves, lcms2's tetrahedral
    /// interpolation and its output curves.
    fn sample_pcs_xyz_to_cmyk(&self, pcs: [f64; 3]) -> [f64; 4] {
        let v = self.matrixed(pcs);
        let curved: [f64; 3] = std::array::from_fn(|ch| {
            let x = v[ch].clamp(0.0, 1.0) as f32;
            sample_curve_f32(&self.input_table, ch, self.n_in_entries, x) as f64
        });
        let mut grid = [0.0; 4];
        eval3_grids(
            &self.cube_data,
            [self.cube_grid; 3],
            curved,
            false,
            &mut grid,
        );
        std::array::from_fn(|ch| {
            let y = sample_curve_f32(&self.output_table, ch, self.n_out_entries, grid[ch] as f32);
            y.clamp(0.0, 1.0) as f64
        })
    }

    /// 3-in / 3-out: an RGB source's ink → its PCS Lab in the legacy v2
    /// `lut16Type` encoding (each in `[0, 1]`, `0xFF00`-denominated), the
    /// form an OutputIntent's B2A input curves take: through the input
    /// curves, lcms2's tetrahedral interpolation and the output curves.
    fn rgb_to_pcs_lab(&self, rgb: [f64; 3]) -> [f64; 3] {
        let rgb = self.matrixed(rgb);
        let curved: [f64; 3] = std::array::from_fn(|ch| {
            sample_curve_f32(&self.input_table, ch, self.n_in_entries, rgb[ch] as f32) as f64
        });
        let mut pcs = [0.0; 3];
        eval3_lcms(&self.cube_data, self.cube_grid, curved, &mut pcs);
        let post: [f64; 3] = std::array::from_fn(|ch| {
            sample_curve_f32(&self.output_table, ch, self.n_out_entries, pcs[ch] as f32) as f64
        });
        let v2 = match self.coding {
            PcsCoding::Lab(LabEncoding::V2) => post,
            PcsCoding::Lab(LabEncoding::V4) => post.map(|v| v * f64::from(PCS_LAB_DENOM) / 65535.0),
            PcsCoding::Xyz => LabEncoding::V2.encode(self.coding.decode_lab(post)),
        };
        v2.map(|v| v.clamp(0.0, 1.0))
    }

    /// 3-in / 4-out: mft2 PCS-Lab → CMYK in `[0, 1]`. Input is the raw
    /// 65280-denominated form so this composes byte-for-byte with the
    /// `lut16Type` upstream of it.
    fn sample_pcs_lab_to_cmyk(&self, pcs_lab: [f32; 3]) -> [f32; 4] {
        let pcs_lab = match self.matrix {
            Some(_) => self.matrixed(pcs_lab.map(f64::from)).map(|v| v as f32),
            None => pcs_lab,
        };
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

/// A profile's A2B or B2A table as lcms2 evaluates it: a `lut8Type` or
/// `lut16Type` one, or an ICC v4 `lutAToBType`/`lutBToAType` one, with a Lab
/// or an XYZ PCS. Lab on the PCS side is L\* 0–100, a\*/b\* −128–127; XYZ
/// is D50 with white Y = 1. lcms2 moves between the two in floating point,
/// so either is exact whatever the table holds.
enum LcmsLut {
    Legacy(OwnedLutSampler),
    Multi(MultiLut),
}

impl LcmsLut {
    /// A device-to-PCS (A2B) table of `n_in` inputs and three outputs, of
    /// the profile whose raw bytes are `icc`.
    fn to_pcs((table, sig): Table, icc: &[u8], n_in: usize) -> Option<Self> {
        let pcs = Pcs::of(icc)?;
        match table {
            LutWarehouse::Lut(_) => {
                OwnedLutSampler::lcms_exact(table, n_in, 3, pcs).map(Self::Legacy)
            }
            LutWarehouse::Multidimensional(t) => {
                MultiLut::new(t, CurveSets::read(icc, sig)?, n_in, 3, true, pcs).map(Self::Multi)
            }
        }
    }

    /// A PCS-to-device (B2A) table of three inputs and `n_out` outputs, of
    /// the profile whose raw bytes are `icc`.
    fn from_pcs((table, sig): Table, icc: &[u8], n_out: usize) -> Option<Self> {
        let pcs = Pcs::of(icc)?;
        match table {
            LutWarehouse::Lut(_) => {
                OwnedLutSampler::lcms_exact(table, 3, n_out, pcs).map(Self::Legacy)
            }
            LutWarehouse::Multidimensional(t) => {
                MultiLut::new(t, CurveSets::read(icc, sig)?, 3, n_out, false, pcs).map(Self::Multi)
            }
        }
    }

    /// How the table encodes its PCS side.
    fn coding(&self) -> PcsCoding {
        match self {
            LcmsLut::Legacy(l) => l.coding,
            LcmsLut::Multi(m) => m.coding,
        }
    }

    /// CMYK ink (each `[0, 1]`) → the table's `[0, 1]` PCS outputs.
    fn ink_to_raw(&self, ink: [f64; 4]) -> [f64; 3] {
        match self {
            LcmsLut::Legacy(l) => l.ink_to_raw(ink),
            LcmsLut::Multi(m) => {
                let mut out = [0.0; 4];
                m.eval(&ink, &mut out);
                [out[0], out[1], out[2]]
            }
        }
    }

    /// CMYK ink (each `[0, 1]`) → Lab.
    fn ink_to_lab(&self, ink: [f64; 4]) -> [f64; 3] {
        self.coding().decode_lab(self.ink_to_raw(ink))
    }

    /// CMYK ink (each `[0, 1]`) → XYZ (D50, white Y = 1).
    fn ink_to_xyz(&self, ink: [f64; 4]) -> [f64; 3] {
        self.coding().decode_xyz(self.ink_to_raw(ink))
    }

    /// XYZ (D50, white Y = 1) → CMYK ink (each `[0, 1]`): straight into an
    /// XYZ-indexed table, as lcms2 joins two XYZ stages, and through Lab
    /// into a Lab-indexed one.
    fn xyz_to_ink(&self, xyz: [f64; 3]) -> [f64; 4] {
        let coding = self.coding();
        if coding != PcsCoding::Xyz {
            return self.lab_to_ink(xyz_d50_to_lab(xyz));
        }
        let pcs = coding.encode_xyz(xyz);
        match self {
            LcmsLut::Legacy(l) => l.sample_pcs_xyz_to_cmyk(pcs),
            LcmsLut::Multi(m) => {
                let mut out = [0.0; 4];
                m.eval(&pcs, &mut out);
                out.map(|v| v.clamp(0.0, 1.0))
            }
        }
    }

    /// Lab → CMYK ink (each `[0, 1]`).
    fn lab_to_ink(&self, lab: [f64; 3]) -> [f64; 4] {
        match self {
            LcmsLut::Legacy(l) => l.lab_to_ink(lab),
            LcmsLut::Multi(m) => {
                let mut out = [0.0; 4];
                m.eval(&m.coding.encode_lab(lab), &mut out);
                out.map(|v| v.clamp(0.0, 1.0))
            }
        }
    }

    /// RGB (each `[0, 1]`) → its PCS Lab in the legacy v2 `lut16Type`
    /// encoding, each in `[0, 1]`: the form an output intent's `lut16Type`
    /// B2A input curves take.
    fn rgb_to_pcs_lab(&self, rgb: [f64; 3]) -> [f64; 3] {
        match self {
            LcmsLut::Legacy(l) => l.rgb_to_pcs_lab(rgb),
            LcmsLut::Multi(_) => LabEncoding::V2
                .encode(self.rgb_to_lab(rgb))
                .map(|v| v.clamp(0.0, 1.0)),
        }
    }

    /// RGB (each `[0, 1]`) → Lab.
    fn rgb_to_lab(&self, rgb: [f64; 3]) -> [f64; 3] {
        match self {
            LcmsLut::Legacy(l) => LabEncoding::V2.decode(l.rgb_to_pcs_lab(rgb)),
            LcmsLut::Multi(m) => {
                let mut out = [0.0; 4];
                m.eval(&rgb, &mut out);
                m.coding.decode_lab([out[0], out[1], out[2]])
            }
        }
    }
}

/// The A, M and B curve sets of a `lutAToBType` or `lutBToAType` tag, read
/// from the profile's bytes as lcms2 reads them (`ReadSetOfCurves`). moxcms
/// 0.8.1 does not advance past an empty `curv` — the usual identity — so
/// every curve after one in the same set comes back as another identity.
struct CurveSets {
    a: Vec<LcmsCurve>,
    m: Vec<LcmsCurve>,
    b: Vec<LcmsCurve>,
}

impl CurveSets {
    /// The sets of the tag `sig` in the profile whose raw bytes are `icc`;
    /// `None` when the tag is not one of those types or does not parse.
    fn read(icc: &[u8], sig: &[u8; 4]) -> Option<Self> {
        let be32 = |at: usize| -> Option<usize> {
            Some(u32::from_be_bytes(icc.get(at..at + 4)?.try_into().ok()?) as usize)
        };
        let count = be32(128)?;
        let entry = (0..count.min(1024))
            .map(|i| 132 + 12 * i)
            .find(|&e| icc.get(e..e + 4) == Some(sig))?;
        let start = be32(entry + 4)?;
        let to_pcs = match icc.get(start..start + 4)? {
            b"mAB " => true,
            b"mBA " => false,
            _ => return None,
        };
        let (n_in, n_out) = (
            usize::from(*icc.get(start + 8)?),
            usize::from(*icc.get(start + 9)?),
        );
        let (device, pcs) = if to_pcs { (n_in, n_out) } else { (n_out, n_in) };
        let set = |field: usize, n: usize| -> Option<Vec<LcmsCurve>> {
            let offset = be32(start + field)?;
            if offset == 0 {
                return Some(Vec::new());
            }
            let mut at = start + offset;
            let mut curves = Vec::with_capacity(n);
            for _ in 0..n {
                let (curve, len) = LcmsCurve::read(icc.get(at..)?)?;
                curves.push(curve);
                // lcms2 aligns to four bytes of the profile between curves.
                at = (at + len).next_multiple_of(4);
            }
            Some(curves)
        };
        // The header's offsets: B at 12, matrix 16, M 20, CLUT 24, A 28.
        Some(Self {
            a: set(28, device)?,
            m: set(20, pcs)?,
            b: set(12, pcs)?,
        })
    }
}

/// An ICC v4 `lutAToBType` (`to_pcs`) or `lutBToAType` table, in lcms2's
/// order: device curves (A), CLUT, M curves, matrix with offset, PCS curves
/// (B) toward the PCS, and the reverse from it. Any element may be absent.
/// The CLUT has its own number of grid points per input and interpolates
/// as lcms2's does: tetrahedral, except trilinear for a Lab-indexed output
/// table (`ChangeInterpolationToTrilinear`). Lab is v4-encoded.
struct MultiLut {
    to_pcs: bool,
    coding: PcsCoding,
    n_in: usize,
    n_out: usize,
    a: Vec<LcmsCurve>,
    clut: Option<(Vec<f32>, Vec<usize>)>,
    m: Vec<LcmsCurve>,
    matrix: [[f64; 3]; 3],
    offset: [f64; 3],
    b: Vec<LcmsCurve>,
}

impl MultiLut {
    /// moxcms's parse of the table `t`, of a profile whose PCS is `space`,
    /// with its curves from `sets`. `None` unless the table has `n_in`
    /// inputs and `n_out` outputs, one side three (the PCS) and the other
    /// three or four, a CLUT whenever they differ, and a curve per channel
    /// in each set it carries.
    fn new(
        t: &LutMultidimensionalType,
        sets: CurveSets,
        n_in: usize,
        n_out: usize,
        to_pcs: bool,
        space: Pcs,
    ) -> Option<Self> {
        let (device, pcs) = if to_pcs { (n_in, n_out) } else { (n_out, n_in) };
        if usize::from(t.num_input_channels) != n_in
            || usize::from(t.num_output_channels) != n_out
            || pcs != 3
            || !(3..=4).contains(&device)
        {
            return None;
        }
        let curves = |set: Vec<LcmsCurve>, n: usize| -> Option<Vec<LcmsCurve>> {
            (set.is_empty() || set.len() == n).then_some(set)
        };
        let clut = match &t.clut {
            Some(store) => {
                let grids: Vec<usize> = t.grid_points[..n_in]
                    .iter()
                    .map(|&g| usize::from(g))
                    .collect();
                if grids.iter().any(|&g| g < 2) {
                    return None;
                }
                let len = grids.iter().try_fold(n_out, |acc, &g| acc.checked_mul(g))?;
                Some((normalised(store, len)?, grids))
            }
            None if n_in == n_out => None,
            None => return None,
        };
        Some(Self {
            to_pcs,
            coding: PcsCoding::new(space, None),
            n_in,
            n_out,
            a: curves(sets.a, device)?,
            clut,
            m: curves(sets.m, 3)?,
            matrix: t.matrix.v,
            offset: t.bias.v,
            b: curves(sets.b, 3)?,
        })
    }

    /// `input` (`n_in` values, `[0, 1]` encoded) → `out` (`n_out`).
    fn eval(&self, input: &[f64], out: &mut [f64; 4]) {
        let mut v = [0.0; 4];
        v[..self.n_in].copy_from_slice(&input[..self.n_in]);
        if self.to_pcs {
            Self::curves(&self.a, &mut v);
            self.clut(&mut v);
            Self::curves(&self.m, &mut v);
            self.matrix(&mut v);
            Self::curves(&self.b, &mut v);
        } else {
            Self::curves(&self.b, &mut v);
            self.matrix(&mut v);
            Self::curves(&self.m, &mut v);
            self.clut(&mut v);
            Self::curves(&self.a, &mut v);
        }
        out[..self.n_out].copy_from_slice(&v[..self.n_out]);
    }

    fn curves(set: &[LcmsCurve], v: &mut [f64; 4]) {
        for (curve, x) in set.iter().zip(v.iter_mut()) {
            *x = curve.eval(*x);
        }
    }

    fn matrix(&self, v: &mut [f64; 4]) {
        let m = &self.matrix;
        let x = [v[0], v[1], v[2]];
        for i in 0..3 {
            v[i] = m[i][0] * x[0] + m[i][1] * x[1] + m[i][2] * x[2] + self.offset[i];
        }
    }

    fn clut(&self, v: &mut [f64; 4]) {
        let Some((data, grids)) = &self.clut else {
            return;
        };
        let mut out = [0.0; 4];
        match *grids.as_slice() {
            [g0, g1, g2, g3] => {
                eval4_grids(data, [g0, g1, g2, g3], *v, &mut out[..self.n_out]);
            }
            [g0, g1, g2] => {
                let trilinear = !self.to_pcs && self.coding != PcsCoding::Xyz;
                eval3_grids(
                    data,
                    [g0, g1, g2],
                    [v[0], v[1], v[2]],
                    trilinear,
                    &mut out[..self.n_out],
                );
            }
            _ => unreachable!("MultiLut::new admits three or four inputs"),
        }
        *v = out;
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
    eval4_grids(cube, [grid; 4], input, out);
}

/// [`eval4_lcms`] on a grid with its own number of points per input, as a
/// v4 table's may be; `out.len()` outputs per point.
fn eval4_grids(cube: &[f32], grids: [usize; 4], input: [f64; 4], out: &mut [f64]) {
    let [s0, s1, s2, s3] = strides(grids, out.len());
    let (k0, rk, k_step) = grid_axis(input[0], grids[0], s0);
    let x = grid_axis(input[1], grids[1], s1);
    let y = grid_axis(input[2], grids[2], s2);
    let z = grid_axis(input[3], grids[3], s3);
    let mut lo = [0.0; 4];
    let mut hi = [0.0; 4];
    let n_out = out.len();
    tetrahedral3(cube, k0, x, y, z, &mut lo[..n_out]);
    tetrahedral3(cube, k0 + k_step, x, y, z, &mut hi[..n_out]);
    for ch in 0..n_out {
        out[ch] = lo[ch] + (hi[ch] - lo[ch]) * rk;
    }
}

/// lcms2's interpolation of a three-input table (`TetrahedralInterp16`):
/// tetrahedral. `cube` holds the grid with the last input varying fastest
/// and three outputs per point.
fn eval3_lcms(cube: &[f32], grid: usize, input: [f64; 3], out: &mut [f64; 3]) {
    eval3_grids(cube, [grid; 3], input, false, out);
}

/// [`eval3_lcms`] on a grid with its own number of points per input, and
/// `out.len()` outputs per point; `trilinear` as lcms2 reads a Lab-indexed
/// output table.
fn eval3_grids(cube: &[f32], grids: [usize; 3], input: [f64; 3], trilinear: bool, out: &mut [f64]) {
    let [s0, s1, s2] = strides(grids, out.len());
    let x = grid_axis(input[0], grids[0], s0);
    let y = grid_axis(input[1], grids[1], s1);
    let z = grid_axis(input[2], grids[2], s2);
    if trilinear {
        trilinear3(cube, x, y, z, out);
    } else {
        tetrahedral3(cube, 0, x, y, z, out);
    }
}

/// The distance between neighbouring points along each input of a grid
/// with `grids` points per input, the last varying fastest, and `n_out`
/// outputs per point.
fn strides<const N: usize>(grids: [usize; N], n_out: usize) -> [usize; N] {
    let mut s = [0; N];
    let mut stride = n_out;
    for i in (0..N).rev() {
        s[i] = stride;
        stride *= grids[i];
    }
    s
}

/// lcms2's `TrilinearInterpFloat` over a three-input grid; each axis is
/// `(offset, fraction, step)`.
fn trilinear3(
    cube: &[f32],
    (x0, rx, sx): (usize, f64, usize),
    (y0, ry, sy): (usize, f64, usize),
    (z0, rz, sz): (usize, f64, usize),
    out: &mut [f64],
) {
    let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;
    for (ch, o) in out.iter_mut().enumerate() {
        let d = |x: usize, y: usize, z: usize| f64::from(cube[x + y + z + ch]);
        let (x1, y1, z1) = (x0 + sx, y0 + sy, z0 + sz);
        let near = lerp(
            lerp(d(x0, y0, z0), d(x0, y0, z1), rz),
            lerp(d(x0, y1, z0), d(x0, y1, z1), rz),
            ry,
        );
        let far = lerp(
            lerp(d(x1, y0, z0), d(x1, y0, z1), rz),
            lerp(d(x1, y1, z0), d(x1, y1, z1), rz),
            ry,
        );
        *o = lerp(near, far, rx);
    }
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
    out: &mut [f64],
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
    bpc: Option<BpcParams>,
}

impl HandRolledChainStage1Rgb {
    /// Build the chain stage-1 sampler from the source RGB profile and
    /// the OutputIntent CMYK profile, picking the source's A2B table
    /// and the OI's B2A table for the given rendering intent, with
    /// black-point compensation `bpc` in XYZ between them. Returns
    /// `source_icc` and `oi_icc` are the profiles' raw bytes. `None` when
    /// either side has a table these evaluators cannot read
    /// or the source has neither table nor matrix — the caller falls back
    /// to the moxcms-driven chain in that case.
    pub(super) fn new(
        source: &ColorProfile,
        source_icc: &[u8],
        output_intent: &ColorProfile,
        oi_icc: &[u8],
        intent: RenderingIntent,
        bpc: Option<BpcParams>,
    ) -> Option<Self> {
        if source.color_space != DataColorSpace::Rgb {
            return None;
        }
        let src = SourceA2BSampler::new(source, source_icc, intent)?;
        let oi = LabToCmykSampler::new(output_intent, oi_icc, intent, None)?;
        Some(Self { src, oi, bpc })
    }

    /// Without compensation the source's encoded PCS Lab goes straight into
    /// the B2A table; with it, through XYZ, where lcms2 compensates.
    #[inline]
    fn sample(&self, r: f32, g: f32, b: f32) -> [f32; 4] {
        if let Some(p) = &self.bpc {
            let xyz = self.src.sample_xyz([r, g, b].map(f64::from));
            return self
                .oi
                .lut
                .xyz_to_ink(apply_bpc_xyz_d50(xyz, p))
                .map(|v| v as f32);
        }
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
fn lcms_table(tables: [Option<&LutWarehouse>; 3], intent: RenderingIntent) -> Option<usize> {
    let i = match intent {
        RenderingIntent::Perceptual => 0,
        RenderingIntent::RelativeColorimetric | RenderingIntent::AbsoluteColorimetric => 1,
        RenderingIntent::Saturation => 2,
    };
    if tables[i].is_some() {
        Some(i)
    } else {
        tables[0].map(|_| 0)
    }
}

/// A table of a profile chosen as [`lcms_table`] chooses it, with its tag
/// signature.
pub(super) type Table<'a> = (&'a LutWarehouse, &'static [u8; 4]);

/// The profile's A2B table for `intent` as lcms2 reads it.
fn a2b_table(profile: &ColorProfile, intent: RenderingIntent) -> Option<Table<'_>> {
    let tables = [
        profile.lut_a_to_b_perceptual.as_ref(),
        profile.lut_a_to_b_colorimetric.as_ref(),
        profile.lut_a_to_b_saturation.as_ref(),
    ];
    let i = lcms_table(tables, intent)?;
    Some((tables[i]?, [b"A2B0", b"A2B1", b"A2B2"][i]))
}

/// The profile's B2A table for `intent` as lcms2 reads it.
fn b2a_table(profile: &ColorProfile, intent: RenderingIntent) -> Option<Table<'_>> {
    let tables = [
        profile.lut_b_to_a_perceptual.as_ref(),
        profile.lut_b_to_a_colorimetric.as_ref(),
        profile.lut_b_to_a_saturation.as_ref(),
    ];
    let i = lcms_table(tables, intent)?;
    Some((tables[i]?, [b"B2A0", b"B2A1", b"B2A2"][i]))
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
/// as lcms2 converts: the source's A2B table for the intent to Lab,
/// black-point compensation in XYZ when it applies, then the OutputIntent's
/// B2A table for the intent, composed per pixel so no intermediate table
/// quantises it.
pub(super) struct HandRolledChainStage1Cmyk {
    a2b: LcmsLut,
    b2a: LcmsLut,
    bpc: Option<BpcParams>,
}

impl HandRolledChainStage1Cmyk {
    /// `source_icc` and `oi_icc` are the profiles' raw bytes. `None` when
    /// either profile is not CMYK, or carries tables these evaluators cannot
    /// read; the caller then builds moxcms's transform for the same intent.
    pub(super) fn new(
        source: &ColorProfile,
        source_icc: &[u8],
        output_intent: &ColorProfile,
        oi_icc: &[u8],
        intent: RenderingIntent,
        bpc: Option<BpcParams>,
    ) -> Option<Self> {
        if source.color_space != DataColorSpace::Cmyk
            || output_intent.color_space != DataColorSpace::Cmyk
        {
            return None;
        }
        Some(Self {
            a2b: LcmsLut::to_pcs(a2b_table(source, intent)?, source_icc, 4)?,
            b2a: LcmsLut::from_pcs(b2a_table(output_intent, intent)?, oi_icc, 4)?,
            bpc,
        })
    }

    /// Source ink (each `[0, 1]`) → OutputIntent ink.
    pub(super) fn sample(&self, ink: [f64; 4]) -> [f64; 4] {
        match &self.bpc {
            Some(p) => self
                .b2a
                .xyz_to_ink(apply_bpc_xyz_d50(self.a2b.ink_to_xyz(ink), p)),
            None => self.b2a.lab_to_ink(self.a2b.ink_to_lab(ink)),
        }
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
    b2a: LcmsLut,
}

impl HandRolledChainStage1Gray {
    /// `None` when the source is not a TRC-only Gray ([`GrayTrc`]), or the
    /// output intent, whose raw bytes are `oi_icc`, not CMYK with a B2A
    /// table these evaluators read.
    pub(super) fn new(
        source: &ColorProfile,
        output_intent: &ColorProfile,
        oi_icc: &[u8],
        intent: RenderingIntent,
        bpc: Option<BpcParams>,
    ) -> Option<Self> {
        if output_intent.color_space != DataColorSpace::Cmyk {
            return None;
        }
        Some(Self {
            trc: GrayTrc::new(source)?,
            bpc,
            b2a: LcmsLut::from_pcs(b2a_table(output_intent, intent)?, oi_icc, 4)?,
        })
    }

    /// Gray (`[0, 1]`) → OutputIntent ink.
    pub(super) fn sample(&self, gray: f64) -> [f64; 4] {
        let y = self.trc.y(gray);
        let mut xyz = [WP_D50[0] * y, y, WP_D50[2] * y];
        if let Some(p) = &self.bpc {
            xyz = apply_bpc_xyz_d50(xyz, p);
        }
        self.b2a.xyz_to_ink(xyz)
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
    const RGB_LUT_V4: &[u8] = include_bytes!("../../tests/data/cmyk_intent/rgb_lut_v4.icc");
    const SRGB: &[u8] = include_bytes!("../../tests/data/cmyk_intent/srgb.icc");
    const SHADOW: &[u8] = include_bytes!("../../tests/data/cmyk_intent/shadow.icc");
    const SHADOW_V4: &[u8] = include_bytes!("../../tests/data/cmyk_intent/shadow_v4.icc");
    const MAB: &[u8] = include_bytes!("../../tests/data/cmyk_intent/mab.icc");
    const RGB_MAB: &[u8] = include_bytes!("../../tests/data/cmyk_intent/rgb_mab.icc");
    const XYZ_V4: &[u8] = include_bytes!("../../tests/data/cmyk_intent/xyz_v4.icc");
    const XYZ_MAB: &[u8] = include_bytes!("../../tests/data/cmyk_intent/xyz_mab.icc");
    const INKLIMIT_MATRIX: &[u8] =
        include_bytes!("../../tests/data/cmyk_intent/inklimit_matrix.icc");
    const RGB_XYZ: &[u8] = include_bytes!("../../tests/data/cmyk_intent/rgb_xyz.icc");
    const RGB_XYZ_MAB: &[u8] = include_bytes!("../../tests/data/cmyk_intent/rgb_xyz_mab.icc");

    /// The two legs of lcms2's black-point round trip: `B2A0` and `A2B1`.
    fn legs(icc: &[u8]) -> (LcmsLut, LcmsLut) {
        let profile = ColorProfile::new_from_slice(icc).unwrap();
        let b2a0 = profile.lut_b_to_a_perceptual.as_ref().unwrap();
        let a2b1 = profile.lut_a_to_b_colorimetric.as_ref().unwrap();
        (
            LcmsLut::from_pcs((b2a0, b"B2A0"), icc, 4).unwrap(),
            LcmsLut::to_pcs((a2b1, b"A2B1"), icc, 4).unwrap(),
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
    // One 16-bit step of an XYZ PCS. Near black that is several hundredths
    // of an a\* or b\*, so XYZ tables are compared in XYZ.
    const XYZ_TOLERANCE: f64 = XYZ_MAX / 65535.0;

    #[test]
    fn perceptual_output_table_matches_lcms() {
        for (name, icc, want) in [
            ("inklimit", INKLIMIT, &reference::INKLIMIT_B2A0),
            (
                "inklimit_lut8",
                INKLIMIT_LUT8,
                &reference::INKLIMIT_LUT8_B2A0,
            ),
            // A matrix that lcms2 applies to the table's input first.
            (
                "inklimit_matrix",
                INKLIMIT_MATRIX,
                &reference::INKLIMIT_MATRIX_B2A0,
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

    /// An ICC v4 profile's `lutBToAType` and `lutAToBType` tables evaluate
    /// as lcms2's do: their curves, matrix and offset, CLUTs with a grid
    /// per input, 16-bit and 8-bit, trilinear from Lab and tetrahedral to
    /// it. (Relative colorimetric, where lcms2 does not force black-point
    /// compensation into the profile.)
    #[test]
    fn v4_tables_match_lcms() {
        let profile = ColorProfile::new_from_slice(MAB).unwrap();
        let relcol = RenderingIntent::RelativeColorimetric;
        let b2a1 = LcmsLut::from_pcs(b2a_table(&profile, relcol).unwrap(), MAB, 4).unwrap();
        let a2b1 = LcmsLut::to_pcs(a2b_table(&profile, relcol).unwrap(), MAB, 4).unwrap();
        assert!(matches!(b2a1, LcmsLut::Multi(_)) && matches!(a2b1, LcmsLut::Multi(_)));
        for (lab, want) in reference::LAB_SAMPLES.iter().zip(&reference::MAB_B2A1) {
            assert_near(
                &format!("mab B2A1 at {lab:?}"),
                b2a1.lab_to_ink(*lab),
                *want,
                INK_TOLERANCE,
            );
        }
        for (cmyk, want) in reference::SAMPLES.iter().zip(&reference::MAB_A2B1) {
            let got = a2b1.ink_to_lab(cmyk.map(|v| f64::from(v) / 255.0));
            assert_near(&format!("mab A2B1 at {cmyk:?}"), got, *want, LAB_TOLERANCE);
        }
    }

    /// Tables with an XYZ PCS evaluate as lcms2's do: the PCS side holds
    /// XYZ / (1 + 32767/32768); an XYZ-indexed B2A table is tetrahedral,
    /// through its matrix first (`lut16Type`, as Ghostscript's
    /// `ps_cmyk.icc` has one) or its `mBA` matrix element.
    #[test]
    fn xyz_tables_match_lcms() {
        let relcol = RenderingIntent::RelativeColorimetric;
        for (name, icc, b2a1_want, a2b1_want) in [
            (
                "xyz_v4",
                XYZ_V4,
                &reference::XYZ_V4_B2A1,
                &reference::XYZ_V4_A2B1,
            ),
            (
                "xyz_mab",
                XYZ_MAB,
                &reference::XYZ_MAB_B2A1,
                &reference::XYZ_MAB_A2B1,
            ),
        ] {
            let profile = ColorProfile::new_from_slice(icc).unwrap();
            let b2a1 = LcmsLut::from_pcs(b2a_table(&profile, relcol).unwrap(), icc, 4).unwrap();
            let a2b1 = LcmsLut::to_pcs(a2b_table(&profile, relcol).unwrap(), icc, 4).unwrap();
            assert_eq!(b2a1.coding(), PcsCoding::Xyz);
            for (lab, want) in reference::LAB_SAMPLES.iter().zip(b2a1_want) {
                let got = b2a1.lab_to_ink(*lab);
                assert_near(
                    &format!("{name} B2A1 at {lab:?}"),
                    got,
                    *want,
                    INK_TOLERANCE,
                );
            }
            for (cmyk, want) in reference::SAMPLES.iter().zip(a2b1_want) {
                let got = a2b1.ink_to_xyz(cmyk.map(|v| f64::from(v) / 255.0));
                let want = lab_to_xyz_d50(*want);
                assert_near(
                    &format!("{name} A2B1 at {cmyk:?}"),
                    got,
                    want,
                    XYZ_TOLERANCE,
                );
            }
        }
    }

    /// A Lab-PCS table's XYZ is its Lab through stet's own Lab → XYZ, white
    /// [`WP_D50`], as the display bake has always taken it: the XYZ PCS,
    /// which converts with lcms2's D50, leaves every Lab table's bytes as
    /// they were.
    #[test]
    fn a_lab_tables_xyz_is_unchanged() {
        let relcol = RenderingIntent::RelativeColorimetric;
        for icc in [MAB, INKLIMIT_LUT8] {
            let profile = ColorProfile::new_from_slice(icc).unwrap();
            let a2b1 = LcmsLut::to_pcs(a2b_table(&profile, relcol).unwrap(), icc, 4).unwrap();
            for cmyk in reference::SAMPLES {
                let ink = cmyk.map(|v| f64::from(v) / 255.0);
                assert_eq!(a2b1.ink_to_xyz(ink), lab_to_xyz_d50(a2b1.ink_to_lab(ink)));
            }
        }
    }

    /// [`display_srgb_to_lab`] undoes the display: every sRGB colour on a
    /// grid comes back from its Lab through the display matrix and transfer
    /// function to rounding error. It is also lcms2's built-in sRGB, to the
    /// display matrix's eight-digit coefficients and stet's D50 white: within
    /// 0.014 at a saturated primary.
    #[test]
    fn display_srgb_to_lab_inverts_the_display() {
        let steps = (0..=16).map(|i| f64::from(i) / 16.0);
        for r in steps.clone() {
            for g in steps.clone() {
                for b in steps.clone() {
                    let lab = display_srgb_to_lab([r, g, b]);
                    let linear = xyz_d50_to_linear_srgb_d65(lab_to_xyz_d50(lab));
                    let back = linear.map(linear_to_srgb);
                    assert_near(&format!("{:?}", [r, g, b]), back, [r, g, b], 1e-12);
                }
            }
        }
        for (rgb, want) in reference::RGB_SAMPLES.iter().zip(&reference::SRGB_LAB) {
            let rgb = rgb.map(|v| f64::from(v) / 255.0);
            assert_near(
                &format!("lcms2 at {rgb:?}"),
                display_srgb_to_lab(rgb),
                *want,
                0.02,
            );
        }
    }

    /// A matrix-shaper RGB source's Lab is lcms2's: its XYZ is in lcms2's
    /// frame, white [`LCMS_D50`], so taking it to Lab with moxcms's or
    /// stet's own D50 tints white by 0.02 b\* and moves every colour.
    #[test]
    fn a_matrix_shapers_lab_is_lcms2s() {
        let relcol = RenderingIntent::RelativeColorimetric;
        for (name, icc, want) in [
            ("rgb_gamma", RGB_GAMMA, &reference::RGB_GAMMA_LAB),
            ("srgb", SRGB, &reference::SRGB_LAB),
        ] {
            let profile = ColorProfile::new_from_slice(icc).unwrap();
            let shaper = SourceA2BSampler::new(&profile, icc, relcol).unwrap();
            assert!(matches!(shaper, SourceA2BSampler::Shaper(_)));
            for (rgb, want) in reference::RGB_SAMPLES.iter().zip(want) {
                let rgb = rgb.map(|v| f64::from(v) / 255.0);
                let got = xyz_d50_to_lab(shaper.sample_xyz(rgb));
                assert_near(&format!("{name} at {rgb:?}"), got, *want, 1e-4);
                let encoded = rgb.map(|v| v as f32);
                let got = shaper.sample_pcs_lab(encoded[0], encoded[1], encoded[2]);
                let got = LabEncoding::V2.decode(got.map(f64::from));
                assert_near(
                    &format!("{name} encoded at {rgb:?}"),
                    got,
                    *want,
                    LAB_TOLERANCE,
                );
            }
        }
    }

    /// The black of an XYZ-PCS table is its XYZ, not its bytes read as
    /// Lab: `split_xyz.icc`'s 400% ink through `A2B1` is lcms2's.
    #[test]
    fn an_xyz_tables_black_is_its_xyz() {
        let profile = ColorProfile::new_from_slice(SPLIT_XYZ).unwrap();
        let relcol = RenderingIntent::RelativeColorimetric;
        let a2b1 = LcmsLut::to_pcs(a2b_table(&profile, relcol).unwrap(), SPLIT_XYZ, 4).unwrap();
        let xyz = lab_to_xyz_d50;
        for (cmyk, want) in reference::SAMPLES.iter().zip(&reference::SPLIT_XYZ_A2B1) {
            let got = a2b1.ink_to_xyz(cmyk.map(|v| f64::from(v) / 255.0));
            assert_near(
                &format!("split_xyz A2B1 at {cmyk:?}"),
                got,
                xyz(*want),
                XYZ_TOLERANCE,
            );
        }
        let black = reference::SAMPLES
            .iter()
            .position(|s| *s == [255; 4])
            .unwrap();
        let got = xyz(table_black(&profile, SPLIT_XYZ, relcol).unwrap());
        let want = xyz(reference::SPLIT_XYZ_A2B1[black]);
        assert_near("table black", got, want, XYZ_TOLERANCE);
    }

    /// A four-input grid with its own number of points per input is
    /// indexed by each input's own size: an affine table, which any
    /// interpolation reproduces, comes back exactly only if the strides
    /// and fractions are right.
    #[test]
    fn four_input_grids_may_differ_per_input() {
        let grids = [3, 4, 5, 2];
        let f = |x: [f64; 4]| {
            [
                0.1 + 0.2 * x[0] + 0.3 * x[1],
                0.4 * x[2] + 0.5 * x[3],
                0.25 + 0.15 * x[0] + 0.2 * x[3],
            ]
        };
        let mut cube = Vec::new();
        for i in 0..grids[0] {
            for j in 0..grids[1] {
                for k in 0..grids[2] {
                    for l in 0..grids[3] {
                        let at = |n: usize, g: usize| n as f64 / (g - 1) as f64;
                        let x = [
                            at(i, grids[0]),
                            at(j, grids[1]),
                            at(k, grids[2]),
                            at(l, grids[3]),
                        ];
                        cube.extend(f(x).map(|v| v as f32));
                    }
                }
            }
        }
        for x in [
            [0.3, 0.7, 0.45, 0.9],
            [1.0, 0.0, 0.62, 0.5],
            [0.05, 0.99, 1.0, 0.0],
        ] {
            let mut out = [0.0; 3];
            eval4_grids(&cube, grids, x, &mut out);
            assert_near(&format!("{x:?}"), out, f(x), 1e-6);
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
            (
                "inklimit_matrix",
                INKLIMIT_MATRIX,
                reference::INKLIMIT_MATRIX_ROUND_TRIP,
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
    /// display bake through `LcmsLut`, the black point and the chain stage
    /// 1), not by the `lut16Type` display sampler.
    #[test]
    fn lut8_tables_are_for_the_lcms_exact_evaluators_only() {
        let profile = ColorProfile::new_from_slice(INKLIMIT_LUT8).unwrap();
        let a2b1 = profile.lut_a_to_b_colorimetric.as_ref().unwrap();
        let b2a0 = profile.lut_b_to_a_perceptual.as_ref().unwrap();
        assert!(OwnedLutSampler::from_warehouse(a2b1, 4, 3).is_none());
        assert!(OwnedLutSampler::from_warehouse(b2a0, 3, 4).is_none());
        assert!(!can_sample(&profile, a2b1));
        let perceptual = RenderingIntent::Perceptual;
        assert!(LabToCmykSampler::new(&profile, INKLIMIT_LUT8, perceptual, None).is_some());
        assert!(OwnedLutSampler::lcms_exact(a2b1, 4, 3, Pcs::Lab).is_some());
        assert!(OwnedLutSampler::lcms_exact(b2a0, 3, 4, Pcs::Lab).is_some());
    }

    const INTENTS: [RenderingIntent; 3] = [
        RenderingIntent::Perceptual,
        RenderingIntent::RelativeColorimetric,
        RenderingIntent::Saturation,
    ];

    /// The CMYK chain stage 1 is lcms2's, ink for ink, under each intent:
    /// the intent's table on each side, or table 0 where a profile has
    /// none (`split.icc` has no `A2B2`, `inklimit.icc` no `B2A2`), and
    /// `lut8Type` tables and an XYZ PCS too.
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
            ("xyz", SPLIT_XYZ, SPLIT, &reference::CHAIN_SPLIT_XYZ_SPLIT),
        ] {
            let (source_icc, oi_icc) = (source, oi);
            let source = ColorProfile::new_from_slice(source_icc).unwrap();
            let oi = ColorProfile::new_from_slice(oi_icc).unwrap();
            for (intent, want) in INTENTS.into_iter().zip(want) {
                let stage1 =
                    HandRolledChainStage1Cmyk::new(&source, source_icc, &oi, oi_icc, intent, None)
                        .unwrap();
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
            let source_icc = source;
            let source = ColorProfile::new_from_slice(source_icc).unwrap();
            for (intent, want) in INTENTS.into_iter().zip(want) {
                let stage1 =
                    HandRolledChainStage1Rgb::new(&source, source_icc, &oi, INKLIMIT, intent, None)
                        .unwrap();
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

    /// Stage 1 with black-point compensation, from each kind of source
    /// into an output intent with a black to compensate to, is lcms2's with
    /// its flag on and off. Into the ICC v4 output intent lcms2 compensates
    /// perceptual and saturation either way.
    #[test]
    fn chain_stage1_compensates_as_lcms() {
        use super::super::black_point::{chain_compensation, detect_rgb, detect_xyz};
        type Chain<const N: usize> = [[[[f64; 4]; N]; 2]; 3];
        let compensation = |source_black, oi: &[u8], intent, bpc: usize| {
            let profile = ColorProfile::new_from_slice(oi).unwrap();
            chain_compensation(source_black, &profile, oi, intent, bpc == 1)
        };
        let cmyk: [(&str, &[u8], &[u8], &Chain<16>); 10] = [
            ("split", SPLIT, SHADOW, &reference::CHAIN_BPC_SPLIT_SHADOW),
            (
                "split_sat v4",
                SPLIT_SAT,
                SHADOW_V4,
                &reference::CHAIN_BPC_SPLIT_SAT_SHADOW_V4,
            ),
            (
                "inklimit",
                INKLIMIT,
                SHADOW,
                &reference::CHAIN_BPC_INKLIMIT_SHADOW,
            ),
            ("mab", MAB, SHADOW, &reference::CHAIN_BPC_MAB_SHADOW),
            ("into mab", SPLIT, MAB, &reference::CHAIN_BPC_SPLIT_MAB),
            // An XYZ PCS, as a source, as the output intent, and both.
            (
                "xyz_v4",
                XYZ_V4,
                SHADOW,
                &reference::CHAIN_BPC_XYZ_V4_SHADOW,
            ),
            (
                "xyz_mab",
                XYZ_MAB,
                SHADOW,
                &reference::CHAIN_BPC_XYZ_MAB_SHADOW,
            ),
            (
                "into xyz_v4",
                SPLIT,
                XYZ_V4,
                &reference::CHAIN_BPC_SPLIT_XYZ_V4,
            ),
            (
                "into xyz_mab",
                SPLIT,
                XYZ_MAB,
                &reference::CHAIN_BPC_SPLIT_XYZ_MAB,
            ),
            (
                "xyz to xyz",
                XYZ_V4,
                XYZ_MAB,
                &reference::CHAIN_BPC_XYZ_V4_XYZ_MAB,
            ),
        ];
        for (name, source_icc, oi_icc, want) in cmyk {
            let source = ColorProfile::new_from_slice(source_icc).unwrap();
            let oi = ColorProfile::new_from_slice(oi_icc).unwrap();
            for (i, intent) in INTENTS.into_iter().enumerate() {
                for (bpc, want) in want[i].iter().enumerate() {
                    let black = detect_xyz(&source, source_icc, intent);
                    let p = compensation(black, oi_icc, intent, bpc);
                    let stage1 =
                        HandRolledChainStage1Cmyk::new(&source, source_icc, &oi, oi_icc, intent, p)
                            .unwrap();
                    for (cmyk, want) in reference::SAMPLES.iter().zip(want) {
                        assert_near(
                            &format!("{name} {intent:?} bpc {bpc} at {cmyk:?}"),
                            stage1.sample(cmyk.map(|v| f64::from(v) / 255.0)),
                            *want,
                            INK_TOLERANCE,
                        );
                    }
                }
            }
        }
        let rgb: [(&str, &[u8], &[u8], &Chain<15>); 10] = [
            (
                "rgb_gamma",
                RGB_GAMMA,
                SHADOW,
                &reference::CHAIN_BPC_RGB_GAMMA_SHADOW,
            ),
            (
                "rgb_lut",
                RGB_LUT,
                SHADOW,
                &reference::CHAIN_BPC_RGB_LUT_SHADOW,
            ),
            (
                "rgb_lut_v4",
                RGB_LUT_V4,
                SHADOW,
                &reference::CHAIN_BPC_RGB_LUT_V4_SHADOW,
            ),
            ("srgb", SRGB, SHADOW, &reference::CHAIN_BPC_SRGB_SHADOW),
            (
                "srgb v4",
                SRGB,
                SHADOW_V4,
                &reference::CHAIN_BPC_SRGB_SHADOW_V4,
            ),
            (
                "rgb_mab",
                RGB_MAB,
                SHADOW,
                &reference::CHAIN_BPC_RGB_MAB_SHADOW,
            ),
            (
                "rgb into mab",
                RGB_GAMMA,
                MAB,
                &reference::CHAIN_BPC_RGB_GAMMA_MAB,
            ),
            (
                "rgb_xyz",
                RGB_XYZ,
                SHADOW,
                &reference::CHAIN_BPC_RGB_XYZ_SHADOW,
            ),
            (
                "rgb_xyz_mab",
                RGB_XYZ_MAB,
                SHADOW,
                &reference::CHAIN_BPC_RGB_XYZ_MAB_SHADOW,
            ),
            (
                "rgb into xyz_v4",
                RGB_GAMMA,
                XYZ_V4,
                &reference::CHAIN_BPC_RGB_GAMMA_XYZ_V4,
            ),
        ];
        for (name, source_icc, oi_icc, want) in rgb {
            let source = ColorProfile::new_from_slice(source_icc).unwrap();
            let oi = ColorProfile::new_from_slice(oi_icc).unwrap();
            for (i, intent) in INTENTS.into_iter().enumerate() {
                for (bpc, want) in want[i].iter().enumerate() {
                    let black = detect_rgb(&source, source_icc, intent);
                    let p = compensation(black, oi_icc, intent, bpc);
                    let stage1 =
                        HandRolledChainStage1Rgb::new(&source, source_icc, &oi, oi_icc, intent, p)
                            .unwrap();
                    for (rgb, want) in reference::RGB_SAMPLES.iter().zip(want) {
                        let [r, g, b] = rgb.map(|v| f64::from(v) / 255.0);
                        assert_near(
                            &format!("{name} {intent:?} bpc {bpc} at {rgb:?}"),
                            stage1.sample_cmyk_f64(r, g, b),
                            *want,
                            INK_TOLERANCE,
                        );
                    }
                }
            }
        }
        let lab: [(&str, &[u8], &Chain<6>); 5] = [
            ("lab", SHADOW, &reference::CHAIN_BPC_LAB_SHADOW),
            ("lab v4", SHADOW_V4, &reference::CHAIN_BPC_LAB_SHADOW_V4),
            ("lab into mab", MAB, &reference::CHAIN_BPC_LAB_MAB),
            ("lab into xyz_v4", XYZ_V4, &reference::CHAIN_BPC_LAB_XYZ_V4),
            (
                "lab into xyz_mab",
                XYZ_MAB,
                &reference::CHAIN_BPC_LAB_XYZ_MAB,
            ),
        ];
        for (name, oi_icc, want) in lab {
            let oi = ColorProfile::new_from_slice(oi_icc).unwrap();
            for (i, intent) in INTENTS.into_iter().enumerate() {
                for (bpc, want) in want[i].iter().enumerate() {
                    let p = compensation(None, oi_icc, intent, bpc);
                    let sampler = LabToCmykSampler::new(&oi, oi_icc, intent, p).unwrap();
                    for (lab, want) in reference::LAB_SAMPLES.iter().zip(want) {
                        assert_near(
                            &format!("{name} {intent:?} bpc {bpc} at {lab:?}"),
                            sampler.sample_pdf_lab(lab[0], lab[1], lab[2]),
                            *want,
                            INK_TOLERANCE,
                        );
                    }
                }
            }
        }
    }
}
