// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Text extraction from PostScript: `InterpreterBuilder::extract_text()`.
//!
//! The switch only adds `DisplayElement::TextRun` elements. Everything else
//! in the display list — and so everything rendered or written to PDF — must
//! be identical with it on and off, and with it off no `TextRun` may appear
//! at all.

use stet::{DisplayElement, Interpreter, PsDisplayList};

/// Every show operator, plus the places text can hide: a composite font,
/// a Type 3 font, a pattern cell, a form, and a clip.
const SHOW_FAMILY: &str = r#"%!PS-Adobe-3.0
/Helvetica findfont 12 scalefont setfont
72 700 moveto (show) show
72 680 moveto 1 0 (ashow) ashow
72 660 moveto 2 0 32 (width show) widthshow
72 640 moveto 2 0 32 1 0 (awidth show) awidthshow
72 620 moveto { pop pop 1 0 rmoveto } (kshow) kshow
72 600 moveto (xshow) [8 8 8 8 8] xshow
72 580 moveto (yshow) [2 2 2 2 2] yshow
72 560 moveto (xy) [8 1 8 1] xyshow
72 540 moveto /Aacute glyphshow
72 520 moveto { pop pop pop } (cshow) cshow
gsave 72 500 moveto 30 rotate (rotated) show grestore
gsave 0 0 100 100 rectclip 72 480 moveto (clipped) show grestore

% Composite font: FMapType 2 (8/8) over two base fonts.
/Composite <<
  /FontType 0 /FMapType 2 /FontMatrix [1 0 0 1 0 0]
  /Encoding [0 1]
  /FDepVector [/Helvetica findfont /Times-Roman findfont]
>> definefont pop
/Composite findfont 12 scalefont setfont
72 460 moveto <0041 0142> show

% Type 3 font with one named glyph.
/T3 <<
  /FontType 3 /FontMatrix [0.001 0 0 0.001 0 0] /FontBBox [0 0 1000 1000]
  /Encoding 256 array dup 0 1 255 { /.notdef put dup } for pop dup 65 /A put
  /BuildGlyph { pop pop 1000 0 0 0 1000 1000 setcachedevice 0 0 1000 1000 rectfill }
  /BuildChar { 1 index /Encoding get exch get 1 index /BuildGlyph get exec }
>> definefont pop
/T3 findfont 12 scalefont setfont
72 440 moveto (AAA) show

% Text inside a pattern cell and inside a form.
<< /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 40 20]
   /XStep 40 /YStep 20
   /PaintProc { pop /Helvetica findfont 8 scalefont setfont 0 5 moveto (tile) show }
>> matrix makepattern setpattern
72 380 200 40 rectfill
<< /FormType 1 /BBox [0 0 200 20] /Matrix [1 0 0 1 0 0]
   /PaintProc { pop /Helvetica findfont 12 scalefont setfont 0 5 moveto (form) show }
>> 72 340 translate execform
showpage
"#;

fn render(extract: bool) -> Vec<PsDisplayList> {
    let mut builder = Interpreter::builder();
    if extract {
        builder = builder.extract_text();
    }
    let mut interp = builder.build();
    interp
        .render_to_display_list(SHOW_FAMILY.as_bytes(), 72.0)
        .expect("fixture renders")
        .into_iter()
        .map(|page| page.display_list)
        .collect()
}

/// `list` with every `TextRun` removed, at every depth — including
/// transparency groups, soft masks, layers and pattern cells.
fn without_text_runs(list: &PsDisplayList) -> PsDisplayList {
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
    let off = render(false);
    let on = render(true);
    assert_eq!(off.len(), 1);
    assert_eq!(on.len(), off.len());
    for (off, on) in off.iter().zip(&on) {
        // With the switch off, not a single TextRun at any depth.
        assert_eq!(format!("{off:?}"), format!("{:?}", without_text_runs(off)));
        // With it on, removing the TextRuns gives back the list exactly.
        assert_eq!(format!("{off:?}"), format!("{:?}", without_text_runs(on)));
        // No show operator records a TextRun yet (emission lands in a later
        // change), so the two lists are identical outright.
        assert_eq!(format!("{off:?}"), format!("{on:?}"));
    }
}

#[test]
fn fixture_exercises_the_show_family() {
    // Guards the test above against a fixture that silently stopped
    // showing text: every show operator records a `Text` element.
    let lists = render(false);
    let texts = lists[0]
        .elements()
        .iter()
        .filter(|e| matches!(e, DisplayElement::Text { .. }))
        .count();
    assert!(texts >= 12, "only {texts} Text elements");
}
