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

#[test]
fn relative_colorimetric_matches_lcms() {
    for bpc in [false, true] {
        assert_matches_lcms(
            "split.icc",
            SPLIT,
            &reference::SPLIT,
            IccRenderingIntent::RelativeColorimetric,
            bpc,
        );
        assert_matches_lcms(
            "split_sat.icc",
            SPLIT_SAT,
            &reference::SPLIT_SAT,
            IccRenderingIntent::RelativeColorimetric,
            bpc,
        );
    }
}
