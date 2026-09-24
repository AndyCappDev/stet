// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A form drawn with `execform` keeps its transparency groups, soft masks
//! and layers.
//!
//! `execform` caches a form's output in form space and replays it through
//! the CTM of each use. Groups, soft masks and layers were dropped from the
//! replay as "PDF-only", but stet's PostScript transparency operators make
//! them too, so a form built with them drew nothing. They cannot be replayed
//! from form space — their bounding boxes are resolved in device space — so
//! such a form is now executed afresh at each use.
#![cfg(feature = "render")]

use stet::Interpreter;

/// Five 100 × 100 cells, each filled blue by a form: a group form used
/// twice, a group without a `/BBox` (bounded by the clip instead), a layer,
/// and a soft mask.
const FORMS: &str = "%!PS
<< /PageSize [500 100] >> setpagedevice
/Group << /FormType 1 /BBox [0 0 100 100] /Matrix [1 0 0 1 0 0]
  /PaintProc { pop
    << /Isolated true /BBox [0 0 100 100] >> begintransparencygroup
    0 0 1 setrgbcolor 10 10 80 80 rectfill
    endtransparencygroup }
>> def
/ClipBoundGroup << /FormType 1 /BBox [0 0 100 100] /Matrix [1 0 0 1 0 0]
  /PaintProc { pop
    << /Isolated true >> begintransparencygroup
    0 0 1 setrgbcolor 10 10 80 80 rectfill
    endtransparencygroup }
>> def
/Layer << /FormType 1 /BBox [0 0 100 100] /Matrix [1 0 0 1 0 0]
  /PaintProc { pop
    << /Name (Blue) /DefaultVisible true >> defineocg beginoptionalcontent
    0 0 1 setrgbcolor 10 10 80 80 rectfill
    endoptionalcontent }
>> def
/Masked << /FormType 1 /BBox [0 0 100 100] /Matrix [1 0 0 1 0 0]
  /PaintProc { pop
    << /Subtype /Alpha /BBox [0 0 100 100] >> beginsoftmask
    0 0 100 100 rectfill
    endsoftmask
    0 0 1 setrgbcolor 10 10 80 80 rectfill
    clearsoftmask }
>> def
gsave   0 0 translate Group execform grestore
gsave 100 0 translate Group execform grestore
gsave 200 0 translate ClipBoundGroup execform grestore
gsave 300 0 translate Layer execform grestore
gsave 400 0 translate Masked execform grestore
showpage
";

#[test]
fn transparency_constructs_inside_forms_are_drawn() {
    let mut interp = Interpreter::new();
    let pages = interp.render(FORMS.as_bytes(), 72.0).unwrap();
    let page = &pages[0];
    assert_eq!((page.width, page.height), (500, 100));
    let pixel = |x: u32, y: u32| {
        let i = ((y * page.width + x) * 4) as usize;
        [page.rgba[i], page.rgba[i + 1], page.rgba[i + 2]]
    };
    for (cell, what) in [
        "group",
        "group, second use",
        "clip-bound group",
        "layer",
        "soft mask",
    ]
    .iter()
    .enumerate()
    {
        let centre = pixel(cell as u32 * 100 + 50, 50);
        assert_eq!(centre, [0, 0, 255], "{what} (cell {cell}) drew {centre:?}");
        // And nothing leaks outside the square.
        let margin = pixel(cell as u32 * 100 + 5, 50);
        assert_eq!(
            margin,
            [255, 255, 255],
            "{what} (cell {cell}) margin {margin:?}"
        );
    }
}
