// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A `restore` must not cross a callback into PostScript.
//!
//! An operator that calls a procedure and carries on afterwards (a tint
//! transform, a Type 3 `BuildChar`, `kshow`'s procedure, an image data
//! source) does so through `exec_sync`, a nested eval loop. While the
//! procedure runs, the operator still holds objects in Rust locals that it
//! reads when the procedure returns: the string being shown, the font, the
//! procedure itself if it is to be called again. They are on no stack, so a
//! `restore` inside the procedure of a save made before the call freed them,
//! and the operator's next read of one panicked.
//!
//! So each `exec_sync` call is a frame recording the newest save live when it
//! began, and `restore` raises `invalidrestore` if it would cross one: if the
//! save it restores was made before some callback in progress began. A save
//! made inside the callback can be restored there as usual.
//!
//! This is what Ghostscript does, checked against 10.05 at every call site:
//! each of these operators allocates its working state in VM when it starts
//! and keeps it on the execution stack, so that state is always newer than
//! an enclosing save, and PLRM 3e's rule for `restore` (`invalidrestore` if a
//! stack holds a local composite newer than the save) refuses the restore
//! even when everything the program passed in is older. The one difference
//! is `execform` with a form dictionary older than the save, which
//! Ghostscript runs to an incoherent graphics state (its closing `grestore`
//! pops past the save's) and stet refuses.
//!
//! Save ids only grow and are never reused, which is what makes "made before
//! the callback began" a comparison of two ids.

/// The `exec_sync` calls in progress, innermost last: for each, the newest
/// save live when it began.
///
/// Maintained by the engine's `exec_sync` alone, which begins a frame on
/// entry and ends it on every return, error or not, as it does
/// `Context::exec_sync_depth`.
#[derive(Debug, Default)]
pub struct SyncFrames {
    /// [`SaveStack::last_save_id`](crate::save_stack::SaveStack::last_save_id)
    /// when each callback began: 0 if no save was live.
    entered_under_save: Vec<u32>,
}

impl SyncFrames {
    /// No callbacks in progress.
    pub fn new() -> Self {
        Self::default()
    }

    /// Begin a frame for an `exec_sync` call, entered while `last_save_id`
    /// was the newest live save.
    pub fn begin(&mut self, last_save_id: u32) {
        self.entered_under_save.push(last_save_id);
    }

    /// End the innermost frame.
    pub fn end(&mut self) {
        self.entered_under_save.pop();
    }

    /// Number of frames in progress.
    pub fn depth(&self) -> usize {
        self.entered_under_save.len()
    }

    /// Whether a restore of `save_id` would cross a callback in progress:
    /// whether some frame began while `save_id` was live
    /// (`save_id <= entered_under_save`).
    pub fn crossed_by(&self, save_id: u32) -> bool {
        self.entered_under_save.iter().any(|&e| save_id <= e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_entered_before_the_save_is_not_crossed() {
        let mut f = SyncFrames::new();
        f.begin(0);
        assert!(!f.crossed_by(1));
        f.end();
        assert_eq!(f.depth(), 0);
    }

    #[test]
    fn frame_entered_under_the_save_is_crossed() {
        let mut f = SyncFrames::new();
        f.begin(3);
        assert!(f.crossed_by(3));
        assert!(f.crossed_by(2));
        assert!(!f.crossed_by(4));
    }

    #[test]
    fn any_crossed_frame_counts_and_end_releases_it() {
        let mut f = SyncFrames::new();
        f.begin(1);
        f.begin(5);
        assert!(f.crossed_by(4));
        f.end();
        assert!(!f.crossed_by(4));
        assert!(f.crossed_by(1));
        f.end();
        assert!(!f.crossed_by(1));
    }
}
