// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PostScript CMYK colour is converted with the current rendering intent.
//!
//! A CMYK profile holds a table per intent, and a PostScript program has no
//! output intent, so its CMYK profile is a source like any other and each
//! colour takes the table `setrenderingintent` selects. stet converts a
//! colour when it is set; `setrenderingintent` converts the current colour
//! again, because the intent that applies is the one in force when it is
//! painted.
//!
//! The profiles are `stet-graphics/tests/data/cmyk_intent/`'s, whose
//! perceptual, colorimetric and saturation tables differ; `generate.py`
//! there describes them.

use stet::{DisplayElement, Interpreter};
use stet_graphics::color::DeviceColor;
use stet_graphics::icc::{BpcMode, IccCache, IccCacheOptions, IccRenderingIntent};

/// The default CMYK profile: no saturation table.
const SPLIT: &[u8] = include_bytes!("../../stet-graphics/tests/data/cmyk_intent/split.icc");
/// The ICCBased profile: a saturation table unlike both others.
const SPLIT_SAT: &[u8] = include_bytes!("../../stet-graphics/tests/data/cmyk_intent/split_sat.icc");

const PERCEPTUAL: IccRenderingIntent = IccRenderingIntent::Perceptual;
const RELCOL: IccRenderingIntent = IccRenderingIntent::RelativeColorimetric;
const SATURATION: IccRenderingIntent = IccRenderingIntent::Saturation;

fn cache() -> IccCache {
    IccCache::new_with_options(IccCacheOptions {
        bpc_mode: BpcMode::On,
        source_cmyk_profile: Some(SPLIT.to_vec()),
    })
}

/// `cmyk` through the default profile with `intent`.
fn default_profile(cmyk: [f64; 4], intent: IccRenderingIntent) -> [f64; 3] {
    let [c, m, y, k] = cmyk;
    let (r, g, b) = cache()
        .convert_cmyk_readonly_with_intent(c, m, y, k, intent)
        .unwrap();
    [r, g, b]
}

/// `cmyk` through `SPLIT_SAT` with `intent`.
fn iccbased_profile(cmyk: [f64; 4], intent: IccRenderingIntent) -> [f64; 3] {
    let mut cache = cache();
    let hash = cache.register_profile(SPLIT_SAT).unwrap();
    let (r, g, b) = cache
        .convert_color_readonly_with_intent(&hash, &cmyk, intent)
        .unwrap();
    [r, g, b]
}

/// `[/ICCBased <<…>>]` over `SPLIT_SAT`.
fn iccbased_space() -> String {
    let hex: String = SPLIT_SAT.iter().map(|b| format!("{b:02x}")).collect();
    format!("[/ICCBased << /N 4 /DataSource <{hex}> >>]")
}

/// Run `body` as a one-page program with `SPLIT` as the CMYK profile and
/// return the display list's elements.
fn elements(body: &str) -> Vec<DisplayElement> {
    let src = format!("%!PS-Adobe-3.0\n{body}\nshowpage\n");
    let mut interp = Interpreter::new();
    interp.context().icc_cache = cache();
    let mut pages = interp.render_to_display_list(src.as_bytes(), 72.0).unwrap();
    assert_eq!(pages.len(), 1);
    pages.remove(0).display_list.elements().to_vec()
}

/// The colour of every `Fill`, in paint order.
fn fill_colors(body: &str) -> Vec<[f64; 3]> {
    elements(body)
        .iter()
        .filter_map(|e| match e {
            DisplayElement::Fill { params, .. } => Some(rgb(&params.color)),
            _ => None,
        })
        .collect()
}

fn rgb(color: &DeviceColor) -> [f64; 3] {
    [color.r, color.g, color.b]
}

#[track_caller]
fn assert_rgb(got: [f64; 3], want: [f64; 3], what: &str) {
    let close = got.iter().zip(want).all(|(g, w)| (g - w).abs() < 1e-9);
    assert!(close, "{what}: got {got:?}, want {want:?}");
}

const BLACK: [f64; 4] = [0.0, 0.0, 0.0, 1.0];
const BLUE: [f64; 4] = [0.8, 0.4, 0.0, 0.1];

#[test]
fn the_profiles_tables_differ() {
    // Otherwise every test below passes whatever stet does.
    for cmyk in [BLACK, BLUE] {
        assert_ne!(
            default_profile(cmyk, PERCEPTUAL),
            default_profile(cmyk, RELCOL)
        );
        assert_ne!(
            iccbased_profile(cmyk, SATURATION),
            default_profile(cmyk, SATURATION)
        );
    }
}

#[test]
fn setcmykcolor_takes_the_intent() {
    let fills = fill_colors(
        "0.8 0.4 0 0.1 setcmykcolor 0 0 10 10 rectfill\n\
         /Perceptual setrenderingintent 0.8 0.4 0 0.1 setcmykcolor 0 0 10 10 rectfill\n\
         /Saturation setrenderingintent 0.8 0.4 0 0.1 setcmykcolor 0 0 10 10 rectfill",
    );
    assert_rgb(fills[0], default_profile(BLUE, RELCOL), "initial intent");
    assert_rgb(fills[1], default_profile(BLUE, PERCEPTUAL), "Perceptual");
    assert_rgb(fills[2], default_profile(BLUE, SATURATION), "Saturation");
}

#[test]
fn setrenderingintent_converts_the_current_colour_again() {
    let fills = fill_colors(
        "0 0 0 1 setcmykcolor\n\
         /Perceptual setrenderingintent 0 0 10 10 rectfill\n\
         /RelativeColorimetric setrenderingintent 0 0 10 10 rectfill",
    );
    assert_rgb(
        fills[0],
        default_profile(BLACK, PERCEPTUAL),
        "after Perceptual",
    );
    assert_rgb(fills[1], default_profile(BLACK, RELCOL), "and back");
}

#[test]
fn grestore_restores_the_intent_and_the_colour_together() {
    let fills = fill_colors(
        "0 0 0 1 setcmykcolor\n\
         gsave /Perceptual setrenderingintent 0 0 10 10 rectfill grestore\n\
         0 0 10 10 rectfill",
    );
    assert_rgb(fills[0], default_profile(BLACK, PERCEPTUAL), "inside gsave");
    assert_rgb(fills[1], default_profile(BLACK, RELCOL), "after grestore");
}

#[test]
fn iccbased_cmyk_takes_the_intent_through_its_own_profile() {
    let space = iccbased_space();
    let fills = fill_colors(&format!(
        "{space} setcolorspace\n\
         /Saturation setrenderingintent 0 0 10 10 rectfill\n\
         0.8 0.4 0 0.1 setcolor 0 0 10 10 rectfill\n\
         /Perceptual setrenderingintent 0 0 10 10 rectfill\n\
         {space} setcolorspace 0 0 10 10 rectfill"
    ));
    // The initial colour, set under RelativeColorimetric, follows the
    // intent like any other.
    assert_rgb(
        fills[0],
        iccbased_profile(BLACK, SATURATION),
        "initial colour",
    );
    assert_rgb(fills[1], iccbased_profile(BLUE, SATURATION), "setcolor");
    assert_rgb(
        fills[2],
        iccbased_profile(BLUE, PERCEPTUAL),
        "converted again",
    );
    assert_rgb(
        fills[3],
        iccbased_profile(BLACK, PERCEPTUAL),
        "new initial colour",
    );
}

#[test]
fn indexed_and_separation_cmyk_take_the_intent() {
    let fills = fill_colors(
        "[/Indexed /DeviceCMYK 0 <CC66001A>] setcolorspace 0 setcolor\n\
         /Perceptual setrenderingintent 0 0 10 10 rectfill\n\
         [/Separation /Spot /DeviceCMYK {dup 0.5 mul 0 0}] setcolorspace\n\
         1 setcolor /RelativeColorimetric setrenderingintent 0 0 10 10 rectfill\n\
         /Saturation setrenderingintent 0 0 10 10 rectfill",
    );
    let indexed = [0.8, 0.4, 0.0, 26.0 / 255.0];
    let spot = [1.0, 0.5, 0.0, 0.0];
    assert_rgb(fills[0], default_profile(indexed, PERCEPTUAL), "Indexed");
    assert_rgb(fills[1], default_profile(spot, RELCOL), "Separation");
    assert_rgb(
        fills[2],
        default_profile(spot, SATURATION),
        "converted again",
    );
}

#[test]
fn colours_that_never_met_a_cmyk_profile_are_left_alone() {
    // A process colorant behind an RGB alternate records CMYK for overprint,
    // but its colour is the alternate's, which has no profile to convert.
    let fills = fill_colors(
        "[/Separation /Cyan /DeviceRGB {1 exch sub dup 1}] setcolorspace\n\
         0.75 setcolor 0 0 10 10 rectfill\n\
         /Perceptual setrenderingintent 0 0 10 10 rectfill\n\
         0.2 0.4 0.6 setrgbcolor 0 0 10 10 rectfill\n\
         /Saturation setrenderingintent 0 0 10 10 rectfill",
    );
    assert_rgb(fills[0], [0.25, 0.25, 1.0], "Separation, RGB alternate");
    assert_rgb(fills[1], [0.25, 0.25, 1.0], "after setrenderingintent");
    assert_rgb(fills[3], [0.2, 0.4, 0.6], "DeviceRGB");
}

#[test]
fn an_uncoloured_patterns_colour_takes_the_intent() {
    let elements = elements(
        "/pat << /PatternType 1 /PaintType 2 /TilingType 1 /BBox [0 0 10 10]\n\
                 /XStep 10 /YStep 10 /PaintProc { pop 0 0 10 10 rectfill } >>\n\
             matrix makepattern def\n\
         [/Pattern /DeviceCMYK] setcolorspace 0 0 0 1 pat setcolor\n\
         /Perceptual setrenderingintent 0 0 10 10 rectfill",
    );
    let underlying = elements
        .iter()
        .find_map(|e| match e {
            DisplayElement::PatternFill { params } => params.underlying_color.as_ref(),
            _ => None,
        })
        .expect("a pattern fill with an underlying colour");
    assert_rgb(
        rgb(underlying),
        default_profile(BLACK, PERCEPTUAL),
        "underlying colour",
    );
}

#[test]
fn shading_colours_take_the_intent() {
    let elements = elements(
        "/Perceptual setrenderingintent\n\
         << /ShadingType 2 /ColorSpace /DeviceCMYK /Coords [0 0 10 0]\n\
            /Function << /FunctionType 2 /Domain [0 1] /N 1\n\
                         /C0 [0 0 0 1] /C1 [0.8 0.4 0 0.1] >> >> shfill",
    );
    let stops = elements
        .iter()
        .find_map(|e| match e {
            DisplayElement::AxialShading { params } => Some(&params.color_stops),
            _ => None,
        })
        .expect("an axial shading");
    assert_rgb(
        rgb(&stops.first().unwrap().color),
        default_profile(BLACK, PERCEPTUAL),
        "first stop",
    );
    assert_rgb(
        rgb(&stops.last().unwrap().color),
        default_profile(BLUE, PERCEPTUAL),
        "last stop",
    );
}

#[test]
fn images_converted_while_painting_take_the_intent() {
    // A masked image with a non-identity Decode is converted to RGB when it
    // is painted, so that MaskColor is compared with the undecoded samples
    // (PLRM 4.10.6). Other CMYK images keep their samples and record the
    // intent for the renderer.
    let elements = elements(
        "/Perceptual setrenderingintent /DeviceCMYK setcolorspace\n\
         << /ImageType 4 /Width 1 /Height 1 /BitsPerComponent 8\n\
            /Decode [1 0 1 0 1 0 1 0] /MaskColor [0 0 0 0]\n\
            /ImageMatrix [1 0 0 1 0 0] /DataSource <3399FFE6> >> image",
    );
    let data = elements
        .iter()
        .find_map(|e| match e {
            DisplayElement::Image { sample_data, .. } => Some(sample_data),
            _ => None,
        })
        .expect("an image");
    // 255 minus each sample.
    let decoded = [204.0, 102.0, 0.0, 25.0].map(|v| v / 255.0);
    let want = default_profile(decoded, PERCEPTUAL);
    for (ch, w) in want.iter().enumerate() {
        let w = (w * 255.0).round() as u8;
        assert!(
            data[ch].abs_diff(w) <= 1,
            "channel {ch}: got {}, want {w}",
            data[ch]
        );
    }
}
