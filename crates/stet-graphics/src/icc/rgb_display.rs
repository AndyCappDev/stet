// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A table-based RGB profile's RGB → sRGB conversion, one per rendering
//! intent, outside a proofing chain.
//!
//! An RGB profile with an A2B table — a scanner's, a camera's, an RGB
//! printer's — reads a different table for each intent, as lcms2 (the engine
//! inside Ghostscript) converts it to its built-in (v4) sRGB:
//!
//! | intent | table | black-point scaling |
//! |---|---|---|
//! | relative colorimetric | `A2B1` | as configured (`BpcMode`) |
//! | perceptual | `A2B0` | always |
//! | saturation | `A2B2` | always |
//! | absolute colorimetric | as relative colorimetric | |
//!
//! A missing table is replaced by `A2B0`, and with no `A2B0` either by the
//! profile's tone curves and colorant matrix, as lcms2 reads it
//! ([`hand_rolled::RgbToXyz`]). "Always" is lcms2 forcing compensation for
//! perceptual and saturation when either profile is ICC v4, as its sRGB is;
//! the black scaled is the one lcms2 detects for the source
//! ([`black_point::detect_rgb`]), to sRGB's zero. Absolute colorimetric's
//! white-point adaptation is not applied, as on the CMYK paths.
//!
//! Colours and images are both evaluated exactly, with no baked table: the
//! table lcms2's own optimiser would build misses some profiles by tens of
//! levels (a matrix that mixes the channels ahead of steep curves defeats
//! any grid), and the exact evaluation of an 8-bit image costs about what
//! moxcms's transform did. An image pixel is exactly the colour of the same
//! value, rounded to 8 bits.
//!
//! A matrix-shaper profile — sRGB, Adobe RGB, Display P3, most RGB in a
//! PDF — has no table and stays with moxcms, whose matrix path is already
//! exact: there every intent is the same conversion and black is zero.
//!
//! `crates/stet-graphics/tests/rgb_display.rs` checks these rules against
//! lcms2's recorded output.

use std::sync::{Arc, LazyLock, OnceLock};

use moxcms::{ColorProfile, DataColorSpace, RenderingIntent};

use super::black_point;
use super::bpc::{BpcParams, apply_bpc_xyz_d50};
use super::hand_rolled::{self, RgbToXyz};

/// A table-based RGB profile's conversions to sRGB, one per rendering
/// intent, each built on first use.
pub(super) struct RgbDisplay {
    profile: Arc<ColorProfile>,
    /// The profile's raw bytes, which hold a v4 table's curve sets.
    icc: Arc<[u8]>,
    /// Whether relative colorimetric compensates its black point
    /// (`BpcMode`).
    bpc: bool,
    /// Perceptual, relative colorimetric and saturation, by the moxcms
    /// discriminant; absolute colorimetric takes relative colorimetric's.
    /// `None` inside means stet cannot read the intent's table.
    slots: [OnceLock<Option<Conversion>>; 3],
}

/// One intent's conversion: the profile to XYZ, then black-point
/// compensation.
struct Conversion {
    to_xyz: RgbToXyz,
    bpc: Option<BpcParams>,
}

impl Conversion {
    /// Linear sRGB, unclipped, of the profile's XYZ `xyz`.
    #[inline]
    fn linear(&self, xyz: [f64; 3]) -> [f64; 3] {
        let xyz = match &self.bpc {
            Some(p) => apply_bpc_xyz_d50(xyz, p),
            None => xyz,
        };
        hand_rolled::xyz_d50_to_linear_srgb_d65(xyz)
    }
}

impl RgbDisplay {
    /// The conversions for `profile`, whose raw bytes are `icc`; `None`
    /// unless it is an RGB profile with at least one A2B table. Builds
    /// nothing.
    pub(super) fn new(profile: Arc<ColorProfile>, icc: &[u8], bpc: bool) -> Option<Self> {
        let has_table = profile.lut_a_to_b_perceptual.is_some()
            || profile.lut_a_to_b_colorimetric.is_some()
            || profile.lut_a_to_b_saturation.is_some();
        if profile.color_space != DataColorSpace::Rgb || !has_table {
            return None;
        }
        Some(Self {
            profile,
            icc: icc.into(),
            bpc,
            slots: Default::default(),
        })
    }

    /// The conversion for `intent`, building it if this is its first use;
    /// `None` when stet cannot read the table it needs.
    fn conversion(&self, intent: RenderingIntent) -> Option<&Conversion> {
        let intent = match intent {
            RenderingIntent::AbsoluteColorimetric => RenderingIntent::RelativeColorimetric,
            other => other,
        };
        self.slots[intent as usize]
            .get_or_init(|| {
                Some(Conversion {
                    to_xyz: RgbToXyz::new(&self.profile, &self.icc, intent)?,
                    bpc: self.compensation(intent),
                })
            })
            .as_ref()
    }

    /// The black-point compensation `intent` applies (see the module docs).
    fn compensation(&self, intent: RenderingIntent) -> Option<BpcParams> {
        let forced = matches!(
            intent,
            RenderingIntent::Perceptual | RenderingIntent::Saturation
        );
        if !forced && !self.bpc {
            return None;
        }
        let black = black_point::detect_rgb(&self.profile, &self.icc, intent)?;
        black_point::compensation(black, [0.0; 3])
    }

    /// The sRGB, each channel in `[0, 1]`, of `rgb` (each in `[0, 1]`)
    /// under `intent`; `None` when stet cannot read the table it needs.
    pub(super) fn convert(&self, intent: RenderingIntent, rgb: [f64; 3]) -> Option<[f64; 3]> {
        let conversion = self.conversion(intent)?;
        let linear = conversion.linear(conversion.to_xyz.xyz(rgb));
        Some(linear.map(|v| hand_rolled::linear_to_srgb(v.clamp(0.0, 1.0))))
    }

    /// The 8-bit sRGB of packed 8-bit RGB `samples` under `intent`: each
    /// pixel [`Self::convert`] of its value, rounded. `None` when stet
    /// cannot read the table it needs.
    pub(super) fn convert_image(&self, intent: RenderingIntent, samples: &[u8]) -> Option<Vec<u8>> {
        let conversion = self.conversion(intent)?;
        let encode = &*ENCODE;
        let mut out = vec![0u8; samples.len() / 3 * 3];
        let out_px = out.as_chunks_mut::<3>().0;
        conversion
            .to_xyz
            .each_xyz8(samples.as_chunks::<3>().0, |i, xyz| {
                out_px[i] = conversion.linear(xyz).map(|c| encode.code(c));
            });
        Some(out)
    }
}

/// Bands of linear sRGB [`EncodeTables::code`] looks codes up by: enough
/// that none holds more than one code boundary.
const ENCODE_BANDS: usize = 4096;

/// The tables that take linear sRGB to its 8-bit code without a `powf`.
struct EncodeTables {
    /// The code at the start of each band, and 255 for 1 itself.
    band: [u8; ENCODE_BANDS + 1],
    /// `threshold[k]`: the least linear value whose code is at least `k`;
    /// infinite past 255.
    threshold: [f64; 257],
}

impl EncodeTables {
    /// The 8-bit code of linear sRGB `linear`:
    /// `round(255 · linear_to_srgb(clamp(linear)))`, exactly. The code at
    /// the start of the 1/4096 band `linear` falls in, plus one if
    /// `linear` reaches the next code's threshold, as no band holds two;
    /// without branches, as noisy images defeat their prediction.
    #[inline]
    fn code(&self, linear: f64) -> u8 {
        // NaN passes through `clamp`, and the casts take it to band 0 and
        // below every threshold: code 0, as the sRGB curve gives.
        let v = linear.clamp(0.0, 1.0);
        let start = self.band[(v * ENCODE_BANDS as f64) as usize];
        start + u8::from(v >= self.threshold[usize::from(start) + 1])
    }
}

static ENCODE: LazyLock<EncodeTables> = LazyLock::new(|| EncodeTables {
    band: std::array::from_fn(|i| code_of(i as f64 / ENCODE_BANDS as f64)),
    threshold: std::array::from_fn(|k| match u8::try_from(k) {
        Ok(code) => least_reaching(code),
        Err(_) => f64::INFINITY,
    }),
});

/// The 8-bit code of linear sRGB `linear`, by the sRGB curve itself.
fn code_of(linear: f64) -> u8 {
    (hand_rolled::linear_to_srgb(linear.clamp(0.0, 1.0)) * 255.0)
        .round()
        .clamp(0.0, 255.0) as u8
}

/// The least linear value in `[0, 1]` whose [`code_of`] is at least `code`,
/// by bisection over the `f64`s between: non-negative `f64`s order as
/// their bit patterns do.
fn least_reaching(code: u8) -> f64 {
    let (mut lo, mut hi) = (0u64, 1.0f64.to_bits());
    if code_of(0.0) >= code {
        return 0.0;
    }
    // Invariant: code_of(lo) < code <= code_of(hi).
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if code_of(f64::from_bits(mid)) >= code {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    f64::from_bits(hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_encoding_band_holds_two_code_boundaries() {
        for i in 0..ENCODE_BANDS {
            let (start, end) = (
                i as f64 / ENCODE_BANDS as f64,
                (i + 1) as f64 / ENCODE_BANDS as f64,
            );
            let end = f64::from_bits(end.to_bits() - 1);
            assert!(code_of(end) - code_of(start) <= 1, "band {i}");
        }
    }

    #[test]
    fn encoding_is_the_srgb_curve_rounded() {
        let mut x: u64 = 0x2545_f491_4f6c_dd1d;
        let mut values = vec![-1.0, -0.0, 0.0, 1.0, 1.5, f64::NAN, f64::INFINITY];
        // Either side of every threshold, where rounding decides.
        for k in 1..=255u8 {
            let t = least_reaching(k);
            values.extend([
                t,
                f64::from_bits(t.to_bits() - 1),
                f64::from_bits(t.to_bits() + 1),
            ]);
        }
        for _ in 0..200_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            values.push((x >> 11) as f64 / (1u64 << 53) as f64);
        }
        for v in values {
            assert_eq!(ENCODE.code(v), code_of(v), "linear {v:e}");
        }
    }
}
