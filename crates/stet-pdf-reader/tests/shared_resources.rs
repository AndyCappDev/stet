// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Resources a page names many times are read once.
//!
//! A flyer exported from a layout program can carry thousands of small
//! images, each naming the same ICC profile and each an entry of one
//! `/XObject` dictionary. Reading the profile's stream for every image, and
//! copying the dictionary for every `Do`, made such a page take seconds and
//! gigabytes to read.

use std::sync::Arc;

use stet_graphics::device::ImageColorSpace;
use stet_graphics::display_list::DisplayElement;
use stet_pdf_reader::PdfDocument;

fn pdf_from(objects: &[String]) -> Vec<u8> {
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = pdf.len();
    pdf.extend(format!("xref\n0 {}\n0000000000 65535 f\r\n", objects.len() + 1).as_bytes());
    for off in offsets {
        pdf.extend(format!("{off:010} 00000 n\r\n").as_bytes());
    }
    pdf.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}

fn hex_stream(dict: &str, data: &[u8]) -> String {
    let hex: String = data.iter().map(|b| format!("{b:02X}")).collect();
    format!(
        "<< {dict} /Filter /ASCIIHexDecode /Length {} >>\nstream\n{hex}>\nendstream",
        hex.len() + 1
    )
}

/// A page painting two one-pixel CMYK images in the ICCBased space of
/// object 5, found through the `/XObject` dictionary written as `xobjects`
/// in the page's resources (object 8 holds the same dictionary).
fn two_images(xobjects: &str) -> Vec<u8> {
    let profile = include_bytes!("../../stet-graphics/tests/data/cmyk_intent/inklimit.icc");
    let image = |data: &[u8]| {
        hex_stream(
            "/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 \
             /ColorSpace [/ICCBased 5 0 R]",
            data,
        )
    };
    let content = "q 10 0 0 10 0 0 cm /Im0 Do Q q 10 0 0 10 10 0 cm /Im1 Do Q";
    pdf_from(&[
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 20 10] /Contents 4 0 R \
             /Resources << /XObject {xobjects} >> >>"
        ),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
        hex_stream("/N 4", profile),
        image(&[255, 0, 0, 0]),
        image(&[0, 255, 0, 0]),
        "<< /Im0 6 0 R /Im1 7 0 R >>".into(),
    ])
}

/// The profile each image on the page is painted in, and its samples.
fn profiles(pdf: Vec<u8>) -> Vec<(Arc<Vec<u8>>, Vec<u8>)> {
    let doc = PdfDocument::from_owned(pdf).expect("document opens");
    let list = doc.render_page(0, 72.0).expect("page reads");
    assert!(
        doc.parse_warnings().is_empty(),
        "{:?}",
        doc.parse_warnings()
    );
    list.elements()
        .iter()
        .filter_map(|e| match e {
            DisplayElement::Image {
                sample_data,
                params,
            } => match &params.color_space {
                ImageColorSpace::ICCBased { profile_data, .. } => {
                    Some((profile_data.clone(), sample_data.to_vec()))
                }
                other => panic!("an ICCBased image expected, got {other:?}"),
            },
            _ => None,
        })
        .collect()
}

/// Two images naming one profile object carry one copy of it.
#[test]
fn images_share_the_profile_they_name() {
    let painted = profiles(two_images("<< /Im0 6 0 R /Im1 7 0 R >>"));
    let [(first, cyan), (second, magenta)] = painted.as_slice() else {
        panic!("two images expected, got {}", painted.len());
    };
    assert!(Arc::ptr_eq(first, second));
    assert_eq!(
        first.len(),
        include_bytes!("../../stet-graphics/tests/data/cmyk_intent/inklimit.icc").len()
    );
    assert_eq!(cyan, &[255, 0, 0, 0]);
    assert_eq!(magenta, &[0, 255, 0, 0]);
}

/// A resource category held in an object of its own gives the same entries
/// as one written into the resources.
#[test]
fn an_indirect_category_gives_the_same_entries() {
    let direct = profiles(two_images("<< /Im0 6 0 R /Im1 7 0 R >>"));
    let indirect = profiles(two_images("8 0 R"));
    assert_eq!(indirect.len(), 2);
    let samples =
        |p: &[(Arc<Vec<u8>>, Vec<u8>)]| p.iter().map(|(_, s)| s.clone()).collect::<Vec<_>>();
    assert_eq!(samples(&direct), samples(&indirect));
}

/// A name the category does not hold, or a category that is not there
/// (object 9 does not exist), is reported and does not stop the page.
#[test]
fn a_missing_entry_is_reported() {
    for xobjects in ["<< /Im1 7 0 R >>", "9 0 R"] {
        let doc = PdfDocument::from_owned(two_images(xobjects)).expect("document opens");
        doc.render_page(0, 72.0).expect("page reads");
        assert!(!doc.parse_warnings().is_empty(), "{xobjects}");
    }
}
