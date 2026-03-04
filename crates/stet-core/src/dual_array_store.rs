// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Dual-arena array storage: routes to global or local `ArrayStore`
//! based on the tag bit in `EntityId`.

use crate::array_store::ArrayStore;
use crate::entity_table::EntityMeta;
use crate::object::{EntityId, PsObject};

/// Dual-arena array store with separate global and local backing stores.
pub struct DualArrayStore {
    pub global: ArrayStore,
    pub local: ArrayStore,
}

impl DualArrayStore {
    pub fn new() -> Self {
        Self {
            global: ArrayStore::new(),
            local: ArrayStore::new(),
        }
    }

    #[inline]
    fn store(&self, entity: EntityId) -> &ArrayStore {
        if entity.is_global() {
            &self.global
        } else {
            &self.local
        }
    }

    #[inline]
    fn store_mut(&mut self, entity: EntityId) -> &mut ArrayStore {
        if entity.is_global() {
            &mut self.global
        } else {
            &mut self.local
        }
    }

    // --- Allocation ---

    /// Allocate `len` null-filled slots in local VM.
    pub fn allocate(&mut self, len: usize) -> EntityId {
        self.local.allocate(len)
    }

    /// Allocate and copy `items` into local VM.
    pub fn allocate_from(&mut self, items: &[PsObject]) -> EntityId {
        self.local.allocate_from(items)
    }

    /// Allocate and copy `items` with a specific save level and global flag.
    pub fn allocate_from_with(
        &mut self,
        items: &[PsObject],
        save_level: u16,
        global: bool,
    ) -> EntityId {
        if global {
            self.global.allocate_from_with(items, save_level, global)
        } else {
            self.local.allocate_from_with(items, save_level, global)
        }
    }

    /// Allocate with a specific save level and global flag.
    pub fn allocate_with(&mut self, len: usize, save_level: u16, global: bool) -> EntityId {
        if global {
            self.global.allocate_with(len, save_level, global)
        } else {
            self.local.allocate_with(len, save_level, global)
        }
    }

    // --- Access ---

    /// Get a slice of array elements.
    pub fn get(&self, entity: EntityId, start: u32, len: u32) -> &[PsObject] {
        self.store(entity).get(entity, start, len)
    }

    /// Get a mutable slice of array elements.
    pub fn get_mut(&mut self, entity: EntityId, start: u32, len: u32) -> &mut [PsObject] {
        self.store_mut(entity).get_mut(entity, start, len)
    }

    /// Get a single element.
    #[inline]
    pub fn get_element(&self, entity: EntityId, index: u32) -> PsObject {
        self.store(entity).get_element(entity, index)
    }

    /// Set a single element.
    pub fn set_element(&mut self, entity: EntityId, index: u32, obj: PsObject) {
        self.store_mut(entity).set_element(entity, index, obj);
    }

    // --- COW ---

    /// COW copy (always local — global entities skip COW).
    pub fn cow_copy(&mut self, entity: EntityId) -> EntityId {
        debug_assert!(!entity.is_global(), "COW copy on global entity");
        self.local.cow_copy(entity)
    }

    /// Swap offsets between two entities (used by restore, always local).
    pub fn swap_offsets(&mut self, a: EntityId, b: EntityId) {
        debug_assert!(!a.is_global() && !b.is_global(), "swap_offsets on global entity");
        self.local.swap_offsets(a, b);
    }

    // --- Metadata access ---

    /// Get entity metadata (read-only).
    pub fn entity_meta(&self, entity: EntityId) -> &EntityMeta {
        self.store(entity).entities.get(entity)
    }

    /// Get mutable entity metadata.
    pub fn entity_meta_mut(&mut self, entity: EntityId) -> &mut EntityMeta {
        self.store_mut(entity).entities.get_mut(entity)
    }

    // --- Stats ---

    /// Total entity count across both stores.
    pub fn entity_count(&self) -> usize {
        self.local.entities.len() + self.global.entities.len()
    }

    /// Reset local VM (for job boundary cleanup).
    pub fn reset_local(&mut self) {
        self.local = ArrayStore::new();
    }
}

impl Default for DualArrayStore {
    fn default() -> Self {
        Self::new()
    }
}
