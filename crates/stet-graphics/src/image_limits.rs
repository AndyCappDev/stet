// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Bounds on file-declared image dimensions.
//!
//! `/Width`, `/Height`, and `/BitsPerComponent` are arbitrary integers taken
//! from the input, and every buffer size and loop bound downstream is derived
//! from them. Two things go wrong without a ceiling: the products overflow —
//! silently in release, where a wrapped size yields a buffer smaller than the
//! loops that fill it — and even where the arithmetic survives, a declared
//! size far larger than the data behind it is a denial of service. A 60-byte
//! PostScript file requesting a 2000000000 x 2000000000 image asks for a
//! 4 x 10^18 byte allocation, which aborts the process rather than failing.
//!
//! These live in `stet-graphics` because both input paths need them and
//! neither can see the other: the PostScript operators are in `stet-ops`, the
//! PDF image handler is in `stet-pdf-reader`, and `stet-pdf-reader`
//! deliberately does not depend on the interpreter. Duplicating the constants
//! would let two prepress-calibrated numbers drift apart.
//!
//! # Calibration
//!
//! **Sized for prepress, not for the sample corpus.** The corpus maximum is
//! 151M pixels, but that is a sample of ordinary documents and is the wrong
//! yardstick for a RIP. The sizes that matter:
//!
//! | Case | Pixels |
//! |---|---|
//! | 40x28 inch press sheet @ 600 dpi | 403M |
//! | A0 poster (33x47 in) @ 600 dpi | 558M |
//! | 60x40 inch grand format @ 600 dpi | 864M |
//! | 60x40 inch grand format @ 1200 dpi | 3.46G |
//!
//! An earlier 400M ceiling, calibrated from the corpus, rejected all four.

/// Largest accepted value for an image's width or height, in samples.
///
/// No prepress case comes close — the largest above is 72000 — while the
/// bound keeps every dimension-derived product finite.
pub const MAX_IMAGE_DIMENSION: i64 = 100_000;

/// Largest accepted pixel count (`width * height`) for a single image.
///
/// 4e9 rather than something larger because it must stay under `2^32`: a
/// number of sites compute `width * height` in `u32`, and that product is
/// only exact while it fits. Anything multiplying further by a component
/// count must use checked or saturating arithmetic instead.
pub const MAX_IMAGE_PIXELS: u64 = 4_000_000_000;

/// Largest accepted bits-per-component.
///
/// PDF 32000-1 permits 1, 2, 4, 8, and 16; PostScript adds 12. The value
/// reaches shift expressions such as `1u32 << bpc`, which panic in debug
/// builds at 32 or more.
pub const MAX_BITS_PER_COMPONENT: i64 = 16;

/// Validate one file-supplied image dimension.
///
/// Returns `None` for a missing, non-positive, or out-of-range value, so
/// callers reject the image rather than truncating it with an `as u32` cast —
/// a width of 4294967297 otherwise silently becomes a 1-pixel image.
pub fn validate_image_dimension(value: Option<i64>) -> Option<u32> {
    match value {
        Some(n) if n > 0 && n <= MAX_IMAGE_DIMENSION => Some(n as u32),
        _ => None,
    }
}

/// Validate a `width`/`height` pair and return the pixel count.
///
/// Guarantees `width * height` fits in `u32` and in `usize` — including the
/// 32-bit `usize` of the wasm32 target — so downstream sites may multiply the
/// two directly.
pub fn validate_image_size(width: u32, height: u32) -> Option<usize> {
    let pixels = u64::from(width).checked_mul(u64::from(height))?;
    if pixels == 0 || pixels > MAX_IMAGE_PIXELS {
        return None;
    }
    usize::try_from(pixels).ok()
}

/// Validate a file-supplied bits-per-component, falling back to 8 when absent.
pub fn validate_bits_per_component(value: Option<i64>) -> Option<u32> {
    match value {
        None => Some(8),
        Some(n) if n > 0 && n <= MAX_BITS_PER_COMPONENT => Some(n as u32),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepress_sizes_are_accepted() {
        // The four cases from the table above.
        for (label, w, h) in [
            ("40x28in press sheet @ 600dpi", 24_000, 16_800),
            ("A0 poster @ 600dpi", 19_800, 28_200),
            ("60x40in grand format @ 600dpi", 36_000, 24_000),
            ("60x40in grand format @ 1200dpi", 72_000, 48_000),
        ] {
            assert!(
                validate_image_size(w, h).is_some(),
                "{label} ({w}x{h}) must be accepted"
            );
        }
    }

    #[test]
    fn overflowing_and_degenerate_sizes_are_rejected() {
        // 65537 * 65536 overflows u32 to 65536 rather than wrapping to zero.
        assert!(validate_image_size(65_537, 65_536).is_none());
        assert!(validate_image_size(0, 100).is_none());
        assert!(validate_image_dimension(Some(0)).is_none());
        assert!(validate_image_dimension(Some(-1)).is_none());
        assert!(validate_image_dimension(Some(4_294_967_297)).is_none());
        assert!(validate_image_dimension(None).is_none());
    }

    #[test]
    fn pixel_ceiling_stays_within_u32() {
        assert!(
            MAX_IMAGE_PIXELS < u64::from(u32::MAX),
            "sites computing width * height in u32 depend on this"
        );
    }

    #[test]
    fn bits_per_component_is_bounded() {
        assert_eq!(validate_bits_per_component(None), Some(8));
        assert_eq!(validate_bits_per_component(Some(8)), Some(8));
        assert!(validate_bits_per_component(Some(99)).is_none());
        assert!(validate_bits_per_component(Some(0)).is_none());
    }
}
