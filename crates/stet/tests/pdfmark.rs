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

/// Locate every PDF indirect object body in `pdf` whose dict contains
/// the substring `marker`. Returns each matching object body as a
/// borrowed slice of `pdf`. Used to assert on outline-tree contents
/// without having to fully parse the PDF.
fn objects_containing(pdf: &[u8], marker: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut cursor = 0;
    while cursor < pdf.len() {
        let Some(obj_idx) = pdf[cursor..]
            .windows(b" 0 obj".len())
            .position(|w| w == b" 0 obj")
        else {
            break;
        };
        let body_start_rel = obj_idx + b" 0 obj".len();
        let body_end_rel = pdf[cursor + body_start_rel..]
            .windows(b"endobj".len())
            .position(|w| w == b"endobj")
            .unwrap_or(pdf.len() - cursor - body_start_rel);
        let body = &pdf[cursor + body_start_rel..cursor + body_start_rel + body_end_rel];
        if body.windows(marker.len()).any(|w| w == marker) {
            out.push(body.to_vec());
        }
        cursor += body_start_rel + body_end_rel;
    }
    out
}

#[test]
fn outline_simple_two_bookmarks() {
    let pdf = render_one_page_pdf(
        "[ /Title (Intro) /Page 1 /OUT pdfmark
         [ /Title (Details) /Page 1 /OUT pdfmark",
    );
    // The catalog must reference /Outlines and have /PageMode /UseOutlines.
    assert!(
        pdf.windows(b"/Outlines ".len()).any(|w| w == b"/Outlines "),
        "/Outlines not referenced from catalog"
    );
    assert!(
        pdf.windows(b"/PageMode /UseOutlines".len())
            .any(|w| w == b"/PageMode /UseOutlines"),
        "/PageMode /UseOutlines missing"
    );
    // Both bookmark titles appear as /Title literals.
    let intro = objects_containing(&pdf, b"(Intro)");
    let details = objects_containing(&pdf, b"(Details)");
    assert_eq!(intro.len(), 1, "expected exactly one Intro outline node");
    assert_eq!(
        details.len(),
        1,
        "expected exactly one Details outline node"
    );
    // The /Outlines root carries /Count 2 (two visible top-level entries).
    let outlines_root = objects_containing(&pdf, b"/Type /Outlines");
    assert_eq!(outlines_root.len(), 1);
    let root_str = String::from_utf8_lossy(&outlines_root[0]);
    assert!(
        root_str.contains("/Count 2"),
        "expected /Count 2 on /Outlines root, got: {root_str}",
    );
}

#[test]
fn outline_count_based_nesting() {
    // Adobe count-based authoring: parent declares /Count 2, then two
    // children follow. Render multi-page so /Page targets resolve.
    let mut interp = Interpreter::new();
    let script = "[ /Title (Parent) /Page 1 /Count 2 /OUT pdfmark
                  [ /Title (Child A) /Page 1 /OUT pdfmark
                  [ /Title (Child B) /Page 1 /OUT pdfmark
                  showpage";
    let pdf = interp
        .render_to_pdf(script.as_bytes(), 72.0)
        .expect("render");
    let parent = objects_containing(&pdf, b"(Parent)");
    assert_eq!(parent.len(), 1);
    let parent_str = String::from_utf8_lossy(&parent[0]);
    // Parent dict carries /First, /Last, /Count (signed).
    assert!(
        parent_str.contains("/First"),
        "parent missing /First: {parent_str}"
    );
    assert!(
        parent_str.contains("/Last"),
        "parent missing /Last: {parent_str}"
    );
    // /Count -2 (default closed since the producer didn't set positive).
    assert!(
        parent_str.contains("/Count 2") || parent_str.contains("/Count -2"),
        "parent missing /Count: {parent_str}",
    );
    // Outlines root has /Count 3 (1 parent visible + 2 children counted
    // because /Count is positive — the producer asked for expanded).
    let outlines_root = objects_containing(&pdf, b"/Type /Outlines");
    let root_str = String::from_utf8_lossy(&outlines_root[0]);
    assert!(
        root_str.contains("/Count 3"),
        "expected /Count 3 on root, got: {root_str}",
    );
}

#[test]
fn outline_uri_action() {
    let pdf = render_one_page_pdf(
        "[ /Title (Visit Example)
            /Action << /S /URI /URI (https://example.org) >>
            /OUT pdfmark",
    );
    let nodes = objects_containing(&pdf, b"(Visit Example)");
    assert_eq!(nodes.len(), 1);
    let body = String::from_utf8_lossy(&nodes[0]);
    assert!(body.contains("/A "), "expected /A action entry: {body}",);
    assert!(
        body.contains("(https://example.org)"),
        "expected URI string: {body}",
    );
    assert!(body.contains("/S /URI"), "expected /S /URI: {body}",);
}

#[test]
fn outline_view_xyz_passes_through() {
    let pdf = render_one_page_pdf("[ /Title (Top) /Page 1 /View [/XYZ 100 700 1.5] /OUT pdfmark");
    let nodes = objects_containing(&pdf, b"(Top)");
    assert_eq!(nodes.len(), 1);
    let body = String::from_utf8_lossy(&nodes[0]);
    assert!(body.contains("/XYZ"), "expected /XYZ in dest: {body}");
    assert!(body.contains("100"), "expected x=100 in dest: {body}");
    assert!(body.contains("700"), "expected y=700 in dest: {body}");
    assert!(body.contains("1.5"), "expected zoom=1.5 in dest: {body}");
}

#[test]
fn outline_named_dest_passes_through() {
    let pdf = render_one_page_pdf("[ /Title (Jump) /Dest /chap1 /OUT pdfmark");
    let nodes = objects_containing(&pdf, b"(Jump)");
    assert_eq!(nodes.len(), 1);
    let body = String::from_utf8_lossy(&nodes[0]);
    assert!(
        body.contains("/Dest (chap1)"),
        "expected named destination as literal string, got: {body}",
    );
}

#[test]
fn outline_titleless_does_not_emit() {
    // No /Title → record dropped silently → no /Outlines on catalog.
    let pdf = render_one_page_pdf("[ /Page 1 /OUT pdfmark");
    assert!(
        !pdf.windows(b"/Outlines ".len()).any(|w| w == b"/Outlines "),
        "expected catalog without /Outlines reference",
    );
}

#[test]
fn no_outline_records_no_catalog_reference() {
    // Sanity: when no /OUT pdfmark issued, /Catalog has no /Outlines.
    let pdf = render_one_page_pdf("");
    assert!(
        !pdf.windows(b"/Outlines ".len()).any(|w| w == b"/Outlines "),
        "expected catalog without /Outlines reference",
    );
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
