// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Save/restore stack for PostScript VM persistence.
//!
//! Implements copy-on-write save/restore: `save` records a level, mutations
//! create COW copies, and `restore` swaps offsets to revert changes.

use crate::graphics_state::{GraphicsState, GstateEntry};
use crate::object::EntityId;

/// Which store type a save record refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreType {
    String,
    Array,
    Dict,
}

/// Records one COW copy made during a save level.
#[derive(Debug, Clone)]
pub struct SaveRecord {
    /// The original entity that was COW-copied.
    pub src: EntityId,
    /// The backup entity holding the pre-mutation data.
    pub copy: EntityId,
    /// Which store the entities belong to.
    pub store_type: StoreType,
}

/// One save level's state.
pub struct SaveLevel {
    /// Numeric level (1-based, 0 = no save active).
    pub level: u16,
    /// Unique save id for invalidation tracking.
    pub save_id: u32,
    /// COW records accumulated during this save level.
    pub records: Vec<SaveRecord>,
    /// Whether this save level is still valid (becomes false on restore).
    pub valid: bool,
    /// Snapshot of d_stack length at save time (for restore validation).
    pub d_stack_depth: usize,
    /// Saved packing mode (`setpacking`/`currentpacking`).
    pub packing_mode: bool,
    /// Saved VM allocation mode (`setglobal`/`currentglobal`).
    pub vm_alloc_mode: bool,
    /// Saved binary object format (`setobjectformat`/`currentobjectformat`).
    pub object_format: i32,
    /// Saved graphics state and graphics state stack.
    pub gstate: GraphicsState,
    pub gstate_stack: Vec<GstateEntry>,
}

/// The save/restore stack.
pub struct SaveStack {
    levels: Vec<SaveLevel>,
    next_save_id: u32,
}

impl SaveStack {
    /// Create an empty save stack.
    pub fn new() -> Self {
        Self {
            levels: Vec::new(),
            next_save_id: 1,
        }
    }

    /// Push a new save level. Returns `(level, save_id)`.
    pub fn save(
        &mut self,
        d_stack_depth: usize,
        packing_mode: bool,
        vm_alloc_mode: bool,
        object_format: i32,
        gstate: GraphicsState,
        gstate_stack: Vec<GstateEntry>,
    ) -> (u16, u32) {
        let level = (self.levels.len() + 1) as u16;
        let save_id = self.next_save_id;
        self.next_save_id += 1;
        self.levels.push(SaveLevel {
            level,
            save_id,
            records: Vec::new(),
            valid: true,
            d_stack_depth,
            packing_mode,
            vm_alloc_mode,
            object_format,
            gstate,
            gstate_stack,
        });
        (level, save_id)
    }

    /// Add a COW record to the current save level.
    pub fn add_record(&mut self, record: SaveRecord) {
        if let Some(level) = self.levels.last_mut() {
            level.records.push(record);
        }
    }

    /// Pop the topmost save level, returning its records for restore processing.
    /// Returns `None` if the stack is empty.
    pub fn restore(&mut self) -> Option<SaveLevel> {
        self.levels.pop()
    }

    /// Pop all save levels from `save_id` upward (inclusive), returning them
    /// in stack order (target level first, newest level last).
    /// Per PLRM, `restore` can target any valid save — not just the topmost.
    /// All newer saves are invalidated and their COW records are also returned
    /// so they can be undone.
    pub fn restore_to(&mut self, save_id: u32) -> Option<Vec<SaveLevel>> {
        let idx = self.levels.iter().position(|l| l.save_id == save_id)?;
        let popped: Vec<SaveLevel> = self.levels.drain(idx..).collect();
        Some(popped)
    }

    /// Current save level (0 if no save active).
    pub fn current_level(&self) -> u16 {
        self.levels.last().map(|l| l.level).unwrap_or(0)
    }

    /// Save ID of the most recent save (0 if no save active).
    /// Used for entity creation tracking (invalidrestore).
    pub fn last_save_id(&self) -> u32 {
        self.levels.last().map(|l| l.save_id).unwrap_or(0)
    }

    /// Check if a save_id is valid (exists and not invalidated).
    pub fn is_valid(&self, save_id: u32) -> bool {
        self.levels.iter().any(|l| l.save_id == save_id && l.valid)
    }

    /// Number of active save levels.
    pub fn depth(&self) -> usize {
        self.levels.len()
    }

    /// Read-only access to the levels (for validation checks).
    pub fn levels_ref(&self) -> &[SaveLevel] {
        &self.levels
    }

    /// Invalidate all save levels newer than the given save_id.
    pub fn invalidate_newer(&mut self, save_id: u32) {
        let mut found = false;
        for level in &mut self.levels {
            if found {
                level.valid = false;
            }
            if level.save_id == save_id {
                found = true;
            }
        }
    }
}

impl Default for SaveStack {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_save_and_depth() {
        let mut ss = SaveStack::new();
        assert_eq!(ss.depth(), 0);
        assert_eq!(ss.current_level(), 0);

        let (level, id) = ss.save(3, false, false, 0, GraphicsState::new(), Vec::new());
        assert_eq!(level, 1);
        assert_eq!(id, 1);
        assert_eq!(ss.depth(), 1);
        assert_eq!(ss.current_level(), 1);
    }

    #[test]
    fn test_nested_save() {
        let mut ss = SaveStack::new();
        let (l1, _) = ss.save(3, false, false, 0, GraphicsState::new(), Vec::new());
        let (l2, _) = ss.save(3, false, false, 0, GraphicsState::new(), Vec::new());
        assert_eq!(l1, 1);
        assert_eq!(l2, 2);
        assert_eq!(ss.depth(), 2);
        assert_eq!(ss.current_level(), 2);
    }

    #[test]
    fn test_restore() {
        let mut ss = SaveStack::new();
        let (_, id1) = ss.save(3, false, false, 0, GraphicsState::new(), Vec::new());
        ss.add_record(SaveRecord {
            src: EntityId(0),
            copy: EntityId(1),
            store_type: StoreType::String,
        });

        let level = ss.restore().unwrap();
        assert_eq!(level.save_id, id1);
        assert_eq!(level.records.len(), 1);
        assert_eq!(ss.depth(), 0);
    }

    #[test]
    fn test_is_valid() {
        let mut ss = SaveStack::new();
        let (_, id1) = ss.save(3, false, false, 0, GraphicsState::new(), Vec::new());
        assert!(ss.is_valid(id1));
        ss.restore();
        assert!(!ss.is_valid(id1));
    }

    #[test]
    fn test_invalidate_newer() {
        let mut ss = SaveStack::new();
        let (_, id1) = ss.save(3, false, false, 0, GraphicsState::new(), Vec::new());
        let (_, id2) = ss.save(3, false, false, 0, GraphicsState::new(), Vec::new());
        let (_, id3) = ss.save(3, false, false, 0, GraphicsState::new(), Vec::new());

        ss.invalidate_newer(id1);
        assert!(ss.is_valid(id1));
        assert!(!ss.is_valid(id2));
        assert!(!ss.is_valid(id3));
    }

    #[test]
    fn test_add_record_to_current() {
        let mut ss = SaveStack::new();
        ss.save(3, false, false, 0, GraphicsState::new(), Vec::new());
        ss.add_record(SaveRecord {
            src: EntityId(0),
            copy: EntityId(1),
            store_type: StoreType::Array,
        });
        ss.add_record(SaveRecord {
            src: EntityId(2),
            copy: EntityId(3),
            store_type: StoreType::Dict,
        });

        let level = ss.restore().unwrap();
        assert_eq!(level.records.len(), 2);
    }

    #[test]
    fn test_restore_empty() {
        let mut ss = SaveStack::new();
        assert!(ss.restore().is_none());
    }

    #[test]
    fn test_d_stack_depth_snapshot() {
        let mut ss = SaveStack::new();
        ss.save(5, false, false, 0, GraphicsState::new(), Vec::new());
        let level = ss.restore().unwrap();
        assert_eq!(level.d_stack_depth, 5);
    }

    #[test]
    fn test_unique_save_ids() {
        let mut ss = SaveStack::new();
        let (_, id1) = ss.save(3, false, false, 0, GraphicsState::new(), Vec::new());
        let (_, id2) = ss.save(3, false, false, 0, GraphicsState::new(), Vec::new());
        ss.restore();
        let (_, id3) = ss.save(3, false, false, 0, GraphicsState::new(), Vec::new());
        assert_ne!(id1, id2);
        assert_ne!(id2, id3);
        assert_ne!(id1, id3);
    }

    #[test]
    fn test_save_level_numbers() {
        let mut ss = SaveStack::new();
        let (l1, _) = ss.save(3, false, false, 0, GraphicsState::new(), Vec::new());
        let (l2, _) = ss.save(3, false, false, 0, GraphicsState::new(), Vec::new());
        ss.restore();
        // After restoring level 2, next save should be level 2 again
        let (l3, _) = ss.save(3, false, false, 0, GraphicsState::new(), Vec::new());
        assert_eq!(l1, 1);
        assert_eq!(l2, 2);
        assert_eq!(l3, 2);
    }
}
