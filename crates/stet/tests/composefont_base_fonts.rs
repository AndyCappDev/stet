// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! CMap-keyed (FMapType 9) composite fonts over base fonts, as `composefont`
//! makes them (PLRM 5.11.4–5.11.5): `bfchar` and `bfrange` select a
//! character code or a glyph name in a base font, each mapping in the font
//! the `usefont` before it names.
//!
//! stet took every descendant for a CIDFont, so such text drew nothing and
//! advanced 1000 units a code (#48). The widths here are Ghostscript's, at
//! 1000 points so they are whole numbers: Helvetica's `A` is 667, `.notdef`
//! 278, Courier's glyphs 600.

use stet::Interpreter;

/// PostScript that builds CMaps and checks numbers: `near` divides by zero
/// when a value is off, so the page fails to render.
const PRELUDE: &str = r#"
/near { sub abs 0.01 gt { 1 0 div } if } def
/run1 { dup type /nametype eq { load } if exec } def
/cmap { % name codespace mappings
  /CIDInit /ProcSet findresource begin 12 dict begin begincmap
  3 -1 roll /CMapName exch def /CMapType 1 def
  exch run1 run1
  endcmap CMapName currentdict /CMap defineresource pop end end
} def
/one { 1 begincodespacerange <00> <FF> endcodespacerange } def
/two { 1 begincodespacerange <0000> <FFFF> endcodespacerange } def
/hello { 4 beginbfchar <48> <48> <65> <65> <6C> <6C> <6F> <6F> endbfchar } def
/advance { 0 0 moveto show currentpoint pop } def
/width { stringwidth pop } def
"#;

/// Runs `body` after [`PRELUDE`]; a passing program renders one page.
fn passes(body: &str) {
    let src = format!("%!PS\n{PRELUDE}\n{body}\nshowpage\n");
    let mut interp = Interpreter::builder().suppress_output().build();
    match interp.render(src.as_bytes(), 18.0) {
        Ok(pages) => assert_eq!(pages.len(), 1),
        Err(e) => panic!("{e:?}"),
    }
}

/// #48: Aspose.Words writes a two-byte codespace over one-byte `bfchar`
/// codes into Helvetica.
#[test]
fn one_byte_codes_in_a_two_byte_codespace_show_the_base_font() {
    passes(
        "/Two-Hello /two /hello cmap\n\
         /C /Two-Hello [/Helvetica findfont] composefont pop /C 1000 selectfont\n\
         (Hello) advance 2278 near (Hello) width 2278 near",
    );
}

#[test]
fn a_one_byte_codespace_shows_the_base_font() {
    passes(
        "/One-Hello /one /hello cmap\n\
         /C /One-Hello [/Helvetica findfont] composefont pop /C 1000 selectfont\n\
         (Hello) advance 2278 near (Hello) width 2278 near",
    );
}

/// A name shows that glyph, `.notdef` when the font has none; a code its
/// last byte (Ghostscript: `<0142>` shows `B`); an unmapped code `.notdef`.
#[test]
fn bfchar_selects_a_code_or_a_name() {
    passes(
        "/Bf /one { 4 beginbfchar <41> /nosuchglyph <42> /B <43> <0142> <44> <004D> endbfchar } cmap\n\
         /C /Bf [/Helvetica findfont] composefont pop /C 1000 selectfont\n\
         (A) width 278 near (B) width 667 near (C) width 667 near (D) width 833 near\n\
         (Z) width 278 near",
    );
}

/// A string target counts up from its code; an array gives one selector
/// per code.
#[test]
fn bfrange_counts_up_from_a_code_or_walks_its_array() {
    passes(
        "/Rg /one { 2 beginbfrange <61> <63> <41> <70> <72> [/W /space /nosuchglyph] endbfrange } cmap\n\
         /C /Rg [/Helvetica findfont] composefont pop /C 1000 selectfont\n\
         (abc) width 2056 near (p) width 944 near (q) width 278 near (r) width 278 near",
    );
}

/// Each mapping block is in the font of the `usefont` before it. `widthshow`
/// compares its character with (font · 256) + code for a code selector and
/// with the input code for a name (PLRM, `widthshow`), and with a composite
/// font takes any integer. The spacing is user space; the font's matrix
/// scales only the glyphs.
#[test]
fn usefont_picks_the_descendant_and_spacing_is_user_space() {
    passes(
        "/Uf /one { 0 usefont 1 beginbfchar <41> <41> endbfchar\n\
                    1 usefont 2 beginbfchar <42> <43> <44> /W endbfchar } cmap\n\
         /C /Uf [/Helvetica findfont /Courier findfont] composefont pop /C 1000 selectfont\n\
         (ABD) width 1867 near\n\
         [[65 1967] [66 1867] [322 1867] [323 1967] [68 1967] [579 1867] [-1 1867] [70000 1867]] {\n\
           aload pop exch 0 0 moveto 100 0 3 -1 roll (ABD) widthshow currentpoint pop near\n\
         } forall\n\
         0 0 moveto 10 0 (ABD) ashow currentpoint pop 1897 near\n\
         0 0 moveto 100 0 323 10 0 (ABD) awidthshow currentpoint pop 1997 near\n\
         /C [500 0 0 1000 0 0] selectfont 0 0 moveto 10 0 (ABD) ashow currentpoint pop 963.5 near\n\
         /C findfont 2000 scalefont setfont (A) width 1334 near",
    );
}

#[test]
fn charpath_appends_the_base_font_outline() {
    passes(
        "/Bf /one { 1 beginbfchar <42> /B endbfchar } cmap\n\
         /C /Bf [/Helvetica findfont] composefont 1000 scalefont setfont\n\
         0 0 moveto (B) false charpath pathbbox newpath\n\
         729 near 623 near 0 near 79 near",
    );
}

/// A base font still takes only a byte as `widthshow`'s character.
#[test]
fn widthshow_with_a_base_font_still_takes_a_byte() {
    passes(
        "/Helvetica 10 selectfont 0 0 moveto\n\
         { 1 0 300 (A) widthshow } stopped not { 1 0 div } if",
    );
}

/// The glyphs are drawn: `(Hello)` at 36 points from x = 10 inks from H's
/// left side bearing to o's right edge, 10 + 2278 × 0.036 less o's right
/// side bearing.
#[cfg(feature = "render")]
#[test]
fn the_base_font_glyphs_are_drawn() {
    let src = format!(
        "%!PS\n<< /PageSize [120 60] >> setpagedevice\n{PRELUDE}\n\
         /Two-Hello /two /hello cmap\n\
         /C /Two-Hello [/Helvetica findfont] composefont pop /C 36 selectfont\n\
         10 20 moveto (Hello) show showpage\n"
    );
    let mut interp = Interpreter::new();
    let pages = interp.render(src.as_bytes(), 72.0).unwrap();
    let page = &pages[0];
    let inked: Vec<u32> = (0..page.width)
        .filter(|&x| (0..page.height).any(|y| page.rgba[((y * page.width + x) * 4) as usize] < 128))
        .collect();
    let (first, last) = (inked[0], *inked.last().unwrap());
    assert!((12..=14).contains(&first), "H starts at x = {first}");
    assert!((89..=92).contains(&last), "o ends at x = {last}");
}

/// `cshow` hands its procedure each code with the glyph's width and the
/// selected descendant, scaled by the composite font's matrix, as the
/// current font (PLRM, `cshow`). The code is the selected character code
/// for a code selector (`<0142>` gives 66), 0 for an unmapped code, and the
/// string's byte for a glyph name; a `show` of it inside the procedure
/// shows that byte in the descendant (Ghostscript: `Z` mapped to `/B`
/// shows `Z`, 611).
#[test]
fn cshow_gives_the_code_width_and_descendant() {
    passes(
        "/Nm /one { 3 beginbfchar <5A> /B <58> /eacute <57> <0142> endbfchar\n\
                    1 beginbfchar <56> /nosuchglyph endbfchar } cmap\n\
         /C /Nm [/Helvetica findfont] composefont pop /C 1000 selectfont\n\
         [ { 3 array astore } (ZXWVY) cshow ] /got exch def\n\
         got length 5 ne { 1 0 div } if\n\
         [[90 667 0] [88 556 0] [66 667 0] [86 278 0] [0 278 0]] {\n\
           /want exch def /g got 0 get def /got got 1 got length 1 sub getinterval def\n\
           0 1 2 { dup g exch get exch want exch get near } for\n\
         } forall\n\
         0 0 moveto { pop pop 1 string dup 0 4 -1 roll put show } (ZXWVY) cshow\n\
         currentpoint pop 2890 near",
    );
}

/// Inside the procedure `currentfont` is the descendant at the composite
/// font's size and `rootfont` the composite font; afterwards the composite
/// font is current again.
#[test]
fn cshow_makes_the_scaled_descendant_current() {
    passes(
        "/Uf /one { 0 usefont 1 beginbfchar <41> <41> endbfchar\n\
                    1 usefont 1 beginbfchar <42> <42> endbfchar } cmap\n\
         /D /Uf [/Helvetica findfont /Courier findfont] composefont pop /D 100 selectfont\n\
         [ { pop pop pop currentfont /FontName get currentfont /FontMatrix get 0 get\n\
             rootfont /FontName get } (AB) cshow ] /names exch def\n\
         names 0 get /Helvetica ne { 1 0 div } if names 1 get 0.1 near\n\
         names 2 get /D ne { 1 0 div } if\n\
         names 3 get /Courier ne { 1 0 div } if names 4 get 0.1 near\n\
         names 5 get /D ne { 1 0 div } if\n\
         currentfont /FontName get /D ne { 1 0 div } if",
    );
}

/// `cshow` gave every glyph a width of 0 0, whatever the font.
#[test]
fn cshow_gives_a_base_fonts_widths() {
    passes(
        "/Helvetica 1000 selectfont [ {} (AB) cshow ] /w exch def\n\
         w 1 get 667 near w 2 get 0 near w 4 get 667 near",
    );
}

/// PLRM, `kshow`: only base fonts.
#[test]
fn kshow_refuses_a_composite_font() {
    passes(
        "/Uf /one { 1 beginbfchar <41> <41> endbfchar } cmap\n\
         /D /Uf [/Helvetica findfont] composefont pop /D 1000 selectfont\n\
         0 0 moveto { {pop pop} (AB) kshow } stopped not { 1 0 div } if\n\
         $error /errorname get /invalidfont ne { 1 0 div } if",
    );
}

#[test]
fn xshow_and_xyshow_place_base_font_glyphs() {
    passes(
        "/Uf /one { 0 usefont 1 beginbfchar <41> <41> endbfchar\n\
                    1 usefont 1 beginbfchar <42> <42> endbfchar } cmap\n\
         /D /Uf [/Helvetica findfont /Courier findfont] composefont pop /D 1000 selectfont\n\
         0 0 moveto (ABA) [100 200 300] xshow currentpoint pop 600 near\n\
         0 0 moveto (AB) [100 5 200 7] xyshow currentpoint 12 near 300 near",
    );
}

/// Text extraction reads base-font glyphs by their names, whichever
/// descendant and show operator drew them.
#[test]
fn base_font_text_is_extracted() {
    use stet::{LayerSet, TextExtraction, text_lines, text_runs};
    let src = format!(
        "%!PS\n{PRELUDE}\n\
         /Two-Hello /two /hello cmap\n\
         /C /Two-Hello [/Helvetica findfont] composefont pop /C 24 selectfont\n\
         72 700 moveto (Hello) show\n\
         /Uf /one {{ 0 usefont 1 beginbfrange <41> <5A> <41> endbfrange\n\
                     1 usefont 1 beginbfrange <61> <7A> <61> endbfrange\n\
                     1 beginbfchar <20> /space endbfchar }} cmap\n\
         /D /Uf [/Helvetica-Bold findfont /Times-Roman findfont] composefont pop\n\
         /D 24 selectfont\n\
         72 650 moveto (Two Fonts) show\n\
         72 600 moveto (XShow) [16 16 12 12 17] xshow\n\
         72 550 moveto {{ pop pop 1 string dup 0 4 -1 roll put show }} (Cshow) cshow\n\
         showpage\n"
    );
    let mut interp = Interpreter::builder()
        .suppress_output()
        .text_extraction(TextExtraction::Runs)
        .build();
    let pages = interp
        .render_to_display_list(src.as_bytes(), 72.0)
        .expect("renders");
    let lines: Vec<String> = text_lines(text_runs(&pages[0].display_list, &LayerSet::new()))
        .into_iter()
        .map(|l| l.text)
        .collect();
    assert_eq!(lines, ["Hello", "Two Fonts", "XShow", "Cshow"]);
}
