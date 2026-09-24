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
use stet_graphics::display_list::{DisplayElement, DisplayList};
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
/// so a Lab colour converts to different CMYK under the two intents.
fn cmyk_output_profile() -> Vec<u8> {
    // Legacy 16-bit Lab: a/b = 0 encodes as 0x8000.
    let ab_zero = 32768.0 / 65535.0;
    let a2b = |coords: &[f64]| {
        let (c, m, y, k) = (coords[0], coords[1], coords[2], coords[3]);
        let l = (1.0 - 0.2 * (c + m + y) - 0.6 * k).max(0.0) * (65280.0 / 65535.0);
        vec![l, ab_zero, ab_zero]
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
        (b"A2B0", lut16(4, 3, a2b)),
        (b"A2B1", lut16(4, 3, a2b)),
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
/// `/Sh0` axial shading of a constant Lab 60 0 0, and `/P0` a shading
/// pattern over it.
fn build_pdf(pages: &[&str]) -> Vec<u8> {
    let profile = cmyk_output_profile();
    let shading = format!(
        "<< /ShadingType 2 /ColorSpace {LAB} /Coords [0 0 10 0] /Extend [true true] \
         /Function << /FunctionType 2 /Domain [0 1] /C0 [60 0 0] /C1 [60 0 0] /N 1 >> >>"
    );
    let first_page = 7;
    let kids: Vec<String> = (0..pages.len())
        .map(|i| format!("{} 0 R", first_page + 2 * i))
        .collect();

    let mut objects: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R /OutputIntents [<< /Type /OutputIntent \
          /S /GTS_PDFX /OutputConditionIdentifier (Test) /DestOutputProfile 3 0 R >>] >>"
            .to_vec(),
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
            "<< /ColorSpace << /Lab0 {LAB} >> /Pattern << /P0 5 0 R >> /Shading << /Sh0 6 0 R >> >>"
        )
        .into_bytes(),
        b"<< /Type /Pattern /PatternType 2 /Shading 6 0 R >>".to_vec(),
        shading.into_bytes(),
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
