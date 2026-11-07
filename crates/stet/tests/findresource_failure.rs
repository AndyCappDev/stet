// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A `findresource` that fails leaves its operands on the stack, as any operator does (PLRM 3.11.1).
//! Prologs depend on it: `{/OCProcSet /ProcSet findresource} stopped not {...} {pop pop ...} ifelse`.

use stet::Interpreter;

/// Blue when the procedure leaves exactly `want` objects on the stack, red otherwise.
fn leaves(probe: &str, want: usize) -> [u8; 3] {
    let src = format!(
        "%!PS\n{{ {probe} }} stopped pop count {want} eq {{ 0 0 1 }} {{ 1 0 0 }} ifelse setrgbcolor clear 0 0 100 100 rectfill showpage\n"
    );
    let mut interp = Interpreter::builder().suppress_output().build();
    let pages = interp.render(src.as_bytes(), 18.0).expect("renders");
    assert_eq!(pages.len(), 1);
    let w = pages[0].width as usize;
    let at = ((pages[0].height as usize - 5) * w + 5) * 4;
    [
        pages[0].rgba[at],
        pages[0].rgba[at + 1],
        pages[0].rgba[at + 2],
    ]
}

#[test]
fn a_missing_procset_leaves_key_and_category() {
    assert_eq!(
        leaves("/NoSuchProcSet /ProcSet findresource", 2),
        [0, 0, 255]
    );
}

#[test]
fn a_missing_category_leaves_key_and_category() {
    assert_eq!(
        leaves("/NoSuchCategory /Category findresource", 2),
        [0, 0, 255]
    );
}

#[test]
fn a_missing_instance_of_an_implicit_category_leaves_key_and_category() {
    assert_eq!(leaves("/NoSuchFilter /Filter findresource", 2), [0, 0, 255]);
}

#[test]
fn a_found_procset_is_still_one_object() {
    assert_eq!(
        leaves("/ColorRendering /ProcSet findresource", 1),
        [0, 0, 255]
    );
}
