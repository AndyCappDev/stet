// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `render_to_display_list` hands back each page's own display list. The facade's device only records that a page
//! was sent, so the capture takes the page's list itself rather than a copy, and the device gets an empty one.

use stet::Interpreter;
use stet_graphics::display_list::DisplayElement;

fn fills(els: &[DisplayElement]) -> usize {
    els.iter()
        .filter(|e| matches!(e, DisplayElement::Fill { .. }))
        .count()
}

#[test]
fn each_page_keeps_its_own_marks() {
    let ps = b"%!PS\n\
        0 0 10 10 rectfill 20 0 10 10 rectfill showpage\n\
        0 0 10 10 rectfill showpage\n\
        showpage\n";
    let pages = Interpreter::new()
        .render_to_display_list(ps, 72.0)
        .expect("renders");
    let counts: Vec<usize> = pages
        .iter()
        .map(|p| fills(p.display_list.elements()))
        .collect();
    // The erase fill showpage lays down is not a mark: count from the first page's baseline.
    let base = counts[2];
    assert_eq!(
        counts,
        vec![base + 2, base + 1, base],
        "two, one and no rectangles"
    );
}

#[test]
fn a_page_with_many_marks_comes_back_whole() {
    let ps = b"%!PS\n0 1 9999 { 100 mod 0 1 1 rectfill } for showpage\n";
    let pages = Interpreter::new()
        .render_to_display_list(ps, 72.0)
        .expect("renders");
    assert_eq!(pages.len(), 1);
    assert!(fills(pages[0].display_list.elements()) >= 10_000);
}
