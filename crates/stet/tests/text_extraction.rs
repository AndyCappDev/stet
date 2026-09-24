// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Text extraction from PostScript: `InterpreterBuilder::text_extraction`.
//!
//! The switch only adds `DisplayElement::TextRun` elements. Everything else
//! in the display list — and so everything rendered or written to PDF — must
//! be identical with it on and off, and with it off no `TextRun` may appear
//! at all.

use stet::{
    DisplayElement, Interpreter, PsDisplayList, TextExtraction, TextRunParams, UnicodeSource,
};

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

fn render(level: TextExtraction) -> Vec<PsDisplayList> {
    let mut interp = Interpreter::builder().text_extraction(level).build();
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
    let off = render(TextExtraction::Off);
    assert_eq!(off.len(), 1);
    for level in [TextExtraction::Runs, TextExtraction::Glyphs] {
        let on = render(level);
        assert_eq!(on.len(), off.len());
        for (off, on) in off.iter().zip(&on) {
            // With the switch off, not a single TextRun at any depth.
            assert_eq!(format!("{off:?}"), format!("{:?}", without_text_runs(off)));
            // With it on, removing the TextRuns gives back the list exactly.
            assert_eq!(format!("{off:?}"), format!("{:?}", without_text_runs(on)));
            assert!(!runs(on).is_empty());
        }
    }
}

#[test]
fn runs_level_records_the_same_runs_without_glyphs() {
    let glyphs = runs(&render(TextExtraction::Glyphs)[0]);
    let light = runs(&render(TextExtraction::Runs)[0]);
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

#[test]
fn fixture_exercises_the_show_family() {
    // Guards the test above against a fixture that silently stopped
    // showing text: every show operator records a `Text` element.
    let lists = render(TextExtraction::Off);
    let texts = lists[0]
        .elements()
        .iter()
        .filter(|e| matches!(e, DisplayElement::Text { .. }))
        .count();
    assert!(texts >= 12, "only {texts} Text elements");
}

/// Every run in `list`, in order, with the containers it sits in.
fn runs(list: &PsDisplayList) -> Vec<(Vec<&'static str>, TextRunParams)> {
    fn walk(
        list: &PsDisplayList,
        path: &mut Vec<&'static str>,
        out: &mut Vec<(Vec<&'static str>, TextRunParams)>,
    ) {
        for e in list.elements() {
            let (label, nested): (&'static str, Vec<&PsDisplayList>) = match e {
                DisplayElement::TextRun { params } => {
                    out.push((path.clone(), params.clone()));
                    continue;
                }
                DisplayElement::Group { elements, .. } => ("group", vec![elements]),
                DisplayElement::OcgGroup { elements, .. } => ("layer", vec![elements]),
                DisplayElement::SoftMasked { mask, content, .. } => ("masked", vec![mask, content]),
                DisplayElement::PatternFill { params } => ("pattern", vec![&params.tile]),
                _ => continue,
            };
            path.push(label);
            for list in nested {
                walk(list, path, out);
            }
            path.pop();
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

/// Render `ps` with extraction on and return page 0's runs.
fn runs_of(ps: &str) -> Vec<TextRunParams> {
    let mut interp = Interpreter::builder()
        .text_extraction(TextExtraction::Glyphs)
        .build();
    let pages = interp
        .render_to_display_list(ps.as_bytes(), 72.0)
        .expect("renders");
    runs(&pages[0].display_list)
        .into_iter()
        .map(|(_, run)| run)
        .collect()
}

#[test]
fn show_operators_record_runs_along_each_baseline() {
    let lists = render(TextExtraction::Glyphs);
    let texts: Vec<(Vec<&str>, String)> = runs(&lists[0])
        .into_iter()
        .map(|(path, run)| (path, run.text))
        .collect();
    // Nothing from the pattern cell (`tile`): its text is paint.
    let expected: Vec<(Vec<&str>, String)> = [
        "show",
        "ashow",
        "width show",
        "awidth show",
        "kshow",
        "xshow",
        // yshow moves each glyph off the last one's baseline.
        "y",
        "s",
        "h",
        "o",
        "w",
        "xy",
        // glyphshow's /Aacute. cshow records nothing itself, and this
        // cshow's procedure shows nothing.
        "Á",
        "rotated",
        "clipped",
        // The FMapType composite: a run per descendant font it switches to.
        "A",
        "B",
        "AAA",
        "form",
    ]
    .iter()
    .map(|t| (vec![], t.to_string()))
    .collect();
    assert_eq!(texts, expected);
    let all = runs(&lists[0]);
    let composite: Vec<(&str, &str, u32)> = all[15..17]
        .iter()
        .map(|(_, r)| (r.text.as_str(), r.font_name.as_str(), r.glyphs[0].code))
        .collect();
    assert_eq!(
        composite,
        [("A", "Helvetica", 0x0041), ("B", "Times-Roman", 0x0142)]
    );
    for (_, run) in all {
        assert!(!run.invisible && !run.vertical);
        assert!(
            run.glyphs
                .iter()
                .all(|g| g.source == UnicodeSource::GlyphName),
            "{}",
            run.text
        );
    }
}

#[test]
fn glyph_positions_are_in_device_space() {
    // At 72 dpi device space is user space with y flipped on the 792-point
    // page. Helvetica's s, h, a are 500, 556, 556 units wide.
    let lists = render(TextExtraction::Glyphs);
    let all = runs(&lists[0]);
    let run = |text: &str| &all.iter().find(|(_, r)| r.text == text).unwrap().1;

    let show = run("show");
    assert!(close(show.glyphs[0].origin, (72.0, 92.0)));
    assert!(close(show.glyphs[0].advance, (6.0, 0.0)));
    assert!(close(show.glyphs[1].origin, (78.0, 92.0)));
    assert_eq!(show.font_name, "Helvetica");
    let m = show.glyph_to_device;
    assert!(close((m.a, m.b), (0.012, 0.0)));
    assert!(close((m.c, m.d), (0.0, -0.012)));
    assert!(close((m.tx, m.ty), (72.0, 92.0)));
    // From the font's FontBBox, in its 1000-unit glyph space.
    assert!(show.ascent > 800.0 && show.descent < -100.0);

    // ashow's extra spacing moves the next glyph but is not in the advance.
    let ashow = run("ashow");
    assert!(close(ashow.glyphs[0].advance, (6.672, 0.0)));
    assert!(close(ashow.glyphs[1].origin, (72.0 + 6.672 + 1.0, 112.0)));

    // widthshow adds its spacing after the space only.
    let widthshow = run("width show");
    let space = widthshow.glyphs.iter().position(|g| g.code == 32).unwrap();
    let after = &widthshow.glyphs[space + 1];
    let expected_x = widthshow.glyphs[space].origin.0 + widthshow.glyphs[space].advance.0 + 2.0;
    assert!(close(after.origin, (expected_x, 132.0)));

    // Rotated 30°: advance and up vector turn with the text.
    let rotated = run("rotated");
    let (ax, ay) = rotated.glyphs[0].advance;
    assert!(((-ay).atan2(ax).to_degrees() - 30.0).abs() < 0.1);
    let (ux, uy) = rotated.glyph_to_device.transform_delta(0.0, 1000.0);
    assert!(((-uy).atan2(ux).to_degrees() - 120.0).abs() < 0.1);

    // xshow moves by its displacements; each advance is still the
    // glyph's own width (x is 500 units).
    let xshow = run("xshow");
    assert!(close(xshow.glyphs[0].advance, (6.0, 0.0)));
    assert!(close(xshow.glyphs[1].origin, (80.0, 192.0)));
    // yshow's second glyph is 2 points up, on a baseline of its own.
    let yshow_s = run("s");
    assert!(close(yshow_s.glyphs[0].origin, (72.0, 212.0 - 2.0)));

    // glyphshow shows by name, so there is no character code.
    let glyphshow = run("Á");
    assert_eq!(glyphshow.glyphs[0].code, 0);
    assert!(close(glyphshow.glyphs[0].origin, (72.0, 252.0)));

    // The form is replayed through its CTM: 72 340 translate, 0 5 moveto.
    let form = run("form");
    assert!(close(form.glyphs[0].origin, (72.0, 792.0 - 345.0)));

    // Type 3: glyph space is the font's own (1000 units here), with ascent
    // and descent from its FontBBox.
    let type3 = run("AAA");
    assert!(close(type3.glyphs[0].advance, (12.0, 0.0)));
    assert_eq!((type3.ascent, type3.descent), (1000.0, 0.0));
}

#[test]
fn runs_span_show_operators_along_a_baseline() {
    // A producer that shows a glyph at a time still gives one run per
    // stretch of text, and a new line starts a new run.
    let ps = r#"%!PS
/Helvetica findfont 12 scalefont setfont
72 700 moveto (H) show (e) show (y) show
72 686 moveto (you) show
showpage
"#;
    let runs = runs_of(ps);
    let texts: Vec<&str> = runs.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(texts, ["Hey", "you"]);
    assert_eq!(glyph_texts(&runs[0]), ["H", "e", "y"]);
    assert!(close(runs[0].start, (72.0, 92.0)));
    let last = &runs[0].glyphs[2];
    let expected_end = (last.origin.0 + last.advance.0, last.origin.1);
    assert!(close(runs[0].end, expected_end));
    assert!(close(runs[1].start, (72.0, 106.0)));
    assert!(runs.iter().all(|r| r.word_breaks.is_empty()));
}

#[test]
fn word_gaps_are_marked_where_no_space_was_shown() {
    // TeX's way: words placed apart, no space glyph between them. A kern
    // is no word gap, and a gap after a shown space adds no break.
    let ps = r#"%!PS
/Helvetica findfont 12 scalefont setfont
72 700 moveto (Paper) show 4 0 rmoveto (Title) show 0.5 0 rmoveto (s) show
72 680 moveto (Hi ) show 4 0 rmoveto (there) show
showpage
"#;
    let runs = runs_of(ps);
    let texts: Vec<&str> = runs.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(texts, ["PaperTitles", "Hi there"]);
    assert_eq!(runs[0].word_breaks, [5]);
    assert!(runs[1].word_breaks.is_empty());
}

#[test]
fn a_run_is_not_reopened_from_another_display_list() {
    // Text carrying on inside a transparency group sits in the group's
    // list, so it starts a run of its own there.
    let ps = r#"%!PS
/Helvetica findfont 12 scalefont setfont
72 700 moveto (ab) show
<< /Isolated true /BBox [0 0 612 792] >> begintransparencygroup
(c) show
endtransparencygroup
(d) show
showpage
"#;
    let mut interp = Interpreter::builder()
        .text_extraction(TextExtraction::Glyphs)
        .build();
    let pages = interp
        .render_to_display_list(ps.as_bytes(), 72.0)
        .expect("renders");
    let found: Vec<(Vec<&str>, String)> = runs(&pages[0].display_list)
        .into_iter()
        .map(|(path, run)| (path, run.text))
        .collect();
    assert_eq!(
        found,
        [
            (vec![], "ab".to_string()),
            (vec!["group"], "c".to_string()),
            (vec![], "d".to_string()),
        ]
    );
}

#[test]
fn text_that_is_not_the_documents_is_not_recorded() {
    // A Type 3 glyph procedure that shows text to draw its glyph, and a
    // kshow whose procedure shows a separator between characters.
    let ps = r#"%!PS
/T3 <<
  /FontType 3 /FontMatrix [0.001 0 0 0.001 0 0] /FontBBox [0 0 1000 1000]
  /Encoding 256 array dup 0 1 255 { /.notdef put dup } for pop dup 66 /B put
  /BuildChar { pop pop 1000 0 setcharwidth
    /Helvetica findfont 800 scalefont setfont 0 0 moveto (hidden) show }
>> definefont pop
/T3 findfont 12 scalefont setfont
72 700 moveto (BB) show
/Helvetica findfont 12 scalefont setfont
72 680 moveto { pop pop (-) show } (ab) kshow
/T3 findfont 12 scalefont setfont
72 660 moveto (BB) [20 20] xshow
showpage
"#;
    let texts: Vec<(String, Vec<String>)> = runs_of(ps)
        .iter()
        .map(|r| {
            (
                r.text.clone(),
                glyph_texts(r).into_iter().map(String::from).collect(),
            )
        })
        .collect();
    let owned = |t: &str, g: &[&str]| (t.to_string(), g.iter().map(|s| s.to_string()).collect());
    assert_eq!(
        texts,
        vec![
            // The Type 3 glyphs are the text; what BuildChar shows is not.
            owned("BB", &["B", "B"]),
            // kshow's procedure shows between the characters, carrying on
            // along the kshow's baseline: one run, in page order.
            owned("a-b", &["a", "-", "b"]),
            // The same through xshow, whose Type 3 path is its own.
            owned("BB", &["B", "B"]),
        ]
    );
}

#[test]
fn dvips_numeric_glyph_names() {
    // dvips names bitmap-font glyphs by code (`a65`); read as Latin-1, as
    // Poppler does.
    let ps = r#"%!PS
/D <<
  /FontType 3 /FontMatrix [0.001 0 0 0.001 0 0] /FontBBox [0 0 1000 1000]
  /Encoding 256 array dup 0 1 255 { /.notdef put dup } for pop
    dup 72 /a72 put dup 105 /a105 put
  /BuildChar { pop pop 600 0 setcharwidth }
>> definefont pop
/D findfont 12 scalefont setfont
72 700 moveto (Hi) show
showpage
"#;
    let runs = runs_of(ps);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].text, "Hi");
    assert!(
        runs[0]
            .glyphs
            .iter()
            .all(|g| g.source == UnicodeSource::GlyphName)
    );
}

#[test]
fn type3_fonts_without_a_bbox_use_their_glyph_boxes() {
    // dvips's bitmap fonts: FontBBox [0 0 0 0] and a glyph space of their
    // own (here one unit per em), so the 1000-unit defaults would make each
    // glyph a thousand ems tall. The run covers its glyphs'
    // `setcachedevice` boxes instead, whether a glyph is built or replayed
    // from the cache.
    let ps = r#"%!PS
/U <<
  /FontType 3 /FontMatrix [1 0 0 1 0 0] /FontBBox [0 0 0 0]
  /Encoding 256 array dup 0 1 255 { /.notdef put dup } for pop
    dup 72 /H put dup 103 /g put
  /BuildChar {
    exch pop 72 eq { 0.6 0 0 0 0.6 0.7 } { 0.5 0 0 -0.2 0.5 0.5 } ifelse
    setcachedevice
  }
>> definefont pop
/U findfont 12 scalefont setfont
72 700 moveto (Hg) show
72 680 moveto (Hg) show
72 660 moveto (H) show
showpage
"#;
    let runs = runs_of(ps);
    assert_eq!(runs.len(), 3);
    for run in &runs[..2] {
        assert!((run.ascent - 0.7).abs() < 1e-9, "{}", run.ascent);
        assert!((run.descent + 0.2).abs() < 1e-9, "{}", run.descent);
    }
    assert!((runs[2].ascent - 0.7).abs() < 1e-9, "{}", runs[2].ascent);
    assert!(runs[2].descent.abs() < 1e-9, "{}", runs[2].descent);
}

/// A minimal TrueType font: glyph 1 is 600 units wide in a 1000-unit em,
/// with ascender 900 and descender -250 in `hhea`. The glyphs have no
/// outlines, which is all a text-extraction test needs.
fn minimal_sfnt() -> Vec<u8> {
    let mut head = vec![0u8; 54];
    head[12..16].copy_from_slice(&0x5F0F_3CF5u32.to_be_bytes()); // magic
    head[18..20].copy_from_slice(&1000u16.to_be_bytes()); // unitsPerEm
    let mut hhea = vec![0u8; 36];
    hhea[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    hhea[4..6].copy_from_slice(&900i16.to_be_bytes());
    hhea[6..8].copy_from_slice(&(-250i16).to_be_bytes());
    hhea[34..36].copy_from_slice(&2u16.to_be_bytes()); // numberOfHMetrics
    let mut hmtx = Vec::new();
    for advance in [500u16, 600] {
        hmtx.extend(advance.to_be_bytes());
        hmtx.extend(0i16.to_be_bytes());
    }
    let mut maxp = vec![0u8; 6];
    maxp[0..4].copy_from_slice(&0x0000_5000u32.to_be_bytes());
    maxp[4..6].copy_from_slice(&2u16.to_be_bytes());
    let loca = vec![0u8; 6]; // short offsets: both glyphs empty
    let glyf = vec![0u8; 4];

    let tables: [(&[u8; 4], &[u8]); 6] = [
        (b"glyf", &glyf),
        (b"head", &head),
        (b"hhea", &hhea),
        (b"hmtx", &hmtx),
        (b"loca", &loca),
        (b"maxp", &maxp),
    ];
    let mut font = Vec::new();
    font.extend(0x0001_0000u32.to_be_bytes());
    font.extend((tables.len() as u16).to_be_bytes());
    font.extend([0u8; 6]); // searchRange etc., unused
    let mut offset = 12 + 16 * tables.len();
    let mut bodies = Vec::new();
    for (tag, body) in tables {
        font.extend(tag);
        font.extend(0u32.to_be_bytes());
        font.extend((offset as u32).to_be_bytes());
        font.extend((body.len() as u32).to_be_bytes());
        let mut padded = body.to_vec();
        padded.resize(body.len().div_ceil(4) * 4, 0);
        offset += padded.len();
        bodies.extend(padded);
    }
    font.extend(bodies);
    font
}

#[test]
fn type42_fonts_use_their_hhea_metrics() {
    let hex: String = minimal_sfnt().iter().map(|b| format!("{b:02X}")).collect();
    let ps = format!(
        r#"%!PS
/TT <<
  /FontType 42 /FontMatrix [1 0 0 1 0 0] /FontBBox [0 -0.25 1 0.9] /PaintType 0
  /Encoding 256 array dup 0 1 255 {{ /.notdef put dup }} for pop dup 65 /A put
  /CharStrings << /.notdef 0 /A 1 >>
  /sfnts [<{hex}>]
>> definefont pop
/TT findfont 12 scalefont setfont
72 700 moveto (AA) show
showpage
"#
    );
    let runs = runs_of(&ps);
    assert_eq!(runs.len(), 1);
    let run = &runs[0];
    assert_eq!(glyph_texts(run), ["A", "A"]);
    // Glyph space is the font's units: 1000 per em at 12 pt.
    assert!(close(run.glyphs[0].advance, (7.2, 0.0)));
    assert!(close(run.glyphs[1].origin, (79.2, 92.0)));
    assert!(close(
        (run.glyph_to_device.a, run.glyph_to_device.d),
        (0.012, -0.012)
    ));
    assert_eq!((run.ascent, run.descent), (900.0, -250.0));
}

/// A CIDFontType 2 font over [`minimal_sfnt`], in Adobe's Japan1
/// collection, composed with `cmap` into the Type 0 font `/J`. Its glyphs
/// have no outlines; only their text and positions matter here.
fn cid_font_ps(cmap: &str, show: &str) -> String {
    let hex: String = minimal_sfnt().iter().map(|b| format!("{b:02X}")).collect();
    format!(
        r#"%!PS
/CIDF <<
  /CIDFontType 2 /CIDFontName /CIDF /FontType 42
  /CIDSystemInfo << /Registry (Adobe) /Ordering (Japan1) /Supplement 4 >>
  /FontMatrix [1 0 0 1 0 0] /FontBBox [0 -0.25 1 0.9]
  /CIDCount 65536 /GDBytes 2 /CIDMap 0 /Encoding [] /CharStrings << /.notdef 0 >>
  /sfnts [<{hex}>]
>> /CIDFont defineresource pop
{cmap}
/J /{cmap_name} [/CIDF /CIDFont findresource] composefont pop
/J findfont 12 scalefont setfont
{show}
showpage
"#,
        cmap_name = cmap
            .split_whitespace()
            .skip_while(|t| *t != "/CMapName")
            .nth(1)
            .map(|n| n.trim_start_matches('/'))
            .unwrap_or("Identity-H"),
    )
}

#[test]
fn cid_fonts_give_the_collections_text() {
    // Japan1 CIDs 3851 and 3852 are 諭 and 輸, shown through Identity-H
    // and then through Identity-V.
    let ps = cid_font_ps(
        "",
        "72 700 moveto <0F0B0F0C> show \
         /JV /Identity-V [/CIDF /CIDFont findresource] composefont 12 scalefont setfont \
         300 700 moveto <0F0B0F0C> show",
    );
    let runs = runs_of(&ps);
    assert_eq!(runs.len(), 2, "{runs:#?}");
    let (horizontal, vertical) = (&runs[0], &runs[1]);
    for run in [horizontal, vertical] {
        assert_eq!(glyph_texts(run), ["諭", "輸"]);
        let codes: Vec<u32> = run.glyphs.iter().map(|g| g.code).collect();
        assert_eq!(codes, [0x0F0B, 0x0F0C]);
        assert!(
            run.glyphs
                .iter()
                .all(|g| g.source == UnicodeSource::CidOrdering)
        );
    }
    // Horizontal: 600-unit advances in a 1000-unit em at 12 pt.
    assert!(!horizontal.vertical);
    assert!(close(horizontal.glyphs[1].origin, (72.0 + 7.2, 92.0)));
    assert_eq!((horizontal.ascent, horizontal.descent), (900.0, -250.0));
    // Vertical: origins run down the column by the default 1000-unit
    // vertical advance, and the box spans half the em either side.
    assert!(vertical.vertical);
    assert!(close(vertical.glyphs[0].origin, (300.0, 92.0)));
    assert!(close(vertical.glyphs[0].advance, (0.0, 12.0)));
    assert!(close(vertical.glyphs[1].origin, (300.0, 104.0)));
    assert_eq!((vertical.ascent, vertical.descent), (500.0, -500.0));
}

#[test]
fn unicode_cmaps_give_the_codes_as_text() {
    // A `Uni…-UCS2-…` CMap's codes are the text, whatever CID they select:
    // here あ (U+3042) selects CID 1, which Japan1 would read as a space.
    let cmap = "/CIDInit /ProcSet findresource begin 12 dict begin begincmap \
        /CIDSystemInfo << /Registry (Adobe) /Ordering (Japan1) /Supplement 4 >> def \
        /CMapName /UniTest-UCS2-H def /CMapType 1 def \
        1 begincodespacerange <0000> <FFFF> endcodespacerange \
        1 begincidrange <3042> <3042> 1 endcidrange \
        endcmap CMapName currentdict /CMap defineresource pop end end";
    let runs = runs_of(&cid_font_ps(cmap, "72 700 moveto <3042> show"));
    assert_eq!(runs.len(), 1, "{runs:#?}");
    assert_eq!(runs[0].text, "あ");
    assert_eq!(runs[0].glyphs[0].code, 0x3042);
}

#[test]
fn cff_cid_fonts_give_the_collections_text() {
    // A CIDFontType 0 font with two empty Type 2 charstrings, each
    // `1000 endchar` (a 1000-unit width, no outline), at Japan1 CIDs 3851
    // and 3852.
    let ps = r#"%!PS
/CIDC <<
  /CIDFontType 0 /CIDFontName /CIDC /FontType 9
  /CIDSystemInfo << /Registry (Adobe) /Ordering (Japan1) /Supplement 4 >>
  /FontMatrix [0.001 0 0 0.001 0 0] /FontBBox [0 -120 1000 880]
  /CIDCount 65536 /DW 1000
  /CharStrings << 3851 <1C03E80E> 3852 <1C03E80E> >>
>> /CIDFont defineresource pop
/C /Identity-H [/CIDC /CIDFont findresource] composefont pop
/C findfont 12 scalefont setfont
72 700 moveto <0F0B0F0C> show
showpage
"#;
    let runs = runs_of(ps);
    assert_eq!(runs.len(), 1, "{runs:#?}");
    let run = &runs[0];
    assert_eq!(glyph_texts(run), ["諭", "輸"]);
    assert_eq!(run.font_name, "CIDC");
    assert_eq!((run.ascent, run.descent), (880.0, -120.0));
    assert!(close(run.glyphs[0].advance, (12.0, 0.0)));
    assert!(close(run.glyphs[1].origin, (84.0, 92.0)));
}

#[test]
fn cid_fonts_through_the_displaced_and_cshow_paths() {
    let cmap = "/CIDInit /ProcSet findresource begin 12 dict begin begincmap \
        /CIDSystemInfo << /Registry (Adobe) /Ordering (Japan1) /Supplement 4 >> def \
        /CMapName /UniTest-UCS2-H def /CMapType 1 def \
        1 begincodespacerange <0000> <FFFF> endcodespacerange \
        1 begincidrange <3042> <3042> 1 endcidrange \
        endcmap CMapName currentdict /CMap defineresource pop end end";
    // xyshow, and cshow with the usual procedure: the procedure gets only
    // the code's last byte, but the glyph records the whole code, which
    // for a Uni CMap is the text.
    let runs = runs_of(&cid_font_ps(
        cmap,
        "72 700 moveto <30423042> [0 -20 0 -20] xyshow \
         72 600 moveto { pop pop 1 string dup 0 4 -1 roll put show } <3042> cshow",
    ));
    let summary: Vec<(&str, Vec<u32>)> = runs
        .iter()
        .map(|r| (r.text.as_str(), r.glyphs.iter().map(|g| g.code).collect()))
        .collect();
    // xyshow puts its second glyph 20 points down the page, on a
    // baseline of its own.
    assert_eq!(
        summary,
        [
            ("あ", vec![0x3042]),
            ("あ", vec![0x3042]),
            ("あ", vec![0x3042])
        ]
    );
    assert!(close(runs[1].glyphs[0].origin, (72.0, 112.0)));
}

#[test]
fn an_error_in_a_cshow_procedure_leaves_no_cid_behind() {
    // The cshow procedure fails before showing; `stopped` catches it. The
    // CID cshow selected (1, a space in Japan1) must not carry over to the
    // next show, which shows CID 0 through <0000> — no text at all.
    let cmap = "/CIDInit /ProcSet findresource begin 12 dict begin begincmap \
        /CIDSystemInfo << /Registry (Adobe) /Ordering (Japan1) /Supplement 4 >> def \
        /CMapName /Test-H def /CMapType 1 def \
        1 begincodespacerange <0000> <FFFF> endcodespacerange \
        1 begincidrange <0000> <0000> 0 endcidrange \
        1 begincidrange <0041> <0041> 1 endcidrange \
        endcmap CMapName currentdict /CMap defineresource pop end end";
    let runs = runs_of(&cid_font_ps(
        cmap,
        "72 700 moveto { { pop pop pop stop } <0041> cshow } stopped pop \
         72 680 moveto <0000> show",
    ));
    assert_eq!(runs.len(), 1, "{runs:#?}");
    assert_eq!(runs[0].glyphs[0].code, 0);
    assert_eq!(runs[0].glyphs[0].source, UnicodeSource::Unmapped);
}
