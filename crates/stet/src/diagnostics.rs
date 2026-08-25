// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Execution-time diagnostics — non-fatal problems noticed while running a
//! PostScript program that would otherwise be silent.
//!
//! A PostScript program communicates "this page is finished, emit it" by
//! calling `showpage`. A program that paints marks and then ends without one
//! leaves those marks on a page the device is never asked to output, and the
//! page is discarded. That is correct behaviour — it is what the PLRM
//! specifies and what Ghostscript's file devices do — but from the outside it
//! is indistinguishable from a broken renderer: [`Interpreter::render`]
//! returns `Ok(vec![])` and no file appears.
//!
//! [`Interpreter::warnings`] surfaces these so callers can say *why* nothing
//! came out. Warnings are recorded per call and cleared at the start of the
//! next one.
//!
//! [`Interpreter::render`]: crate::Interpreter::render
//! [`Interpreter::warnings`]: crate::Interpreter::warnings

use stet_core::context::Context;

/// One non-fatal problem noticed while executing a PostScript program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecWarning {
    /// What kind of problem this is, with the details needed to act on it.
    pub kind: ExecWarningKind,
}

/// The kind of execution-time problem, and its particulars.
///
/// `#[non_exhaustive]`: new kinds land additively, so `match` sites need a
/// `_ =>` arm.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExecWarningKind {
    /// The program painted marks and then ended without a matching
    /// `showpage`, so the partially-composed page was discarded.
    ///
    /// When `pages_emitted` is 0 this is the whole output: the program
    /// produced nothing at all. When it is non-zero, earlier pages came out
    /// fine and only the trailing one was lost.
    DroppedFinalPage {
        /// How many display-list objects were on the discarded page.
        objects: usize,
        /// How many pages the program did successfully emit before ending.
        pages_emitted: i32,
    },
}

impl ExecWarning {
    /// The remedy for this warning, phrased for a human reading a log.
    ///
    /// Kept separate from [`Display`] so callers rendering into a UI can show
    /// the problem and the fix in different places.
    ///
    /// [`Display`]: std::fmt::Display
    pub fn hint(&self) -> &'static str {
        match self.kind {
            ExecWarningKind::DroppedFinalPage { .. } => {
                "Append `showpage` to the program, or mark it as EPS (an `EPSF` header line \
                 or a `.eps` extension) to have stet emit the final page implicitly."
            }
        }
    }
}

impl std::fmt::Display for ExecWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            ExecWarningKind::DroppedFinalPage {
                objects,
                pages_emitted,
            } => {
                write!(
                    f,
                    "painted {} object(s) and then ended without a matching `showpage`, so ",
                    objects
                )?;
                if pages_emitted == 0 {
                    write!(f, "no page was output")
                } else {
                    write!(
                        f,
                        "that page was dropped ({} earlier page(s) were output)",
                        pages_emitted
                    )
                }
            }
        }
    }
}

/// Detect a page that was painted but never emitted.
///
/// Call this at end-of-job **before** the device is finished and before
/// `vm_restore` — the page device this reads `PageCount` from is torn down by
/// the restore, and `showpage` is what would have cleared the display list.
///
/// Returns `None` when the program ended cleanly with nothing pending, which
/// is the overwhelmingly common case.
pub fn dropped_final_page(ctx: &Context) -> Option<ExecWarning> {
    if ctx.display_list.is_empty() {
        return None;
    }
    Some(ExecWarning {
        kind: ExecWarningKind::DroppedFinalPage {
            objects: ctx.display_list.len(),
            pages_emitted: stet_ops::device_ops::get_pd_int(ctx, b"PageCount").unwrap_or(0),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_names_total_loss_when_no_pages_came_out() {
        let w = ExecWarning {
            kind: ExecWarningKind::DroppedFinalPage {
                objects: 8,
                pages_emitted: 0,
            },
        };
        assert_eq!(
            w.to_string(),
            "painted 8 object(s) and then ended without a matching `showpage`, \
             so no page was output"
        );
    }

    #[test]
    fn message_names_partial_loss_when_earlier_pages_came_out() {
        let w = ExecWarning {
            kind: ExecWarningKind::DroppedFinalPage {
                objects: 1,
                pages_emitted: 5,
            },
        };
        assert_eq!(
            w.to_string(),
            "painted 1 object(s) and then ended without a matching `showpage`, \
             so that page was dropped (5 earlier page(s) were output)"
        );
    }

    #[test]
    fn hint_points_at_both_remedies() {
        let w = ExecWarning {
            kind: ExecWarningKind::DroppedFinalPage {
                objects: 1,
                pages_emitted: 0,
            },
        };
        assert!(w.hint().contains("showpage"));
        assert!(w.hint().contains(".eps"));
    }
}
