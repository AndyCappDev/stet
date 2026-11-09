// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! An Indexed colour is the colour its palette entry gives in the base
//! space, whatever the base is (PLRM 3e §4.8.4).
//!
//! Only a DeviceGray, DeviceRGB or DeviceCMYK base was looked up. Over a
//! CIE-based, ICCBased, Separation or DeviceN base a fill came out black,
//! an image drew the palette's bytes as RGB, and a mesh shading drew the
//! index as a gray. A lookup procedure's results were clipped to 0-1, so a
//! Lab palette, which the PLRM says to give as a procedure, lost its L*,
//! a* and b*.

#![cfg(feature = "render")]

use stet::Interpreter;
use stet_graphics::display_list::DisplayElement;

/// A CIEBasedABC space set up as Lab (D50).
const LAB: &str = "[/CIEBasedABC << /RangeABC [0 100 -128 127 -128 127] \
    /DecodeABC [{16 add 116 div} bind {500 div} bind {200 div} bind] \
    /MatrixABC [1 1 1 1 0 0 0 0 -1] \
    /DecodeLMN [{dup 6 29 div ge {dup dup mul mul} {4 29 div sub 108 841 div mul} ifelse 0.9642 mul} bind \
                {dup 6 29 div ge {dup dup mul mul} {4 29 div sub 108 841 div mul} ifelse} bind \
                {dup 6 29 div ge {dup dup mul mul} {4 29 div sub 108 841 div mul} ifelse 0.8249 mul} bind] \
    /WhitePoint [0.9642 1 0.8249] >>]";

const SEP: &str = "[/Separation /Spot /DeviceCMYK \
    {dup 0.1 mul exch dup 0.8 mul exch dup 0.9 mul exch 0 mul}]";

const DEVN: &str = "[/DeviceN [/A /B] /DeviceCMYK {0 0 4 2 roll 0 exch}]";

/// The CMYK profile stet embeds, as an ICCBased space.
fn icc_cmyk() -> String {
    let profile = include_bytes!("../resources/default_cmyk.icc");
    let hex: String = profile.iter().map(|b| format!("{b:02x}")).collect();
    format!("[/ICCBased << /N 4 /DataSource <{hex}> >>]")
}

/// The page, 100 points square at 72 dpi, as RGBA rows.
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

/// The left half filled with `reference` in its own space, the right half
/// with `indexed`; the two pixels.
fn halves(reference: &str, indexed: &str) -> ([u8; 3], [u8; 3]) {
    let page = render(&format!(
        "{reference} 0 0 50 100 rectfill\n{indexed} 50 0 50 100 rectfill"
    ));
    (pixel(&page, 25, 50), pixel(&page, 75, 50))
}

fn assert_close(reference: [u8; 3], got: [u8; 3]) {
    assert!(
        reference.iter().zip(got).all(|(&a, b)| a.abs_diff(b) <= 3),
        "indexed {got:?}, reference {reference:?}"
    );
}

/// A two-entry Lab palette given as a procedure: green, then blue.
fn lab_proc_palette() -> String {
    format!("[/Indexed {LAB} 1 {{0 eq {{52.2 -67 34}} {{40 20 -60}} ifelse}}]")
}

#[test]
fn a_lab_palette_from_a_procedure_keeps_its_values() {
    let (lab, indexed) = halves(
        &format!("{LAB} setcolorspace 52.2 -67 34 setcolor"),
        &format!("{} setcolorspace 0 setcolor", lab_proc_palette()),
    );
    assert!(lab[1] > lab[0] + 100, "the reference is green: {lab:?}");
    assert_close(lab, indexed);
}

#[test]
fn a_lookup_string_is_divided_by_255_over_any_base() {
    // PLRM 3e §4.8.4: the bytes become 0-1 whatever the base's ranges, so
    // FF 80 80 is A, B, C = 1, 0.502, 0.502 in the Lab space, not L* 100.
    let (lab, indexed) = halves(
        &format!("{LAB} setcolorspace 1 128 255 div 128 255 div setcolor"),
        &format!("[/Indexed {LAB} 0 <FF8080>] setcolorspace 0 setcolor"),
    );
    assert_close(lab, indexed);
}

#[test]
fn an_icc_base_goes_through_its_profile() {
    let icc = icc_cmyk();
    let (direct, indexed) = halves(
        &format!("{icc} setcolorspace 0 1 0 0 setcolor"),
        &format!("[/Indexed {icc} 1 <00000000 00FF0000> ] setcolorspace 1 setcolor"),
    );
    assert!(direct[0] > direct[1] + 100, "magenta: {direct:?}");
    assert_close(direct, indexed);
}

#[test]
fn a_separation_base_runs_the_tint_transform() {
    let (direct, indexed) = halves(
        &format!("{SEP} setcolorspace 0.5 setcolor"),
        &format!("[/Indexed {SEP} 1 <00 80>] setcolorspace 1 setcolor"),
    );
    assert!(direct != [0, 0, 0], "{direct:?}");
    assert_close(direct, indexed);
}

#[test]
fn a_devicen_base_runs_the_tint_transform() {
    let (direct, indexed) = halves(
        &format!("{DEVN} setcolorspace 1 0 setcolor"),
        &format!("[/Indexed {DEVN} 1 <0000 FF00>] setcolorspace 1 setcolor"),
    );
    assert!(direct != [0, 0, 0], "{direct:?}");
    assert_close(direct, indexed);
}

#[test]
fn the_initial_colour_is_entry_0() {
    let (lab, indexed) = halves(
        &format!("{LAB} setcolorspace 52.2 -67 34 setcolor"),
        &format!("{} setcolorspace", lab_proc_palette()),
    );
    assert_close(lab, indexed);
}

#[test]
fn currentcolor_is_still_the_index() {
    let ps = format!(
        "%!PS\n{} setcolorspace 1.4 setcolor currentcolor \
         dup type /integertype ne {{ /currentcolor-not-integer cvx exec }} if \
         1 ne {{ /currentcolor-not-1 cvx exec }} if\n",
        lab_proc_palette()
    );
    let mut interp = Interpreter::builder().suppress_output().build();
    interp.render(ps.as_bytes(), 72.0).expect("runs");
}

/// A 2x1 image of indices 0 and 1 over the right half.
fn index_image() -> &'static str {
    "gsave 50 0 translate 50 100 scale \
     << /ImageType 1 /Width 2 /Height 1 /BitsPerComponent 8 /Decode [0 255] \
        /ImageMatrix [2 0 0 1 0 0] /DataSource <0001> >> image grestore"
}

#[test]
fn an_image_over_a_lab_palette_is_drawn_through_it() {
    let page = render(&format!(
        "{LAB} setcolorspace 52.2 -67 34 setcolor 0 0 50 100 rectfill\n\
         {} setcolorspace {}",
        lab_proc_palette(),
        index_image()
    ));
    assert_close(pixel(&page, 25, 50), pixel(&page, 62, 50));
}

#[test]
fn an_image_over_a_separation_palette_is_drawn_through_it() {
    let page = render(&format!(
        "{SEP} setcolorspace 0.5 setcolor 0 0 50 100 rectfill\n\
         [/Indexed {SEP} 1 <80 FF>] setcolorspace {}",
        index_image()
    ));
    assert_close(pixel(&page, 25, 50), pixel(&page, 62, 50));
}

#[test]
fn a_mesh_in_an_indexed_space_takes_its_palette() {
    let ps = "%!PS\n<< /ShadingType 4 /ColorSpace [/Indexed /DeviceRGB 1 <FF0000 0000FF>] \
              /DataSource [0 0 0 0  0 100 0 0  0 0 100 1] >> shfill showpage\n";
    let mut interp = Interpreter::builder().suppress_output().build();
    let pages = interp
        .render_to_display_list(ps.as_bytes(), 72.0)
        .expect("renders");
    let tri = pages[0]
        .display_list
        .elements()
        .iter()
        .find_map(|e| match e {
            DisplayElement::MeshShading { params } => Some(params.triangles[0].clone()),
            _ => None,
        })
        .expect("a mesh");
    let rgb = |c: &stet_graphics::color::DeviceColor| [c.r, c.g, c.b];
    assert_eq!(rgb(&tri.v0.color), [1.0, 0.0, 0.0]);
    assert_eq!(rgb(&tri.v2.color), [0.0, 0.0, 1.0]);
}

/// PDF output keeps a spot palette in its Separation space, and draws it
/// as the PostScript does.
#[cfg(feature = "pdf-output")]
#[test]
fn a_separation_palette_stays_a_separation_in_pdf_output() {
    let body = format!(
        "{SEP} setcolorspace 0.5 setcolor 0 0 50 100 rectfill\n\
         [/Indexed {SEP} 1 <80 FF>] setcolorspace {}",
        index_image()
    );
    let ps = format!("%!PS\n<< /PageSize [100 100] >> setpagedevice\n{body}\nshowpage\n");
    let pdf = Interpreter::builder()
        .suppress_output()
        .build()
        .render_to_pdf(ps.as_bytes(), 72.0)
        .expect("writes");
    let text = String::from_utf8_lossy(&pdf);
    assert!(
        text.contains("/Indexed [/Separation /Spot"),
        "no spot palette"
    );
    let doc = stet_pdf_reader::PdfDocument::from_bytes(&pdf).expect("opens");
    let (rgba, _, _) = doc.render_page_to_rgba(0, 72.0).expect("renders");
    assert_close(pixel(&rgba, 25, 50), pixel(&rgba, 62, 50));
}
