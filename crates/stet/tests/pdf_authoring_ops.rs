// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `pdfmark` and the distiller-parameter operators are in `systemdict` for a
//! PDF job and only for it, however many jobs one `Interpreter` runs.
//!
//! `render_to_pdf` registered them afresh on every call: the operator table
//! grew by three each time until its `u16` opcodes wrapped, after 21 723
//! calls, and `pdfmark` ran another operator. And `systemdict` is in global
//! VM, which `restore` does not touch, so they outlived the job and a later
//! screen render took the Distiller branch of prologues that ask
//! `systemdict /pdfmark known`.
#![cfg(feature = "pdf-output")]

use stet::Interpreter;

/// Fails the job unless `systemdict /pdfmark known` is `expected`.
fn require_pdfmark(expected: bool) -> String {
    format!(
        "%!PS\n systemdict /pdfmark known {expected} ne {{ pdfmark-visibility-wrong }} if\n\
         systemdict /setdistillerparams known {expected} ne {{ pdfmark-visibility-wrong }} if\n\
         systemdict /currentdistillerparams known {expected} ne {{ pdfmark-visibility-wrong }} if\n"
    )
}

#[test]
fn only_a_pdf_job_sees_pdfmark() {
    let mut interp = Interpreter::new();
    let screen = require_pdfmark(false);
    let pdf = require_pdfmark(true);
    interp.render(screen.as_bytes(), 72.0).unwrap();
    interp.render_to_pdf(pdf.as_bytes(), 72.0).unwrap();
    interp.render(screen.as_bytes(), 72.0).unwrap();
    interp
        .render_to_display_list(screen.as_bytes(), 72.0)
        .unwrap();
    interp.exec(screen.as_bytes()).unwrap();
    interp.render_to_pdf(pdf.as_bytes(), 72.0).unwrap();
    // Also after a PDF job that fails.
    assert!(interp.render_to_pdf(b"%!PS\nundefined-op\n", 72.0).is_err());
    interp.render(screen.as_bytes(), 72.0).unwrap();
}

#[test]
fn pdf_jobs_do_not_grow_the_operator_table() {
    let mut interp = Interpreter::new();
    interp.render_to_pdf(b"%!PS\n", 72.0).unwrap();
    let operators = interp.context().operators.len();
    for _ in 0..3 {
        interp
            .render_to_pdf(b"%!PS\n[ /Title (t) /DOCINFO pdfmark\n", 72.0)
            .unwrap();
    }
    assert_eq!(interp.context().operators.len(), operators);
}
