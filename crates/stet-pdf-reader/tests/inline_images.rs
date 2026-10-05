// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Inline images are painted as image XObjects are.
//!
//! An inline image is an image written into the content stream (ISO 32000-1
//! §8.9.7). It cannot carry the masks only an XObject may, but everything
//! else — `/Decode`, `/Interpolate`, transfer functions, the colour space's
//! conversion — applies to it alike. stet painted inline images through a
//! second, thinner pipeline that ignored `/Decode` and transfer functions;
//! both forms now share one, and these tests hold them to it.

use stet_graphics::device::ImageColorSpace;
use stet_graphics::display_list::DisplayElement;
use stet_graphics::icc::IccCache;
use stet_pdf_reader::PdfDocument;

/// A CMYK output profile, for gray painted as K under an output intent.
const CMYK_PROFILE: &[u8] =
    include_bytes!("../../stet-graphics/tests/data/cmyk_intent/inklimit.icc");

/// A two-input DeviceN space over DeviceGray averaging its inputs.
const DEVICE_N: &str = "[/DeviceN [/A /B] /DeviceGray 4 0 R]";

/// A Separation over DeviceGray whose tint 1 is black.
const SEPARATION: &str =
    "[/Separation /Spot /DeviceGray << /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1 >>]";

/// `/TR0`, an ExtGState with an inverting transfer function.
const EXT_G_STATE: &str =
    "/ExtGState << /TR0 << /TR << /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1 >> >> >>";

/// How the image is written.
#[derive(Clone, Copy, Debug)]
enum Form {
    Inline,
    XObject,
}

/// A one-page document painting a 2×1 image with dictionary entries
/// `entries` (full key names) and samples `data`, as `form`, after
/// `before` in the content stream; with a CMYK output intent if
/// `output_intent`.
fn document(entries: &str, data: &[u8], before: &str, form: Form, output_intent: bool) -> Vec<u8> {
    let hex: String = data.iter().map(|b| format!("{b:02X}")).collect();
    // Only an inline image may name a colour space resource; an XObject
    // spells it out.
    let entries = match form {
        Form::Inline => entries.to_string(),
        Form::XObject => entries
            .replace("/ColorSpace /DN", &format!("/ColorSpace {DEVICE_N}"))
            .replace("/ColorSpace /SP", &format!("/ColorSpace {SEPARATION}")),
    };
    let image = format!("/Width 2 /Height 1 {entries} /Filter /ASCIIHexDecode");
    let paint = match form {
        Form::Inline => format!("BI {image} ID {hex}> EI"),
        Form::XObject => "/Im0 Do".to_string(),
    };
    let content = format!("{before} q 20 0 0 20 0 0 cm {paint} Q");
    let profile_hex: String = CMYK_PROFILE.iter().map(|b| format!("{b:02X}")).collect();
    let catalog = if output_intent {
        "<< /Type /Catalog /Pages 2 0 R /OutputIntents [<< /Type /OutputIntent /S /GTS_PDFX \
         /OutputConditionIdentifier (Test) /DestOutputProfile 5 0 R >>] >>"
    } else {
        "<< /Type /Catalog /Pages 2 0 R >>"
    };
    let objects = [
        catalog.to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 20 20] \
             /Resources << /ColorSpace << /DN {DEVICE_N} /SP {SEPARATION} >> {EXT_G_STATE} \
             /XObject << /Im0 6 0 R >> >> /Contents 7 0 R >>"
        ),
        "<< /FunctionType 4 /Domain [0 1 0 1] /Range [0 1] /Length 13 >>\nstream\n{add 2 div}\nendstream"
            .to_string(),
        format!(
            "<< /N 4 /Filter /ASCIIHexDecode /Length {} >>\nstream\n{profile_hex}>\nendstream",
            profile_hex.len() + 1
        ),
        format!(
            "<< /Type /XObject /Subtype /Image {image} /Length {} >>\nstream\n{hex}>\nendstream",
            hex.len() + 1
        ),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
    ];

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

/// `pdf`, with no CMYK source profile: the system profile is not looked
/// for, which is most of the cost of opening a document, and the pipeline
/// under test does not depend on it.
fn open(pdf: &[u8]) -> PdfDocument<'_> {
    PdfDocument::from_bytes_with_icc(pdf, IccCache::new()).unwrap()
}

/// The page's one image: its samples, and its parameters as text.
fn image(pdf: &[u8]) -> (Vec<u8>, String) {
    let doc = open(pdf);
    let list = doc.render_page(0, 72.0).unwrap();
    let [
        DisplayElement::Image {
            sample_data,
            params,
        },
    ] = list.elements()
    else {
        panic!("one image expected, got {:?}", list.elements());
    };
    (sample_data.to_vec(), format!("{params:?}"))
}

/// The image's samples, painted inline, after checking that the XObject
/// form paints the same element.
fn samples(entries: &str, data: &[u8]) -> Vec<u8> {
    same_either_way(entries, data, "", false)
}

/// The image painted inline, after checking that the XObject form paints
/// the same samples with the same parameters.
fn same_either_way(entries: &str, data: &[u8], before: &str, output_intent: bool) -> Vec<u8> {
    let inline = image(&document(
        entries,
        data,
        before,
        Form::Inline,
        output_intent,
    ));
    let xobject = image(&document(
        entries,
        data,
        before,
        Form::XObject,
        output_intent,
    ));
    assert_eq!(
        inline, xobject,
        "{entries} after {before:?}: inline and XObject differ"
    );
    inline.0
}

/// Every case paints the same, inline or as an XObject: the guard that
/// keeps the two from drifting apart again.
#[test]
fn inline_images_paint_as_xobjects_do() {
    let indexed_rgb = "[/Indexed /DeviceRGB 3 <FF000000FF000000FFFFFFFF>]";
    let k_only = "[/Indexed /DeviceCMYK 1 <00000000000000FF>]";
    let cases: &[(&str, &[u8])] = &[
        ("/ColorSpace /DeviceGray /BitsPerComponent 1", &[0x80]),
        ("/ColorSpace /DeviceGray /BitsPerComponent 2", &[0xD0]),
        ("/ColorSpace /DeviceGray /BitsPerComponent 4", &[0xF3]),
        ("/ColorSpace /DeviceGray /BitsPerComponent 8", &[200, 55]),
        (
            "/ColorSpace /DeviceGray /BitsPerComponent 16",
            &[0x10, 0xFF, 0x81, 0x01],
        ),
        // A depth outside the spec: two 12-bit samples in three bytes.
        (
            "/ColorSpace /DeviceGray /BitsPerComponent 12",
            &[0x10, 0xF8, 0x01],
        ),
        (
            "/ColorSpace /DeviceRGB /BitsPerComponent 8",
            &[1, 2, 3, 4, 5, 6],
        ),
        (
            "/ColorSpace /DeviceCMYK /BitsPerComponent 8",
            &[1, 2, 3, 4, 5, 6, 7, 8],
        ),
        (
            &format!("/ColorSpace {indexed_rgb} /BitsPerComponent 2"),
            &[0b0111_0000],
        ),
        (
            &format!("/ColorSpace {indexed_rgb} /BitsPerComponent 8"),
            &[3, 1],
        ),
        (
            &format!("/ColorSpace {k_only} /BitsPerComponent 8"),
            &[0, 1],
        ),
        ("/ColorSpace /SP /BitsPerComponent 8", &[0, 255]),
        ("/ColorSpace /DN /BitsPerComponent 8", &[0, 0, 255, 255]),
        ("/ColorSpace /DN /BitsPerComponent 4", &[0xFF, 0x00]),
    ];
    let decodes = [
        "",
        "/Decode [1 0]",
        "/Decode [0 0.5]",
        "/Decode [1 0 1 0]",
        "/Decode [1 0 1 0 1 0]",
        "/Decode [1 0 1 0 1 0 1 0]",
        "/Decode [3 0]",
        "/Interpolate true",
    ];
    for (entries, data) in cases {
        for extra in decodes {
            same_either_way(&format!("{entries} {extra}"), data, "", false);
        }
        same_either_way(entries, data, "/TR0 gs", false);
        same_either_way(entries, data, "", true);
        same_either_way(entries, data, "/TR0 gs", true);
    }
}

/// Under a CMYK output intent a DeviceGray image is carried to K, in
/// either form: the case above that changes the samples and the channels
/// painted.
#[test]
fn inline_gray_is_carried_to_k_under_an_output_intent() {
    for form in [Form::Inline, Form::XObject] {
        let pdf = document(
            "/ColorSpace /DeviceGray /BitsPerComponent 8",
            &[200, 55],
            "",
            form,
            true,
        );
        let (samples, params) = image(&pdf);
        assert_eq!(samples, [0, 0, 0, 55, 0, 0, 0, 200], "{form:?}");
        assert!(params.contains("color_space: DeviceCMYK"), "{form:?}");
    }
}

/// The `/Decode` rows of the plan's probe, as Ghostscript and poppler
/// paint them.
#[test]
fn inline_images_honour_decode() {
    assert_eq!(
        samples(
            "/ColorSpace /DeviceGray /BitsPerComponent 8 /Decode [1 0]",
            &[200, 55]
        ),
        [55, 200]
    );
    assert_eq!(
        samples(
            "/ColorSpace /DeviceGray /BitsPerComponent 4 /Decode [1 0]",
            &[0xF0]
        ),
        [0, 255]
    );
    assert_eq!(
        samples(
            "/ColorSpace /DeviceCMYK /BitsPerComponent 8 /Decode [1 0 1 0 1 0 1 0]",
            &[255, 255, 255, 255, 0, 0, 0, 0]
        ),
        [0, 0, 0, 0, 255, 255, 255, 255]
    );
    let indexed = format!(
        "/ColorSpace [/Indexed /DeviceRGB 15 <FF0000{}00FF00>] /BitsPerComponent 4 /Decode [15 0]",
        "000000".repeat(14)
    );
    assert_eq!(samples(&indexed, &[0x0F]), [15, 0]);
}

/// An inline image carries the transfer function in force, as an XObject
/// does, for the renderer to apply to its colour; its samples are left
/// alone.
#[test]
fn inline_images_take_the_transfer_function() {
    let entries = "/ColorSpace /DeviceRGB /BitsPerComponent 8";
    let data = [200, 200, 200, 55, 55, 55];
    let got = same_either_way(entries, &data, "/TR0 gs", false);
    assert_eq!(got, data);
    let (_, params) = image(&document(entries, &data, "/TR0 gs", Form::Inline, false));
    assert!(
        params.contains("transfer: TransferState { gray: Some("),
        "{params}"
    );
}

#[test]
fn inline_images_keep_interpolate() {
    let doc = document(
        "/ColorSpace /DeviceGray /BitsPerComponent 8 /Interpolate true",
        &[0, 255],
        "",
        Form::Inline,
        false,
    );
    assert!(image(&doc).1.contains("interpolate: true"));
}

/// A K-only Indexed CMYK image paints K alone (GWG 3.1), inline too.
#[test]
fn inline_k_only_indexed_paints_k() {
    let doc = document(
        "/ColorSpace [/Indexed /DeviceCMYK 1 <00000000000000FF>] /BitsPerComponent 8",
        &[0, 1],
        "",
        Form::Inline,
        false,
    );
    let k = stet_graphics::device::CMYK_K;
    assert!(image(&doc).1.contains(&format!("painted_channels: {k},")));
}

/// A multi-input DeviceN image is evaluated from its samples expanded and
/// decoded, in either form: `[1 0 1 0]` turns (0, 0) to white, and 4-bit
/// samples are read at 4 bits.
#[test]
fn device_n_images_take_decode_and_depth() {
    let rgba = |pdf: &[u8]| {
        let doc = open(pdf);
        let list = doc.render_page(0, 72.0).unwrap();
        match &list.elements()[0] {
            DisplayElement::Image {
                sample_data,
                params,
            } => {
                assert!(matches!(
                    params.color_space,
                    ImageColorSpace::PreconvertedRGBA
                ));
                sample_data.to_vec()
            }
            other => panic!("an image expected, got {other:?}"),
        }
    };
    for form in [Form::Inline, Form::XObject] {
        let pdf = document(
            "/ColorSpace /DN /BitsPerComponent 8 /Decode [1 0 1 0]",
            &[0, 0, 255, 255],
            "",
            form,
            false,
        );
        assert_eq!(rgba(&pdf), [255, 255, 255, 255, 0, 0, 0, 255], "{form:?}");
        let pdf = document(
            "/ColorSpace /DN /BitsPerComponent 4",
            &[0xFF, 0x00],
            "",
            form,
            false,
        );
        assert_eq!(rgba(&pdf), [255, 255, 255, 255, 0, 0, 0, 255], "{form:?}");
    }
}

/// Abbreviated keys, names and filters still parse.
#[test]
fn inline_abbreviations() {
    let pdf_with = |paint: &str| {
        let content = format!("q 20 0 0 20 0 0 cm {paint} Q");
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 20 20] /Contents 4 0 R >>".to_string(),
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            ),
        ];
        let mut pdf = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
        }
        let xref = pdf.len();
        pdf.extend(b"xref\n0 5\n0000000000 65535 f\r\n");
        for off in offsets {
            pdf.extend(format!("{off:010} 00000 n\r\n").as_bytes());
        }
        pdf.extend(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
        );
        pdf
    };
    let pdf = pdf_with("BI /W 2 /H 1 /CS /G /BPC 8 /D [1 0] /F [/AHx] ID C837> EI");
    assert_eq!(image(&pdf).0, [55, 200]);
    let pdf = pdf_with("BI /W 2 /H 1 /CS [/I /RGB 1 <FF000000FF00>] /BPC 8 /F /AHx ID 0001> EI");
    let (samples, params) = image(&pdf);
    assert_eq!(samples, [0, 1]);
    assert!(params.contains("Indexed"));
}
