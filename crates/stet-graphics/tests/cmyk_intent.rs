// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! CMYK → sRGB conversion against lcms2, the colour engine inside
//! Ghostscript.
//!
//! `data/cmyk_intent/` holds two generated CMYK profiles whose perceptual,
//! colorimetric and (in one) saturation tables differ, and lcms2's output for
//! each intent with black-point compensation off and on. `generate.py` there
//! makes all three files; re-run it rather than editing them.

use stet_graphics::icc::{BpcMode, IccCache, IccCacheOptions, IccRenderingIntent};

mod reference {
    include!("data/cmyk_intent/reference.rs");
}

const SPLIT: &[u8] = include_bytes!("data/cmyk_intent/split.icc");
const SPLIT_SAT: &[u8] = include_bytes!("data/cmyk_intent/split_sat.icc");
const SPLIT_LUT8: &[u8] = include_bytes!("data/cmyk_intent/split_lut8.icc");

/// Largest per-channel difference allowed from lcms2. stet bakes a 17⁴ table
/// and interpolates it, and its Lab → sRGB arithmetic is its own.
const TOLERANCE: u8 = 2;

/// Row of the reference tables for `intent`.
fn reference_row(intent: IccRenderingIntent) -> usize {
    match intent {
        IccRenderingIntent::Perceptual => 0,
        IccRenderingIntent::RelativeColorimetric => 1,
        IccRenderingIntent::Saturation => 2,
        IccRenderingIntent::AbsoluteColorimetric => {
            unreachable!("lcms2's absolute colorimetric is not recorded")
        }
    }
}

/// Convert every reference sample through `profile` with `intent`, and check
/// the result against lcms2's.
fn assert_matches_lcms(
    name: &str,
    profile: &[u8],
    expected: &[[[[u8; 3]; 16]; 2]; 3],
    intent: IccRenderingIntent,
    bpc: bool,
) {
    let cache = IccCache::new_with_options(IccCacheOptions {
        bpc_mode: if bpc { BpcMode::On } else { BpcMode::Off },
        source_cmyk_profile: Some(profile.to_vec()),
    });
    let hash = *cache
        .default_cmyk_hash()
        .unwrap_or_else(|| panic!("{name}: profile not registered"));
    let samples: Vec<u8> = reference::SAMPLES.iter().flatten().copied().collect();
    let rgb = cache
        .convert_image_8bit_with_intent(&hash, &samples, reference::SAMPLES.len(), intent)
        .unwrap_or_else(|| panic!("{name}: conversion failed"));

    let want = &expected[reference_row(intent)][bpc as usize];
    let mut failures = Vec::new();
    for (i, (cmyk, lcms)) in reference::SAMPLES.iter().zip(want).enumerate() {
        let got = &rgb[i * 3..i * 3 + 3];
        let worst = (0..3).map(|c| got[c].abs_diff(lcms[c])).max().unwrap();
        if worst > TOLERANCE {
            failures.push(format!("  CMYK {cmyk:?}: stet {got:?}, lcms2 {lcms:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{name}, {intent:?}, BPC {}: differs from lcms2 by more than {TOLERANCE}:\n{}",
        if bpc { "on" } else { "off" },
        failures.join("\n")
    );
}

const INTENTS: [IccRenderingIntent; 3] = [
    IccRenderingIntent::Perceptual,
    IccRenderingIntent::RelativeColorimetric,
    IccRenderingIntent::Saturation,
];

#[test]
fn every_intent_matches_lcms() {
    for intent in INTENTS {
        for bpc in [false, true] {
            assert_matches_lcms("split.icc", SPLIT, &reference::SPLIT, intent, bpc);
            assert_matches_lcms(
                "split_sat.icc",
                SPLIT_SAT,
                &reference::SPLIT_SAT,
                intent,
                bpc,
            );
        }
    }
}

fn cache(profile: &[u8]) -> IccCache {
    IccCache::new_with_options(IccCacheOptions {
        bpc_mode: BpcMode::On,
        source_cmyk_profile: Some(profile.to_vec()),
    })
}

fn to_u8((r, g, b): (f64, f64, f64)) -> [u8; 3] {
    [r, g, b].map(|v| (v * 255.0).round() as u8)
}

/// Single colours, cached and uncached, take the same table as images, so
/// a flat fill matches the image beside it.
#[test]
fn single_colours_use_the_intents_table() {
    let mut cache = cache(SPLIT_SAT);
    let hash = *cache.default_cmyk_hash().unwrap();
    for intent in INTENTS {
        let samples: Vec<u8> = reference::SAMPLES.iter().flatten().copied().collect();
        let image = cache
            .convert_image_8bit_with_intent(&hash, &samples, reference::SAMPLES.len(), intent)
            .unwrap();
        for (i, cmyk) in reference::SAMPLES.iter().enumerate() {
            let [c, m, y, k] = cmyk.map(|v| v as f64 / 255.0);
            let readonly = cache
                .convert_cmyk_readonly_with_intent(c, m, y, k, intent)
                .unwrap();
            let cached = cache.convert_cmyk_with_intent(c, m, y, k, intent).unwrap();
            assert_eq!(readonly, cached, "{intent:?} {cmyk:?}");
            let single = to_u8(readonly);
            let bulk = &image[i * 3..i * 3 + 3];
            for ch in 0..3 {
                assert!(
                    single[ch].abs_diff(bulk[ch]) <= 1,
                    "{intent:?} {cmyk:?}: single {single:?}, image {bulk:?}"
                );
            }
        }
    }
}

/// The intent-less conversions are relative colorimetric, as before.
#[test]
fn intent_less_conversions_are_relative_colorimetric() {
    let mut cache = cache(SPLIT_SAT);
    let hash = *cache.default_cmyk_hash().unwrap();
    let relcol = IccRenderingIntent::RelativeColorimetric;
    let samples: Vec<u8> = reference::SAMPLES.iter().flatten().copied().collect();
    let n = reference::SAMPLES.len();
    assert_eq!(
        cache.convert_image_8bit(&hash, &samples, n),
        cache.convert_image_8bit_with_intent(&hash, &samples, n, relcol)
    );
    for cmyk in reference::SAMPLES {
        let [c, m, y, k] = cmyk.map(|v| v as f64 / 255.0);
        let want = cache
            .convert_cmyk_readonly_with_intent(c, m, y, k, relcol)
            .unwrap();
        assert_eq!(cache.convert_cmyk_readonly(c, m, y, k), Some(want));
        assert_eq!(cache.convert_cmyk(c, m, y, k), Some(want));
        assert_eq!(
            cache.convert_color_readonly(&hash, &[c, m, y, k]),
            Some(want)
        );
    }
}

/// Absolute colorimetric uses the relative colorimetric table: its
/// white-point adaptation is not applied.
#[test]
fn absolute_colorimetric_is_relative_colorimetric() {
    let cache = cache(SPLIT_SAT);
    for cmyk in reference::SAMPLES {
        let [c, m, y, k] = cmyk.map(|v| v as f64 / 255.0);
        assert_eq!(
            cache.convert_cmyk_readonly_with_intent(
                c,
                m,
                y,
                k,
                IccRenderingIntent::AbsoluteColorimetric
            ),
            cache.convert_cmyk_readonly_with_intent(
                c,
                m,
                y,
                k,
                IccRenderingIntent::RelativeColorimetric
            ),
        );
    }
}

/// In a PDF/X document (proofing on) the default CMYK profile is the output
/// intent, and CMYK in it is the output: the intent governs converting into
/// it, not showing it. A DeviceCMYK paint shows the same under every
/// intent, and an RGB colour chained into the output intent with any intent
/// shows as a DeviceCMYK paint of the CMYK it landed on.
#[test]
fn the_output_intent_is_shown_with_one_table() {
    let mut oi = cache(SPLIT_SAT);
    oi.set_proofing_enabled(true);
    let srgb = moxcms::ColorProfile::new_srgb().encode().unwrap();
    let rgb_hash = oi.register_profile(&srgb).unwrap();
    let relcol = IccRenderingIntent::RelativeColorimetric;
    for intent in INTENTS {
        for cmyk in reference::SAMPLES {
            let [c, m, y, k] = cmyk.map(|v| v as f64 / 255.0);
            assert_eq!(
                oi.convert_cmyk_readonly_with_intent(c, m, y, k, intent),
                oi.convert_cmyk_readonly_with_intent(c, m, y, k, relcol),
                "{intent:?} {cmyk:?}"
            );
        }
        for rgb in [[0.2, 0.5, 0.8], [0.9, 0.3, 0.1], [0.1, 0.1, 0.1]] {
            let [c, m, y, k] = oi
                .convert_to_oi_cmyk(&rgb_hash, &rgb, intent)
                .unwrap_or_else(|| panic!("{intent:?}: no hand-rolled chain"));
            let direct = to_u8(oi.convert_cmyk_readonly(c, m, y, k).unwrap());
            let chained = to_u8(
                oi.convert_color_readonly_with_intent(&rgb_hash, &rgb, intent)
                    .unwrap(),
            );
            for ch in 0..3 {
                assert!(
                    chained[ch].abs_diff(direct[ch]) <= 1,
                    "{intent:?} {rgb:?}: chain {chained:?}, OI CMYK {c},{m},{y},{k} shown {direct:?}"
                );
            }
        }
    }
    // The same profile as a source (proofing off) does take the intent.
    let source = cache(SPLIT_SAT);
    assert_ne!(
        source.convert_cmyk_readonly_with_intent(
            0.0,
            0.0,
            0.0,
            1.0,
            IccRenderingIntent::Perceptual
        ),
        source.convert_cmyk_readonly_with_intent(0.0, 0.0, 0.0, 1.0, relcol),
    );
}

/// A profile the hand-rolled sampler cannot read (here `lut8Type`) is baked
/// from moxcms's transform instead, and that bake follows the intent too.
/// moxcms's arithmetic is its own, so this asserts only which of lcms2's
/// tables each result is nearer: relative colorimetric used to be baked
/// from the perceptual table.
#[test]
fn moxcms_fallback_follows_the_intent() {
    let cache = cache(SPLIT_LUT8);
    let hash = *cache.default_cmyk_hash().unwrap();
    let samples: Vec<u8> = reference::SAMPLES.iter().flatten().copied().collect();
    let n = reference::SAMPLES.len();
    let distance = |rgb: &[u8], want: &[[u8; 3]; 16]| -> u32 {
        want.iter()
            .flatten()
            .zip(rgb)
            .map(|(a, b)| a.abs_diff(*b) as u32)
            .sum()
    };
    let bpc_on = 1;
    let perceptual = &reference::SPLIT_LUT8[0][bpc_on];
    let relcol = &reference::SPLIT_LUT8[1][bpc_on];
    for (intent, want, other) in [
        (IccRenderingIntent::RelativeColorimetric, relcol, perceptual),
        (IccRenderingIntent::Perceptual, perceptual, relcol),
    ] {
        let rgb = cache
            .convert_image_8bit_with_intent(&hash, &samples, n, intent)
            .unwrap();
        let (near, far) = (distance(&rgb, want), distance(&rgb, other));
        assert!(
            near < far,
            "{intent:?}: {near} levels from lcms2's {intent:?}, {far} from the other intent"
        );
    }
}
