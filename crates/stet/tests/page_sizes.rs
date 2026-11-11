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

/// Pages are clamped to 14400 pt a side by default; a host may allow larger ones, as an Illustrator
/// artboard of 204 inches declares.
#[test]
fn the_page_size_limit_is_the_hosts_to_raise() {
    let eps = "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 14688 20\n\
               1 0 0 setrgbcolor 0 0 14688 20 rectfill\n";
    let width = |interp: &mut Interpreter| {
        interp.render_to_display_list(eps.as_bytes(), 72.0).unwrap()[0].width
    };
    assert_eq!(width(&mut Interpreter::new()), 14400);
    assert_eq!(
        width(&mut Interpreter::builder().max_page_size(16383.0).build()),
        14688
    );
}

/// `f64::INFINITY` lifts the limit.
#[test]
fn an_infinite_page_size_limit_lifts_it() {
    let ps = "%!PS\n<< /PageSize [20000 30] >> setpagedevice\n\
              1 0 0 setrgbcolor 0 0 20000 30 rectfill showpage\n";
    let mut interp = Interpreter::builder().max_page_size(f64::INFINITY).build();
    let pages = interp.render_to_display_list(ps.as_bytes(), 72.0).unwrap();
    assert_eq!(pages[0].width, 20000);
}

/// `max_page_size` refuses a limit that is not greater than zero.
#[test]
#[should_panic(expected = "greater than zero")]
fn a_page_size_limit_of_zero_is_refused() {
    let _ = Interpreter::builder().max_page_size(0.0);
}

/// `max_page_size` refuses NaN, which `f64::min` would treat as no limit.
#[test]
#[should_panic(expected = "greater than zero")]
fn a_nan_page_size_limit_is_refused() {
    let _ = Interpreter::builder().max_page_size(f64::NAN);
}

/// A limit set on the context directly that is not greater than zero counts as the default,
/// rather than giving 0x0 pages or none at all.
#[test]
fn a_page_size_limit_on_the_context_that_is_not_positive_is_the_default() {
    let ps = "%!PS\n<< /PageSize [20000 30] >> setpagedevice\n\
              1 0 0 setrgbcolor 0 0 20000 30 rectfill showpage\n";
    for limit in [0.0, -100.0, f64::NAN] {
        let mut interp = Interpreter::new();
        interp.context().max_page_size_points = limit;
        let pages = interp.render_to_display_list(ps.as_bytes(), 72.0).unwrap();
        assert_eq!(
            (pages[0].width, pages[0].height),
            (14400, 30),
            "limit {limit}"
        );
    }
}
