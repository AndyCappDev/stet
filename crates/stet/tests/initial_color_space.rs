// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A job starts in `DeviceGray`, whatever the output device.
//!
//! Each device's `Install` procedure set `DeviceRGB`, so the first page of
//! a job, and the first page after every `setpagedevice`, began in a colour
//! space no program expects: `0.5 setcolor` with nothing set before it
//! raised `stackunderflow`. `showpage` put `DeviceGray` back, which hid it
//! on every later page.

use stet::Interpreter;

/// Fills the page with `0.5 setcolor`, which is one number in `DeviceGray`
/// and two short in `DeviceRGB`.
const HALF_GRAY: &str = "0.5 setcolor 0 0 100 100 rectfill showpage\n";

#[cfg(feature = "render")]
fn each_page_at_40_40(job: &str) -> Vec<[u8; 3]> {
    Interpreter::new()
        .render(job.as_bytes(), 72.0)
        .unwrap()
        .iter()
        .map(|page| {
            let i = (((page.height - 41) * page.width + 40) * 4) as usize;
            [page.rgba[i], page.rgba[i + 1], page.rgba[i + 2]]
        })
        .collect()
}

#[cfg(feature = "render")]
fn assert_half_gray(pages: &[[u8; 3]], count: usize) {
    assert_eq!(pages.len(), count);
    for [r, g, b] in pages {
        assert!(r == g && g == b && (120..=136).contains(r), "{r} {g} {b}");
    }
}

#[cfg(feature = "render")]
#[test]
fn the_first_page_starts_in_gray() {
    assert_half_gray(&each_page_at_40_40(&format!("%!PS\n{HALF_GRAY}")), 1);
}

#[cfg(feature = "render")]
#[test]
fn the_page_after_setpagedevice_starts_in_gray() {
    let job = format!(
        "%!PS\n<< /PageSize [100 100] >> setpagedevice {HALF_GRAY}\
         1 0 0 setrgbcolor << /PageSize [80 80] >> setpagedevice {HALF_GRAY}"
    );
    assert_half_gray(&each_page_at_40_40(&job), 2);
}

#[cfg(feature = "pdf-output")]
#[test]
fn pdf_output_starts_in_gray() {
    let job = format!("%!PS\n<< /PageSize [100 100] >> setpagedevice {HALF_GRAY}");
    let pdf = Interpreter::new()
        .render_to_pdf(job.as_bytes(), 72.0)
        .unwrap();
    let doc = stet_pdf_reader::PdfDocument::from_bytes(&pdf).unwrap();
    assert_eq!(doc.page_count(), 1);
}
