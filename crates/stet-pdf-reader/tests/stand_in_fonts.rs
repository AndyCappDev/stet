// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A font the document does not embed is drawn with a stand-in, the first
//! that parses from each source in turn: the caller's `FontProvider`, the
//! fonts installed on the machine, then stet's own. A source whose data is
//! not a Type 1 font is passed over. The machine's copy of Nimbus Sans can
//! be the OpenType one, and the reader took it as the end of the search and
//! drew no text, on whichever machines list the OpenType font folder first.
//!
//! Every document here is read with `set_host_files(false)`, and the
//! provider stands in for a source with the wrong kind of font, so the
//! tests give the same result on any machine.

use std::sync::Arc;

use stet_graphics::display_list::DisplayElement;
use stet_pdf_reader::PdfDocument;

/// A PDF of `objects`, numbered from 1, with a cross-reference table.
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

fn stream(dict: &str, body: &str) -> String {
    format!(
        "<< {dict} /Length {} >>\nstream\n{body}\nendstream",
        body.len()
    )
}

/// One page showing `text` in `/F1`, which is object 5, `font`, with
/// `extra` objects from 6 up.
fn one_page(font: &str, text: &str, extra: &[String]) -> Vec<u8> {
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 100] \
         /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"
            .to_string(),
        stream("", &format!("BT /F1 50 Tf 20 30 Td {text} Tj ET")),
        font.to_string(),
    ];
    objects.extend_from_slice(extra);
    pdf_from(&objects)
}

/// Helvetica, not embedded, showing `(A)`.
fn simple_font() -> Vec<u8> {
    one_page(
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        "(A)",
        &[],
    )
}

/// A composite font over a Helvetica that is not embedded, showing CID 1,
/// which its `/ToUnicode` makes an `A`.
fn composite_font() -> Vec<u8> {
    let descendant = "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Helvetica \
         /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
         /FontDescriptor 8 0 R /DW 667 >>"
        .to_string();
    let descriptor = "<< /Type /FontDescriptor /FontName /Helvetica /Flags 32 \
         /FontBBox [-166 -225 1000 931] /ItalicAngle 0 /Ascent 718 /Descent -207 \
         /CapHeight 718 /StemV 88 >>"
        .to_string();
    let to_unicode = stream(
        "",
        "/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
         /CMapName /Test def\n1 begincodespacerange <0000> <FFFF> endcodespacerange\n\
         1 beginbfchar\n<0001> <0041>\nendbfchar\n\
         endcmap CMapName currentdict /CMap defineresource pop end end",
    );
    one_page(
        "<< /Type /Font /Subtype /Type0 /BaseFont /Helvetica /Encoding /Identity-H \
         /DescendantFonts [6 0 R] /ToUnicode 7 0 R >>",
        "<0001>",
        &[descendant, to_unicode, descriptor],
    )
}

/// The outline of each glyph the page draws, as its number of segments.
fn glyphs(pdf: &[u8], provider_gives_no_font: bool) -> Vec<usize> {
    let mut doc = PdfDocument::from_bytes(pdf).unwrap();
    doc.set_host_files(false);
    if provider_gives_no_font {
        doc.set_font_provider(Arc::new(|_: &str| Some(b"not a font".to_vec())));
    }
    let list = doc.render_page(0, 72.0).unwrap();
    list.elements()
        .iter()
        .filter_map(|e| match e {
            DisplayElement::Fill { path, .. } => Some(path.segments.len()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_stand_in_that_is_not_type_1_is_passed_over() {
    let pdf = simple_font();
    let drawn = glyphs(&pdf, false);
    assert_eq!(drawn.len(), 1, "Helvetica's A: {drawn:?}");
    assert_eq!(glyphs(&pdf, true), drawn);
}

#[test]
fn a_composite_font_passes_over_a_stand_in_that_is_not_type_1() {
    let pdf = composite_font();
    let drawn = glyphs(&pdf, false);
    assert_eq!(drawn.len(), 1, "Helvetica's A: {drawn:?}");
    assert_eq!(glyphs(&pdf, true), drawn);
}
