// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A Separation or DeviceN colour whose alternate space is CIE-based is
//! drawn in that space.
//!
//! The tint transform's results were taken by their number alone, as one
//! gray, three RGB or four CMYK values clamped to 0-1. pdftops writes an
//! InDesign spot colour as a Separation over a CIEBasedABC space set up as
//! Lab, so a green (L* 52, a* -67, b* 34) became RGB (1, 0, 1): every
//! green spot colour on the page came out magenta, in fills, in images and
//! in PDF output.

use stet::Interpreter;

/// A CIEBasedABC space set up as Lab (D50), as pdftops writes one.
const LAB: &str = "[/CIEBasedABC << /RangeABC [0 100 -128 127 -128 127] \
    /DecodeABC [{16 add 116 div} bind {500 div} bind {200 div} bind] \
    /MatrixABC [1 1 1 1 0 0 0 0 -1] \
    /DecodeLMN [{dup 6 29 div ge {dup dup mul mul} {4 29 div sub 108 841 div mul} ifelse 0.9642 mul} bind \
                {dup 6 29 div ge {dup dup mul mul} {4 29 div sub 108 841 div mul} ifelse} bind \
                {dup 6 29 div ge {dup dup mul mul} {4 29 div sub 108 841 div mul} ifelse 0.8249 mul} bind] \
    /WhitePoint [0.9642 1 0.8249] >>]";

/// PANTONE 355 C as pdftops writes it: tint 1 is L* 52.2, a* -67, b* 34.
fn green_spot() -> String {
    format!(
        "[/Separation (PANTONE 355 C) {LAB} \
         {{ dup -47.8 mul 100 add exch dup -67 mul exch 34 mul }}]"
    )
}

/// The page, 100 points square at 72 dpi, as RGB rows.
fn render(body: &str) -> Vec<u8> {
    let ps = format!("%!PS\n<< /PageSize [100 100] >> setpagedevice\n{body}\nshowpage\n");
    let mut interp = Interpreter::builder().suppress_output().build();
    let pages = interp.render(ps.as_bytes(), 72.0).expect("renders");
    pages[0].rgba.clone()
}

fn pixel(rgba: &[u8], x: usize, y: usize) -> [u8; 3] {
    let i = (y * 100 + x) * 4;
    [rgba[i], rgba[i + 1], rgba[i + 2]]
}

fn close(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter().zip(b).all(|(&p, q)| p.abs_diff(q) <= 3)
}

/// The Lab colour itself, filled on the left half; the reference.
fn lab_green() -> String {
    format!("{LAB} setcolorspace 52.2 -67 34 setcolor 0 0 50 100 rectfill")
}

#[test]
fn a_spot_colour_fill_is_drawn_in_its_lab_alternate() {
    let page = render(&format!(
        "{}\n{} setcolorspace 1 setcolor 50 0 50 100 rectfill",
        lab_green(),
        green_spot()
    ));
    let (lab, spot) = (pixel(&page, 25, 50), pixel(&page, 75, 50));
    assert!(lab[1] > lab[0] + 100, "the reference is green: {lab:?}");
    assert!(close(lab, spot), "spot {spot:?}, Lab {lab:?}");
}

#[test]
fn a_spot_colour_image_is_drawn_in_its_lab_alternate() {
    let page = render(&format!(
        "{}\n{} setcolorspace gsave 50 0 translate 50 100 scale \
         << /ImageType 1 /Width 1 /Height 1 /BitsPerComponent 8 /Decode [0 1] \
            /ImageMatrix [1 0 0 1 0 0] /DataSource <FF> >> image grestore",
        lab_green(),
        green_spot()
    ));
    let (lab, spot) = (pixel(&page, 25, 50), pixel(&page, 75, 50));
    assert!(close(lab, spot), "spot image {spot:?}, Lab {lab:?}");
}

#[test]
fn a_devicen_colour_is_drawn_in_its_lab_alternate() {
    let page = render(&format!(
        "{}\n[/DeviceN [/Green /Unused] {LAB} \
         {{ pop dup -47.8 mul 100 add exch dup -67 mul exch 34 mul }}] setcolorspace \
         1 0 setcolor 50 0 50 100 rectfill",
        lab_green()
    ));
    let (lab, spot) = (pixel(&page, 25, 50), pixel(&page, 75, 50));
    assert!(close(lab, spot), "DeviceN {spot:?}, Lab {lab:?}");
}

/// PDF output names the spot colour's alternate and samples its tint
/// transform; with a Lab alternate it named DeviceCMYK over Lab numbers.
#[cfg(feature = "pdf-output")]
#[test]
fn a_spot_colour_keeps_its_colour_in_pdf_output() {
    let body = format!(
        "{}\n{} setcolorspace 1 setcolor 50 0 50 100 rectfill",
        lab_green(),
        green_spot()
    );
    let ps = format!("%!PS\n<< /PageSize [100 100] >> setpagedevice\n{body}\nshowpage\n");
    let pdf = Interpreter::builder()
        .suppress_output()
        .build()
        .render_to_pdf(ps.as_bytes(), 72.0)
        .expect("writes");
    let doc = stet_pdf_reader::PdfDocument::from_bytes(&pdf).expect("opens");
    let (rgba, w, _) = doc.render_page_to_rgba(0, 72.0).expect("renders");
    assert_eq!(w, 100);
    let (lab, spot) = (pixel(&rgba, 25, 50), pixel(&rgba, 75, 50));
    assert!(lab[1] > lab[0] + 100, "the reference is green: {lab:?}");
    assert!(close(lab, spot), "spot in PDF {spot:?}, Lab {lab:?}");
}
