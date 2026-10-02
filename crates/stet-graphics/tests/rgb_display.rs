// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! ICCBased RGB → sRGB conversion against lcms2, the colour engine inside
//! Ghostscript, outside a proofing chain.
//!
//! An RGB profile with A2B tables reads a different table for each
//! rendering intent and compensates its black point as lcms2 does; colours
//! and images are evaluated exactly, so an image pixel is the colour of the
//! same value. A matrix-shaper profile stays with moxcms, unchanged. The
//! profiles and lcms2's output are in `data/cmyk_intent/`, made by
//! `generate.py` there: `rgb_lut.icc` (`lut16Type` A2B1 and A2B2, and tone
//! curves and colorants for perceptual), `rgb_lut_v4.icc` (A2B0–2, v4),
//! `rgb_mab.icc` (v4 `lutAToBType`), `rgb_xyz.icc` and `rgb_xyz_mab.icc`
//! (XYZ PCS, `lut16Type` and `lutAToBType`), and `rgb_mixed.icc` (a
//! `lutAToBType` with no CLUT whose matrix mixes the channels ahead of
//! steep curves).

use moxcms::{ColorProfile, Layout, TransformOptions};
use stet_graphics::icc::{BpcMode, IccCache, IccCacheOptions, IccRenderingIntent};

#[allow(dead_code)]
mod reference {
    include!("data/cmyk_intent/reference.rs");
}

const RGB_LUT: &[u8] = include_bytes!("data/cmyk_intent/rgb_lut.icc");
const RGB_LUT_V4: &[u8] = include_bytes!("data/cmyk_intent/rgb_lut_v4.icc");
const RGB_MAB: &[u8] = include_bytes!("data/cmyk_intent/rgb_mab.icc");
const RGB_XYZ: &[u8] = include_bytes!("data/cmyk_intent/rgb_xyz.icc");
const RGB_XYZ_MAB: &[u8] = include_bytes!("data/cmyk_intent/rgb_xyz_mab.icc");
const RGB_MIXED: &[u8] = include_bytes!("data/cmyk_intent/rgb_mixed.icc");
const RGB_GAMMA: &[u8] = include_bytes!("data/cmyk_intent/rgb_gamma.icc");
const SRGB: &[u8] = include_bytes!("data/cmyk_intent/srgb.icc");

type Display = [[[[f64; 3]; reference::DISPLAY_SAMPLES.len()]; 2]; 3];

static PROFILES: [(&str, &[u8], &Display); 6] = [
    ("rgb_lut", RGB_LUT, &reference::DISPLAY_RGB_LUT),
    ("rgb_lut_v4", RGB_LUT_V4, &reference::DISPLAY_RGB_LUT_V4),
    ("rgb_mab", RGB_MAB, &reference::DISPLAY_RGB_MAB),
    ("rgb_xyz", RGB_XYZ, &reference::DISPLAY_RGB_XYZ),
    ("rgb_xyz_mab", RGB_XYZ_MAB, &reference::DISPLAY_RGB_XYZ_MAB),
    ("rgb_mixed", RGB_MIXED, &reference::DISPLAY_RGB_MIXED),
];

const INTENTS: [IccRenderingIntent; 3] = [
    IccRenderingIntent::Perceptual,
    IccRenderingIntent::RelativeColorimetric,
    IccRenderingIntent::Saturation,
];

/// Largest difference from lcms2 allowed a colour, in 8-bit levels. stet
/// evaluates every stage as lcms2 does, in floating point; lcms2 takes a
/// 16-bit CLUT's input to 16 bits and interpolates it in fixed point even
/// in a floating-point transform, and writes its display matrix and D50
/// to more digits. Measured: at most 0.08 levels with no CLUT, 0.21 with
/// one.
const COLOUR_TOLERANCE: f64 = 0.25;

/// Largest difference from lcms2 rounded allowed an 8-bit image pixel:
/// a colour within [`COLOUR_TOLERANCE`] can round the other way.
const IMAGE_TOLERANCE: u8 = 1;

fn cache(icc: &[u8], bpc: bool) -> (IccCache, [u8; 32]) {
    let mut cache = IccCache::new_with_options(IccCacheOptions {
        bpc_mode: if bpc { BpcMode::On } else { BpcMode::Off },
        source_cmyk_profile: None,
    });
    let hash = cache.register_profile(icc).expect("profile registers");
    (cache, hash)
}

fn samples() -> Vec<u8> {
    reference::DISPLAY_SAMPLES
        .iter()
        .flatten()
        .copied()
        .collect()
}

fn colour(rgb: &[u8; 3]) -> [f64; 3] {
    rgb.map(|v| f64::from(v) / 255.0)
}

fn level(v: f64) -> u8 {
    (v * 255.0).round() as u8
}

#[test]
fn colours_match_lcms_for_every_intent() {
    for (name, icc, want) in PROFILES {
        for (row, intent) in INTENTS.into_iter().enumerate() {
            for bpc in [false, true] {
                let (cache, hash) = cache(icc, bpc);
                let mut worst = (0.0, [0u8; 3]);
                for (rgb, lcms) in reference::DISPLAY_SAMPLES
                    .iter()
                    .zip(&want[row][bpc as usize])
                {
                    let (r, g, b) = cache
                        .convert_color_readonly_with_intent(&hash, &colour(rgb), intent)
                        .unwrap();
                    for (got, lcms) in [r, g, b].into_iter().zip(lcms) {
                        let d = (got - lcms).abs() * 255.0;
                        if d > worst.0 {
                            worst = (d, *rgb);
                        }
                    }
                }
                assert!(
                    worst.0 <= COLOUR_TOLERANCE,
                    "{name}, {intent:?}, BPC {bpc}: RGB {:?} is {:.3} levels from lcms2",
                    worst.1,
                    worst.0
                );
            }
        }
    }
}

#[test]
fn images_match_lcms_and_their_colours() {
    let samples = samples();
    let n = reference::DISPLAY_SAMPLES.len();
    for (name, icc, want) in PROFILES {
        for (row, intent) in INTENTS.into_iter().enumerate() {
            for bpc in [false, true] {
                let (cache, hash) = cache(icc, bpc);
                let image = cache
                    .convert_image_8bit_with_intent(&hash, &samples, n, intent)
                    .unwrap();
                for ((rgb, px), lcms) in reference::DISPLAY_SAMPLES
                    .iter()
                    .zip(image.as_chunks::<3>().0)
                    .zip(&want[row][bpc as usize])
                {
                    let (r, g, b) = cache
                        .convert_color_readonly_with_intent(&hash, &colour(rgb), intent)
                        .unwrap();
                    assert_eq!(
                        *px,
                        [r, g, b].map(level),
                        "{name}, {intent:?}, BPC {bpc}: RGB {rgb:?} as a pixel and as a colour"
                    );
                    let lcms = lcms.map(level);
                    assert!(
                        (0..3).all(|c| px[c].abs_diff(lcms[c]) <= IMAGE_TOLERANCE),
                        "{name}, {intent:?}, BPC {bpc}: RGB {rgb:?} → {px:?}, lcms2 {lcms:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn absolute_colorimetric_is_relative_colorimetric() {
    let samples = samples();
    let n = reference::DISPLAY_SAMPLES.len();
    for (name, icc, _) in PROFILES {
        for bpc in [false, true] {
            let (cache, hash) = cache(icc, bpc);
            let image = |intent| {
                cache
                    .convert_image_8bit_with_intent(&hash, &samples, n, intent)
                    .unwrap()
            };
            assert_eq!(
                image(IccRenderingIntent::AbsoluteColorimetric),
                image(IccRenderingIntent::RelativeColorimetric),
                "{name}, BPC {bpc}"
            );
        }
    }
}

/// The conversions that take no intent are relative colorimetric, the PDF
/// and PostScript default.
#[test]
fn no_intent_is_relative_colorimetric() {
    let samples = samples();
    let n = reference::DISPLAY_SAMPLES.len();
    let relcol = IccRenderingIntent::RelativeColorimetric;
    for (name, icc, _) in PROFILES {
        let (mut cache, hash) = cache(icc, true);
        assert_eq!(
            cache.convert_image_8bit(&hash, &samples, n),
            cache.convert_image_8bit_with_intent(&hash, &samples, n, relcol),
            "{name}"
        );
        for rgb in &reference::DISPLAY_SAMPLES {
            let want = cache.convert_color_readonly_with_intent(&hash, &colour(rgb), relcol);
            assert_eq!(
                cache.convert_color_readonly(&hash, &colour(rgb)),
                want,
                "{name}"
            );
            assert_eq!(cache.convert_color(&hash, &colour(rgb)), want, "{name}");
        }
    }
}

/// A matrix-shaper is shown through moxcms's transform as before, whatever
/// the intent: every intent is the same matrix there.
#[test]
fn matrix_shapers_keep_moxcms() {
    let samples = samples();
    let n = reference::DISPLAY_SAMPLES.len();
    let srgb = ColorProfile::new_srgb();
    for (name, icc) in [("rgb_gamma", RGB_GAMMA), ("srgb", SRGB)] {
        let moxcms = ColorProfile::new_from_slice(icc)
            .unwrap()
            .create_transform_8bit(Layout::Rgb, &srgb, Layout::Rgb, TransformOptions::default())
            .unwrap();
        let mut want = vec![0u8; n * 3];
        moxcms.transform(&samples, &mut want).unwrap();
        let (cache, hash) = cache(icc, true);
        for intent in INTENTS {
            assert_eq!(
                cache
                    .convert_image_8bit_with_intent(&hash, &samples, n, intent)
                    .unwrap(),
                want,
                "{name}, {intent:?}"
            );
        }
    }
}
