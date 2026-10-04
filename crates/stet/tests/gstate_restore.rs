// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `restore` reverts what `currentgstate` writes into a `gstate` object made
//! before the `save`.
//!
//! The write had no backup, so the object kept the state inside the save —
//! and with it the objects `restore` reclaimed. Reinstating it named a page
//! device that no longer existed, and the interpreter panicked.

use stet::Interpreter;

#[test]
fn a_gstate_written_inside_a_save_is_reverted_by_the_restore() {
    // `0 0 div` raises an error, failing the job, if the gstate does not
    // hold the page device it was made with.
    let job = b"<< /PageSize [300 300] >> setpagedevice
        /g gstate def
        save << /PageSize [200 100] >> setpagedevice g currentgstate pop restore
        50 { 10 dict pop } repeat
        gsave g setgstate
          currentpagedevice /PageSize get aload pop 300 ne exch 300 ne or { 0 0 div } if
        grestore";
    Interpreter::new().exec(job).unwrap();
}
