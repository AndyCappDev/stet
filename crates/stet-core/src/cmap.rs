// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A CMap's codespace and CID mappings, read from its dictionary once and
//! indexed for lookup.
//!
//! Showing text with a CMap-keyed Type 0 font splits each string into
//! character codes and maps each code to a CID. Reading the tables out of the
//! CMap dictionary for every string made a large CMap (UniJIS-UCS2-H has about
//! 8,500 mappings) cost seconds over a few thousand `show`s, so they are read
//! once per CMap and kept in [`Context::cmap_tables`].
//!
//! [`Context::cmap_tables`]: crate::context::Context::cmap_tables

use rustc_hash::FxHashMap;

use crate::context::Context;
use crate::dict::DictKey;
use crate::object::{EntityId, PsObject, PsValue};

/// The longest character code a CMap may define, in bytes (Adobe TN 5014).
const MAX_CODE_LEN: usize = 4;

/// A CMap's codespace and CID mappings, its `usecmap` parents' included.
///
/// Mappings match a code of their own length only (TN 5014): a code no
/// mapping holds is `.notdef`. A CMap's own entries come before its parent's,
/// and within one CMap the first entry that holds a code wins.
#[derive(Debug, Default)]
pub struct CmapTables {
    codespace: Vec<(Vec<u8>, Vec<u8>)>,
    shortest: usize,
    chars: FxHashMap<(usize, u32), i32>,
    ranges: RangeTable,
    notdef_chars: FxHashMap<(usize, u32), i32>,
    notdef_ranges: RangeTable,
}

/// One `cidrange` or `notdefrange` entry, by code length.
#[derive(Debug, Clone, Copy)]
struct Range {
    lo: u32,
    hi: u32,
    cid: i32,
    /// Where the entry stands among its CMap's, parents' last: the first
    /// that holds a code wins.
    order: usize,
    /// The largest `hi` of this entry and every one sorted before it, so a
    /// lookup knows when no earlier entry can hold the code.
    max_hi: u32,
}

/// Ranges sorted by their low end, one list per code length.
#[derive(Debug, Default)]
struct RangeTable {
    by_len: [Vec<Range>; MAX_CODE_LEN + 1],
}

impl RangeTable {
    fn push(&mut self, lo: &[u8], hi: &[u8], cid: i32, order: usize) {
        if lo.len() != hi.len() || lo.is_empty() || lo.len() > MAX_CODE_LEN {
            return;
        }
        self.by_len[lo.len()].push(Range {
            lo: value(lo),
            hi: value(hi),
            cid,
            order,
            max_hi: 0,
        });
    }

    fn finish(&mut self) {
        for list in &mut self.by_len {
            list.sort_by_key(|r| (r.lo, r.order));
            let mut max_hi = 0;
            for r in list.iter_mut() {
                max_hi = max_hi.max(r.hi);
                r.max_hi = max_hi;
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.by_len.iter().all(Vec::is_empty)
    }

    /// The CID of code `c`, `len` bytes long, from the first range that holds
    /// it.
    fn find(&self, len: usize, c: u32) -> Option<i32> {
        let list = self.by_len.get(len)?;
        let mut i = list.partition_point(|r| r.lo <= c);
        let mut found: Option<(usize, i32)> = None;
        while i > 0 {
            i -= 1;
            let r = &list[i];
            if r.max_hi < c {
                break;
            }
            if c <= r.hi && found.is_none_or(|(order, _)| r.order < order) {
                found = Some((r.order, r.cid.wrapping_add((c - r.lo) as i32)));
            }
        }
        found.map(|(_, cid)| cid)
    }
}

/// A code's bytes as a number, big-endian.
fn value(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0u32, |a, &b| (a << 8) | b as u32)
}

impl CmapTables {
    /// Read a CMap dictionary, then the CMap it names with `usecmap`: its own
    /// entries come first and so win over the parent's.
    pub fn read(ctx: &Context, cmap_entity: EntityId) -> Self {
        let mut tables = Self::default();
        let mut order = 0;
        let mut entity = Some(cmap_entity);
        // A usecmap chain is short; the cap stops a CMap that names itself.
        for _ in 0..8 {
            let Some(e) = entity else { break };
            tables.add(ctx, e, &mut order);
            entity = ctx
                .names
                .find(b"UseCMap")
                .and_then(|id| ctx.dicts.get(e, &DictKey::Name(id)))
                .and_then(|obj| match obj.value {
                    PsValue::Dict(parent) if parent != e => Some(parent),
                    _ => None,
                });
        }
        tables.ranges.finish();
        tables.notdef_ranges.finish();
        tables.shortest = tables
            .codespace
            .iter()
            .map(|(lo, _)| lo.len())
            .min()
            .unwrap_or(1)
            .max(1);
        tables
    }

    fn add(&mut self, ctx: &Context, cmap: EntityId, order: &mut usize) {
        let array = |key: &[u8]| -> Vec<PsObject> {
            ctx.names
                .find(key)
                .and_then(|id| ctx.dicts.get(cmap, &DictKey::Name(id)))
                .and_then(|obj| match obj.value {
                    PsValue::Array { entity, start, len } => {
                        Some(ctx.arrays.get(entity, start, len).to_vec())
                    }
                    _ => None,
                })
                .unwrap_or_default()
        };
        let bytes = |obj: &PsObject| match obj.value {
            PsValue::String { entity, start, len } => Some(ctx.strings.get(entity, start, len)),
            _ => None,
        };
        self.codespace.extend(
            array(b"CodeSpaceRange")
                .as_chunks::<2>()
                .0
                .iter()
                .filter_map(|[lo, hi]| Some((bytes(lo)?.to_vec(), bytes(hi)?.to_vec()))),
        );
        let chars = |key: &[u8], into: &mut FxHashMap<(usize, u32), i32>| {
            for [src, dst] in array(key).as_chunks::<2>().0 {
                if let (Some(src), Some(cid)) = (bytes(src), dst.as_i32())
                    && (1..=MAX_CODE_LEN).contains(&src.len())
                {
                    into.entry((src.len(), value(src))).or_insert(cid);
                }
            }
        };
        chars(b"CIDCharMappings", &mut self.chars);
        chars(b"NotDefCharMappings", &mut self.notdef_chars);
        let mut ranges = |key: &[u8], into: &mut RangeTable| {
            for [lo, hi, cid] in array(key).as_chunks::<3>().0 {
                if let (Some(lo), Some(hi), Some(cid)) = (bytes(lo), bytes(hi), cid.as_i32()) {
                    into.push(lo, hi, cid, *order);
                    *order += 1;
                }
            }
        };
        ranges(b"CIDRangeMappings", &mut self.ranges);
        ranges(b"NotDefRangeMappings", &mut self.notdef_ranges);
    }

    /// Whether the CMap has no codespace, so its codes can't be read.
    pub fn is_empty(&self) -> bool {
        self.codespace.is_empty()
    }

    /// The length of the shortest codespace range, at least 1: a byte
    /// sequence no range holds is one code of this length.
    pub fn shortest(&self) -> usize {
        self.shortest
    }

    /// The length of the code at the start of `bytes`: the shortest that a
    /// codespace range holds, byte by byte (Adobe TN 5014). None when no
    /// range does.
    pub fn code_len(&self, bytes: &[u8]) -> Option<usize> {
        (1..=MAX_CODE_LEN.min(bytes.len())).find(|&n| {
            let code = &bytes[..n];
            self.codespace.iter().any(|(lo, hi)| {
                lo.len() == n
                    && hi.len() == n
                    && code
                        .iter()
                        .zip(lo.iter().zip(hi))
                        .all(|(&b, (&l, &h))| l <= b && b <= h)
            })
        })
    }

    /// The CID of `code`: a cidchar, then a cidrange, then a notdef mapping,
    /// each of the code's own length. With no CID mappings at all, the code
    /// itself; otherwise a code nothing maps is CID 0.
    pub fn cid(&self, code: &[u8]) -> i32 {
        let (len, c) = (code.len(), value(code));
        if let Some(cid) = self
            .chars
            .get(&(len, c))
            .copied()
            .or_else(|| self.ranges.find(len, c))
            .or_else(|| self.notdef_chars.get(&(len, c)).copied())
            .or_else(|| self.notdef_ranges.find(len, c))
        {
            return cid;
        }
        if self.chars.is_empty() && self.ranges.is_empty() {
            c as i32
        } else {
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_range_that_holds_a_code_wins() {
        let mut t = RangeTable::default();
        t.push(&[0x00, 0x10], &[0x00, 0x20], 100, 0);
        t.push(&[0x00, 0x00], &[0x00, 0xff], 500, 1);
        t.push(&[0x01, 0x00], &[0x01, 0x05], 900, 2);
        t.finish();
        assert_eq!(t.find(2, 0x0015), Some(105));
        assert_eq!(t.find(2, 0x0030), Some(548));
        assert_eq!(t.find(2, 0x0103), Some(903));
        assert_eq!(t.find(2, 0x0106), None);
        assert_eq!(t.find(1, 0x15), None, "a one-byte code");
    }
}
