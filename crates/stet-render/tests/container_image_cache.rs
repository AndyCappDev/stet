// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Images inside transparency groups and soft masks find their own entries
//! in the banded renderer's per-page image cache.
//!
//! The banded renderer converts and prescales each image once per page. An
//! image inside a group or a soft mask is at a position within that
//! container's list, so it must be handed the container's entries: with the
//! page's it would draw whichever image shares its position, and with none
//! every band converts the whole image for itself. A soft-masked element has
//! two lists, its content and its mask, each with its own entries.
//!
//! The viewport renderer run without a cache converts every image as it
//! draws it, and is the reference each banded render is held to.

use std::sync::{Arc, Mutex};

use stet_fonts::geometry::Matrix;
use stet_graphics::device::{ImageColorSpace, ImageParams, TransferState};
use stet_graphics::display_list::{
    DisplayElement, DisplayList, GroupColorSpace, GroupParams, OcgVisibility, SoftMaskParams,
    SoftMaskSubtype,
};
use stet_render::{render_to_rgba, render_to_rgba_viewport};

const W: u32 = 80;
const H: u32 = 20;
/// Each image fills one 20-point stripe, numbered from the left.
const STRIPE: u32 = 20;

const RED: [u8; 3] = [255, 0, 0];
const GREEN: [u8; 3] = [0, 255, 0];
const BLUE: [u8; 3] = [0, 0, 255];
const PAPER: [u8; 3] = [255; 3];

fn list(elements: Vec<DisplayElement>) -> DisplayList {
    let mut list = DisplayList::new();
    for e in elements {
        list.push(e);
    }
    list
}

/// A one-pixel image of `samples` in `color_space`, filling `stripes`
/// stripes from stripe `stripe`.
fn image_in(
    color_space: ImageColorSpace,
    samples: Vec<u8>,
    stripe: u32,
    stripes: u32,
    transfer: TransferState,
) -> DisplayElement {
    DisplayElement::Image {
        sample_data: Arc::new(samples),
        params: ImageParams {
            width: 1,
            height: 1,
            color_space,
            bits_per_component: 8,
            ctm: Matrix::new(
                (stripes * STRIPE) as f64,
                0.0,
                0.0,
                H as f64,
                (stripe * STRIPE) as f64,
                0.0,
            ),
            image_matrix: Matrix::new(1.0, 0.0, 0.0, 1.0, 0.0, 0.0),
            transfer,
            ..ImageParams::default()
        },
    }
}

/// A one-pixel DeviceRGB image of `rgb` filling stripe `stripe`.
fn image(rgb: [u8; 3], stripe: u32) -> DisplayElement {
    image_in(
        ImageColorSpace::DeviceRGB,
        rgb.to_vec(),
        stripe,
        1,
        TransferState::default(),
    )
}

/// A one-pixel DeviceGray image of `gray` over the whole page: a soft
/// mask's mask.
fn mask_image(gray: u8) -> DisplayElement {
    image_in(
        ImageColorSpace::DeviceGray,
        vec![gray],
        0,
        W / STRIPE,
        TransferState::default(),
    )
}

/// A one-pixel DeviceGray image of `gray` in stripe `stripe` whose transfer
/// function inverts it.
fn inverting_image(gray: u8, stripe: u32) -> DisplayElement {
    let invert = TransferState {
        gray: Some(Arc::new((0..256).map(|i| 1.0 - i as f64 / 255.0).collect())),
        color: None,
    };
    image_in(ImageColorSpace::DeviceGray, vec![gray], stripe, 1, invert)
}

fn group_with(
    elements: Vec<DisplayElement>,
    alpha: f64,
    blend_mode: u8,
    knockout: bool,
) -> DisplayElement {
    DisplayElement::Group {
        elements: list(elements),
        params: GroupParams {
            bbox: [0.0, 0.0, W as f64, H as f64],
            isolated: true,
            knockout,
            blend_mode,
            alpha,
            color_space: GroupColorSpace::Inherited,
        },
    }
}

/// An opaque, Normal-blend group.
fn group(elements: Vec<DisplayElement>) -> DisplayElement {
    group_with(elements, 1.0, 0, false)
}

fn knockout_group(elements: Vec<DisplayElement>) -> DisplayElement {
    group_with(elements, 1.0, 0, true)
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

fn layer(elements: Vec<DisplayElement>) -> DisplayElement {
    DisplayElement::OcgGroup {
        elements: list(elements),
        visibility: OcgVisibility::single(1, true),
    }
}

/// The banded render of `list`, which must equal the uncached viewport
/// render of it byte for byte.
fn render(list: &DisplayList, w: u32, h: u32) -> Vec<u8> {
    let banded = render_to_rgba(list, w, h, 72.0, None, false);
    let uncached = render_to_rgba_viewport(list, w, h, 72.0, None, false);
    assert!(
        banded == uncached,
        "the banded render differs from the uncached one"
    );
    banded
}

/// The colour at the centre of each stripe.
fn stripes(list: &DisplayList) -> Vec<[u8; 3]> {
    let data = render(list, W, H);
    (0..W / STRIPE)
        .map(|stripe| {
            let i = (((H / 2) * W + stripe * STRIPE + STRIPE / 2) * 4) as usize;
            [data[i], data[i + 1], data[i + 2]]
        })
        .collect()
}

/// A top-level image and an image inside a group at the same index: each
/// draws its own.
#[test]
fn a_group_image_does_not_take_the_top_level_image_at_its_index() {
    let page = list(vec![image(RED, 0), group(vec![image(BLUE, 1)])]);
    assert_eq!(stripes(&page), [RED, BLUE, PAPER, PAPER]);
}

/// Two groups deep, with an image at index 0 of every list.
#[test]
fn nested_groups_take_their_own_entries() {
    let page = list(vec![
        image(RED, 0),
        group(vec![image(GREEN, 1), group(vec![image(BLUE, 2)])]),
    ]);
    assert_eq!(stripes(&page), [RED, GREEN, BLUE, PAPER]);
}

/// A knockout group draws its elements one at a time; each still finds the
/// entry at its own index, in the group's entries and not the page's.
#[test]
fn each_image_in_a_knockout_group_takes_its_own_entry() {
    let page = list(vec![
        image(RED, 0),
        knockout_group(vec![image(GREEN, 1), image(BLUE, 2)]),
    ]);
    assert_eq!(stripes(&page), [RED, GREEN, BLUE, PAPER]);
}

/// A soft mask's content does not take the page's entries, nor its mask's:
/// the mask image and the content image are both at index 0 of their lists,
/// and a top-level image is at index 0 of the page.
#[test]
fn soft_masked_content_takes_its_own_entries() {
    let page = list(vec![
        image(RED, 0),
        soft_masked(vec![mask_image(255)], vec![image(BLUE, 1)]),
    ]);
    assert_eq!(stripes(&page), [RED, BLUE, PAPER, PAPER]);
}

/// Nor does the mask take its content's entries: a black mask hides the
/// content altogether, where the content's blue image as the mask would let
/// some of it through.
#[test]
fn a_soft_mask_takes_its_own_entries() {
    let page = list(vec![soft_masked(vec![mask_image(0)], vec![image(BLUE, 1)])]);
    assert_eq!(stripes(&page), [PAPER; 4]);
    // And a mask of two images takes each at its own index.
    let half = |gray, stripe| {
        image_in(
            ImageColorSpace::DeviceGray,
            vec![gray],
            stripe,
            1,
            TransferState::default(),
        )
    };
    let page = list(vec![soft_masked(
        vec![half(255, 0), half(0, 1)],
        vec![image(BLUE, 0), image(BLUE, 1)],
    )]);
    assert_eq!(stripes(&page), [BLUE, PAPER, PAPER, PAPER]);
}

/// Containers inside containers: a group inside a soft mask's content, and
/// a soft-masked image inside a group inside a layer.
#[test]
fn containers_nest() {
    let page = list(vec![
        image(RED, 0),
        soft_masked(
            vec![mask_image(255)],
            vec![image(GREEN, 1), group(vec![image(BLUE, 2)])],
        ),
    ]);
    assert_eq!(stripes(&page), [RED, GREEN, BLUE, PAPER]);

    let page = list(vec![
        image(RED, 0),
        layer(vec![
            image(GREEN, 1),
            group(vec![soft_masked(
                vec![mask_image(255)],
                vec![image(BLUE, 2)],
            )]),
        ]),
    ]);
    assert_eq!(stripes(&page), [RED, GREEN, BLUE, PAPER]);
}

/// A soft mask inside a group that does not start at the page's edge is
/// rendered into the group's offscreen by a path of its own; its mask's
/// images still come from the mask's entries, each at its own index.
#[test]
fn a_soft_mask_in_an_offset_group_takes_its_own_entries() {
    let gray = |gray, stripe| {
        image_in(
            ImageColorSpace::DeviceGray,
            vec![gray],
            stripe,
            1,
            TransferState::default(),
        )
    };
    let masked = soft_masked(
        vec![gray(255, 1), gray(0, 2)],
        vec![image(BLUE, 1), image(GREEN, 2)],
    );
    let offset_group = DisplayElement::Group {
        elements: list(vec![masked]),
        params: GroupParams {
            bbox: [STRIPE as f64, 0.0, W as f64, H as f64],
            isolated: true,
            knockout: false,
            blend_mode: 0,
            alpha: 1.0,
            color_space: GroupColorSpace::Inherited,
        },
    };
    let page = list(vec![image(RED, 0), offset_group]);
    assert_eq!(stripes(&page), [RED, BLUE, PAPER, PAPER]);
}

/// An image is converted for the cache as its context draws it: its
/// transfer function applies in an opaque group and nowhere that is not
/// fully opaque (ISO 32000-1 §11.7.5.2).
#[test]
fn cached_images_take_their_transfer_function_only_where_opaque() {
    let gray = |v: u8| [v; 3];
    // Opaque group: the function inverts 51 to 204.
    let page = list(vec![group(vec![inverting_image(51, 0)])]);
    assert_eq!(stripes(&page)[0], gray(204));
    // Half alpha: no function, and 51 at half over white is 153.
    let page = list(vec![group_with(
        vec![inverting_image(51, 0)],
        0.5,
        0,
        false,
    )]);
    assert_eq!(stripes(&page)[0], gray(153));
    // A blend mode (Multiply, over white): no function.
    let page = list(vec![group_with(
        vec![inverting_image(51, 0)],
        1.0,
        1,
        false,
    )]);
    assert_eq!(stripes(&page)[0], gray(51));
    // The outer group decides for an opaque group inside it.
    let page = list(vec![group_with(
        vec![group(vec![inverting_image(51, 0)])],
        0.5,
        0,
        false,
    )]);
    assert_eq!(stripes(&page)[0], gray(153));
    // A knockout group at half alpha likewise.
    let page = list(vec![group_with(vec![inverting_image(51, 0)], 0.5, 0, true)]);
    assert_eq!(stripes(&page)[0], gray(153));
    // Soft-masked content: no function.
    let page = list(vec![soft_masked(
        vec![mask_image(255)],
        vec![inverting_image(51, 0)],
    )]);
    assert_eq!(stripes(&page)[0], gray(51));
    // A function on the mask's own image is not applied either: the mask
    // stays 64, and black through it leaves 191 of the paper.
    let page = list(vec![soft_masked(
        vec![inverting_image(64, 0)],
        vec![image([0; 3], 0)],
    )]);
    let [v, ..] = stripes(&page)[0];
    assert!(v.abs_diff(191) <= 1, "function on the mask image: got {v}");
}

/// A large image drawn small inside a soft mask, on a page tall enough to
/// render in several bands: each band draws the one prescaled image at its
/// own offset, and the page equals the uncached render.
#[test]
fn a_prescaled_image_in_a_soft_mask_spans_bands() {
    const PAGE_W: u32 = 2000;
    const PAGE_H: u32 = 640;
    const IMG: u32 = 1500;
    // A gradient in both directions, so a band drawing the wrong rows or a
    // filter fed the wrong scale shows.
    let mut samples = Vec::with_capacity((IMG * IMG * 3) as usize);
    for y in 0..IMG {
        for x in 0..IMG {
            samples.extend([
                (x * 255 / IMG) as u8,
                (y * 255 / IMG) as u8,
                ((x + y) % 251) as u8,
            ]);
        }
    }
    let mask: Vec<u8> = (0..IMG * IMG)
        .map(|i| (i % IMG * 255 / IMG) as u8)
        .collect();
    let placed = |color_space, samples: Vec<u8>| DisplayElement::Image {
        sample_data: Arc::new(samples),
        params: ImageParams {
            width: IMG,
            height: IMG,
            color_space,
            bits_per_component: 8,
            // 600 × 600 device pixels, 20 in from the page's corner.
            ctm: Matrix::new(600.0, 0.0, 0.0, 600.0, 20.0, 20.0),
            image_matrix: Matrix::new(IMG as f64, 0.0, 0.0, -(IMG as f64), 0.0, IMG as f64),
            ..ImageParams::default()
        },
    };
    let page = list(vec![DisplayElement::SoftMasked {
        mask: list(vec![placed(ImageColorSpace::DeviceGray, mask)]),
        content: list(vec![placed(ImageColorSpace::DeviceRGB, samples)]),
        params: SoftMaskParams {
            subtype: SoftMaskSubtype::Luminosity,
            bbox: [20.0, 20.0, 620.0, 620.0],
            backdrop_color: None,
            transfer_invert: false,
            has_nested_mask_scope: false,
            parent_clip_bbox: None,
        },
        mask_cache: Arc::new(Mutex::new(None)),
    }]);
    let data = render(&page, PAGE_W, PAGE_H);
    // The image is there: its right-hand side, where the mask is light,
    // differs from the paper in every band.
    for y in [60, 200, 330, 460, 590] {
        let i = ((y * PAGE_W + 560) * 4) as usize;
        assert_ne!(data[i..i + 3], PAPER, "nothing drawn at row {y}");
    }
}

/// One image placed many times shares a conversion between the placements
/// that come out as the same pixels. Each placement still draws its own way
/// round and at its own size: mirrored, flipped, larger, inside a soft mask.
#[test]
fn placements_sharing_a_conversion_each_draw_their_own_way() {
    const PAGE_W: u32 = 400;
    const PAGE_H: u32 = 120;
    const IMG: u32 = 200;
    let samples: Arc<Vec<u8>> = Arc::new(
        (0..IMG * IMG)
            .flat_map(|i| {
                let (x, y) = (i % IMG, i / IMG);
                [(x * 255 / IMG) as u8, (y * 255 / IMG) as u8, 90]
            })
            .collect(),
    );
    let placed = |sx: f64, sy: f64, x: f64, y: f64| DisplayElement::Image {
        sample_data: Arc::clone(&samples),
        params: ImageParams {
            width: IMG,
            height: IMG,
            color_space: ImageColorSpace::DeviceRGB,
            bits_per_component: 8,
            ctm: Matrix::new(sx, 0.0, 0.0, sy, x, y),
            image_matrix: Matrix::new(IMG as f64, 0.0, 0.0, IMG as f64, 0.0, 0.0),
            ..ImageParams::default()
        },
    };
    let mask = DisplayElement::Image {
        sample_data: Arc::new(vec![200]),
        params: ImageParams {
            width: 1,
            height: 1,
            color_space: ImageColorSpace::DeviceGray,
            bits_per_component: 8,
            ctm: Matrix::new(PAGE_W as f64, 0.0, 0.0, PAGE_H as f64, 0.0, 0.0),
            image_matrix: Matrix::new(1.0, 0.0, 0.0, 1.0, 0.0, 0.0),
            ..ImageParams::default()
        },
    };
    let page = list(vec![
        placed(50.0, 50.0, 5.0, 5.0),
        placed(50.0, 50.0, 60.0, 5.0),
        // Mirrored, flipped, and both: the same pixels, drawn turned.
        placed(-50.0, 50.0, 165.0, 5.0),
        placed(50.0, -50.0, 170.0, 55.0),
        placed(-50.0, -50.0, 275.0, 55.0),
        // Another size, twice.
        placed(80.0, 40.0, 280.0, 5.0),
        placed(80.0, 40.0, 5.0, 65.0),
        // The first size again, inside containers.
        group(vec![placed(50.0, 50.0, 90.0, 65.0)]),
        DisplayElement::SoftMasked {
            mask: list(vec![mask]),
            content: list(vec![placed(50.0, 50.0, 145.0, 65.0)]),
            params: SoftMaskParams {
                subtype: SoftMaskSubtype::Luminosity,
                bbox: [0.0, 0.0, PAGE_W as f64, PAGE_H as f64],
                backdrop_color: None,
                transfer_invert: false,
                has_nested_mask_scope: false,
                parent_clip_bbox: None,
            },
            mask_cache: Arc::new(Mutex::new(None)),
        },
    ]);
    let data = render(&page, PAGE_W, PAGE_H);
    let at = |x: u32, y: u32| {
        let i = ((y * PAGE_W + x) * 4) as usize;
        [data[i], data[i + 1], data[i + 2]]
    };
    // The gradient runs left to right in the first placement and right to
    // left in the mirrored one.
    assert!(at(10, 30)[0] < at(50, 30)[0], "the plain placement");
    assert!(at(120, 30)[0] > at(160, 30)[0], "the mirrored placement");
}
