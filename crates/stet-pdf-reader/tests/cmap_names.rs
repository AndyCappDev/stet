// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A predefined CMap's name comes from the document and is looked for as a
//! file name in the folders CMaps are kept in. A name that is not one a
//! CMap can have, such as a path out of those folders or an absolute
//! path, is not looked for at all. This runs on a machine of its own (see
//! `host_machine`), with a CMap planted outside those folders where such a
//! name would reach it.

mod host_machine;

use host_machine::*;
use stet_pdf_reader::PdfDocument;

#[test]
fn a_cmap_name_reaches_no_file_outside_the_cmap_folders() {
    let machine = Machine::new("names");
    let outside = machine.cmap_dir().join("../outside/Escape-H");
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    std::fs::write(&outside, one_byte_cmap("Escape-H")).unwrap();
    let absolute = std::fs::canonicalize(&outside).unwrap();

    // The first page's CMap is in STET_CMAP_DIR, and is read: four one-byte
    // codes, so the test would notice the escape CMap being read too.
    let names = [
        ENV_CMAP.to_string(),
        "../outside/Escape-H".to_string(),
        absolute.to_string_lossy().into_owned(),
    ];
    let pdf = cmap_pages(&names);
    let mut doc = PdfDocument::from_bytes(&pdf).unwrap();

    // Four glyphs where the CMap was read, two where it was not.
    let glyphs: Vec<usize> = (0..names.len())
        .map(|page| text_and_glyphs(&read(&mut doc, page)).1)
        .collect();
    assert_eq!(
        glyphs,
        [4, 2, 2],
        "only the CMap in STET_CMAP_DIR may be read, of {names:#?}"
    );
    let not_found = warnings(&doc)
        .iter()
        .filter(|w| w.contains("predefined CMap"))
        .count();
    assert_eq!(not_found, 2, "{:#?}", warnings(&doc));
}

/// A CMap with one-byte codes, as `host_machine` plants.
fn one_byte_cmap(name: &str) -> String {
    format!(
        "/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
         /CMapName /{name} def\n\
         1 begincodespacerange\n<00> <FF>\nendcodespacerange\n\
         1 begincidrange\n<00> <FF> 0\nendcidrange\n\
         endcmap CMapName currentdict /CMap defineresource pop end end\n"
    )
}

/// `text` as a PDF name, each byte that is not a letter, a digit or `-`
/// written as `#XX`.
fn pdf_name(text: &str) -> String {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b == b'-' {
                char::from(b).to_string()
            } else {
                format!("#{b:02X}")
            }
        })
        .collect()
}

/// One page for each name: a composite font over an embedded CIDFont,
/// encoded with the predefined CMap of that name, showing `<41424344>`.
fn cmap_pages(names: &[String]) -> Vec<u8> {
    let x = first_extra(names.len());
    let font_file: &[u8] = include_bytes!("../fonts/D050000L.t1");
    let mut extra = vec![
        format!(
            "<< /Type /Font /Subtype /CIDFontType0 /BaseFont /Dingbats \
             /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
             /FontDescriptor {} 0 R /DW 600 >>",
            x + 1
        )
        .into_bytes(),
        format!(
            "<< /Type /FontDescriptor /FontName /Dingbats /Flags 4 \
             /FontBBox [0 -200 1000 900] /ItalicAngle 0 /Ascent 900 /Descent -200 \
             /CapHeight 700 /StemV 80 /FontFile {} 0 R >>",
            x + 2
        )
        .into_bytes(),
        stream("", font_file),
    ];
    for name in names {
        extra.push(
            format!(
                "<< /Type /Font /Subtype /Type0 /BaseFont /Dingbats /Encoding /{} \
                 /DescendantFonts [{x} 0 R] >>",
                pdf_name(name)
            )
            .into_bytes(),
        );
    }
    let fonts: Vec<String> = (0..names.len())
        .map(|i| format!("{} 0 R", x + 3 + i))
        .collect();
    let content = "BT /F1 12 Tf 10 50 Td <41424344> Tj ET";
    let page_list: Vec<(&str, &str)> = fonts.iter().map(|f| (f.as_str(), content)).collect();
    pages(&page_list, &extra)
}
