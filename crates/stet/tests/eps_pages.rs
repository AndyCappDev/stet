// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! An EPS renders as one page, whether or not it calls `showpage` itself.
//!
//! The facade adds a `showpage` when the EPS did not. For rendering it used
//! to decide by asking the device's page sink, which the device reaches only
//! once its background render finishes, so an EPS that did call `showpage`
//! could get a second, blank page — always, once the page was large enough
//! to band. PDF output added the `showpage` unconditionally.

use stet::Interpreter;

fn eps(size: u32, showpage: bool) -> String {
    format!(
        "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 {size} {size}\n\
         1 0 0 setrgbcolor 0 0 50 50 rectfill {}\n",
        if showpage { "showpage" } else { "" }
    )
}

#[test]
fn an_eps_renders_as_one_page() {
    // A small page, and one large enough to band at 300 dpi.
    for size in [100, 600] {
        for showpage in [true, false] {
            let pages = Interpreter::new()
                .render(eps(size, showpage).as_bytes(), 300.0)
                .unwrap();
            assert_eq!(pages.len(), 1, "{size} pt, showpage: {showpage}");
            let p = &pages[0];
            // The red square at the bottom left, 50 pt = 208 px at 300 dpi.
            let i = (((p.height - 10) * p.width + 10) * 4) as usize;
            assert_eq!(&p.rgba[i..i + 3], [255, 0, 0], "{size} pt");
        }
    }
}

/// An EPS is not meant to call `setpagedevice`, and some do. That puts it
/// on a page device of its own, which the `grestore` closing the wrapper
/// around it deactivates, with whatever was painted on it. The page was
/// ended after that `grestore`, and came out blank.
#[test]
fn an_eps_that_sets_its_page_device_still_shows_its_page() {
    for origin in [0, 10] {
        for showpage in [true, false] {
            let eps = format!(
                "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: {origin} {origin} {0} {0}\n\
                 << /PageSize [200 200] >> setpagedevice\n\
                 1 0 0 setrgbcolor {origin} {origin} 50 50 rectfill {1}\n",
                origin + 200,
                if showpage { "showpage" } else { "" }
            );
            let pages = Interpreter::new().render(eps.as_bytes(), 72.0).unwrap();
            assert_eq!(pages.len(), 1, "origin {origin}, showpage: {showpage}");
            let red = pages[0]
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|p| p[..3] == [255, 0, 0])
                .count();
            assert_eq!(red, 2500, "origin {origin}, showpage: {showpage}");
        }
    }
}

#[test]
fn an_eps_becomes_a_one_page_pdf() {
    for showpage in [true, false] {
        let pdf = Interpreter::new()
            .render_to_pdf(eps(100, showpage).as_bytes(), 72.0)
            .unwrap();
        let doc = stet_pdf_reader::PdfDocument::from_bytes(&pdf).unwrap();
        assert_eq!(doc.page_count(), 1, "showpage: {showpage}");
    }
}
