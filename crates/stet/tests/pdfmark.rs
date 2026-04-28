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
fn annotation_link_uri_emits_on_page() {
    let pdf = render_one_page_pdf(
        "[ /Rect [72 720 540 750] /Subtype /Link /Border [0 0 1]
            /Action << /S /URI /URI (https://example.org) >>
            /ANN pdfmark",
    );
    // /Annots array on the page dict
    let pages = objects_containing(&pdf, b"/Type /Page\n");
    assert_eq!(pages.len(), 1, "expected one page dict");
    let page_str = String::from_utf8_lossy(&pages[0]);
    assert!(
        page_str.contains("/Annots"),
        "page missing /Annots: {page_str}",
    );
    // Link annotation indirect carries /Subtype /Link + /A /URI dict
    let links = objects_containing(&pdf, b"/Subtype /Link");
    assert_eq!(links.len(), 1, "expected one /Link annotation");
    let link_body = String::from_utf8_lossy(&links[0]);
    assert!(
        link_body.contains("(https://example.org)"),
        "link missing URI string: {link_body}",
    );
    assert!(
        link_body.contains("/S /URI"),
        "link missing /S /URI: {link_body}",
    );
    // /Rect round-trip
    assert!(
        link_body.contains("/Rect"),
        "link missing /Rect: {link_body}"
    );
}

#[test]
fn annotation_link_internal_goto() {
    let mut interp = Interpreter::new();
    let script = "showpage
        showpage
        [ /Rect [50 50 150 80] /Subtype /Link /Page 1
            /Action << /S /GoTo /D [1 /Fit] >>
            /ANN pdfmark
        showpage";
    let pdf = interp
        .render_to_pdf(script.as_bytes(), 72.0)
        .expect("render");
    let links = objects_containing(&pdf, b"/Subtype /Link");
    assert_eq!(links.len(), 1);
    let body = String::from_utf8_lossy(&links[0]);
    assert!(body.contains("/S /GoTo"), "expected /S /GoTo: {body}",);
    assert!(body.contains("/Fit"), "expected /Fit view: {body}");
}

#[test]
fn annotation_text_sticky_note() {
    let pdf = render_one_page_pdf(
        "[ /Rect [400 400 420 420]
            /Subtype /Text
            /Contents (Look here)
            /Name /Note
            /ANN pdfmark",
    );
    let texts = objects_containing(&pdf, b"/Subtype /Text");
    assert_eq!(texts.len(), 1);
    let body = String::from_utf8_lossy(&texts[0]);
    assert!(body.contains("(Look here)"), "missing /Contents: {body}",);
    assert!(body.contains("/Name /Note"), "missing /Name /Note: {body}",);
    assert!(
        body.contains("/Open false"),
        "expected /Open false default: {body}",
    );
}

#[test]
fn annotation_freetext_default_appearance() {
    let pdf = render_one_page_pdf(
        "[ /Rect [100 100 300 150]
            /Subtype /FreeText
            /Contents (Visible label)
            /ANN pdfmark",
    );
    let ft = objects_containing(&pdf, b"/Subtype /FreeText");
    assert_eq!(ft.len(), 1);
    let body = String::from_utf8_lossy(&ft[0]);
    assert!(body.contains("/DA "), "FreeText missing /DA: {body}",);
    // Default appearance string applied when /DA omitted.
    assert!(
        body.contains("(0 0 0 rg /Helv 10 Tf)"),
        "expected default /DA: {body}",
    );
}

#[test]
fn annotation_implicit_page_scoping() {
    // /ANN with no /Page key scopes to the page being assembled.
    // Two showpages, one /ANN before the second showpage → it lands
    // on page 2.
    let mut interp = Interpreter::new();
    let script = "showpage
        [ /Rect [0 0 10 10] /Subtype /Text /Contents (page 2 note) /ANN pdfmark
        showpage";
    let pdf = interp
        .render_to_pdf(script.as_bytes(), 72.0)
        .expect("render");
    let annotations = objects_containing(&pdf, b"/Subtype /Text");
    assert_eq!(annotations.len(), 1);
    // The annotation's indirect ref appears in page 2's /Annots
    // (the second /Type /Page object).
    let pages = objects_containing(&pdf, b"/Type /Page\n");
    assert_eq!(pages.len(), 2);
    let page1_str = String::from_utf8_lossy(&pages[0]);
    let page2_str = String::from_utf8_lossy(&pages[1]);
    assert!(
        !page1_str.contains("/Annots"),
        "page 1 should have no /Annots: {page1_str}",
    );
    assert!(
        page2_str.contains("/Annots"),
        "page 2 should have /Annots: {page2_str}",
    );
}

#[test]
fn annotation_multiple_per_page_accumulate() {
    let pdf = render_one_page_pdf(
        "[ /Rect [0 0 10 10] /Subtype /Text /Contents (one) /ANN pdfmark
         [ /Rect [20 0 30 10] /Subtype /Text /Contents (two) /ANN pdfmark
         [ /Rect [40 0 50 10] /Subtype /Text /Contents (three) /ANN pdfmark",
    );
    let texts = objects_containing(&pdf, b"/Subtype /Text");
    assert_eq!(texts.len(), 3);
    let pages = objects_containing(&pdf, b"/Type /Page\n");
    let page_str = String::from_utf8_lossy(&pages[0]);
    assert!(page_str.contains("/Annots"));
    // /Annots array carries three indirect refs
    let annots_idx = page_str.find("/Annots").unwrap();
    let after = &page_str[annots_idx..];
    let rs: usize = after.matches(" 0 R").count();
    assert!(rs >= 3, "expected ≥3 indirect refs in /Annots: {page_str}");
}

#[test]
fn annotation_unknown_subtype_no_emit() {
    let pdf = render_one_page_pdf("[ /Rect [0 0 10 10] /Subtype /WeirdThing /ANN pdfmark");
    let pages = objects_containing(&pdf, b"/Type /Page\n");
    let page_str = String::from_utf8_lossy(&pages[0]);
    assert!(
        !page_str.contains("/Annots"),
        "page should have no /Annots when subtype unknown: {page_str}",
    );
}

#[test]
fn dest_named_emits_in_catalog_names_tree() {
    let pdf = render_one_page_pdf("[ /Dest /chap1 /Page 1 /View [/XYZ 100 700 1.5] /DEST pdfmark");
    // /Catalog must reference /Names
    let catalog = objects_containing(&pdf, b"/Type /Catalog");
    assert_eq!(catalog.len(), 1);
    let cat_str = String::from_utf8_lossy(&catalog[0]);
    assert!(
        cat_str.contains("/Names"),
        "catalog missing /Names: {cat_str}"
    );
    // The leaf names array contains the (chap1) string
    let leaves = objects_containing(&pdf, b"(chap1)");
    assert!(!leaves.is_empty(), "expected (chap1) somewhere");
}

#[test]
fn dest_outline_can_reference_named_dest() {
    // An /OUT bookmark with /Dest /chap1 + a /DEST named-dest entry
    // both end up in the output PDF and a viewer can resolve the
    // outline against the name tree.
    let pdf = render_one_page_pdf(
        "[ /Dest /chap1 /Page 1 /View [/Fit] /DEST pdfmark
         [ /Title (Chapter 1) /Dest /chap1 /OUT pdfmark",
    );
    let outline = objects_containing(&pdf, b"(Chapter 1)");
    assert_eq!(outline.len(), 1);
    let outline_body = String::from_utf8_lossy(&outline[0]);
    assert!(
        outline_body.contains("/Dest (chap1)"),
        "outline missing /Dest (chap1): {outline_body}",
    );
    // Name tree present
    let leaves = objects_containing(&pdf, b"(chap1)");
    assert!(
        leaves.len() >= 2,
        "expected (chap1) in both outline + name tree"
    );
}

#[test]
fn page_cropbox_override_applied() {
    let pdf = render_one_page_pdf("[ /CropBox [36 36 576 756] /Page 1 /PAGE pdfmark");
    let pages = objects_containing(&pdf, b"/Type /Page\n");
    assert_eq!(pages.len(), 1);
    let page_str = String::from_utf8_lossy(&pages[0]);
    assert!(
        page_str.contains("/CropBox"),
        "missing /CropBox on page: {page_str}",
    );
    assert!(
        page_str.contains("36"),
        "expected llx=36 in /CropBox: {page_str}",
    );
    assert!(
        page_str.contains("576"),
        "expected urx=576 in /CropBox: {page_str}",
    );
}

#[test]
fn pages_default_overridden_by_page() {
    // /PAGES sets a doc-wide CropBox; /PAGE for page 1 overrides it.
    // Render two pages and check page 1 has the override CropBox while
    // page 2 falls through to the /PAGES default.
    let mut interp = Interpreter::new();
    let script = "[ /CropBox [10 10 100 100] /PAGES pdfmark
         [ /CropBox [200 200 400 400] /Page 1 /PAGE pdfmark
         showpage
         showpage";
    let pdf = interp
        .render_to_pdf(script.as_bytes(), 72.0)
        .expect("render");
    let pages = objects_containing(&pdf, b"/Type /Page\n");
    assert_eq!(pages.len(), 2);
    let p1 = String::from_utf8_lossy(&pages[0]);
    let p2 = String::from_utf8_lossy(&pages[1]);
    assert!(
        p1.contains("200") && p1.contains("400"),
        "page 1 should have /PAGE override (200,400): {p1}",
    );
    assert!(
        p2.contains("10") && p2.contains("100"),
        "page 2 should have /PAGES default (10,100): {p2}",
    );
}

#[test]
fn page_rotate_emits() {
    let pdf = render_one_page_pdf("[ /Rotate 90 /Page 1 /PAGE pdfmark");
    let pages = objects_containing(&pdf, b"/Type /Page\n");
    let page_str = String::from_utf8_lossy(&pages[0]);
    assert!(
        page_str.contains("/Rotate 90"),
        "expected /Rotate 90: {page_str}",
    );
}

#[test]
fn page_rotate_invalid_dropped() {
    // /Rotate 45 is not a multiple of 90 — writer drops it silently.
    let pdf = render_one_page_pdf("[ /Rotate 45 /Page 1 /PAGE pdfmark");
    let pages = objects_containing(&pdf, b"/Type /Page\n");
    let page_str = String::from_utf8_lossy(&pages[0]);
    assert!(
        !page_str.contains("/Rotate"),
        "expected no /Rotate when value invalid: {page_str}",
    );
}

#[test]
fn no_dest_records_no_names_in_catalog() {
    let pdf = render_one_page_pdf("");
    let catalog = objects_containing(&pdf, b"/Type /Catalog");
    let cat_str = String::from_utf8_lossy(&catalog[0]);
    assert!(
        !cat_str.contains("/Names"),
        "catalog should have no /Names when no /DEST records: {cat_str}",
    );
}

#[test]
fn viewer_prefs_emits_dict() {
    let pdf = render_one_page_pdf(
        "[ /HideToolbar true /FitWindow true /Direction /L2R /VIEWERPREFERENCES pdfmark",
    );
    let catalog = objects_containing(&pdf, b"/Type /Catalog");
    let cat_str = String::from_utf8_lossy(&catalog[0]);
    assert!(
        cat_str.contains("/ViewerPreferences"),
        "/ViewerPreferences not on catalog: {cat_str}",
    );
    let vp = objects_containing(&pdf, b"/HideToolbar true");
    assert_eq!(vp.len(), 1);
    let body = String::from_utf8_lossy(&vp[0]);
    assert!(
        body.contains("/FitWindow true"),
        "missing FitWindow: {body}"
    );
    assert!(
        body.contains("/Direction /L2R"),
        "missing Direction: {body}"
    );
}

#[test]
fn viewer_prefs_page_mode_lifts_to_catalog() {
    let pdf = render_one_page_pdf(
        "[ /PageMode /FullScreen /PageLayout /TwoColumnLeft /VIEWERPREFERENCES pdfmark",
    );
    let catalog = objects_containing(&pdf, b"/Type /Catalog");
    let cat_str = String::from_utf8_lossy(&catalog[0]);
    assert!(
        cat_str.contains("/PageMode /FullScreen"),
        "expected /PageMode /FullScreen on catalog: {cat_str}",
    );
    assert!(
        cat_str.contains("/PageLayout /TwoColumnLeft"),
        "expected /PageLayout on catalog: {cat_str}",
    );
}

#[test]
fn viewer_prefs_page_mode_overrides_outline_default() {
    // /OUT records normally cause /PageMode /UseOutlines on the catalog;
    // an explicit /VIEWERPREFERENCES /PageMode /UseThumbs must win.
    let pdf = render_one_page_pdf(
        "[ /Title (Intro) /Page 1 /OUT pdfmark
         [ /PageMode /UseThumbs /VIEWERPREFERENCES pdfmark",
    );
    let catalog = objects_containing(&pdf, b"/Type /Catalog");
    let cat_str = String::from_utf8_lossy(&catalog[0]);
    assert!(
        cat_str.contains("/PageMode /UseThumbs"),
        "expected /PageMode /UseThumbs override: {cat_str}",
    );
    assert!(
        !cat_str.contains("/PageMode /UseOutlines"),
        "did not expect /PageMode /UseOutlines after override: {cat_str}",
    );
}

#[test]
fn viewer_prefs_invalid_page_layout_dropped() {
    let pdf = render_one_page_pdf("[ /PageLayout /MadeUpLayout /VIEWERPREFERENCES pdfmark");
    let catalog = objects_containing(&pdf, b"/Type /Catalog");
    let cat_str = String::from_utf8_lossy(&catalog[0]);
    assert!(
        !cat_str.contains("/PageLayout"),
        "expected invalid /PageLayout to be dropped: {cat_str}",
    );
}

#[test]
fn metadata_emits_xmp_stream() {
    let xmp =
        "<?xpacket begin='?'?><x:xmpmeta xmlns:x='adobe:ns:meta/'></x:xmpmeta><?xpacket end='w'?>";
    let pdf = render_one_page_pdf(&format!("[ /Metadata ({}) /Metadata pdfmark", xmp));
    // /Catalog references /Metadata
    let catalog = objects_containing(&pdf, b"/Type /Catalog");
    let cat_str = String::from_utf8_lossy(&catalog[0]);
    assert!(
        cat_str.contains("/Metadata "),
        "/Metadata not on catalog: {cat_str}",
    );
    // The stream object carries /Type /Metadata /Subtype /XML.
    let metadata = objects_containing(&pdf, b"/Type /Metadata");
    // Two matches: catalog (with `/Metadata <ref>`) and the stream
    // dict itself. We want at least the stream dict to appear.
    assert!(!metadata.is_empty());
    let stream_body = metadata
        .iter()
        .find(|o| {
            let s = String::from_utf8_lossy(o);
            s.contains("/Subtype /XML") && s.contains("stream")
        })
        .expect("metadata stream object present");
    let stream_str = String::from_utf8_lossy(stream_body);
    assert!(
        stream_str.contains("xmpmeta"),
        "XMP body not preserved: {stream_str}",
    );
}

#[test]
fn no_viewer_prefs_no_catalog_entry() {
    let pdf = render_one_page_pdf("");
    let catalog = objects_containing(&pdf, b"/Type /Catalog");
    let cat_str = String::from_utf8_lossy(&catalog[0]);
    assert!(
        !cat_str.contains("/ViewerPreferences"),
        "expected no /ViewerPreferences when no record: {cat_str}",
    );
    assert!(
        !cat_str.contains("/Metadata "),
        "expected no /Metadata when no record: {cat_str}",
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
