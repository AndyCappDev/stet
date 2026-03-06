// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Dual-arena string storage: routes to global or local `StringStore`
//! based on the tag bit in `EntityId`.

use crate::entity_table::EntityMeta;
use crate::object::EntityId;
use crate::string_store::StringStore;

/// Dual-arena string store with separate global and local backing stores.
pub struct DualStringStore {
    pub global: StringStore,
    pub local: StringStore,
}

impl DualStringStore {
    pub fn new() -> Self {
        Self {
            global: StringStore::new(),
            local: StringStore::new(),
        }
    }

    /// Select the store for a given entity.
    #[inline]
    fn store(&self, entity: EntityId) -> &StringStore {
        if entity.is_global() {
            &self.global
        } else {
            &self.local
        }
    }

    /// Select the mutable store for a given entity.
    #[inline]
    fn store_mut(&mut self, entity: EntityId) -> &mut StringStore {
        if entity.is_global() {
            &mut self.global
        } else {
            &mut self.local
        }
    }

    // --- Allocation (defaults to local) ---

    /// Allocate `len` zero-filled bytes in local VM.
    pub fn allocate(&mut self, len: usize) -> EntityId {
        self.local.allocate(len)
    }

    /// Allocate and copy `bytes` into local VM.
    pub fn allocate_from(&mut self, bytes: &[u8]) -> EntityId {
        self.local.allocate_from(bytes)
    }

    /// Allocate and copy `bytes` with a specific save level and global flag.
    pub fn allocate_from_with(
        &mut self,
        bytes: &[u8],
        save_level: u16,
        global: bool,
        created_after_save: u32,
    ) -> EntityId {
        if global {
            self.global
                .allocate_from_with(bytes, save_level, global, created_after_save)
        } else {
            self.local
                .allocate_from_with(bytes, save_level, global, created_after_save)
        }
    }

    /// Allocate with a specific save level and global flag.
    pub fn allocate_with(
        &mut self,
        len: usize,
        save_level: u16,
        global: bool,
        created_after_save: u32,
    ) -> EntityId {
        if global {
            self.global
                .allocate_with(len, save_level, global, created_after_save)
        } else {
            self.local
                .allocate_with(len, save_level, global, created_after_save)
        }
    }

    // --- Access ---

    /// Get a slice of the string data.
    pub fn get(&self, entity: EntityId, start: u32, len: u32) -> &[u8] {
        self.store(entity).get(entity, start, len)
    }

    /// Get a mutable slice of the string data.
    pub fn get_mut(&mut self, entity: EntityId, start: u32, len: u32) -> &mut [u8] {
        self.store_mut(entity).get_mut(entity, start, len)
    }

    /// Set a single byte.
    pub fn put_byte(&mut self, entity: EntityId, offset: u32, byte: u8) {
        self.store_mut(entity).put_byte(entity, offset, byte);
    }

    /// Get a single byte.
    pub fn get_byte(&self, entity: EntityId, offset: u32) -> u8 {
        self.store(entity).get_byte(entity, offset)
    }

    // --- COW ---

    /// COW copy (always local — global entities skip COW).
    pub fn cow_copy(&mut self, entity: EntityId) -> EntityId {
        debug_assert!(!entity.is_global(), "COW copy on global entity");
        self.local.cow_copy(entity)
    }

    /// Swap offsets between two entities (used by restore, always local).
    pub fn swap_offsets(&mut self, a: EntityId, b: EntityId) {
        debug_assert!(
            !a.is_global() && !b.is_global(),
            "swap_offsets on global entity"
        );
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

    /// Access local backing data (for vmstatus approximation).
    pub fn data(&self) -> &[u8] {
        self.local.data()
    }

    /// Total entity count across both stores.
    pub fn entity_count(&self) -> usize {
        self.local.entities.len() + self.global.entities.len()
    }

    /// Reset local VM (for job boundary cleanup).
    pub fn reset_local(&mut self) {
        self.local = StringStore::new();
    }
}

impl Default for DualStringStore {
    fn default() -> Self {
        Self::new()
    }
}
