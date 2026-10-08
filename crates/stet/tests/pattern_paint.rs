// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A pattern is a paint: whatever `fill`, `stroke` and `show` mark, they mark
//! with it (PLRM 3, 4.9). A shading pattern (PatternType 2) painted nothing
//! at all, and a tiling pattern was honoured by the fill operators only,
//! strokes and text coming out in a solid colour.

use stet::Interpreter;

const SIZE: usize = 200;

fn render(body: &str) -> Vec<u8> {
    let program =
        format!("%!PS\n<< /PageSize [{SIZE} {SIZE}] >> setpagedevice\n{body}\nshowpage\n");
    let mut pages = Interpreter::new()
        .render(program.as_bytes(), 72.0)
        .expect("program runs");
    assert_eq!(pages.len(), 1);
    pages.remove(0).rgba
}

/// The pixel at a point given in PostScript's default user space.
fn at(rgba: &[u8], x: usize, y: usize) -> [u8; 3] {
    let i = ((SIZE - 1 - y) * SIZE + x) * 4;
    [rgba[i], rgba[i + 1], rgba[i + 2]]
}

fn is_reddish(p: [u8; 3]) -> bool {
    p[0] > 180 && p[1] < 70 && p[2] < 70
}

fn is_bluish(p: [u8; 3]) -> bool {
    p[2] > 180 && p[0] < 70 && p[1] < 70
}

const WHITE: [u8; 3] = [255, 255, 255];

/// Red at x = 50 running to blue at x = 150, nothing beyond either end.
const AXIAL: &str = "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [50 0 150 0] \
                     /Function << /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1 >>";

/// Left half of each 10-unit cell red, right half blue.
const TILING: &str = "<< /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 10 10] \
                      /XStep 10 /YStep 10 /PaintProc { pop 1 0 0 setrgbcolor 0 0 5 10 rectfill \
                      0 0 1 setrgbcolor 5 0 5 10 rectfill } >> matrix makepattern";

/// Every marked pixel carries pattern colour: none is the black a solid
/// paint would have left.
fn assert_marked_with_pattern(rgba: &[u8], what: &str) {
    let mut marked = 0;
    for p in rgba.as_chunks::<4>().0 {
        if p[..3] == WHITE {
            continue;
        }
        marked += 1;
        assert!(
            p[0].max(p[2]) > 100,
            "{what}: a pixel is {:?}, not pattern colour",
            &p[..3]
        );
    }
    assert!(marked > 200, "{what}: only {marked} pixels marked");
}

#[test]
fn a_shading_pattern_fills() {
    let rgba = render(&format!(
        "<< /PatternType 2 /Shading {AXIAL} >> >> matrix makepattern setpattern\n\
         20 60 160 80 rectfill"
    ));
    assert!(is_reddish(at(&rgba, 55, 100)));
    assert!(is_bluish(at(&rgba, 145, 100)));
    let middle = at(&rgba, 100, 100);
    assert!(
        middle[0] > 90 && middle[2] > 90 && middle[1] < 40,
        "{middle:?}"
    );
    // Not extended, so the ends of the rectangle stay unpainted, and nothing
    // is painted outside the rectangle.
    assert_eq!(at(&rgba, 30, 100), WHITE);
    assert_eq!(at(&rgba, 170, 100), WHITE);
    assert_eq!(at(&rgba, 100, 30), WHITE);
    assert_eq!(at(&rgba, 100, 170), WHITE);
}

#[test]
fn a_shading_pattern_fills_by_the_even_odd_rule() {
    let rgba = render(&format!(
        "<< /PatternType 2 /Shading {AXIAL} >> >> matrix makepattern setpattern\n\
         50 50 100 100 rectfill 0 0 0 setrgbcolor\n\
         << /PatternType 2 /Shading {AXIAL} /Extend [true true] >> >> matrix makepattern setpattern\n\
         newpath 20 20 moveto 180 20 lineto 180 180 lineto 20 180 lineto closepath\n\
         60 60 moveto 140 60 lineto 140 140 lineto 60 140 lineto closepath eofill"
    ));
    // The hole shows the first fill, which did not extend past its ends; the
    // ring around it is extended.
    assert!(is_reddish(at(&rgba, 30, 100)));
    assert!(is_bluish(at(&rgba, 170, 100)));
    assert!(is_reddish(at(&rgba, 30, 30)));
}

#[test]
fn the_shading_stays_where_makepattern_put_it() {
    let rgba = render(&format!(
        "/P << /PatternType 2 /Shading {AXIAL} >> >> matrix makepattern def\n\
         40 0 translate 2 2 scale P setpattern 0 30 50 40 rectfill"
    ));
    // The rectangle covers x = 40..140; the shading still starts at 50.
    assert_eq!(at(&rgba, 45, 100), WHITE);
    assert!(is_reddish(at(&rgba, 55, 100)));
    let p = at(&rgba, 135, 100);
    assert!(p[2] > 180 && p[0] < 80, "{p:?}");
}

#[test]
fn the_background_covers_what_the_shading_does_not() {
    let rgba = render(&format!(
        "<< /PatternType 2 /Shading {AXIAL} /Background [0 1 0] >> >> matrix makepattern setpattern\n\
         20 60 160 80 rectfill"
    ));
    assert_eq!(at(&rgba, 30, 100), [0, 255, 0]);
    assert_eq!(at(&rgba, 170, 100), [0, 255, 0]);
    assert!(is_reddish(at(&rgba, 55, 100)));
    assert_eq!(at(&rgba, 100, 30), WHITE);
}

#[test]
fn the_clip_of_a_shading_fill_ends_with_it() {
    let rgba = render(&format!(
        "<< /PatternType 2 /Shading {AXIAL} >> >> matrix makepattern setpattern\n\
         60 60 20 20 rectfill 0 1 0 setrgbcolor 100 150 40 40 rectfill"
    ));
    assert_eq!(at(&rgba, 120, 170), [0, 255, 0]);
}

#[test]
fn a_mesh_pattern_reads_its_reusable_stream_from_the_start_each_time() {
    let mesh = "<< /PatternType 2 /Shading << /ShadingType 4 /ColorSpace /DeviceRGB \
                /BitsPerCoordinate 8 /BitsPerComponent 8 /BitsPerFlag 8 \
                /Decode [0 200 0 200 0 1 0 1 0 1] /DataSource S >> >> matrix makepattern setpattern";
    let rgba = render(&format!(
        "/S currentfile /ASCIIHexDecode filter /ReusableStreamDecode filter\n\
         001e1eff0000 00c81eff0000 0064c8ff0000 >\ndef\n\
         {mesh} 0 0 100 200 rectfill\n\
         {mesh} 100 0 100 200 rectfill"
    ));
    // One red triangle, its left half from the first pattern and its right
    // half from the second.
    assert!(is_reddish(at(&rgba, 80, 60)));
    assert!(is_reddish(at(&rgba, 120, 60)));
}

#[test]
fn a_stroke_is_painted_with_the_pattern() {
    for (what, pattern) in [
        (
            "shading",
            format!(
                "<< /PatternType 2 /Shading {AXIAL} /Extend [true true] >> >> matrix makepattern"
            ),
        ),
        ("tiling", TILING.to_string()),
    ] {
        let rgba = render(&format!(
            "{pattern} setpattern 12 setlinewidth 20 40 moveto 180 160 lineto stroke"
        ));
        assert_marked_with_pattern(&rgba, &format!("{what} stroke"));
        let rgba = render(&format!(
            "{pattern} setpattern 10 setlinewidth 30 30 140 140 rectstroke"
        ));
        assert_marked_with_pattern(&rgba, &format!("{what} rectstroke"));
        // rectstroke leaves no path behind and does not fill its rectangle.
        assert_eq!(at(&rgba, 100, 100), WHITE);
    }
}

#[test]
fn a_patterned_stroke_leaves_the_current_path_to_rectstroke() {
    let rgba = render(&format!(
        "{TILING} setpattern 4 setlinewidth 20 20 moveto 20 180 lineto\n\
         100 20 60 60 rectstroke 0 1 0 setrgbcolor 180 180 lineto closepath fill"
    ));
    // The triangle (20,20) (20,180) (180,180) built around the rectstroke.
    assert_eq!(at(&rgba, 40, 150), [0, 255, 0]);
}

#[test]
fn text_is_painted_with_the_pattern() {
    for (what, pattern) in [
        (
            "shading",
            format!(
                "<< /PatternType 2 /Shading {AXIAL} /Extend [true true] >> >> matrix makepattern"
            ),
        ),
        ("tiling", TILING.to_string()),
    ] {
        let rgba = render(&format!(
            "/Helvetica-Bold findfont 60 scalefont setfont {pattern} setpattern\n\
             10 80 moveto (Stet) show"
        ));
        assert_marked_with_pattern(&rgba, &format!("{what} text"));
    }
}

#[test]
fn an_uncoloured_cell_takes_the_colour_it_is_used_with() {
    // Made while a coloured pattern is current: the cell is not painted with
    // that pattern.
    let rgba = render(&format!(
        "{TILING} setpattern\n\
         /U << /PatternType 1 /PaintType 2 /TilingType 1 /BBox [0 0 10 10] /XStep 10 /YStep 10 \
         /PaintProc {{ pop 0 0 10 10 rectfill }} >> matrix makepattern def\n\
         [/Pattern /DeviceRGB] setcolorspace 0 1 0 U setcolor 20 20 160 160 rectfill"
    ));
    assert_eq!(at(&rgba, 62, 100), [0, 255, 0]);
    assert_eq!(at(&rgba, 67, 100), [0, 255, 0]);
}

#[test]
fn makepattern_leaves_the_graphics_state_alone() {
    // PaintProc sets a colour and a line width and builds a path; none of it
    // is the program's.
    let rgba = render(
        "0 1 0 setrgbcolor 4 setlinewidth 20 100 moveto\n\
         << /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 10 10] /XStep 10 /YStep 10 \
         /PaintProc { pop 1 0 0 setrgbcolor 60 setlinewidth 0 0 moveto 5 5 lineto } >> \
         matrix makepattern pop\n\
         180 100 lineto stroke",
    );
    assert_eq!(at(&rgba, 100, 100), [0, 255, 0]);
    assert_eq!(at(&rgba, 100, 110), WHITE);
}

#[test]
fn a_shading_pattern_needs_a_shading_dictionary() {
    for dict in ["<< /PatternType 2 >>", "<< /PatternType 2 /Shading 3 >>"] {
        let rgba = render(&format!(
            "{{ {dict} matrix makepattern }} stopped {{ 0 1 0 setrgbcolor 0 0 200 200 rectfill }} if"
        ));
        assert_eq!(at(&rgba, 100, 100), [0, 255, 0], "{dict}");
    }
}

/// An 8 × 8 stencil over (20,20)–(180,180): the left half of its top four
/// rows and the right half of its bottom four are painted.
const STENCIL: &str = "20 20 translate 160 160 scale \
                       8 8 true [8 0 0 -8 0 8] {<f0 f0 f0 f0 0f 0f 0f 0f>} imagemask";

#[test]
fn a_stencil_mask_is_painted_with_the_pattern() {
    for (what, pattern) in [
        (
            "shading",
            format!(
                "<< /PatternType 2 /Shading {AXIAL} /Extend [true true] >> >> matrix makepattern"
            ),
        ),
        ("tiling", TILING.to_string()),
    ] {
        let rgba = render(&format!("{pattern} setpattern {STENCIL}"));
        assert_marked_with_pattern(&rgba, &format!("{what} imagemask"));
        // Painted where the stencil says, and only there.
        assert_ne!(at(&rgba, 60, 140), WHITE, "{what}");
        assert_ne!(at(&rgba, 140, 60), WHITE, "{what}");
        assert_eq!(at(&rgba, 140, 140), WHITE, "{what}");
        assert_eq!(at(&rgba, 60, 60), WHITE, "{what}");
    }
}

#[test]
fn a_stencil_mask_in_dictionary_form_is_painted_with_the_pattern() {
    let rgba = render(&format!(
        "{TILING} setpattern 20 20 translate 160 160 scale\n\
         << /ImageType 1 /Width 8 /Height 8 /ImageMatrix [8 0 0 -8 0 8] /BitsPerComponent 1 \
         /Decode [1 0] /DataSource <f0 f0 f0 f0 0f 0f 0f 0f> >> imagemask"
    ));
    assert_marked_with_pattern(&rgba, "dictionary imagemask");
    assert_ne!(at(&rgba, 60, 140), WHITE);
    assert_eq!(at(&rgba, 140, 140), WHITE);
}

#[test]
fn a_stencil_mask_without_a_pattern_is_painted_with_the_colour() {
    let rgba = render(&format!("0 1 0 setrgbcolor {STENCIL}"));
    assert_eq!(at(&rgba, 60, 140), [0, 255, 0]);
    assert_eq!(at(&rgba, 140, 140), WHITE);
}
