// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A machine of the test's own for `no_host_files.rs` and `host_files.rs`:
//! a home folder, a `STET_CMAP_DIR` and (on Windows) a system folder, all
//! in one scratch directory, holding a CMap file in each place the reader
//! looks for one and a CMYK profile where the system's is looked for. The
//! documents the two tests read are built here too.
//!
//! The environment is the process's, so each of those tests is a single
//! test function, and the only one in its binary.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use stet_graphics::display_list::{DisplayElement, DisplayList};
use stet_graphics::text::text_runs;
use stet_pdf_reader::diagnostics::{LocationHint, ParsePhase};
use stet_pdf_reader::{LayerSet, PdfDocument, TextExtraction};

/// A CMap only `STET_CMAP_DIR` has, named as a document's encoding.
pub const ENV_CMAP: &str = "EnvTest-H";
/// A CMap only `~/.local/share/stet/CMap` has, named as an encoding.
pub const HOME_CMAP: &str = "HomeTest-H";
/// A CMap only `STET_CMAP_DIR` has, named by `usecmap` in an embedded one.
pub const ENV_BASE_CMAP: &str = "EnvBase-H";
/// A CMap only `~/.local/share/stet/CMap` has, named by `usecmap`.
pub const HOME_BASE_CMAP: &str = "HomeBase-H";

/// The CMYK profile planted where the system's is looked for.
pub const PLANTED_PROFILE: &[u8] =
    include_bytes!("../../../stet-graphics/tests/data/cmyk_intent/inklimit.icc");

/// stet's own CMYK profile, as the reader embeds it.
pub const OWN_PROFILE: &[u8] = include_bytes!("../../src/default_cmyk.icc");

/// The test's machine. Removed when dropped.
pub struct Machine {
    root: PathBuf,
}

impl Machine {
    /// Build the machine under the system's temporary folder and point the
    /// process at it. Call it first, before the reader reads anything.
    pub fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("stet-host-files-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let machine = Machine { root };

        // A CMap with one-byte codes: a document reading it shows "ABCD"
        // as four glyphs, where the two-byte codes it falls back to
        // without one make two.
        let one_byte = |name: &str| {
            format!(
                "/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
                 /CMapName /{name} def\n\
                 1 begincodespacerange\n<00> <FF>\nendcodespacerange\n\
                 1 begincidrange\n<00> <FF> 0\nendcidrange\n\
                 endcmap CMapName currentdict /CMap defineresource pop end end\n"
            )
        };
        let env_dir = machine.cmap_dir();
        let home_dir = machine.home().join(".local/share/stet/CMap");
        for (dir, name) in [
            (&env_dir, ENV_CMAP),
            (&env_dir, ENV_BASE_CMAP),
            (&home_dir, HOME_CMAP),
            (&home_dir, HOME_BASE_CMAP),
        ] {
            write(&dir.join(name), one_byte(name).as_bytes());
        }

        // The CMYK profile, where each system's search starts. On Linux
        // that is under /usr/share, which a test cannot write to.
        let profile_dir = if cfg!(target_os = "macos") {
            Some(machine.home().join("Library/ColorSync/Profiles"))
        } else if cfg!(target_os = "windows") {
            Some(machine.root.join("Windows/System32/spool/drivers/color"))
        } else {
            None
        };
        if let Some(dir) = profile_dir {
            write(&dir.join("planted.icc"), PLANTED_PROFILE);
        }

        // SAFETY: the only test in this binary calls this before it reads
        // anything, so no other thread reads the environment meanwhile.
        unsafe {
            std::env::set_var("HOME", machine.home());
            std::env::set_var("USERPROFILE", machine.home());
            std::env::set_var("STET_CMAP_DIR", machine.cmap_dir());
            if cfg!(target_os = "windows") {
                std::env::set_var("SYSTEMROOT", machine.root.join("Windows"));
            }
        }
        machine
    }

    pub fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    pub fn cmap_dir(&self) -> PathBuf {
        self.root.join("cmap")
    }

    /// The list of the machine's fonts the reader keeps, once it has
    /// looked for them.
    pub fn font_cache_file(&self) -> PathBuf {
        self.home().join(".cache/stet/system_fonts.json")
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn write(path: &Path, data: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, data).unwrap();
}

/// A PDF of `objects`, numbered from 1, with a cross-reference table.
pub fn pdf_from(objects: &[Vec<u8>]) -> Vec<u8> {
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
pub fn stream(dict: &str, data: impl AsRef<[u8]>) -> Vec<u8> {
    let data = data.as_ref();
    let mut out = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    out.extend(data);
    out.extend(b"\nendstream");
    out
}

/// One page for each `(font, content)`: page `i` has the font as `/F1`
/// and shows the content. Objects 1 and 2 are the catalog and the page
/// tree, then a page and its content for each, then `extra` from
/// `3 + 2 * pages.len()` on.
pub fn pages(pages: &[(&str, &str)], extra: &[Vec<u8>]) -> Vec<u8> {
    let kids: Vec<String> = (0..pages.len())
        .map(|i| format!("{} 0 R", 3 + 2 * i))
        .collect();
    let mut objects: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>",
            kids.join(" "),
            pages.len()
        )
        .into_bytes(),
    ];
    for (i, (font, content)) in pages.iter().enumerate() {
        objects.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents {} 0 R \
                 /Resources << /Font << /F1 {font} >> >> >>",
                4 + 2 * i
            )
            .into_bytes(),
        );
        objects.push(stream("", content));
    }
    objects.extend_from_slice(extra);
    pdf_from(&objects)
}

/// The first object number [`pages`] gives `extra`.
pub fn first_extra(page_count: usize) -> usize {
    3 + 2 * page_count
}

/// A ToUnicode CMap for two-byte codes, mapping each `(code, unicode)`.
pub fn to_unicode(entries: &[(u16, char)]) -> String {
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
         /CMapName /Test def\n1 begincodespacerange <0000> <FFFF> endcodespacerange\n",
    );
    cmap.push_str(&format!("{} beginbfchar\n", entries.len()));
    for (code, c) in entries {
        cmap.push_str(&format!("<{code:04X}> <{:04X}>\n", *c as u32));
    }
    cmap.push_str("endbfchar\nendcmap CMapName currentdict /CMap defineresource pop end end");
    cmap
}

/// Four pages, each showing text in a font the document names and does
/// not carry, and the text each shows:
///
/// 0. a simple Type 1 font, Palatino;
/// 1. a simple TrueType font, Verdana;
/// 2. a composite font over a CID TrueType font, Arial, whose CIDs are
///    not Unicode, with a `/ToUnicode` CMap;
/// 3. a composite font over a CID TrueType font in the Adobe-Japan1
///    collection, MS-Mincho, which is where the reader turns to a CJK
///    font of the machine's when it has no other.
pub fn fonts_not_carried() -> (Vec<u8>, [&'static str; 4]) {
    let x = first_extra(4);
    let cid_font = |base: &str, ordering: &str, descriptor: usize| {
        format!(
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /{base} \
             /CIDSystemInfo << /Registry (Adobe) /Ordering ({ordering}) /Supplement 0 >> \
             /FontDescriptor {descriptor} 0 R /DW 600 >>"
        )
        .into_bytes()
    };
    let descriptor = |name: &str| {
        format!(
            "<< /Type /FontDescriptor /FontName /{name} /Flags 32 \
             /FontBBox [0 -200 1000 900] /ItalicAngle 0 /Ascent 900 /Descent -200 \
             /CapHeight 700 /StemV 80 >>"
        )
        .into_bytes()
    };
    let widths = format!("[{}]", ["600"; 95].join(" "));
    let simple = |subtype: &str, base: &str, descriptor: usize| {
        format!(
            "<< /Type /Font /Subtype /{subtype} /BaseFont /{base} /Encoding /WinAnsiEncoding \
             /FirstChar 32 /LastChar 126 /Widths {widths} /FontDescriptor {descriptor} 0 R >>"
        )
    };
    let fonts = [
        simple("Type1", "Palatino-Roman", x + 5),
        simple("TrueType", "Verdana", x + 6),
        format!(
            "<< /Type /Font /Subtype /Type0 /BaseFont /Arial /Encoding /Identity-H \
             /DescendantFonts [{} 0 R] /ToUnicode {} 0 R >>",
            x,
            x + 1
        ),
        format!(
            "<< /Type /Font /Subtype /Type0 /BaseFont /MS-Mincho /Encoding /Identity-H \
             /DescendantFonts [{} 0 R] >>",
            x + 2
        ),
    ];
    let font_objects: Vec<String> = (0..4).map(|i| format!("{} 0 R", x + 7 + i)).collect();
    // Arial's CIDs 1 to 4 are W, i, d, e; in Adobe-Japan1, CIDs 1 to 95
    // are the ASCII characters from the space on.
    let japan1 = |c: char| c as u32 - 0x1F;
    let pages_content = [
        "BT /F1 12 Tf 10 50 Td (Plain) Tj ET".to_string(),
        "BT /F1 12 Tf 10 50 Td (True) Tj ET".to_string(),
        "BT /F1 12 Tf 10 50 Td <0001000200030004> Tj ET".to_string(),
        format!(
            "BT /F1 12 Tf 10 50 Td <{:04X}{:04X}> Tj ET",
            japan1('J'),
            japan1('p')
        ),
    ];
    let page_list: Vec<(&str, &str)> = font_objects
        .iter()
        .zip(pages_content.iter())
        .map(|(f, c)| (f.as_str(), c.as_str()))
        .collect();
    let extra = vec![
        cid_font("Arial", "Identity", x + 3),
        stream("", to_unicode(&[(1, 'W'), (2, 'i'), (3, 'd'), (4, 'e')])),
        cid_font("MS-Mincho", "Japan1", x + 4),
        descriptor("Arial"),
        descriptor("MS-Mincho"),
        descriptor("Palatino-Roman"),
        descriptor("Verdana"),
        fonts[0].clone().into_bytes(),
        fonts[1].clone().into_bytes(),
        fonts[2].clone().into_bytes(),
        fonts[3].clone().into_bytes(),
    ];
    (pages(&page_list, &extra), ["Plain", "True", "Wide", "Jp"])
}

/// Four pages showing the codes `<41424344>` in a composite font whose
/// encoding is a CMap the document does not carry, in turn: [`ENV_CMAP`]
/// and [`HOME_CMAP`] by name, and [`ENV_BASE_CMAP`] and
/// [`HOME_BASE_CMAP`] through `usecmap` in an embedded CMap. The CIDFont
/// is embedded, so nothing about the font depends on the machine. Its
/// `/ToUnicode` gives the two two-byte codes the text "ab".
pub fn cmaps_not_carried() -> Vec<u8> {
    let x = first_extra(4);
    let embedded_using = |base: &str| {
        stream(
            "/Type /CMap /CMapName /Using /CIDSystemInfo \
             << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >>",
            format!(
                "/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
                 /{base} usecmap\n\
                 /CMapName /Using def\n\
                 endcmap CMapName currentdict /CMap defineresource pop end end\n"
            ),
        )
    };
    let type0 = |encoding: String| {
        format!(
            "<< /Type /Font /Subtype /Type0 /BaseFont /Dingbats /Encoding {encoding} \
             /DescendantFonts [{x} 0 R] /ToUnicode {} 0 R >>",
            x + 9
        )
        .into_bytes()
    };
    let font_file: &[u8] = include_bytes!("../../fonts/D050000L.t1");
    let extra = vec![
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
        embedded_using(ENV_BASE_CMAP),
        embedded_using(HOME_BASE_CMAP),
        type0(format!("/{ENV_CMAP}")),
        type0(format!("/{HOME_CMAP}")),
        type0(format!("{} 0 R", x + 3)),
        type0(format!("{} 0 R", x + 4)),
        stream("", to_unicode(&[(0x4142, 'a'), (0x4344, 'b')])),
    ];
    let fonts: Vec<String> = (0..4).map(|i| format!("{} 0 R", x + 5 + i)).collect();
    let content = "BT /F1 12 Tf 10 50 Td <41424344> Tj ET";
    let page_list: Vec<(&str, &str)> = fonts.iter().map(|f| (f.as_str(), content)).collect();
    pages(&page_list, &extra)
}

/// One page filled with a DeviceCMYK color.
pub fn cmyk_page() -> Vec<u8> {
    pages(&[("<< >>", "0.2 0.5 0.7 0.1 k 0 0 200 100 re f")], &[])
}

/// The text each run on `list` shows, joined, and the glyphs shown.
pub fn text_and_glyphs(list: &DisplayList) -> (String, usize) {
    let runs = text_runs(list, &LayerSet::new());
    (
        runs.iter().map(|run| run.text.as_str()).collect(),
        runs.iter().map(|run| run.glyphs.len()).sum(),
    )
}

/// How many paths `list` fills at its top level: here, the glyphs drawn.
pub fn fills(list: &DisplayList) -> usize {
    list.elements()
        .iter()
        .filter(|e| matches!(e, DisplayElement::Fill { .. }))
        .count()
}

/// Read `page` of `doc` with glyph positions recorded.
pub fn read(doc: &mut PdfDocument, page: usize) -> DisplayList {
    doc.set_text_extraction(TextExtraction::Glyphs);
    doc.render_page(page, 72.0).expect("the page renders")
}

/// The content warnings `doc` has given, as text.
pub fn warnings(doc: &PdfDocument) -> Vec<String> {
    doc.parse_warnings()
        .iter()
        .map(|w| format!("{:?} {:?}: {}", w.location, w.severity, w.message))
        .collect()
}

/// The pages `doc` has given a warning of `phase` for, in order.
pub fn pages_warned(doc: &PdfDocument, phase: ParsePhase) -> Vec<usize> {
    doc.parse_warnings()
        .iter()
        .filter(|w| w.phase == phase)
        .filter_map(|w| match w.location {
            Some(LocationHint::Page(page)) => Some(page),
            _ => None,
        })
        .collect()
}
