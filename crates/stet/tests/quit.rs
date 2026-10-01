// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `quit` ends a job cleanly, with the pages it printed.
//!
//! `quit` is PostScript: it clears `$error /newerror` and calls `stop`, so a
//! program that quits reaches the facade as the same `stop` an error does.
//! The facade took every `stop` for an error, so a program ending in
//! `showpage quit` — common in hand-written and generated PostScript —
//! failed with "PostScript error: stop" and its pages were discarded.

use stet::Interpreter;

const QUITS: &[u8] = b"%!PS\n0 0 1 setrgbcolor 0 0 100 100 rectfill showpage quit\n";

/// Nothing after `quit` runs: one page, not two.
const QUITS_EARLY: &[u8] = b"%!PS\n0 0 100 100 rectfill showpage quit\n\
    0 0 100 100 rectfill showpage\n";

/// An error the program caught leaves `newerror` set; `quit` clears it.
const QUITS_AFTER_CAUGHT_ERROR: &[u8] =
    b"%!PS\n{ nosuchoperator } stopped pop 0 0 100 100 rectfill showpage quit\n";

const ERRS: &[u8] = b"%!PS\n0 0 100 100 rectfill showpage nosuchoperator\n";

#[test]
fn render_to_display_list_keeps_the_pages_of_a_job_that_quits() {
    let mut interp = Interpreter::new();
    for (ps, pages) in [(QUITS, 1), (QUITS_EARLY, 1), (QUITS_AFTER_CAUGHT_ERROR, 1)] {
        let got = interp.render_to_display_list(ps, 72.0).unwrap();
        assert_eq!(got.len(), pages);
    }
}

#[test]
fn exec_of_a_job_that_quits_succeeds() {
    let mut interp = Interpreter::new();
    interp.exec(QUITS).unwrap();
    interp.exec(QUITS_AFTER_CAUGHT_ERROR).unwrap();
}

#[test]
fn an_error_is_still_an_error() {
    let mut interp = Interpreter::new();
    assert!(interp.render_to_display_list(ERRS, 72.0).is_err());
    assert!(interp.exec(ERRS).is_err());
    // And a clean job after it is clean: the error's `newerror` is not
    // left over to fail the next job's `quit`.
    assert_eq!(interp.render_to_display_list(QUITS, 72.0).unwrap().len(), 1);
}

#[cfg(feature = "render")]
#[test]
fn render_keeps_the_pages_of_a_job_that_quits() {
    let mut interp = Interpreter::new();
    let pages = interp.render(QUITS, 72.0).unwrap();
    assert_eq!(pages.len(), 1);
    // The page was painted, not just counted: its corner is the fill.
    let p = &pages[0];
    let corner = ((p.height - 1) * p.width * 4) as usize;
    assert_eq!(&p.rgba[corner..corner + 3], &[0, 0, 255]);
}

#[cfg(feature = "pdf-output")]
#[test]
fn render_to_pdf_of_a_job_that_quits_succeeds() {
    let mut interp = Interpreter::new();
    let pdf = interp.render_to_pdf(QUITS, 72.0).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    assert!(interp.render_to_pdf(ERRS, 72.0).is_err());
}
