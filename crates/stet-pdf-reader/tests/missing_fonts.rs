// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A font or a CMap the document uses without carrying it, and that the
//! reader does not carry either, is reported as its own kind,
//! `ParsePhase::MissingFont` or `ParsePhase::MissingCMap`, for the page
//! that uses it. A missing font is drawn with a substitute and its text is
//! still read; so is text in a font whose CMap is missing, through the
//! document's own `/ToUnicode`.
//!
//! Every document here is read with `set_host_files(false)`, so no CMap
//! or font the machine has installed is looked for, and the tests give the
//! same result on any machine.

use stet_graphics::text::text_runs;
use stet_pdf_reader::{
    LayerSet, LocationHint, ParsePhase, ParseWarning, PdfDocument, Severity, TextExtraction,
};

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

/// Two pages that both run `content`, with fonts `/F1` (Helvetica, which
/// the reader carries) and `/F2` (object 7, `font`), and `extra` objects
/// from 8 up.
fn two_pages(font: &str, content: &str, extra: &[String]) -> Vec<u8> {
    let resources = "/Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> \
                     /F2 7 0 R >>";
    let page = |contents: u32| {
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 100] \
             /Resources << {resources} >> /Contents {contents} 0 R >>"
        )
    };
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".to_string(),
        page(4),
        stream("", content),
        page(6),
        stream("", content),
        font.to_string(),
    ];
    objects.extend_from_slice(extra);
    pdf_from(&objects)
}

/// A composite font over a CIDFontType2 that is not embedded (object 8),
/// encoded with the CMap `encoding`. Its `/ToUnicode` (object 10) gives
/// the text of the two GB 2312 codes in [`MIXED`], so that text read from
/// codes guessed to be two bytes long would be found.
fn composite_font(encoding: &str) -> (String, Vec<String>) {
    let font = format!(
        "<< /Type /Font /Subtype /Type0 /BaseFont /SimSun /Encoding /{encoding} \
         /DescendantFonts [8 0 R] /ToUnicode 10 0 R >>"
    );
    let to_unicode = stream(
        "",
        "/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
         /CMapName /Test def\n1 begincodespacerange <0000> <FFFF> endcodespacerange\n\
         2 beginbfchar\n<B0A1> <554A>\n<B0A2> <963F>\nendbfchar\n\
         endcmap CMapName currentdict /CMap defineresource pop end end",
    );
    let descendant = "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /SimSun \
         /CIDSystemInfo << /Registry (Adobe) /Ordering (GB1) /Supplement 2 >> \
         /FontDescriptor 9 0 R /DW 1000 >>"
        .to_string();
    let descriptor = "<< /Type /FontDescriptor /FontName /SimSun /Flags 6 \
         /FontBBox [0 -141 996 855] /ItalicAngle 0 /Ascent 859 /Descent -141 \
         /CapHeight 700 /StemV 80 >>"
        .to_string();
    (font, vec![descendant, descriptor, to_unicode])
}

/// Latin text in Helvetica, then text in `/F2`: two GB 2312 characters
/// (four bytes, each pair a character under GBK-EUC-H) and, under the same
/// CMap, the one-byte codes of "SSN".
const MIXED: &str = "BT /F1 12 Tf 10 60 Td (Latin value) Tj ET \
                     BT /F2 12 Tf 10 30 Td <B0A1B0A2> Tj (SSN) Tj ET";

/// Each page's text, and the document's warnings, after both pages are
/// rendered twice with no file read from the machine.
fn read(pdf: &[u8]) -> (Vec<String>, Vec<ParseWarning>) {
    let (texts, _, warnings) = read_glyphs(pdf);
    (texts, warnings)
}

/// [`read`], with the number of glyphs each page draws.
fn read_glyphs(pdf: &[u8]) -> (Vec<String>, Vec<usize>, Vec<ParseWarning>) {
    let mut doc = PdfDocument::from_bytes(pdf).unwrap();
    doc.set_host_files(false);
    doc.set_text_extraction(TextExtraction::Glyphs);
    let mut texts = Vec::new();
    let mut glyphs = Vec::new();
    for round in 0..2 {
        for page in 0..doc.page_count() {
            let list = doc.render_page(page, 72.0).unwrap();
            let runs = text_runs(&list, &LayerSet::new());
            if round == 0 {
                texts.push(runs.iter().map(|run| run.text.as_str()).collect());
                glyphs.push(runs.iter().map(|run| run.glyphs.len()).sum());
            }
        }
    }
    let warnings = doc.parse_warnings().to_vec();
    (texts, glyphs, warnings)
}

/// The warnings of `phase`, as `(page, severity)`.
fn of_kind(warnings: &[ParseWarning], phase: ParsePhase) -> Vec<(Option<usize>, Severity)> {
    warnings
        .iter()
        .filter(|w| w.phase == phase)
        .map(|w| {
            let page = match w.location {
                Some(LocationHint::Page(page)) => Some(page),
                None => None,
                ref other => panic!("unexpected location {other:?}"),
            };
            (page, w.severity)
        })
        .collect()
}

fn both_pages() -> Vec<(Option<usize>, Severity)> {
    vec![(Some(0), Severity::Warning), (Some(1), Severity::Warning)]
}

/// What the MissingCMap warnings of `warnings` say.
fn cmap_messages(warnings: &[ParseWarning]) -> Vec<&str> {
    warnings
        .iter()
        .filter(|w| w.phase == ParsePhase::MissingCMap)
        .map(|w| w.message.as_str())
        .collect()
}

/// The message a CMap the document names as `name`, and that is not
/// found, is reported with.
fn not_found(name: &str) -> String {
    format!(
        "predefined CMap '{name}' not found; \
         set STET_CMAP_DIR or install poppler-data for CJK support"
    )
}

/// Text in [`MIXED`] read through the font's `/ToUnicode`: the two GB 2312
/// characters come out right; the one-byte codes of "SSN", taken two bytes
/// at a time, have no text there.
const MIXED_TEXT: &str = "Latin value\u{554A}\u{963F}";

/// GBK-EUC-H, a predefined CMap, is not embedded and not found. The text
/// in its font is still drawn and read: the codes are taken two bytes at a
/// time and read through the document's own `/ToUnicode`. Each page
/// reports the CMap as missing, by name and with the remedy, and the font
/// too, since it is not embedded either. Nothing is a content warning.
#[test]
fn a_predefined_cmap_that_is_not_found_is_its_own_kind_and_its_text_is_read() {
    let (font, extra) = composite_font("GBK-EUC-H");
    let pdf = two_pages(&font, MIXED, &extra);
    let (texts, glyphs, warnings) = read_glyphs(&pdf);

    assert_eq!(of_kind(&warnings, ParsePhase::MissingCMap), both_pages());
    assert_eq!(of_kind(&warnings, ParsePhase::MissingFont), both_pages());
    assert_eq!(of_kind(&warnings, ParsePhase::Content), []);
    assert_eq!(texts, [MIXED_TEXT, MIXED_TEXT]);
    let name = not_found("GBK-EUC-H");
    assert_eq!(cmap_messages(&warnings), [name.as_str(), name.as_str()]);

    // The font's codes are drawn: more glyphs than the Helvetica line
    // alone draws.
    let (latin_font, latin_extra) = composite_font("GBK-EUC-H");
    let latin_only = two_pages(
        &latin_font,
        "BT /F1 12 Tf 10 60 Td (Latin value) Tj ET",
        &latin_extra,
    );
    let (_, latin_glyphs, _) = read_glyphs(&latin_only);
    for page in 0..2 {
        assert!(
            glyphs[page] > latin_glyphs[page],
            "page {page}: {} glyphs, {} without the font",
            glyphs[page],
            latin_glyphs[page]
        );
    }
}

/// A UTF-16 CMap that is not found is read the same way: the two
/// characters, in UTF-16 codes, are read, and the CMap is reported.
#[test]
fn a_utf16_cmap_that_is_not_found_is_its_own_kind_and_its_text_is_read() {
    let (font, extra) = composite_font("UniGB-UTF16-H");
    let content = "BT /F1 12 Tf 10 60 Td (Latin value) Tj ET \
                   BT /F2 12 Tf 10 30 Td <554A963F> Tj ET";
    let (texts, warnings) = read(&two_pages(&font, content, &extra));
    assert_eq!(of_kind(&warnings, ParsePhase::MissingCMap), both_pages());
    assert_eq!(of_kind(&warnings, ParsePhase::Content), []);
    assert_eq!(texts, [MIXED_TEXT, MIXED_TEXT]);
    let name = not_found("UniGB-UTF16-H");
    assert_eq!(cmap_messages(&warnings), [name.as_str(), name.as_str()]);
}

/// A UCS-2 CMap's codes are Unicode, two bytes each, so the text in its
/// font is read from them when the CMap is not found. The CMap is still
/// reported as missing, for each page, and nothing is a content warning.
#[test]
fn a_ucs2_cmap_that_is_not_found_is_its_own_kind_and_its_text_is_read() {
    let font = "<< /Type /Font /Subtype /Type0 /BaseFont /SimSun /Encoding /UniGB-UCS2-H \
                /DescendantFonts [8 0 R] >>";
    let (_, extra) = composite_font("UniGB-UCS2-H");
    // "SSN 12", two bytes a character.
    let content = "BT /F1 12 Tf 10 60 Td (Latin value) Tj ET \
                   BT /F2 12 Tf 10 30 Td <00530053004E00200031> Tj <0032> Tj ET";
    let (texts, warnings) = read(&two_pages(font, content, &extra[..2]));

    assert_eq!(of_kind(&warnings, ParsePhase::MissingCMap), both_pages());
    assert_eq!(of_kind(&warnings, ParsePhase::Content), []);
    assert_eq!(texts, ["Latin valueSSN 12", "Latin valueSSN 12"]);
    let name = not_found("UniGB-UCS2-H");
    assert_eq!(cmap_messages(&warnings), [name.as_str(), name.as_str()]);
}

/// A CMap name that no machine has is reported the same way, and its
/// text read the same way.
#[test]
fn a_cmap_no_machine_has_is_its_own_kind() {
    let (font, extra) = composite_font("NoSuchCMap-H");
    let (texts, warnings) = read(&two_pages(&font, MIXED, &extra));
    assert_eq!(of_kind(&warnings, ParsePhase::MissingCMap), both_pages());
    assert_eq!(of_kind(&warnings, ParsePhase::Content), []);
    assert_eq!(texts, [MIXED_TEXT, MIXED_TEXT]);
    let name = not_found("NoSuchCMap-H");
    assert_eq!(cmap_messages(&warnings), [name.as_str(), name.as_str()]);
}

/// Identity-H is the reader's own and is never reported; the font, which
/// is not embedded, still is.
#[test]
fn identity_h_is_never_reported_as_missing() {
    let (font, extra) = composite_font("Identity-H");
    let (_, warnings) = read(&two_pages(
        &font,
        "BT /F2 12 Tf 10 30 Td <00240025> Tj ET",
        &extra,
    ));
    assert_eq!(of_kind(&warnings, ParsePhase::MissingCMap), []);
    assert_eq!(of_kind(&warnings, ParsePhase::MissingFont), both_pages());
    assert_eq!(of_kind(&warnings, ParsePhase::Content), []);
}

/// A simple font that is not embedded is drawn with a substitute, and its
/// text is read. It is reported for each page as a missing font, and not
/// by name.
#[test]
fn a_font_that_is_not_embedded_is_its_own_kind_and_its_text_is_read() {
    let font = "<< /Type /Font /Subtype /TrueType /BaseFont /Arial,Bold \
                /FirstChar 32 /LastChar 126 /Encoding /WinAnsiEncoding \
                /FontDescriptor 8 0 R >>";
    let descriptor = "<< /Type /FontDescriptor /FontName /Arial,Bold /Flags 32 \
                      /FontBBox [-628 -376 2000 1056] /ItalicAngle 0 /Ascent 905 \
                      /Descent -212 /CapHeight 716 /StemV 153 >>"
        .to_string();
    let (texts, warnings) = read(&two_pages(
        font,
        "BT /F2 12 Tf 10 30 Td (Account 1234) Tj ET",
        &[descriptor],
    ));
    assert_eq!(of_kind(&warnings, ParsePhase::MissingFont), both_pages());
    assert_eq!(of_kind(&warnings, ParsePhase::Content), []);
    assert_eq!(texts, ["Account 1234", "Account 1234"]);
    for w in &warnings {
        assert!(!w.message.contains("Arial"), "{w:?}");
    }
}

/// The standard 14 fonts are the reader's own, and a subset tag does not
/// hide one.
#[test]
fn a_standard_14_font_is_never_reported_as_missing() {
    for name in [
        "Times-Roman",
        "ABCDEF+Courier-Bold",
        "Helvetica,Bold",
        "Symbol",
    ] {
        let font = format!("<< /Type /Font /Subtype /Type1 /BaseFont /{name} >>");
        let (_, warnings) = read(&two_pages(&font, "BT /F2 12 Tf 10 30 Td (x) Tj ET", &[]));
        assert!(warnings.is_empty(), "{name}: {warnings:?}");
    }
}

/// A font the document embeds but whose program cannot be read is not a
/// missing font: it is a fault in the file, and a content warning.
#[test]
fn an_embedded_font_that_cannot_be_read_is_a_content_warning() {
    let font = "<< /Type /Font /Subtype /TrueType /BaseFont /Broken \
                /FirstChar 32 /LastChar 126 /Encoding /WinAnsiEncoding \
                /FontDescriptor 8 0 R >>";
    let descriptor = "<< /Type /FontDescriptor /FontName /Broken /Flags 32 \
                      /FontBBox [0 0 1000 1000] /ItalicAngle 0 /Ascent 900 \
                      /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 9 0 R >>"
        .to_string();
    let program = stream("", "this is not a font program");
    let (_, warnings) = read(&two_pages(
        font,
        "BT /F2 12 Tf 10 30 Td (x) Tj ET",
        &[descriptor, program],
    ));
    assert_eq!(of_kind(&warnings, ParsePhase::MissingFont), []);
    assert_eq!(of_kind(&warnings, ParsePhase::Content), both_pages());
}
