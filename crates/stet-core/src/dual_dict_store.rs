// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Dual-arena dictionary storage: routes to global or local `DictStore`
//! based on the tag bit in `EntityId`.

use crate::dict::{DictEntry, DictKey, DictStore};
use crate::entity_table::EntityMeta;
use crate::object::{EntityId, PsObject};

/// Dual-arena dict store with separate global and local backing stores.
pub struct DualDictStore {
    pub global: DictStore,
    pub local: DictStore,
}

impl DualDictStore {
    pub fn new() -> Self {
        Self {
            global: DictStore::new(),
            local: DictStore::new(),
        }
    }

    #[inline]
    fn store(&self, entity: EntityId) -> &DictStore {
        if entity.is_global() {
            &self.global
        } else {
            &self.local
        }
    }

    #[inline]
    fn store_mut(&mut self, entity: EntityId) -> &mut DictStore {
        if entity.is_global() {
            &mut self.global
        } else {
            &mut self.local
        }
    }

    // --- Allocation ---

    /// Allocate a new dictionary in local VM.
    pub fn allocate(&mut self, max_length: usize, name: &[u8]) -> EntityId {
        self.local.allocate(max_length, name)
    }

    /// Allocate with a specific save level and global flag.
    pub fn allocate_with(
        &mut self,
        max_length: usize,
        name: &[u8],
        save_level: u16,
        global: bool,
        created_after_save: u32,
    ) -> EntityId {
        if global {
            self.global
                .allocate_with(max_length, name, save_level, global, created_after_save)
        } else {
            self.local
                .allocate_with(max_length, name, save_level, global, created_after_save)
        }
    }

    // --- Access ---

    /// Look up a key in a dictionary.
    #[inline]
    pub fn get(&self, entity: EntityId, key: &DictKey) -> Option<PsObject> {
        self.store(entity).get(entity, key)
    }

    /// Insert or update a key-value pair.
    pub fn put(&mut self, entity: EntityId, key: DictKey, value: PsObject) {
        self.store_mut(entity).put(entity, key, value);
    }

    /// Check if a key exists.
    pub fn known(&self, entity: EntityId, key: &DictKey) -> bool {
        self.store(entity).known(entity, key)
    }

    /// Get the dict's name.
    pub fn get_name(&self, entity: EntityId) -> &[u8] {
        self.store(entity).get_name(entity)
    }

    /// Set the dict's name.
    pub fn set_name(&mut self, entity: EntityId, name: &[u8]) {
        self.store_mut(entity).set_name(entity, name);
    }

    /// Current number of entries.
    pub fn length(&self, entity: EntityId) -> usize {
        self.store(entity).length(entity)
    }

    /// Maximum capacity.
    pub fn max_length(&self, entity: EntityId) -> usize {
        self.store(entity).max_length(entity)
    }

    /// Remove a key.
    pub fn remove(&mut self, entity: EntityId, key: &DictKey) {
        self.store_mut(entity).remove(entity, key);
    }

    /// Borrow the dict entry.
    pub fn entry(&self, entity: EntityId) -> &DictEntry {
        self.store(entity).entry(entity)
    }

    /// Mutably borrow the dict entry.
    pub fn entry_mut(&mut self, entity: EntityId) -> &mut DictEntry {
        self.store_mut(entity).entry_mut(entity)
    }

    /// Iterate over keys of a dictionary.
    pub fn keys(&self, entity: EntityId) -> impl Iterator<Item = &DictKey> {
        self.store(entity).keys(entity)
    }

    /// Get the access level of a dictionary.
    pub fn access(&self, entity: EntityId) -> u8 {
        self.store(entity).access(entity)
    }

    /// Require read access on a dict.
    #[inline]
    pub fn require_read(&self, entity: EntityId) -> Result<(), crate::error::PsError> {
        self.store(entity).require_read(entity)
    }

    /// Require write access on a dict.
    #[inline]
    pub fn require_write(&self, entity: EntityId) -> Result<(), crate::error::PsError> {
        self.store(entity).require_write(entity)
    }

    /// Set the access level of a dictionary.
    pub fn set_access(&mut self, entity: EntityId, access: u8) {
        self.store_mut(entity).set_access(entity, access);
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

    /// Total entity count across both stores.
    pub fn entity_count(&self) -> usize {
        self.local.entities.len() + self.global.entities.len()
    }

    /// Reset local VM (for job boundary cleanup).
    pub fn reset_local(&mut self) {
        self.local = DictStore::new();
    }
}

impl Default for DualDictStore {
    fn default() -> Self {
        Self::new()
    }
}
