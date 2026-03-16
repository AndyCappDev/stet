// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PostScript dictionary storage.
//!
//! Dictionaries are backed by `HashMap` and identified by `EntityId`.
//! An entity table provides indirection for save/restore COW semantics.

use rustc_hash::FxHashMap as HashMap;

use crate::entity_table::EntityTable;
use crate::object::{EntityId, NameId, ObjFlags, PsObject};

/// Key type for PostScript dictionary entries.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DictKey {
    Name(NameId),
    Int(i32),
    Real(u64), // f64 bits for hashable comparison
    Bool(bool),
    String(Vec<u8>), // String keys are copied (per PLRM)
    Operator(u16),   // Operator opcode
    /// Identity key for composite objects (array, packedarray, dict).
    /// Uses (entity_id, start, len) for arrays, (entity_id, 0, 0) for dicts.
    Identity(u32, u32, u32),
}

/// A single dictionary with its metadata.
pub struct DictEntry {
    pub max_length: usize,
    pub entries: HashMap<DictKey, PsObject>,
    pub access: u8,
    pub name: Vec<u8>,
}

/// Storage for all PostScript dictionaries.
///
/// The entity table maps EntityId → index into `dicts`. For Phase 2 this is
/// a 1:1 mapping (entity N → dicts\[N\]), but the indirection enables
/// save_level tracking and future COW.
pub struct DictStore {
    dicts: Vec<DictEntry>,
    pub entities: EntityTable,
}

impl DictStore {
    pub fn new() -> Self {
        Self {
            dicts: Vec::new(),
            entities: EntityTable::new(),
        }
    }

    /// Allocate a new dictionary with the given maximum length and name.
    pub fn allocate(&mut self, max_length: usize, name: &[u8]) -> EntityId {
        let index = self.dicts.len() as u32;
        self.dicts.push(DictEntry {
            max_length,
            entries: HashMap::with_capacity_and_hasher(max_length.min(64), Default::default()),
            access: ObjFlags::ACCESS_UNLIMITED,
            name: name.to_vec(),
        });
        // Entity offset = index into dicts vec
        self.entities.allocate(index, 0, 0, false, 0)
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
        let index = self.dicts.len() as u32;
        self.dicts.push(DictEntry {
            max_length,
            entries: HashMap::with_capacity_and_hasher(max_length.min(64), Default::default()),
            access: ObjFlags::ACCESS_UNLIMITED,
            name: name.to_vec(),
        });
        self.entities
            .allocate(index, 0, save_level, global, created_after_save)
    }

    /// Resolve entity to dict index.
    fn dict_index(&self, entity: EntityId) -> usize {
        self.entities.get(entity).offset as usize
    }

    /// Look up a key in a dictionary.
    #[inline]
    pub fn get(&self, entity: EntityId, key: &DictKey) -> Option<PsObject> {
        self.dicts[self.dict_index(entity)]
            .entries
            .get(key)
            .copied()
    }

    /// Insert or update a key-value pair.
    pub fn put(&mut self, entity: EntityId, key: DictKey, value: PsObject) {
        let idx = self.dict_index(entity);
        self.dicts[idx].entries.insert(key, value);
    }

    /// Check if a key exists.
    pub fn known(&self, entity: EntityId, key: &DictKey) -> bool {
        self.dicts[self.dict_index(entity)]
            .entries
            .contains_key(key)
    }

    /// Get the dict's name.
    pub fn get_name(&self, entity: EntityId) -> &[u8] {
        &self.dicts[self.dict_index(entity)].name
    }

    /// Set the dict's name.
    pub fn set_name(&mut self, entity: EntityId, name: &[u8]) {
        let idx = self.dict_index(entity);
        self.dicts[idx].name.clear();
        self.dicts[idx].name.extend_from_slice(name);
    }

    /// Current number of entries.
    pub fn length(&self, entity: EntityId) -> usize {
        self.dicts[self.dict_index(entity)].entries.len()
    }

    /// Maximum capacity.
    pub fn max_length(&self, entity: EntityId) -> usize {
        self.dicts[self.dict_index(entity)].max_length
    }

    /// Remove a key.
    pub fn remove(&mut self, entity: EntityId, key: &DictKey) {
        let idx = self.dict_index(entity);
        self.dicts[idx].entries.remove(key);
    }

    /// Borrow the dict entry.
    pub fn entry(&self, entity: EntityId) -> &DictEntry {
        &self.dicts[self.dict_index(entity)]
    }

    /// Mutably borrow the dict entry.
    pub fn entry_mut(&mut self, entity: EntityId) -> &mut DictEntry {
        let idx = self.dict_index(entity);
        &mut self.dicts[idx]
    }

    /// Iterate over keys of a dictionary.
    pub fn keys(&self, entity: EntityId) -> impl Iterator<Item = &DictKey> {
        self.dicts[self.dict_index(entity)].entries.keys()
    }

    /// Get the access level of a dictionary.
    pub fn access(&self, entity: EntityId) -> u8 {
        self.dicts[self.dict_index(entity)].access
    }

    /// Require read access on a dict. Returns InvalidAccess if access < READ_ONLY.
    #[inline]
    pub fn require_read(&self, entity: EntityId) -> Result<(), crate::error::PsError> {
        if self.access(entity) >= ObjFlags::ACCESS_READ_ONLY {
            Ok(())
        } else {
            Err(crate::error::PsError::InvalidAccess)
        }
    }

    /// Require write access on a dict. Returns InvalidAccess if access < UNLIMITED.
    #[inline]
    pub fn require_write(&self, entity: EntityId) -> Result<(), crate::error::PsError> {
        if self.access(entity) >= ObjFlags::ACCESS_UNLIMITED {
            Ok(())
        } else {
            Err(crate::error::PsError::InvalidAccess)
        }
    }

    /// Set the access level of a dictionary.
    pub fn set_access(&mut self, entity: EntityId, access: u8) {
        let idx = self.dict_index(entity);
        self.dicts[idx].access = access;
    }

    /// Copy a dictionary's contents to a new dict (for COW). Returns the new EntityId.
    /// The original entity is updated to point at the copy; the returned entity
    /// points at the original data (the backup).
    pub fn cow_copy(&mut self, entity: EntityId) -> EntityId {
        let idx = self.dict_index(entity);
        let meta = self.entities.get(entity);
        let save_level = meta.save_level;
        let is_global = meta.is_global();
        let created_after_save = meta.created_after_save;

        // Clone the dict
        let orig = &self.dicts[idx];
        let copy = DictEntry {
            max_length: orig.max_length,
            entries: orig.entries.clone(),
            access: orig.access,
            name: orig.name.clone(),
        };

        let new_index = self.dicts.len() as u32;
        self.dicts.push(copy);

        // Backup points to original index
        let backup_id = self.entities.allocate(
            idx as u32, // original dict index
            0,
            save_level,
            is_global,
            created_after_save,
        );

        // Original entity now points to new copy
        self.entities.get_mut(entity).offset = new_index;

        backup_id
    }

    /// Swap indices between two entities (used by restore).
    pub fn swap_offsets(&mut self, a: EntityId, b: EntityId) {
        let off_a = self.entities.get(a).offset;
        let off_b = self.entities.get(b).offset;
        self.entities.get_mut(a).offset = off_b;
        self.entities.get_mut(b).offset = off_a;
    }
}

impl Default for DictStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dict_basic() {
        let mut store = DictStore::new();
        let d = store.allocate(10, b"testdict");
        let key = DictKey::Name(NameId(0));
        assert!(!store.known(d, &key));

        store.put(d, key.clone(), PsObject::int(42));
        assert!(store.known(d, &key));
        assert_eq!(store.get(d, &key).unwrap().as_i32(), Some(42));
        assert_eq!(store.length(d), 1);
        assert_eq!(store.max_length(d), 10);

        store.remove(d, &key);
        assert!(!store.known(d, &key));
    }

    #[test]
    fn test_multiple_dicts() {
        let mut store = DictStore::new();
        let d1 = store.allocate(10, b"dict1");
        let d2 = store.allocate(10, b"dict2");
        let key = DictKey::Int(1);

        store.put(d1, key.clone(), PsObject::int(100));
        store.put(d2, key.clone(), PsObject::int(200));

        assert_eq!(store.get(d1, &key).unwrap().as_i32(), Some(100));
        assert_eq!(store.get(d2, &key).unwrap().as_i32(), Some(200));
    }

    #[test]
    fn test_cow_copy() {
        let mut store = DictStore::new();
        let d = store.allocate(10, b"test");
        let key = DictKey::Name(NameId(0));
        store.put(d, key.clone(), PsObject::int(42));

        let backup = store.cow_copy(d);

        // Modify original — should not affect backup
        store.put(d, key.clone(), PsObject::int(99));
        assert_eq!(store.get(d, &key).unwrap().as_i32(), Some(99));
        assert_eq!(store.get(backup, &key).unwrap().as_i32(), Some(42));
    }

    #[test]
    fn test_swap_offsets() {
        let mut store = DictStore::new();
        let d1 = store.allocate(10, b"a");
        let d2 = store.allocate(10, b"b");
        let key = DictKey::Int(0);
        store.put(d1, key.clone(), PsObject::int(1));
        store.put(d2, key.clone(), PsObject::int(2));

        store.swap_offsets(d1, d2);
        assert_eq!(store.get(d1, &key).unwrap().as_i32(), Some(2));
        assert_eq!(store.get(d2, &key).unwrap().as_i32(), Some(1));
    }
}
