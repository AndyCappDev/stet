// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The black point black-point compensation starts from, for a CMYK
//! source profile and a rendering intent — lcms2's `cmsDetectBlackPoint`,
//! as Ghostscript runs it.
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
//! Two departures, both documented where they apply: a profile whose
//! round-trip tables are v4 `mAB`/`mBA` keeps 400% ink, as stet has no
//! evaluator for them; and the `bkpt` tag is ignored, as Ghostscript's
//! lcms2 build ignores it.

use moxcms::{ColorProfile, DataColorSpace, RenderingIntent};

use super::bpc::{lab_to_xyz_d50, xyz_d50_to_lab};
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
        return ink_limited_black(profile, v4);
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

/// lcms2's `BlackPointUsingPerceptualBlack`: the round trip's landing
/// point, made neutral and clipped to L\* 50.
fn ink_limited_black(profile: &ColorProfile, v4: bool) -> Option<SourceBlack> {
    // Perceptual must be supported as input, and lcms2 cannot build the
    // round trip without `B2A0`; either way the black point is zero.
    profile.lut_a_to_b_perceptual.as_ref()?;
    profile.lut_b_to_a_perceptual.as_ref()?;
    let start = if v4 {
        xyz_d50_to_lab(PERCEPTUAL_BLACK)
    } else {
        [0.0; 3]
    };
    let Some((ink, lab)) = hand_rolled::perceptual_round_trip(profile, start) else {
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

    const PROFILES: [Reference; 9] = [
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
    ];

    /// The XYZ a detected black point stands for: the darker colorant is
    /// 400% ink through the intent's table, as lcms2 computes it.
    fn resolve(
        profile: &ColorProfile,
        intent: RenderingIntent,
        black: Option<SourceBlack>,
    ) -> [f64; 3] {
        match black {
            None => [0.0; 3],
            Some(SourceBlack::Xyz(xyz)) => xyz,
            Some(SourceBlack::DarkerColorant) => {
                let table = match intent {
                    Perceptual => &profile.lut_a_to_b_perceptual,
                    Saturation => &profile.lut_a_to_b_saturation,
                    _ => &profile.lut_a_to_b_colorimetric,
                };
                let lab = hand_rolled::table_black(table.as_ref().unwrap()).unwrap();
                lab_to_xyz_d50([lab[0].min(50.0), 0.0, 0.0])
            }
        }
    }

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
                let got = resolve(&profile, intent, detect(&profile, icc, intent));
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
}
