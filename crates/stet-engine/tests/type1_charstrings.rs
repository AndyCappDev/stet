// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What a Type 1 charstring needs from its font, on every operator that
//! draws or measures a glyph.
//!
//! A glyph made of a letter and an accent (`seac`) names its two parts, and
//! they have to be looked up in the font. The charstrings of a multiple
//! master font carry every master design and blend them by the font's
//! `/WeightVector`. `show` supplied the first and nothing supplied the
//! second, so `xyshow` — which is what `pdftops` writes — dropped accents,
//! and a multiple master font came out as scribbles everywhere.

use stet_core::context::Context;
use stet_core::display_list::DisplayElement;
use stet_core::geometry::{Matrix, PathSegment};
use stet_render::SkiaDevice;

/// A charstring number, in the five-byte form.
fn num(value: i32) -> Vec<u8> {
    let mut bytes = vec![255];
    bytes.extend_from_slice(&value.to_be_bytes());
    bytes
}

const HSBW: &[u8] = &[13];
const RMOVETO: &[u8] = &[21];
const RLINETO: &[u8] = &[5];
const CLOSEPATH: &[u8] = &[9];
const ENDCHAR: &[u8] = &[14];
const SEAC: &[u8] = &[12, 6];
const CALLOTHERSUBR: &[u8] = &[12, 16];
const POP: &[u8] = &[12, 17];

/// `value` on its own, or as `base` and the amount the second master adds
/// to it, blended by OtherSubr 14.
enum Amount {
    Plain(i32),
    Blended { base: i32, delta: i32 },
}

fn amount(a: &Amount) -> Vec<u8> {
    match *a {
        Amount::Plain(value) => num(value),
        Amount::Blended { base, delta } => [
            num(base),
            num(delta),
            num(2),
            num(14),
            CALLOTHERSUBR.to_vec(),
            POP.to_vec(),
        ]
        .concat(),
    }
}

/// A glyph `advance` wide holding one box, `width` by `height`, with its
/// lower left corner at (`x`, `y`).
fn box_glyph(advance: i32, x: i32, y: i32, width: (i32, i32), height: i32) -> Vec<u8> {
    let (base, delta) = width;
    let across = |sign: i32| {
        if delta == 0 {
            Amount::Plain(sign * base)
        } else {
            Amount::Blended {
                base: sign * base,
                delta: sign * delta,
            }
        }
    };
    [
        num(0),
        num(advance),
        HSBW.to_vec(),
        num(x),
        num(y),
        RMOVETO.to_vec(),
        amount(&across(1)),
        num(0),
        RLINETO.to_vec(),
        num(0),
        num(height),
        RLINETO.to_vec(),
        amount(&across(-1)),
        num(0),
        RLINETO.to_vec(),
        CLOSEPATH.to_vec(),
        ENDCHAR.to_vec(),
    ]
    .concat()
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

/// A font of two masters weighted a quarter and three quarters, drawn at
/// 1000 units to the em so that a font of size 1000 draws in device units.
///
/// - `A` is `aacute`: the letter `a`, a box from (0, 0) to (300, 300),
///   under `acute`, a box from (100, 500) to (200, 600).
/// - `B` is `M`: a box 300 high whose width is 100 in the first master and
///   500 in the second, which is 400 at these weights.
fn font() -> String {
    let a = box_glyph(600, 0, 0, (300, 0), 300);
    let acute = box_glyph(0, 100, 500, (100, 0), 100);
    let aacute = [
        num(0),
        num(600),
        HSBW.to_vec(),
        num(0),
        num(0),
        num(0),
        num(i32::from(b'a')),
        num(194),
        SEAC.to_vec(),
    ]
    .concat();
    let m = box_glyph(600, 0, 0, (100, 400), 300);
    let notdef = [num(0), num(600), HSBW.to_vec(), ENDCHAR.to_vec()].concat();
    format!(
        "/TestMM 12 dict begin
           /FontType 1 def /FontName /TestMM def /PaintType 0 def
           /FontMatrix [0.001 0 0 0.001 0 0] def
           /FontBBox [0 0 1000 1000] def
           /Encoding 256 array 0 1 255 {{ 1 index exch /.notdef put }} for
             dup 65 /aacute put dup 66 /M put def
           /WeightVector [0.25 0.75] def
           /Private 2 dict dup begin /lenIV 4 def /Subrs [] def end def
           /CharStrings 5 dict dup begin
             /.notdef {} def /a {} def /acute {} def /aacute {} def /M {} def
           end def
           currentdict end definefont pop
         /TestMM findfont 1000 scalefont setfont\n",
        encrypted(&notdef),
        encrypted(&a),
        encrypted(&acute),
        encrypted(&aacute),
        encrypted(&m),
    )
}

/// The font selected, device space the same as user space.
fn ctx_with_font() -> Context {
    let mut ctx = Context::new();
    stet_ops::build_system_dict(&mut ctx);
    ctx.exec_sync_fn = Some(stet_engine::eval::exec_sync);
    ctx.device = Some(Box::new(SkiaDevice::new(1000, 1000)));
    ctx.page_width = 1000;
    ctx.page_height = 1000;
    ctx.gstate.ctm = Matrix::identity();
    ctx.gstate.default_ctm = Matrix::identity();
    stet_engine::eval::parse_and_exec(&mut ctx, font().as_bytes()).expect("font defines");
    ctx
}

/// The corners of everything `program` paints: left, bottom, right, top.
fn painted(program: &str) -> [f64; 4] {
    let mut ctx = ctx_with_font();
    stet_engine::eval::parse_and_exec(&mut ctx, program.as_bytes()).expect("program runs");
    let mut bbox = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for element in ctx.display_list.elements() {
        let DisplayElement::Fill { path, .. } = element else {
            continue;
        };
        for segment in &path.segments {
            if let PathSegment::MoveTo(x, y) | PathSegment::LineTo(x, y) = *segment {
                bbox = [
                    bbox[0].min(x),
                    bbox[1].min(y),
                    bbox[2].max(x),
                    bbox[3].max(y),
                ];
            }
        }
    }
    bbox.map(f64::round)
}

/// The letter and its accent together.
const ACCENTED: [f64; 4] = [0.0, 0.0, 300.0, 600.0];
/// The box at the width the two masters blend to.
const BLENDED: [f64; 4] = [0.0, 0.0, 400.0, 300.0];

#[test]
fn show_draws_the_accent_and_blends_the_masters() {
    assert_eq!(painted("0 0 moveto (A) show"), ACCENTED);
    assert_eq!(painted("0 0 moveto (B) show"), BLENDED);
}

/// `pdftops` positions every glyph itself, with `xyshow`.
#[test]
fn the_positioned_shows_draw_the_accent_and_blend_the_masters() {
    assert_eq!(painted("0 0 moveto (A) [0] xshow"), ACCENTED);
    assert_eq!(painted("0 0 moveto (B) [0] xshow"), BLENDED);
    assert_eq!(painted("0 0 moveto (A) [0 0] xyshow"), ACCENTED);
    assert_eq!(painted("0 0 moveto (B) [0 0] xyshow"), BLENDED);
}

#[test]
fn glyphshow_draws_the_accent_and_blends_the_masters() {
    assert_eq!(painted("0 0 moveto /aacute glyphshow"), ACCENTED);
    assert_eq!(painted("0 0 moveto /M glyphshow"), BLENDED);
}

/// `stringwidth` keeps the outline it worked out for the `show` that
/// follows, so it has to work out the whole of it.
#[test]
fn a_glyph_measured_first_is_still_drawn_whole() {
    assert_eq!(
        painted("(AB) stringwidth pop pop 0 0 moveto (A) show"),
        ACCENTED
    );
    assert_eq!(
        painted("(AB) stringwidth pop pop 0 0 moveto (B) show"),
        BLENDED
    );
}

#[test]
fn charpath_has_the_accent_and_the_blended_width() {
    for (text, expected) in [("A", ACCENTED), ("B", BLENDED)] {
        let mut ctx = ctx_with_font();
        let program = format!("0 0 moveto ({text}) false charpath pathbbox");
        stet_engine::eval::parse_and_exec(&mut ctx, program.as_bytes()).expect("charpath runs");
        let bbox: Vec<f64> = (0..4)
            .rev()
            .map(|i| ctx.o_stack.peek(i).unwrap().as_f64().unwrap().round())
            .collect();
        assert_eq!(bbox, expected, "({text})");
    }
}
