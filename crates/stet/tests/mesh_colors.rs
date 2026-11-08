// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Mesh shadings (Types 4-7) whose DataSource is a string or file gave every
// vertex the colour its component count suggested: one component became a
// gray, so a full Separation or DeviceN tint was white, and four went
// through the naive CMYK formula instead of the CMYK profile that fills and
// the other shadings use. Illustrator's gradient meshes are such shadings,
// in a Separation space read through a filter. Each vertex now takes the
// colour a fill in the same space takes.

use stet::Interpreter;
use stet_graphics::color::DeviceColor;
use stet_graphics::display_list::DisplayElement;

const SEP: &str = "[/Separation /Purple /DeviceCMYK \
                   { dup 0.6 mul exch dup 0.9 mul exch dup 0 mul exch 0.5 mul }]";

/// The colour of the first fill and the first mesh vertex of `body`.
fn colours(body: &str) -> (DeviceColor, DeviceColor) {
    let ps = format!("%!PS\n{body}\nshowpage\n");
    let mut interp = Interpreter::builder().suppress_output().build();
    let pages = interp
        .render_to_display_list(ps.as_bytes(), 72.0)
        .expect("renders");
    let mut fill = None;
    let mut vertex = None;
    for e in pages[0].display_list.elements() {
        match e {
            DisplayElement::Fill { params, .. } if fill.is_none() => {
                fill = Some(params.color.clone())
            }
            DisplayElement::MeshShading { params } if vertex.is_none() => {
                vertex = Some(params.triangles[0].v0.color.clone())
            }
            DisplayElement::PatchShading { params } if vertex.is_none() => {
                vertex = Some(params.patches[0].colors[0].clone())
            }
            _ => {}
        }
    }
    (fill.expect("a fill"), vertex.expect("a mesh"))
}

fn assert_same(fill: DeviceColor, vertex: DeviceColor) {
    for (f, v) in [(fill.r, vertex.r), (fill.g, vertex.g), (fill.b, vertex.b)] {
        assert!((f - v).abs() < 0.01, "fill {fill:?}, vertex {vertex:?}");
    }
}

/// A triangle (Type 4) with 16-bit coordinates over 0-400 and 8-bit
/// components, each vertex `comps` (hex).
fn triangle(space: &str, decode_comps: &str, comps: &str) -> String {
    let v = |x: &str, y: &str| format!("00{x}{y}{comps}");
    format!(
        "<< /ShadingType 4 /ColorSpace {space} /BitsPerFlag 8 /BitsPerCoordinate 16 \
         /BitsPerComponent 8 /Decode [0 400 0 400 {decode_comps}] /DataSource <{}{}{}> >> shfill",
        v("4000", "4000"),
        v("C000", "4000"),
        v("8000", "C000")
    )
}

#[test]
fn separation_mesh_runs_the_tint_transform() {
    let (fill, vertex) = colours(&format!(
        "{SEP} setcolorspace 1 setcolor 0 0 10 10 rectfill\n{}",
        triangle(SEP, "0 1", "ff")
    ));
    assert_same(fill, vertex);
}

#[test]
fn separation_mesh_from_a_filter_runs_the_tint_transform() {
    // The data through ASCII85Decode on the current file, as Illustrator
    // writes a gradient mesh.
    let shading = triangle(SEP, "0 1", "ff");
    let hex = shading
        .split("/DataSource <")
        .nth(1)
        .unwrap()
        .split('>')
        .next()
        .unwrap();
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect();
    let a85 = ascii85(&bytes);
    let dict = shading.replace(&format!("<{hex}>"), "currentfile /ASCII85Decode filter");
    let (fill, vertex) = colours(&format!(
        "{SEP} setcolorspace 1 setcolor 0 0 10 10 rectfill\n{dict}\n{a85}~>"
    ));
    assert_same(fill, vertex);
}

#[test]
fn cmyk_mesh_goes_through_the_profile() {
    let (fill, vertex) = colours(&format!(
        "0.6 0.9 0 0.5 setcmykcolor 0 0 10 10 rectfill\n{}",
        triangle("/DeviceCMYK", "0 1 0 1 0 1 0 1", "99e60080")
    ));
    assert_same(fill, vertex);
}

#[test]
fn devicen_patch_mesh_runs_the_tint_transform() {
    // A tensor-product patch (Type 7) over the square 100-300.
    let devn = "[/DeviceN [/Purple] /DeviceCMYK \
                { dup 0.6 mul exch dup 0.9 mul exch dup 0 mul exch 0.5 mul }]";
    let q = |v: f64| format!("{:04X}", (v / 400.0 * 65535.0).round() as u32);
    let at = |u: usize, v: usize| {
        format!(
            "{}{}",
            q(100.0 + 200.0 * u as f64 / 3.0),
            q(100.0 + 200.0 * v as f64 / 3.0)
        )
    };
    let order = [
        (0, 0),
        (0, 1),
        (0, 2),
        (0, 3),
        (1, 3),
        (2, 3),
        (3, 3),
        (3, 2),
        (3, 1),
        (3, 0),
        (2, 0),
        (1, 0),
        (1, 1),
        (1, 2),
        (2, 2),
        (2, 1),
    ];
    let mut data = String::from("00");
    for (u, v) in order {
        data.push_str(&at(u, v));
    }
    data.push_str("ffffffff");
    let (fill, vertex) = colours(&format!(
        "{devn} setcolorspace 1 setcolor 0 0 10 10 rectfill\n\
         << /ShadingType 7 /ColorSpace {devn} /BitsPerFlag 8 /BitsPerCoordinate 16 \
         /BitsPerComponent 8 /Decode [0 400 0 400 0 1] /DataSource <{data}> >> shfill"
    ));
    assert_same(fill, vertex);
}

fn ascii85(data: &[u8]) -> String {
    let mut out = String::new();
    for chunk in data.chunks(4) {
        let mut b = [0u8; 4];
        b[..chunk.len()].copy_from_slice(chunk);
        let mut n = u32::from_be_bytes(b);
        if chunk.len() == 4 && n == 0 {
            out.push('z');
            continue;
        }
        let mut c = [0u8; 5];
        for d in c.iter_mut().rev() {
            *d = (n % 85) as u8 + b'!';
            n /= 85;
        }
        out.push_str(std::str::from_utf8(&c[..chunk.len() + 1]).unwrap());
    }
    out
}
