// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF output embeds a multiple master font as one.
//!
//! The charstrings of a multiple master font carry every master design and
//! are embedded as they are, so the embedded font has to say how to blend
//! them. It was written as an ordinary Type 1 font: every reader drew the
//! first master alone, or nothing. `pdftops` writes Acrobat's substitute
//! font, AdobeSansMM, into any file whose PDF used it.
#![cfg(feature = "pdf-output")]

use std::io::Read;

use stet::{DisplayElement, Interpreter};
use stet_fonts::geometry::PathSegment;

/// A charstring number, in the five-byte form.
fn num(value: i32) -> Vec<u8> {
    let mut bytes = vec![255];
    bytes.extend_from_slice(&value.to_be_bytes());
    bytes
}

/// Charstring encryption, with the four leading bytes `/lenIV 4` expects,
/// as a PostScript hex string.
fn encrypted(plain: &[u8]) -> String {
    let mut r: u32 = 4330;
    let mut hex = String::from("<");
    for &byte in [0u8; 4].iter().chain(plain) {
        let cipher = byte ^ (r >> 8) as u8;
        r = ((u32::from(cipher) + r) * 52845 + 22719) & 0xFFFF;
        hex.push_str(&format!("{cipher:02x}"));
    }
    hex.push('>');
    hex
}

/// A font of two masters weighted a quarter and three quarters. Its one
/// glyph, `M` at code 66, is a box 300 high whose width is 100 in the first
/// master and 500 in the second: 400 at these weights.
fn font() -> String {
    const HSBW: u8 = 13;
    const RMOVETO: u8 = 21;
    const RLINETO: u8 = 5;
    const CLOSEPATH: u8 = 9;
    const ENDCHAR: u8 = 14;
    // `base delta 2 14 callothersubr pop`: OtherSubr 14 blends one value.
    let blended = |base: i32, delta: i32| {
        [num(base), num(delta), num(2), num(14), vec![12, 16, 12, 17]].concat()
    };
    let m = [
        num(0),
        num(600),
        vec![HSBW],
        num(0),
        num(0),
        vec![RMOVETO],
        blended(100, 400),
        num(0),
        vec![RLINETO],
        num(0),
        num(300),
        vec![RLINETO],
        blended(-100, -400),
        num(0),
        vec![RLINETO, CLOSEPATH, ENDCHAR],
    ]
    .concat();
    let notdef = [num(0), num(600), vec![HSBW, ENDCHAR]].concat();
    format!(
        "/TestMM 14 dict begin
           /FontType 1 def /FontName /TestMM def /PaintType 0 def
           /FontMatrix [0.001 0 0 0.001 0 0] def
           /FontBBox [0 0 500 300] def
           /FontInfo 3 dict dup begin
             /BlendDesignPositions [[0] [1]] def
             /BlendDesignMap [[[1 0] [1000 1]]] def
             /BlendAxisTypes [/Weight] def
           end def
           /Blend 1 dict dup begin
             /FontBBox {{{{0 0}} {{0 0}} {{100 500}} {{300 300}}}} def
           end def
           /WeightVector [0.25 0.75] def
           /Encoding 256 array 0 1 255 {{ 1 index exch /.notdef put }} for
             dup 66 /M put def
           /Private 2 dict dup begin /lenIV 4 def /Subrs [] def end def
           /CharStrings 2 dict dup begin /.notdef {} def /M {} def end def
           currentdict end definefont pop\n",
        encrypted(&notdef),
        encrypted(&m),
    )
}

fn job() -> String {
    format!(
        "%!PS\n<< /PageSize [600 400] >> setpagedevice\n{}\
         /TestMM findfont 1000 scalefont setfont 50 50 moveto (B) show showpage\n",
        font()
    )
}

/// The width of the one glyph box among `elements`, to the nearest point.
fn glyph_width<'a>(elements: impl Iterator<Item = &'a DisplayElement>) -> f64 {
    let mut span = (f64::MAX, f64::MIN);
    for element in elements {
        let DisplayElement::Fill { path, .. } = element else {
            continue;
        };
        for segment in &path.segments {
            if let PathSegment::MoveTo(x, _) | PathSegment::LineTo(x, _) = *segment {
                span = (span.0.min(x), span.1.max(x));
            }
        }
    }
    (span.1 - span.0).round()
}

/// Every stream of `pdf` that inflates, inflated.
fn inflated_streams(pdf: &[u8]) -> Vec<Vec<u8>> {
    let mut streams = Vec::new();
    let mut at = 0;
    while let Some(found) = pdf[at..].windows(7).position(|w| w == b"stream\n") {
        let start = at + found + 7;
        let mut out = Vec::new();
        if flate2::read::ZlibDecoder::new(&pdf[start..])
            .read_to_end(&mut out)
            .is_ok()
        {
            streams.push(out);
        }
        at = start;
    }
    streams
}

/// The glyph is as wide in the PDF as PostScript drew it: the reader had
/// the weights to blend the masters by.
#[test]
fn the_pdf_draws_the_blended_glyph() {
    let job = job();
    let pages = Interpreter::new()
        .render_to_display_list(job.as_bytes(), 72.0)
        .unwrap();
    assert_eq!(glyph_width(pages[0].display_list.elements().iter()), 400.0);

    let pdf = Interpreter::new()
        .render_to_pdf(job.as_bytes(), 72.0)
        .unwrap();
    let doc = stet_pdf_reader::PdfDocument::from_bytes(&pdf).unwrap();
    let page = doc.render_page(0, 72.0).unwrap();
    assert_eq!(glyph_width(page.elements().iter()), 400.0);
}

/// The embedded font carries all three things readers look for. Ghostscript
/// draws the first master alone without `/Blend`, and it and poppler draw
/// nothing given `/Blend` without the axes.
#[test]
fn the_embedded_font_says_it_is_a_multiple_master() {
    let pdf = Interpreter::new()
        .render_to_pdf(job().as_bytes(), 72.0)
        .unwrap();
    let streams = inflated_streams(&pdf);
    let program = streams
        .iter()
        .find(|s| s.windows(14).any(|w| w == b"%!PS-AdobeFont"))
        .expect("an embedded Type 1 font");
    let header = String::from_utf8_lossy(program);
    for entry in [
        "/WeightVector [0.25 0.75] def",
        "/BlendDesignPositions [[0] [1]] def",
        "/BlendDesignMap [[[1 0] [1000 1]]] def",
        "/BlendAxisTypes [/Weight] def",
        "/FontBBox [[0 0] [0 0] [100 500] [300 300]] def",
    ] {
        assert!(header.contains(entry), "no `{entry}` in the embedded font");
    }
}

/// A font that is not a multiple master gains none of it.
#[test]
fn an_ordinary_font_is_embedded_as_before() {
    let job = job()
        .replace("/WeightVector [0.25 0.75] def", "")
        .replace("/Blend 1 dict dup begin", "/Unused 1 dict dup begin");
    let pdf = Interpreter::new()
        .render_to_pdf(job.as_bytes(), 72.0)
        .unwrap();
    let streams = inflated_streams(&pdf);
    let program = streams
        .iter()
        .find(|s| s.windows(14).any(|w| w == b"%!PS-AdobeFont"))
        .expect("an embedded Type 1 font");
    let header = String::from_utf8_lossy(program);
    assert!(!header.contains("WeightVector"));
    assert!(!header.contains("/Blend "));
    assert!(!header.contains("BlendAxisTypes"));
}
