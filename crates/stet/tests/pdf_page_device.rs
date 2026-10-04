// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF output keeps every page of a job that calls `setpagedevice` more
//! than once.
//!
//! Every `setpagedevice` replaced the device with a new one from the device
//! factory. A raster device has written its pages by then; the PDF device
//! holds them all until the end of the job, so each call discarded every
//! page before it. Ghostscript's `ps2write` calls `setpagedevice` before
//! every page, and a document whose page sizes change calls it between
//! them: only the pages after the last call reached the PDF.
#![cfg(feature = "pdf-output")]

use stet::{DisplayElement, Interpreter, PsDisplayList, TextExtraction};

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
    // To the device pixel at 300 dpi: exact page sizes are a separate fix.
    assert_sizes(
        &got,
        &[(612.0, 792.0), (595.0, 842.0), (300.0, 200.0)],
        0.24,
    );
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
