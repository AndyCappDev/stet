// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A job's leftovers are discarded before the `restore` that ends it.
//!
//! The facade wraps every job in `save` / `restore`. A program that ends with
//! composite objects on the operand stack, or with dictionaries it created
//! still on the dictionary stack, is common and legal; those objects belong
//! to the job's VM, which the restore releases. Discarding them only after
//! the restore left the stacks holding released objects for the length of
//! it — which the debug-build dangling-reference audit rejects, so a library
//! user's debug test suite panicked on such a program.

use stet::Interpreter;

const LEFTOVERS: &str = "%!PS\n\
    10 dict [1 2 3] (a string)\n\
    5 dict begin /x 1 def\n\
    72 72 moveto 100 100 lineto stroke\n\
    showpage\n\
    20 dict\n";

#[test]
fn render_to_display_list_discards_leftovers() {
    let mut interp = Interpreter::new();
    let pages = interp
        .render_to_display_list(LEFTOVERS.as_bytes(), 72.0)
        .unwrap();
    assert_eq!(pages.len(), 1);
    // And the interpreter is still usable for the next job.
    let pages = interp
        .render_to_display_list(LEFTOVERS.as_bytes(), 72.0)
        .unwrap();
    assert_eq!(pages.len(), 1);
}

#[test]
fn exec_discards_leftovers() {
    let mut interp = Interpreter::new();
    interp.exec(LEFTOVERS.as_bytes()).unwrap();
    interp.exec(LEFTOVERS.as_bytes()).unwrap();
}

#[cfg(feature = "render")]
#[test]
fn render_discards_leftovers() {
    let mut interp = Interpreter::new();
    let pages = interp.render(LEFTOVERS.as_bytes(), 72.0).unwrap();
    assert_eq!(pages.len(), 1);
}

#[cfg(feature = "pdf-output")]
#[test]
fn render_to_pdf_discards_leftovers() {
    let mut interp = Interpreter::new();
    let pdf = interp.render_to_pdf(LEFTOVERS.as_bytes(), 72.0).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
}
