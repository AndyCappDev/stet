// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Black Point Compensation (BPC) for CMYK→sRGB conversion.
//!
//! moxcms 0.8.1 ships its BPC implementation commented out. This module
//! lifts the algorithm into stet so K-heavy CMYK colors map to true sRGB
//! black instead of the source profile's "as-mapped" black (≈RGB(55, 53, 53)
//! for Ghostscript's `default_cmyk.icc`).
//!
//! # Pipeline
//!
//! moxcms outputs sRGB (D65). We post-correct in PCS XYZ-D50 per Adobe Tech
//! Note #5188:
//!
//! ```text
//! sRGB → linear sRGB → XYZ-D65 → XYZ-D50 → BPC shift → XYZ-D65 → linear → sRGB
//! ```
//!
//! Per-axis shift parameters (`ax`, `bx`, etc.) map the source black point
//! `sbp` to the destination black point `dbp = (0, 0, 0)` (sRGB's true zero
//! black) while leaving the D50 white point invariant.
//!
//! # Accuracy vs. lcms2
//!
//! This implementation closes most of the gap between stet's pre-fix
//! K=1 → RGB(55, 53, 53) and Adobe Acrobat's K=1 → RGB(35, 31, 32)
//! (with `default_cmyk.icc` as the source profile). The residual ~8 RGB
//! levels stems from differences in how moxcms's sRGB B2A handles very
//! dark XYZ inputs vs. lcms2's reference implementation — the BPC math
//! itself matches Adobe TN #5188 exactly, and the source-black detection
//! returns the same XYZ-D50 value either via sRGB round-trip or via a
//! direct Lab-as-XYZ destination probe.
//!
//! # References
//! - Adobe Tech Note #5188 — Black Point Compensation
//! - moxcms 0.8.1 source — `src/conversions/bpc.rs` (commented-out reference)
//! - ICC v4 spec, §6.3.4 (PCS encoding) and §9.2.21 (`bkpt` tag)

use moxcms::TransformExecutor;

/// D50 reference white point in PCS XYZ (`Xn`, `Yn`, `Zn` with `Yn = 1.0`).
/// Matches the ICC PCS illuminant.
pub const WP_D50: [f64; 3] = [0.96422, 1.0, 0.82521];

/// Linear-sRGB → XYZ-D65 (sRGB primaries with D65 white point).
const RGB_TO_XYZ_D65: [[f64; 3]; 3] = [
    [0.4124564, 0.3575761, 0.1804375],
    [0.2126729, 0.7151522, 0.0721750],
    [0.0193339, 0.1191920, 0.9503041],
];

/// XYZ-D65 → linear sRGB (inverse of `RGB_TO_XYZ_D65`).
const XYZ_D65_TO_RGB: [[f64; 3]; 3] = [
    [3.2404542, -1.5371385, -0.4985314],
    [-0.9692660, 1.8760108, 0.0415560],
    [0.0556434, -0.2040259, 1.0572252],
];

/// Bradford chromatic adaptation D65 → D50.
const D65_TO_D50: [[f64; 3]; 3] = [
    [1.0478112, 0.0228866, -0.0501270],
    [0.0295424, 0.9904844, -0.0170491],
    [-0.0092345, 0.0150436, 0.7521316],
];

/// Bradford chromatic adaptation D50 → D65 (inverse of `D65_TO_D50`).
const D50_TO_D65: [[f64; 3]; 3] = [
    [0.9555766, -0.0230393, 0.0631636],
    [-0.0282895, 1.0099416, 0.0210077],
    [0.0122982, -0.0204830, 1.3299098],
];

/// CIE Lab δ constant (`6/29`). `δ³` is the threshold below which the
/// linear approximation kicks in.
const LAB_DELTA: f64 = 6.0 / 29.0;

/// Cached per-axis BPC parameters for a single source/destination pair.
///
/// Computed once per profile at registration time. Apply per pixel via
/// [`apply_bpc_xyz_d50`] (or the higher-level [`apply_bpc_f64`] /
/// [`apply_bpc_rgb_u8`] wrappers).
#[derive(Clone, Copy, Debug)]
pub struct BpcParams {
    pub ax: f64,
    pub ay: f64,
    pub az: f64,
    pub bx: f64,
    pub by: f64,
    pub bz: f64,
}

impl BpcParams {
    /// Identity BPC (no shift). Useful as a sentinel when callers prefer to
    /// store the params unconditionally.
    #[inline]
    pub fn identity() -> Self {
        Self {
            ax: 1.0,
            ay: 1.0,
            az: 1.0,
            bx: 0.0,
            by: 0.0,
            bz: 0.0,
        }
    }
}

/// Compute per-axis BPC parameters mapping `sbp → dbp` while pinning `wp →
/// wp`. All three points must be in the same XYZ-D50 PCS.
///
/// Returns identity params if `sbp` coincides with `wp` on any axis (would
/// otherwise divide by zero).
pub fn compute_bpc_params(sbp: [f64; 3], dbp: [f64; 3], wp: [f64; 3]) -> BpcParams {
    let tx = sbp[0] - wp[0];
    let ty = sbp[1] - wp[1];
    let tz = sbp[2] - wp[2];
    if tx.abs() < 1e-12 || ty.abs() < 1e-12 || tz.abs() < 1e-12 {
        return BpcParams::identity();
    }
    BpcParams {
        ax: (dbp[0] - wp[0]) / tx,
        ay: (dbp[1] - wp[1]) / ty,
        az: (dbp[2] - wp[2]) / tz,
        bx: -wp[0] * (dbp[0] - sbp[0]) / tx,
        by: -wp[1] * (dbp[1] - sbp[1]) / ty,
        bz: -wp[2] * (dbp[2] - sbp[2]) / tz,
    }
}

/// Apply BPC to an XYZ-D50 colour: `X' = ax·X + bx` per axis.
#[inline]
pub fn apply_bpc_xyz_d50(xyz: [f64; 3], p: &BpcParams) -> [f64; 3] {
    [
        p.ax * xyz[0] + p.bx,
        p.ay * xyz[1] + p.by,
        p.az * xyz[2] + p.bz,
    ]
}

/// Apply BPC to an sRGB colour expressed as `f64` in `[0, 1]`. Round-trips
/// through linear → XYZ-D65 → XYZ-D50 → shift → XYZ-D65 → linear → sRGB.
pub fn apply_bpc_f64(rgb: [f64; 3], p: &BpcParams) -> [f64; 3] {
    let lin = [
        srgb_to_linear(rgb[0]),
        srgb_to_linear(rgb[1]),
        srgb_to_linear(rgb[2]),
    ];
    let xyz_d65 = matmul3(&RGB_TO_XYZ_D65, lin);
    let xyz_d50 = matmul3(&D65_TO_D50, xyz_d65);
    let shifted = apply_bpc_xyz_d50(xyz_d50, p);
    let xyz_d65_out = matmul3(&D50_TO_D65, shifted);
    let lin_out = matmul3(&XYZ_D65_TO_RGB, xyz_d65_out);
    [
        linear_to_srgb(lin_out[0]).clamp(0.0, 1.0),
        linear_to_srgb(lin_out[1]).clamp(0.0, 1.0),
        linear_to_srgb(lin_out[2]).clamp(0.0, 1.0),
    ]
}

/// Apply BPC to an 8-bit packed sRGB triple. Used by the CLUT-bake path
/// (commit 3) so per-pixel runtime cost stays at zero.
pub fn apply_bpc_rgb_u8(rgb: [u8; 3], p: &BpcParams) -> [u8; 3] {
    let f = apply_bpc_f64(
        [
            rgb[0] as f64 / 255.0,
            rgb[1] as f64 / 255.0,
            rgb[2] as f64 / 255.0,
        ],
        p,
    );
    [
        (f[0] * 255.0).round().clamp(0.0, 255.0) as u8,
        (f[1] * 255.0).round().clamp(0.0, 255.0) as u8,
        (f[2] * 255.0).round().clamp(0.0, 255.0) as u8,
    ]
}

/// Detect the "as-mapped" source black point for a CMYK profile: run
/// `(1, 1, 1, 1)` through the supplied 8-bit sRGB transform, convert the
/// result back to XYZ-D50, then apply moxcms's neutralising clamp
/// (`a = b = 0`, `L ≤ 50`). Returns the resulting XYZ-D50 black point.
///
/// The clamp matches moxcms's reference implementation. It guards against
/// pathological profiles whose ink-black is non-neutral or unrealistically
/// light, both of which would over-darken the output if used directly.
pub fn detect_source_black_point(transform_8bit: &dyn TransformExecutor<u8>) -> Option<[f64; 3]> {
    let src = [255u8; 4];
    let mut dst = [0u8; 3];
    transform_8bit.transform(&src, &mut dst).ok()?;
    let rgb = [
        dst[0] as f64 / 255.0,
        dst[1] as f64 / 255.0,
        dst[2] as f64 / 255.0,
    ];
    let lin = [
        srgb_to_linear(rgb[0]),
        srgb_to_linear(rgb[1]),
        srgb_to_linear(rgb[2]),
    ];
    let xyz_d65 = matmul3(&RGB_TO_XYZ_D65, lin);
    let mut xyz_d50 = matmul3(&D65_TO_D50, xyz_d65);
    let mut lab = xyz_d50_to_lab(xyz_d50);
    lab[1] = 0.0;
    lab[2] = 0.0;
    if lab[0] > 50.0 {
        lab[0] = 50.0;
    }
    xyz_d50 = lab_to_xyz_d50(lab);
    Some(xyz_d50)
}

/// sRGB transfer function (gamma decode) — companding curve from the IEC
/// 61966-2-1 standard.
#[inline]
pub fn srgb_to_linear(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Inverse sRGB transfer function (gamma encode).
#[inline]
pub fn linear_to_srgb(c: f64) -> f64 {
    if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// XYZ-D50 → CIE Lab using the ICC PCS reference white.
pub fn xyz_d50_to_lab(xyz: [f64; 3]) -> [f64; 3] {
    let fx = lab_f(xyz[0] / WP_D50[0]);
    let fy = lab_f(xyz[1] / WP_D50[1]);
    let fz = lab_f(xyz[2] / WP_D50[2]);
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// CIE Lab → XYZ-D50 (inverse of [`xyz_d50_to_lab`]).
pub fn lab_to_xyz_d50(lab: [f64; 3]) -> [f64; 3] {
    let fy = (lab[0] + 16.0) / 116.0;
    let fx = lab[1] / 500.0 + fy;
    let fz = fy - lab[2] / 200.0;
    [
        WP_D50[0] * lab_f_inv(fx),
        WP_D50[1] * lab_f_inv(fy),
        WP_D50[2] * lab_f_inv(fz),
    ]
}

#[inline]
fn lab_f(t: f64) -> f64 {
    let d3 = LAB_DELTA * LAB_DELTA * LAB_DELTA;
    if t > d3 {
        t.cbrt()
    } else {
        t / (3.0 * LAB_DELTA * LAB_DELTA) + 4.0 / 29.0
    }
}

#[inline]
fn lab_f_inv(t: f64) -> f64 {
    if t > LAB_DELTA {
        t * t * t
    } else {
        3.0 * LAB_DELTA * LAB_DELTA * (t - 4.0 / 29.0)
    }
}

#[inline]
fn matmul3(m: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() < tol
    }

    fn rgb_approx_eq(a: [f64; 3], b: [f64; 3], tol: f64) -> bool {
        approx_eq(a[0], b[0], tol) && approx_eq(a[1], b[1], tol) && approx_eq(a[2], b[2], tol)
    }

    #[test]
    fn srgb_linear_round_trip() {
        for v in [0.0, 0.04, 0.1, 0.5, 0.9, 1.0] {
            let r = linear_to_srgb(srgb_to_linear(v));
            assert!(approx_eq(r, v, 1e-9), "v={v} round={r}");
        }
    }

    #[test]
    fn srgb_to_xyz_round_trip() {
        // White → linear → XYZ-D65 → linear → sRGB ≈ white
        for color in [
            [1.0, 1.0, 1.0],
            [0.5, 0.5, 0.5],
            [0.8, 0.2, 0.4],
            [0.0, 0.0, 0.0],
        ] {
            let lin = color.map(srgb_to_linear);
            let xyz = matmul3(&RGB_TO_XYZ_D65, lin);
            let back_lin = matmul3(&XYZ_D65_TO_RGB, xyz);
            let back = back_lin.map(linear_to_srgb);
            assert!(
                rgb_approx_eq(color, back, 1e-6),
                "color={color:?} back={back:?}"
            );
        }
    }

    #[test]
    fn bradford_round_trip() {
        // D65 white in XYZ
        for xyz in [
            [0.95047, 1.0, 1.08883],
            [0.5, 0.5, 0.5],
            [0.1, 0.05, 0.2],
        ] {
            let d50 = matmul3(&D65_TO_D50, xyz);
            let back = matmul3(&D50_TO_D65, d50);
            assert!(rgb_approx_eq(xyz, back, 1e-6), "xyz={xyz:?} back={back:?}");
        }
    }

    #[test]
    fn lab_xyz_round_trip() {
        for xyz in [
            WP_D50,
            [0.5, 0.5, 0.5],
            [0.04, 0.04, 0.04],
            [0.18, 0.18, 0.18],
            [0.001, 0.001, 0.001],
        ] {
            let lab = xyz_d50_to_lab(xyz);
            let back = lab_to_xyz_d50(lab);
            assert!(
                rgb_approx_eq(xyz, back, 1e-9),
                "xyz={xyz:?} lab={lab:?} back={back:?}"
            );
        }
    }

    #[test]
    fn lab_clamp_caps_lightness() {
        // A "too light" black with non-neutral chroma. After detect_source's
        // clamp logic — a=b=0 and L≤50 — we want neutral and ≤50.
        let xyz = [0.5, 0.5, 0.4];
        let mut lab = xyz_d50_to_lab(xyz);
        lab[1] = 0.0;
        lab[2] = 0.0;
        if lab[0] > 50.0 {
            lab[0] = 50.0;
        }
        assert!(lab[0] <= 50.0);
        assert_eq!(lab[1], 0.0);
        assert_eq!(lab[2], 0.0);
        let back = lab_to_xyz_d50(lab);
        // L=50 → Y ≈ 0.184. Neutral chroma → X/Xn = Y/Yn = Z/Zn approximately.
        assert!(approx_eq(back[1], 0.184, 0.01), "back Y={}", back[1]);
    }

    #[test]
    fn bpc_params_white_invariant() {
        // BPC must leave the D50 white point untouched.
        let sbp = [0.0072, 0.0067, 0.0064];
        let dbp = [0.0, 0.0, 0.0];
        let p = compute_bpc_params(sbp, dbp, WP_D50);
        let out = apply_bpc_xyz_d50(WP_D50, &p);
        assert!(rgb_approx_eq(out, WP_D50, 1e-9), "out={out:?}");
    }

    #[test]
    fn bpc_params_map_sbp_to_dbp() {
        // BPC must map source black to destination black.
        let sbp = [0.0072, 0.0067, 0.0064];
        let dbp = [0.0, 0.0, 0.0];
        let p = compute_bpc_params(sbp, dbp, WP_D50);
        let out = apply_bpc_xyz_d50(sbp, &p);
        assert!(rgb_approx_eq(out, dbp, 1e-9), "out={out:?}");
    }

    #[test]
    fn bpc_identity_when_sbp_equals_wp() {
        // Degenerate input must not divide by zero.
        let p = compute_bpc_params(WP_D50, [0.0; 3], WP_D50);
        // identity() leaves any value untouched
        let out = apply_bpc_xyz_d50([0.5, 0.5, 0.5], &p);
        assert!(rgb_approx_eq(out, [0.5, 0.5, 0.5], 1e-12));
    }

    #[test]
    fn apply_bpc_f64_white_anchored() {
        // White must round-trip back to white (within sRGB→XYZ→sRGB drift).
        let sbp = [0.0072, 0.0067, 0.0064];
        let p = compute_bpc_params(sbp, [0.0; 3], WP_D50);
        let out = apply_bpc_f64([1.0, 1.0, 1.0], &p);
        // Pure-white sRGB maps to D65 white, adapt to D50, BPC fixes it,
        // adapt back. Drift comes from finite-precision matrices.
        assert!(rgb_approx_eq(out, [1.0, 1.0, 1.0], 0.005), "out={out:?}");
    }

    #[test]
    fn apply_bpc_f64_darkens_grays() {
        // A mid-grey on the source-black axis should darken under BPC when
        // the source black is non-zero.
        let sbp = [0.012, 0.013, 0.011];
        let p = compute_bpc_params(sbp, [0.0; 3], WP_D50);
        let original = [0.3, 0.3, 0.3];
        let out = apply_bpc_f64(original, &p);
        assert!(out[1] < original[1], "expected darker, got {}", out[1]);
    }

    #[test]
    fn apply_bpc_rgb_u8_matches_f64_path() {
        let sbp = [0.0072, 0.0067, 0.0064];
        let p = compute_bpc_params(sbp, [0.0; 3], WP_D50);
        // Use matching inputs: 128/255 ≠ 0.5 exactly, so feeding 0.5 to the
        // f64 path and 128 to the u8 path produces values that differ in the
        // last bit. Build the u8-equivalent f64 input by exact division.
        let v = 128.0 / 255.0;
        let f = apply_bpc_f64([v, v, v], &p);
        let u = apply_bpc_rgb_u8([128, 128, 128], &p);
        let f_u = [
            (f[0] * 255.0).round() as u8,
            (f[1] * 255.0).round() as u8,
            (f[2] * 255.0).round() as u8,
        ];
        assert_eq!(f_u, u);
    }
}
