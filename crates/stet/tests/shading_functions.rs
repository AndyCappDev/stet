// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A shading's `Function` in the forms PLRM 3e §4.9.3 allows, in every
//! shading type that takes one.
//!
//! A mesh shading (Types 4 to 7) with a `Function` carries one parametric
//! value t per vertex, and the function gives the colour. stet read t as
//! colour components and drew nothing. It now gives the shading a colour
//! table over t, as the PDF reader does, so a function with several stops
//! is drawn through all of them, not just at the vertices.
//!
//! A `Function` may also be an array of one-output functions, one per
//! colour component. That was executed as a procedure and stopped with
//! `stackunderflow`.

use stet::Interpreter;
use stet_graphics::color::DeviceColor;
use stet_graphics::display_list::DisplayElement;

/// Red to green over t in [0, 0.5], green to blue over [0.5, 1].
const THREE_STOPS: &str = "<< /FunctionType 3 /Domain [0 1] /Bounds [0.5] /Encode [0 1 0 1] \
     /Functions [ << /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 1 0] /N 1 >> \
                  << /FunctionType 2 /Domain [0 1] /C0 [0 1 0] /C1 [0 0 1] /N 1 >> ] >>";

/// Red to blue.
const RED_TO_BLUE: &str = "<< /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1 >>";

fn elements(body: &str) -> Vec<DisplayElement> {
    let ps = format!("%!PS\n{body}\nshowpage\n");
    let mut interp = Interpreter::builder().suppress_output().build();
    let pages = interp
        .render_to_display_list(ps.as_bytes(), 72.0)
        .expect("renders");
    pages[0].display_list.elements().to_vec()
}

fn near(c: &DeviceColor, rgb: [f64; 3]) -> bool {
    (c.r - rgb[0]).abs() < 0.02 && (c.g - rgb[1]).abs() < 0.02 && (c.b - rgb[2]).abs() < 0.02
}

/// The mesh's colour table and each vertex's t, from the first mesh or
/// patch shading.
fn mesh_lut(els: &[DisplayElement]) -> (Vec<DeviceColor>, Vec<f64>) {
    for e in els {
        match e {
            DisplayElement::MeshShading { params } => {
                let ts = params
                    .triangles
                    .iter()
                    .flat_map(|t| [&t.v0, &t.v1, &t.v2])
                    .map(|v| v.raw_components[0])
                    .collect();
                return (
                    params.color_lut.as_deref().expect("a colour table").clone(),
                    ts,
                );
            }
            DisplayElement::PatchShading { params } => {
                let ts = params
                    .patches
                    .iter()
                    .flat_map(|p| p.raw_colors.iter())
                    .map(|c| c[0])
                    .collect();
                return (
                    params.color_lut.as_deref().expect("a colour table").clone(),
                    ts,
                );
            }
            _ => {}
        }
    }
    panic!("no mesh shading");
}

#[test]
fn a_triangle_mesh_with_a_function_goes_through_every_stop() {
    let (lut, ts) = mesh_lut(&elements(&format!(
        "<< /ShadingType 4 /ColorSpace /DeviceRGB /Function {THREE_STOPS} \
         /DataSource [ 0 0 0 0  0 0 100 0  0 100 0 1  1 100 100 1 ] >> shfill"
    )));
    assert_eq!(lut.len(), 256);
    assert!(near(&lut[0], [1.0, 0.0, 0.0]), "{:?}", lut[0]);
    assert!(near(&lut[255], [0.0, 0.0, 1.0]), "{:?}", lut[255]);
    // Halfway along t the colour is the middle stop, green, which the
    // vertex colours, red and blue, do not hold.
    assert!(
        lut[127].g > 0.95 && lut[128].g > 0.95,
        "{:?}",
        &lut[126..130]
    );
    assert!(ts.iter().all(|t| *t == 0.0 || *t == 1.0), "{ts:?}");
}

#[test]
fn a_mesh_read_from_a_string_takes_t_through_its_decode() {
    // 8-bit t over Decode [0 1]: 0x00, 0x80 and 0xFF.
    let (lut, ts) = mesh_lut(&elements(&format!(
        "<< /ShadingType 4 /ColorSpace /DeviceRGB /Function {RED_TO_BLUE} \
         /BitsPerCoordinate 8 /BitsPerComponent 8 /BitsPerFlag 8 /Decode [0 255 0 255 0 1] \
         /DataSource <00000000 00006480 003264FF> >> shfill"
    )));
    assert!(near(&lut[0], [1.0, 0.0, 0.0]), "{:?}", lut[0]);
    assert!(near(&lut[255], [0.0, 0.0, 1.0]), "{:?}", lut[255]);
    assert_eq!(ts.len(), 3);
    assert!((ts[1] - 128.0 / 255.0).abs() < 1e-6, "{ts:?}");
}

#[test]
fn a_patch_mesh_with_a_function_has_a_colour_table() {
    let (lut, ts) = mesh_lut(&elements(&format!(
        "<< /ShadingType 6 /ColorSpace /DeviceRGB /Function {RED_TO_BLUE} \
         /DataSource [ 0  0 0  0 33  0 67  0 100  33 100  67 100  100 100  100 67  100 33 \
                       100 0  67 0  33 0  0 0.5 1 0.25 ] >> shfill"
    )));
    assert!(near(&lut[0], [1.0, 0.0, 0.0]), "{:?}", lut[0]);
    assert!(near(&lut[255], [0.0, 0.0, 1.0]), "{:?}", lut[255]);
    assert_eq!(ts, [0.0, 0.5, 1.0, 0.25]);
}

#[test]
fn a_spot_mesh_with_a_function_runs_the_tint_transform() {
    // The full tint of the spot colour must come out as a fill of it does.
    let sep =
        "[/Separation /Spot /DeviceCMYK { dup 0.6 mul exch dup 0.9 mul exch 0 exch 0.5 mul }]";
    let els = elements(&format!(
        "{sep} setcolorspace 1 setcolor 0 0 10 10 rectfill \
         << /ShadingType 4 /ColorSpace {sep} \
         /Function << /FunctionType 2 /Domain [0 1] /C0 [0] /C1 [1] /N 1 >> \
         /DataSource [ 0 0 0 0  0 0 100 0.5  0 50 100 1 ] >> shfill"
    ));
    let fill = els
        .iter()
        .find_map(|e| match e {
            DisplayElement::Fill { params, .. } => Some(params.color.clone()),
            _ => None,
        })
        .expect("a fill");
    let (lut, _) = mesh_lut(&els);
    assert!(
        near(&lut[255], [fill.r, fill.g, fill.b]),
        "{:?} vs {fill:?}",
        lut[255]
    );
    assert!(
        near(&lut[0], [1.0, 1.0, 1.0]),
        "no tint is white: {:?}",
        lut[0]
    );
}

#[test]
fn an_array_of_functions_gives_one_component_each() {
    let els = elements(
        "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 100 0] \
         /Function [ << /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1 >> \
                     << /FunctionType 2 /Domain [0 1] /C0 [0] /C1 [0] /N 1 >> \
                     << /FunctionType 2 /Domain [0 1] /C0 [0] /C1 [1] /N 1 >> ] >> shfill",
    );
    let stops = els
        .iter()
        .find_map(|e| match e {
            DisplayElement::AxialShading { params } => Some(params.color_stops.clone()),
            _ => None,
        })
        .expect("an axial shading");
    assert!(
        near(&stops[0].color, [1.0, 0.0, 0.0]),
        "{:?}",
        stops[0].color
    );
    let last = &stops[stops.len() - 1].color;
    assert!(near(last, [0.0, 0.0, 1.0]), "{last:?}");
}

#[test]
fn a_mesh_with_an_array_of_functions_has_a_colour_table() {
    let (lut, _) = mesh_lut(&elements(
        "<< /ShadingType 4 /ColorSpace /DeviceRGB \
         /Function [ << /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1 >> \
                     << /FunctionType 2 /Domain [0 1] /C0 [0] /C1 [0] /N 1 >> \
                     << /FunctionType 2 /Domain [0 1] /C0 [0] /C1 [1] /N 1 >> ] \
         /DataSource [ 0 0 0 0  0 0 100 0.5  0 50 100 1 ] >> shfill",
    ));
    assert!(near(&lut[0], [1.0, 0.0, 0.0]), "{:?}", lut[0]);
    assert!(near(&lut[255], [0.0, 0.0, 1.0]), "{:?}", lut[255]);
}
