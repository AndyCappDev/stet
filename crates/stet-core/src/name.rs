// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Name interning table.
//!
//! Maps byte sequences to `NameId` values. Names persist forever
//! (not subject to save/restore or GC).

use rustc_hash::FxHashMap as HashMap;

use crate::object::NameId;

/// Interning table: maps byte sequences to unique `NameId` values.
pub struct NameTable {
    names: Vec<Vec<u8>>,
    lookup: HashMap<Vec<u8>, NameId>,
}

impl NameTable {
    pub fn new() -> Self {
        Self {
            names: Vec::new(),
            lookup: HashMap::default(),
        }
    }

    /// Get or create a `NameId` for the given byte sequence.
    pub fn intern(&mut self, name: &[u8]) -> NameId {
        if let Some(&id) = self.lookup.get(name) {
            return id;
        }
        let id = NameId(self.names.len() as u32);
        self.names.push(name.to_vec());
        self.lookup.insert(name.to_vec(), id);
        id
    }

    /// Get the byte sequence for a `NameId`.
    pub fn get_bytes(&self, id: NameId) -> &[u8] {
        &self.names[id.0 as usize]
    }

    /// Look up a name without creating it.
    pub fn find(&self, name: &[u8]) -> Option<NameId> {
        self.lookup.get(name).copied()
    }
}

impl Default for NameTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_intern_and_lookup() {
        let mut table = NameTable::new();
        let id1 = table.intern(b"add");
        let id2 = table.intern(b"sub");
        let id3 = table.intern(b"add"); // same as id1

        assert_eq!(id1, id3);
        assert_ne!(id1, id2);
        assert_eq!(table.get_bytes(id1), b"add");
        assert_eq!(table.get_bytes(id2), b"sub");
    }

    #[test]
    fn test_find() {
        let mut table = NameTable::new();
        assert_eq!(table.find(b"foo"), None);
        let id = table.intern(b"foo");
        assert_eq!(table.find(b"foo"), Some(id));
    }
}
