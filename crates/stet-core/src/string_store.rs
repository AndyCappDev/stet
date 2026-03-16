// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Contiguous byte buffer for PostScript string storage.
//!
//! Strings are identified by `EntityId` indices into the entity table,
//! which provides indirection for save/restore COW semantics.

use crate::entity_table::EntityTable;
use crate::object::EntityId;

/// Storage for PostScript string byte data.
pub struct StringStore {
    data: Vec<u8>,
    pub entities: EntityTable,
}

impl StringStore {
    pub fn new() -> Self {
        Self {
            data: Vec::new(),
            entities: EntityTable::new(),
        }
    }

    /// Allocate `len` zero-filled bytes, returning an `EntityId`.
    pub fn allocate(&mut self, len: usize) -> EntityId {
        let offset = self.data.len() as u32;
        self.data.resize(self.data.len() + len, 0);
        self.entities.allocate(offset, len as u32, 0, false, 0)
    }

    /// Allocate and copy `bytes` into the store.
    pub fn allocate_from(&mut self, bytes: &[u8]) -> EntityId {
        let offset = self.data.len() as u32;
        self.data.extend_from_slice(bytes);
        self.entities
            .allocate(offset, bytes.len() as u32, 0, false, 0)
    }

    /// Allocate and copy `bytes` with a specific save level and global flag.
    pub fn allocate_from_with(
        &mut self,
        bytes: &[u8],
        save_level: u16,
        global: bool,
        created_after_save: u32,
    ) -> EntityId {
        let offset = self.data.len() as u32;
        self.data.extend_from_slice(bytes);
        self.entities.allocate(
            offset,
            bytes.len() as u32,
            save_level,
            global,
            created_after_save,
        )
    }

    /// Allocate with a specific save level and global flag.
    pub fn allocate_with(
        &mut self,
        len: usize,
        save_level: u16,
        global: bool,
        created_after_save: u32,
    ) -> EntityId {
        let offset = self.data.len() as u32;
        self.data.resize(self.data.len() + len, 0);
        self.entities
            .allocate(offset, len as u32, save_level, global, created_after_save)
    }

    /// Get a slice of the string data via entity table indirection.
    /// `start` is the byte offset from the entity's base; `len` is the number of bytes.
    pub fn get(&self, entity: EntityId, start: u32, len: u32) -> &[u8] {
        let base = self.entities.get(entity).offset as usize + start as usize;
        &self.data[base..base + len as usize]
    }

    /// Get a mutable slice of the string data via entity table indirection.
    /// `start` is the byte offset from the entity's base; `len` is the number of bytes.
    pub fn get_mut(&mut self, entity: EntityId, start: u32, len: u32) -> &mut [u8] {
        let base = self.entities.get(entity).offset as usize + start as usize;
        &mut self.data[base..base + len as usize]
    }

    /// Set a single byte.
    pub fn put_byte(&mut self, entity: EntityId, offset: u32, byte: u8) {
        let base = self.entities.get(entity).offset as usize;
        self.data[base + offset as usize] = byte;
    }

    /// Get a single byte.
    pub fn get_byte(&self, entity: EntityId, offset: u32) -> u8 {
        let base = self.entities.get(entity).offset as usize;
        self.data[base + offset as usize]
    }

    /// Copy entity data to a new region (for COW). Returns the new EntityId
    /// pointing to the copy. The original entity's offset is updated to
    /// point at the copy, so the original EntityId now sees the new data.
    pub fn cow_copy(&mut self, entity: EntityId) -> EntityId {
        let meta = self.entities.get(entity);
        let old_offset = meta.offset as usize;
        let len = meta.len;
        let save_level = meta.save_level;
        let is_global = meta.is_global();
        let created_after_save = meta.created_after_save;

        // Copy data to a new region
        let temp: Vec<u8> = self.data[old_offset..old_offset + len as usize].to_vec();
        let new_offset = self.data.len() as u32;
        self.data.extend_from_slice(&temp);

        // Create a new entity pointing at the OLD data (this is the backup)
        let copy_id = self.entities.allocate(
            meta.offset, // points to original data
            len,
            save_level,
            is_global,
            created_after_save,
        );

        // Update the original entity to point at the NEW copy
        self.entities.get_mut(entity).offset = new_offset;

        copy_id
    }

    /// Swap offsets between two entities (used by restore).
    pub fn swap_offsets(&mut self, a: EntityId, b: EntityId) {
        let off_a = self.entities.get(a).offset;
        let off_b = self.entities.get(b).offset;
        self.entities.get_mut(a).offset = off_b;
        self.entities.get_mut(b).offset = off_a;
    }

    /// Access to the backing data (for advanced operations).
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

impl Default for StringStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allocate_from() {
        let mut store = StringStore::new();
        let id = store.allocate_from(b"hello");
        assert_eq!(store.get(id, 0, 5), b"hello");
    }

    #[test]
    fn test_allocate_zeroed() {
        let mut store = StringStore::new();
        let id = store.allocate(3);
        assert_eq!(store.get(id, 0, 3), &[0, 0, 0]);
    }

    #[test]
    fn test_put_get_byte() {
        let mut store = StringStore::new();
        let id = store.allocate(3);
        store.put_byte(id, 1, 42);
        assert_eq!(store.get_byte(id, 0), 0);
        assert_eq!(store.get_byte(id, 1), 42);
    }

    #[test]
    fn test_multiple_strings() {
        let mut store = StringStore::new();
        let id1 = store.allocate_from(b"abc");
        let id2 = store.allocate_from(b"xyz");
        assert_eq!(store.get(id1, 0, 3), b"abc");
        assert_eq!(store.get(id2, 0, 3), b"xyz");
    }

    #[test]
    fn test_entity_indirection() {
        let mut store = StringStore::new();
        let id = store.allocate_from(b"test");
        let meta = store.entities.get(id);
        assert_eq!(meta.len, 4);
        assert_eq!(meta.save_level, 0);
        assert!(!meta.is_global());
    }

    #[test]
    fn test_cow_copy() {
        let mut store = StringStore::new();
        let id = store.allocate_from(b"hello");

        // Mutate via the original entity
        store.put_byte(id, 0, b'H');
        assert_eq!(store.get(id, 0, 5), b"Hello");

        // COW copy: backup the original, original now points to copy
        let backup = store.cow_copy(id);

        // Modify the original — should not affect the backup
        store.put_byte(id, 1, b'a');
        assert_eq!(store.get(id, 0, 5), b"Hallo");
        assert_eq!(store.get(backup, 0, 5), b"Hello");
    }

    #[test]
    fn test_swap_offsets() {
        let mut store = StringStore::new();
        let id1 = store.allocate_from(b"aaa");
        let id2 = store.allocate_from(b"bbb");

        store.swap_offsets(id1, id2);
        assert_eq!(store.get(id1, 0, 3), b"bbb");
        assert_eq!(store.get(id2, 0, 3), b"aaa");
    }

    #[test]
    fn test_allocate_with_save_level() {
        let mut store = StringStore::new();
        let id = store.allocate_with(5, 2, true, 0);
        let meta = store.entities.get(id);
        assert_eq!(meta.save_level, 2);
        assert!(meta.is_global());
    }
}
