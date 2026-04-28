// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! End-to-end tests for the `pdfmark` operator: PostScript code that
//! issues `/DOCINFO pdfmark` calls is rendered through the stet facade,
//! and the resulting PDF bytes are scanned for the expected `/Info`
//! entries.

#![cfg(feature = "pdf-output")]

use stet::Interpreter;

/// Locate the PDF body byte offset of the trailer's `/Info NN 0 R`
/// reference, then read the referenced indirect object out of the file.
fn find_info_dict_bytes(pdf: &[u8]) -> Vec<u8> {
    let trailer_idx = pdf
        .windows(7)
        .rposition(|w| w == b"trailer")
        .expect("trailer marker present");
    let trailer = &pdf[trailer_idx..];
    let info_marker = b"/Info ";
    let m = trailer
        .windows(info_marker.len())
        .position(|w| w == info_marker)
        .expect("/Info present in trailer");
    let after = &trailer[m + info_marker.len()..];
    let after_str = std::str::from_utf8(&after[..16.min(after.len())]).unwrap();
    let space = after_str.find(' ').unwrap();
    let info_obj_num: u32 = after_str[..space].parse().unwrap();

    let header = format!("{} 0 obj", info_obj_num);
    let start = pdf
        .windows(header.len())
        .position(|w| w == header.as_bytes())
        .expect("info indirect object present");
    let body_start = start + header.len();
    let end = pdf[body_start..]
        .windows(7)
        .position(|w| w == b"endobj\n" || w == b"endobj ")
        .expect("endobj marker present");
    pdf[body_start..body_start + end].to_vec()
}

fn render_one_page_pdf(prelude: &str) -> Vec<u8> {
    let mut interp = Interpreter::new();
    let mut script = String::new();
    script.push_str(prelude);
    script.push_str("\nshowpage\n");
    interp
        .render_to_pdf(script.as_bytes(), 72.0)
        .expect("render_to_pdf succeeds")
}

#[test]
fn docinfo_writes_title_and_author() {
    let pdf = render_one_page_pdf(
        "[ /Title (Hello World) /Author (Scott) /Subject (Phase 1) /DOCINFO pdfmark",
    );
    let info = find_info_dict_bytes(&pdf);
    let info_str = String::from_utf8_lossy(&info);
    assert!(
        info_str.contains("/Title (Hello World)"),
        "missing /Title in info dict: {info_str}",
    );
    assert!(
        info_str.contains("/Author (Scott)"),
        "missing /Author in info dict: {info_str}",
    );
    assert!(
        info_str.contains("/Subject (Phase 1)"),
        "missing /Subject in info dict: {info_str}",
    );
}

#[test]
fn docinfo_overrides_default_producer() {
    let pdf = render_one_page_pdf("[ /Producer (custom-pipeline 1.0) /DOCINFO pdfmark");
    let info = find_info_dict_bytes(&pdf);
    let info_str = String::from_utf8_lossy(&info);
    assert!(
        info_str.contains("/Producer (custom-pipeline 1.0)"),
        "expected pdfmark producer to win, got: {info_str}",
    );
    assert!(
        !info_str.contains("/Producer (stet)"),
        "default producer should have been overridden: {info_str}",
    );
}

#[test]
fn docinfo_creation_date_passthrough() {
    let pdf = render_one_page_pdf("[ /CreationDate (D:20261231120000Z) /DOCINFO pdfmark");
    let info = find_info_dict_bytes(&pdf);
    let info_str = String::from_utf8_lossy(&info);
    assert!(
        info_str.contains("/CreationDate (D:20261231120000Z)"),
        "expected pdfmark creation-date to round-trip, got: {info_str}",
    );
}

#[test]
fn docinfo_trapped_writes_name() {
    let pdf = render_one_page_pdf("[ /Trapped /True /DOCINFO pdfmark");
    let info = find_info_dict_bytes(&pdf);
    let info_str = String::from_utf8_lossy(&info);
    assert!(
        info_str.contains("/Trapped /True"),
        "expected /Trapped /True in info dict: {info_str}",
    );
}

#[test]
fn no_docinfo_keeps_default_producer() {
    let pdf = render_one_page_pdf("");
    let info = find_info_dict_bytes(&pdf);
    let info_str = String::from_utf8_lossy(&info);
    assert!(
        info_str.contains("/Producer (stet)"),
        "expected default Producer when no pdfmark issued: {info_str}",
    );
    // No pdfmark → no Author / Subject / Keywords
    assert!(!info_str.contains("/Author"));
    assert!(!info_str.contains("/Subject"));
    assert!(!info_str.contains("/Keywords"));
}

#[test]
fn unknown_typetag_is_silent() {
    let pdf = render_one_page_pdf("[ /Foo (bar) /SOMENEWTHING pdfmark");
    let info = find_info_dict_bytes(&pdf);
    let info_str = String::from_utf8_lossy(&info);
    // Default info dict is built without crashing; no leak from unknown
    // type-tag.
    assert!(info_str.contains("/Producer (stet)"));
    assert!(!info_str.contains("/Foo"));
}
