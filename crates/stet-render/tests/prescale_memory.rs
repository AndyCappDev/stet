// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A large image drawn small is never held at full size while it is
//! rendered.
//!
//! Converting an image to RGBA and then scaling it down costs four bytes
//! for every source pixel, to keep a few thousand of them: 116 MB for a
//! 4500×6442 image that ends up 800 pixels wide, and once that again for
//! its soft mask. The renderer converts such an image a strip of rows at a
//! time, straight into the filter.
//!
//! This file holds one test and its own allocator, which records the most
//! memory live at once. It is the test that fails if a full-size buffer
//! comes back; the pixels would not show it.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use stet_fonts::geometry::Matrix;
use stet_graphics::device::{ImageColorSpace, ImageParams};
use stet_graphics::display_list::{DisplayElement, DisplayList, SoftMaskParams, SoftMaskSubtype};
use stet_render::{render_to_rgba, render_to_rgba_viewport};

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// The most memory live at once during `f`, beyond what was live when it
/// started.
fn peak_during<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let out = f();
    (out, PEAK.load(Ordering::Relaxed).saturating_sub(before))
}

const IMG_W: u32 = 3000;
const IMG_H: u32 = 2400;
/// The image is drawn a tenth of its size.
const DRAWN_W: u32 = IMG_W / 10;
const DRAWN_H: u32 = IMG_H / 10;
const PAGE_W: u32 = DRAWN_W + 20;
const PAGE_H: u32 = DRAWN_H + 20;

fn image(color_space: ImageColorSpace, samples: Vec<u8>) -> DisplayElement {
    DisplayElement::Image {
        sample_data: Arc::new(samples),
        params: ImageParams {
            width: IMG_W,
            height: IMG_H,
            color_space,
            bits_per_component: 8,
            ctm: Matrix::new(DRAWN_W as f64, 0.0, 0.0, DRAWN_H as f64, 10.0, 10.0),
            image_matrix: Matrix::new(IMG_W as f64, 0.0, 0.0, -(IMG_H as f64), 0.0, IMG_H as f64),
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

#[test]
fn a_large_image_drawn_small_is_not_held_at_full_size() {
    let pixels = (IMG_W * IMG_H) as usize;
    let rgb: Vec<u8> = (0..pixels * 3).map(|i| (i % 251) as u8).collect();
    let gray: Vec<u8> = (0..pixels).map(|i| (i % 241) as u8).collect();
    // The image at top level, and again as an image with a soft mask: the
    // shape every PDF image with an /SMask takes.
    let page = list(vec![
        image(ImageColorSpace::DeviceRGB, rgb.clone()),
        DisplayElement::SoftMasked {
            mask: list(vec![image(ImageColorSpace::DeviceGray, gray)]),
            content: list(vec![image(ImageColorSpace::DeviceRGB, rgb)]),
            params: SoftMaskParams {
                subtype: SoftMaskSubtype::Luminosity,
                bbox: [10.0, 10.0, (DRAWN_W + 10) as f64, (DRAWN_H + 10) as f64],
                backdrop_color: None,
                transfer_invert: false,
                has_nested_mask_scope: false,
                parent_clip_bbox: None,
            },
            mask_cache: Arc::new(Mutex::new(None)),
        },
    ]);

    // One image at full size as RGBA: what a conversion that is not
    // streamed would hold, before the filter's own working memory.
    let full_size = pixels * 4;
    let budget = full_size / 4;

    // The banded renderer, which converts each image once for the page.
    let (banded, peak) = peak_during(|| render_to_rgba(&page, PAGE_W, PAGE_H, 72.0, None, false));
    assert!(
        peak < budget,
        "banded render peaked at {peak} bytes; an image at full size is {full_size}"
    );

    // The viewport renderer with no image cache, which converts each image
    // as it draws it (and a soft mask's content in each of its bands).
    let (viewport, peak) =
        peak_during(|| render_to_rgba_viewport(&page, PAGE_W, PAGE_H, 72.0, None, false));
    assert!(
        peak < budget,
        "viewport render peaked at {peak} bytes; an image at full size is {full_size}"
    );

    // And the image was drawn: the same pixels both ways, not blank paper.
    assert!(banded == viewport, "the two renders differ");
    let centre = ((PAGE_H / 2 * PAGE_W + PAGE_W / 2) * 4) as usize;
    assert_ne!(banded[centre..centre + 3], [255; 3]);
}
