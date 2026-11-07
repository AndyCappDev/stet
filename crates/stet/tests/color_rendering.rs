// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `findcolorrendering` and the ColorRendering ProcSet (PLRM 3rd ed., 7.1.3). Illustrator and CorelDRAW EPS
//! prologs copy the ProcSet and replace GetSubstituteCRD whenever `findcolorrendering` is defined.

use stet::Interpreter;

/// Runs `body`; it divides by zero when a check fails, so a passing program renders one page.
fn passes(body: &str) {
    let src = format!("%!PS-Adobe-3.0\n{body}\n0 0 1 setrgbcolor 20 20 100 100 rectfill showpage\n");
    let mut interp = Interpreter::builder().suppress_output().build();
    match interp.render(src.as_bytes(), 18.0) {
        Ok(pages) => assert_eq!(pages.len(), 1),
        Err(e) => panic!("{e:?}"),
    }
}

#[test]
fn an_unknown_intent_gives_the_substitute() {
    passes(
        "/Perceptual findcolorrendering { 1 0 div } if\n\
         /DefaultColorRendering ne { 1 0 div } if\n\
         /DefaultColorRendering /ColorRendering findresource setcolorrendering",
    );
}

#[test]
fn a_program_can_replace_get_substitute_crd() {
    passes(
        "/ColorRendering /ProcSet findresource dup length dict copy begin\n\
           /GetSubstituteCRD { pop /MyCRD } bind def\n\
         currentdict end /ColorRendering exch /ProcSet defineresource pop\n\
         /MyCRD << /ColorRenderingType 1 /WhitePoint [0.9505 1 1.089] >> /ColorRendering defineresource pop\n\
         /Saturation findcolorrendering { 1 0 div } if\n\
         dup /MyCRD ne { 1 0 div } if\n\
         /ColorRendering findresource setcolorrendering",
    );
}

#[test]
fn a_crd_named_for_the_intent_is_found() {
    // The name is intent.pagedevice.halftone, built here from the ProcSet's own procedures.
    passes(
        "/cat { 1 index length 1 index length add string dup 0 4 index putinterval
                dup 3 index length 3 index putinterval exch pop exch pop } def
         /ColorRendering /ProcSet findresource begin
         (RelativeColorimetric.) GetPageDeviceName 64 string cvs cat (.) cat GetHalftoneName 64 string cvs cat cvn
         end
         << /ColorRenderingType 1 /WhitePoint [0.9505 1 1.089] >> /ColorRendering defineresource pop
         /RelativeColorimetric findcolorrendering not { 1 0 div } if pop",
    );
}
