// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Text extraction from PDF: `PdfDocument::set_text_extraction`.
//!
//! The switch only adds `DisplayElement::TextRun` elements. Everything else
//! in the display list — and so everything rendered or written to PDF — must
//! be identical with it on and off, and with it off no `TextRun` may appear
//! at all.

use stet_graphics::device::{TextRunParams, UnicodeSource};
use stet_graphics::display_list::{DisplayElement, DisplayList};
use stet_graphics::text::{TextLine, text_lines, text_runs};
use stet_pdf_reader::{LayerSet, PdfDocument, TextExtraction};

/// One page showing text every way a content stream can, and in the places
/// text can hide: rotated, invisible, in a Type 3 font, in a form XObject,
/// in an optional-content layer, and under a soft mask — plus text that is
/// not the document's: inside a Type 3 glyph procedure, a tiling pattern
/// cell and a soft-mask group.
const CONTENT: &str = "\
BT /F1 12 Tf 72 700 Td (Tj) Tj ET
BT /F1 12 Tf 72 680 Td [(T) 120 (J)] TJ ET
BT /F1 12 Tf 14 TL 72 660 Td (quote) ' ET
BT /F1 12 Tf 14 TL 72 640 Td 1 2 (dquote) \" ET
BT /F1 12 Tf 0.866 0.5 -0.5 0.866 72 600 Tm (rotated) Tj ET
q BT /F1 12 Tf 3 Tr 72 560 Td (invisible) Tj ET Q
BT /F2 12 Tf 72 540 Td (AAB) Tj ET
BT /F3 12 Tf 72 520 Td (ABCD) Tj ET
q 1 0 0 1 72 500 cm /Fm1 Do Q
/OC /L1 BDC BT /F1 12 Tf 72 460 Td (layer) Tj ET EMC
q /Pattern cs /P1 scn 72 400 200 40 re f Q
q /GS1 gs BT /F1 12 Tf 72 360 Td (masked) Tj ET Q
q /GS1 gs BT /F1 12 Tf 3 Tr 72 340 Td (ocr) Tj ET Q
BT /F4 12 Tf 72 320 Td (Pa) Tj ET
";

/// A PDF of `objects`, numbered from 1, with a cross-reference table.
fn pdf_from(objects: &[Vec<u8>]) -> Vec<u8> {
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

/// A stream object with `dict` entries and `data`.
fn stream(dict: &str, data: impl AsRef<[u8]>) -> Vec<u8> {
    let data = data.as_ref();
    let mut out = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    out.extend(data);
    out.extend(b"\nendstream");
    out
}

/// A ToUnicode CMap mapping each `(code, destination)` pair, both hex.
fn to_unicode_cmap(entries: &[(&str, &str)]) -> String {
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
         /CMapName /Test def\n1 begincodespacerange <00> <FF> endcodespacerange\n",
    );
    cmap.push_str(&format!("{} beginbfchar\n", entries.len()));
    for (code, dst) in entries {
        cmap.push_str(&format!("<{code}> <{dst}>\n"));
    }
    cmap.push_str("endbfchar\nendcmap CMapName currentdict /CMap defineresource pop end end");
    cmap
}

fn build_pdf() -> Vec<u8> {
    let helvetica =
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>";
    let objects: Vec<Vec<u8>> = vec![
        // 1: catalog, with the layer in /OCProperties.
        "<< /Type /Catalog /Pages 2 0 R \
         /OCProperties << /OCGs [7 0 R] /D << /Order [7 0 R] >> >> >>"
            .into(),
        // 2: pages
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        // 3: page
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
         /Resources << /Font << /F1 5 0 R /F2 6 0 R /F3 10 0 R /F4 16 0 R >> \
         /XObject << /Fm1 8 0 R >> /Properties << /L1 7 0 R >> \
         /Pattern << /P1 12 0 R >> /ExtGState << /GS1 13 0 R >> >> >>"
            .into(),
        // 4: content
        stream("", CONTENT),
        // 5: a standard-14 font
        helvetica.into(),
        // 6: a Type 3 font. Glyph /A is a square; glyph /B shows text of
        // its own, which is part of drawing the glyph, not document text.
        "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
         /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << /A 9 0 R /B 11 0 R >> \
         /Encoding << /Type /Encoding /Differences [65 /A /B] >> \
         /Resources << /Font << /F1 5 0 R >> >> \
         /FirstChar 65 /LastChar 66 /Widths [1000 1000] >>"
            .into(),
        // 7: the layer
        "<< /Type /OCG /Name (Layer) >>".into(),
        // 8: form XObject showing text
        stream(
            "/Type /XObject /Subtype /Form /BBox [0 0 200 20] \
             /Resources << /Font << /F1 5 0 R >> >>",
            "BT /F1 12 Tf 0 5 Td (form) Tj ET",
        ),
        // 9: Type 3 glyph /A
        stream("", "1000 0 0 0 1000 1000 d1 0 0 1000 1000 re f"),
        // 10: Helvetica with a ToUnicode CMap that overrides the glyph
        // names for A, B and C, and leaves D to its name.
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
         /Encoding /WinAnsiEncoding /ToUnicode 14 0 R >>"
            .into(),
        // 11: Type 3 glyph /B
        stream("", "1000 0 d0 BT /F1 500 Tf 0 100 Td (hidden) Tj ET"),
        // 12: a tiling pattern whose cell shows text
        stream(
            "/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 \
             /BBox [0 0 50 20] /XStep 50 /YStep 20 \
             /Resources << /Font << /F1 5 0 R >> >>",
            "BT /F1 8 Tf 0 5 Td (cell) Tj ET",
        ),
        // 13: a soft mask whose group shows text
        "<< /Type /ExtGState /SMask << /Type /Mask /S /Luminosity /G 15 0 R >> >>".into(),
        // 14: the ToUnicode CMap: A → Ω, B → "fi" (two characters),
        // C → 𠮷 U+20BB7 (a surrogate pair).
        stream(
            "",
            to_unicode_cmap(&[("41", "03A9"), ("42", "00660069"), ("43", "D842DFB7")]),
        ),
        // 15: the soft-mask group
        stream(
            "/Type /XObject /Subtype /Form /BBox [0 0 612 792] \
             /Group << /S /Transparency /CS /DeviceGray >> \
             /Resources << /Font << /F1 5 0 R >> >>",
            "1 g 0 0 612 792 re f 0 g BT /F1 12 Tf 72 300 Td (mask) Tj ET",
        ),
        // 16: a Type 3 font named the way dvips names bitmap fonts' glyphs:
        // `a` and the character code.
        format!(
            "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
             /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << /a80 9 0 R /a97 9 0 R >> \
             /Encoding << /Type /Encoding /Differences [80 /a80 97 /a97] >> \
             /FirstChar 80 /LastChar 97 /Widths [{}] >>",
            ["1000"; 18].join(" ")
        )
        .into(),
    ];
    pdf_from(&objects)
}

fn render(pdf: &[u8], level: TextExtraction) -> DisplayList {
    let mut doc = PdfDocument::from_bytes(pdf).expect("fixture parses");
    doc.set_text_extraction(level);
    doc.render_page(0, 72.0).expect("fixture renders")
}

/// `list` with every `TextRun` removed, at every depth — including
/// transparency groups, soft masks, layers and pattern cells.
fn without_text_runs(list: &DisplayList) -> DisplayList {
    let mut out = list.clone();
    out.clear();
    for element in list.elements() {
        let kept = match element {
            DisplayElement::TextRun { .. } => continue,
            DisplayElement::Group { elements, params } => DisplayElement::Group {
                elements: without_text_runs(elements),
                params: params.clone(),
            },
            DisplayElement::OcgGroup {
                elements,
                visibility,
            } => DisplayElement::OcgGroup {
                elements: without_text_runs(elements),
                visibility: visibility.clone(),
            },
            DisplayElement::SoftMasked {
                mask,
                content,
                params,
                mask_cache,
            } => DisplayElement::SoftMasked {
                mask: without_text_runs(mask),
                content: without_text_runs(content),
                params: params.clone(),
                mask_cache: mask_cache.clone(),
            },
            DisplayElement::PatternFill { params } => {
                let mut params = params.clone();
                params.tile = without_text_runs(&params.tile);
                DisplayElement::PatternFill { params }
            }
            other => other.clone(),
        };
        out.push(kept);
    }
    out
}

#[test]
fn switch_changes_nothing_but_text_runs() {
    let pdf = build_pdf();
    let off = render(&pdf, TextExtraction::Off);
    // With the switch off, not a single TextRun at any depth.
    assert_eq!(format!("{off:?}"), format!("{:?}", without_text_runs(&off)));
    for level in [TextExtraction::Runs, TextExtraction::Glyphs] {
        let on = render(&pdf, level);
        // With it on, removing the TextRuns gives back the list exactly —
        // including the soft-mask scope that holds nothing but invisible
        // text, which must not become a `SoftMasked` for the run alone.
        assert_eq!(format!("{off:?}"), format!("{:?}", without_text_runs(&on)));
        assert!(!runs(&on).is_empty());
    }
}

#[test]
fn runs_level_records_the_same_runs_without_glyphs() {
    for pdf in [build_pdf(), build_cid_pdf(), build_actual_text_pdf()] {
        let glyphs = runs(&render(&pdf, TextExtraction::Glyphs));
        let light = runs(&render(&pdf, TextExtraction::Runs));
        assert_eq!(glyphs.len(), light.len());
        for ((path, full), (light_path, light)) in glyphs.iter().zip(&light) {
            assert_eq!(path, light_path);
            assert!(light.glyphs.is_empty());
            let without_glyphs = TextRunParams {
                glyphs: Vec::new(),
                ..full.clone()
            };
            assert_eq!(format!("{without_glyphs:?}"), format!("{light:?}"));
        }
    }
}

/// Recursive element count.
fn count(list: &DisplayList, f: &dyn Fn(&DisplayElement) -> bool) -> usize {
    list.elements()
        .iter()
        .map(|e| {
            let nested = match e {
                DisplayElement::Group { elements, .. }
                | DisplayElement::OcgGroup { elements, .. } => count(elements, f),
                DisplayElement::SoftMasked { mask, content, .. } => {
                    count(mask, f) + count(content, f)
                }
                DisplayElement::PatternFill { params } => count(&params.tile, f),
                _ => 0,
            };
            nested + usize::from(f(e))
        })
        .sum()
}

#[test]
fn fixture_draws_every_text_construct() {
    // Guards the tests here against a fixture that silently stopped
    // drawing: each visible string paints glyph fills, and the layer,
    // pattern and soft mask arrive as their own elements.
    let list = render(&build_pdf(), TextExtraction::Off);
    let fills = count(&list, &|e| matches!(e, DisplayElement::Fill { .. }));
    let layers = count(&list, &|e| matches!(e, DisplayElement::OcgGroup { .. }));
    let patterns = count(&list, &|e| matches!(e, DisplayElement::PatternFill { .. }));
    let masks = count(&list, &|e| matches!(e, DisplayElement::SoftMasked { .. }));
    assert!(fills >= 50, "only {fills} fills");
    assert_eq!(layers, 1);
    assert_eq!(patterns, 1);
    // `masked` gets one; the scope holding only invisible `ocr` has
    // nothing to mask.
    assert_eq!(masks, 1);
}

/// Every run in `list`, in order, with the containers it sits in.
fn runs(list: &DisplayList) -> Vec<(Vec<&'static str>, TextRunParams)> {
    fn walk(
        list: &DisplayList,
        path: &mut Vec<&'static str>,
        out: &mut Vec<(Vec<&'static str>, TextRunParams)>,
    ) {
        for e in list.elements() {
            match e {
                DisplayElement::TextRun { params } => out.push((path.clone(), params.clone())),
                DisplayElement::Group { elements, .. } => {
                    path.push("group");
                    walk(elements, path, out);
                    path.pop();
                }
                DisplayElement::OcgGroup { elements, .. } => {
                    path.push("layer");
                    walk(elements, path, out);
                    path.pop();
                }
                DisplayElement::SoftMasked { mask, content, .. } => {
                    path.push("mask");
                    walk(mask, path, out);
                    path.pop();
                    path.push("masked");
                    walk(content, path, out);
                    path.pop();
                }
                DisplayElement::PatternFill { params } => {
                    path.push("pattern");
                    walk(&params.tile, path, out);
                    path.pop();
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(list, &mut Vec::new(), &mut out);
    out
}

/// The text of each glyph of `run`.
fn glyph_texts(run: &TextRunParams) -> Vec<&str> {
    run.glyphs
        .iter()
        .map(|g| &run.text[g.text_range.start as usize..g.text_range.end as usize])
        .collect()
}

fn close(a: (f64, f64), b: (f64, f64)) -> bool {
    (a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6
}

#[test]
fn records_the_document_text_and_nothing_else() {
    let list = render(&build_pdf(), TextExtraction::Glyphs);
    let runs = runs(&list);
    let texts: Vec<(&str, Vec<&str>)> = runs
        .iter()
        .map(|(path, run)| (run.text.as_str(), path.clone()))
        .collect();
    // A run per stretch of text, in content order, nested like the
    // content. Nothing from the Type 3 glyph procedure (`hidden`), the
    // pattern cell (`cell`) or the soft-mask group (`mask`).
    assert_eq!(
        texts,
        vec![
            ("Tj", vec![]),
            ("TJ", vec![]),
            ("quote", vec![]),
            ("dquote", vec![]),
            ("rotated", vec![]),
            ("invisible", vec![]),
            ("AAB", vec![]),
            ("Ωfi𠮷D", vec![]),
            ("form", vec![]),
            ("layer", vec!["layer"]),
            ("masked", vec!["masked"]),
            ("ocr", vec![]),
            ("Pa", vec![]),
        ]
    );
    for (_, run) in &runs {
        let invisible = matches!(run.text.as_str(), "invisible" | "ocr");
        assert_eq!(run.invisible, invisible, "{}", run.text);
        assert!(!run.vertical);
    }
}

#[test]
fn unicode_sources() {
    let list = render(&build_pdf(), TextExtraction::Glyphs);
    let runs = runs(&list);
    let run = |text: &str| {
        runs.iter()
            .map(|(_, r)| r)
            .find(|r| r.text == text)
            .unwrap_or_else(|| panic!("no run {text:?}"))
    };

    // WinAnsi names through the AGL.
    let tj = run("Tj");
    assert!(
        tj.glyphs
            .iter()
            .all(|g| g.source == UnicodeSource::GlyphName)
    );
    assert_eq!(tj.font_name, "Helvetica");

    // A Type 3 font's /Differences names.
    let type3 = run("AAB");
    assert_eq!(glyph_texts(type3), ["A", "A", "B"]);
    assert!(
        type3
            .glyphs
            .iter()
            .all(|g| g.source == UnicodeSource::GlyphName)
    );

    // Glyph names outside the AGL that spell a code, as dvips writes them.
    let dvips = run("Pa");
    assert!(
        dvips
            .glyphs
            .iter()
            .all(|g| g.source == UnicodeSource::GlyphName)
    );

    // ToUnicode wins over the glyph name, keeps multi-character and
    // supplementary-plane text whole, and leaves unmapped codes to the name.
    let mapped = run("Ωfi𠮷D");
    assert_eq!(glyph_texts(mapped), ["Ω", "fi", "𠮷", "D"]);
    let sources: Vec<_> = mapped.glyphs.iter().map(|g| g.source).collect();
    assert_eq!(
        sources,
        [
            UnicodeSource::ToUnicode,
            UnicodeSource::ToUnicode,
            UnicodeSource::ToUnicode,
            UnicodeSource::GlyphName
        ]
    );
    let codes: Vec<u32> = mapped.glyphs.iter().map(|g| g.code).collect();
    assert_eq!(codes, [0x41, 0x42, 0x43, 0x44]);
}

#[test]
fn glyph_positions_are_in_device_space() {
    // Rendered at 72 dpi, device space is PDF space with y flipped.
    let list = render(&build_pdf(), TextExtraction::Glyphs);
    let runs = runs(&list);
    let run = |text: &str| &runs.iter().find(|(_, r)| r.text == text).unwrap().1;

    // Helvetica T is 611 units wide, j 222; at 12 pt.
    let tj = run("Tj");
    assert!(close(tj.glyphs[0].origin, (72.0, 92.0)));
    assert!(close(tj.glyphs[0].advance, (611.0 * 0.012, 0.0)));
    assert!(close(tj.glyphs[1].origin, (72.0 + 611.0 * 0.012, 92.0)));
    // glyph_to_device: 1000-unit glyph space at 12 pt, y flipped, starting
    // at the first origin.
    let m = tj.glyph_to_device;
    assert!(close((m.a, m.b), (0.012, 0.0)));
    assert!(close((m.c, m.d), (0.0, -0.012)));
    assert!(close((m.tx, m.ty), (72.0, 92.0)));
    // A standard-14 font with no descriptor: the default metrics.
    assert_eq!((tj.ascent, tj.descent), (800.0, -200.0));

    // TJ: the kerning moves J but is not part of T's advance.
    let tj_array = run("TJ");
    assert!(close(tj_array.glyphs[0].advance, (611.0 * 0.012, 0.0)));
    let j_x = 72.0 + (611.0 - 120.0) * 0.012;
    assert!(close(tj_array.glyphs[1].origin, (j_x, 112.0)));

    // Rotated 30°: the advance and the box's up vector turn with the text.
    let rotated = run("rotated");
    let (ax, ay) = rotated.glyphs[0].advance;
    let angle = (-ay).atan2(ax).to_degrees();
    assert!((angle - 30.0).abs() < 0.1, "advance at {angle}°");
    let (ux, uy) = rotated.glyph_to_device.transform_delta(0.0, 1000.0);
    let up = (-uy).atan2(ux).to_degrees();
    assert!((up - 120.0).abs() < 0.1, "up at {up}°");

    // The form's run is placed by the form's CTM.
    let form = run("form");
    assert!(close(form.glyphs[0].origin, (72.0, 792.0 - 505.0)));

    // Type 3 glyph space is the font's own: 1000 units at 12 pt again,
    // with ascent and descent from the /FontBBox. The `"` line's character
    // spacing of 2 is still in force (text state outlives ET): it moves
    // the next glyph but is not part of the advance.
    let type3 = run("AAB");
    assert!(close(type3.glyphs[0].advance, (12.0, 0.0)));
    assert!(close(type3.glyphs[1].origin, (72.0 + 12.0 + 2.0, 252.0)));
    assert_eq!((type3.ascent, type3.descent), (1000.0, 0.0));
}

/// A page showing CIDs 3851 and 3852 of Adobe-Japan1 (諭, 輸) through
/// `Identity-H` and again through `Identity-V`, from a font with no
/// ToUnicode CMap. The descendant embeds a bundled Type 1 font, which the
/// reader accepts as a CIDFontType0 program, so the font resolves the same
/// on every machine; it has no glyphs for these CIDs, and only the text
/// matters here.
fn build_cid_pdf() -> Vec<u8> {
    let program = include_bytes!("../fonts/NimbusSans-Regular.t1");
    let type0 = |encoding: &str| -> Vec<u8> {
        format!(
            "<< /Type /Font /Subtype /Type0 /BaseFont /Test /Encoding /{encoding} \
             /DescendantFonts [6 0 R] >>"
        )
        .into()
    };
    let objects: Vec<Vec<u8>> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
         /Resources << /Font << /F1 5 0 R /F2 7 0 R >> >> >>"
            .into(),
        stream(
            "",
            "BT /F1 12 Tf 72 700 Td <0F0B0F0C> Tj ET \
             BT /F2 12 Tf 300 700 Td <0F0B0F0C> Tj ET",
        ),
        type0("Identity-H"),
        "<< /Type /Font /Subtype /CIDFontType0 /BaseFont /Test \
         /CIDSystemInfo << /Registry (Adobe) /Ordering (Japan1) /Supplement 4 >> \
         /FontDescriptor 8 0 R /DW 1000 >>"
            .into(),
        type0("Identity-V"),
        "<< /Type /FontDescriptor /FontName /Test /Flags 4 /ItalicAngle 0 \
         /Ascent 880 /Descent -120 /CapHeight 700 /StemV 80 \
         /FontBBox [0 -120 1000 880] /FontFile 9 0 R >>"
            .into(),
        stream("", program),
    ];
    pdf_from(&objects)
}

#[test]
fn cid_text_from_the_collection() {
    let pdf = build_cid_pdf();
    let off = render(&pdf, TextExtraction::Off);
    let on = render(&pdf, TextExtraction::Glyphs);
    assert_eq!(format!("{off:?}"), format!("{:?}", without_text_runs(&on)));

    let runs = runs(&on);
    assert_eq!(runs.len(), 2);
    let (horizontal, vertical) = (&runs[0].1, &runs[1].1);
    for run in [horizontal, vertical] {
        assert_eq!(glyph_texts(run), ["諭", "輸"]);
        let codes: Vec<u32> = run.glyphs.iter().map(|g| g.code).collect();
        assert_eq!(codes, [3851, 3852]);
        assert!(
            run.glyphs
                .iter()
                .all(|g| g.source == UnicodeSource::CidOrdering)
        );
    }

    // Horizontal: descriptor metrics, advancing right.
    assert!(!horizontal.vertical);
    assert_eq!((horizontal.ascent, horizontal.descent), (880.0, -120.0));
    assert!(close(horizontal.glyphs[1].origin, (84.0, 92.0)));

    // Vertical: origins run down the column (y grows down in device
    // space), and the cross-column extent is half the em either way.
    assert!(vertical.vertical);
    assert_eq!((vertical.ascent, vertical.descent), (500.0, -500.0));
    assert!(close(vertical.glyphs[0].origin, (300.0, 92.0)));
    assert!(close(vertical.glyphs[0].advance, (0.0, 12.0)));
    assert!(close(vertical.glyphs[1].origin, (300.0, 104.0)));
    let across = vertical
        .glyph_to_device
        .transform_delta(vertical.ascent, 0.0);
    assert!(close(across, (6.0, 0.0)));
}

/// `/ActualText` spans: the forms they take, where they reach, and where
/// they stop.
const ACTUAL_TEXT_CONTENT: &str = "\
/Span << /ActualText <FEFF00660066> >> BDC BT /F1 12 Tf 72 700 Td (X) Tj ET EMC
/Span << /ActualText (xyz) >> BDC BT /F1 12 Tf 72 680 Td (ab) Tj ET BT /F1 12 Tf 100 680 Td (c) Tj ET EMC
BT /F1 12 Tf 72 660 Td (hy) Tj /Span << /ActualText () >> BDC (-) Tj EMC ET
/Span /P1 BDC BT /F1 12 Tf 72 640 Td (N) Tj ET EMC
/Span << /ActualText (outer) >> BDC BT /F1 12 Tf 72 620 Td /Span << /ActualText (inner) >> BDC (q) Tj EMC (r) Tj ET EMC
/Span << /ActualText (viaform) >> BDC q 1 0 0 1 72 600 cm /Fm1 Do Q BT /F1 12 Tf 150 600 Td (s) Tj ET EMC
q 1 0 0 1 72 580 cm /Fm2 Do Q BT /F1 12 Tf 150 580 Td (after) Tj ET
/Span << /ActualText (nothing) >> BDC 72 560 m 100 560 l S EMC BT /F1 12 Tf 72 540 Td (plain) Tj ET
/Artifact BMC BT /F1 12 Tf 72 520 Td (bmc) Tj ET EMC
";

fn build_actual_text_pdf() -> Vec<u8> {
    let objects: Vec<Vec<u8>> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
         /Resources << /Font << /F1 5 0 R >> /XObject << /Fm1 6 0 R /Fm2 7 0 R >> \
         /Properties << /P1 8 0 R >> >> >>"
            .into(),
        stream("", ACTUAL_TEXT_CONTENT),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".into(),
        // 6: a form drawn inside a span: its glyphs are the span's.
        stream(
            "/Type /XObject /Subtype /Form /BBox [0 0 200 20] \
             /Resources << /Font << /F1 5 0 R >> >>",
            "BT /F1 12 Tf 0 5 Td (form) Tj ET",
        ),
        // 7: a form that opens a span and never closes it.
        stream(
            "/Type /XObject /Subtype /Form /BBox [0 0 200 20] \
             /Resources << /Font << /F1 5 0 R >> >>",
            "/Span << /ActualText (leak) >> BDC BT /F1 12 Tf 0 5 Td (unclosed) Tj ET",
        ),
        // 8: named properties
        "<< /ActualText (named) >>".into(),
    ];
    pdf_from(&objects)
}

#[test]
fn actual_text_replaces_the_text_of_its_span() {
    let pdf = build_actual_text_pdf();
    let off = render(&pdf, TextExtraction::Off);
    let on = render(&pdf, TextExtraction::Glyphs);
    assert_eq!(format!("{off:?}"), format!("{:?}", without_text_runs(&on)));

    let runs: Vec<TextRunParams> = runs(&on).into_iter().map(|(_, r)| r).collect();
    let summary: Vec<(&str, Vec<&str>)> = runs
        .iter()
        .map(|r| (r.text.as_str(), glyph_texts(r)))
        .collect();
    assert_eq!(
        summary,
        vec![
            // UTF-16 with a byte-order mark.
            ("ff", vec!["ff"]),
            // Across two text objects on one baseline: the first glyph
            // takes the whole text, every later one in the span takes
            // none, and the gap between them is no word break.
            ("xyz", vec!["xyz", "", ""]),
            // An empty ActualText says the glyph is not text: a line-end
            // hyphen.
            ("hy", vec!["h", "y", ""]),
            // Properties named in the resources.
            ("named", vec!["named"]),
            // The outermost span wins, in one Tj and the next.
            ("outer", vec!["outer", ""]),
            // A form drawn inside a span is covered by it, and the span
            // carries on after the form.
            ("viaform", vec!["viaform", "", "", ""]),
            ("", vec![""]),
            // A span a form leaves open ends with the form.
            ("leak", vec!["leak", "", "", "", "", "", "", ""]),
            ("after", vec!["a", "f", "t", "e", "r"]),
            // A span with no glyphs gives nothing and ends at its EMC.
            ("plain", vec!["p", "l", "a", "i", "n"]),
            // BMC carries no properties.
            ("bmc", vec!["b", "m", "c"]),
        ]
    );
    let sources = |i: usize| runs[i].glyphs.iter().map(|g| g.source).collect::<Vec<_>>();
    assert_eq!(sources(1), [UnicodeSource::ActualText; 3]);
    assert_eq!(
        sources(2),
        [
            UnicodeSource::GlyphName,
            UnicodeSource::GlyphName,
            UnicodeSource::ActualText
        ]
    );
    assert_eq!(sources(8), [UnicodeSource::GlyphName; 5]);
}

/// A one-page PDF whose content is `content`, with `/F1` Helvetica.
fn helvetica_page(content: &str) -> Vec<u8> {
    pdf_from(&[
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
         /Resources << /Font << /F1 5 0 R >> >> >>"
            .into(),
        stream("", content),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".into(),
    ])
}

fn page_runs(content: &str) -> Vec<TextRunParams> {
    runs(&render(&helvetica_page(content), TextExtraction::Glyphs))
        .into_iter()
        .map(|(_, r)| r)
        .collect()
}

#[test]
fn runs_span_show_operators_along_a_baseline() {
    // One glyph per text object, each placed where the last one ended
    // (Helvetica's H is 722 units wide), then one per Tj: a run each for
    // the two lines.
    let runs = page_runs(
        "BT /F1 12 Tf 1 0 0 1 72 700 Tm (H) Tj ET \
         BT /F1 12 Tf 1 0 0 1 80.664 700 Tm (e) Tj ET \
         BT /F1 12 Tf 72 686 Td (y) Tj (o) Tj (u) Tj ET",
    );
    let texts: Vec<&str> = runs.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(texts, ["He", "you"]);
    assert!(close(runs[0].start, (72.0, 92.0)));
    let last = &runs[0].glyphs[1];
    assert!(close(
        runs[0].end,
        (last.origin.0 + last.advance.0, last.origin.1)
    ));
    assert!(runs.iter().all(|r| r.word_breaks.is_empty()));
}

#[test]
fn tj_word_gaps_are_marked_where_no_space_was_shown() {
    // TeX's way: `[(Paper)-333(Title)]`. A kern is no word gap, and a gap
    // after a shown space adds no break.
    let runs = page_runs(
        "BT /F1 12 Tf 72 700 Td [(Paper) -333 (Title) -30 (s)] TJ ET \
         BT /F1 12 Tf 72 680 Td [(Hi ) -333 (there)] TJ ET",
    );
    let texts: Vec<&str> = runs.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(texts, ["PaperTitles", "Hi there"]);
    assert_eq!(runs[0].word_breaks, [5]);
    assert!(runs[1].word_breaks.is_empty());
}

#[test]
fn a_run_starts_at_its_first_glyph() {
    // TJ's leading adjustment moves the first glyph before it is shown.
    let runs = page_runs("BT /F1 12 Tf 72 700 Td [-1000 (A)] TJ ET");
    assert_eq!(runs.len(), 1);
    assert!(close(runs[0].start, (84.0, 92.0)));
    assert!(close(runs[0].glyphs[0].origin, (84.0, 92.0)));
    let m = runs[0].glyph_to_device;
    assert!(close((m.tx, m.ty), (84.0, 92.0)));
}

#[test]
fn runs_split_where_the_rendering_mode_turns_invisible() {
    let runs = page_runs("BT /F1 12 Tf 72 700 Td (ab) Tj 3 Tr (cd) Tj ET");
    let found: Vec<(&str, bool)> = runs
        .iter()
        .map(|r| (r.text.as_str(), r.invisible))
        .collect();
    assert_eq!(found, [("ab", false), ("cd", true)]);
}

#[test]
fn no_word_break_inside_an_actual_text_span() {
    let runs = page_runs(
        "BT /F1 12 Tf 72 700 Td (A) Tj \
         /Span << /ActualText (xy) >> BDC [-500 (a) -1000 (b)] TJ EMC ET",
    );
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].text, "Axy");
    assert_eq!(runs[0].word_breaks, [1]);
}

/// The lines of text `pdf`'s first page shows at `level`, with `layers`.
fn lines_at(pdf: &[u8], level: TextExtraction, layers: &LayerSet) -> Vec<TextLine> {
    let list = render(pdf, level);
    text_lines(text_runs(&list, layers))
}

#[test]
fn lines_read_the_same_at_both_levels() {
    for pdf in [build_pdf(), build_cid_pdf(), build_actual_text_pdf()] {
        let layers = LayerSet::new();
        let glyphs = lines_at(&pdf, TextExtraction::Glyphs, &layers);
        let runs = lines_at(&pdf, TextExtraction::Runs, &layers);
        assert!(!glyphs.is_empty());
        let text = |lines: &[TextLine]| lines.iter().map(|l| l.text.clone()).collect::<Vec<_>>();
        assert_eq!(text(&glyphs), text(&runs));
        for (g, r) in glyphs.iter().zip(&runs) {
            assert_eq!(g.bbox, r.bbox);
            // Word boxes need the glyphs.
            assert!(g.words.iter().all(|w| w.bbox.is_some()), "{}", g.text);
            assert!(r.words.iter().all(|w| w.bbox.is_none()));
        }
    }
}

#[test]
fn lines_of_a_tex_style_page() {
    let pdf = helvetica_page(
        "BT /F1 12 Tf 72 700 Td [(Paper) -333 (Title)] TJ \
         0 -14 Td [(by) -333 (Some) -30 (one)] TJ ( and) Tj ET",
    );
    let lines = lines_at(&pdf, TextExtraction::Glyphs, &LayerSet::new());
    let text: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(text, ["Paper Title", "by Someone and"]);
    // "Paper" spans its five glyphs: from x 72 to the end of its "r".
    let paper = lines[0].words[0].bbox.unwrap();
    assert!((paper[0] - 72.0).abs() < 1e-6);
    let runs = runs(&render(&pdf, TextExtraction::Glyphs));
    let r = &runs[0].1.glyphs[4];
    assert!((paper[2] - (r.origin.0 + r.advance.0)).abs() < 1e-6);
}

#[test]
fn text_runs_follow_the_layer_set() {
    let pdf = build_pdf();
    let list = render(&pdf, TextExtraction::Runs);
    let has_layer_text =
        |layers: &LayerSet| text_runs(&list, layers).iter().any(|r| r.text == "layer");
    assert!(has_layer_text(&LayerSet::new()));
    let doc = PdfDocument::from_bytes(&pdf).unwrap();
    let mut hidden = LayerSet::new();
    for layer in doc.layers() {
        hidden.set(layer.ocg_id, false);
    }
    assert!(!has_layer_text(&hidden));
}
