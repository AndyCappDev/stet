// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Transfer functions (`/TR`, `/TR2`) in the PDF reader.
//!
//! The reader carries the function in force beside each paint, and the
//! renderer applies it to the paint's final RGB (ISO 32000-1 §10.5), only
//! where the paint is fully opaque (§11.7.5.2). Expected values are
//! Ghostscript 10.05.1's `png16m` at 72 dpi, except where the spec and
//! Ghostscript part (noted there).

// Renders to pixels, which is the `render` feature.
#![cfg(feature = "render")]

use stet_graphics::display_list::DisplayElement;
use stet_graphics::icc::IccCache;
use stet_pdf_reader::PdfDocument;

/// An inverting function as an ExtGState, `/T`.
const INVERT: &str = "/T << /TR << /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1 >> >>";

/// A one-page 40×20 document. `ext_gstates` and `xobjects` are the
/// contents of those resource dictionaries; `extra` are further objects,
/// numbered from 5.
fn document(content: &str, ext_gstates: &str, xobjects: &str, extra: &[String]) -> Vec<u8> {
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 40 20] \
             /Resources << /ExtGState << {INVERT} {ext_gstates} >> \
             /XObject << {xobjects} >> >> /Contents 4 0 R >>"
        ),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
    ];
    objects.extend(extra.iter().cloned());
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

/// A form XObject over the whole page, with the page's ExtGStates.
fn form(content: &str, group: &str) -> String {
    format!(
        "<< /Type /XObject /Subtype /Form /BBox [0 0 40 20] {group} \
         /Resources << /ExtGState << {INVERT} >> >> /Length {} >>\n\
         stream\n{content}\nendstream",
        content.len()
    )
}

/// A transparency group dictionary entry.
const GROUP: &str = "/Group << /S /Transparency >>";

/// The page's RGB at `(x, y)` in PDF user space.
fn pixel(pdf: &[u8], x: u32, y: u32) -> [u8; 3] {
    let doc = PdfDocument::from_bytes_with_icc(pdf, IccCache::new()).unwrap();
    let (rgba, width, height) = doc.render_page_to_rgba(0, 72.0).unwrap();
    let i = (((height - 1 - y) * width + x) * 4) as usize;
    [rgba[i], rgba[i + 1], rgba[i + 2]]
}

fn at_centre(content: &str) -> [u8; 3] {
    pixel(&document(content, "", "", &[]), 20, 10)
}

fn inverted(rgb: [u8; 3]) -> [u8; 3] {
    rgb.map(|v| 255 - v)
}

/// The colour of a fill keeps its native components: the function travels
/// beside it rather than being baked into its RGB.
#[test]
fn fills_carry_the_function_beside_their_colour() {
    let pdf = document("/T gs 0 0 0 1 k 0 0 40 20 re f", "", "", &[]);
    let doc = PdfDocument::from_bytes_with_icc(&pdf, IccCache::new()).unwrap();
    let list = doc.render_page(0, 72.0).unwrap();
    let fill = list
        .elements()
        .iter()
        .find_map(|e| match e {
            DisplayElement::Fill { params, .. } => Some(params.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(fill.color.native_cmyk, Some((0.0, 0.0, 0.0, 1.0)));
    assert!(fill.color.r < 0.5, "the colour is not inverted in the list");
    assert!(fill.transfer.has_functions());

    assert_eq!(at_centre("/T gs 0.2 g 0 0 40 20 re f"), [204; 3]);
    assert_eq!(at_centre("/T gs 0.2 G 20 w 0 10 m 40 10 l S"), [204; 3]);
    assert_eq!(
        at_centre("/T gs 0 0 0 1 k 0 0 40 20 re f"),
        inverted(at_centre("0 0 0 1 k 0 0 40 20 re f"))
    );
}

/// Images take the function after conversion to RGB, in every colour
/// space, as XObjects and inline.
#[test]
fn images_take_the_function_in_every_colour_space() {
    let cases = [
        ("/DeviceGray", "C837"),
        ("/DeviceRGB", "C86414 3700FF"),
        ("/DeviceCMYK", "00000000 1E3C00FF"),
        ("[/Indexed /DeviceRGB 1 <FF0000 0050A0>]", "0100"),
        (
            "[/Separation /Spot /DeviceCMYK << /FunctionType 2 /Domain [0 1] \
             /C0 [0 0 0 0] /C1 [0.5 0 0.8 0.1] /N 1 >>]",
            "FF3C",
        ),
    ];
    for (space, data) in cases {
        let inline =
            format!("q 40 0 0 20 0 0 cm BI /W 2 /H 1 /CS {space} /BPC 8 /F /AHx ID {data}> EI Q");
        let xobject = format!(
            "<< /Type /XObject /Subtype /Image /Width 2 /Height 1 /ColorSpace {space} \
             /BitsPerComponent 8 /Filter /ASCIIHexDecode /Length {} >>\n\
             stream\n{data}>\nendstream",
            data.len() + 1
        );
        for (name, content, xobjects, extra) in [
            ("inline", inline.clone(), "", vec![]),
            (
                "XObject",
                "q 40 0 0 20 0 0 cm /Im0 Do Q".to_string(),
                "/Im0 5 0 R",
                vec![xobject],
            ),
        ] {
            let plain = document(&content, "", xobjects, &extra);
            let under = document(&format!("/T gs {content}"), "", xobjects, &extra);
            for x in [10, 30] {
                assert_eq!(
                    pixel(&under, x, 10),
                    inverted(pixel(&plain, x, 10)),
                    "{name} {space} at x = {x}"
                );
            }
        }
    }
}

/// An image XObject painted twice reuses its decoded samples; the second
/// paint takes the function in force then, not the first one's.
#[test]
fn a_reused_image_takes_the_function_in_force() {
    let image = "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 \
                 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /ASCIIHexDecode \
                 /Length 3 >>\nstream\n33>\nendstream"
        .to_string();
    let first_under = document(
        "q /T gs 20 0 0 20 0 0 cm /Im0 Do Q q 20 0 0 20 20 0 cm /Im0 Do Q",
        "",
        "/Im0 5 0 R",
        std::slice::from_ref(&image),
    );
    assert_eq!(pixel(&first_under, 10, 10), [204; 3]);
    assert_eq!(pixel(&first_under, 30, 10), [51; 3]);
    let second_under = document(
        "q 20 0 0 20 0 0 cm /Im0 Do Q q /T gs 20 0 0 20 20 0 cm /Im0 Do Q",
        "",
        "/Im0 5 0 R",
        &[image],
    );
    assert_eq!(pixel(&second_under, 10, 10), [51; 3]);
    assert_eq!(pixel(&second_under, 30, 10), [204; 3]);
}

#[test]
fn shadings_take_the_function() {
    let shading = "/Shading << /S0 << /ShadingType 2 /ColorSpace /DeviceGray \
                   /Coords [0 0 40 0] /Function << /FunctionType 2 /Domain [0 1] \
                   /C0 [0.1] /C1 [0.9] /N 1 >> /Extend [true true] >> >>";
    let doc = |content: &str| {
        let mut pdf = document(content, "", "", &[]);
        // The shading resource goes beside the ExtGStates.
        let page =
            String::from_utf8_lossy(&pdf).replace("/XObject <<", &format!("{shading} /XObject <<"));
        pdf = fix_xref(page.into_bytes());
        pdf
    };
    let plain = doc("/S0 sh");
    let under = doc("/T gs /S0 sh");
    for x in [5, 20, 35] {
        let want = inverted(pixel(&plain, x, 10));
        let got = pixel(&under, x, 10);
        assert!(
            want.iter().zip(&got).all(|(a, b)| a.abs_diff(*b) <= 1),
            "at x = {x}: want {want:?}, got {got:?}"
        );
    }
}

/// Rewrite the cross-reference table of a document edited in place.
fn fix_xref(pdf: Vec<u8>) -> Vec<u8> {
    let text = String::from_utf8_lossy(&pdf).into_owned();
    let body_end = text.find("xref\n").unwrap();
    let body = &text[..body_end];
    let mut offsets = Vec::new();
    let mut search = 0;
    while let Some(i) = body[search..].find(" 0 obj\n") {
        let start = body[..search + i].rfind('\n').map_or(0, |n| n + 1);
        offsets.push(start);
        search += i + 1;
    }
    let mut out = body.as_bytes().to_vec();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f\r\n", offsets.len() + 1).as_bytes());
    for off in &offsets {
        out.extend(format!("{off:010} 00000 n\r\n").as_bytes());
    }
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{body_end}\n%%EOF\n",
            offsets.len() + 1
        )
        .as_bytes(),
    );
    out
}

// ---------------------------------------------------------------------------
// Only fully opaque paints take their function (ISO 32000-1 §11.7.5.2).
// Gray 0.2 at half alpha over white is 153 with no function, 229 with it.
// ---------------------------------------------------------------------------

const HALF: &str = "/H << /ca 0.5 /CA 0.5 >>";

#[test]
fn transparent_paints_take_no_function() {
    let pdf = document("/T gs /H gs 0.2 g 0 0 40 20 re f", HALF, "", &[]);
    assert_eq!(pixel(&pdf, 20, 10), [153; 3]);
    // Multiply over an opaque 0.6 that takes its function (0.4): 0.4 × 0.2.
    // The spec reverts the region to the default function, 0.6 × 0.2 = 31;
    // Ghostscript, and stet, apply the opaque paint's beneath (20).
    let pdf = document(
        "/T gs 0.6 g 0 0 40 20 re f /M gs 0.2 g 0 0 40 20 re f",
        "/M << /BM /Multiply >>",
        "",
        &[],
    );
    let [v, ..] = pixel(&pdf, 20, 10);
    assert!(v.abs_diff(20) <= 1, "multiply: got {v}");
}

#[test]
fn paints_in_a_transparent_group_take_no_function() {
    // The function and the alpha inside the group.
    let pdf = document(
        "/F0 Do",
        "",
        "/F0 5 0 R",
        &[form("/T gs /H gs 0.2 g 0 0 40 20 re f", GROUP)
            .replace("/ExtGState << ", &format!("/ExtGState << {HALF} "))],
    );
    assert_eq!(pixel(&pdf, 20, 10), [153; 3]);
    // Opaque content in a group drawn at half alpha: the spec's ancestor
    // condition gives no function (153). Ghostscript and poppler apply it
    // (229).
    let group = form("0.2 g 0 0 40 20 re f", GROUP);
    let pdf = document(
        "/T gs /H gs /F0 Do",
        HALF,
        "/F0 5 0 R",
        std::slice::from_ref(&group),
    );
    assert_eq!(pixel(&pdf, 20, 10), [153; 3]);
    // The same group drawn opaque: its content takes the function.
    let pdf = document("/T gs /F0 Do", "", "/F0 5 0 R", &[group]);
    assert_eq!(pixel(&pdf, 20, 10), [204; 3]);
}

#[test]
fn soft_masks_take_no_function() {
    let smask = |mask_content: &str| {
        (
            "/SM << /SMask << /Type /Mask /S /Luminosity /G 5 0 R >> >>".to_string(),
            form(
                mask_content,
                "/Group << /S /Transparency /CS /DeviceGray >>",
            ),
        )
    };
    // An object under a soft mask is not fully opaque.
    let (sm, mask) = smask("0.5 g 0 0 40 20 re f");
    let pdf = document("/T gs /SM gs 0.2 g 0 0 40 20 re f", &sm, "", &[mask]);
    let [v, ..] = pixel(&pdf, 20, 10);
    assert!(v.abs_diff(153) <= 1, "masked object: got {v}");
    // A function in the mask's own content is not a final colour: the mask
    // stays 0.25, and black through it is 0.75 white.
    let (sm, mask) = smask("/T gs 0.25 g 0 0 40 20 re f");
    let pdf = document("/SM gs 0 g 0 0 40 20 re f", &sm, "", &[mask]);
    let [v, ..] = pixel(&pdf, 20, 10);
    assert!(v.abs_diff(191) <= 1, "function in the mask: got {v}");
    // Nor one in force when the mask is set.
    let (sm, mask) = smask("0.25 g 0 0 40 20 re f");
    let pdf = document(
        "/T gs /SM gs /I gs 0 g 0 0 40 20 re f",
        &format!("{sm} /I << /TR /Identity >>"),
        "",
        &[mask],
    );
    let [v, ..] = pixel(&pdf, 20, 10);
    assert!(
        v.abs_diff(191) <= 1,
        "function when the mask is set: got {v}"
    );
}

/// An image with a soft mask is never fully opaque, even where the mask
/// is.
#[test]
fn soft_masked_images_take_no_function() {
    let image = "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 \
                 /ColorSpace /DeviceGray /BitsPerComponent 8 /SMask 6 0 R \
                 /Filter /ASCIIHexDecode /Length 3 >>\nstream\n33>\nendstream"
        .to_string();
    let mask = "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 \
                /ColorSpace /DeviceGray /BitsPerComponent 8 \
                /Filter /ASCIIHexDecode /Length 3 >>\nstream\nFF>\nendstream"
        .to_string();
    let pdf = document(
        "/T gs q 40 0 0 20 0 0 cm /Im0 Do Q",
        "",
        "/Im0 5 0 R",
        &[image, mask],
    );
    assert_eq!(pixel(&pdf, 20, 10), [51; 3]);
}

/// Overprint simulation composites the ink in CMYK; the function applies to
/// the RGB that composite resolves to, not to the ink.
#[test]
fn overprinted_paints_take_the_function_on_the_resolved_colour() {
    let content = "1 0 0 0 k 0 0 40 20 re f /OP gs 0 0 0 0.5 k 0 0 40 20 re f";
    let op = "/OP << /OP true /op true /OPM 1 >>";
    let plain = pixel(&document(content, op, "", &[]), 20, 10);
    // Simulated: the cyan beneath shows through the black.
    let k_alone = pixel(&document("0 0 0 0.5 k 0 0 40 20 re f", "", "", &[]), 20, 10);
    assert_ne!(plain, k_alone, "overprint is simulated");
    let under = pixel(&document(&format!("/T gs {content}"), op, "", &[]), 20, 10);
    let want = inverted(plain);
    assert!(
        want.iter().zip(&under).all(|(a, b)| a.abs_diff(*b) <= 1),
        "want {want:?}, got {under:?}"
    );
}
