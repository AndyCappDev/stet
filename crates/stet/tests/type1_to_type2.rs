// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Every glyph of the bundled URW Type 1 fonts, converted to Type 2 and
//! packed into a CID-keyed CFF, must draw what the Type 1 original draws.
//!
//! The fonts exercise what synthetic charstrings do not: real subroutine
//! nesting, hint replacement through OtherSubr 3 (about 30% of glyphs), and
//! fractional coordinates.

use stet_fonts::cff_parser::parse_cff;
use stet_fonts::cff_writer::{CidFont, FontDict, Glyph, write_cid_font};
use stet_fonts::charstring::execute_charstring;
use stet_fonts::type1_parser::parse_type1;
use stet_fonts::type1_to_type2::{choose_widths, convert_charstring};
use stet_fonts::type2_charstring::execute_type2_charstring;
use stet_fonts::{PathSegment, PsPath};

/// The path as subpaths of drawn points, ignoring how each interpreter
/// marks a subpath closed and dropping subpaths that draw nothing.
fn subpaths(path: &PsPath) -> Vec<Vec<(i64, i64)>> {
    let key = |x: f64, y: f64| ((x * 1000.0).round() as i64, (y * 1000.0).round() as i64);
    let mut out: Vec<Vec<(i64, i64)>> = Vec::new();
    for seg in &path.segments {
        match *seg {
            PathSegment::MoveTo(x, y) => out.push(vec![key(x, y)]),
            PathSegment::LineTo(x, y) => out.last_mut().unwrap().push(key(x, y)),
            PathSegment::CurveTo {
                x1,
                y1,
                x2,
                y2,
                x3,
                y3,
            } => out
                .last_mut()
                .unwrap()
                .extend([key(x1, y1), key(x2, y2), key(x3, y3)]),
            PathSegment::ClosePath => {}
        }
    }
    out.retain(|s| s.len() > 1);
    out
}

#[test]
fn urw_glyphs_survive_conversion_and_cff_packing() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/resources/Font");
    let mut fonts: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "t1"))
        .collect();
    fonts.sort();
    assert_eq!(fonts.len(), 35);

    let mut hint_masked = 0;
    for path in fonts {
        let font = parse_type1(&std::fs::read(&path).unwrap()).unwrap();
        let mut names: Vec<&String> = font.charstrings.keys().collect();
        names.sort();

        let mut converted = Vec::new();
        for name in &names {
            let cs = &font.charstrings[*name];
            let glyph = convert_charstring(cs, &font.subrs, font.len_iv)
                .unwrap_or_else(|e| panic!("{path:?} {name}: {e}"));
            let original = execute_charstring(cs, &font.subrs, font.len_iv, false).unwrap();
            converted.push((glyph, original));
        }

        let widths: Vec<f64> = converted.iter().map(|(g, _)| g.width).collect();
        let (default_width_x, nominal_width_x) = choose_widths(&widths);
        let mut cff = CidFont::new("URW", "Adobe", "Identity", 0);
        cff.cid_count = names.len() as u32;
        let mut fd = FontDict::default();
        fd.private.default_width_x = default_width_x;
        fd.private.nominal_width_x = nominal_width_x;
        cff.font_dicts = vec![fd];
        for (cid, (glyph, _)) in converted.iter().enumerate() {
            let cs = glyph.charstring(default_width_x, nominal_width_x).unwrap();
            if cs.contains(&19) {
                hint_masked += 1;
            }
            cff.glyphs.push(Glyph::new(cid as u16, 0, cs));
        }

        let parsed = parse_cff(&write_cid_font(&cff).unwrap()).unwrap();
        let parsed = &parsed[0];
        let fd = &parsed.fd_array[0];
        for (gid, (_, original)) in converted.iter().enumerate() {
            let r = execute_type2_charstring(
                &parsed.char_strings[gid],
                &fd.local_subrs,
                &parsed.global_subrs,
                fd.default_width_x,
                fd.nominal_width_x,
                false,
            )
            .unwrap();
            let name = names[gid];
            assert_eq!(r.width_x, original.width_x, "{path:?} {name}");
            assert_eq!(
                subpaths(&r.path),
                subpaths(&original.path),
                "{path:?} {name}"
            );
        }
    }
    // Hint replacement is common enough that a regression in it shows.
    assert!(hint_masked > 5000, "{hint_masked}");
}
