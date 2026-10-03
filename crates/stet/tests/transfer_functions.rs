// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PostScript transfer functions (`settransfer`, `setcolortransfer`).
//!
//! A transfer function adjusts the device's colour components just before
//! output (PLRM 7.3), so on an RGB device it applies to the final RGB of
//! every paint: fills, strokes, images in every colour space, and
//! shadings. Each test paints under `{1 exch sub}` and checks against the
//! same paint with no function, which Ghostscript's `png16m` matches.

use stet::Interpreter;
use stet_pdf_reader::PdfDocument;

const INVERT: &str = "{1 exch sub} settransfer";

/// The page `body` paints on a 100×20 page, as RGB at user-space points.
struct Page {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl Page {
    fn from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Self {
        Page {
            width,
            height,
            rgba,
        }
    }

    fn render(body: &str) -> Self {
        let mut pages = Interpreter::new()
            .render(source(body).as_bytes(), 72.0)
            .unwrap();
        let p = pages.remove(0);
        Page::from_rgba(p.width, p.height, p.rgba)
    }

    /// The page written as PDF, then read and rendered by the PDF reader.
    fn through_pdf(body: &str) -> Self {
        let pdf = Interpreter::new()
            .render_to_pdf(source(body).as_bytes(), 72.0)
            .unwrap();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        let (rgba, width, height) = doc.render_page_to_rgba(0, 72.0).unwrap();
        Page::from_rgba(width, height, rgba)
    }

    /// RGB at a point given in user space (origin bottom-left).
    fn at(&self, x: u32, y: u32) -> [u8; 3] {
        let row = self.height - 1 - y;
        let i = ((row * self.width + x) * 4) as usize;
        [self.rgba[i], self.rgba[i + 1], self.rgba[i + 2]]
    }

    /// The left and right halves of the page.
    fn halves(&self) -> [[u8; 3]; 2] {
        [self.at(25, 10), self.at(75, 10)]
    }
}

fn source(body: &str) -> String {
    format!("%!PS-Adobe-3.0\n<< /PageSize [100 20] >> setpagedevice\n{body}\nshowpage\n")
}

fn inverted(rgb: [u8; 3]) -> [u8; 3] {
    rgb.map(|v| 255 - v)
}

/// `body` under the inverting function is the inverse of `body` alone, in
/// both halves of the page, to within `tolerance`.
fn assert_inverts(body: &str, tolerance: u8) {
    let plain = Page::render(body).halves();
    let under = Page::render(&format!("{INVERT}\n{body}")).halves();
    for (p, u) in plain.iter().zip(&under) {
        let want = inverted(*p);
        assert!(
            want.iter().zip(u).all(|(a, b)| a.abs_diff(*b) <= tolerance),
            "{body}: want {want:?}, got {u:?}"
        );
    }
}

#[test]
fn fills_take_the_function() {
    let page = Page::render(&format!(
        "{INVERT} 0.2 setgray 0 0 50 20 rectfill 0.2 0.2 0.2 setrgbcolor 50 0 50 20 rectfill"
    ));
    assert_eq!(page.halves(), [[204; 3], [204; 3]]);
    assert_inverts(
        "0 0 0 1 setcmykcolor 0 0 50 20 rectfill 0.3 0.6 0 0 setcmykcolor 50 0 50 20 rectfill",
        0,
    );
    assert_inverts(
        "[/Separation /Spot /DeviceCMYK {dup 0.5 mul exch 0 exch 0}] setcolorspace \
         1 setcolor 0 0 50 20 rectfill 0.3 setcolor 50 0 50 20 rectfill",
        0,
    );
}

#[test]
fn strokes_take_the_function() {
    assert_inverts(
        "0.2 setgray 12 setlinewidth 0 10 moveto 100 10 lineto stroke",
        0,
    );
}

#[test]
fn images_take_the_function_in_every_colour_space() {
    let image = |space: &str, data: &str, comps: u32| {
        format!(
            "{space} setcolorspace gsave 100 20 scale \
             << /ImageType 1 /Width 2 /Height 1 /BitsPerComponent 8 \
                /Decode [{}] /ImageMatrix [2 0 0 1 0 0] /DataSource <{data}> >> image \
             grestore",
            "0 1 ".repeat(comps as usize)
        )
    };
    assert_inverts(&image("/DeviceGray", "C837", 1), 0);
    assert_inverts(&image("/DeviceRGB", "C86414 3700FF", 3), 0);
    assert_inverts(&image("/DeviceCMYK", "00000000 1E3C00FF", 4), 0);
    assert_inverts(
        "[/Indexed /DeviceRGB 1 <FF0000 0050A0>] setcolorspace gsave 100 20 scale \
         << /ImageType 1 /Width 2 /Height 1 /BitsPerComponent 8 /Decode [0 255] \
            /ImageMatrix [2 0 0 1 0 0] /DataSource <0100> >> image grestore",
        0,
    );
    // An image mask paints the current colour in one half, and leaves the
    // paper, which no function touches, in the other.
    let mask = "0.2 setgray gsave 100 20 scale 2 1 true [2 0 0 1 0 0] <80> imagemask grestore";
    assert_eq!(Page::render(mask).halves(), [[51; 3], [255; 3]]);
    assert_eq!(
        Page::render(&format!("{INVERT} {mask}")).halves(),
        [[204; 3], [255; 3]]
    );
}

#[test]
fn shadings_take_the_function() {
    let axial = "<< /ShadingType 2 /ColorSpace /DeviceGray /Coords [0 0 100 0] \
                 /Function << /FunctionType 2 /Domain [0 1] /C0 [0.1] /C1 [0.9] /N 1 >> \
                 /Extend [true true] >> shfill";
    assert_inverts(axial, 1);
    let radial = "<< /ShadingType 3 /ColorSpace /DeviceRGB /Coords [50 10 0 50 10 60] \
                  /Function << /FunctionType 2 /Domain [0 1] /C0 [0.1 0.5 0.9] \
                  /C1 [0.9 0.2 0.1] /N 1 >> /Extend [true true] >> shfill";
    assert_inverts(radial, 1);
}

/// `setcolortransfer` maps red, green and blue each through its own
/// function.
#[test]
fn colour_transfer_applies_per_channel() {
    let page = Page::render(
        "{1 exch sub} {} {2 div} {pop 0} setcolortransfer \
         0.2 0.4 0.8 setrgbcolor 0 0 100 20 rectfill",
    );
    assert_eq!(page.at(50, 10), [204, 102, 102]);
}

#[test]
fn grestore_restores_the_function() {
    let page = Page::render(&format!(
        "gsave {INVERT} 0.2 setgray 0 0 50 20 rectfill grestore \
         0.2 setgray 50 0 50 20 rectfill"
    ));
    assert_eq!(page.halves(), [[204; 3], [51; 3]]);
}

/// A cached Type 3 glyph is a stencil painted with the colour in force at
/// each show, and so with the transfer function in force then.
#[test]
fn cached_type3_glyphs_take_the_current_function() {
    let page = Page::render(&format!(
        "8 dict begin /FontType 3 def /FontMatrix [0.05 0 0 0.05 0 0] def \
         /FontBBox [0 0 20 20] def /Encoding 256 array def \
         0 1 255 {{Encoding exch /.notdef put}} for Encoding 65 /box put \
         /BuildChar {{pop 20 0 0 0 20 20 setcachedevice 0 0 20 20 rectfill}} def \
         /Box currentdict end definefont pop \
         /Box 400 selectfont 0.2 setgray \
         0 0 moveto (A) show \
         gsave {INVERT} 50 0 moveto (A) show grestore"
    ));
    assert_eq!(page.halves(), [[51; 3], [204; 3]]);
}

/// PDF output carries the function as `/TR`, once, for every kind of paint,
/// and says when it returns to identity.
#[test]
fn pdf_output_preserves_the_function() {
    let body = format!(
        "gsave {INVERT} /DeviceGray setcolorspace gsave 50 20 scale \
         2 1 8 [2 0 0 1 0 0] <C837> image grestore grestore \
         gsave {INVERT} 0.2 setgray 50 0 25 20 rectfill grestore \
         0.2 setgray 75 0 25 20 rectfill"
    );
    let direct = Page::render(&body);
    let via_pdf = Page::through_pdf(&body);
    for x in [12, 37, 62, 87] {
        assert_eq!(direct.at(x, 10), via_pdf.at(x, 10), "at x = {x}");
    }
    assert_eq!(direct.at(12, 10), [55; 3]);
    assert_eq!(direct.at(62, 10), [204; 3]);
    assert_eq!(direct.at(87, 10), [51; 3]);

    let shading = format!(
        "{INVERT} << /ShadingType 2 /ColorSpace /DeviceGray /Coords [0 0 100 0] \
         /Function << /FunctionType 2 /Domain [0 1] /C0 [0.1] /C1 [0.9] /N 1 >> \
         /Extend [true true] >> shfill"
    );
    let direct = Page::render(&shading);
    let via_pdf = Page::through_pdf(&shading);
    for (d, p) in direct.halves().iter().zip(via_pdf.halves()) {
        assert!(d[0].abs_diff(p[0]) <= 2, "shading via PDF: {d:?} vs {p:?}");
    }
}

/// Every page the job sends, rendered, with `background`.
fn pages(body: &str, background: stet::PageBackground) -> Vec<Page> {
    Interpreter::builder()
        .page_background(background)
        .build()
        .render(source(body).as_bytes(), 72.0)
        .unwrap()
        .into_iter()
        .map(|p| Page::from_rgba(p.width, p.height, p.rgba))
        .collect()
}

/// `erasepage` paints the page "with gray level 1.0 (which is ordinarily
/// white, but may be some other color if an atypical transfer function has
/// been defined)" (PLRM 3e), as Ghostscript does.
#[test]
fn erasepage_paints_white_through_the_function() {
    let page = Page::render(&format!(
        "{INVERT} 0.5 setgray 0 0 100 20 rectfill erasepage 0.2 setgray 0 0 50 20 rectfill"
    ));
    assert_eq!(page.halves(), [[204; 3], [0; 3]]);
}

/// `showpage` erases the page for the next under the function in force, so
/// a job that inverts its output once gets black paper from page 2 on —
/// page 1 was erased before the function was set. A page with no marks is
/// sent black too.
#[test]
fn showpage_erases_through_the_function() {
    let body = format!(
        "{INVERT} 0.2 setgray 0 0 50 20 rectfill showpage \
         0.2 setgray 0 0 50 20 rectfill showpage"
    ); // and `source`'s own showpage, with no marks
    let sent = pages(&body, stet::PageBackground::White);
    let halves: Vec<_> = sent.iter().map(Page::halves).collect();
    assert_eq!(
        halves,
        [[[204; 3], [255; 3]], [[204; 3], [0; 3]], [[0; 3], [0; 3]],]
    );
    // The colour is paint, not paper: a transparent page keeps it.
    let clear = pages(&body, stet::PageBackground::Transparent);
    let i = ((10 * clear[1].width + 75) * 4) as usize;
    assert_eq!(&clear[1].rgba[i..i + 4], [0, 0, 0, 255]);
}

/// A function that maps white elsewhere adds the erase colour after the
/// erase; one that leaves white white adds nothing. And the erase colour is
/// no mark: a job that ends on a `showpage` under an inverting function has
/// not dropped a page.
#[test]
fn the_erase_colour_is_not_a_mark() {
    let mut interp = Interpreter::new();
    let lists = interp
        .render_to_display_list(
            source("{0.5 mul} settransfer erasepage 0 0 10 10 rectfill").as_bytes(),
            72.0,
        )
        .unwrap();
    assert_eq!(
        lists[0].display_list.len(),
        3,
        "erase, its colour, the fill"
    );
    let lists = interp
        .render_to_display_list(
            source("{} settransfer erasepage 0 0 10 10 rectfill").as_bytes(),
            72.0,
        )
        .unwrap();
    assert_eq!(lists[0].display_list.len(), 2, "erase, the fill");

    let body = format!("%!PS\n<< /PageSize [100 20] >> setpagedevice {INVERT} showpage\n");
    interp.render(body.as_bytes(), 72.0).unwrap();
    assert!(interp.warnings().is_empty(), "{:?}", interp.warnings());
}

/// PDF output carries the erase colour as a page-filling rectangle.
#[test]
fn pdf_output_carries_the_erase_colour() {
    let body = format!("{INVERT} showpage 0.2 setgray 0 0 50 20 rectfill");
    let pdf = Interpreter::new()
        .render_to_pdf(source(&body).as_bytes(), 72.0)
        .unwrap();
    let doc = PdfDocument::from_bytes(&pdf).unwrap();
    assert_eq!(doc.page_count(), 2);
    let (rgba, width, height) = doc.render_page_to_rgba(1, 72.0).unwrap();
    let page = Page::from_rgba(width, height, rgba);
    assert_eq!(page.halves(), [[204; 3], [0; 3]]);
}

/// `setpagedevice` reinitializes the graphics state, transfer function
/// included, before it erases (PLRM 3e), so its erase is white even after
/// `showpage` erased through an inverting function.
#[test]
fn setpagedevice_erases_white() {
    let sent = pages(
        &format!(
            "{INVERT} showpage << /PageSize [100 20] >> setpagedevice \
             0.2 setgray 0 0 50 20 rectfill"
        ),
        stet::PageBackground::White,
    );
    assert_eq!(sent[1].halves(), [[51; 3], [255; 3]]);
}
