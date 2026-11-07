// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Execution engine for the stet PostScript interpreter.
//!
//! This crate is the eval loop that drives PostScript execution. It
//! processes the execution stack one object at a time: executable names
//! are looked up in the dictionary stack and dispatched, operators run,
//! and procedures (`{…}`) are stepped through element-by-element. The
//! three public entry points are:
//!
//! - [`eval::parse_and_exec`] — tokenise and execute a byte slice of
//!   PostScript source.
//! - [`eval::parse_and_exec_file`] — same, with a canonical file path
//!   recorded so relative resource lookups work.
//! - [`eval::exec_sync`] — synchronously execute an already-parsed
//!   procedure object; used by operators such as `if`, `for`, `loop`.
//!
//! Most users should use the [`stet`](https://crates.io/crates/stet)
//! facade crate rather than depending on `stet-engine` directly.

pub mod eval;
