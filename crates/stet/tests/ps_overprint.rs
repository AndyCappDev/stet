// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `setoverprint` in PostScript leaves the colorants a paint does not name
//! untouched (PLRM 4.8.5).
//!
//! PostScript paints used to carry no painted-colorant information and no
//! process-only CMYK, so the renderer's overprint checks never fired for
//! them: a spot colour overprinting cyan knocked the cyan out, while the
//! same content read from a PDF (`/op true`) overprinted correctly. Each
//! case below paints over a cyan square and checks the overlap against
//! either the paint alone (knockout) or a process reference colour on the
//! same page. Ghostscript's `tiffsep` agrees with every expectation here
//! plate for plate.
//!
//! `setoverprintmode` (an Adobe version-3015 extension, PDF's `/OPM`)
//! narrows a DeviceCMYK paint further: under mode 1 with overprint on, the
//! zero components leave their colorants untouched too.
//!
//! Images, `imagemask` included, do not overprint yet: the renderer's
//! overprint image path samples nearest-neighbour and scans the whole band,
//! so routing PostScript images through it cost bitmap text its smoothing
//! and made image-heavy pages several times slower. They join these cases
//! once that path is fixed.
#![cfg(feature = "render")]

use stet::Interpreter;

/// Cases are 100 pt apart; the cyan backdrop covers `(X..X+60, 20..80)` and
/// the paint `(X+30..X+90, 50..110)`, so the overlap centre is `(X+45, 65)`
/// and the paint alone is at `(X+75, 95)`. Two process references follow.
const CASES: &str = r#"%!PS
<< /PageSize [800 120] >> setpagedevice
/spot [/Separation (TestSpot) /DeviceCMYK { 0 exch 0 0 }] def
/bg { false setoverprint 1 0 0 0 setcmykcolor 0 20 60 60 rectfill } def
/fg { 30 50 60 60 rectfill } def
gsave   0 0 translate bg true setoverprint spot setcolorspace 1 setcolor fg grestore
gsave 100 0 translate bg false setoverprint spot setcolorspace 1 setcolor fg grestore
gsave 200 0 translate bg true setoverprint
  [/Separation /Magenta /DeviceCMYK { 0 exch 0 0 }] setcolorspace 1 setcolor fg grestore
gsave 300 0 translate bg true setoverprint 0 1 0 0 setcmykcolor fg grestore
gsave 400 0 translate bg true setoverprint
  [/DeviceN [/Yellow /TestSpot] /DeviceCMYK { 0 exch 0 0 4 -1 roll 0 0 0 }]
  setcolorspace 1 0 setcolor fg grestore
gsave 500 0 translate bg true setoverprint spot setcolorspace 1 setcolor
  40 setlinewidth 60 50 moveto 60 110 lineto stroke grestore
gsave 600 0 translate 1 1 0 0 setcmykcolor fg grestore
gsave 700 0 translate 1 0 1 0 setcmykcolor fg grestore
showpage
"#;

struct Page {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl Page {
    fn render(ps: &str) -> Page {
        let mut interp = Interpreter::new();
        let mut pages = interp.render(ps.as_bytes(), 72.0).unwrap();
        let p = pages.remove(0);
        Page {
            width: p.width,
            height: p.height,
            rgba: p.rgba,
        }
    }

    /// RGB at a point given in PostScript user space (origin bottom-left).
    fn at(&self, x: u32, y: u32) -> [u8; 3] {
        let row = self.height - 1 - y;
        let i = ((row * self.width + x) * 4) as usize;
        [self.rgba[i], self.rgba[i + 1], self.rgba[i + 2]]
    }

    fn overlap(&self, case: u32) -> [u8; 3] {
        self.at(case * 100 + 45, 65)
    }

    fn paint_alone(&self, case: u32) -> [u8; 3] {
        self.at(case * 100 + 75, 95)
    }

    fn cyan_alone(&self, case: u32) -> [u8; 3] {
        self.at(case * 100 + 15, 30)
    }
}

const SPOT_OP: u32 = 0;
const SPOT_KO: u32 = 1;
const SEP_MAGENTA_OP: u32 = 2;
const CMYK_MAGENTA_OP: u32 = 3;
const DEVICEN_YELLOW_OP: u32 = 4;
const STROKE_OP: u32 = 5;
const REF_CM: u32 = 6;
const REF_CY: u32 = 7;

fn reference(page: &Page, case: u32) -> [u8; 3] {
    page.paint_alone(case)
}

#[test]
fn spot_overprint_keeps_the_process_plates_beneath() {
    let page = Page::render(CASES);
    let overlap = page.overlap(SPOT_OP);
    assert_ne!(overlap, page.paint_alone(SPOT_OP), "spot knocked out cyan");
    assert_ne!(overlap, page.cyan_alone(SPOT_OP), "spot not painted");
}

#[test]
fn spot_without_overprint_knocks_out() {
    let page = Page::render(CASES);
    assert_eq!(page.overlap(SPOT_KO), page.paint_alone(SPOT_KO));
}

#[test]
fn process_named_separation_paints_only_its_own_plate() {
    let page = Page::render(CASES);
    assert_eq!(page.overlap(SEP_MAGENTA_OP), reference(&page, REF_CM));
}

/// PostScript has no overprint mode: DeviceCMYK marks all four process
/// colorants, zero components included, so it knocks out even with
/// overprint on.
#[test]
fn device_cmyk_overprint_paints_every_process_plate() {
    let page = Page::render(CASES);
    assert_eq!(
        page.overlap(CMYK_MAGENTA_OP),
        page.paint_alone(CMYK_MAGENTA_OP)
    );
}

#[test]
fn devicen_overprint_paints_only_its_named_plates() {
    let page = Page::render(CASES);
    assert_eq!(page.overlap(DEVICEN_YELLOW_OP), reference(&page, REF_CY));
}

#[test]
fn strokes_overprint_like_fills() {
    let page = Page::render(CASES);
    assert_eq!(page.overlap(STROKE_OP), page.overlap(SPOT_OP));
}

/// A cached Type 3 glyph is a stencil painted with the paint in force when
/// it is shown. Replay used to swap in only the new colour, so a glyph first
/// built with overprint off stayed a knockout when shown with overprint on.
#[test]
fn cached_type3_glyph_takes_the_current_overprint() {
    let page = Page::render(
        r#"%!PS
<< /PageSize [300 120] >> setpagedevice
/spot [/Separation (TestSpot) /DeviceCMYK { 0 exch 0 0 }] def
8 dict begin
/FontType 3 def /FontMatrix [0.01 0 0 0.01 0 0] def /FontBBox [0 0 100 100] def
/Encoding 256 array def 0 1 255 { Encoding exch /.notdef put } for
Encoding 65 /sq put
/BuildChar { pop pop 100 0 0 0 100 100 setcachedevice 0 0 100 100 rectfill } def
currentdict end /SqFont exch definefont pop
/SqFont findfont 60 scalefont setfont
spot setcolorspace 1 setcolor false setoverprint 10 30 moveto (A) show
1 0 0 0 setcmykcolor 100 10 60 60 rectfill
true setoverprint spot setcolorspace 1 setcolor 130 40 moveto (A) show
showpage
"#,
    );
    let spot_alone = page.at(40, 60);
    let cyan_alone = page.at(110, 20);
    let overlap = page.at(145, 55);
    assert_ne!(overlap, spot_alone, "cached glyph knocked out cyan");
    assert_ne!(overlap, cyan_alone, "cached glyph not painted");
}

/// The same layout as [`CASES`], for `setoverprintmode`.
const OPM_CASES: &str = r#"%!PS
<< /PageSize [800 120] >> setpagedevice
/bg { false setoverprint 1 0 0 0 setcmykcolor 0 20 60 60 rectfill } def
/fg { 30 50 60 60 rectfill } def
gsave   0 0 translate bg true setoverprint true setoverprintmode 0 1 0 0 setcmykcolor fg grestore
gsave 100 0 translate bg true setoverprint true setoverprintmode 0 0 0 0 setcmykcolor fg grestore
gsave 200 0 translate bg false setoverprint true setoverprintmode 0 1 0 0 setcmykcolor fg grestore
gsave 300 0 translate bg true setoverprint true setoverprintmode 0 setgray fg grestore
gsave 400 0 translate bg true setoverprint true setoverprintmode 0 1 0 0 setcmykcolor
  40 setlinewidth 60 50 moveto 60 110 lineto stroke grestore
gsave 500 0 translate bg true setoverprint true setoverprintmode
  gsave false setoverprintmode grestore 0 1 0 0 setcmykcolor fg grestore
gsave 600 0 translate 1 1 0 0 setcmykcolor fg grestore
showpage
"#;

const OPM_MAGENTA: u32 = 0;
const OPM_ZERO: u32 = 1;
const OPM_WITHOUT_OVERPRINT: u32 = 2;
const OPM_GRAY: u32 = 3;
const OPM_STROKE: u32 = 4;
const OPM_AFTER_GRESTORE: u32 = 5;
const OPM_REF_CM: u32 = 6;

#[test]
fn opm1_cmyk_leaves_zero_components_untouched() {
    let page = Page::render(OPM_CASES);
    assert_eq!(
        page.overlap(OPM_MAGENTA),
        reference(&page, OPM_REF_CM),
        "OPM 1 magenta knocked out cyan"
    );
}

/// Ghostscript's `tiffsep` paints nothing at all here: PostScript has no
/// PDF-style "OPM set in the same ExtGState as /op" signal, so the strict
/// reading always applies.
#[test]
fn opm1_all_zero_cmyk_paints_nothing() {
    let page = Page::render(OPM_CASES);
    assert_eq!(page.overlap(OPM_ZERO), page.cyan_alone(OPM_ZERO));
}

#[test]
fn opm1_without_overprint_knocks_out() {
    let page = Page::render(OPM_CASES);
    assert_eq!(
        page.overlap(OPM_WITHOUT_OVERPRINT),
        page.paint_alone(OPM_WITHOUT_OVERPRINT)
    );
}

/// Overprint mode applies to DeviceCMYK only; DeviceGray paints every
/// process colorant, so black gray still knocks out cyan.
#[test]
fn opm1_does_not_apply_to_device_gray() {
    let page = Page::render(OPM_CASES);
    assert_eq!(page.overlap(OPM_GRAY), page.paint_alone(OPM_GRAY));
}

#[test]
fn opm1_strokes_like_fills() {
    let page = Page::render(OPM_CASES);
    assert_eq!(page.overlap(OPM_STROKE), reference(&page, OPM_REF_CM));
}

#[test]
fn overprint_mode_is_restored_by_grestore() {
    let page = Page::render(OPM_CASES);
    assert_eq!(
        page.overlap(OPM_AFTER_GRESTORE),
        reference(&page, OPM_REF_CM)
    );
}

/// A cached Type 3 glyph first built under mode 0 must take mode 1 when it
/// is shown again with it in force.
#[test]
fn cached_type3_glyph_takes_the_current_overprint_mode() {
    let page = Page::render(
        r#"%!PS
<< /PageSize [300 120] >> setpagedevice
8 dict begin
/FontType 3 def /FontMatrix [0.01 0 0 0.01 0 0] def /FontBBox [0 0 100 100] def
/Encoding 256 array def 0 1 255 { Encoding exch /.notdef put } for
Encoding 65 /sq put
/BuildChar { pop pop 100 0 0 0 100 100 setcachedevice 0 0 100 100 rectfill } def
currentdict end /SqFont exch definefont pop
/SqFont findfont 60 scalefont setfont
0 1 0 0 setcmykcolor 10 30 moveto (A) show
1 0 0 0 setcmykcolor 100 10 60 60 rectfill
true setoverprint true setoverprintmode 0 1 0 0 setcmykcolor 130 40 moveto (A) show
1 1 0 0 setcmykcolor 220 30 60 60 rectfill
showpage
"#,
    );
    let magenta_alone = page.at(40, 60);
    let overlap = page.at(145, 55);
    assert_ne!(overlap, magenta_alone, "cached glyph knocked out cyan");
    assert_eq!(overlap, page.at(250, 60), "overlap is not cyan + magenta");
}
