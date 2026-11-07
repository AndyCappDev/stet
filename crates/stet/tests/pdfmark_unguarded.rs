// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Without the PDF device `pdfmark` stays undefined, so prologs that test `/pdfmark where` take their
//! printer branch, but a file that calls it unguarded keeps rendering: the call consumes its operands
//! to the mark.

use stet::Interpreter;

#[test]
fn pdfmark_is_still_undefined_for_where() {
    let src = b"%!PS\n/pdfmark where { pop 1 0 0 setrgbcolor } { 0 0 1 setrgbcolor } ifelse 0 0 100 100 rectfill showpage\n";
    let mut interp = Interpreter::builder().suppress_output().build();
    let pages = interp.render(src, 18.0).expect("renders");
    assert_eq!(pages.len(), 1);
    let px = &pages[0].rgba;
    // The printer branch: blue.
    let w = pages[0].width as usize;
    let at = ((pages[0].height as usize - 5) * w + 5) * 4;
    assert_eq!(&px[at..at + 3], &[0, 0, 255], "blue, not red");
}

#[test]
fn an_unguarded_pdfmark_consumes_its_operands() {
    let src = b"%!PS\n[ /Title (A document) /DOCINFO pdfmark\n\
        [ /Dest /here /View [/XYZ null null null] /DEST pdfmark\n\
        count 0 ne { 1 0 0 setrgbcolor } { 0 0 1 setrgbcolor } ifelse 0 0 100 100 rectfill showpage\n";
    let mut interp = Interpreter::builder().suppress_output().build();
    let pages = interp.render(src, 18.0).expect("renders");
    assert_eq!(pages.len(), 1);
    let w = pages[0].width as usize;
    let at = ((pages[0].height as usize - 5) * w + 5) * 4;
    assert_eq!(
        &pages[0].rgba[at..at + 3],
        &[0, 0, 255],
        "an empty operand stack"
    );
}

#[test]
fn a_pdfmark_inside_a_procedure_consumes_its_operands() {
    // Procedures resolve names on their own fast path: a bound and an unbound one.
    let src = b"%!PS\n/mark1 { [ /Title (A) /DOCINFO pdfmark } def\n\
        /mark2 { [ /Subject (B) /DOCINFO pdfmark } bind def\n\
        mark1 mark2 mark1\n\
        count 0 ne { 1 0 0 setrgbcolor } { 0 0 1 setrgbcolor } ifelse 0 0 100 100 rectfill showpage\n";
    let mut interp = Interpreter::builder().suppress_output().build();
    let pages = interp.render(src, 18.0).expect("renders");
    assert_eq!(pages.len(), 1);
    let w = pages[0].width as usize;
    let at = ((pages[0].height as usize - 5) * w + 5) * 4;
    assert_eq!(
        &pages[0].rgba[at..at + 3],
        &[0, 0, 255],
        "an empty operand stack"
    );
}
