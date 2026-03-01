// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Entity table: indirection layer for arena stores.
//!
//! Each composite object (string, array, dict) is identified by an `EntityId`.
//! The entity table maps `EntityId → EntityMeta`, which records the offset
//! into the backing store's data vec, the allocated length, the save level
//! at creation/last COW copy, and flags (global, gc_mark).

use crate::object::EntityId;

/// Metadata for one entity in an arena store.
#[derive(Clone, Debug)]
pub struct EntityMeta {
    /// Offset into the backing store's data vec.
    pub offset: u32,
    /// Allocated capacity (number of elements/bytes).
    pub len: u32,
    /// Save level when created or last COW-copied.
    pub save_level: u16,
    /// Bit 0: is_global, Bit 1: gc_mark (reserved for future use).
    pub flags: u8,
}

impl EntityMeta {
    const FLAG_GLOBAL: u8 = 1;

    /// Check if this entity is in global VM.
    pub fn is_global(&self) -> bool {
        self.flags & Self::FLAG_GLOBAL != 0
    }

    /// Set the global flag.
    pub fn set_global(&mut self, global: bool) {
        if global {
            self.flags |= Self::FLAG_GLOBAL;
        } else {
            self.flags &= !Self::FLAG_GLOBAL;
        }
    }
}

/// Indirection table mapping `EntityId` to metadata about stored data.
pub struct EntityTable {
    entries: Vec<EntityMeta>,
}

impl EntityTable {
    /// Create an empty entity table.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Allocate a new entity, returning its `EntityId`.
    pub fn allocate(&mut self, offset: u32, len: u32, save_level: u16, global: bool) -> EntityId {
        let id = EntityId(self.entries.len() as u32);
        let mut flags = 0u8;
        if global {
            flags |= EntityMeta::FLAG_GLOBAL;
        }
        self.entries.push(EntityMeta {
            offset,
            len,
            save_level,
            flags,
        });
        id
    }

    /// Get metadata for an entity (read-only).
    #[inline]
    pub fn get(&self, id: EntityId) -> &EntityMeta {
        &self.entries[id.0 as usize]
    }

    /// Get mutable metadata for an entity.
    pub fn get_mut(&mut self, id: EntityId) -> &mut EntityMeta {
        &mut self.entries[id.0 as usize]
    }

    /// Number of entities allocated.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for EntityTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allocate_and_get() {
        let mut table = EntityTable::new();
        let id = table.allocate(0, 10, 0, false);
        assert_eq!(id, EntityId(0));
        let meta = table.get(id);
        assert_eq!(meta.offset, 0);
        assert_eq!(meta.len, 10);
        assert_eq!(meta.save_level, 0);
        assert!(!meta.is_global());
    }

    #[test]
    fn test_multiple_allocations() {
        let mut table = EntityTable::new();
        let id0 = table.allocate(0, 5, 0, false);
        let id1 = table.allocate(5, 10, 0, true);
        assert_eq!(id0, EntityId(0));
        assert_eq!(id1, EntityId(1));
        assert_eq!(table.len(), 2);
        assert!(!table.get(id0).is_global());
        assert!(table.get(id1).is_global());
    }

    #[test]
    fn test_get_mut() {
        let mut table = EntityTable::new();
        let id = table.allocate(0, 5, 0, false);
        table.get_mut(id).offset = 100;
        assert_eq!(table.get(id).offset, 100);
    }

    #[test]
    fn test_global_flag() {
        let mut table = EntityTable::new();
        let id = table.allocate(0, 5, 0, false);
        assert!(!table.get(id).is_global());
        table.get_mut(id).set_global(true);
        assert!(table.get(id).is_global());
        table.get_mut(id).set_global(false);
        assert!(!table.get(id).is_global());
    }

    #[test]
    fn test_save_level_tracking() {
        let mut table = EntityTable::new();
        let id = table.allocate(0, 5, 1, false);
        assert_eq!(table.get(id).save_level, 1);
        table.get_mut(id).save_level = 2;
        assert_eq!(table.get(id).save_level, 2);
    }

    #[test]
    fn test_empty_table() {
        let table = EntityTable::new();
        assert_eq!(table.len(), 0);
        assert!(table.is_empty());
    }

    #[test]
    fn test_default() {
        let table = EntityTable::default();
        assert!(table.is_empty());
    }

    #[test]
    fn test_len_after_allocations() {
        let mut table = EntityTable::new();
        table.allocate(0, 1, 0, false);
        table.allocate(1, 2, 0, false);
        table.allocate(3, 3, 0, false);
        assert_eq!(table.len(), 3);
        assert!(!table.is_empty());
    }
}
