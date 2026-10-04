// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF output keeps every page of a job that calls `setpagedevice` more
//! than once, each at its exact size.
//!
//! Every `setpagedevice` replaced the device with a new one from the device
//! factory. A raster device has written its pages by then; the PDF device
//! holds them all until the end of the job, so each call discarded every
//! page before it. Ghostscript's `ps2write` calls `setpagedevice` before
//! every page, and a document whose page sizes change calls it between
//! them: only the pages after the last call reached the PDF.
//!
//! And the MediaBox was the page rounded to the device's pixels: A4 at the
//! command line's 300 dpi came out 594.96 × 841.92.
#![cfg(feature = "pdf-output")]

use stet::{DisplayElement, Interpreter, PsDisplayList, TextExtraction};
use stet_fonts::geometry::PathSegment;

/// The text of each run, at every depth.
fn texts(list: &PsDisplayList, out: &mut Vec<String>) {
    for e in list.elements() {
        match e {
            DisplayElement::TextRun { params } => out.push(params.text.clone()),
            DisplayElement::Group { elements, .. } | DisplayElement::OcgGroup { elements, .. } => {
                texts(elements, out)
            }
            _ => {}
        }
    }
}

/// Each page of the job's PDF: its MediaBox width and height, and its text.
fn pages(job: &str) -> Vec<((f64, f64), Vec<String>)> {
    let pdf = Interpreter::new()
        .render_to_pdf(job.as_bytes(), 300.0)
        .unwrap();
    let mut doc = stet_pdf_reader::PdfDocument::from_bytes(&pdf).unwrap();
    doc.set_text_extraction(TextExtraction::Runs);
    (0..doc.page_count())
        .map(|i| {
            let [llx, lly, urx, ury] = doc.page_boxes(i).unwrap().media_box;
            let mut t = Vec::new();
            texts(&doc.render_page(i, 72.0).unwrap(), &mut t);
            ((urx - llx, ury - lly), t)
        })
        .collect()
}

/// Each page's MediaBox is within `tolerance` points of `want`.
fn assert_sizes(got: &[((f64, f64), Vec<String>)], want: &[(f64, f64)], tolerance: f64) {
    assert_eq!(got.len(), want.len());
    for (((w, h), _), (ww, wh)) in got.iter().zip(want) {
        assert!(
            (w - ww).abs() <= tolerance && (h - wh).abs() <= tolerance,
            "page {w} × {h}, want {ww} × {wh}"
        );
    }
}

const FONT: &str = "/Times-Roman findfont 20 scalefont setfont\n";

/// Letter, then A4, then a small page: three pages, each its own size.
#[test]
fn a_change_of_page_size_keeps_the_pages_before_it() {
    let job = format!(
        "%!PS\n{FONT}72 72 moveto (one) show showpage\n\
         << /PageSize [595 842] >> setpagedevice 72 72 moveto (two) show showpage\n\
         << /PageSize [300 200] >> setpagedevice 72 72 moveto (three) show showpage\n"
    );
    let got = pages(&job);
    let text: Vec<_> = got.iter().map(|(_, t)| t.clone()).collect();
    assert_eq!(text, [["one"], ["two"], ["three"]]);
    assert_sizes(&got, &[(612.0, 792.0), (595.0, 842.0), (300.0, 200.0)], 0.0);
}

/// `setpagedevice` with the page size the job already has, before every
/// page, as Ghostscript's `ps2write` writes it.
#[test]
fn setpagedevice_before_every_page_keeps_them_all() {
    let job = format!(
        "%!PS\n{FONT}\
         << /PageSize [612 792] >> setpagedevice 72 72 moveto (one) show showpage\n\
         << /PageSize [612 792] >> setpagedevice 72 72 moveto (two) show showpage\n\
         << /PageSize [612 792] >> setpagedevice 72 72 moveto (three) show showpage\n"
    );
    let got = pages(&job);
    let text: Vec<_> = got.iter().map(|(_, t)| t.clone()).collect();
    assert_eq!(text, [["one"], ["two"], ["three"]]);
    assert_sizes(&got, &[(612.0, 792.0); 3], 0.0);
}

/// One `Interpreter`, two jobs: the second job's PDF has only its own
/// pages, though its device is no longer replaced by `setpagedevice`.
#[test]
fn each_job_gets_its_own_document() {
    let mut interp = Interpreter::new();
    let first = format!("%!PS\n{FONT}72 72 moveto (first) show showpage\n");
    let second = format!(
        "%!PS\n{FONT}<< /PageSize [612 792] >> setpagedevice 72 72 moveto (second) show showpage\n"
    );
    interp.render_to_pdf(first.as_bytes(), 300.0).unwrap();
    let pdf = interp.render_to_pdf(second.as_bytes(), 300.0).unwrap();
    let doc = stet_pdf_reader::PdfDocument::from_bytes(&pdf).unwrap();
    assert_eq!(doc.page_count(), 1);
}

/// Each fill's bounding box, at the top level.
fn boxes(list: &PsDisplayList) -> Vec<[f64; 4]> {
    list.elements()
        .iter()
        .filter_map(|e| match e {
            DisplayElement::Fill { path, .. } => {
                let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
                for s in &path.segments {
                    let pts: &[(f64, f64)] = match s {
                        PathSegment::MoveTo(x, y) | PathSegment::LineTo(x, y) => &[(*x, *y)],
                        PathSegment::CurveTo { x3, y3, .. } => &[(*x3, *y3)],
                        PathSegment::ClosePath => &[],
                    };
                    for &(x, y) in pts {
                        b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
                    }
                }
                Some(b)
            }
            _ => None,
        })
        .collect()
}

/// Page sizes that are not whole pixels at 300 dpi are the MediaBox
/// exactly, and the glyphs sit where the PostScript puts them — near the
/// bottom-left corner and the top-right one, which a mismatch between the
/// box and the content's placement would move apart.
#[test]
fn the_media_box_is_the_page_size_and_the_content_stays_put() {
    for (w, h) in [(595.0, 842.0), (300.0, 200.0), (595.276, 841.89)] {
        let job = format!(
            "%!PS\n<< /PageSize [{w} {h}] >> setpagedevice {FONT}\
             10 10 moveto (low) show {x} {y} moveto (high) show showpage\n",
            x = w - 60.0,
            y = h - 25.0
        );
        let pdf = Interpreter::new()
            .render_to_pdf(job.as_bytes(), 300.0)
            .unwrap();
        let doc = stet_pdf_reader::PdfDocument::from_bytes(&pdf).unwrap();
        assert_eq!(doc.page_boxes(0).unwrap().media_box, [0.0, 0.0, w, h]);
        // At 72 dpi the PDF's device space is its page, `h` points tall from
        // the top; the PostScript's is `h.round()` pixels. Measured from the
        // bottom, where both put user space's origin, they agree.
        let from_pdf = boxes(&doc.render_page(0, 72.0).unwrap());
        let ps = Interpreter::new()
            .render_to_display_list(job.as_bytes(), 72.0)
            .unwrap();
        let from_ps = boxes(&ps[0].display_list);
        assert_eq!(from_pdf.len(), from_ps.len());
        let dy = h - h.round();
        for (pdf, ps) in from_pdf.iter().zip(&from_ps) {
            let shifted = [ps[0], ps[1] + dy, ps[2], ps[3] + dy];
            assert!(
                pdf.iter().zip(shifted).all(|(a, b)| (a - b).abs() < 0.02),
                "{w} × {h}: PDF {pdf:?}, PS {shifted:?}"
            );
        }
    }
}

/// An EPS's page is its `%%HiResBoundingBox`.
#[test]
fn an_eps_page_is_its_high_resolution_bounding_box() {
    let eps = "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 557 557\n\
               %%HiResBoundingBox: 0 0 556.4937 556.4942\n\
               0 0 moveto 556 556 lineto stroke\n";
    let pdf = Interpreter::new()
        .render_to_pdf(eps.as_bytes(), 300.0)
        .unwrap();
    let doc = stet_pdf_reader::PdfDocument::from_bytes(&pdf).unwrap();
    assert_eq!(
        doc.page_boxes(0).unwrap().media_box,
        [0.0, 0.0, 556.4937, 556.4942]
    );
}

/// `gsave nulldevice … grestore` between pages: both pages reach the PDF —
/// the null device replaced the PDF device for the rest of the job, so
/// nothing did — and what was drawn on the null device, a string measured
/// with `show` as programs do, is not among them.
#[test]
fn the_null_device_neither_ends_the_document_nor_adds_to_it() {
    let job = format!(
        "%!PS\n{FONT}72 72 moveto (one) show showpage\n\
         gsave nulldevice 72 72 moveto (hidden) show grestore\n\
         72 72 moveto (two) show showpage\n"
    );
    let got = pages(&job);
    let text: Vec<_> = got.iter().map(|(_, t)| t.clone()).collect();
    assert_eq!(text, [["one"], ["two"]]);
}
