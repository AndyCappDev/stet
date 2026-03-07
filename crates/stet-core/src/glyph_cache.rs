// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Glyph path cache: caches charstring interpretation results per font.
//!
//! Stores glyph outlines in charstring coordinates (pre-FontMatrix, pre-CTM).
//! One cache entry per glyph per font, regardless of size/orientation.
//! FontMatrix and CTM transforms are still applied per-render.

use crate::display_list::DisplayElement;
use crate::graphics_state::PathSegment;
use crate::object::{EntityId, NameId};
use rustc_hash::FxHashMap;
use std::sync::Arc;

/// Cached glyph outline + width in charstring coordinates.
#[derive(Clone)]
pub struct CachedGlyph {
    pub segments: Arc<Vec<PathSegment>>,
    pub width_x: f64,
    pub width_y: f64,
}

/// Cached Type 3 glyph: display list elements + origin for translation.
#[derive(Clone)]
pub struct CachedType3Glyph {
    pub elements: Vec<DisplayElement>,
    pub origin_dev_x: f64,
    pub origin_dev_y: f64,
    pub width: (f64, f64),
}

/// Cache mode set by setcachedevice/setcharwidth during Type 3 BuildChar.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Type3CacheMode {
    /// setcachedevice was called — glyph is cacheable.
    Cache,
    /// setcharwidth was called — glyph is not cacheable (may use color ops).
    NoCache,
}

/// Per-font glyph path cache.
#[derive(Default)]
pub struct GlyphCache {
    /// Type 1 and CFF glyphs keyed by glyph name.
    pub by_name: FxHashMap<NameId, CachedGlyph>,
    /// CIDFont glyphs keyed by CID.
    pub by_cid: FxHashMap<i32, CachedGlyph>,
    /// TrueType glyphs keyed by glyph ID.
    pub by_gid: FxHashMap<u16, CachedGlyph>,
    /// Type 3 glyphs keyed by char code (only setcachedevice glyphs).
    pub by_charcode: FxHashMap<u8, CachedType3Glyph>,
}

impl GlyphCache {
    pub fn new() -> Self {
        Self {
            by_name: FxHashMap::default(),
            by_cid: FxHashMap::default(),
            by_gid: FxHashMap::default(),
            by_charcode: FxHashMap::default(),
        }
    }
}

/// Get or create the glyph cache for a font entity.
#[inline]
pub fn get_or_create_cache(
    caches: &mut FxHashMap<EntityId, GlyphCache>,
    font_entity: EntityId,
) -> &mut GlyphCache {
    caches.entry(font_entity).or_insert_with(GlyphCache::new)
}
