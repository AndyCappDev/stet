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
    let mut interp = Interpreter::builder()
        .suppress_output()
        .keep_partial_pages()
        .build();
    let pages = interp
        .render(FAILS_ON_PAGE_2, 18.0)
        .expect("pages are kept");
    assert_eq!(
        pages.len(),
        2,
        "the finished page and the one being painted"
    );
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
    let mut interp = Interpreter::builder()
        .suppress_output()
        .keep_partial_pages()
        .build();
    interp
        .context()
        .set_timeout(Some(std::time::Duration::from_millis(200)));
    let started = std::time::Instant::now();
    let pages = interp
        .render(
            b"%!PS\n0 0 1 setrgbcolor 20 20 100 100 rectfill\n{ 1 1 add pop } loop\n",
            18.0,
        )
        .expect("the painted page is kept");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "took {:?}",
        started.elapsed()
    );
    assert_eq!(pages.len(), 1);
    assert!(interp.last_error().is_some());
}

/// The flush after an error runs under a deadline of its own, and the
/// caller's comes back afterwards: a later call is not stopped by the flush's
/// deadline once it has passed.
#[test]
fn the_flush_gives_the_callers_deadline_back() {
    let mut interp = Interpreter::builder()
        .suppress_output()
        .keep_partial_pages()
        .build();
    interp
        .context()
        .set_timeout(Some(std::time::Duration::from_secs(60)));
    let callers = interp.context().deadline;
    interp
        .render(FAILS_ON_PAGE_2, 18.0)
        .expect("pages are kept");
    assert_eq!(interp.context().deadline, callers);

    // Past the flush's own deadline (2 s), which used to be left behind.
    std::thread::sleep(std::time::Duration::from_millis(2100));
    let pages = interp
        .render(b"%!PS\n20 20 100 100 rectfill showpage\n", 18.0)
        .expect("a later render has the caller's deadline, not the flush's");
    assert_eq!(pages.len(), 1);

    // With no deadline, none is left behind.
    interp.context().set_timeout(None);
    interp
        .render(FAILS_ON_PAGE_2, 18.0)
        .expect("pages are kept");
    assert_eq!(interp.context().deadline, None);
    let eps =
        b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 100 100\n0 0 50 50 rectfill nosuchoperator\n";
    interp.render(eps, 18.0).expect("an EPS's page is kept");
    assert_eq!(interp.context().deadline, None);
}

/// `/Type /Page` objects (not `/Pages`) in an uncompressed PDF.
fn page_objects(pdf: &[u8]) -> usize {
    let text = String::from_utf8_lossy(pdf);
    text.match_indices("/Type /Page")
        .chain(text.match_indices("/Type/Page"))
        .filter(|(i, m)| text.as_bytes().get(i + m.len()) != Some(&b's'))
        .count()
}

#[test]
fn render_to_pdf_keeps_partial_pages_too() {
    let mut interp = Interpreter::builder()
        .suppress_output()
        .keep_partial_pages()
        .build();
    let pdf = interp
        .render_to_pdf(FAILS_ON_PAGE_2, 72.0)
        .expect("the pages are kept");
    assert!(pdf.starts_with(b"%PDF"), "a PDF");
    let pages = page_objects(&pdf);
    assert_eq!(pages, 2, "the finished page and the one being painted");
    match interp.last_error() {
        Some(StetError::PostScript(m)) => assert_eq!(m, "undefined in nosuchoperator"),
        other => panic!("expected last_error, got {other:?} ({pages} pages)"),
    }

    // Without keep_partial_pages it fails, and exec clears last_error.
    let mut strict = Interpreter::builder().suppress_output().build();
    assert!(strict.render_to_pdf(FAILS_ON_PAGE_2, 72.0).is_err());
    interp.exec(b"1 1 add pop").expect("runs");
    assert!(interp.last_error().is_none());
}
