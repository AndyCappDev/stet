// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Storage for PostScript array contents.
//!
//! Each array is a contiguous range of `PsObject` values. The `EntityId`
//! indexes into the entity table, which provides the base offset into the
//! flat data vector. Subarrays use `start` and `len` fields in the
//! `PsObject` for view support.

use crate::entity_table::EntityTable;
use crate::object::{EntityId, PsObject};

/// Storage for PostScript array element data.
pub struct ArrayStore {
    data: Vec<PsObject>,
    pub entities: EntityTable,
}

impl ArrayStore {
    pub fn new() -> Self {
        Self {
            data: Vec::new(),
            entities: EntityTable::new(),
        }
    }

    /// Allocate `len` null-filled slots, returning an `EntityId`.
    pub fn allocate(&mut self, len: usize) -> EntityId {
        let offset = self.data.len() as u32;
        self.data.resize(self.data.len() + len, PsObject::null());
        self.entities.allocate(offset, len as u32, 0, false, 0)
    }

    /// Allocate and copy `items` into the store.
    pub fn allocate_from(&mut self, items: &[PsObject]) -> EntityId {
        let offset = self.data.len() as u32;
        self.data.extend_from_slice(items);
        self.entities
            .allocate(offset, items.len() as u32, 0, false, 0)
    }

    /// Allocate and copy `items` with a specific save level and global flag.
    pub fn allocate_from_with(
        &mut self,
        items: &[PsObject],
        save_level: u16,
        global: bool,
        created_after_save: u32,
    ) -> EntityId {
        let offset = self.data.len() as u32;
        self.data.extend_from_slice(items);
        self.entities.allocate(
            offset,
            items.len() as u32,
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
        self.data.resize(self.data.len() + len, PsObject::null());
        self.entities
            .allocate(offset, len as u32, save_level, global, created_after_save)
    }

    /// Get a slice of array elements via entity table indirection.
    pub fn get(&self, entity: EntityId, start: u32, len: u32) -> &[PsObject] {
        let base = self.entities.get(entity).offset as usize + start as usize;
        &self.data[base..base + len as usize]
    }

    /// Get a mutable slice of array elements via entity table indirection.
    pub fn get_mut(&mut self, entity: EntityId, start: u32, len: u32) -> &mut [PsObject] {
        let base = self.entities.get(entity).offset as usize + start as usize;
        &mut self.data[base..base + len as usize]
    }

    /// Get a single element.
    #[inline]
    pub fn get_element(&self, entity: EntityId, index: u32) -> PsObject {
        let base = self.entities.get(entity).offset as usize;
        self.data[base + index as usize]
    }

    /// Set a single element.
    pub fn set_element(&mut self, entity: EntityId, index: u32, obj: PsObject) {
        let base = self.entities.get(entity).offset as usize;
        self.data[base + index as usize] = obj;
    }

    /// Copy entity data to a new region (for COW). Returns the new EntityId
    /// pointing at the backup. The original entity's offset is updated to
    /// point at the fresh copy.
    pub fn cow_copy(&mut self, entity: EntityId) -> EntityId {
        let meta = self.entities.get(entity);
        let old_offset = meta.offset as usize;
        let len = meta.len;
        let save_level = meta.save_level;
        let is_global = meta.is_global();
        let created_after_save = meta.created_after_save;

        let temp: Vec<PsObject> = self.data[old_offset..old_offset + len as usize].to_vec();
        let new_offset = self.data.len() as u32;
        self.data.extend_from_slice(&temp);

        // Backup entity points to original data
        let copy_id = self.entities.allocate(
            meta.offset, // original offset
            len,
            save_level,
            is_global,
            created_after_save,
        );

        // Original entity now points to new copy
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
}

impl Default for ArrayStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::PsValue;

    #[test]
    fn test_allocate_from() {
        let mut store = ArrayStore::new();
        let items = [PsObject::int(1), PsObject::int(2), PsObject::int(3)];
        let id = store.allocate_from(&items);
        assert_eq!(store.get_element(id, 0).as_i32(), Some(1));
        assert_eq!(store.get_element(id, 1).as_i32(), Some(2));
        assert_eq!(store.get_element(id, 2).as_i32(), Some(3));
    }

    #[test]
    fn test_allocate_null_filled() {
        let mut store = ArrayStore::new();
        let id = store.allocate(3);
        for i in 0..3 {
            assert!(matches!(store.get_element(id, i).value, PsValue::Null));
        }
    }

    #[test]
    fn test_set_element() {
        let mut store = ArrayStore::new();
        let id = store.allocate(2);
        store.set_element(id, 0, PsObject::int(42));
        assert_eq!(store.get_element(id, 0).as_i32(), Some(42));
    }

    #[test]
    fn test_subarray_view() {
        let mut store = ArrayStore::new();
        let items = [
            PsObject::int(10),
            PsObject::int(20),
            PsObject::int(30),
            PsObject::int(40),
        ];
        let id = store.allocate_from(&items);
        // Subarray starting at index 1, length 2
        let sub = store.get(id, 1, 2);
        assert_eq!(sub[0].as_i32(), Some(20));
        assert_eq!(sub[1].as_i32(), Some(30));
    }

    #[test]
    fn test_cow_copy() {
        let mut store = ArrayStore::new();
        let items = [PsObject::int(1), PsObject::int(2), PsObject::int(3)];
        let id = store.allocate_from(&items);

        let backup = store.cow_copy(id);

        // Modify original — should not affect backup
        store.set_element(id, 0, PsObject::int(99));
        assert_eq!(store.get_element(id, 0).as_i32(), Some(99));
        assert_eq!(store.get_element(backup, 0).as_i32(), Some(1));
    }

    #[test]
    fn test_swap_offsets() {
        let mut store = ArrayStore::new();
        let id1 = store.allocate_from(&[PsObject::int(1)]);
        let id2 = store.allocate_from(&[PsObject::int(2)]);

        store.swap_offsets(id1, id2);
        assert_eq!(store.get_element(id1, 0).as_i32(), Some(2));
        assert_eq!(store.get_element(id2, 0).as_i32(), Some(1));
    }
}
