// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Copies of the fonts text was shown with, for output devices that read
//! them after the page is sent.
//!
//! PDF output embeds fonts at the end of the job, but by then `restore` may
//! have reclaimed the local VM a font lived in, or reverted glyphs a page
//! added to a font defined before its `save` (incremental definition, PLRM
//! 3e §5.9.2). Read through the [`EntityId`] a `Text` element recorded, such
//! a font panics, or reads whatever later object reused the id, or silently
//! lacks the glyphs the page showed.
//!
//! So while the output device asks for them
//! ([`OutputDevice::keeps_text_fonts`](crate::device::OutputDevice::keeps_text_fonts)),
//! each show records its font in an *instance*
//! ([`FontSnapshots::note_shown`]), and every instance is copied out of the
//! VM — *frozen* — once, at the end of its life: by the next `restore`,
//! before that restore changes anything ([`FontSnapshots::freeze_live`]), or
//! when the device reads it at the end of the job
//! ([`FontSnapshots::resolve`]). Between two restores a font's contents only
//! grow — PLRM §5.9.2 lets a program add glyphs and fill `.notdef` encoding
//! slots, never replace them, and only a `restore` takes them away — so that
//! one copy holds everything any show of the instance needed.
//!
//! A copy keeps what PDF output reads from a font: the entries of each font
//! dictionary, descending only into the values listed in [`FONT_KEYS`]
//! (only `Encoding`, `FontBBox` and `FontMatrix` for a Type 3 font, whose
//! glyph procedures can reach anything); any other composite value of a
//! font dictionary is [`Frozen::NotKept`], and a dictionary inside a copied
//! value is [`Frozen::Other`]. Copies share structure: freezing a font again
//! compares it against its previous copy and reuses every part that has not
//! changed, so a font shown on every page of a document that brackets each
//! page with `save`/`restore` is stored once.

use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::context::{CidGlyphMetrics, Context};
use crate::dict::DictKey;
use crate::object::{EntityId, NameId, PsObject, PsValue};

/// The keys of a font dictionary whose values PDF output reads as data, and
/// so are copied in full: every key `stet-pdf` reads a composite from. The
/// two arrays of fonts (`FDepVector`, `FDArray`) have each element copied as
/// a font dictionary in its own right. Any other composite value of a font
/// dictionary is [`Frozen::NotKept`]; scalars are always copied.
pub const FONT_KEYS: &[&[u8]] = &[
    b"FontName",
    b"CIDFontName",
    b"Encoding",
    b"FontBBox",
    b"FontMatrix",
    b"CharStrings",
    b"Private",
    b"_cff_global_subrs",
    b"_CFFData",
    b"sfnts",
    b"GlyphDirectory",
    b"GlyphData",
    b"CIDMap",
    b"CIDSystemInfo",
    b"FDArray",
    b"FDepVector",
];

/// The keys copied from a Type 3 font: its encoding and geometry. Its
/// `CharProcs`, `BuildGlyph` and whatever else it carries are procedures
/// and data for them, which PDF output does not embed.
pub const TYPE3_FONT_KEYS: &[&[u8]] = &[b"Encoding", b"FontBBox", b"FontMatrix"];

/// A value copied out of the VM.
///
/// Composites become shared, immutable copies: two copies of an unchanged
/// object are the same [`Arc`].
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Frozen {
    /// `null`.
    Null,
    /// A boolean.
    Bool(bool),
    /// An integer.
    Int(i64),
    /// A real.
    Real(f64),
    /// A name. Names are never reclaimed, so the id stays valid.
    Name(NameId),
    /// A string's bytes.
    String(Arc<[u8]>),
    /// An array, literal or executable (a font's `FontBBox` is often written
    /// as a procedure).
    Array(Arc<[Frozen]>),
    /// A packed array.
    PackedArray(Arc<[Frozen]>),
    /// A dictionary.
    Dict(Arc<FrozenDict>),
    /// A value PDF output never reads as data: an operator, a file, a font
    /// ID, a mark, a `save` or `gstate` object; a dictionary inside a copied
    /// value; or a reference back into an object being copied, where the
    /// copy cuts a cycle.
    Other,
    /// A composite value of a font dictionary under a key outside
    /// [`FONT_KEYS`]: the key is present, its value was not copied. Reading
    /// one as data means `FONT_KEYS` is missing the key, which the accessors
    /// assert in debug builds.
    NotKept,
}

impl Frozen {
    /// The dictionary, if this is one.
    pub fn as_dict(&self) -> Option<&Arc<FrozenDict>> {
        self.assert_kept();
        match self {
            Frozen::Dict(d) => Some(d),
            _ => None,
        }
    }

    /// The elements, if this is an (unpacked) array.
    pub fn as_array(&self) -> Option<&[Frozen]> {
        self.assert_kept();
        match self {
            Frozen::Array(a) => Some(a),
            _ => None,
        }
    }

    /// The bytes, if this is a string.
    pub fn as_string(&self) -> Option<&[u8]> {
        self.assert_kept();
        match self {
            Frozen::String(s) => Some(s),
            _ => None,
        }
    }

    /// The name, if this is one.
    pub fn as_name(&self) -> Option<NameId> {
        match self {
            Frozen::Name(n) => Some(*n),
            _ => None,
        }
    }

    /// The number, integer or real, as [`PsObject::as_f64`].
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Frozen::Int(v) => Some(*v as f64),
            Frozen::Real(v) => Some(*v),
            _ => None,
        }
    }

    /// The integer, as [`PsObject::as_i64`].
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Frozen::Int(v) => Some(*v),
            _ => None,
        }
    }

    /// The integer, if it fits an `i32`, as [`PsObject::as_i32`].
    pub fn as_i32(&self) -> Option<i32> {
        match self {
            Frozen::Int(v) => i32::try_from(*v).ok(),
            _ => None,
        }
    }

    fn assert_kept(&self) {
        debug_assert!(
            !matches!(self, Frozen::NotKept),
            "read a font value the snapshot did not keep: add its key to FONT_KEYS"
        );
    }
}

/// A dictionary copied out of the VM. Keys that name composite objects by
/// identity are dropped: nothing reads them, and the identity would not
/// survive the copy.
#[derive(Debug, Default)]
pub struct FrozenDict {
    entries: FxHashMap<DictKey, Frozen>,
}

impl FrozenDict {
    /// The value under `key`.
    pub fn get(&self, key: &DictKey) -> Option<&Frozen> {
        self.entries.get(key)
    }

    /// The value under the name `name`.
    pub fn get_name(&self, name: NameId) -> Option<&Frozen> {
        self.entries.get(&DictKey::Name(name))
    }

    /// The number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the dictionary is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The entries, in no particular order.
    pub fn iter(&self) -> impl Iterator<Item = (&DictKey, &Frozen)> {
        self.entries.iter()
    }
}

/// A font as it stood at the end of an instance's life.
#[derive(Clone, Debug)]
pub struct FrozenFont {
    /// The font dictionary's id when the text was shown. Only an identity:
    /// it may since have been reclaimed or reused.
    pub entity: EntityId,
    /// The copy of the font dictionary.
    pub root: Arc<FrozenDict>,
}

impl FrozenFont {
    /// Whether `self` and `other` are the same font: shown with the same
    /// dictionary, unchanged between them. A font kept across pages is one
    /// font; a font reusing a reclaimed one's id, or one a page changed, is
    /// another. The copies say so by themselves: a copy is reused only for
    /// the dictionary it was made from, and only while that is unchanged.
    pub fn same_font(&self, other: &FrozenFont) -> bool {
        Arc::ptr_eq(&self.root, &other.root)
    }
}

/// The fonts text was shown with during a job, as the interpreter keeps them
/// for an output device that reads them after the page is sent. See the
/// [module documentation](self).
#[derive(Default)]
pub struct FontSnapshots {
    /// Every instance, indexed by the id `Text` elements carry.
    instances: Vec<Instance>,
    /// The instances shown since the last `restore`, by font dictionary.
    live: FxHashMap<EntityId, u32>,
    /// Each font dictionary's latest copy, which the next copy of it shares
    /// unchanged parts with.
    last: FxHashMap<EntityId, Arc<FrozenDict>>,
    /// CIDFont glyph metrics from `Metrics2` / `CDevProc`
    /// ([`Context::cid_glyph_metrics`]), by copied CIDFont and CID.
    cid_metrics: FxHashMap<(ByPtr, u32), CidGlyphMetrics>,
}

struct Instance {
    entity: EntityId,
    root: Option<Arc<FrozenDict>>,
}

impl FontSnapshots {
    /// The instance for text being shown with the font dictionary `font`,
    /// opening one if the font has not been shown since the last `restore`.
    pub fn note_shown(&mut self, font: EntityId) -> u32 {
        if let Some(&id) = self.live.get(&font) {
            return id;
        }
        let id = u32::try_from(self.instances.len()).expect("fewer than 2^32 font instances");
        self.instances.push(Instance {
            entity: font,
            root: None,
        });
        self.live.insert(font, id);
        id
    }

    /// Freeze every instance shown since the last `restore`, and close them:
    /// text shown afterwards opens new ones. `restore` calls this before it
    /// changes the VM.
    pub fn freeze_live(&mut self, ctx: &Context) {
        if self.live.is_empty() {
            return;
        }
        let mut freezer = Freezer::new(ctx);
        for (entity, id) in self.live.drain() {
            let root = freezer.font(entity, self.last.get(&entity));
            self.last.insert(entity, root.clone());
            self.instances[id as usize].root = Some(root);
        }
        freezer.copy_metrics(&mut self.cid_metrics);
    }

    /// The copy of `instance`, once it is frozen.
    pub fn frozen(&self, instance: u32) -> Option<&Arc<FrozenDict>> {
        self.instances.get(instance as usize)?.root.as_ref()
    }

    /// Every instance as a font: frozen ones as they were copied, live ones
    /// copied now from the VM. For the device, at the end of the job,
    /// before the job's own `restore`.
    pub fn resolve(&self, ctx: &Context) -> ResolvedFonts {
        let mut freezer = Freezer::new(ctx);
        let fonts = self
            .instances
            .iter()
            .map(|instance| FrozenFont {
                entity: instance.entity,
                root: match &instance.root {
                    Some(root) => root.clone(),
                    None => freezer.font(instance.entity, self.last.get(&instance.entity)),
                },
            })
            .collect();
        let mut cid_metrics = self.cid_metrics.clone();
        freezer.copy_metrics(&mut cid_metrics);
        ResolvedFonts { fonts, cid_metrics }
    }

    /// The number of instances.
    pub fn len(&self) -> usize {
        self.instances.len()
    }

    /// Whether no text has been shown with a font kept.
    pub fn is_empty(&self) -> bool {
        self.instances.is_empty()
    }

    /// Forget everything: the job is over, and its device has read what it
    /// needed. Done before the job's own `restore`, which would otherwise
    /// freeze every live instance for nothing.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// Every font instance of a job, frozen: what an output device reads at the
/// end of the job.
pub struct ResolvedFonts {
    fonts: Vec<FrozenFont>,
    cid_metrics: FxHashMap<(ByPtr, u32), CidGlyphMetrics>,
}

impl ResolvedFonts {
    /// The font of `instance` — a [`TextParams::font_snapshot`](crate::device::TextParams::font_snapshot).
    pub fn font(&self, instance: u32) -> Option<&FrozenFont> {
        self.fonts.get(instance as usize)
    }

    /// The metrics a show gave `cid` of `cidfont` — a CIDFont reached from
    /// one of these fonts — through its `Metrics2` or `CDevProc`.
    pub fn cid_metrics(&self, cidfont: &Arc<FrozenDict>, cid: u32) -> Option<CidGlyphMetrics> {
        self.cid_metrics
            .get(&(ByPtr(cidfont.clone()), cid))
            .copied()
    }
}

/// A copied dictionary compared and hashed by identity. Holding the `Arc`
/// keeps the address from being reused while the key exists.
#[derive(Clone)]
struct ByPtr(Arc<FrozenDict>);

impl PartialEq for ByPtr {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for ByPtr {}

impl std::hash::Hash for ByPtr {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::ptr::hash(Arc::as_ptr(&self.0), state);
    }
}

/// What a value is, for deciding how far to copy it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Role {
    /// A font dictionary: its entries, descending into [`FONT_KEYS`].
    Font,
    /// A value under one of those keys: copied in full, except that a
    /// dictionary inside it is [`Frozen::Other`].
    Data,
    /// Inside a [`Role::Data`] value: dictionaries are [`Frozen::Other`].
    Inner,
    /// An array of fonts (`FDepVector`, `FDArray`): its dictionaries are
    /// fonts.
    Fonts,
}

/// One copy pass: a single `restore`, or the end of the job.
struct Freezer<'a> {
    ctx: &'a Context,
    /// The names of [`FONT_KEYS`], with the role of each one's value.
    font_keys: FxHashMap<NameId, Role>,
    /// The names of [`TYPE3_FONT_KEYS`].
    type3_keys: FxHashSet<NameId>,
    font_type: Option<NameId>,
    /// Objects copied this pass: fonts that share a `CharStrings` or
    /// `Private` (every `scalefont` of a font does) copy it once.
    memo: FxHashMap<Memo, Frozen>,
    /// Objects being copied, to stop at a cycle: by kind, id and range,
    /// whatever role they were reached in.
    active: FxHashSet<(bool, EntityId, u32, u32)>,
    /// The font dictionaries copied this pass, by id, for their metrics.
    fonts: Vec<(EntityId, Arc<FrozenDict>)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Memo {
    String(EntityId, u32, u32),
    Array(EntityId, u32, u32, bool, Role),
    Dict(EntityId, Role),
}

impl<'a> Freezer<'a> {
    fn new(ctx: &'a Context) -> Self {
        let font_keys = FONT_KEYS
            .iter()
            .filter_map(|key| {
                let role = match *key {
                    b"FDArray" | b"FDepVector" => Role::Fonts,
                    _ => Role::Data,
                };
                Some((ctx.names.find(key)?, role))
            })
            .collect();
        let type3_keys = TYPE3_FONT_KEYS
            .iter()
            .filter_map(|key| ctx.names.find(key))
            .collect();
        Self {
            ctx,
            font_keys,
            type3_keys,
            font_type: ctx.names.find(b"FontType"),
            memo: FxHashMap::default(),
            active: FxHashSet::default(),
            fonts: Vec::new(),
        }
    }

    /// Copy the font dictionary `font`, sharing what is unchanged with its
    /// previous copy `hint`.
    fn font(&mut self, font: EntityId, hint: Option<&Arc<FrozenDict>>) -> Arc<FrozenDict> {
        let hint = hint.map(|h| Frozen::Dict(h.clone()));
        match self.value(PsObject::dict(font), Role::Font, hint.as_ref()) {
            Frozen::Dict(d) => d,
            // Only a cycle back to the root could give anything else, and
            // the root is not yet being copied when this starts.
            _ => Arc::default(),
        }
    }

    fn value(&mut self, obj: PsObject, role: Role, hint: Option<&Frozen>) -> Frozen {
        match obj.value {
            PsValue::Null => Frozen::Null,
            PsValue::Bool(b) => Frozen::Bool(b),
            PsValue::Int(v) => Frozen::Int(v),
            PsValue::Real(v) => Frozen::Real(v),
            PsValue::Name(n) => Frozen::Name(n),
            PsValue::String { entity, start, len } => self.string(entity, start, len, hint),
            PsValue::Array { entity, start, len } => {
                self.array(entity, start, len, false, role, hint)
            }
            PsValue::PackedArray { entity, start, len } => {
                self.array(entity, start, len, true, role, hint)
            }
            PsValue::Dict(entity) => match role {
                Role::Font | Role::Data => self.dict(entity, role, hint),
                Role::Inner | Role::Fonts => Frozen::Other,
            },
            _ => Frozen::Other,
        }
    }

    fn string(&mut self, entity: EntityId, start: u32, len: u32, hint: Option<&Frozen>) -> Frozen {
        let key = Memo::String(entity, start, len);
        if let Some(done) = self.memo.get(&key) {
            return done.clone();
        }
        let bytes = self.ctx.strings.get(entity, start, len);
        let frozen = match hint {
            Some(Frozen::String(h)) if **h == *bytes => Frozen::String(h.clone()),
            _ => Frozen::String(Arc::from(bytes)),
        };
        self.memo.insert(key, frozen.clone());
        frozen
    }

    fn array(
        &mut self,
        entity: EntityId,
        start: u32,
        len: u32,
        packed: bool,
        role: Role,
        hint: Option<&Frozen>,
    ) -> Frozen {
        let key = Memo::Array(entity, start, len, packed, role);
        if let Some(done) = self.memo.get(&key) {
            return done.clone();
        }
        let visiting = (false, entity, start, len);
        if !self.active.insert(visiting) {
            return Frozen::Other;
        }
        let hint = match hint {
            Some(Frozen::Array(h)) if !packed => Some(h),
            Some(Frozen::PackedArray(h)) if packed => Some(h),
            _ => None,
        };
        let element_role = match role {
            Role::Fonts => Role::Font,
            _ => Role::Inner,
        };
        let ctx = self.ctx;
        let elements: Vec<Frozen> = ctx
            .arrays
            .get(entity, start, len)
            .iter()
            .enumerate()
            .map(|(i, &element)| {
                // A non-font element of an array of fonts is copied as data.
                let role = match (element_role, element.value) {
                    (Role::Font, PsValue::Dict(_)) => Role::Font,
                    _ => Role::Inner,
                };
                self.value(element, role, hint.and_then(|h| h.get(i)))
            })
            .collect();
        let frozen = match hint {
            Some(h)
                if h.len() == elements.len()
                    && h.iter().zip(&elements).all(|(a, b)| same(a, b)) =>
            {
                if packed {
                    Frozen::PackedArray(h.clone())
                } else {
                    Frozen::Array(h.clone())
                }
            }
            _ if packed => Frozen::PackedArray(elements.into()),
            _ => Frozen::Array(elements.into()),
        };
        self.active.remove(&visiting);
        self.memo.insert(key, frozen.clone());
        frozen
    }

    fn dict(&mut self, entity: EntityId, role: Role, hint: Option<&Frozen>) -> Frozen {
        let key = Memo::Dict(entity, role);
        if let Some(done) = self.memo.get(&key) {
            return done.clone();
        }
        let visiting = (true, entity, 0, 0);
        if !self.active.insert(visiting) {
            return Frozen::Other;
        }
        let hint = match hint {
            Some(Frozen::Dict(h)) => Some(h),
            _ => None,
        };
        let ctx = self.ctx;
        let source = &ctx.dicts.entry(entity).entries;
        let type3 = role == Role::Font
            && self
                .font_type
                .and_then(|n| source.get(&DictKey::Name(n)))
                .and_then(PsObject::as_i32)
                == Some(3);
        let mut entries = FxHashMap::default();
        entries.reserve(source.len());
        for (k, v) in source {
            if matches!(k, DictKey::Identity(..)) {
                continue;
            }
            let value_role = match role {
                Role::Font => match k {
                    DictKey::Name(n) if type3 => self.type3_keys.contains(n).then_some(Role::Data),
                    DictKey::Name(n) => self.font_keys.get(n).copied(),
                    _ => None,
                },
                _ => Some(Role::Inner),
            };
            let frozen = match value_role {
                Some(r) => self.value(*v, r, hint.and_then(|h| h.get(k))),
                // Not read as data: scalars are copied, composites not kept.
                None => match v.value {
                    PsValue::String { .. }
                    | PsValue::Array { .. }
                    | PsValue::PackedArray { .. }
                    | PsValue::Dict(_) => Frozen::NotKept,
                    _ => self.value(*v, Role::Inner, None),
                },
            };
            entries.insert(k.clone(), frozen);
        }
        let copy = match hint {
            Some(h)
                if h.len() == entries.len()
                    && entries
                        .iter()
                        .all(|(k, v)| h.get(k).is_some_and(|old| same(old, v))) =>
            {
                h.clone()
            }
            _ => Arc::new(FrozenDict { entries }),
        };
        if role == Role::Font {
            self.fonts.push((entity, copy.clone()));
        }
        self.active.remove(&visiting);
        let frozen = Frozen::Dict(copy);
        self.memo.insert(key, frozen.clone());
        frozen
    }

    /// Copy the CIDFont metrics the interpreter recorded for the font
    /// dictionaries copied this pass.
    fn copy_metrics(&self, into: &mut FxHashMap<(ByPtr, u32), CidGlyphMetrics>) {
        if self.ctx.cid_glyph_metrics.is_empty() || self.fonts.is_empty() {
            return;
        }
        let copies: FxHashMap<EntityId, &Arc<FrozenDict>> =
            self.fonts.iter().map(|(e, d)| (*e, d)).collect();
        for (&(entity, cid), &metrics) in &self.ctx.cid_glyph_metrics {
            if let Some(copy) = copies.get(&entity) {
                into.insert((ByPtr((*copy).clone()), cid), metrics);
            }
        }
    }
}

/// Whether two copies are the same: equal scalars, or the same shared copy.
fn same(a: &Frozen, b: &Frozen) -> bool {
    match (a, b) {
        (Frozen::Null, Frozen::Null)
        | (Frozen::Other, Frozen::Other)
        | (Frozen::NotKept, Frozen::NotKept) => true,
        (Frozen::Bool(x), Frozen::Bool(y)) => x == y,
        (Frozen::Int(x), Frozen::Int(y)) => x == y,
        (Frozen::Real(x), Frozen::Real(y)) => x.to_bits() == y.to_bits(),
        (Frozen::Name(x), Frozen::Name(y)) => x == y,
        (Frozen::String(x), Frozen::String(y)) => Arc::ptr_eq(x, y),
        (Frozen::Array(x), Frozen::Array(y)) | (Frozen::PackedArray(x), Frozen::PackedArray(y)) => {
            Arc::ptr_eq(x, y)
        }
        (Frozen::Dict(x), Frozen::Dict(y)) => Arc::ptr_eq(x, y),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "did not keep")]
    fn reading_a_value_the_copy_did_not_keep_is_a_bug() {
        Frozen::NotKept.as_dict();
    }

    #[test]
    fn copies_are_the_same_only_when_shared() {
        let bytes: Arc<[u8]> = Arc::from(&b"abc"[..]);
        assert!(same(
            &Frozen::String(bytes.clone()),
            &Frozen::String(bytes.clone())
        ));
        assert!(!same(
            &Frozen::String(bytes),
            &Frozen::String(Arc::from(&b"abc"[..]))
        ));
        assert!(same(&Frozen::Real(0.5), &Frozen::Real(0.5)));
        assert!(!same(&Frozen::Int(1), &Frozen::Real(1.0)));
        assert!(!same(
            &Frozen::Array(Arc::from(Vec::new())),
            &Frozen::PackedArray(Arc::from(Vec::new()))
        ));
    }
}
