// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The same job gives the same PDF, apart from its creation date.
//!
//! A page's `/Font` resource dictionary was written in the order of a hash
//! set, which changes from run to run, so two runs of one job differed.
#![cfg(feature = "pdf-output")]

use stet::Interpreter;

/// Twelve fonts, each showing a line: their resources are `F0` … `F11`.
const FONTS: [&str; 12] = [
    "Times-Roman",
    "Times-Bold",
    "Times-Italic",
    "Times-BoldItalic",
    "Helvetica",
    "Helvetica-Bold",
    "Helvetica-Oblique",
    "Helvetica-BoldOblique",
    "Courier",
    "Courier-Bold",
    "Courier-Oblique",
    "Courier-BoldOblique",
];

fn pdf() -> Vec<u8> {
    let mut job = String::from("%!PS\n");
    for (i, font) in FONTS.iter().enumerate() {
        job.push_str(&format!(
            "/{font} findfont 12 scalefont setfont 72 {} moveto ({font}) show\n",
            700 - 20 * i
        ));
    }
    job.push_str("showpage\n");
    Interpreter::new()
        .render_to_pdf(job.as_bytes(), 72.0)
        .unwrap()
}

/// `bytes` without its `/CreationDate` entry, the one thing that should vary.
fn without_creation_date(bytes: &[u8]) -> Vec<u8> {
    let key = b"/CreationDate (";
    let start = bytes
        .windows(key.len())
        .position(|w| w == key)
        .expect("a /CreationDate entry");
    let end = start + bytes[start..].iter().position(|&b| b == b')').unwrap() + 1;
    [&bytes[..start], &bytes[end..]].concat()
}

/// The resource names in the page's `/Font` dictionary, in written order.
fn font_resource_names(bytes: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    let start = text.find("/Font <<").expect("a /Font resource dictionary") + "/Font <<".len();
    let dict = &text[start..start + text[start..].find(">>").unwrap()];
    dict.split_whitespace()
        .filter_map(|token| token.strip_prefix('/'))
        .map(str::to_string)
        .collect()
}

#[test]
fn font_resources_are_written_in_order() {
    let expected: Vec<String> = (0..FONTS.len()).map(|i| format!("F{i}")).collect();
    assert_eq!(font_resource_names(&pdf()), expected);
}

#[test]
fn two_runs_give_the_same_pdf() {
    assert_eq!(without_creation_date(&pdf()), without_creation_date(&pdf()));
}
