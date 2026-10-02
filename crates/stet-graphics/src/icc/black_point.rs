// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The black points black-point compensation maps between, for a profile
//! and a rendering intent, as lcms2 detects them and Ghostscript runs it:
//! a CMYK, RGB or Gray source's (`cmsDetectBlackPoint`, [`detect`],
//! [`detect_rgb`], [`detect_gray`]) and a CMYK output intent's as the
//! destination of the proofing chain (`cmsDetectDestinationBlackPoint`,
//! [`detect_destination`]); and, from them, the compensation lcms2 applies
//! on the chain's first stage ([`chain_compensation`]). For a CMYK source:
//!
//! | case | black point |
//! |---|---|
//! | ICC v4, perceptual or saturation | the v4 perceptual black, fixed |
//! | relative colorimetric, output-class (`prtr`) profile | L\*=0 through `B2A0`, back through `A2B1` |
//! | otherwise | 400% ink through the intent's table |
//! | the table that black needs is missing | none: no compensation |
//!
//! The round trip lands on the profile's **ink-limited** black, the
//! darkest colour the press is allowed to make, which is lighter than
//! 400% ink. For an ICC v4 profile lcms2 starts it from the perceptual
//! black instead of L\*=0, because it forces compensation on the round
//! trip's perceptual leg. Absolute colorimetric is rendered as relative
//! colorimetric and takes its black point.
//!
//! One departure: the `bkpt` tag is ignored, as Ghostscript's lcms2 build
//! ignores it.

use moxcms::{ColorProfile, DataColorSpace, RenderingIntent};

use super::bpc::{BpcParams, WP_D50, compute_bpc_params, lab_to_xyz_d50, xyz_d50_to_lab};
use super::hand_rolled;

/// lcms2's `cmsPERCEPTUAL_BLACK_{X,Y,Z}`: the black of the ICC v4
/// perceptual reference medium, XYZ-D50 (L\* 3.14).
const PERCEPTUAL_BLACK: [f64; 3] = [0.00336, 0.0034731, 0.00287];

/// Where black-point compensation takes a CMYK table's black from.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum SourceBlack {
    /// 400% ink through the table being converted — lcms2's
    /// `BlackPointAsDarkerColorant`. Whoever bakes the table samples it
    /// from that table.
    DarkerColorant,
    /// A black point the profile fixes, XYZ-D50 with white Y = 1.
    Xyz([f64; 3]),
}

/// The black point lcms2 detects for the CMYK profile `profile`, whose raw
/// bytes are `icc`, under `intent`. `None` is lcms2's zero black point: no
/// compensation.
pub(super) fn detect(
    profile: &ColorProfile,
    icc: &[u8],
    intent: RenderingIntent,
) -> Option<SourceBlack> {
    // moxcms keeps neither the device class nor the version.
    let class = icc.get(12..16)?;
    if matches!(class, b"link" | b"abst" | b"nmcl") {
        return None;
    }
    let v4 = icc.get(8).is_some_and(|&major| major >= 4);
    let intent = match intent {
        RenderingIntent::AbsoluteColorimetric => RenderingIntent::RelativeColorimetric,
        other => other,
    };
    if v4
        && matches!(
            intent,
            RenderingIntent::Perceptual | RenderingIntent::Saturation
        )
    {
        return Some(SourceBlack::Xyz(PERCEPTUAL_BLACK));
    }
    if intent == RenderingIntent::RelativeColorimetric
        && class == b"prtr"
        && profile.color_space == DataColorSpace::Cmyk
    {
        return ink_limited_black(profile, icc, v4);
    }
    // lcms2 assumes black is zero when the profile has no table for the
    // intent ("intent not supported").
    let table = match intent {
        RenderingIntent::Perceptual => &profile.lut_a_to_b_perceptual,
        RenderingIntent::Saturation => &profile.lut_a_to_b_saturation,
        _ => &profile.lut_a_to_b_colorimetric,
    };
    table.as_ref().map(|_| SourceBlack::DarkerColorant)
}

/// The black point lcms2 detects for a TRC-only Gray profile
/// (`BlackPointAsDarkerColorant`): gray 0 through the tone curve, made
/// neutral and clipped to L\* 50. The same for every intent, as the curve
/// is; an ICC v4 profile under perceptual or saturation takes the darker
/// colorant too, being a matrix-shaper. `None` is lcms2's zero black point,
/// or a Gray profile that is not TRC-only.
pub(super) fn detect_gray(profile: &ColorProfile, icc: &[u8]) -> Option<[f64; 3]> {
    let class = icc.get(12..16)?;
    if matches!(class, b"link" | b"abst" | b"nmcl") {
        return None;
    }
    let y = hand_rolled::GrayTrc::new(profile)?.y(0.0);
    let l_star = xyz_d50_to_lab([0.0, y, 0.0])[0];
    Some(lab_to_xyz_d50([l_star.min(50.0), 0.0, 0.0]))
}

/// [`detect`] as the XYZ it stands for. `None` is lcms2's zero black point,
/// or a table that black needs which stet cannot read.
pub(super) fn detect_xyz(
    profile: &ColorProfile,
    icc: &[u8],
    intent: RenderingIntent,
) -> Option<[f64; 3]> {
    resolve(profile, icc, intent, detect(profile, icc, intent)?)
}

/// The black point lcms2 detects for an RGB source
/// (`BlackPointAsDarkerColorant`): RGB 0 through the source as the chain
/// reads it for `intent` — its table, or its tone curves and matrix — made
/// neutral and clipped to L\* 50. For an ICC v4 profile under perceptual
/// or saturation lcms2 takes relative colorimetric's if the profile is a
/// matrix-shaper, and the v4 perceptual black if not. `None` is lcms2's
/// zero black point, or a source the chain cannot read.
pub(super) fn detect_rgb(
    profile: &ColorProfile,
    icc: &[u8],
    intent: RenderingIntent,
) -> Option<[f64; 3]> {
    let class = icc.get(12..16)?;
    if matches!(class, b"link" | b"abst" | b"nmcl") {
        return None;
    }
    let v4 = icc.get(8).is_some_and(|&major| major >= 4);
    let mut intent = match intent {
        RenderingIntent::AbsoluteColorimetric => RenderingIntent::RelativeColorimetric,
        other => other,
    };
    if v4
        && matches!(
            intent,
            RenderingIntent::Perceptual | RenderingIntent::Saturation
        )
    {
        if !profile.is_matrix_shaper() {
            return Some(PERCEPTUAL_BLACK);
        }
        intent = RenderingIntent::RelativeColorimetric;
    }
    // lcms2 assumes black is zero when the profile supports the intent
    // neither by a table of its own nor as a matrix-shaper
    // (`cmsIsIntentSupported`), though the chain then reads `A2B0`.
    let own_table = match intent {
        RenderingIntent::Perceptual => &profile.lut_a_to_b_perceptual,
        RenderingIntent::Saturation => &profile.lut_a_to_b_saturation,
        _ => &profile.lut_a_to_b_colorimetric,
    };
    if own_table.is_none() && !profile.is_matrix_shaper() {
        return None;
    }
    let lab = hand_rolled::rgb_to_lab(profile, icc, intent, [0.0; 3])?;
    Some(lab_to_xyz_d50([lab[0].min(50.0), 0.0, 0.0]))
}

/// The black-point compensation lcms2 applies on the proofing chain's first
/// stage under `intent`, from a source whose black is `source_black` (zero
/// when `None`) into the output intent `oi`, whose raw bytes are `oi_icc`:
///
/// - only when `enabled` (`--bpc`), or — lcms2 forces it — when the output
///   intent is ICC v4 and the intent perceptual or saturation;
/// - never under absolute colorimetric;
/// - to the output intent's black as a destination, [`detect_destination`];
/// - none when the two blacks are equal, or so close that lcms2 drops the
///   matrix as an empty layer (`IsEmptyLayer`): into a v2 output intent
///   under perceptual, typically, whose black is L\* 0.2.
pub(super) fn chain_compensation(
    source_black: Option<[f64; 3]>,
    oi: &ColorProfile,
    oi_icc: &[u8],
    intent: RenderingIntent,
    enabled: bool,
) -> Option<BpcParams> {
    if intent == RenderingIntent::AbsoluteColorimetric {
        return None;
    }
    let v4 = oi_icc.get(8).is_some_and(|&major| major >= 4);
    let forced = v4 && intent != RenderingIntent::RelativeColorimetric;
    if !enabled && !forced {
        return None;
    }
    let source = source_black.unwrap_or([0.0; 3]);
    let destination = detect_destination(oi, oi_icc, intent).unwrap_or([0.0; 3]);
    if source == destination {
        return None;
    }
    let p = compute_bpc_params(source, destination, WP_D50);
    // lcms2 adds a matrix stage only when Σ|m − I| + Σ|offset| ≥ 0.002.
    let change = (p.ax - 1.0).abs()
        + (p.ay - 1.0).abs()
        + (p.az - 1.0).abs()
        + p.bx.abs()
        + p.by.abs()
        + p.bz.abs();
    (change >= 0.002).then_some(p)
}

/// The black point lcms2 detects for the CMYK output profile `profile`,
/// whose raw bytes are `icc`, as the *destination* of a conversion under
/// `intent` (`cmsDetectDestinationBlackPoint`). `None` is lcms2's zero
/// black point.
///
/// - ICC v4 under perceptual or saturation: the v4 perceptual black.
/// - No B2A table for the intent (lcms2 does not fall back to `B2A0`
///   here): the profile's black point as a source, [`detect`].
/// - Otherwise a round trip of L\* 0–100 through the intent's B2A and back
///   through `A2B1`, starting at a\*/b\* of the profile's own black point
///   for relative colorimetric and of Lab 0 otherwise. Where relative
///   colorimetric's round trip is straight through the mid-tones its black
///   is that black point; otherwise a quadratic fitted to the shadows finds
///   where the round trip leaves black.
///
/// A profile whose round-trip tables stet cannot read has no destination
/// black point here.
pub(super) fn detect_destination(
    profile: &ColorProfile,
    icc: &[u8],
    intent: RenderingIntent,
) -> Option<[f64; 3]> {
    let v4 = icc.get(8).is_some_and(|&major| major >= 4);
    let intent = match intent {
        RenderingIntent::AbsoluteColorimetric => RenderingIntent::RelativeColorimetric,
        other => other,
    };
    if v4
        && matches!(
            intent,
            RenderingIntent::Perceptual | RenderingIntent::Saturation
        )
    {
        return Some(PERCEPTUAL_BLACK);
    }
    let b2a = match intent {
        RenderingIntent::Perceptual => &profile.lut_b_to_a_perceptual,
        RenderingIntent::Saturation => &profile.lut_b_to_a_saturation,
        _ => &profile.lut_b_to_a_colorimetric,
    };
    if b2a.is_none() || profile.color_space != DataColorSpace::Cmyk {
        return resolve(profile, icc, intent, detect(profile, icc, intent)?);
    }
    let initial = if intent == RenderingIntent::RelativeColorimetric {
        xyz_d50_to_lab(resolve(
            profile,
            icc,
            intent,
            detect(profile, icc, intent)?,
        )?)
    } else {
        [0.0; 3]
    };
    let trip = hand_rolled::RoundTrip::new(profile, icc, intent)?;
    let (a, b) = (initial[1].clamp(-50.0, 50.0), initial[2].clamp(-50.0, 50.0));
    let in_ramp: [f64; 256] = std::array::from_fn(|l| l as f64 * 100.0 / 255.0);
    let mut out_ramp = in_ramp.map(|l| trip.run([l, a, b]).1[0]);
    for l in (1..255).rev() {
        out_ramp[l] = out_ramp[l].min(out_ramp[l + 1]);
    }
    let (min_l, max_l) = (out_ramp[0], out_ramp[255]);
    // A round trip that does not rise has no black to find; nor has one
    // that is not a number.
    if min_l.partial_cmp(&max_l) != Some(std::cmp::Ordering::Less) {
        return None;
    }
    if intent == RenderingIntent::RelativeColorimetric
        && in_ramp
            .iter()
            .zip(&out_ramp)
            .all(|(&i, &o)| i <= min_l + 0.2 * (max_l - min_l) || (i - o).abs() < 4.0)
    {
        return Some(lab_to_xyz_d50(initial));
    }
    let (lo, hi) = if intent == RenderingIntent::RelativeColorimetric {
        (0.1, 0.5)
    } else {
        (0.03, 0.25)
    };
    let shadows: Vec<(f64, f64)> = in_ramp
        .iter()
        .zip(&out_ramp)
        .map(|(&i, &o)| (i, (o - min_l) / (max_l - min_l)))
        .filter(|&(_, y)| y >= lo && y < hi)
        .collect();
    if shadows.len() < 3 {
        return None;
    }
    let l_star = quadratic_root(&shadows).max(0.0);
    Some(lab_to_xyz_d50([l_star, initial[1], initial[2]]))
}

/// lcms2's `RootOfLeastSquaresFitQuadraticCurve`: the least-squares
/// quadratic through `points`, and the root of it lcms2 takes, clipped to
/// L\* 0–50. A straight fit gives 0: lcms2 clamps it with `min` and `max`
/// the wrong way round, and stet follows lcms2.
fn quadratic_root(points: &[(f64, f64)]) -> f64 {
    if points.len() < 4 {
        return 0.0;
    }
    let n = points.len() as f64;
    let (mut sx, mut sx2, mut sx3, mut sx4, mut sy, mut syx, mut syx2) =
        (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    for &(x, y) in points {
        sx += x;
        sx2 += x * x;
        sx3 += x * x * x;
        sx4 += x * x * x * x;
        sy += y;
        syx += y * x;
        syx2 += y * x * x;
    }
    let Some([c, b, a]) = solve3(
        [[n, sx, sx2], [sx, sx2, sx3], [sx2, sx3, sx4]],
        [sy, syx, syx2],
    ) else {
        return 0.0;
    };
    if a.abs() < 1.0e-10 {
        if b.abs() < 1.0e-10 {
            return 0.0;
        }
        return f64::min(0.0, f64::max(50.0, -c / b));
    }
    let d = b * b - 4.0 * a * c;
    if d <= 0.0 {
        return 0.0;
    }
    ((-b + d.sqrt()) / (2.0 * a)).clamp(0.0, 50.0)
}

/// `m · x = v` by lcms2's `_cmsMAT3solve`: the inverse, refused when the
/// determinant is under its tolerance.
fn solve3(m: [[f64; 3]; 3], v: [f64; 3]) -> Option<[f64; 3]> {
    let c0 = m[1][1] * m[2][2] - m[1][2] * m[2][1];
    let c1 = -m[1][0] * m[2][2] + m[1][2] * m[2][0];
    let c2 = m[1][0] * m[2][1] - m[1][1] * m[2][0];
    let det = m[0][0] * c0 + m[0][1] * c1 + m[0][2] * c2;
    if det.abs() < 0.0001 {
        return None;
    }
    let inv = [
        [
            c0 / det,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) / det,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) / det,
        ],
        [
            c1 / det,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) / det,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) / det,
        ],
        [
            c2 / det,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) / det,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) / det,
        ],
    ];
    Some(inv.map(|row| row[0] * v[0] + row[1] * v[1] + row[2] * v[2]))
}

/// The XYZ a detected CMYK black point stands for: the darker colorant is
/// 400% ink through the intent's table, as lcms2 computes it.
fn resolve(
    profile: &ColorProfile,
    icc: &[u8],
    intent: RenderingIntent,
    black: SourceBlack,
) -> Option<[f64; 3]> {
    match black {
        SourceBlack::Xyz(xyz) => Some(xyz),
        SourceBlack::DarkerColorant => {
            let lab = hand_rolled::table_black(profile, icc, intent)?;
            Some(lab_to_xyz_d50([lab[0].min(50.0), 0.0, 0.0]))
        }
    }
}

/// lcms2's `BlackPointUsingPerceptualBlack`: the round trip's landing
/// point, made neutral and clipped to L\* 50.
fn ink_limited_black(profile: &ColorProfile, icc: &[u8], v4: bool) -> Option<SourceBlack> {
    // Perceptual must be supported as input, and lcms2 cannot build the
    // round trip without `B2A0`; either way the black point is zero.
    profile.lut_a_to_b_perceptual.as_ref()?;
    profile.lut_b_to_a_perceptual.as_ref()?;
    let start = if v4 {
        xyz_d50_to_lab(PERCEPTUAL_BLACK)
    } else {
        [0.0; 3]
    };
    let Some((ink, lab)) = hand_rolled::perceptual_round_trip(profile, icc, start) else {
        return Some(SourceBlack::DarkerColorant);
    };
    // A `B2A0` that reaches 400% ink lands on the darker colorant, through
    // the table relative colorimetric reads anyway. Saying so lets an
    // intent with the same table and black share its bake.
    if ink.iter().all(|&v| v >= 1.0) {
        return Some(SourceBlack::DarkerColorant);
    }
    Some(SourceBlack::Xyz(lab_to_xyz_d50([
        lab[0].min(50.0),
        0.0,
        0.0,
    ])))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::icc::bpc::xyz_d50_to_lab;
    use RenderingIntent::*;

    // lcms2's answers for the generated test profiles; this module reads
    // the black points.
    #[allow(dead_code)]
    mod reference {
        include!("../../tests/data/cmyk_intent/reference.rs");
    }

    macro_rules! profile {
        ($name:literal) => {
            include_bytes!(concat!("../../tests/data/cmyk_intent/", $name, ".icc")).as_slice()
        };
    }

    /// A generated profile, its bytes, and lcms2's black point for each
    /// intent.
    type Reference = (&'static str, &'static [u8], [[f64; 3]; 3]);

    const PROFILES: [Reference; 13] = [
        ("split", profile!("split"), reference::SPLIT_BLACK_POINT),
        (
            "split_sat",
            profile!("split_sat"),
            reference::SPLIT_SAT_BLACK_POINT,
        ),
        (
            "split_lut8",
            profile!("split_lut8"),
            reference::SPLIT_LUT8_BLACK_POINT,
        ),
        ("same", profile!("same"), reference::SAME_BLACK_POINT),
        (
            "inklimit",
            profile!("inklimit"),
            reference::INKLIMIT_BLACK_POINT,
        ),
        (
            "inklimit_lut8",
            profile!("inklimit_lut8"),
            reference::INKLIMIT_LUT8_BLACK_POINT,
        ),
        (
            "inklimit_scnr",
            profile!("inklimit_scnr"),
            reference::INKLIMIT_SCNR_BLACK_POINT,
        ),
        (
            "inklimit_v4",
            profile!("inklimit_v4"),
            reference::INKLIMIT_V4_BLACK_POINT,
        ),
        (
            "inklimit_no_a2b0",
            profile!("inklimit_no_a2b0"),
            reference::INKLIMIT_NO_A2B0_BLACK_POINT,
        ),
        // ICC v4 `lutAToBType` / `lutBToAType`: relative colorimetric's
        // black is the round trip through them.
        ("mab", profile!("mab"), reference::MAB_BLACK_POINT),
        // An XYZ PCS, whose round trip goes through an XYZ-indexed `B2A0`
        // with a matrix (`lut16Type`) or a matrix element (`mBA`).
        ("xyz_v4", profile!("xyz_v4"), reference::XYZ_V4_BLACK_POINT),
        (
            "xyz_mab",
            profile!("xyz_mab"),
            reference::XYZ_MAB_BLACK_POINT,
        ),
        // A Lab-indexed `B2A0` with a matrix, which lcms2 applies too.
        (
            "inklimit_matrix",
            profile!("inklimit_matrix"),
            reference::INKLIMIT_MATRIX_BLACK_POINT,
        ),
    ];

    /// Every generated profile, every intent: the black point is lcms2's,
    /// to a hundredth of an L\* — which covers the v4 perceptual black,
    /// the ink-limited round trip in `lut16Type` and `lut8Type`, v4's
    /// round trip from the perceptual black, the input class's 400%, and
    /// the zero of a missing table.
    #[test]
    fn black_point_is_lcms2s() {
        for (name, icc, want) in PROFILES {
            let profile = ColorProfile::new_from_slice(icc).unwrap();
            for (i, intent) in [Perceptual, RelativeColorimetric, Saturation]
                .into_iter()
                .enumerate()
            {
                let got = detect(&profile, icc, intent)
                    .and_then(|black| resolve(&profile, icc, intent, black))
                    .unwrap_or_default();
                let (l_got, l_want) = (xyz_d50_to_lab(got)[0], xyz_d50_to_lab(want[i])[0]);
                assert!(
                    (l_got - l_want).abs() < 0.01,
                    "{name} {intent:?}: L* {l_got}, lcms2 {l_want}"
                );
            }
        }
    }

    #[test]
    fn which_rule_applies() {
        let detect_in = |icc: &[u8], intent| {
            let profile = ColorProfile::new_from_slice(icc).unwrap();
            detect(&profile, icc, intent)
        };
        let inklimit = profile!("inklimit");
        assert!(matches!(
            detect_in(inklimit, RelativeColorimetric),
            Some(SourceBlack::Xyz(_))
        ));
        assert_eq!(
            detect_in(inklimit, Perceptual),
            Some(SourceBlack::DarkerColorant)
        );
        // Rendered as relative colorimetric, so its black point too.
        assert_eq!(
            detect_in(inklimit, AbsoluteColorimetric),
            detect_in(inklimit, RelativeColorimetric)
        );
        // No A2B2: lcms2 declines the intent, so no black to scale.
        assert_eq!(detect_in(inklimit, Saturation), None);
        // B2A0 reaching 400% ink is the darker colorant.
        assert_eq!(
            detect_in(profile!("split"), RelativeColorimetric),
            Some(SourceBlack::DarkerColorant)
        );
        assert_eq!(
            detect_in(profile!("inklimit_scnr"), RelativeColorimetric),
            Some(SourceBlack::DarkerColorant)
        );
        assert_eq!(
            detect_in(profile!("inklimit_no_a2b0"), RelativeColorimetric),
            None
        );
        let v4 = profile!("inklimit_v4");
        assert_eq!(
            detect_in(v4, Perceptual),
            Some(SourceBlack::Xyz(PERCEPTUAL_BLACK))
        );
        assert_eq!(
            detect_in(v4, Saturation),
            Some(SourceBlack::Xyz(PERCEPTUAL_BLACK))
        );
    }

    /// Output profiles and lcms2's destination black point for each intent.
    const DESTINATIONS: [Reference; 6] = [
        (
            "shadow",
            profile!("shadow"),
            reference::SHADOW_DESTINATION_BLACK_POINT,
        ),
        (
            "shadow_straight",
            profile!("shadow_straight"),
            reference::SHADOW_STRAIGHT_DESTINATION_BLACK_POINT,
        ),
        (
            "shadow_v4",
            profile!("shadow_v4"),
            reference::SHADOW_V4_DESTINATION_BLACK_POINT,
        ),
        (
            "inklimit",
            profile!("inklimit"),
            reference::INKLIMIT_DESTINATION_BLACK_POINT,
        ),
        (
            "split_sat",
            profile!("split_sat"),
            reference::SPLIT_SAT_DESTINATION_BLACK_POINT,
        ),
        (
            "mab",
            profile!("mab"),
            reference::MAB_DESTINATION_BLACK_POINT,
        ),
    ];

    /// The destination black point is lcms2's, to a hundredth of an L\*:
    /// fitted to curved shadows under every intent (`shadow`), relative
    /// colorimetric's own black point where its round trip is straight
    /// through the mid-tones (`shadow_straight`, L\* 26.4), the fixed v4
    /// perceptual black, and zero where the round trip is a straight line,
    /// as lcms2's fit returns.
    #[test]
    fn destination_black_point_is_lcms2s() {
        for (name, icc, want) in DESTINATIONS {
            let profile = ColorProfile::new_from_slice(icc).unwrap();
            for (i, intent) in [Perceptual, RelativeColorimetric, Saturation]
                .into_iter()
                .enumerate()
            {
                let got = detect_destination(&profile, icc, intent).unwrap_or_default();
                let (l_got, l_want) = (xyz_d50_to_lab(got)[0], xyz_d50_to_lab(want[i])[0]);
                assert!(
                    (l_got - l_want).abs() < 0.01,
                    "{name} {intent:?}: L* {l_got}, lcms2 {l_want}"
                );
            }
        }
        // The cases are distinct, or the test proves less than it says.
        let l = |xyz| xyz_d50_to_lab(xyz)[0];
        let shadow = reference::SHADOW_DESTINATION_BLACK_POINT;
        assert!(l(shadow[1]) > 20.0);
        assert!((l(reference::SHADOW_STRAIGHT_DESTINATION_BLACK_POINT[1]) - 26.4).abs() < 0.01);
        assert!(l(shadow[0]) > 20.0 && l(shadow[2]) > 10.0);
    }

    /// lcms2's quadratic root: a straight fit gives zero, whatever line it is.
    #[test]
    fn a_straight_fit_has_no_root() {
        let line: Vec<(f64, f64)> = (0..10).map(|i| (i as f64, 0.1 * i as f64 - 0.3)).collect();
        assert_eq!(quadratic_root(&line), 0.0);
        let parabola: Vec<(f64, f64)> = (0..10)
            .map(|i| (i as f64, (i as f64 - 4.0).powi(2) / 100.0 - 0.01))
            .collect();
        assert!((quadratic_root(&parabola) - 5.0).abs() < 1e-9);
    }

    /// A Gray profile's black is its tone curve at 0, the same under every
    /// intent.
    #[test]
    fn gray_black_point_is_lcms2s() {
        let icc = profile!("gray_trc");
        let profile = ColorProfile::new_from_slice(icc).unwrap();
        let got = detect_gray(&profile, icc).unwrap();
        for want in reference::GRAY_TRC_BLACK_POINT {
            let (l_got, l_want) = (xyz_d50_to_lab(got)[0], xyz_d50_to_lab(want)[0]);
            assert!((l_got - l_want).abs() < 0.01, "L* {l_got}, lcms2 {l_want}");
        }
    }

    /// A Gray source converts into an output intent as lcms2 converts it,
    /// with black-point compensation from the gray's black to the output
    /// intent's destination black, and without.
    #[test]
    fn gray_chain_stage1_matches_lcms() {
        for (name, gray_icc, icc, want) in [
            (
                "shadow",
                profile!("gray_trc"),
                profile!("shadow"),
                &reference::GRAY_CHAIN_SHADOW,
            ),
            (
                "inklimit",
                profile!("gray_trc"),
                profile!("inklimit"),
                &reference::GRAY_CHAIN_INKLIMIT,
            ),
            (
                "pure gamma",
                profile!("gray_gamma"),
                profile!("inklimit"),
                &reference::CHAIN_GRAY_GAMMA_INKLIMIT,
            ),
            // Into ICC v4 `lutBToAType` tables, compensated under perceptual
            // and saturation whatever the flag.
            (
                "mab",
                profile!("gray_trc"),
                profile!("mab"),
                &reference::GRAY_CHAIN_MAB,
            ),
            // Into an XYZ PCS.
            (
                "xyz_v4",
                profile!("gray_trc"),
                profile!("xyz_v4"),
                &reference::GRAY_CHAIN_XYZ_V4,
            ),
            (
                "xyz_mab",
                profile!("gray_trc"),
                profile!("xyz_mab"),
                &reference::GRAY_CHAIN_XYZ_MAB,
            ),
            // Compensating this black to zero is an empty layer to lcms2.
            (
                "near black",
                profile!("gray_near_black"),
                profile!("inklimit"),
                &reference::CHAIN_GRAY_NEAR_BLACK_INKLIMIT,
            ),
        ] {
            let gray = ColorProfile::new_from_slice(gray_icc).unwrap();
            let gray_black = detect_gray(&gray, gray_icc).unwrap();
            let oi = ColorProfile::new_from_slice(icc).unwrap();
            for (i, intent) in [Perceptual, RelativeColorimetric, Saturation]
                .into_iter()
                .enumerate()
            {
                for (bpc, want) in want[i].iter().enumerate() {
                    let params = chain_compensation(Some(gray_black), &oi, icc, intent, bpc == 1);
                    let stage1 = hand_rolled::HandRolledChainStage1Gray::new(
                        &gray, &oi, icc, intent, params,
                    )
                    .unwrap();
                    for (g, want) in reference::GRAY_SAMPLES.iter().zip(want) {
                        let got = stage1.sample(*g);
                        for (got, want) in got.iter().zip(want) {
                            assert!(
                                (got - want).abs() < 2e-4,
                                "{name} {intent:?} bpc {bpc} gray {g}: {got} vs lcms2 {want}"
                            );
                        }
                    }
                }
            }
        }
        // Compensation moves the ink, or the test proves nothing.
        let shadow = &reference::GRAY_CHAIN_SHADOW;
        assert_ne!(shadow[0][0], shadow[0][1]);
    }

    /// An RGB source's black point is lcms2's: RGB 0 through the source,
    /// except an ICC v4 profile without a matrix under perceptual and
    /// saturation, which takes the v4 perceptual black.
    #[test]
    fn rgb_black_point_is_lcms2s() {
        for (name, icc, want) in [
            (
                "rgb_gamma",
                profile!("rgb_gamma"),
                reference::RGB_GAMMA_BLACK_POINT,
            ),
            (
                "rgb_lut",
                profile!("rgb_lut"),
                reference::RGB_LUT_BLACK_POINT,
            ),
            (
                "rgb_lut_v4",
                profile!("rgb_lut_v4"),
                reference::RGB_LUT_V4_BLACK_POINT,
            ),
            ("srgb", profile!("srgb"), reference::SRGB_BLACK_POINT),
            (
                "rgb_mab",
                profile!("rgb_mab"),
                reference::RGB_MAB_BLACK_POINT,
            ),
            // An XYZ PCS: `lut16Type`, and ICC v4 `lutAToBType` with no
            // `A2B1`, which lcms2 finds no relative colorimetric black in.
            (
                "rgb_xyz",
                profile!("rgb_xyz"),
                reference::RGB_XYZ_BLACK_POINT,
            ),
            (
                "rgb_xyz_mab",
                profile!("rgb_xyz_mab"),
                reference::RGB_XYZ_MAB_BLACK_POINT,
            ),
        ] {
            let profile = ColorProfile::new_from_slice(icc).unwrap();
            for (i, intent) in [Perceptual, RelativeColorimetric, Saturation]
                .into_iter()
                .enumerate()
            {
                let got = detect_rgb(&profile, icc, intent).unwrap_or_default();
                for (got, want) in got.iter().zip(want[i]) {
                    assert!(
                        (got - want).abs() < 1e-5,
                        "{name} {intent:?}: {got:?} vs lcms2 {:?}",
                        want
                    );
                }
            }
        }
        // The v4 rule is exercised: a black that is not zero.
        assert_ne!(reference::RGB_LUT_V4_BLACK_POINT[0], [0.0; 3]);
    }

    /// When the proofing chain compensates, as lcms2 decides: by the flag,
    /// forced into an ICC v4 output intent under perceptual and saturation,
    /// never under absolute colorimetric, and not at all when the blacks
    /// are equal or the change is an empty layer.
    #[test]
    fn chain_compensation_is_lcms2s() {
        let shadow_icc = profile!("shadow");
        let shadow = ColorProfile::new_from_slice(shadow_icc).unwrap();
        let shadow_v4_icc = profile!("shadow_v4");
        let shadow_v4 = ColorProfile::new_from_slice(shadow_v4_icc).unwrap();
        let inklimit_icc = profile!("inklimit");
        let inklimit = ColorProfile::new_from_slice(inklimit_icc).unwrap();
        for intent in [Perceptual, RelativeColorimetric, Saturation] {
            assert!(chain_compensation(None, &shadow, shadow_icc, intent, true).is_some());
            assert!(chain_compensation(None, &shadow, shadow_icc, intent, false).is_none());
            let forced = chain_compensation(None, &shadow_v4, shadow_v4_icc, intent, false);
            assert_eq!(
                forced.is_some(),
                intent != RelativeColorimetric,
                "{intent:?}"
            );
            // Black to black: nothing to do.
            assert!(chain_compensation(None, &inklimit, inklimit_icc, intent, true).is_none());
        }
        for oi in [&shadow, &shadow_v4] {
            assert!(
                chain_compensation(None, oi, shadow_v4_icc, AbsoluteColorimetric, true).is_none()
            );
        }
        // Y 0.0002 → 0 changes the matrix by about 0.0012: an empty layer.
        let near = Some(lab_to_xyz_d50([0.18, 0.0, 0.0]));
        assert!(
            chain_compensation(near, &inklimit, inklimit_icc, RelativeColorimetric, true).is_none()
        );
        let further = Some(lab_to_xyz_d50([0.5, 0.0, 0.0]));
        assert!(
            chain_compensation(further, &inklimit, inklimit_icc, RelativeColorimetric, true)
                .is_some()
        );
    }
}
