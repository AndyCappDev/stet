// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `InterpreterBuilder::default_page_size`: the page a PostScript job starts
//! on when it asks for none.

use stet::Interpreter;

fn page(builder: stet::InterpreterBuilder, src: &str) -> (u32, u32) {
    let pages = builder
        .build()
        .render_to_display_list(src.as_bytes(), 72.0)
        .unwrap();
    assert_eq!(pages.len(), 1);
    (pages[0].width, pages[0].height)
}

const PLAIN: &str = "%!PS-Adobe-3.0\n0 0 moveto 10 10 lineto stroke\nshowpage\n";

#[test]
fn letter_without_it() {
    assert_eq!(page(Interpreter::builder(), PLAIN), (612, 792));
}

#[test]
fn a_job_that_asks_for_nothing_gets_it() {
    let b = Interpreter::builder().default_page_size(842.0, 1191.0);
    assert_eq!(page(b, PLAIN), (842, 1191));
    // The program sees it as its page.
    let src = "%!PS-Adobe-3.0\ncurrentpagedevice /PageSize get aload pop \
               2 copy 1191 eq exch 842 eq and not { nonexistent_page } if \
               moveto 0 0 lineto stroke\nshowpage\n";
    let b = Interpreter::builder().default_page_size(842.0, 1191.0);
    assert_eq!(page(b, src), (842, 1191));
}

#[test]
fn the_program_still_chooses() {
    let src = "%!PS-Adobe-3.0\n<< /PageSize [595 842] >> setpagedevice\n\
               0 0 moveto 10 10 lineto stroke\nshowpage\n";
    let b = Interpreter::builder().default_page_size(842.0, 1191.0);
    assert_eq!(page(b, src), (595, 842));
}

#[test]
fn an_eps_keeps_its_box() {
    let src = "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 100 50\n\
               0 0 moveto 10 10 lineto stroke\n";
    let b = Interpreter::builder().default_page_size(842.0, 1191.0);
    assert_eq!(page(b, src), (100, 50));
}
