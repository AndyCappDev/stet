// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! CMYK → sRGB conversion against lcms2, the colour engine inside
//! Ghostscript.
//!
//! `data/cmyk_intent/` holds generated CMYK profiles — two whose perceptual,
//! colorimetric and (in one) saturation tables differ, the first again as
//! `lut8Type`, one whose perceptual table copies its colorimetric one, and
//! `inklimit*.icc`, whose perceptual `B2A0` stops short of 400% ink as a
//! press profile's does — and lcms2's output for each intent with
//! black-point compensation off and on, and lcms2's proofing-chain stage 1
//! (a CMYK or Gray source into an output-intent CMYK) for each intent.
//! `generate.py` there makes every file; re-run it rather than editing
//! them.

use stet_graphics::icc::{BpcMode, IccCache, IccCacheOptions, IccRenderingIntent};

// The black points and round-trip legs are for the black-point detector's
// own unit tests; this file reads the sRGB tables.
#[allow(dead_code)]
mod reference {
    include!("data/cmyk_intent/reference.rs");
}

const SPLIT: &[u8] = include_bytes!("data/cmyk_intent/split.icc");
const SPLIT_SAT: &[u8] = include_bytes!("data/cmyk_intent/split_sat.icc");
const SPLIT_LUT8: &[u8] = include_bytes!("data/cmyk_intent/split_lut8.icc");
const SPLIT_XYZ: &[u8] = include_bytes!("data/cmyk_intent/split_xyz.icc");
const SHADOW: &[u8] = include_bytes!("data/cmyk_intent/shadow.icc");
const SHADOW_V4: &[u8] = include_bytes!("data/cmyk_intent/shadow_v4.icc");
const GRAY_TRC: &[u8] = include_bytes!("data/cmyk_intent/gray_trc.icc");
const SAME: &[u8] = include_bytes!("data/cmyk_intent/same.icc");
const INKLIMIT: &[u8] = include_bytes!("data/cmyk_intent/inklimit.icc");
const INKLIMIT_LUT8: &[u8] = include_bytes!("data/cmyk_intent/inklimit_lut8.icc");
const INKLIMIT_SCNR: &[u8] = include_bytes!("data/cmyk_intent/inklimit_scnr.icc");
const INKLIMIT_V4: &[u8] = include_bytes!("data/cmyk_intent/inklimit_v4.icc");
const RGB_GAMMA: &[u8] = include_bytes!("data/cmyk_intent/rgb_gamma.icc");
const RGB_LUT: &[u8] = include_bytes!("data/cmyk_intent/rgb_lut.icc");
const RGB_LUT_V4: &[u8] = include_bytes!("data/cmyk_intent/rgb_lut_v4.icc");
const SRGB: &[u8] = include_bytes!("data/cmyk_intent/srgb.icc");
const MAB: &[u8] = include_bytes!("data/cmyk_intent/mab.icc");
const RGB_MAB: &[u8] = include_bytes!("data/cmyk_intent/rgb_mab.icc");

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
            assert_matches_lcms("same.icc", SAME, &reference::SAME, intent, bpc);
        }
    }
}

/// lcms2 compensates relative colorimetric from a CMYK output profile's
/// ink-limited black: Lab L*=0 through the perceptual `B2A0`, then back
/// through `A2B1`. `inklimit.icc` is `split.icc` with a `B2A0` that stops
/// near 310% ink, so lcms2 renders the two alike except there.
#[test]
fn ink_limit_moves_only_the_relative_colorimetric_black_point() {
    let (relcol, bpc_on) = (1, 1);
    for intent in 0..3 {
        for bpc in 0..2 {
            let same = reference::INKLIMIT[intent][bpc] == reference::SPLIT[intent][bpc];
            assert_eq!(
                same,
                (intent, bpc) != (relcol, bpc_on),
                "lcms2 itself, intent {intent}, bpc {bpc}"
            );
        }
    }
    // Lighter: L* 19.4 against 8 for 400% ink.
    assert!(reference::INKLIMIT_BLACK_POINT[relcol][1] > reference::SPLIT_BLACK_POINT[relcol][1]);
    // An input-class profile has no ink limit to discount.
    assert_eq!(reference::INKLIMIT_SCNR, reference::SPLIT);
}

/// stet's black point for relative colorimetric is the one lcms2 detects:
/// the round trip on an output profile, 400% ink on an input-class one, and
/// for ICC v4 the fixed perceptual black under perceptual and saturation.
#[test]
fn black_point_is_the_one_lcms_detects() {
    for intent in INTENTS {
        for bpc in [false, true] {
            assert_matches_lcms("inklimit.icc", INKLIMIT, &reference::INKLIMIT, intent, bpc);
            assert_matches_lcms(
                "inklimit_scnr.icc",
                INKLIMIT_SCNR,
                &reference::INKLIMIT_SCNR,
                intent,
                bpc,
            );
            assert_matches_lcms(
                "inklimit_v4.icc",
                INKLIMIT_V4,
                &reference::INKLIMIT_V4,
                intent,
                bpc,
            );
        }
    }
}

/// A profile the hand-rolled sampler cannot read is baked from moxcms's
/// transform, and compensates from the same black point: the ink-limited
/// one, read from its `lut8Type` tables. moxcms's arithmetic is its own
/// (4 levels off lcms2 with BPC off too), but 400% ink would leave 400%
/// ink itself ~20 levels off.
#[test]
fn moxcms_fallback_compensates_from_the_ink_limit() {
    let relcol = IccRenderingIntent::RelativeColorimetric;
    let samples: Vec<u8> = reference::SAMPLES.iter().flatten().copied().collect();
    for bpc in [false, true] {
        let cache = IccCache::new_with_options(IccCacheOptions {
            bpc_mode: if bpc { BpcMode::On } else { BpcMode::Off },
            source_cmyk_profile: Some(INKLIMIT_LUT8.to_vec()),
        });
        let hash = *cache.default_cmyk_hash().unwrap();
        let rgb = cache
            .convert_image_8bit_with_intent(&hash, &samples, reference::SAMPLES.len(), relcol)
            .unwrap();
        let want = &reference::INKLIMIT_LUT8[1][bpc as usize];
        for (i, (cmyk, lcms)) in reference::SAMPLES.iter().zip(want).enumerate() {
            let got = &rgb[i * 3..i * 3 + 3];
            for ch in 0..3 {
                assert!(
                    got[ch].abs_diff(lcms[ch]) <= 4,
                    "BPC {bpc}, CMYK {cmyk:?}: stet {got:?}, lcms2 {lcms:?}"
                );
            }
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

/// A profile whose perceptual table is a copy of its colorimetric one, as
/// in FOGRA39L and most profiles a system installs, shows perceptual
/// exactly as relative colorimetric with black-point compensation (the
/// default), as lcms2 does: honouring the intent moves nothing there.
#[test]
fn identical_tables_show_perceptual_as_relative_colorimetric() {
    let bpc_on = 1;
    assert_eq!(
        reference::SAME[0][bpc_on],
        reference::SAME[1][bpc_on],
        "lcms2 itself"
    );
    let cache = cache(SAME);
    let hash = *cache.default_cmyk_hash().unwrap();
    let samples: Vec<u8> = reference::SAMPLES.iter().flatten().copied().collect();
    let n = reference::SAMPLES.len();
    let image = |intent| {
        cache
            .convert_image_8bit_with_intent(&hash, &samples, n, intent)
            .unwrap()
    };
    assert_eq!(
        image(IccRenderingIntent::Perceptual),
        image(IccRenderingIntent::RelativeColorimetric)
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

/// A PDF/X cache with black-point compensation `bpc`: `oi` is the output
/// intent, and `source` is registered after proofing is on, so its colours
/// chain through the output intent.
fn proofing(oi: &[u8], source: &[u8], bpc: BpcMode) -> (IccCache, [u8; 32]) {
    let mut cache = IccCache::new_with_options(IccCacheOptions {
        bpc_mode: bpc,
        source_cmyk_profile: Some(oi.to_vec()),
    });
    cache.set_proofing_enabled(true);
    let hash = cache.register_profile(source).unwrap();
    (cache, hash)
}

/// The output intent's CMYK as a DeviceCMYK paint shows it.
fn shown(cache: &IccCache, [c, m, y, k]: [f64; 4]) -> [u8; 3] {
    to_u8(cache.convert_cmyk_readonly(c, m, y, k).unwrap())
}

fn ink(cmyk: [u8; 4]) -> [f64; 4] {
    cmyk.map(|v| v as f64 / 255.0)
}

/// Check every sample of `source` chained into `oi` under `intent`, with
/// `--bpc off`, against lcms2's stage 1 without compensation shown through
/// the output intent: at most `tolerance` levels per channel, from the
/// stage-1 ink alone, since both sides share stage 2.
fn assert_chain_matches_lcms(
    name: &str,
    (oi, source): (&[u8], &[u8]),
    expected: &[[[f64; 4]; 16]; 3],
    tolerance: u8,
) {
    let (cache, hash) = proofing(oi, source, BpcMode::Off);
    for intent in INTENTS {
        let want = &expected[reference_row(intent)];
        for (cmyk, want) in reference::SAMPLES.iter().zip(want) {
            let got = to_u8(
                cache
                    .convert_color_readonly_with_intent(&hash, &ink(*cmyk), intent)
                    .unwrap(),
            );
            let want = shown(&cache, *want);
            for ch in 0..3 {
                assert!(
                    got[ch].abs_diff(want[ch]) <= tolerance,
                    "{name} {intent:?} {cmyk:?}: chain {got:?}, lcms2's ink shown {want:?}"
                );
            }
        }
    }
}

/// In a PDF/X document an ICCBased CMYK colour converts into the output
/// condition through its own intent's tables on both sides, as lcms2 does;
/// here without black-point compensation, which the tests below add. It
/// used to take moxcms's perceptual transform whatever the intent.
#[test]
fn cmyk_sources_chain_through_the_output_intent_by_intent() {
    for (name, profiles, expected) in [
        ("split", (INKLIMIT, SPLIT), &reference::CHAIN_SPLIT_INKLIMIT),
        (
            "split_sat",
            (INKLIMIT, SPLIT_SAT),
            &reference::CHAIN_SPLIT_SAT_INKLIMIT,
        ),
        (
            "lut8",
            (INKLIMIT_LUT8, SPLIT_LUT8),
            &reference::CHAIN_SPLIT_LUT8_INKLIMIT_LUT8,
        ),
    ] {
        assert_chain_matches_lcms(name, profiles, expected, 1);
    }
}

/// The intents land on different ink, or the test above proves nothing.
#[test]
fn the_chain_references_tell_the_intents_apart() {
    for expected in [
        &reference::CHAIN_SPLIT_INKLIMIT,
        &reference::CHAIN_SPLIT_SAT_INKLIMIT,
        &reference::CHAIN_SPLIT_XYZ_SPLIT,
    ] {
        assert_ne!(expected[0], expected[1]);
    }
    assert_ne!(
        reference::CHAIN_SPLIT_SAT_INKLIMIT[2],
        reference::CHAIN_SPLIT_SAT_INKLIMIT[0]
    );
}

/// A profile the hand-rolled stage 1 cannot read (here an XYZ PCS) chains
/// through moxcms's transform built for the intent. Neither profile has a
/// saturation table, which moxcms refuses and lcms2 reads as perceptual.
#[test]
fn moxcms_fallback_chains_by_intent() {
    assert_chain_matches_lcms(
        "split_xyz",
        (SPLIT, SPLIT_XYZ),
        &reference::CHAIN_SPLIT_XYZ_SPLIT,
        1,
    );
}

/// Absolute colorimetric chains as relative colorimetric, and so does a
/// conversion that names no intent; an image converts as its single
/// colours do.
#[test]
fn chain_defaults_and_images() {
    for (oi, source) in [(INKLIMIT, SPLIT), (SPLIT, SPLIT_XYZ)] {
        let (cache, hash) = proofing(oi, source, BpcMode::On);
        let relcol = IccRenderingIntent::RelativeColorimetric;
        let samples: Vec<u8> = reference::SAMPLES.iter().flatten().copied().collect();
        let n = reference::SAMPLES.len();
        for intent in INTENTS {
            let image = cache
                .convert_image_8bit_with_intent(&hash, &samples, n, intent)
                .unwrap();
            for (cmyk, px) in reference::SAMPLES.iter().zip(image.as_chunks::<3>().0) {
                let single = to_u8(
                    cache
                        .convert_color_readonly_with_intent(&hash, &ink(*cmyk), intent)
                        .unwrap(),
                );
                // The 8-bit chain rounds the intermediate ink to 8 bits.
                for ch in 0..3 {
                    assert!(
                        px[ch].abs_diff(single[ch]) <= 2,
                        "{intent:?} {cmyk:?}: image {px:?}, single colour {single:?}"
                    );
                }
            }
        }
        for cmyk in reference::SAMPLES {
            let at = |intent| cache.convert_color_readonly_with_intent(&hash, &ink(cmyk), intent);
            assert_eq!(at(IccRenderingIntent::AbsoluteColorimetric), at(relcol));
            assert_eq!(cache.convert_color_readonly(&hash, &ink(cmyk)), at(relcol));
        }
    }
}

/// In a PDF/X document an ICCBased Gray colour converts into the output
/// condition by its intent, with black-point compensation when `--bpc` is
/// on, as lcms2 does with its flag; into an ICC v4 output intent lcms2
/// compensates perceptual and saturation whatever the flag. It used to
/// convert absolute colorimetric, uncompensated, whatever the intent.
#[test]
fn gray_sources_chain_with_compensation() {
    for (name, oi, expected, v4) in [
        ("shadow", SHADOW, &reference::GRAY_CHAIN_SHADOW, false),
        (
            "shadow_v4",
            SHADOW_V4,
            &reference::GRAY_CHAIN_SHADOW_V4,
            true,
        ),
        ("inklimit", INKLIMIT, &reference::GRAY_CHAIN_INKLIMIT, false),
        ("mab", MAB, &reference::GRAY_CHAIN_MAB, true),
    ] {
        for mode in [BpcMode::On, BpcMode::Off, BpcMode::Auto] {
            let (cache, hash) = proofing(oi, GRAY_TRC, mode);
            for intent in INTENTS {
                let forced = v4 && intent != IccRenderingIntent::RelativeColorimetric;
                let bpc = (mode.is_enabled() || forced) as usize;
                let want = &expected[reference_row(intent)][bpc];
                for (g, want) in reference::GRAY_SAMPLES.iter().zip(want) {
                    let got = to_u8(
                        cache
                            .convert_color_readonly_with_intent(&hash, &[*g], intent)
                            .unwrap(),
                    );
                    let want = shown(&cache, *want);
                    for ch in 0..3 {
                        assert!(
                            got[ch].abs_diff(want[ch]) <= 1,
                            "{name} {mode:?} {intent:?} gray {g}: chain {got:?}, lcms2's ink shown {want:?}"
                        );
                    }
                }
            }
        }
    }
    // Compensation moves these colours, or the test proves nothing.
    let shadow = &reference::GRAY_CHAIN_SHADOW;
    for row in shadow {
        assert_ne!(row[0], row[1]);
    }
    assert_eq!(
        reference::GRAY_CHAIN_SHADOW_V4[0][0],
        reference::GRAY_CHAIN_SHADOW_V4[0][1]
    );
}

/// Gray, like CMYK: absolute colorimetric and a conversion that names no
/// intent chain as relative colorimetric, and an image as its single
/// colours do.
#[test]
fn gray_chain_defaults_and_images() {
    let (cache, hash) = proofing(SHADOW, GRAY_TRC, BpcMode::On);
    let relcol = IccRenderingIntent::RelativeColorimetric;
    let samples: Vec<u8> = (0..=255).step_by(17).map(|v| v as u8).collect();
    for intent in INTENTS {
        let image = cache
            .convert_image_8bit_with_intent(&hash, &samples, samples.len(), intent)
            .unwrap();
        for (&g, px) in samples.iter().zip(image.as_chunks::<3>().0) {
            let single = to_u8(
                cache
                    .convert_color_readonly_with_intent(&hash, &[g as f64 / 255.0], intent)
                    .unwrap(),
            );
            // The 8-bit chain rounds the intermediate ink to 8 bits.
            for ch in 0..3 {
                assert!(
                    px[ch].abs_diff(single[ch]) <= 2,
                    "{intent:?} {g}: image {px:?}, single colour {single:?}"
                );
            }
        }
    }
    for g in reference::GRAY_SAMPLES {
        let at = |intent| cache.convert_color_readonly_with_intent(&hash, &[g], intent);
        assert_eq!(at(IccRenderingIntent::AbsoluteColorimetric), at(relcol));
        assert_eq!(cache.convert_color_readonly(&hash, &[g]), at(relcol));
    }
}

/// lcms2's stage-1 ink for each sample, indexed `[intent][bpc]`.
type Chain<const N: usize> = [[[[f64; 4]; N]; 2]; 3];

/// A chain to check: name, output intent, source, whether the output
/// intent is ICC v4, and lcms2's ink.
type Case<'a, const N: usize> = (&'a str, &'a [u8], &'a [u8], bool, &'a Chain<N>);

/// `--bpc` as lcms2's flag: on for `On` and `Auto`, and forced into an ICC
/// v4 output intent under perceptual and saturation.
fn compensated(mode: BpcMode, v4: bool, intent: IccRenderingIntent) -> usize {
    let forced = v4 && intent != IccRenderingIntent::RelativeColorimetric;
    (mode.is_enabled() || forced) as usize
}

/// In a PDF/X document an ICCBased RGB or CMYK colour converts into the
/// output condition with black-point compensation when `--bpc` is on, as
/// lcms2 does with its flag, and as Gray already did: what is shown, and
/// the ink recorded for CMYK blending. They used to convert uncompensated
/// whatever `--bpc` said.
#[test]
fn rgb_and_cmyk_sources_chain_with_compensation() {
    let cmyk: [Case<16>; 5] = [
        (
            "split",
            SHADOW,
            SPLIT,
            false,
            &reference::CHAIN_BPC_SPLIT_SHADOW,
        ),
        (
            "split_sat v4",
            SHADOW_V4,
            SPLIT_SAT,
            true,
            &reference::CHAIN_BPC_SPLIT_SAT_SHADOW_V4,
        ),
        (
            "inklimit",
            SHADOW,
            INKLIMIT,
            false,
            &reference::CHAIN_BPC_INKLIMIT_SHADOW,
        ),
        // ICC v4 `lutAToBType` / `lutBToAType` tables, which moxcms used to
        // convert, uncompensated.
        ("mab", SHADOW, MAB, false, &reference::CHAIN_BPC_MAB_SHADOW),
        (
            "into mab",
            MAB,
            SPLIT,
            true,
            &reference::CHAIN_BPC_SPLIT_MAB,
        ),
    ];
    let rgb: [Case<15>; 7] = [
        (
            "rgb_gamma",
            SHADOW,
            RGB_GAMMA,
            false,
            &reference::CHAIN_BPC_RGB_GAMMA_SHADOW,
        ),
        (
            "rgb_lut",
            SHADOW,
            RGB_LUT,
            false,
            &reference::CHAIN_BPC_RGB_LUT_SHADOW,
        ),
        (
            "rgb_lut_v4",
            SHADOW,
            RGB_LUT_V4,
            false,
            &reference::CHAIN_BPC_RGB_LUT_V4_SHADOW,
        ),
        (
            "srgb",
            SHADOW,
            SRGB,
            false,
            &reference::CHAIN_BPC_SRGB_SHADOW,
        ),
        (
            "srgb v4",
            SHADOW_V4,
            SRGB,
            true,
            &reference::CHAIN_BPC_SRGB_SHADOW_V4,
        ),
        (
            "rgb_mab",
            SHADOW,
            RGB_MAB,
            false,
            &reference::CHAIN_BPC_RGB_MAB_SHADOW,
        ),
        (
            "rgb into mab",
            MAB,
            RGB_GAMMA,
            true,
            &reference::CHAIN_BPC_RGB_GAMMA_MAB,
        ),
    ];
    let inputs_cmyk: Vec<Vec<f64>> = reference::SAMPLES
        .iter()
        .map(|s| ink(*s).to_vec())
        .collect();
    let inputs_rgb: Vec<Vec<f64>> = reference::RGB_SAMPLES
        .iter()
        .map(|s| s.map(|v| v as f64 / 255.0).to_vec())
        .collect();
    let cases = cmyk
        .iter()
        .map(|&(n, oi, src, v4, want)| {
            (
                n,
                oi,
                src,
                v4,
                &inputs_cmyk,
                want.map(|i| i.map(|b| b.to_vec())),
            )
        })
        .chain(rgb.iter().map(|&(n, oi, src, v4, want)| {
            (
                n,
                oi,
                src,
                v4,
                &inputs_rgb,
                want.map(|i| i.map(|b| b.to_vec())),
            )
        }));
    for (name, oi, source, v4, inputs, expected) in cases {
        for mode in [BpcMode::On, BpcMode::Off, BpcMode::Auto] {
            let (cache, hash) = proofing(oi, source, mode);
            for intent in INTENTS {
                let want = &expected[reference_row(intent)][compensated(mode, v4, intent)];
                for (input, want) in inputs.iter().zip(want) {
                    let got = to_u8(
                        cache
                            .convert_color_readonly_with_intent(&hash, input, intent)
                            .unwrap(),
                    );
                    let shown_want = shown(&cache, *want);
                    for ch in 0..3 {
                        assert!(
                            got[ch].abs_diff(shown_want[ch]) <= 1,
                            "{name} {mode:?} {intent:?} {input:?}: chain {got:?}, lcms2's ink shown {shown_want:?}"
                        );
                    }
                    // RGB records the stage-1 ink for CMYK blending.
                    if input.len() == 3 {
                        let ink = cache.convert_to_oi_cmyk(&hash, input, intent).unwrap();
                        for (got, want) in ink.iter().zip(want) {
                            assert!(
                                (got - want).abs() < 1e-4,
                                "{name} {mode:?} {intent:?} {input:?}: ink {ink:?}, lcms2 {want:?}"
                            );
                        }
                    }
                }
            }
        }
    }
}

/// An RGB image converts as its single colours do with compensation on,
/// and compensation moves the chain, or the test above proves nothing.
#[test]
fn compensated_rgb_images_match_their_colours() {
    let (cache, hash) = proofing(SHADOW, SRGB, BpcMode::On);
    let samples: Vec<u8> = reference::RGB_SAMPLES.iter().flatten().copied().collect();
    let n = reference::RGB_SAMPLES.len();
    for intent in INTENTS {
        let image = cache
            .convert_image_8bit_with_intent(&hash, &samples, n, intent)
            .unwrap();
        for (rgb, px) in reference::RGB_SAMPLES.iter().zip(image.as_chunks::<3>().0) {
            let input = rgb.map(|v| v as f64 / 255.0);
            let single = to_u8(
                cache
                    .convert_color_readonly_with_intent(&hash, &input, intent)
                    .unwrap(),
            );
            // The 8-bit chain rounds the intermediate ink to 8 bits.
            for ch in 0..3 {
                assert!(
                    px[ch].abs_diff(single[ch]) <= 2,
                    "{intent:?} {rgb:?}: image {px:?}, single colour {single:?}"
                );
            }
        }
    }
    for row in &reference::CHAIN_BPC_SRGB_SHADOW {
        assert_ne!(row[0], row[1]);
    }
    for row in &reference::CHAIN_BPC_SPLIT_SHADOW {
        assert_ne!(row[0], row[1]);
    }
}

/// A Lab colour's ink in the output intent, which CMYK blending uses, is
/// lcms2's with its flag as `--bpc` sets it: from Lab's black, zero, to the
/// output intent's.
#[test]
fn lab_ink_is_compensated_as_lcms() {
    for (name, oi, v4, expected) in [
        ("lab", SHADOW, false, &reference::CHAIN_BPC_LAB_SHADOW),
        (
            "lab v4",
            SHADOW_V4,
            true,
            &reference::CHAIN_BPC_LAB_SHADOW_V4,
        ),
        ("lab into mab", MAB, true, &reference::CHAIN_BPC_LAB_MAB),
    ] {
        for mode in [BpcMode::On, BpcMode::Off, BpcMode::Auto] {
            let mut cache = IccCache::new_with_options(IccCacheOptions {
                bpc_mode: mode,
                source_cmyk_profile: Some(oi.to_vec()),
            });
            cache.set_proofing_enabled(true);
            cache.prepare_lab_to_oi_cmyk();
            for intent in INTENTS {
                let want = &expected[reference_row(intent)][compensated(mode, v4, intent)];
                for (lab, want) in reference::LAB_SAMPLES.iter().zip(want) {
                    let got = cache
                        .convert_lab_to_oi_cmyk(lab[0], lab[1], lab[2], intent)
                        .unwrap();
                    for (g, w) in got.iter().zip(want) {
                        assert!(
                            (g - w).abs() < 1e-4,
                            "{name} {mode:?} {intent:?} {lab:?}: {got:?}, lcms2 {want:?}"
                        );
                    }
                }
            }
        }
    }
}
