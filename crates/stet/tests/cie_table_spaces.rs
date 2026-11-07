// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! CIEBasedDEF and CIEBasedDEFG colour spaces paint the colours their
//! lookup tables give, as Ghostscript does.
//!
//! A CIEBasedDEFG table is m1 arrays, each of m2 strings (PLRM). stet read
//! it as m1 strings, found arrays where it wanted strings, and painted every
//! colour in such a space black — the layout `pdftops` writes every ICC
//! profile it converts to. Neither space applied its `/Decode*` procedures or
//! `/RangeHIJ(K)`, and DEFG took the nearest table entry instead of
//! interpolating. The expected colours are Ghostscript's.

#![cfg(feature = "render")]

use stet::Interpreter;

/// The colour of each 20-point-wide patch along the middle of the page.
fn patches(job: &str, count: u32) -> Vec<[u8; 3]> {
    let pages = Interpreter::new().render(job.as_bytes(), 72.0).unwrap();
    let page = &pages[0];
    (0..count)
        .map(|n| {
            let i = ((10 * page.width + 20 * n + 10) * 4) as usize;
            [page.rgba[i], page.rgba[i + 1], page.rgba[i + 2]]
        })
        .collect()
}

#[track_caller]
fn assert_near(got: &[[u8; 3]], want: &[[u8; 3]]) {
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        let close = g.iter().zip(w).all(|(a, b)| a.abs_diff(*b) <= 3);
        assert!(close, "patch {i}: got {g:?}, want {w:?}");
    }
}

/// A CIE dictionary's white point and an sRGB-like MatrixABC, so table
/// entries read as RGB.
const ABC: &str = "/WhitePoint [0.9505 1 1.089] \
                   /MatrixABC [0.4124 0.2126 0.0193 0.3576 0.7152 0.1192 0.1805 0.0722 0.9505]";

/// A 2×2×2×2 CIEBasedDEFG table in the PLRM's layout that turns CMYK into
/// RGB: each of R, G, B is 255·(1 − C, M or Y)·(1 − K).
fn cmyk_table() -> String {
    let mut outer = String::new();
    for c in 0..2u32 {
        outer.push('[');
        for m in 0..2u32 {
            outer.push('<');
            for y in 0..2u32 {
                for k in 0..2u32 {
                    for v in [c, m, y] {
                        outer.push_str(&format!("{:02x}", 255 * (1 - v) * (1 - k)));
                    }
                }
            }
            outer.push_str("> ");
        }
        outer.push_str("] ");
    }
    format!("[2 2 2 2 [{outer}]]")
}

/// A 3×3×3 CIEBasedDEF table: each of A, B, C falls 255, 127, 0 along its
/// own axis.
fn def_table() -> String {
    let mut strings = String::new();
    for d in 0..3u32 {
        strings.push('<');
        for e in 0..3u32 {
            for f in 0..3u32 {
                for v in [d, e, f] {
                    strings.push_str(&format!("{:02x}", 255 * (2 - v) / 2));
                }
            }
        }
        strings.push_str("> ");
    }
    format!("[3 3 3 [{strings}]]")
}

fn job(body: &str) -> String {
    format!("%!PS\n<< /PageSize [100 20] >> setpagedevice\n{body}\nshowpage\n")
}

#[test]
fn defg_table_in_the_plrm_layout_is_read() {
    let space = format!("[/CIEBasedDEFG << {ABC} /Table {} >>]", cmyk_table());
    let got = patches(
        &job(&format!(
            "{space} setcolorspace\n\
             0 0 0 0 setcolor 0 0 20 20 rectfill\n\
             1 0 0 0 setcolor 20 0 20 20 rectfill"
        )),
        2,
    );
    assert_near(&got, &[[255, 255, 244], [53, 255, 246]]);
}

#[test]
fn defg_table_is_interpolated() {
    // Between grid points: the nearest entry would give white or cyan, not
    // the tints Ghostscript paints.
    let space = format!("[/CIEBasedDEFG << {ABC} /Table {} >>]", cmyk_table());
    let got = patches(
        &job(&format!(
            "{space} setcolorspace\n\
             0.5 0.5 0 0 setcolor 0 0 20 20 rectfill\n\
             0 0 0 0.5 setcolor 20 0 20 20 rectfill"
        )),
        2,
    );
    assert_near(&got, &[[189, 187, 253], [187, 188, 187]]);
}

#[test]
fn defg_decode_and_range_hijk_are_applied() {
    // DecodeDEFG inverts C, and RangeHIJK stretches K's axis to 0..2, so
    // 0 0 0 1 reads the table at C 1, K 0.5.
    let space = format!(
        "[/CIEBasedDEFG << {ABC} /Table {} /DecodeDEFG [{{1 exch sub}} {{}} {{}} {{}}] \
         /RangeHIJK [0 1 0 1 0 1 0 2] >>]",
        cmyk_table()
    );
    let got = patches(
        &job(&format!(
            "{space} setcolorspace 0 0 0 1 setcolor 0 0 20 20 rectfill"
        )),
        1,
    );
    assert_near(&got, &[[0, 188, 188]]);
}

#[test]
fn def_decode_and_range_hij_are_applied() {
    let plain = format!("[/CIEBasedDEF << {ABC} /Table {} >>]", def_table());
    let decoded = format!(
        "[/CIEBasedDEF << {ABC} /Table {} /DecodeDEF [{{dup mul}} {{}} {{1 exch sub}}] \
         /RangeHIJ [0 1 0 2 0 1] >>]",
        def_table()
    );
    let got = patches(
        &job(&format!(
            "{plain} setcolorspace 0.25 0.5 0.75 setcolor 0 0 20 20 rectfill\n\
             {decoded} setcolorspace 0.5 0.5 0.25 setcolor 20 0 20 20 rectfill"
        )),
        2,
    );
    assert_near(&got, &[[225, 187, 136], [225, 225, 135]]);
}
