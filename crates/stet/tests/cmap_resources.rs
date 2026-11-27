// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! CMap resources as CIDInit builds them, and the CMap category's
//! `resourcestatus`.

use stet::Interpreter;
use stet_core::cmap::{CmapTables, Selector, Target};
use stet_core::object::PsValue;

/// Runs `body`; it divides by zero when a check fails, so a passing program renders one page.
fn passes(body: &str) {
    let src =
        format!("%!PS-Adobe-3.0\n{body}\n0 0 1 setrgbcolor 20 20 100 100 rectfill showpage\n");
    let mut interp = Interpreter::builder().suppress_output().build();
    match interp.render(src.as_bytes(), 18.0) {
        Ok(pages) => assert_eq!(pages.len(), 1),
        Err(e) => panic!("{e:?}"),
    }
}

/// A CMap's mappings come in blocks of at most 100. `endcmap` joins each kind's
/// blocks into one array, in order, and leaves nothing else behind.
#[test]
fn endcmap_joins_a_cmaps_blocks() {
    passes(
        "/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
         /CMapName /Blocks-H def /CMapType 1 def\n\
         1 begincodespacerange <0000> <FFFF> endcodespacerange\n\
         1 begincidrange <0001> <0001> 1 endcidrange\n\
         2 begincidrange <0002> <0002> 2 <0003> <0003> 3 endcidrange\n\
         1 begincidchar <0010> 16 endcidchar\n\
         1 begincidrange <0004> <0004> 4 endcidrange\n\
         endcmap CMapName currentdict /CMap defineresource pop end end\n\
         /Blocks-H /CMap findresource\n\
         dup /.MappingBlocks known { 1 0 div } if\n\
         dup /CIDCharMappings get length 2 ne { 1 0 div } if\n\
         /CIDRangeMappings get\n\
         dup length 12 ne { 1 0 div } if\n\
         dup 2 get 1 ne { 1 0 div } if\n\
         dup 5 get 2 ne { 1 0 div } if\n\
         11 get 4 ne { 1 0 div } if",
    );
}

/// A key that names a folder is no CMap, as `findresource` already decides: its
/// status is not looked for in a file.
#[test]
fn resourcestatus_refuses_a_key_naming_a_folder() {
    passes(
        "(../ProcSet/CIDInit) cvn /CMap resourcestatus { 1 0 div } if\n\
         (..\\\\ProcSet\\\\CIDInit) cvn /CMap resourcestatus { 1 0 div } if\n\
         /Identity-H /CMap resourcestatus not { 1 0 div } if pop pop",
    );
}

/// Builds a CMap from `body` (the lines between `begincmap` and `endcmap`)
/// and calls `check` with the tables read from it and a name lookup.
fn with_tables(body: &str, check: impl FnOnce(&CmapTables, &dyn Fn(&str) -> Selector)) {
    let src = format!(
        "/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
         /CMapType 1 def\n{body}\n\
         endcmap CMapName currentdict /CMap defineresource end end"
    );
    let mut interp = Interpreter::builder().suppress_output().build();
    let ctx = interp.context();
    stet_engine::eval::parse_and_exec(ctx, src.as_bytes()).expect("the CMap is defined");
    let PsValue::Dict(cmap) = ctx.o_stack.peek(0).expect("the CMap is on the stack").value else {
        panic!("not a dictionary");
    };
    let tables = CmapTables::read(ctx, cmap);
    let name = |n: &str| Selector::Name(ctx.names.find(n.as_bytes()).expect("a known name"));
    check(&tables, &name);
}

fn target(font: usize, selector: Selector) -> Target {
    Target { font, selector }
}

/// The CMap example of PLRM 5.11.4: CIDs for font 0, character codes and
/// glyph names for font 1 (a base font), each block in the font `usefont`
/// set before it.
#[test]
fn the_plrm_example_cmap_maps_to_two_fonts() {
    with_tables(
        "/CMapName /Example def\n\
         4 begincodespacerange <00> <80> <8140> <9FFC> <A0> <DF> <E040> <FCFC> endcodespacerange\n\
         0 usefont\n\
         4 begincidrange <20> <7e> 231 <8140> <817e> 633 <8180> <81ac> 696 <8940> <897e> 1219 endcidrange\n\
         1 usefont\n\
         2 beginbfrange <C1> <C3> <63> <A1> <A3> [/A /B /C] endbfrange\n\
         3 beginbfchar <CA> /j <CB> <6B> <CC> <6C> endbfchar",
        |t, name| {
            assert_eq!(t.target(b"\x21"), target(0, Selector::Cid(232)));
            assert_eq!(t.target(b"\x81\x41"), target(0, Selector::Cid(634)));
            assert_eq!(t.target(b"\xc2"), target(1, Selector::Code(0x64)));
            assert_eq!(t.target(b"\xa2"), target(1, name("B")));
            assert_eq!(t.target(b"\xca"), target(1, name("j")));
            assert_eq!(t.target(b"\xcb"), target(1, Selector::Code(0x6b)));
            assert_eq!(t.target(b"\xa0"), target(0, Selector::Notdef), "unmapped");
            assert_eq!(t.cid(b"\xcb"), 0, "a base-font selector is no CID");
        },
    );
}

/// A `bfchar` target longer than one byte is a code all the same, as
/// Ghostscript reads it; notdef mappings keep their own fonts.
#[test]
fn bf_and_notdef_mappings_keep_their_fonts() {
    with_tables(
        "/CMapName /Fonts def\n\
         1 begincodespacerange <00> <FF> endcodespacerange\n\
         2 usefont 1 beginbfchar <44> <0041> endbfchar\n\
         1 beginnotdefrange <50> <5F> 1 endnotdefrange\n\
         0 usefont 1 beginnotdefchar <60> 2 endnotdefchar",
        |t, _| {
            assert_eq!(t.target(b"\x44"), target(2, Selector::Code(0x41)));
            assert_eq!(t.target(b"\x51"), target(2, Selector::Cid(1)));
            assert_eq!(t.target(b"\x60"), target(0, Selector::Cid(2)));
        },
    );
}

/// A CMap written all in font 0 — the pdftops and Adobe CMaps — records no
/// fonts, and a `usecmap` parent's blocks keep the parent's fonts.
#[test]
fn font_0_records_nothing_and_a_parent_keeps_its_fonts() {
    passes(
        "/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
         /CMapName /Font0 def /CMapType 1 def\n\
         1 begincodespacerange <0000> <FFFF> endcodespacerange\n\
         0 usefont 1 begincidrange <0000> <FFFF> 0 endcidrange\n\
         endcmap CMapName currentdict /CMap defineresource\n\
         /.MappingFonts known { 1 0 div } if end end",
    );
    with_tables(
        "/CMapName /Child def\n\
         /Parent-H usecmap\n\
         1 begincodespacerange <00> <FF> endcodespacerange\n\
         1 begincidchar <41> 7 endcidchar"
            .replace(
                "/Parent-H usecmap",
                "/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
                 /CMapName /Parent-H def /CMapType 1 def\n\
                 1 begincodespacerange <00> <FF> endcodespacerange\n\
                 3 usefont 1 begincidchar <42> 9 endcidchar\n\
                 endcmap CMapName currentdict /CMap defineresource pop end end\n\
                 /Parent-H usecmap",
            )
            .as_str(),
        |t, _| {
            assert_eq!(t.target(b"\x41"), target(0, Selector::Cid(7)));
            assert_eq!(t.target(b"\x42"), target(3, Selector::Cid(9)));
        },
    );
}
