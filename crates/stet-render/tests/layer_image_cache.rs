// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Images inside layers (`OcgGroup`) find their own entries in the page's
//! image caches.
//!
//! Both caches — the banded renderer's prescaled images and the viewer's
//! [`ImageCache`] — are indexed by an element's position in its list. An
//! image inside a layer is at a position within the layer, so a cache that
//! is flat over the page would hand it whichever top-level image shares that
//! position. Each test puts a different image there and checks that every
//! image draws its own colour, in its own stripe of the page.

use std::sync::Arc;

use stet_fonts::geometry::Matrix;
use stet_graphics::device::{ImageColorSpace, ImageParams};
use stet_graphics::display_list::{DisplayElement, DisplayList, OcgVisibility};
use stet_graphics::layer_set::LayerSet;
use stet_render::{
    ImageCache, prepare_display_list, render_region_prepared, render_to_rgba_with_layers,
};

const W: u32 = 80;
const H: u32 = 20;
/// Each image fills one 20-point stripe, numbered from the left.
const STRIPE: u32 = 20;

const RED: [u8; 3] = [255, 0, 0];
const GREEN: [u8; 3] = [0, 255, 0];
const BLUE: [u8; 3] = [0, 0, 255];
const YELLOW: [u8; 3] = [255, 255, 0];
const PAPER: [u8; 3] = [255; 3];

/// A one-pixel DeviceRGB image of `rgb` filling stripe `stripe`.
fn image(rgb: [u8; 3], stripe: u32) -> DisplayElement {
    DisplayElement::Image {
        sample_data: Arc::new(rgb.to_vec()),
        params: ImageParams {
            width: 1,
            height: 1,
            color_space: ImageColorSpace::DeviceRGB,
            bits_per_component: 8,
            ctm: Matrix::new(
                STRIPE as f64,
                0.0,
                0.0,
                H as f64,
                (stripe * STRIPE) as f64,
                0.0,
            ),
            image_matrix: Matrix::new(1.0, 0.0, 0.0, 1.0, 0.0, 0.0),
            ..ImageParams::default()
        },
    }
}

fn list(elements: Vec<DisplayElement>) -> DisplayList {
    let mut list = DisplayList::new();
    for e in elements {
        list.push(e);
    }
    list
}

/// A layer for OCG `id`, visible by default when `visible`.
fn layer(id: u32, visible: bool, elements: Vec<DisplayElement>) -> DisplayElement {
    DisplayElement::OcgGroup {
        elements: list(elements),
        visibility: OcgVisibility::single(id, visible),
    }
}

/// The colour of each stripe, from the banded renderer (which converts
/// images through its own per-page cache) and from the viewport renderer
/// with the viewer's [`ImageCache`]; the two must agree.
fn stripes(list: &DisplayList, layers: &LayerSet) -> Vec<[u8; 3]> {
    let at = |data: &[u8], stripe: u32| {
        let i = (((H / 2) * W + stripe * STRIPE + STRIPE / 2) * 4) as usize;
        [data[i], data[i + 1], data[i + 2]]
    };
    let banded = render_to_rgba_with_layers(list, W, H, 72.0, None, false, layers);
    let cache = ImageCache::build(list, None);
    let viewer = render_region_prepared(
        list,
        &prepare_display_list(list),
        0.0,
        0.0,
        W as f64,
        H as f64,
        W,
        H,
        72.0,
        None,
        Some(&cache),
        false,
    );
    let banded: Vec<_> = (0..W / STRIPE).map(|s| at(&banded, s)).collect();
    // The viewport renderer takes no layer overrides; compare it only where
    // the page is drawn with the default visibility.
    if layers.is_empty() {
        let viewer: Vec<_> = (0..W / STRIPE).map(|s| at(&viewer, s)).collect();
        assert_eq!(banded, viewer, "banded and viewer renders disagree");
    }
    banded
}

/// A top-level image and an image inside a layer at the same index: each
/// draws its own.
#[test]
fn a_layer_image_does_not_take_the_top_level_image_at_its_index() {
    let page = list(vec![image(RED, 0), layer(1, true, vec![image(BLUE, 1)])]);
    assert_eq!(stripes(&page, &LayerSet::new()), [RED, BLUE, PAPER, PAPER]);
}

/// The same, two layers deep: the inner layer's image is at index 0 in its
/// own list, in its parent's, and in the page's.
#[test]
fn nested_layers_take_their_own_entries() {
    let page = list(vec![
        image(RED, 0),
        layer(
            1,
            true,
            vec![image(GREEN, 1), layer(2, true, vec![image(BLUE, 2)])],
        ),
    ]);
    assert_eq!(stripes(&page, &LayerSet::new()), [RED, GREEN, BLUE, PAPER]);
}

/// A hidden layer between two images paints nothing and leaves the images
/// on either side their own entries; shown by an override, it draws its own
/// image.
#[test]
fn a_hidden_layer_leaves_its_neighbours_their_entries() {
    let page = list(vec![
        image(RED, 0),
        layer(1, false, vec![image(GREEN, 1)]),
        image(BLUE, 2),
    ]);
    assert_eq!(stripes(&page, &LayerSet::new()), [RED, PAPER, BLUE, PAPER]);
    let mut shown = LayerSet::new();
    shown.set(1, true);
    assert_eq!(stripes(&page, &shown), [RED, GREEN, BLUE, PAPER]);
}

/// An image after a layer keeps its own top-level entry, and the layer's
/// images keep theirs.
#[test]
fn an_image_after_a_layer_keeps_its_own_entry() {
    let page = list(vec![
        image(RED, 0),
        layer(1, true, vec![image(GREEN, 1), image(BLUE, 2)]),
        image(YELLOW, 3),
    ]);
    assert_eq!(stripes(&page, &LayerSet::new()), [RED, GREEN, BLUE, YELLOW]);
}
