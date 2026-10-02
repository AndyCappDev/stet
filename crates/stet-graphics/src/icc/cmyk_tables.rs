// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A CMYK profile's CMYK → sRGB tables, one per rendering intent.
//!
//! Which A2B table an intent reads, and whether its black point is scaled to
//! sRGB black, follows lcms2 — the engine inside Ghostscript — converting a
//! CMYK profile to its built-in (v4) sRGB:
//!
//! | intent | table | black-point scaling |
//! |---|---|---|
//! | relative colorimetric | `A2B1` | as configured (`BpcMode`) |
//! | perceptual | `A2B0` | always |
//! | saturation | `A2B2` | always |
//! | absolute colorimetric | as relative colorimetric | |
//!
//! "Always" is lcms2 forcing compensation for perceptual and saturation when
//! either profile is ICC v4. *Which* black is scaled is lcms2's black-point
//! detector's call ([`super::black_point`]): for relative colorimetric on an
//! output profile, the ink-limited black; otherwise 400% ink. It finds none
//! for an intent the profile has no table for — saturation without `A2B2`,
//! say — and then nothing is scaled. A missing table is otherwise replaced
//! by `A2B0`, as the ICC specification says, then by whichever table the
//! profile has. Absolute colorimetric's white-point adaptation is not
//! applied, as on the RGB paths.
//!
//! `crates/stet-graphics/tests/cmyk_intent.rs` checks these rules against
//! lcms2's recorded output.
//!
//! Each table is a 245 KiB [`Clut4`] baked on first use, so a document that
//! never asks for an intent never pays for it. Intents that resolve to the
//! same table bytes and the same black point share one bake: on a profile
//! whose `A2B0` and `A2B1` are identical and whose `B2A0` reaches 400% ink,
//! perceptual and relative colorimetric (with BPC on) are one table.

use std::sync::{Arc, OnceLock};

use moxcms::{ColorProfile, Layout, LutWarehouse, RenderingIntent, TransformOptions};

use super::black_point::{self, SourceBlack};
use super::bpc::{self, BpcParams, compute_bpc_params, detect_source_black_point};
use super::{Clut4, bake_clut4, hand_rolled};

/// Grid points per axis of every baked table.
const GRID_N: u8 = 17;

/// One of a CMYK profile's device-to-PCS tables.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Table {
    A2B0,
    A2B1,
    A2B2,
}

impl Table {
    const ALL: [Table; 3] = [Table::A2B0, Table::A2B1, Table::A2B2];

    fn signature(self) -> &'static [u8; 4] {
        match self {
            Table::A2B0 => b"A2B0",
            Table::A2B1 => b"A2B1",
            Table::A2B2 => b"A2B2",
        }
    }

    fn warehouse(self, profile: &ColorProfile) -> Option<&LutWarehouse> {
        match self {
            Table::A2B0 => profile.lut_a_to_b_perceptual.as_ref(),
            Table::A2B1 => profile.lut_a_to_b_colorimetric.as_ref(),
            Table::A2B2 => profile.lut_a_to_b_saturation.as_ref(),
        }
    }

    /// The moxcms intent that reads this table.
    fn moxcms_intent(self) -> RenderingIntent {
        match self {
            Table::A2B0 => RenderingIntent::Perceptual,
            Table::A2B1 => RenderingIntent::RelativeColorimetric,
            Table::A2B2 => RenderingIntent::Saturation,
        }
    }
}

/// What one baked table is made from.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Recipe {
    table: Table,
    /// The black point to compensate from; `None` for none.
    black: Option<SourceBlack>,
}

/// The recipe `intent` uses (see the module docs). `present` says which
/// tables the profile has; `detect` is its black point for an intent.
fn recipe(
    intent: RenderingIntent,
    present: impl Fn(Table) -> bool,
    bpc: bool,
    detect: impl Fn(RenderingIntent) -> Option<SourceBlack>,
) -> Recipe {
    let resolve = |wanted: Table| {
        if present(wanted) {
            return wanted;
        }
        [Table::A2B0, Table::A2B1, Table::A2B2]
            .into_iter()
            .find(|&t| present(t))
            .unwrap_or(wanted)
    };
    let (table, scale_black) = match intent {
        RenderingIntent::Perceptual => (resolve(Table::A2B0), true),
        RenderingIntent::Saturation => (resolve(Table::A2B2), true),
        RenderingIntent::RelativeColorimetric | RenderingIntent::AbsoluteColorimetric => {
            (resolve(Table::A2B1), bpc)
        }
    };
    Recipe {
        table,
        black: if scale_black { detect(intent) } else { None },
    }
}

/// The bytes of tag `sig` in the ICC profile `icc`.
fn tag_data<'a>(icc: &'a [u8], sig: &[u8; 4]) -> Option<&'a [u8]> {
    let count = u32::from_be_bytes(icc.get(128..132)?.try_into().ok()?) as usize;
    for i in 0..count {
        let entry = icc.get(132 + 12 * i..144 + 12 * i)?;
        if &entry[0..4] == sig {
            let offset = u32::from_be_bytes(entry[4..8].try_into().ok()?) as usize;
            let len = u32::from_be_bytes(entry[8..12].try_into().ok()?) as usize;
            return icc.get(offset..offset.checked_add(len)?);
        }
    }
    None
}

/// A CMYK profile's CMYK → sRGB tables, one per rendering intent, each baked
/// on first use. Shared by `Arc` between the profile's cache entry and any
/// proofing chain that ends in this profile.
pub(super) struct CmykTables {
    profile: Arc<ColorProfile>,
    /// The profile's raw bytes, which hold a v4 table's curve sets.
    icc: Arc<[u8]>,
    /// The slot each intent uses, indexed by the moxcms discriminant.
    slot_of: [usize; 4],
    /// What each slot is baked from; no two are equal.
    recipes: Vec<Recipe>,
    /// The baked tables. `None` inside means the bake failed.
    slots: Vec<OnceLock<Option<Clut4>>>,
}

impl CmykTables {
    /// Work out each intent's table for `profile`, whose raw bytes are `icc`.
    /// Bakes nothing.
    pub(super) fn new(profile: Arc<ColorProfile>, icc: &[u8], bpc: bool) -> Self {
        let present = |t: Table| t.warehouse(&profile).is_some();
        // Equal bytes are one table, whichever tags point at them.
        let canonical = |t: Table| {
            let Some(bytes) = tag_data(icc, t.signature()) else {
                return t;
            };
            Table::ALL
                .into_iter()
                .find(|&o| present(o) && tag_data(icc, o.signature()) == Some(bytes))
                .unwrap_or(t)
        };
        // Detected once per intent: relative colorimetric's is a round trip
        // through two tables, and absolute colorimetric shares it.
        let detected = [
            RenderingIntent::Perceptual,
            RenderingIntent::RelativeColorimetric,
            RenderingIntent::Saturation,
        ]
        .map(|intent| black_point::detect(&profile, icc, intent));
        let detect = |intent: RenderingIntent| match intent {
            RenderingIntent::Perceptual => detected[0],
            RenderingIntent::Saturation => detected[2],
            _ => detected[1],
        };
        let mut recipes: Vec<Recipe> = Vec::with_capacity(4);
        let mut slot_of = [0; 4];
        for intent in [
            RenderingIntent::Perceptual,
            RenderingIntent::RelativeColorimetric,
            RenderingIntent::Saturation,
            RenderingIntent::AbsoluteColorimetric,
        ] {
            let mut r = recipe(intent, present, bpc, detect);
            r.table = canonical(r.table);
            slot_of[intent as usize] = match recipes.iter().position(|&x| x == r) {
                Some(slot) => slot,
                None => {
                    recipes.push(r);
                    recipes.len() - 1
                }
            };
        }
        let slots = recipes.iter().map(|_| OnceLock::new()).collect();
        Self {
            profile,
            icc: icc.into(),
            slot_of,
            recipes,
            slots,
        }
    }

    /// The table for `intent`, baking it if this is its first use. `None`
    /// when the profile cannot be baked at all.
    pub(super) fn get(&self, intent: RenderingIntent) -> Option<&Clut4> {
        let slot = self.slot_of[intent as usize];
        self.slots[slot]
            .get_or_init(|| self.bake(self.recipes[slot]))
            .as_ref()
    }

    /// Whether `a` and `b` use the same table.
    #[cfg(test)]
    fn shared(&self, a: RenderingIntent, b: RenderingIntent) -> bool {
        self.slot_of[a as usize] == self.slot_of[b as usize]
    }

    /// Whether stet's own evaluators bake `intent`'s table, compensating
    /// its black point themselves. When they do not, the table is sampled
    /// from a moxcms transform instead.
    pub(super) fn hand_rolled(&self, intent: RenderingIntent) -> bool {
        let table = self.recipes[self.slot_of[intent as usize]].table;
        table.warehouse(&self.profile).is_some_and(|t| {
            hand_rolled::can_sample(&self.profile, t)
                || hand_rolled::can_bake_lcms(&self.profile, &self.icc, (t, table.signature()))
        })
    }

    /// Black-point compensation for `intent` on a path that samples the
    /// moxcms `transform` rather than one of these tables: the per-pixel
    /// fallback, and the fallback bake. The darker colorant is taken from
    /// `transform`'s output.
    pub(super) fn moxcms_bpc_params(
        &self,
        intent: RenderingIntent,
        transform: &dyn moxcms::TransformExecutor<u8>,
    ) -> Option<BpcParams> {
        moxcms_bpc_params(self.recipes[self.slot_of[intent as usize]].black, transform)
    }

    /// Bake `recipe`, by the first of these that reads its table:
    /// 1. the `lut16Type` sampler ([`hand_rolled::bake_clut4_hand_rolled`]);
    /// 2. the evaluator that reproduces lcms2, for ICC v4 `lutAToBType` and
    ///    `lut8Type` tables ([`hand_rolled::bake_clut4_lcms`]);
    /// 3. moxcms's transform for the same table, now only for an XYZ PCS.
    ///
    /// `lut16Type` keeps its own sampler, though the second would read it
    /// too, so that those profiles' tables do not move.
    fn bake(&self, recipe: Recipe) -> Option<Clut4> {
        if let Some(table) = recipe.table.warehouse(&self.profile) {
            let grid_n = GRID_N as usize;
            if let Some(clut) =
                hand_rolled::bake_clut4_hand_rolled(&self.profile, table, grid_n, recipe.black)
            {
                return Some(clut);
            }
            if let Some(clut) = hand_rolled::bake_clut4_lcms(
                &self.profile,
                &self.icc,
                (table, recipe.table.signature()),
                grid_n,
                recipe.black,
            ) {
                return Some(clut);
            }
        }
        let transform = moxcms_transform(&self.profile, recipe.table.moxcms_intent())?;
        let params = moxcms_bpc_params(recipe.black, transform.as_ref());
        bake_clut4(transform.as_ref(), GRID_N, params.as_ref())
    }
}

/// Black-point compensation from `black` for output sampled from the
/// moxcms `transform`, which supplies the darker colorant.
fn moxcms_bpc_params(
    black: Option<SourceBlack>,
    transform: &dyn moxcms::TransformExecutor<u8>,
) -> Option<BpcParams> {
    let sbp = match black? {
        SourceBlack::DarkerColorant => detect_source_black_point(transform)?,
        SourceBlack::Xyz(xyz) => xyz,
    };
    Some(compute_bpc_params(sbp, [0.0; 3], bpc::WP_D50))
}

/// moxcms's 8-bit CMYK → sRGB transform reading `intent`'s table, or, when
/// the profile has none it can build, the first other intent's.
fn moxcms_transform(
    profile: &ColorProfile,
    intent: RenderingIntent,
) -> Option<Arc<dyn moxcms::TransformExecutor<u8> + Send + Sync>> {
    let srgb = ColorProfile::new_srgb();
    [
        intent,
        RenderingIntent::Perceptual,
        RenderingIntent::RelativeColorimetric,
        RenderingIntent::Saturation,
    ]
    .into_iter()
    .find_map(|intent| {
        let options = TransformOptions {
            rendering_intent: intent,
            ..TransformOptions::default()
        };
        profile
            .create_transform_8bit(Layout::Rgba, &srgb, Layout::Rgb, options)
            .ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPLIT: &[u8] = include_bytes!("../../tests/data/cmyk_intent/split.icc");
    const SPLIT_SAT: &[u8] = include_bytes!("../../tests/data/cmyk_intent/split_sat.icc");
    const INKLIMIT: &[u8] = include_bytes!("../../tests/data/cmyk_intent/inklimit.icc");
    const SPLIT_LUT8: &[u8] = include_bytes!("../../tests/data/cmyk_intent/split_lut8.icc");
    const SPLIT_XYZ: &[u8] = include_bytes!("../../tests/data/cmyk_intent/split_xyz.icc");
    const MAB: &[u8] = include_bytes!("../../tests/data/cmyk_intent/mab.icc");

    fn tables(icc: &[u8], bpc: bool) -> CmykTables {
        let profile = Arc::new(ColorProfile::new_from_slice(icc).unwrap());
        CmykTables::new(profile, icc, bpc)
    }

    /// `icc` with tag `from`'s entry pointed at tag `to`'s bytes.
    fn alias_tag(icc: &[u8], from: &[u8; 4], to: &[u8; 4]) -> Vec<u8> {
        let mut out = icc.to_vec();
        let count = u32::from_be_bytes(icc[128..132].try_into().unwrap()) as usize;
        let entry = |sig: &[u8; 4]| {
            (0..count)
                .map(|i| 132 + 12 * i)
                .find(|&at| &icc[at..at + 4] == sig)
                .unwrap()
        };
        let (src, dst) = (entry(to), entry(from));
        out[dst + 4..dst + 12].copy_from_slice(&icc[src + 4..src + 12]);
        out
    }

    use RenderingIntent::*;

    #[test]
    fn distinct_tables_are_distinct_slots() {
        let t = tables(SPLIT_SAT, true);
        assert!(!t.shared(Perceptual, RelativeColorimetric));
        assert!(!t.shared(Saturation, Perceptual));
        assert!(!t.shared(Saturation, RelativeColorimetric));
        assert!(t.shared(AbsoluteColorimetric, RelativeColorimetric));
    }

    /// Saturation with no `A2B2` reads `A2B0`, unscaled — not Perceptual's
    /// slot, which is scaled.
    #[test]
    fn saturation_without_its_table_is_unscaled_a2b0() {
        let t = tables(SPLIT, true);
        assert_eq!(
            t.recipes[t.slot_of[Saturation as usize]],
            Recipe {
                table: Table::A2B0,
                black: None
            }
        );
        assert!(!t.shared(Saturation, Perceptual));
    }

    /// Identical `A2B0` and `A2B1` with BPC on: one table serves both.
    /// With BPC off they differ, because perceptual is always scaled.
    #[test]
    fn equal_tables_share_a_slot() {
        let icc = alias_tag(SPLIT, b"A2B0", b"A2B1");
        assert!(tables(&icc, true).shared(Perceptual, RelativeColorimetric));
        assert!(!tables(&icc, false).shared(Perceptual, RelativeColorimetric));
    }

    /// Identical tables, but a `B2A0` that stops short of 400% ink: relative
    /// colorimetric compensates from the ink-limited black and perceptual
    /// from 400%, so they are two tables.
    #[test]
    fn an_ink_limit_separates_identical_tables() {
        let icc = alias_tag(INKLIMIT, b"A2B0", b"A2B1");
        let t = tables(&icc, true);
        assert!(!t.shared(Perceptual, RelativeColorimetric));
        assert!(matches!(
            t.recipes[t.slot_of[RelativeColorimetric as usize]].black,
            Some(SourceBlack::Xyz(_))
        ));
    }

    #[test]
    fn tables_are_baked_on_first_use() {
        let t = tables(SPLIT_SAT, true);
        assert!(t.slots.iter().all(|s| s.get().is_none()));
        assert!(t.get(Perceptual).is_some());
        let baked = t.slots.iter().filter(|s| s.get().is_some()).count();
        assert_eq!(baked, 1);
    }

    /// Each table shape has its own bake: `lut16Type` keeps its sampler,
    /// byte for byte, though lcms2's evaluator reads it too; `lut8Type` and
    /// ICC v4 go to that evaluator. Both count as stet's own, so `IccCache`
    /// takes no compensation from moxcms for them. An XYZ PCS is left to
    /// moxcms.
    #[test]
    fn each_table_shape_has_its_bake() {
        let grid_n = GRID_N as usize;
        for (name, icc, lut16) in [
            ("split_sat", SPLIT_SAT, true),
            ("split_lut8", SPLIT_LUT8, false),
            ("mab", MAB, false),
        ] {
            let t = tables(icc, true);
            for intent in [Perceptual, RelativeColorimetric, Saturation] {
                let recipe = t.recipes[t.slot_of[intent as usize]];
                let table = recipe.table.warehouse(&t.profile).unwrap();
                let want = if lut16 {
                    hand_rolled::bake_clut4_hand_rolled(&t.profile, table, grid_n, recipe.black)
                } else {
                    let table = (table, recipe.table.signature());
                    hand_rolled::bake_clut4_lcms(&t.profile, icc, table, grid_n, recipe.black)
                };
                let got = t.get(intent).unwrap();
                assert!(got.data == want.unwrap().data, "{name} {intent:?}");
                assert!(t.hand_rolled(intent), "{name} {intent:?}");
            }
        }
        let xyz = tables(SPLIT_XYZ, true);
        assert!(xyz.get(RelativeColorimetric).is_some());
        assert!(!xyz.hand_rolled(RelativeColorimetric));
    }

    #[test]
    fn missing_tag_reads_none() {
        assert!(tag_data(SPLIT, b"A2B2").is_none());
        assert!(tag_data(SPLIT_SAT, b"A2B2").is_some());
        assert!(tag_data(&[0; 40], b"A2B0").is_none());
    }
}
