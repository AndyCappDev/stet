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
use crate::object::{EntityId, NameId, PsObject, PsValue};

/// The longest character code a CMap may define, in bytes (Adobe TN 5014).
const MAX_CODE_LEN: usize = 4;

/// What a CMap maps a character code to (PLRM 5.11.4): the number of a font
/// in the Type 0 font's `FDepVector`, and the character selector that picks
/// a glyph in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    /// The font number: the `usefont` in effect where the mapping was
    /// written, 0 if none.
    pub font: usize,
    /// The character selector.
    pub selector: Selector,
}

/// A character selector (PLRM 5.11.4): what a CMap gives for a code in the
/// font its mapping names.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selector {
    /// A CID in a CIDFont, from `cidchar`, `cidrange` or a notdef mapping.
    Cid(i32),
    /// A character code in a base font, from a `bfchar` or `bfrange` whose
    /// target is a string: the value of the string's bytes, big-endian.
    Code(u32),
    /// A glyph name in a base font, from a `bfchar` or `bfrange` whose
    /// target is a name.
    Name(NameId),
    /// No mapping holds the code: `.notdef` in a base font, CID 0 in a
    /// CIDFont.
    Notdef,
}

/// A CMap's codespace and mappings, its `usecmap` parents' included.
///
/// Mappings match a code of their own length only (TN 5014): a code no
/// mapping holds is `.notdef`. [`decode`](Self::decode) takes a code's
/// length from the codespace, but uses a mapping of another length when no
/// mapping of the codespace's length holds the code, as Ghostscript does.
/// A code takes its target from the nearest CMap
/// in the `usecmap` chain that maps it, the CMap itself first: within that
/// CMap a `cidchar` or `bfchar` comes before a `cidrange` or `bfrange`, and
/// the first entry that holds the code wins. Notdef mappings are looked at
/// only when no character mapping holds the code.
#[derive(Debug, Default)]
pub struct CmapTables {
    codespace: Vec<(Vec<u8>, Vec<u8>)>,
    shortest: usize,
    chars: FxHashMap<(usize, u32), Mapped>,
    ranges: RangeTable,
    notdef_chars: FxHashMap<(usize, u32), Mapped>,
    notdef_ranges: RangeTable,
}

/// A target and the level of the CMap that maps it: 0 for the CMap itself,
/// 1 for its `usecmap` parent, and so on.
type Mapped = (usize, Target);

/// The nearer of two mappings of one code, `a` on a tie.
fn nearest(a: Option<Mapped>, b: Option<Mapped>) -> Option<Mapped> {
    match (a, b) {
        (Some(a), Some(b)) if b.0 < a.0 => Some(b),
        (Some(a), _) => Some(a),
        (None, b) => b,
    }
}

/// What a range entry maps its first code to; each later code adds its
/// offset from the range's low end.
#[derive(Debug, Clone, Copy)]
enum RangeStart {
    /// A CID, counting up (`cidrange`).
    Cid(i32),
    /// One CID for every code in the range (`notdefrange`: TN 5014, and
    /// Ghostscript's `CODE_VALUE_NOTDEF`, which adds no offset).
    SameCid(i32),
    /// A character code, counting up (`bfrange` with a string).
    Code(u32),
    /// One selector per code, `len` of them from `start` in
    /// [`RangeTable::lists`] (`bfrange` with an array).
    List { start: u32, len: u32 },
}

/// One range entry, by code length.
#[derive(Debug, Clone, Copy)]
struct Range {
    lo: u32,
    hi: u32,
    first: RangeStart,
    font: usize,
    /// The level of the CMap the entry is in (see [`Mapped`]).
    level: usize,
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
    /// The selectors of the `bfrange` entries whose target is an array.
    lists: Vec<Selector>,
}

impl RangeTable {
    fn push(
        &mut self,
        lo: &[u8],
        hi: &[u8],
        first: RangeStart,
        font: usize,
        level: usize,
        order: usize,
    ) {
        if lo.len() != hi.len() || lo.is_empty() || lo.len() > MAX_CODE_LEN {
            return;
        }
        self.by_len[lo.len()].push(Range {
            lo: value(lo),
            hi: value(hi),
            first,
            font,
            level,
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

    /// The target of code `c`, `len` bytes long, from the first range that
    /// holds it, with that range's level. Entries are ordered by level first,
    /// so that range is also in the nearest CMap that has one.
    fn find(&self, len: usize, c: u32) -> Option<Mapped> {
        let list = self.by_len.get(len)?;
        let mut i = list.partition_point(|r| r.lo <= c);
        let mut found: Option<&Range> = None;
        while i > 0 {
            i -= 1;
            let r = &list[i];
            if r.max_hi < c {
                break;
            }
            if c <= r.hi && found.is_none_or(|f| r.order < f.order) {
                found = Some(r);
            }
        }
        found.map(|r| {
            let offset = c - r.lo;
            let selector = match r.first {
                RangeStart::Cid(cid) => Selector::Cid(cid.wrapping_add(offset as i32)),
                RangeStart::SameCid(cid) => Selector::Cid(cid),
                RangeStart::Code(code) => Selector::Code(code.wrapping_add(offset)),
                RangeStart::List { start, len } if offset < len => {
                    self.lists[(start + offset) as usize]
                }
                RangeStart::List { .. } => Selector::Notdef,
            };
            (
                r.level,
                Target {
                    font: r.font,
                    selector,
                },
            )
        })
    }
}

/// A code's bytes as a number, big-endian.
fn value(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0u32, |a, &b| (a << 8) | b as u32)
}

/// The kinds of mapping a CMap dictionary holds, under the keys CIDInit
/// writes them to.
#[derive(Clone, Copy)]
enum Kind {
    CidChar,
    CidRange,
    BfChar,
    BfRange,
    NotdefChar,
    NotdefRange,
}

impl Kind {
    fn key(self) -> &'static [u8] {
        match self {
            Kind::CidChar => b"CIDCharMappings",
            Kind::CidRange => b"CIDRangeMappings",
            Kind::BfChar => b"BFCharMappings",
            Kind::BfRange => b"BFRangeMappings",
            Kind::NotdefChar => b"NotDefCharMappings",
            Kind::NotdefRange => b"NotDefRangeMappings",
        }
    }

    /// Objects per entry: source and target, or low, high and target.
    fn stride(self) -> usize {
        match self {
            Kind::CidChar | Kind::BfChar | Kind::NotdefChar => 2,
            Kind::CidRange | Kind::BfRange | Kind::NotdefRange => 3,
        }
    }
}

impl CmapTables {
    /// Read a CMap dictionary, then the CMap it names with `usecmap`, each at
    /// the next level: the CMap's own entries win over the parent's.
    pub fn read(ctx: &Context, cmap_entity: EntityId) -> Self {
        let mut tables = Self::default();
        let mut order = 0;
        let mut entity = Some(cmap_entity);
        // A usecmap chain is short; the cap stops a CMap that names itself.
        for level in 0..8 {
            let Some(e) = entity else { break };
            tables.add(ctx, e, level, &mut order);
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

    fn add(&mut self, ctx: &Context, cmap: EntityId, level: usize, order: &mut usize) {
        let get = |dict: EntityId, key: &[u8]| {
            ctx.names
                .find(key)
                .and_then(|id| ctx.dicts.get(dict, &DictKey::Name(id)))
        };
        let elements = |obj: Option<PsObject>| -> Vec<PsObject> {
            match obj.map(|o| o.value) {
                Some(PsValue::Array { entity, start, len }) => {
                    ctx.arrays.get(entity, start, len).to_vec()
                }
                _ => Vec::new(),
            }
        };
        let bytes = |obj: &PsObject| match obj.value {
            PsValue::String { entity, start, len } => Some(ctx.strings.get(entity, start, len)),
            _ => None,
        };
        // A base-font target: a string is a character code, a name a glyph.
        let bf_selector = |obj: &PsObject| match obj.value {
            PsValue::Name(id) => Some(Selector::Name(id)),
            _ => bytes(obj)
                .filter(|b| (1..=MAX_CODE_LEN).contains(&b.len()))
                .map(|b| Selector::Code(value(b))),
        };
        self.codespace.extend(
            elements(get(cmap, b"CodeSpaceRange"))
                .as_chunks::<2>()
                .0
                .iter()
                .filter_map(|[lo, hi]| Some((bytes(lo)?.to_vec(), bytes(hi)?.to_vec()))),
        );
        let font_runs = get(cmap, b".MappingFonts").and_then(|o| match o.value {
            PsValue::Dict(d) => Some(d),
            _ => None,
        });

        for kind in [
            Kind::CidChar,
            Kind::CidRange,
            Kind::BfChar,
            Kind::BfRange,
            Kind::NotdefChar,
            Kind::NotdefRange,
        ] {
            let entries = elements(get(cmap, kind.key()));
            // The font of each entry: the (length, font) runs CIDInit wrote
            // when some block is not font 0, in elements of `entries`.
            let runs = font_runs.map_or_else(Vec::new, |d| elements(get(d, kind.key())));
            let mut runs = runs
                .as_chunks::<2>()
                .0
                .iter()
                .filter_map(|[n, f]| {
                    Some((n.as_i32()?.max(0) as usize, f.as_i32()?.max(0) as usize))
                })
                .collect::<Vec<_>>()
                .into_iter();
            let (mut run_end, mut font) = (0, 0);
            let mut font_at = |element: usize| {
                while element >= run_end {
                    match runs.next() {
                        Some((n, f)) => (run_end, font) = (run_end + n, f),
                        None => (run_end, font) = (usize::MAX, 0),
                    }
                }
                font
            };

            for (i, entry) in entries.chunks_exact(kind.stride()).enumerate() {
                let font = font_at(i * kind.stride());
                match (kind, entry) {
                    (Kind::CidChar | Kind::BfChar | Kind::NotdefChar, [src, dst]) => {
                        let selector = match kind {
                            Kind::BfChar => bf_selector(dst),
                            _ => dst.as_i32().map(Selector::Cid),
                        };
                        if let (Some(src), Some(selector)) = (bytes(src), selector)
                            && (1..=MAX_CODE_LEN).contains(&src.len())
                        {
                            let into = match kind {
                                Kind::NotdefChar => &mut self.notdef_chars,
                                _ => &mut self.chars,
                            };
                            into.entry((src.len(), value(src)))
                                .or_insert((level, Target { font, selector }));
                        }
                    }
                    (_, [lo, hi, dst]) => {
                        let first = match (kind, dst.value) {
                            (Kind::BfRange, PsValue::Array { entity, start, len }) => {
                                let into = &mut self.ranges.lists;
                                let list_start = into.len() as u32;
                                into.extend(
                                    ctx.arrays
                                        .get(entity, start, len)
                                        .iter()
                                        .map(|o| bf_selector(o).unwrap_or(Selector::Notdef)),
                                );
                                Some(RangeStart::List {
                                    start: list_start,
                                    len,
                                })
                            }
                            (Kind::BfRange, _) => match bf_selector(dst) {
                                Some(Selector::Code(code)) => Some(RangeStart::Code(code)),
                                _ => None,
                            },
                            (Kind::NotdefRange, _) => dst.as_i32().map(RangeStart::SameCid),
                            _ => dst.as_i32().map(RangeStart::Cid),
                        };
                        if let (Some(lo), Some(hi), Some(first)) = (bytes(lo), bytes(hi), first) {
                            let into = match kind {
                                Kind::NotdefRange => &mut self.notdef_ranges,
                                _ => &mut self.ranges,
                            };
                            into.push(lo, hi, first, font, level, *order);
                            *order += 1;
                        }
                    }
                    _ => {}
                }
            }
        }
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
    /// range does. [`decode`](Self::decode) reads a code's length and target
    /// together, and also reads codes a CMap maps outside its codespace.
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

    /// The length and target of the code at the start of `bytes`, None when
    /// they hold no code: too few bytes for one, and none that a mapping
    /// holds.
    ///
    /// The codespace gives the length (TN 5014: "determined exclusively by
    /// the codespace ranges"). When no mapping of that length holds the code,
    /// or no codespace range holds the bytes, a mapping of another length
    /// that matches the bytes there gives it instead, the shortest first.
    /// Ghostscript ignores the codespace altogether and matches the mappings
    /// directly (`gs_cmap_adobe1_decode_next`), and some producers write
    /// CMaps that rely on it: Aspose.Words writes a codespace of `<0000>
    /// <FFFF>` over `bfchar` codes of one byte (#48). In a valid CMap, whose
    /// codespace no code is a prefix of another in, a mapping that matches
    /// the bytes is in a range of its own length, the length the codespace
    /// gives, so this changes nothing for it.
    ///
    /// Bytes no codespace range or mapping holds are one code as long as the
    /// shortest codespace range, and `.notdef` (PLRM 5.11.5).
    pub fn decode(&self, bytes: &[u8]) -> Option<(usize, Target)> {
        let len = self.code_len(bytes);
        if let Some(n) = len
            && let Some(target) = self.mapped(&bytes[..n])
        {
            return Some((n, target));
        }
        let other = (1..=MAX_CODE_LEN.min(bytes.len()))
            .filter(|&m| Some(m) != len)
            .find_map(|m| Some((m, self.mapped(&bytes[..m])?)));
        if other.is_some() {
            return other;
        }
        match len {
            Some(n) => Some((n, self.target(&bytes[..n]))),
            None if self.shortest <= bytes.len() => Some((
                self.shortest,
                Target {
                    font: 0,
                    selector: Selector::Notdef,
                },
            )),
            None => None,
        }
    }

    /// The target of `code`, when a mapping holds it: a character mapping,
    /// then a notdef mapping, each of the code's own length and from the
    /// nearest CMap that has one, where a char mapping comes before a range.
    fn mapped(&self, code: &[u8]) -> Option<Target> {
        let (len, c) = (code.len(), value(code));
        let key = (len, c);
        nearest(self.chars.get(&key).copied(), self.ranges.find(len, c))
            .or_else(|| {
                nearest(
                    self.notdef_chars.get(&key).copied(),
                    self.notdef_ranges.find(len, c),
                )
            })
            .map(|(_, target)| target)
    }

    /// The target of `code` (see [`Target`]). With no character mappings at
    /// all, the code itself as a CID in font 0; otherwise a code no mapping
    /// holds is [`Selector::Notdef`] in font 0 (PLRM 5.11.5, "Handling
    /// Undefined Characters").
    pub fn target(&self, code: &[u8]) -> Target {
        self.mapped(code).unwrap_or_else(|| Target {
            font: 0,
            selector: if self.chars.is_empty() && self.ranges.is_empty() {
                Selector::Cid(value(code) as i32)
            } else {
                Selector::Notdef
            },
        })
    }

    /// The CID of `code`: its [`target`](Self::target) when that is a CID,
    /// otherwise 0.
    pub fn cid(&self, code: &[u8]) -> i32 {
        match self.target(code).selector {
            Selector::Cid(cid) => cid,
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tables with `codespace` and the CID mappings `chars` (code, CID), as
    /// one CMap writes them: the first mapping of a code wins.
    fn cmap(codespace: &[(&[u8], &[u8])], chars: &[(&[u8], i32)]) -> CmapTables {
        let mut t = CmapTables {
            codespace: codespace
                .iter()
                .map(|(lo, hi)| (lo.to_vec(), hi.to_vec()))
                .collect(),
            ..Default::default()
        };
        t.shortest = codespace.iter().map(|(lo, _)| lo.len()).min().unwrap_or(1);
        for (code, c) in chars {
            t.chars
                .entry((code.len(), value(code)))
                .or_insert((0, cid(0, *c)));
        }
        t
    }

    /// Each code `decode` reads from `bytes`, as (length, CID).
    fn codes(t: &CmapTables, mut bytes: &[u8]) -> Vec<(usize, i32)> {
        let mut out = Vec::new();
        while let Some((len, target)) = t.decode(bytes) {
            let cid = match target.selector {
                Selector::Cid(c) => c,
                _ => 0,
            };
            out.push((len, cid));
            bytes = &bytes[len..];
        }
        out
    }

    const TWO_BYTES: &[(&[u8], &[u8])] = &[(&[0x00, 0x00], &[0xff, 0xff])];

    #[test]
    fn a_code_no_mapping_of_the_codespace_length_holds_takes_a_mapped_length() {
        // A two-byte codespace over one-byte mappings (#48's shape).
        let t = cmap(TWO_BYTES, &[(b"H", 72), (b"e", 101)]);
        assert_eq!(codes(&t, b"He"), [(1, 72), (1, 101)]);
        assert_eq!(
            codes(&t, b"Hee"),
            [(1, 72), (1, 101), (1, 101)],
            "a last lone byte"
        );
        assert_eq!(
            codes(&t, b"Hez"),
            [(1, 72), (1, 101)],
            "an unmapped lone byte"
        );
        assert_eq!(codes(&t, b"zH"), [(2, 0)], "one unmapped two-byte code");
        // A one-byte codespace over a two-byte mapping.
        let t = cmap(&[(&[0x00], &[0xff])], &[(b"He", 500)]);
        assert_eq!(codes(&t, b"Hez"), [(2, 500), (1, 0)]);
    }

    #[test]
    fn a_mapping_of_the_codespace_length_wins() {
        let t = cmap(TWO_BYTES, &[(b"H", 72), (b"He", 500)]);
        assert_eq!(codes(&t, b"He"), [(2, 500)]);
        let t = cmap(TWO_BYTES, &[(b"He", 500), (b"H", 72)]);
        assert_eq!(codes(&t, b"He"), [(2, 500)]);
        // A notdef mapping is a mapping, of either length.
        let mut t = cmap(TWO_BYTES, &[]);
        t.notdef_chars.insert((1, 0x48), (0, cid(0, 3)));
        t.notdef_chars.insert((2, 0x4142), (0, cid(0, 9)));
        assert_eq!(codes(&t, b"HAB"), [(1, 3), (2, 9)]);
    }

    #[test]
    fn a_valid_cmap_reads_as_its_codespace_says() {
        // Shift-JIS style: one-byte and two-byte codes, prefix-free.
        let t = cmap(
            &[(&[0x00], &[0x80]), (&[0x81, 0x40], &[0x9f, 0xfc])],
            &[(b"A", 34), (&[0x81, 0x41], 634)],
        );
        assert_eq!(codes(&t, b"A\x81\x41"), [(1, 34), (2, 634)]);
        assert_eq!(codes(&t, b"B\x81\x42"), [(1, 0), (2, 0)], "unmapped");
        assert_eq!(codes(&t, b"\x81"), [(1, 0)], "a lead byte alone");
        assert_eq!(
            codes(&t, b"\xa0\xa0"),
            [(1, 0), (1, 0)],
            "outside the codespace"
        );
        // With no mappings at all, a code is its own CID.
        let t = cmap(TWO_BYTES, &[]);
        assert_eq!(codes(&t, b"\x01\x02"), [(2, 0x0102)]);
    }

    fn cid(font: usize, cid: i32) -> Target {
        Target {
            font,
            selector: Selector::Cid(cid),
        }
    }

    #[test]
    fn the_first_range_that_holds_a_code_wins() {
        let mut t = RangeTable::default();
        t.push(&[0x00, 0x10], &[0x00, 0x20], RangeStart::Cid(100), 0, 0, 0);
        t.push(&[0x00, 0x00], &[0x00, 0xff], RangeStart::Cid(500), 0, 0, 1);
        t.push(&[0x01, 0x00], &[0x01, 0x05], RangeStart::Cid(900), 2, 1, 2);
        t.finish();
        assert_eq!(t.find(2, 0x0015), Some((0, cid(0, 105))));
        assert_eq!(t.find(2, 0x0030), Some((0, cid(0, 548))));
        assert_eq!(t.find(2, 0x0103), Some((1, cid(2, 903))));
        assert_eq!(t.find(2, 0x0106), None);
        assert_eq!(t.find(1, 0x15), None, "a one-byte code");
    }

    #[test]
    fn a_bfrange_counts_up_from_a_code_or_walks_its_array() {
        let mut t = RangeTable::default();
        t.push(&[0xc1], &[0xc3], RangeStart::Code(0x63), 1, 0, 0);
        t.lists = vec![Selector::Name(NameId(7)), Selector::Code(0x41)];
        t.push(
            &[0xa1],
            &[0xa3],
            RangeStart::List { start: 0, len: 2 },
            1,
            0,
            1,
        );
        t.finish();
        let at = |c| t.find(1, c).map(|(_, target)| target.selector);
        assert_eq!(at(0xc2), Some(Selector::Code(0x64)));
        assert_eq!(at(0xa1), Some(Selector::Name(NameId(7))));
        assert_eq!(at(0xa2), Some(Selector::Code(0x41)));
        assert_eq!(at(0xa3), Some(Selector::Notdef), "past the array's end");
    }

    #[test]
    fn the_nearest_cmap_wins_whatever_the_kind() {
        let mut t = CmapTables::default();
        // The parent (level 1) maps <C2B0> with a cidchar and the CMap itself
        // (level 0) with a cidrange, as UniJISPro-UTF8-V does over -H.
        t.chars.insert((2, 0xc2b0), (1, cid(0, 707)));
        t.ranges
            .push(&[0xc2, 0xb0], &[0xc2, 0xb1], RangeStart::Cid(8269), 0, 0, 0);
        // Within one CMap a cidchar comes before a cidrange.
        t.chars.insert((2, 0xc2b1), (0, cid(0, 42)));
        // A parent's cidrange still serves a code the CMap leaves out.
        t.ranges
            .push(&[0xc2, 0xb2], &[0xc2, 0xb2], RangeStart::Cid(900), 0, 1, 1);
        t.ranges.finish();
        assert_eq!(t.cid(&[0xc2, 0xb0]), 8269);
        assert_eq!(t.cid(&[0xc2, 0xb1]), 42);
        assert_eq!(t.cid(&[0xc2, 0xb2]), 900);
        assert_eq!(t.cid(&[0xc2, 0xb3]), 0);
    }

    #[test]
    fn a_code_nothing_maps_is_notdef_in_font_0() {
        let mut t = CmapTables::default();
        assert_eq!(
            t.target(&[0x48]),
            cid(0, 0x48),
            "no mappings: the code itself"
        );
        t.chars.insert(
            (1, 0x41),
            (
                0,
                Target {
                    font: 1,
                    selector: Selector::Name(NameId(3)),
                },
            ),
        );
        let unmapped = Target {
            font: 0,
            selector: Selector::Notdef,
        };
        assert_eq!(t.target(&[0x48]), unmapped);
        assert_eq!(t.cid(&[0x48]), 0);
        assert_eq!(t.cid(&[0x41]), 0, "a glyph name is no CID");
        t.notdef_chars.insert((1, 0x48), (0, cid(2, 5)));
        assert_eq!(t.target(&[0x48]), cid(2, 5));
    }
}
