// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! CMap resources as CIDInit builds them, and the CMap category's
//! `resourcestatus`.

use stet::Interpreter;

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
