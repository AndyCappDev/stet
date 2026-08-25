// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `Interpreter::warnings` — the dropped-final-page diagnostic.
//!
//! A program that paints and never calls `showpage` renders nothing. That is
//! correct, but an empty page list looks identical to "the program drew
//! nothing", so the warning is the only thing telling the two apart.

use stet::{ExecWarningKind, Interpreter};

/// Paints a filled rectangle, never calls `showpage`.
const NO_SHOWPAGE: &[u8] = b"%!PS-Adobe-3.0\n0 0 1 setrgbcolor 20 20 100 100 rectfill\n";

/// The same drawing, properly terminated.
const WITH_SHOWPAGE: &[u8] =
    b"%!PS-Adobe-3.0\n0 0 1 setrgbcolor 20 20 100 100 rectfill\nshowpage\n";

/// Two pages' worth of marks but only one `showpage` — the second is lost.
const TRAILING_PAGE: &[u8] = b"%!PS-Adobe-3.0\n\
    0 0 1 setrgbcolor 20 20 100 100 rectfill showpage\n\
    1 0 0 setrgbcolor 20 20 100 100 rectfill\n";

fn dropped(interp: &Interpreter) -> Option<(usize, i32)> {
    interp.warnings().iter().find_map(|w| match w.kind {
        ExecWarningKind::DroppedFinalPage {
            objects,
            pages_emitted,
        } => Some((objects, pages_emitted)),
        _ => None,
    })
}

#[test]
fn missing_showpage_yields_no_pages_and_a_warning() {
    let mut interp = Interpreter::new();
    let pages = interp.render_to_display_list(NO_SHOWPAGE, 72.0).unwrap();

    // The empty result is the symptom the warning exists to explain.
    assert!(pages.is_empty(), "a program with no showpage emits no page");
    let (objects, pages_emitted) = dropped(&interp).expect("expected a DroppedFinalPage warning");
    assert!(objects > 0, "the discarded page had marks on it");
    assert_eq!(pages_emitted, 0, "nothing was emitted before the drop");
}

#[test]
fn proper_showpage_is_silent() {
    let mut interp = Interpreter::new();
    let pages = interp.render_to_display_list(WITH_SHOWPAGE, 72.0).unwrap();

    assert_eq!(pages.len(), 1);
    assert!(
        interp.warnings().is_empty(),
        "a well-formed program must not warn: {:?}",
        interp.warnings()
    );
}

#[test]
fn trailing_marks_after_a_showpage_report_the_earlier_pages() {
    let mut interp = Interpreter::new();
    let pages = interp.render_to_display_list(TRAILING_PAGE, 72.0).unwrap();

    assert_eq!(pages.len(), 1, "only the shown page comes out");
    let (_, pages_emitted) = dropped(&interp).expect("expected a DroppedFinalPage warning");
    assert_eq!(
        pages_emitted, 1,
        "the message must distinguish partial loss from total loss"
    );
}

#[test]
fn warnings_are_cleared_between_calls() {
    let mut interp = Interpreter::new();

    let _ = interp.render_to_display_list(NO_SHOWPAGE, 72.0).unwrap();
    assert!(dropped(&interp).is_some());

    // A clean run on the same interpreter must not inherit the stale warning.
    let _ = interp.render_to_display_list(WITH_SHOWPAGE, 72.0).unwrap();
    assert!(
        interp.warnings().is_empty(),
        "warnings describe the latest call only: {:?}",
        interp.warnings()
    );
}

#[test]
fn eps_gets_an_implicit_showpage_and_does_not_warn() {
    let mut interp = Interpreter::new();
    // EPSF header + BoundingBox routes this through the EPS path, which
    // supplies the missing showpage itself.
    let eps = b"%!PS-Adobe-3.0 EPSF-3.0\n\
        %%BoundingBox: 0 0 200 200\n\
        %%EndComments\n\
        0 0 1 setrgbcolor 20 20 100 100 rectfill\n";
    let pages = interp.render_to_display_list(eps, 72.0).unwrap();

    assert_eq!(pages.len(), 1, "EPS renders without an explicit showpage");
    assert!(
        interp.warnings().is_empty(),
        "the implicit showpage means nothing was dropped: {:?}",
        interp.warnings()
    );
}

#[cfg(feature = "render")]
#[test]
fn render_surfaces_the_warning_too() {
    let mut interp = Interpreter::new();
    let pages = interp.render(NO_SHOWPAGE, 72.0).unwrap();

    assert!(pages.is_empty());
    assert!(
        dropped(&interp).is_some(),
        "render() delegates to render_to_display_list, so it must warn as well"
    );
}

#[cfg(feature = "pdf-output")]
#[test]
fn pdf_output_warns_on_a_dropped_page() {
    let mut interp = Interpreter::new();
    let _ = interp.render_to_pdf(NO_SHOWPAGE, 72.0).unwrap();

    assert!(
        dropped(&interp).is_some(),
        "the PDF path drops the final page the same way the raster path does"
    );
}
