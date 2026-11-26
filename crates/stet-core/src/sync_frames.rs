// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What each callback into PostScript keeps in use while it runs.
//!
//! An operator that calls a procedure and carries on afterwards (a tint
//! transform, a Type 3 `BuildChar`, `kshow`'s procedure, an image data
//! source) does so through `exec_sync`, a nested eval loop. While the
//! procedure runs, the operator still holds objects in Rust locals that it
//! will read when the procedure returns: the string being shown, the font,
//! the procedure itself if it is to be called again. On Ghostscript those
//! sit on the execution stack, so a `restore` inside the procedure that would
//! reclaim one of them raises `invalidrestore` (PLRM 3e, `restore`). In stet
//! they are on no stack, and the restore freed them.
//!
//! So each `exec_sync` call is a [frame](SyncFrames), recording the newest
//! save live when it began and, if the caller declared them, the objects it
//! holds (its *roots*). `restore` looks at every frame it would cross, that
//! is, every callback that began after the save being restored was made:
//!
//! - a **declared** frame's roots are checked like stack entries;
//! - an **undeclared** frame is `invalidrestore` outright. That is stricter
//!   than Ghostscript, but only for a call site whose holdings nobody has
//!   listed, and it can only be too strict, visibly: a new call site cannot
//!   bring back a dangling reference.
//!
//! A frame entered after the save was made holds nothing older than its own
//! entry that the save could reclaim, so it is skipped. Save ids only grow
//! and are never reused, which is what makes that a comparison of two ids.

use crate::object::PsObject;

/// One `exec_sync` call in progress.
#[derive(Clone, Copy, Debug)]
struct SyncFrame {
    /// [`SaveStack::last_save_id`](crate::save_stack::SaveStack::last_save_id)
    /// when the callback began: 0 if no save was live.
    entered_under_save: u32,
    /// Where this frame's roots start in [`SyncFrames::roots`]. They run to
    /// the next frame's start, or to the end.
    roots_start: usize,
    /// Whether the caller named what it holds.
    declared: bool,
}

/// The `exec_sync` calls in progress, innermost last, and what each holds.
///
/// Maintained by the engine's `exec_sync` alone, which begins a frame on
/// entry and ends it on every return, error or not, as it does
/// `Context::exec_sync_depth`. A caller declares its roots with
/// [`Context::exec_sync_holding`](crate::context::Context::exec_sync_holding).
#[derive(Debug, Default)]
pub struct SyncFrames {
    frames: Vec<SyncFrame>,
    roots: Vec<PsObject>,
    /// Set by [`declare`](Self::declare) for the frame about to begin: where
    /// its roots start.
    pending: Option<usize>,
}

impl SyncFrames {
    /// No callbacks in progress.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record `held` and `proc` as the roots of the next frame to begin.
    ///
    /// `proc` is always a root: a caller that runs it again needs it.
    /// Called by `Context::exec_sync_holding` just before `exec_sync`.
    pub fn declare(&mut self, proc: PsObject, held: &[PsObject]) {
        // A declaration nobody consumed (see `abandon_declaration`) would
        // otherwise hand its roots to an unrelated frame.
        self.abandon_declaration();
        self.pending = Some(self.roots.len());
        self.roots.extend_from_slice(held);
        self.roots.push(proc);
    }

    /// Drop a declaration no frame took up.
    ///
    /// Only an `exec_sync` function that does not maintain frames leaves one
    /// (an embedder may install its own); the declared roots are discarded
    /// rather than left to be attributed to the next frame.
    pub fn abandon_declaration(&mut self) {
        if let Some(start) = self.pending.take() {
            self.roots.truncate(start);
        }
    }

    /// Begin a frame for an `exec_sync` call, entered while `last_save_id`
    /// was the newest live save. Declared if [`declare`](Self::declare) was
    /// called for it.
    pub fn begin(&mut self, last_save_id: u32) {
        let (roots_start, declared) = match self.pending.take() {
            Some(start) => (start, true),
            None => (self.roots.len(), false),
        };
        self.frames.push(SyncFrame {
            entered_under_save: last_save_id,
            roots_start,
            declared,
        });
    }

    /// End the innermost frame, releasing its roots.
    pub fn end(&mut self) {
        if let Some(frame) = self.frames.pop() {
            self.roots.truncate(frame.roots_start);
        }
    }

    /// Number of frames in progress.
    pub fn depth(&self) -> usize {
        self.frames.len()
    }

    /// The frames a restore of `save_id` crosses, innermost last: for each,
    /// `Some(roots)` if its caller declared them, `None` if not.
    ///
    /// A frame is crossed if it began while `save_id` was live, that is,
    /// `save_id <= entered_under_save`.
    pub fn crossed_by(&self, save_id: u32) -> impl Iterator<Item = Option<&[PsObject]>> {
        self.frames
            .iter()
            .enumerate()
            .filter(move |(_, f)| save_id <= f.entered_under_save)
            .map(|(i, f)| {
                f.declared.then(|| {
                    let end = self
                        .frames
                        .get(i + 1)
                        .map_or(self.roots.len(), |next| next.roots_start);
                    &self.roots[f.roots_start..end]
                })
            })
    }

    /// Every declared root of every frame in progress, for the debug sweep
    /// for dangling references.
    pub fn roots(&self) -> &[PsObject] {
        &self.roots
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots_of(frames: &SyncFrames, save_id: u32) -> Vec<Option<Vec<i64>>> {
        frames
            .crossed_by(save_id)
            .map(|r| r.map(|r| r.iter().map(|o| o.as_i64().unwrap()).collect()))
            .collect()
    }

    #[test]
    fn frame_entered_before_the_save_is_not_crossed() {
        let mut f = SyncFrames::new();
        f.begin(0);
        assert!(roots_of(&f, 1).is_empty());
        f.end();
        assert_eq!(f.depth(), 0);
    }

    #[test]
    fn undeclared_frame_has_no_roots() {
        let mut f = SyncFrames::new();
        f.begin(3);
        assert_eq!(roots_of(&f, 3), vec![None]);
        assert_eq!(roots_of(&f, 2), vec![None]);
        assert!(roots_of(&f, 4).is_empty());
    }

    #[test]
    fn declared_roots_are_the_proc_and_what_is_held() {
        let mut f = SyncFrames::new();
        f.declare(PsObject::int(9), &[PsObject::int(1), PsObject::int(2)]);
        f.begin(2);
        assert_eq!(roots_of(&f, 1), vec![Some(vec![1, 2, 9])]);
    }

    #[test]
    fn nested_frames_keep_their_own_roots() {
        let mut f = SyncFrames::new();
        f.declare(PsObject::int(10), &[PsObject::int(1)]);
        f.begin(1);
        f.begin(2); // undeclared, between two declared frames
        f.declare(PsObject::int(30), &[PsObject::int(3)]);
        f.begin(3);
        assert_eq!(
            roots_of(&f, 1),
            vec![Some(vec![1, 10]), None, Some(vec![3, 30])]
        );
        assert_eq!(roots_of(&f, 3), vec![Some(vec![3, 30])]);
        f.end();
        assert_eq!(f.roots().len(), 2);
        f.end();
        f.end();
        assert!(f.roots().is_empty());
    }

    #[test]
    fn an_abandoned_declaration_goes_to_no_frame() {
        let mut f = SyncFrames::new();
        f.declare(PsObject::int(7), &[]);
        f.abandon_declaration();
        f.begin(1);
        assert_eq!(roots_of(&f, 1), vec![None]);
        f.end();
        assert!(f.roots().is_empty());
    }
}
