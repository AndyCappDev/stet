// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `stet text <file>` — print the text a PDF, PostScript or EPS file shows.
//!
//! Records `TextRun`s while interpreting each page and assembles them with
//! `stet_graphics::text::text_lines`: lines of words in content order, as
//! plain text or, with `--json`, with their positions.

use std::collections::HashSet;
use std::io::Write;

use stet::{Interpreter, TextExtraction};
use stet_graphics::display_list::DisplayList;
use stet_graphics::icc::IccCache;
use stet_graphics::layer_set::LayerSet;
use stet_graphics::text::{TextLine, text_lines, text_runs};
use stet_pdf_reader::{PdfDocument, PdfError};

/// What `stet text` was asked for.
pub struct TextOptions {
    /// 1-based page numbers to print; every page when `None`.
    pub pages: Option<HashSet<i32>>,
    pub password: Option<String>,
    /// Print JSON with positions instead of plain text.
    pub json: bool,
    /// In JSON, give every word its box, which needs every glyph recorded.
    pub word_boxes: bool,
    /// Write to this file instead of stdout.
    pub output: Option<String>,
}

/// One page's text.
struct PageText {
    /// 1-based page number.
    number: usize,
    /// Page size in points.
    size: (f64, f64),
    lines: Vec<TextLine>,
}

/// Run the text subcommand on `path`. Returns a process exit code.
pub fn run_text(path: &str, options: &TextOptions) -> i32 {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Error: cannot read '{path}': {e}");
            return 1;
        }
    };
    // Word boxes need each glyph's position; lines and words do not.
    let level = if options.word_boxes {
        TextExtraction::Glyphs
    } else {
        TextExtraction::Runs
    };
    let wanted = |number: usize| {
        options
            .pages
            .as_ref()
            .is_none_or(|pages| pages.contains(&(number as i32)))
    };
    let pages = if is_pdf(path, &data) {
        pdf_pages(path, &data, options.password.as_deref(), level, &wanted)
    } else {
        ps_pages(path, &data, level, &wanted)
    };
    let Some(pages) = pages else {
        return 1;
    };

    // Created only now, so a file that fails to open or parse leaves no
    // empty output behind.
    let (mut out, destination): (Box<dyn Write>, String) = match &options.output {
        Some(output) => match std::fs::File::create(output) {
            Ok(file) => (
                Box::new(std::io::BufWriter::new(file)),
                format!("'{output}'"),
            ),
            Err(e) => {
                eprintln!("Error: cannot create '{output}': {e}");
                return 1;
            }
        },
        None => (Box::new(std::io::stdout().lock()), "the text".into()),
    };
    let written = if options.json {
        write_json(&mut out, &pages)
    } else {
        write_plain(&mut out, &pages)
    };
    match written.and_then(|()| out.flush()) {
        Ok(()) => 0,
        // A closed pipe (`stet text doc.pdf | head`) is not an error.
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe && options.output.is_none() => 0,
        Err(e) => {
            eprintln!("Error: cannot write {destination}: {e}");
            1
        }
    }
}

/// A PDF by its extension, or by a `%PDF-` header near the start (which
/// may follow junk bytes).
fn is_pdf(path: &str, data: &[u8]) -> bool {
    path.to_ascii_lowercase().ends_with(".pdf")
        || data[..data.len().min(1024)]
            .windows(5)
            .any(|w| w == b"%PDF-")
}

/// The lines of text on one page's display list.
fn lines_of(list: &DisplayList) -> Vec<TextLine> {
    text_lines(text_runs(list, &LayerSet::new()))
}

fn pdf_pages(
    path: &str,
    data: &[u8],
    password: Option<&str>,
    level: TextExtraction,
    wanted: &dyn Fn(usize) -> bool,
) -> Option<Vec<PageText>> {
    // Colour management does nothing for text, so no system CMYK profile
    // is loaded.
    let opened = match password {
        Some(pw) => PdfDocument::from_bytes_with_password(data, IccCache::new(), pw.as_bytes()),
        None => PdfDocument::from_bytes_with_icc(data, IccCache::new()),
    };
    let mut doc = match opened {
        Ok(doc) => doc,
        Err(PdfError::PasswordRequired) => {
            eprintln!(
                "Error: '{path}' is encrypted with a non-empty password — use --password <pw>"
            );
            return None;
        }
        Err(e) => {
            eprintln!("Error: failed to parse '{path}': {e}");
            return None;
        }
    };
    doc.set_text_extraction(level);
    let mut pages = Vec::new();
    for index in 0..doc.page_count() {
        let number = index + 1;
        if !wanted(number) {
            continue;
        }
        // 72 dpi: device space is points, from the top-left corner.
        let list = match doc.render_page(index, 72.0) {
            Ok(list) => list,
            Err(e) => {
                eprintln!("Warning: page {number} of '{path}': {e}");
                continue;
            }
        };
        pages.push(PageText {
            number,
            size: doc.page_size(index).unwrap_or_default(),
            lines: lines_of(&list),
        });
    }
    Some(pages)
}

fn ps_pages(
    path: &str,
    data: &[u8],
    level: TextExtraction,
    wanted: &dyn Fn(usize) -> bool,
) -> Option<Vec<PageText>> {
    // The program's own output (`print`, `==`) would mix with the text,
    // and colour management does nothing for it.
    let mut interp = Interpreter::builder()
        .suppress_output()
        .no_icc()
        .text_extraction(level)
        .build();
    let rendered = match interp.render_to_display_list(data, 72.0) {
        Ok(pages) => pages,
        Err(e) => {
            eprintln!("Error: '{path}': {e}");
            return None;
        }
    };
    Some(
        rendered
            .iter()
            .enumerate()
            .filter(|(index, _)| wanted(index + 1))
            .map(|(index, page)| PageText {
                number: index + 1,
                size: (page.width as f64, page.height as f64),
                lines: lines_of(&page.display_list),
            })
            .collect(),
    )
}

/// What `stet text --help` prints.
pub fn print_text_help() {
    println!(
        "stet text <FILE> [-o <PATH>] [--pages <SPEC>] [--password <PW>] [--json [--word-boxes]]

Prints the text a PDF, PostScript or EPS file shows, a line at a time in
the order the file draws it, each page ending with a form feed.

Words the file sets apart without a space character (as TeX does) are
separated. Lines are assembled simply: content order is kept, and columns,
tables and reading order are not detected. Invisible text (an OCR layer)
is included; text in layers hidden by default is not.

Options:
    -o, --output <PATH>
                     Write to PATH instead of stdout: every page selected, in
                     one file (\"%d\" is not a page template here).
    --pages <SPEC>   Pages to print: \"3\", \"1-5\", \"1-3,7,10-12\".
    --password <PW>  Password for encrypted PDF input.
    --json           Print JSON instead: {{\"pages\": [{{\"page\", \"width\",
                     \"height\", \"lines\": [{{\"text\", \"bbox\", \"vertical\",
                     \"words\": [{{\"text\"}}]}}]}}]}}. Positions are in points
                     from the page's top-left corner, y downward; a bbox is
                     [x0, y0, x1, y1].
    --word-boxes     With --json, give each word a \"bbox\" too. Records
                     every glyph's position, which takes more memory."
    );
}

/// Each page's lines, the page ended by a form feed, as `pdftotext` does.
fn write_plain(out: &mut impl Write, pages: &[PageText]) -> std::io::Result<()> {
    for page in pages {
        for line in &page.lines {
            writeln!(out, "{}", line.text)?;
        }
        write!(out, "\x0c")?;
    }
    Ok(())
}

/// The pages as one JSON document: see [`print_text_help`].
fn write_json(out: &mut impl Write, pages: &[PageText]) -> std::io::Result<()> {
    writeln!(out, "{{\"pages\": [")?;
    for (p, page) in pages.iter().enumerate() {
        write!(
            out,
            "  {{\"page\": {}, \"width\": {}, \"height\": {}, \"lines\": [",
            page.number,
            number(page.size.0),
            number(page.size.1)
        )?;
        for (l, line) in page.lines.iter().enumerate() {
            let separator = if l == 0 { "" } else { "," };
            write!(
                out,
                "{separator}\n    {{\"text\": {}, \"bbox\": {}, \"vertical\": {}, \"words\": [",
                string(&line.text),
                bbox(line.bbox),
                line.vertical
            )?;
            for (w, word) in line.words.iter().enumerate() {
                let separator = if w == 0 { "" } else { ", " };
                write!(out, "{separator}{{\"text\": {}", string(&word.text))?;
                if let Some(b) = word.bbox {
                    write!(out, ", \"bbox\": {}", bbox(b))?;
                }
                write!(out, "}}")?;
            }
            write!(out, "]}}")?;
        }
        let end = if page.lines.is_empty() { "" } else { "\n  " };
        let separator = if p + 1 == pages.len() { "" } else { "," };
        writeln!(out, "{end}]}}{separator}")?;
    }
    writeln!(out, "]}}")
}

/// A JSON number, to a hundredth of a point.
fn number(v: f64) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    let s = format!("{:.2}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

fn bbox(b: [f64; 4]) -> String {
    format!(
        "[{}, {}, {}, {}]",
        number(b[0]),
        number(b[1]),
        number(b[2]),
        number(b[3])
    )
}

/// A JSON string literal.
fn string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_strings_and_numbers() {
        assert_eq!(string("a\"b\\c\n\u{1}é"), "\"a\\\"b\\\\c\\n\\u0001é\"");
        assert_eq!(number(612.0), "612");
        assert_eq!(number(91.456), "91.46");
        assert_eq!(number(-0.001), "0");
        assert_eq!(number(f64::NAN), "0");
    }

    #[test]
    fn pdf_is_known_by_its_header_or_extension() {
        assert!(is_pdf("x.PDF", b""));
        assert!(is_pdf("x", b"junk\n%PDF-1.7"));
        assert!(!is_pdf("x.ps", b"%!PS-Adobe-3.0"));
    }
}
