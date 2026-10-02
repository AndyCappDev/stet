// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Image samples at the depth a file encodes them.
//!
//! The display list carries 8-bit samples, so both front ends reduce deeper
//! ones — 16-bit in PDF and PostScript, 12-bit in PostScript — with
//! [`to_8bit`]. A colour-key mask (a PDF image's `/Mask` array, PostScript
//! ImageType 4's `MaskColor`) names samples as encoded, before any reduction
//! or `/Decode`, so [`ColorKey`] tests them there.
//!
//! These live in `stet-graphics` for the reason
//! [`image_limits`](crate::image_limits) does: both input paths need them,
//! and neither can see the other.

/// `value`, a sample of `bpc` bits, as the nearest 8-bit value:
/// `round(value · 255 / (2^bpc − 1))`, in integers and exact. No sample of
/// any depth lies halfway between two 8-bit values, since `2 · value · 255`
/// is even and an odd multiple of `2^bpc − 1` is odd.
///
/// `bpc` is from 1 to 16; a `value` above `2^bpc − 1` gives 255.
pub fn to_8bit(value: u16, bpc: u32) -> u8 {
    debug_assert!((1..=16).contains(&bpc), "bits per component {bpc}");
    let max = (1u32 << bpc.clamp(1, 16)) - 1;
    let v = u32::from(value).min(max);
    ((v * 255 + max / 2) / max) as u8
}

/// A colour-key mask: an inclusive range of encoded sample values for each
/// component. A pixel whose every component lies in its range is not
/// painted (ISO 32000-1 §8.9.6.4; PLRM 4.10.6, ImageType 4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColorKey {
    bpc: u32,
    /// `[min, max]` per component, both within the depth's samples.
    ranges: Vec<[u16; 2]>,
}

impl ColorKey {
    /// The key `values` give an image of `components` components of `bpc`
    /// bits each: `2 × components` values, a minimum and a maximum for each
    /// component, or `components` values, one exact value each (PostScript
    /// only). Each range is clipped to the samples `bpc` bits hold.
    ///
    /// `None` when `values` has another length, when `bpc` is outside 1 to
    /// 16, or when some range holds no sample, so no pixel can match.
    pub fn new(values: &[i64], components: usize, bpc: u32) -> Option<Self> {
        if components == 0 || !(1..=16).contains(&bpc) {
            return None;
        }
        let pairs: Vec<[i64; 2]> = if values.len() == 2 * components {
            values.as_chunks::<2>().0.to_vec()
        } else if values.len() == components {
            values.iter().map(|&v| [v, v]).collect()
        } else {
            return None;
        };
        let max = (1i64 << bpc) - 1;
        let ranges = pairs
            .into_iter()
            .map(|[lo, hi]| {
                let (lo, hi) = (lo.max(0), hi.min(max));
                (lo <= hi).then_some([lo as u16, hi as u16])
            })
            .collect::<Option<_>>()?;
        Some(Self { bpc, ranges })
    }

    /// The alpha of each pixel of `packed`: 0 where the key masks the pixel,
    /// 255 elsewhere, one byte per pixel in row order. `packed` holds
    /// `width` × `height` pixels of this key's components at its depth,
    /// most significant bit first, each row starting on a byte, as PDF and
    /// PostScript both lay samples out. Samples past the end of `packed`
    /// read as 0, as the front ends unpack them.
    ///
    /// `None` when the key masks no pixel.
    pub fn alpha(&self, packed: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
        let (width, height) = (width as usize, height as usize);
        let n = self.ranges.len();
        let row_bytes = (width * n * self.bpc as usize).div_ceil(8);
        let mut alpha = vec![255u8; width.saturating_mul(height)];
        let mut any = false;
        for (y, row_alpha) in alpha.chunks_exact_mut(width.max(1)).enumerate() {
            let row = packed.get(y * row_bytes..).unwrap_or(&[]);
            for (x, a) in row_alpha.iter_mut().enumerate() {
                let masked = self.ranges.iter().enumerate().all(|(c, &[lo, hi])| {
                    let s = sample(row, x * n + c, self.bpc);
                    lo <= s && s <= hi
                });
                if masked {
                    *a = 0;
                    any = true;
                }
            }
        }
        any.then_some(alpha)
    }

    /// The key over samples that `map` reduces to 8 bits, as
    /// [`ImageParams::mask_color`](crate::device::ImageParams::mask_color)
    /// holds it: a minimum and a maximum for each component.
    ///
    /// The same test only when `map` is strictly increasing over the
    /// samples, as for samples of 8 bits or fewer expanded by [`to_8bit`],
    /// or palette indices kept as they are: then a reduced sample lies in
    /// the reduced range exactly when the sample lies in the range. A deeper
    /// sample, or one through a `/Decode`, needs [`Self::alpha`].
    pub fn ranges_8bit(&self, map: impl Fn(u16) -> u8) -> Vec<u8> {
        self.ranges
            .iter()
            .flat_map(|&[lo, hi]| [map(lo), map(hi)])
            .collect()
    }
}

/// Sample `index` of `row` at `bpc` bits, most significant bit first; 0 past
/// the end of the data.
fn sample(row: &[u8], index: usize, bpc: u32) -> u16 {
    let byte = |i: usize| u32::from(row.get(i).copied().unwrap_or(0));
    match bpc {
        8 => byte(index) as u16,
        16 => (byte(2 * index) << 8 | byte(2 * index + 1)) as u16,
        _ => {
            // At most 7 bits of offset and 16 of sample: three bytes hold it.
            let bit = index * bpc as usize;
            let (at, shift) = (bit / 8, (bit % 8) as u32);
            let word = byte(at) << 16 | byte(at + 1) << 8 | byte(at + 2);
            ((word >> (24 - shift - bpc)) & ((1 << bpc) - 1)) as u16
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_8bit_rounds_to_nearest() {
        for bpc in 1..=16 {
            let max = (1u32 << bpc) - 1;
            for v in 0..=max {
                let want = (f64::from(v) * 255.0 / f64::from(max)).round() as u8;
                assert_eq!(to_8bit(v as u16, bpc), want, "{v} at {bpc} bits");
            }
        }
    }

    /// `v · 255 / 65535` is `v / 257`: 128.5 lies between 0x8100 and 0x8101,
    /// and 0x10FF, 16.93, rounds up where its high byte says 16.
    #[test]
    fn to_8bit_16bit_boundaries() {
        assert_eq!(to_8bit(0x8100, 16), 0x80);
        assert_eq!(to_8bit(0x8101, 16), 0x81);
        assert_eq!(to_8bit(0x10FF, 16), 0x11);
        assert_eq!(to_8bit(0x0080, 16), 0x00);
        assert_eq!(to_8bit(0x0081, 16), 0x01);
        assert_eq!(to_8bit(0xFFFF, 16), 0xFF);
    }

    #[test]
    fn to_8bit_is_identity_at_8_bits() {
        for v in 0..=255u16 {
            assert_eq!(to_8bit(v, 8), v as u8);
        }
    }

    #[test]
    fn new_reads_ranges_and_exact_values() {
        let key = ColorKey::new(&[1, 2, 3, 4], 2, 8).unwrap();
        assert_eq!(key.ranges, [[1, 2], [3, 4]]);
        let key = ColorKey::new(&[5, 6], 2, 8).unwrap();
        assert_eq!(key.ranges, [[5, 5], [6, 6]]);
        assert_eq!(ColorKey::new(&[1, 2, 3], 2, 8), None);
        assert_eq!(ColorKey::new(&[], 0, 8), None);
    }

    /// A range is clipped to the depth, not wrapped: `33023 as u8` is 255.
    #[test]
    fn new_clips_ranges_to_the_depth() {
        let key = ColorKey::new(&[32768, 33023], 1, 16).unwrap();
        assert_eq!(key.ranges, [[32768, 33023]]);
        let key = ColorKey::new(&[-5, 300], 1, 8).unwrap();
        assert_eq!(key.ranges, [[0, 255]]);
        let key = ColorKey::new(&[10, 20], 1, 4).unwrap();
        assert_eq!(key.ranges, [[10, 15]]);
        // Nothing a 4-bit sample can hold.
        assert_eq!(ColorKey::new(&[16, 20], 1, 4), None);
        assert_eq!(ColorKey::new(&[5, 4], 1, 8), None);
        assert_eq!(ColorKey::new(&[-2, -1], 1, 8), None);
    }

    #[test]
    fn alpha_tests_samples_at_every_depth() {
        // Two pixels a row, two rows, one component; the first pixel of
        // each row is in range. Rows start on a byte.
        let cases: [(u32, &[u8], [i64; 2]); 6] = [
            (1, &[0b1000_0000, 0b1000_0000], [1, 1]),
            (2, &[0b1101_0000, 0b1110_0000], [3, 3]),
            (4, &[0xF1, 0xF2], [15, 15]),
            (8, &[200, 50, 200, 51], [200, 200]),
            (12, &[0x80, 0x01, 0x00, 0x80, 0x01, 0x00], [0x800, 0x800]),
            (
                16,
                &[0x80, 0x40, 0x10, 0x00, 0x80, 0xFF, 0x7F, 0xFF],
                [32768, 33023],
            ),
        ];
        for (bpc, data, range) in cases {
            let key = ColorKey::new(&range, 1, bpc).unwrap();
            assert_eq!(
                key.alpha(data, 2, 2),
                Some(vec![0, 255, 0, 255]),
                "{bpc} bits"
            );
        }
    }

    #[test]
    fn alpha_needs_every_component_in_range() {
        let key = ColorKey::new(&[10, 20, 0, 0, 100, 255], 3, 8).unwrap();
        let data = [15, 0, 200, 15, 1, 200, 25, 0, 200];
        assert_eq!(key.alpha(&data, 3, 1), Some(vec![0, 255, 255]));
    }

    #[test]
    fn alpha_is_none_when_nothing_is_masked() {
        let key = ColorKey::new(&[7, 7], 1, 8).unwrap();
        assert_eq!(key.alpha(&[1, 2, 3], 3, 1), None);
    }

    #[test]
    fn alpha_reads_missing_samples_as_zero() {
        let key = ColorKey::new(&[0, 0], 1, 16).unwrap();
        assert_eq!(key.alpha(&[0xFF, 0xFF, 0x00], 3, 1), Some(vec![255, 0, 0]));
    }

    #[test]
    fn ranges_8bit_maps_each_bound() {
        let key = ColorKey::new(&[1, 2, 0, 15], 2, 4).unwrap();
        assert_eq!(key.ranges_8bit(|v| to_8bit(v, 4)), [17, 34, 0, 255]);
        assert_eq!(key.ranges_8bit(|v| v as u8), [1, 2, 0, 15]);
    }
}
