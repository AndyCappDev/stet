// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Each rendered page has its own size, as the job set it.
//!
//! The facade learns each page's size from the device it installs while
//! interpreting, which records it as the page is sent, and renders the
//! captured display list itself at that size.

use stet::Interpreter;

/// Width × height of each page, and the colour at its centre.
fn sizes(source: &str, dpi: f64) -> Vec<(u32, u32, [u8; 3])> {
    Interpreter::new()
        .render(source.as_bytes(), dpi)
        .unwrap()
        .into_iter()
        .map(|p| {
            let i = (((p.height / 2) * p.width + p.width / 2) * 4) as usize;
            (p.width, p.height, [p.rgba[i], p.rgba[i + 1], p.rgba[i + 2]])
        })
        .collect()
}

#[test]
fn pages_keep_the_size_they_were_set_to() {
    let job = "%!PS\n\
        << /PageSize [200 100] >> setpagedevice 1 0 0 setrgbcolor clippath fill showpage\n\
        << /PageSize [100 300] >> setpagedevice 0 0 1 setrgbcolor clippath fill showpage\n\
        0 1 0 setrgbcolor clippath fill showpage\n";
    assert_eq!(
        sizes(job, 72.0),
        [
            (200, 100, [255, 0, 0]),
            (100, 300, [0, 0, 255]),
            (100, 300, [0, 255, 0]),
        ]
    );
    assert_eq!(sizes(job, 144.0)[1], (200, 600, [0, 0, 255]));
}

#[test]
fn an_eps_page_is_its_bounding_box() {
    let eps = "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 10 20 130 70\n\
               1 0 0 setrgbcolor 10 20 120 50 rectfill\n";
    assert_eq!(sizes(eps, 72.0), [(120, 50, [255, 0, 0])]);
}
