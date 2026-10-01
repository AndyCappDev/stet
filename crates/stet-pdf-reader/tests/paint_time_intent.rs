// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The rendering intent applies at paint time, not when a colour is set.
//!
//! PDF applies the intent in effect when a shape is painted. The reader
//! converts colours when they are set, so a content stream that selects its
//! intent *after* its colour (`60 0 0 sc /Perceptual ri … f`) must still paint
//! with that intent. The Ghent Workgroup's GWG 22.1 output-intent test is
//! built exactly that way: its Lab background only matches the CMYK "X" drawn
//! over it under Perceptual, so converting with the intent current at `sc`
//! makes the X visible.
//!
//! The intent only changes a colour when the document has an output intent
//! whose profile carries different tables per intent, so these tests embed a
//! small generated CMYK output profile whose perceptual and colorimetric
//! `B2A` tables disagree.

use stet_graphics::color::DeviceColor;
use stet_graphics::device::ImageColorSpace;
use stet_graphics::display_list::{DisplayElement, DisplayList};
use stet_graphics::icc::{BpcMode, IccCache, IccCacheOptions};
use stet_graphics::rendering_intent;
use stet_pdf_reader::PdfDocument;

// ---------------------------------------------------------------- profile

fn s15f16(v: f64) -> [u8; 4] {
    ((v * 65536.0).round() as i32).to_be_bytes()
}

/// A `lut16Type` (`mft2`) tag on a 2-point grid with identity input and
/// output curves. `clut` maps grid coordinates (each 0.0 or 1.0, first
/// channel slowest) to normalised output values.
fn lut16(n_in: usize, n_out: usize, clut: impl Fn(&[f64]) -> Vec<f64>) -> Vec<u8> {
    const GRID: usize = 2;
    let mut t = b"mft2".to_vec();
    t.extend([0u8; 4]);
    t.extend([n_in as u8, n_out as u8, GRID as u8, 0]);
    for v in [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0] {
        t.extend(s15f16(v));
    }
    t.extend(2u16.to_be_bytes()); // input table entries
    t.extend(2u16.to_be_bytes()); // output table entries
    for _ in 0..n_in {
        t.extend(0u16.to_be_bytes());
        t.extend(u16::MAX.to_be_bytes());
    }
    for idx in 0..GRID.pow(n_in as u32) {
        let mut coords = vec![0.0; n_in];
        let mut rem = idx;
        for c in coords.iter_mut().rev() {
            *c = (rem % GRID) as f64;
            rem /= GRID;
        }
        let out = clut(&coords);
        assert_eq!(out.len(), n_out);
        for v in out {
            t.extend(((v.clamp(0.0, 1.0) * 65535.0).round() as u16).to_be_bytes());
        }
    }
    for _ in 0..n_out {
        t.extend(0u16.to_be_bytes());
        t.extend(u16::MAX.to_be_bytes());
    }
    t
}

/// A CMYK output (`prtr`) ICC v2 profile with a Lab PCS whose Perceptual
/// `B2A0` adds CMY under the black that the colorimetric `B2A1` leaves out,
/// so a Lab colour converts to different CMYK under the two intents. Its
/// perceptual `A2B0` likewise has a lighter black than `A2B1`, so a CMYK
/// colour converts to different sRGB.
fn cmyk_output_profile() -> Vec<u8> {
    // Legacy 16-bit Lab: a/b = 0 encodes as 0x8000.
    let ab_zero = 32768.0 / 65535.0;
    let a2b_with_black = |black: f64| {
        move |coords: &[f64]| {
            let (c, m, y, k) = (coords[0], coords[1], coords[2], coords[3]);
            let l = (1.0 - 0.2 * (c + m + y) - black * k).max(0.0) * (65280.0 / 65535.0);
            vec![l, ab_zero, ab_zero]
        }
    };
    let b2a_colorimetric = |coords: &[f64]| {
        let k = 1.0 - coords[0];
        vec![0.0, 0.0, 0.0, k]
    };
    let b2a_perceptual = |coords: &[f64]| {
        let k = 1.0 - coords[0];
        vec![0.5 * k, 0.5 * k, 0.5 * k, k]
    };

    let mut wtpt = b"XYZ ".to_vec();
    wtpt.extend([0u8; 4]);
    for v in [0.9642, 1.0, 0.8249] {
        wtpt.extend(s15f16(v));
    }
    let tags: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"wtpt", wtpt),
        (b"A2B0", lut16(4, 3, a2b_with_black(0.4))),
        (b"A2B1", lut16(4, 3, a2b_with_black(0.6))),
        (b"B2A0", lut16(3, 4, b2a_perceptual)),
        (b"B2A1", lut16(3, 4, b2a_colorimetric)),
    ];

    let table_len = 4 + 12 * tags.len();
    let mut offset = 128 + table_len;
    let mut table = (tags.len() as u32).to_be_bytes().to_vec();
    let mut data = Vec::new();
    for (sig, body) in &tags {
        table.extend(*sig);
        table.extend((offset as u32).to_be_bytes());
        table.extend((body.len() as u32).to_be_bytes());
        data.extend(body);
        while data.len() % 4 != 0 {
            data.push(0);
        }
        offset = 128 + table_len + data.len();
    }

    let total = 128 + table.len() + data.len();
    let mut header = vec![0u8; 128];
    header[0..4].copy_from_slice(&(total as u32).to_be_bytes());
    header[8..12].copy_from_slice(&0x0210_0000u32.to_be_bytes());
    header[12..16].copy_from_slice(b"prtr");
    header[16..20].copy_from_slice(b"CMYK");
    header[20..24].copy_from_slice(b"Lab ");
    header[36..40].copy_from_slice(b"acsp");
    header[68..72].copy_from_slice(&s15f16(0.9642));
    header[72..76].copy_from_slice(&s15f16(1.0));
    header[76..80].copy_from_slice(&s15f16(0.8249));
    [header, table, data].concat()
}

// ---------------------------------------------------------------- PDF

const LAB: &str = "[/Lab << /WhitePoint [0.9642 1 0.8249] /Range [-128 127 -128 127] >>]";

/// A PDF/X-style document with the generated profile as its output intent
/// and one page per content stream. Resources: `/Lab0` colour space,
/// `/Sh0` axial shading of a constant Lab 60 0 0, `/P0` a shading pattern
/// over it, `/Sh1` an axial shading of a constant DeviceCMYK 0 0 0 1,
/// `/Im0` a 1×1 DeviceCMYK image of 0 0 0 1 with an explicit `/Mask`, and
/// `/OP` an ExtGState turning overprint on.
fn build_pdf(pages: &[&str]) -> Vec<u8> {
    build_pdf_with(pages, true)
}

/// [`build_pdf`], with or without the output intent. Without it the profile
/// is still embedded but unused; give it to the reader as a source profile.
fn build_pdf_with(pages: &[&str], output_intent: bool) -> Vec<u8> {
    let profile = cmyk_output_profile();
    let shading = format!(
        "<< /ShadingType 2 /ColorSpace {LAB} /Coords [0 0 10 0] /Extend [true true] \
         /Function << /FunctionType 2 /Domain [0 1] /C0 [60 0 0] /C1 [60 0 0] /N 1 >> >>"
    );
    let cmyk_shading = "<< /ShadingType 2 /ColorSpace /DeviceCMYK /Coords [0 0 10 0] \
         /Extend [true true] /Function << /FunctionType 2 /Domain [0 1] \
         /C0 [0 0 0 1] /C1 [0 0 0 1] /N 1 >> >>";
    let image = "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceCMYK \
         /BitsPerComponent 8 /Mask 10 0 R /Filter /ASCIIHexDecode /Length 9 >>\nstream\n000000FF>\nendstream";
    let mask = "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ImageMask true \
         /Filter /ASCIIHexDecode /Length 3 >>\nstream\n00>\nendstream";
    let first_page = 11;
    let kids: Vec<String> = (0..pages.len())
        .map(|i| format!("{} 0 R", first_page + 2 * i))
        .collect();

    let catalog: &[u8] = if output_intent {
        b"<< /Type /Catalog /Pages 2 0 R /OutputIntents [<< /Type /OutputIntent \
          /S /GTS_PDFX /OutputConditionIdentifier (Test) /DestOutputProfile 3 0 R >>] >>"
    } else {
        b"<< /Type /Catalog /Pages 2 0 R >>"
    };
    let mut objects: Vec<Vec<u8>> = vec![
        catalog.to_vec(),
        format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>",
            kids.join(" "),
            pages.len()
        )
        .into_bytes(),
        [
            format!("<< /N 4 /Length {} >>\nstream\n", profile.len()).as_bytes(),
            &profile,
            b"\nendstream",
        ]
        .concat(),
        format!(
            "<< /ColorSpace << /Lab0 {LAB} >> /Pattern << /P0 5 0 R >> \
             /Shading << /Sh0 6 0 R /Sh1 7 0 R >> /XObject << /Im0 9 0 R >> \
             /ExtGState << /OP << /OP true /op true /OPM 1 >> >> >>"
        )
        .into_bytes(),
        b"<< /Type /Pattern /PatternType 2 /Shading 6 0 R >>".to_vec(),
        shading.into_bytes(),
        cmyk_shading.as_bytes().to_vec(),
        b"null".to_vec(),
        image.as_bytes().to_vec(),
        mask.as_bytes().to_vec(),
    ];
    for (i, content) in pages.iter().enumerate() {
        objects.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 20 20] /Resources 4 0 R \
                 /Contents {} 0 R >>",
                first_page + 2 * i + 1
            )
            .into_bytes(),
        );
        objects.push(
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            )
            .into_bytes(),
        );
    }

    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend(format!("{} 0 obj\n", i + 1).as_bytes());
        pdf.extend(body);
        pdf.extend(b"\nendobj\n");
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

// ---------------------------------------------------------------- helpers

/// Where the generated CMYK profile comes from.
#[derive(Clone, Copy)]
enum Cmyk {
    /// The document's output intent: a PDF/X document.
    OutputIntent,
    /// A CMYK source profile given to the reader (`--cmyk-profile`); the
    /// document has no output intent.
    Source,
}

/// A document of `pages` with the generated profile used as `cmyk`.
fn document(pages: &[&str], cmyk: Cmyk) -> Vec<u8> {
    build_pdf_with(pages, matches!(cmyk, Cmyk::OutputIntent))
}

fn open(pdf: &[u8], cmyk: Cmyk) -> PdfDocument<'_> {
    match cmyk {
        Cmyk::OutputIntent => {
            let mut doc = PdfDocument::from_bytes(pdf).unwrap();
            assert!(
                doc.apply_output_intent_as_default_cmyk(),
                "the generated output profile must register"
            );
            doc
        }
        Cmyk::Source => {
            let cache = IccCache::new_with_options(IccCacheOptions {
                bpc_mode: BpcMode::On,
                source_cmyk_profile: Some(cmyk_output_profile()),
            });
            PdfDocument::from_bytes_with_icc(pdf, cache).unwrap()
        }
    }
}

/// The parts of a colour the intent can change.
type Key = (u64, u64, u64, Option<(u64, u64, u64, u64)>);

fn key(c: &DeviceColor) -> Key {
    (
        c.r.to_bits(),
        c.g.to_bits(),
        c.b.to_bits(),
        c.native_cmyk
            .map(|(c, m, y, k)| (c.to_bits(), m.to_bits(), y.to_bits(), k.to_bits())),
    )
}

/// First painted colour on a page: a `Fill`'s colour, or an axial
/// shading's first stop, searching inside groups.
fn first_color(list: &DisplayList) -> Option<Key> {
    for e in list.elements() {
        let found = match e {
            DisplayElement::Fill { params, .. } => Some(key(&params.color)),
            DisplayElement::AxialShading { params } => {
                params.color_stops.first().map(|s| key(&s.color))
            }
            DisplayElement::Group { elements, .. } | DisplayElement::OcgGroup { elements, .. } => {
                first_color(elements)
            }
            DisplayElement::SoftMasked { content, .. } => first_color(content),
            _ => None,
        };
        if found.is_some() {
            return found;
        }
    }
    None
}

/// Render each page and return its first painted colour.
fn painted(pages: &[&str]) -> Vec<Key> {
    let pdf = build_pdf(pages);
    let mut doc = PdfDocument::from_bytes(&pdf).unwrap();
    assert!(
        doc.apply_output_intent_as_default_cmyk(),
        "the generated output profile must register"
    );
    (0..pages.len())
        .map(|i| {
            let list = doc.render_page(i, 72.0).unwrap();
            first_color(&list).unwrap_or_else(|| panic!("page {i} painted nothing"))
        })
        .collect()
}

// ---------------------------------------------------------------- tests

#[test]
fn intent_selected_after_the_colour_still_applies() {
    let c = painted(&[
        // 0: GWG 22.1's order: colour, then intent, then paint.
        "/Lab0 cs 60 0 0 sc /Perceptual ri 0 0 10 10 re f",
        // 1: intent first.
        "/Perceptual ri /Lab0 cs 60 0 0 sc 0 0 10 10 re f",
        // 2: the other intent, so the comparison above means something.
        "/RelativeColorimetric ri /Lab0 cs 60 0 0 sc 0 0 10 10 re f",
        // 3: an `ri` inside q/Q is undone by Q, colour included.
        "/Lab0 cs 60 0 0 sc q /Perceptual ri Q 0 0 10 10 re f",
    ]);
    assert_ne!(
        c[1], c[2],
        "the generated profile must make the two intents convert Lab differently"
    );
    assert_eq!(c[0], c[1], "the intent in effect at paint time applies");
    assert_eq!(c[3], c[2], "Q restores the colour along with the intent");
}

#[test]
fn stroke_colour_uses_the_paint_time_intent() {
    let stroke = |content: &str| {
        let pdf = build_pdf(&[content]);
        let mut doc = PdfDocument::from_bytes(&pdf).unwrap();
        assert!(doc.apply_output_intent_as_default_cmyk());
        let list = doc.render_page(0, 72.0).unwrap();
        list.elements()
            .iter()
            .find_map(|e| match e {
                DisplayElement::Stroke { params, .. } => Some(key(&params.color)),
                _ => None,
            })
            .expect("a stroke")
    };
    assert_eq!(
        stroke("/Lab0 CS 60 0 0 SC /Perceptual ri 0 0 m 10 10 l S"),
        stroke("/Perceptual ri /Lab0 CS 60 0 0 SC 0 0 m 10 10 l S"),
    );
}

#[test]
fn shadings_use_the_paint_time_intent() {
    let c = painted(&[
        // 0-2: shading pattern selected before / after the intent.
        "/Pattern cs /P0 scn /Perceptual ri 0 0 10 10 re f",
        "/Perceptual ri /Pattern cs /P0 scn 0 0 10 10 re f",
        "/Pattern cs /P0 scn 0 0 10 10 re f",
        // 3-4: `sh` paints immediately, so it uses the current intent.
        "/Perceptual ri /Sh0 sh",
        "/Sh0 sh",
    ]);
    assert_ne!(c[1], c[2], "the two intents must differ for this profile");
    assert_eq!(c[0], c[1], "a shading pattern follows a later `ri`");
    assert_eq!(c[3], c[1], "`sh` converts with the current intent");
    assert_eq!(c[4], c[2], "with no `ri`, RelativeColorimetric applies");
}

/// [`painted`] for a document using the generated profile as `cmyk`.
fn painted_with(pages: &[&str], cmyk: Cmyk) -> Vec<Key> {
    let pdf = document(pages, cmyk);
    let doc = open(&pdf, cmyk);
    (0..pages.len())
        .map(|i| {
            let list = doc.render_page(i, 72.0).unwrap();
            first_color(&list).unwrap_or_else(|| panic!("page {i} painted nothing"))
        })
        .collect()
}

/// With a CMYK *source* profile (no output intent), DeviceCMYK converts with
/// the intent in effect at paint time.
#[test]
fn device_cmyk_uses_the_paint_time_intent() {
    let c = painted_with(
        &[
            // 0: colour, then intent.
            "0 0 0 1 k /Perceptual ri 0 0 10 10 re f",
            // 1: intent, then colour.
            "/Perceptual ri 0 0 0 1 k 0 0 10 10 re f",
            // 2: no intent: relative colorimetric.
            "0 0 0 1 k 0 0 10 10 re f",
            // 3: Q undoes the intent, colour included.
            "0 0 0 1 k q /Perceptual ri Q 0 0 10 10 re f",
        ],
        Cmyk::Source,
    );
    assert_ne!(
        c[1], c[2],
        "the generated profile must make the two intents convert CMYK differently"
    );
    assert_eq!(c[0], c[1], "the intent in effect at paint time applies");
    assert_eq!(c[3], c[2], "Q restores the colour along with the intent");
}

/// In a PDF/X document DeviceCMYK is the output condition: the intent
/// governs converting into it, not showing it, so every intent shows the
/// same. GWG 22.1 depends on this — its DeviceCMYK X, painted under
/// Perceptual, must match a CMYK blend result of the same values.
#[test]
fn device_cmyk_in_the_output_intent_ignores_the_intent() {
    let c = painted_with(
        &[
            "/Perceptual ri 0 0 0 1 k 0 0 10 10 re f",
            "/Saturation ri 0 0 0 1 k 0 0 10 10 re f",
            "0 0 0 1 k 0 0 10 10 re f",
        ],
        Cmyk::OutputIntent,
    );
    assert_eq!(c[0], c[2]);
    assert_eq!(c[1], c[2]);
}

#[test]
fn device_cmyk_stroke_uses_the_paint_time_intent() {
    let stroke = |content: &str| {
        let pdf = document(&[content], Cmyk::Source);
        let doc = open(&pdf, Cmyk::Source);
        let list = doc.render_page(0, 72.0).unwrap();
        list.elements()
            .iter()
            .find_map(|e| match e {
                DisplayElement::Stroke { params, .. } => Some(key(&params.color)),
                _ => None,
            })
            .expect("a stroke")
    };
    let after = stroke("0 0 0 1 K /Perceptual ri 0 0 m 10 10 l S");
    assert_eq!(after, stroke("/Perceptual ri 0 0 0 1 K 0 0 m 10 10 l S"));
    assert_ne!(after, stroke("0 0 0 1 K 0 0 m 10 10 l S"));
}

/// A DeviceCMYK shading converts its stops with the current intent and
/// records it, for the renderer's own conversions.
#[test]
fn device_cmyk_shadings_use_and_record_the_intent() {
    let shading = |content: &str| {
        let pdf = document(&[content], Cmyk::Source);
        let doc = open(&pdf, Cmyk::Source);
        let list = doc.render_page(0, 72.0).unwrap();
        list.elements()
            .iter()
            .find_map(|e| match e {
                DisplayElement::AxialShading { params } => {
                    Some((key(&params.color_stops[0].color), params.rendering_intent))
                }
                _ => None,
            })
            .expect("an axial shading")
    };
    let (perceptual, intent) = shading("/Perceptual ri /Sh1 sh");
    assert_eq!(intent, rendering_intent::PERCEPTUAL);
    let (relcol, intent) = shading("/Sh1 sh");
    assert_eq!(intent, rendering_intent::RELATIVE_COLORIMETRIC);
    assert_ne!(perceptual, relcol);
}

/// The first image's `rendering_intent`, and its first pixel when the
/// reader converted it already.
fn first_image(content: &str, cmyk: Cmyk) -> (u8, Option<[u8; 3]>) {
    let pdf = document(&[content], cmyk);
    let doc = open(&pdf, cmyk);
    let list = doc.render_page(0, 72.0).unwrap();
    list.elements()
        .iter()
        .find_map(|e| match e {
            DisplayElement::Image {
                sample_data,
                params,
                ..
            } => Some((
                params.rendering_intent,
                matches!(params.color_space, ImageColorSpace::PreconvertedRGBA)
                    .then(|| [sample_data[0], sample_data[1], sample_data[2]]),
            )),
            _ => None,
        })
        .expect("an image")
}

/// Inline images take the gstate intent, or their own `/Intent`, as image
/// XObjects do.
#[test]
fn inline_images_carry_their_intent() {
    let inline = |pre: &str, dict: &str| {
        first_image(
            &format!(
                "{pre} q 10 0 0 10 0 0 cm BI /W 1 /H 1 /CS /DeviceCMYK /BPC 8 /F /AHx {dict} \
                 ID 000000FF> EI Q"
            ),
            Cmyk::OutputIntent,
        )
        .0
    };
    assert_eq!(inline("", ""), rendering_intent::RELATIVE_COLORIMETRIC);
    assert_eq!(inline("/Perceptual ri", ""), rendering_intent::PERCEPTUAL);
    assert_eq!(
        inline("/Perceptual ri", "/Intent /Saturation"),
        rendering_intent::SATURATION
    );
}

/// The first fill's colour as sRGB bytes.
fn fill_rgb(content: &str, cmyk: Cmyk) -> [u8; 3] {
    let pdf = document(&[content], cmyk);
    let doc = open(&pdf, cmyk);
    let list = doc.render_page(0, 72.0).unwrap();
    let c = list
        .elements()
        .iter()
        .find_map(|e| match e {
            DisplayElement::Fill { params, .. } => Some(params.color.clone()),
            _ => None,
        })
        .expect("a fill");
    [c.r, c.g, c.b].map(|v| (v * 255.0).round() as u8)
}

fn assert_close(got: [u8; 3], want: [u8; 3], what: &str) {
    for ch in 0..3 {
        assert!(
            got[ch].abs_diff(want[ch]) <= 1,
            "{what}: {got:?}, want {want:?}"
        );
    }
}

/// An image with an explicit `/Mask` is converted by the reader, and must
/// show the colour a DeviceCMYK fill of the same CMYK does, under either
/// intent and either kind of profile.
#[test]
fn masked_images_match_device_cmyk_fills() {
    for cmyk in [Cmyk::Source, Cmyk::OutputIntent] {
        for ri in ["/Perceptual ri", ""] {
            let (_, pixel) = first_image(&format!("{ri} q 10 0 0 10 0 0 cm /Im0 Do Q"), cmyk);
            let pixel = pixel.expect("a masked image is converted by the reader");
            let fill = fill_rgb(&format!("{ri} 0 0 0 1 k 0 0 10 10 re f"), cmyk);
            assert_close(pixel, fill, &format!("{ri:?}: masked image vs fill"));
        }
    }
    // And with a source profile, the two intents differ.
    let (_, perceptual) = first_image("/Perceptual ri q 10 0 0 10 0 0 cm /Im0 Do Q", Cmyk::Source);
    let (_, relcol) = first_image("q 10 0 0 10 0 0 cm /Im0 Do Q", Cmyk::Source);
    assert_ne!(perceptual, relcol);
}

/// The centre pixel of page 1 rendered at 72 dpi.
#[cfg(feature = "render")]
fn centre(content: &str, cmyk: Cmyk) -> [u8; 3] {
    let pdf = document(&[content], cmyk);
    let doc = open(&pdf, cmyk);
    let (rgba, w, _) = doc.render_page_to_rgba(0, 72.0).unwrap();
    let at = (10 * w as usize + 5) * 4;
    [rgba[at], rgba[at + 1], rgba[at + 2]]
}

/// The renderer converts DeviceCMYK image samples itself, with the image's
/// intent: an image of 0 0 0 1 renders the colour a DeviceCMYK fill of
/// 0 0 0 1 does under the same intent.
#[cfg(feature = "render")]
#[test]
fn rendered_device_cmyk_images_match_fills() {
    let image = |ri: &str, cmyk| {
        centre(
            &format!(
                "{ri} q 20 0 0 20 0 0 cm BI /W 1 /H 1 /CS /DeviceCMYK /BPC 8 /F /AHx \
                 ID 000000FF> EI Q"
            ),
            cmyk,
        )
    };
    let fill = |ri: &str, cmyk| centre(&format!("{ri} 0 0 0 1 k 0 0 20 20 re f"), cmyk);
    for cmyk in [Cmyk::Source, Cmyk::OutputIntent] {
        for ri in ["/Perceptual ri", ""] {
            assert_close(
                image(ri, cmyk),
                fill(ri, cmyk),
                &format!("{ri:?}: image vs fill"),
            );
        }
    }
    assert_ne!(
        image("/Perceptual ri", Cmyk::Source),
        image("", Cmyk::Source),
        "with a source profile the two intents must render differently"
    );
}

/// Overprint composites CMYK and converts the result again, with the
/// painting element's intent.
#[cfg(feature = "render")]
#[test]
fn overprint_uses_the_intent() {
    let cmyk = Cmyk::Source;
    // K over K, overprinted: the composite is 0 0 0 1 again.
    let overprinted = centre(
        "/Perceptual ri 0 0 0 1 k 0 0 20 20 re f /OP gs 0 0 0 1 k 0 0 20 20 re f",
        cmyk,
    );
    let plain = centre("/Perceptual ri 0 0 0 1 k 0 0 20 20 re f", cmyk);
    assert_ne!(plain, centre("0 0 0 1 k 0 0 20 20 re f", cmyk));
    assert_close(overprinted, plain, "overprinted vs plain");
}
