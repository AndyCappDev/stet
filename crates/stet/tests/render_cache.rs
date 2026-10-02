// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `Interpreter::render` converts what it renders the way the context
//! converted what it built.
//!
//! Most colours are converted as the display list is built, through
//! `context().icc_cache`. DeviceCMYK images and overprint are converted
//! again as the page renders, through a cache `render` builds for it. That
//! cache was the embedded profile with the default black-point
//! compensation whatever the context held, so a caller who configured the
//! context's CMYK profile or compensation got images that disagreed with
//! the fills beside them.

#![cfg(feature = "render")]

use stet::Interpreter;
use stet_graphics::icc::{BpcMode, IccCache, IccCacheOptions};

/// A CMYK profile whose 400% black is not L* 0, so black-point
/// compensation moves K=100.
const SPLIT: &[u8] = include_bytes!("../../stet-graphics/tests/data/cmyk_intent/split.icc");

/// K=100 filled on the left half; a K=100 DeviceCMYK image, which the
/// display list keeps as CMYK for render time, on the right.
const FILL_AND_IMAGE: &[u8] = b"%!PS\n\
    0 0 0 1 setcmykcolor 0 0 306 792 rectfill\n\
    gsave 306 0 translate 306 792 scale\n\
    1 1 8 [1 0 0 1 0 0] <000000FF> false 4 colorimage grestore showpage\n";

/// The fill's colour and the image's, as `interp` renders them.
fn fill_and_image(interp: &mut Interpreter) -> ([u8; 3], [u8; 3]) {
    let page = interp.render(FILL_AND_IMAGE, 72.0).unwrap().remove(0);
    let (w, h) = (page.width, page.height);
    let at = |x: u32| {
        let i = ((h / 2 * w + x) * 4) as usize;
        [page.rgba[i], page.rgba[i + 1], page.rgba[i + 2]]
    };
    (at(w / 4), at(w * 3 / 4))
}

#[test]
fn images_render_through_the_contexts_profile_and_compensation() {
    let mut fills = Vec::new();
    for mode in [BpcMode::Off, BpcMode::On] {
        let mut interp = Interpreter::new();
        interp.context().icc_cache = IccCache::new_with_options(IccCacheOptions {
            bpc_mode: mode,
            source_cmyk_profile: Some(SPLIT.to_vec()),
        });
        let (fill, image) = fill_and_image(&mut interp);
        assert_eq!(fill, image, "{mode:?}");
        fills.push(fill);
    }
    // The profile tells the modes apart, or the test proves nothing.
    assert_ne!(fills[0], fills[1]);
}

#[test]
fn the_default_context_is_consistent_too() {
    let (fill, image) = fill_and_image(&mut Interpreter::new());
    assert_eq!(fill, image);
}
