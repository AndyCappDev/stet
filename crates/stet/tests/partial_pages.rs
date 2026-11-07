// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `InterpreterBuilder::keep_partial_pages` and errors reported by name.

use stet::{Interpreter, StetError};

/// One finished page, then marks on a second and an undefined name.
const FAILS_ON_PAGE_2: &[u8] = b"%!PS-Adobe-3.0\n\
    0 0 1 setrgbcolor 20 20 100 100 rectfill showpage\n\
    1 0 0 setrgbcolor 20 20 100 100 rectfill\n\
    nosuchoperator\n";

#[test]
fn an_error_fails_the_render_by_default_and_names_itself() {
    let mut interp = Interpreter::builder().suppress_output().build();
    match interp.render(FAILS_ON_PAGE_2, 18.0) {
        Err(StetError::PostScript(m)) => assert_eq!(m, "undefined in nosuchoperator"),
        other => panic!("expected the named error, got {:?}", other.map(|p| p.len())),
    }
}

#[test]
fn partial_pages_keep_what_was_painted() {
    let mut interp = Interpreter::builder().suppress_output().keep_partial_pages().build();
    let pages = interp.render(FAILS_ON_PAGE_2, 18.0).expect("pages are kept");
    assert_eq!(pages.len(), 2, "the finished page and the one being painted");
    match interp.last_error() {
        Some(StetError::PostScript(m)) => assert_eq!(m, "undefined in nosuchoperator"),
        other => panic!("expected last_error, got {other:?}"),
    }

    // A clean run clears it.
    let pages = interp
        .render(b"%!PS\n20 20 100 100 rectfill showpage\n", 18.0)
        .expect("renders");
    assert_eq!(pages.len(), 1);
    assert!(interp.last_error().is_none());
}

/// A job stopped by its deadline is not resumed by the `showpage` that keeps
/// its page: an endless loop ends at the deadline, with what it painted.
#[test]
fn a_timeout_ends_the_job_it_interrupted() {
    let mut interp = Interpreter::builder().suppress_output().keep_partial_pages().build();
    interp.context().set_timeout(Some(std::time::Duration::from_millis(200)));
    let started = std::time::Instant::now();
    let pages = interp
        .render(b"%!PS\n0 0 1 setrgbcolor 20 20 100 100 rectfill\n{ 1 1 add pop } loop\n", 18.0)
        .expect("the painted page is kept");
    assert!(started.elapsed() < std::time::Duration::from_secs(5), "took {:?}", started.elapsed());
    assert_eq!(pages.len(), 1);
    assert!(interp.last_error().is_some());
}
