// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Transfer functions, applied by the renderer to the final RGB of each
//! fully opaque paint (PLRM 7.3; ISO 32000-1 §10.5 and §11.7.5.2).
//!
//! Every test paints under an inverting function, `1 - x`, and checks the
//! pixel against the same paint with no function, so the expectations do
//! not depend on how the renderer converts a colour space.

use std::sync::{Arc, Mutex};

use stet_fonts::geometry::{Matrix, PathSegment, PsPath};
use stet_graphics::color::{DeviceColor, FillRule};
use stet_graphics::device::{
    AxialShadingParams, ColorStop, FillParams, ImageColorSpace, ImageParams, MeshShadingParams,
    PatchShadingParams, RadialShadingParams, ShadingColorSpace, ShadingPatch, ShadingTriangle,
    ShadingVertex, StrokeParams, TintLookupTable, TransferState,
};
use stet_graphics::display_list::{
    DisplayElement, DisplayList, GroupColorSpace, GroupParams, SoftMaskParams, SoftMaskSubtype,
};
use stet_render::{render_to_rgba, render_to_rgba_viewport};

const W: u32 = 80;
const H: u32 = 20;

fn table(f: impl Fn(f64) -> f64) -> Arc<Vec<f64>> {
    Arc::new((0..256).map(|i| f(i as f64 / 255.0)).collect())
}

/// `{1 exch sub} settransfer`.
fn invert() -> TransferState {
    TransferState {
        gray: Some(table(|x| 1.0 - x)),
        color: None,
    }
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> PsPath {
    PsPath {
        segments: vec![
            PathSegment::MoveTo(x, y),
            PathSegment::LineTo(x + w, y),
            PathSegment::LineTo(x + w, y + h),
            PathSegment::LineTo(x, y + h),
            PathSegment::ClosePath,
        ],
    }
}

fn fill(color: DeviceColor, transfer: TransferState) -> FillParams {
    FillParams {
        color,
        fill_rule: FillRule::NonZeroWinding,
        transfer,
        ..FillParams::default()
    }
}

fn fill_element(params: FillParams) -> DisplayElement {
    DisplayElement::Fill {
        path: rect(0.0, 0.0, W as f64, H as f64),
        params,
    }
}

fn list(elements: Vec<DisplayElement>) -> DisplayList {
    let mut list = DisplayList::new();
    for e in elements {
        list.push(e);
    }
    list
}

/// The RGB at `(x, y)`, from the banded renderer and from the viewport
/// renderer, which convert images through different caches; they must
/// agree.
fn pixel(list: &DisplayList, x: u32, y: u32) -> [u8; 3] {
    let at = |data: &[u8]| {
        let i = ((y * W + x) * 4) as usize;
        [data[i], data[i + 1], data[i + 2]]
    };
    let banded = at(&render_to_rgba(list, W, H, 72.0, None, false));
    let viewport = at(&render_to_rgba_viewport(list, W, H, 72.0, None, false));
    assert_eq!(banded, viewport, "banded and viewport renders disagree");
    banded
}

fn inverted(rgb: [u8; 3]) -> [u8; 3] {
    rgb.map(|v| 255 - v)
}

/// `paint` builds the element under the given function. Under the inverting
/// function every sampled pixel must be the inverse of its colour without
/// one, to within `tolerance` (a shading's samples are interpolated).
fn assert_inverts(
    name: &str,
    points: &[(u32, u32)],
    tolerance: u8,
    paint: impl Fn(TransferState) -> Vec<DisplayElement>,
) {
    let plain = list(paint(TransferState::default()));
    let under = list(paint(invert()));
    for &(x, y) in points {
        let want = inverted(pixel(&plain, x, y));
        let got = pixel(&under, x, y);
        let close = want
            .iter()
            .zip(&got)
            .all(|(a, b)| a.abs_diff(*b) <= tolerance);
        assert!(close, "{name} at ({x}, {y}): want {want:?}, got {got:?}");
    }
}

#[test]
fn fills_and_strokes_take_the_function() {
    let gray = DeviceColor::from_gray(0.2);
    assert_eq!(
        pixel(
            &list(vec![fill_element(fill(gray.clone(), invert()))]),
            10,
            10
        ),
        [204; 3]
    );
    // The CMYK of a paint is ink, which an RGB device's transfer function
    // leaves alone: only the RGB it paints changes.
    let k = DeviceColor::from_cmyk(0.0, 0.0, 0.0, 1.0);
    let params = fill(k.clone(), invert());
    assert_eq!(params.color.native_cmyk, k.native_cmyk);
    assert_eq!(pixel(&list(vec![fill_element(params)]), 10, 10), [255; 3]);

    let stroke = |transfer| {
        vec![DisplayElement::Stroke {
            path: PsPath {
                segments: vec![
                    PathSegment::MoveTo(0.0, 10.0),
                    PathSegment::LineTo(80.0, 10.0),
                ],
            },
            params: StrokeParams {
                color: gray.clone(),
                line_width: 8.0,
                transfer,
                ..StrokeParams::default()
            },
        }]
    };
    assert_inverts("stroke", &[(40, 10)], 0, stroke);
    assert_eq!(pixel(&list(stroke(invert())), 40, 10), [204; 3]);
}

#[test]
fn identity_leaves_the_page_unchanged() {
    let identity = TransferState {
        gray: Some(table(|x| x)),
        color: None,
    };
    let a = list(vec![fill_element(fill(
        DeviceColor::from_gray(0.3),
        identity,
    ))]);
    let b = list(vec![fill_element(fill(
        DeviceColor::from_gray(0.3),
        TransferState::default(),
    ))]);
    assert_eq!(
        render_to_rgba(&a, W, H, 72.0, None, false),
        render_to_rgba(&b, W, H, 72.0, None, false)
    );
}

/// `setcolortransfer` (or a four-function `/TR`) maps red, green and blue
/// each through its own function; the fourth, gray, is not used on an RGB
/// device.
#[test]
fn per_component_functions_apply_per_channel() {
    let transfer = TransferState {
        gray: Some(table(|_| 0.0)),
        color: Some([
            Some(table(|x| 1.0 - x)),
            None,
            Some(table(|x| x / 2.0)),
            Some(table(|_| 0.0)),
        ]),
    };
    let color = DeviceColor::from_rgb(0.2, 0.4, 0.8);
    assert_eq!(
        pixel(&list(vec![fill_element(fill(color, transfer))]), 10, 10),
        [204, 102, 102]
    );
}

fn image(
    color_space: ImageColorSpace,
    width: u32,
    samples: Vec<u8>,
    transfer: TransferState,
) -> DisplayElement {
    DisplayElement::Image {
        sample_data: Arc::new(samples),
        params: ImageParams {
            width,
            height: 1,
            color_space,
            bits_per_component: 8,
            ctm: Matrix::new(W as f64, 0.0, 0.0, H as f64, 0.0, 0.0),
            image_matrix: Matrix::new(width as f64, 0.0, 0.0, 1.0, 0.0, 0.0),
            transfer,
            ..ImageParams::default()
        },
    }
}

/// Each pixel of a two-pixel image, after conversion to RGB in whatever
/// space it is in.
const IMAGE_POINTS: [(u32, u32); 2] = [(20, 10), (60, 10)];

#[test]
fn images_take_the_function_after_conversion() {
    let gray_to_cmyk = Arc::new(TintLookupTable {
        num_inputs: 1,
        num_outputs: 4,
        samples_per_dim: 2,
        data: vec![0.0, 0.0, 0.0, 0.0, 0.2, 0.0, 0.6, 0.1],
    });
    let cases: Vec<(&str, ImageColorSpace, Vec<u8>)> = vec![
        ("DeviceGray", ImageColorSpace::DeviceGray, vec![200, 55]),
        (
            "DeviceRGB",
            ImageColorSpace::DeviceRGB,
            vec![200, 100, 20, 55, 0, 255],
        ),
        (
            "DeviceCMYK",
            ImageColorSpace::DeviceCMYK,
            vec![0, 0, 0, 0, 30, 60, 0, 255],
        ),
        (
            "Indexed",
            ImageColorSpace::Indexed {
                base: Box::new(ImageColorSpace::DeviceRGB),
                hival: 1,
                lookup: vec![255, 0, 0, 0, 80, 160],
            },
            vec![1, 0],
        ),
        (
            "Separation",
            ImageColorSpace::Separation {
                name: b"Spot".to_vec(),
                alt_space: Box::new(ImageColorSpace::DeviceCMYK),
                tint_table: gray_to_cmyk,
            },
            vec![255, 60],
        ),
    ];
    for (name, cs, samples) in cases {
        assert_inverts(name, &IMAGE_POINTS, 0, |transfer| {
            vec![image(cs.clone(), 2, samples.clone(), transfer)]
        });
    }
}

/// A stencil mask paints the current colour, which takes the function.
#[test]
fn stencil_masks_take_the_function() {
    let mask = |transfer| {
        vec![DisplayElement::Image {
            sample_data: Arc::new(vec![0b1000_0000]),
            params: ImageParams {
                bits_per_component: 1,
                ..match image(
                    ImageColorSpace::Mask {
                        color: DeviceColor::from_gray(0.2),
                        polarity: true,
                        spot_color: None,
                    },
                    2,
                    vec![],
                    transfer,
                ) {
                    DisplayElement::Image { params, .. } => params,
                    _ => unreachable!(),
                }
            },
        }]
    };
    let painted = |list: &DisplayList| IMAGE_POINTS.map(|(x, y)| pixel(list, x, y));
    let plain = painted(&list(mask(TransferState::default())));
    let under = painted(&list(mask(invert())));
    // One pixel paints 0.2, the other leaves the paper alone.
    let (paint, paper) = if plain[0] == [255; 3] { (1, 0) } else { (0, 1) };
    assert_eq!(plain[paint], [51; 3]);
    assert_eq!(under[paint], [204; 3]);
    assert_eq!(under[paper], [255; 3]);
}

/// Premultiplied RGBA (a front end's own conversion, or an image merged
/// with a stencil `/Mask`): the function applies to each pixel's colour,
/// not to its product with alpha, and clear pixels stay clear.
#[test]
fn premultiplied_images_take_the_function_on_their_colour() {
    let rgba = |transfer| {
        // An opaque 0.2 gray, and a fully clear pixel.
        vec![image(
            ImageColorSpace::PreconvertedRGBA,
            2,
            vec![51, 51, 51, 255, 0, 0, 0, 0],
            transfer,
        )]
    };
    let under = list(rgba(invert()));
    assert_eq!(pixel(&under, 20, 10), [204; 3]);
    assert_eq!(pixel(&under, 60, 10), [255; 3], "a clear pixel stays clear");

    // Half alpha over white: 0.2 inverted is 0.8, which over white at
    // half alpha is 0.9. Applying the function to the premultiplied value
    // (0.1) would give 0.9 + 0.5 = past white.
    let half = list(vec![image(
        ImageColorSpace::PreconvertedRGBA,
        1,
        vec![26, 26, 26, 128],
        invert(),
    )]);
    let [v, ..] = pixel(&half, 40, 10);
    assert!(v.abs_diff(229) <= 1, "half alpha: got {v}");
}

fn stops(from: f64, to: f64) -> Vec<ColorStop> {
    [(0.0, from), (1.0, to)]
        .into_iter()
        .map(|(position, gray)| ColorStop {
            position,
            color: DeviceColor::from_gray(gray),
            raw_components: vec![gray],
            ..ColorStop::default()
        })
        .collect()
}

const SHADING_POINTS: [(u32, u32); 3] = [(10, 10), (40, 10), (70, 10)];

#[test]
fn axial_shadings_take_the_function_at_each_colour() {
    assert_inverts("axial", &SHADING_POINTS, 1, |transfer| {
        vec![DisplayElement::AxialShading {
            params: AxialShadingParams {
                x0: 0.0,
                y0: 0.0,
                x1: W as f64,
                y1: 0.0,
                color_stops: stops(0.1, 0.9),
                extend_start: true,
                extend_end: true,
                color_space: ShadingColorSpace::DeviceGray,
                transfer,
                ..AxialShadingParams::default()
            },
        }]
    });
    // A diagonal axis with an unextended end is drawn by a different path
    // (tiny-skia's gradient, between stops), which must sample the function
    // across each span rather than only at the stops.
    let curve = TransferState {
        gray: Some(table(|x| x * x)),
        color: None,
    };
    let diagonal = |transfer| {
        list(vec![DisplayElement::AxialShading {
            params: AxialShadingParams {
                x0: 0.0,
                y0: 0.0,
                x1: W as f64,
                y1: 4.0,
                color_stops: stops(0.0, 1.0),
                extend_start: true,
                extend_end: false,
                color_space: ShadingColorSpace::DeviceGray,
                transfer,
                ..AxialShadingParams::default()
            },
        }])
    };
    let plain = diagonal(TransferState::default());
    let under = diagonal(curve);
    let [p, ..] = pixel(&plain, 40, 10);
    let [u, ..] = pixel(&under, 40, 10);
    let want = ((p as f64 / 255.0).powi(2) * 255.0).round() as u8;
    assert!(
        u.abs_diff(want) <= 2,
        "diagonal: plain {p}, want {want}, got {u}"
    );
}

#[test]
fn radial_shadings_take_the_function_at_each_colour() {
    assert_inverts("radial", &SHADING_POINTS, 1, |transfer| {
        vec![DisplayElement::RadialShading {
            params: RadialShadingParams {
                x0: 40.0,
                y0: 10.0,
                r0: 0.0,
                x1: 40.0,
                y1: 10.0,
                r1: 45.0,
                color_stops: stops(0.1, 0.9),
                extend_start: true,
                extend_end: true,
                color_space: ShadingColorSpace::DeviceGray,
                transfer,
                ..RadialShadingParams::default()
            },
        }]
    });
}

fn vertex(x: f64, y: f64, gray: f64) -> ShadingVertex {
    ShadingVertex {
        x,
        y,
        color: DeviceColor::from_gray(gray),
        raw_components: vec![gray],
    }
}

#[test]
fn mesh_shadings_take_the_function_at_each_pixel() {
    // Under a nonlinear function, the function of an interpolated colour
    // is not the interpolation of the functions of the vertex colours.
    let curve = || TransferState {
        gray: Some(table(|x| x * x)),
        color: None,
    };
    let mesh = |transfer| {
        list(vec![DisplayElement::MeshShading {
            params: MeshShadingParams {
                triangles: vec![
                    ShadingTriangle {
                        v0: vertex(0.0, -10.0, 0.0),
                        v1: vertex(80.0, -10.0, 1.0),
                        v2: vertex(0.0, 40.0, 0.0),
                    },
                    ShadingTriangle {
                        v0: vertex(80.0, -10.0, 1.0),
                        v1: vertex(80.0, 40.0, 1.0),
                        v2: vertex(0.0, 40.0, 0.0),
                    },
                ],
                color_space: ShadingColorSpace::DeviceGray,
                transfer,
                ..MeshShadingParams::default()
            },
        }])
    };
    assert_squares(
        "mesh",
        &mesh(TransferState::default()),
        &mesh(curve()),
        &SHADING_POINTS,
    );
}

#[test]
fn patch_shadings_take_the_function_at_each_pixel() {
    let curve = || TransferState {
        gray: Some(table(|x| x * x)),
        color: None,
    };
    // A Coons patch whose sides are straight: the unit square, scaled over
    // the page, dark on the left and light on the right.
    let edge = |(x0, y0): (f64, f64), (x1, y1): (f64, f64)| {
        [
            (x0 + (x1 - x0) / 3.0, y0 + (y1 - y0) / 3.0),
            (x0 + 2.0 * (x1 - x0) / 3.0, y0 + 2.0 * (y1 - y0) / 3.0),
        ]
    };
    let corners = [(0.0, -10.0), (0.0, 40.0), (80.0, 40.0), (80.0, -10.0)];
    let mut points = Vec::new();
    for i in 0..4 {
        let a = corners[i];
        let b = corners[(i + 1) % 4];
        points.push(a);
        points.extend(edge(a, b));
    }
    let gray = [0.0, 0.0, 1.0, 1.0];
    let patch = |transfer| {
        list(vec![DisplayElement::PatchShading {
            params: PatchShadingParams {
                patches: vec![ShadingPatch {
                    points: points.clone(),
                    colors: gray.map(DeviceColor::from_gray),
                    raw_colors: gray.map(|g| vec![g]),
                }],
                color_space: ShadingColorSpace::DeviceGray,
                transfer,
                ..PatchShadingParams::default()
            },
        }])
    };
    assert_squares(
        "patch",
        &patch(TransferState::default()),
        &patch(curve()),
        &SHADING_POINTS,
    );
}

/// Under `x²`, each pixel must be the square of its value with no function.
fn assert_squares(name: &str, plain: &DisplayList, under: &DisplayList, points: &[(u32, u32)]) {
    for &(x, y) in points {
        let [p, ..] = pixel(plain, x, y);
        let [u, ..] = pixel(under, x, y);
        let want = ((p as f64 / 255.0).powi(2) * 255.0).round() as u8;
        assert!(
            u.abs_diff(want) <= 2,
            "{name} at ({x}, {y}): plain {p}, want {want}, got {u}"
        );
    }
}

// ---------------------------------------------------------------------------
// Only fully opaque paints take their function (ISO 32000-1 §11.7.5.2).
// Gray 0.2 at half alpha over white is 153 with no function, 229 with it.
// ---------------------------------------------------------------------------

fn group(elements: Vec<DisplayElement>, alpha: f64, blend_mode: u8) -> DisplayElement {
    DisplayElement::Group {
        elements: list(elements),
        params: GroupParams {
            bbox: [0.0, 0.0, W as f64, H as f64],
            isolated: false,
            knockout: false,
            blend_mode,
            alpha,
            color_space: GroupColorSpace::Inherited,
        },
    }
}

#[test]
fn transparent_paints_take_no_function() {
    let half = FillParams {
        alpha: 0.5,
        ..fill(DeviceColor::from_gray(0.2), invert())
    };
    assert_eq!(pixel(&list(vec![fill_element(half)]), 10, 10), [153; 3]);

    // Multiply over an opaque 0.6 that takes its function (0.4): 0.4 × 0.2.
    let backdrop = fill_element(fill(DeviceColor::from_gray(0.6), invert()));
    let multiply = FillParams {
        blend_mode: 1,
        ..fill(DeviceColor::from_gray(0.2), invert())
    };
    let [v, ..] = pixel(&list(vec![backdrop, fill_element(multiply)]), 10, 10);
    assert!(v.abs_diff(20) <= 1, "multiply: got {v}");

    let half_image = |transfer| match image(ImageColorSpace::DeviceGray, 1, vec![51], transfer) {
        DisplayElement::Image {
            sample_data,
            params,
        } => DisplayElement::Image {
            sample_data,
            params: ImageParams {
                alpha: 0.5,
                ..params
            },
        },
        _ => unreachable!(),
    };
    assert_eq!(pixel(&list(vec![half_image(invert())]), 40, 10), [153; 3]);
}

#[test]
fn paints_in_a_transparent_group_take_no_function() {
    let opaque = || fill_element(fill(DeviceColor::from_gray(0.2), invert()));
    // The group is drawn at half alpha: its content is not fully opaque.
    assert_eq!(
        pixel(&list(vec![group(vec![opaque()], 0.5, 0)]), 10, 10),
        [153; 3]
    );
    // Nor under a blend mode.
    assert_eq!(
        pixel(&list(vec![group(vec![opaque()], 1.0, 1)]), 10, 10),
        [51; 3]
    );
    // Nested: the outer group decides for everything inside it.
    assert_eq!(
        pixel(
            &list(vec![group(vec![group(vec![opaque()], 1.0, 0)], 0.5, 0)]),
            10,
            10
        ),
        [153; 3]
    );
    // An opaque group's opaque content takes its function.
    assert_eq!(
        pixel(&list(vec![group(vec![opaque()], 1.0, 0)]), 10, 10),
        [204; 3]
    );
}

fn soft_masked(mask: Vec<DisplayElement>, content: Vec<DisplayElement>) -> DisplayElement {
    DisplayElement::SoftMasked {
        mask: list(mask),
        content: list(content),
        params: SoftMaskParams {
            subtype: SoftMaskSubtype::Luminosity,
            bbox: [0.0, 0.0, W as f64, H as f64],
            backdrop_color: None,
            transfer_invert: false,
            has_nested_mask_scope: false,
            parent_clip_bbox: None,
        },
        mask_cache: Arc::new(Mutex::new(None)),
    }
}

#[test]
fn soft_masks_take_no_function() {
    // Content under a soft mask is not fully opaque.
    let mask = || fill_element(fill(DeviceColor::from_gray(0.5), TransferState::default()));
    let content = fill_element(fill(DeviceColor::from_gray(0.2), invert()));
    let [v, ..] = pixel(
        &list(vec![soft_masked(vec![mask()], vec![content])]),
        10,
        10,
    );
    assert!(v.abs_diff(153) <= 1, "masked content: got {v}");

    // A function in the mask's own content is not a final colour: the mask
    // stays 0.25, and black through it is 0.75 white.
    let mask = fill_element(fill(DeviceColor::from_gray(0.25), invert()));
    let black = fill_element(fill(DeviceColor::from_gray(0.0), TransferState::default()));
    let [v, ..] = pixel(&list(vec![soft_masked(vec![mask], vec![black])]), 10, 10);
    assert!(v.abs_diff(191) <= 1, "function in the mask: got {v}");
}
