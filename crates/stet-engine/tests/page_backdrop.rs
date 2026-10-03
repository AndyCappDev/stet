// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Every page starts from a clear page, whatever its size.
//!
//! The PNG device used to draw a page small enough to need no banding into
//! a pixmap it kept between pages, so the page landed on whatever that
//! held: white paper (blend modes blended with it, where the page group is
//! isolated — ISO 32000-1 §11.4.7), the page before a `copypage`, or the
//! `flushpage` replay of the same page. Each test runs on a page that size
//! and on one large enough to band, and the two must agree.

use std::sync::{Arc, Mutex};

use stet_core::context::Context;
use stet_core::device::{PageSink, PageSinkFactory};
use stet_core::geometry::Matrix;
use stet_render::{PageBackground, SkiaDevice};

/// A page small enough to render without banding, and one that bands.
const SIZES: [(u32, u32); 2] = [(100, 100), (1200, 1600)];

type Pages = Arc<Mutex<Vec<Vec<u8>>>>;

/// Keeps every page's RGBA.
struct Capture(Pages);

struct CaptureSink(Pages);

impl PageSink for CaptureSink {
    fn begin_page(&mut self, _width: u32, _height: u32) -> Result<(), String> {
        self.0.lock().unwrap().push(Vec::new());
        Ok(())
    }
    fn write_rows(&mut self, rgba_rows: &[u8], _num_rows: u32) -> Result<(), String> {
        self.0
            .lock()
            .unwrap()
            .last_mut()
            .unwrap()
            .extend_from_slice(rgba_rows);
        Ok(())
    }
    fn end_page(&mut self) -> Result<(), String> {
        Ok(())
    }
}

impl PageSinkFactory for Capture {
    fn create_sink(&self, _output_path: &str) -> Result<Box<dyn PageSink>, String> {
        Ok(Box::new(CaptureSink(self.0.clone())))
    }
}

/// One rendered page, with the PostScript user space at one pixel per
/// point.
struct Page {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl Page {
    /// RGBA at a point given in user space (origin bottom-left).
    fn at(&self, x: u32, y: u32) -> [u8; 4] {
        let row = self.height - 1 - y;
        let i = ((row * self.width + x) * 4) as usize;
        self.rgba[i..i + 4].try_into().unwrap()
    }
}

/// Run `source` on a `width` × `height` page and return every page it
/// sends to the device.
fn render(source: &str, width: u32, height: u32, background: PageBackground) -> Vec<Page> {
    let pages: Pages = Arc::new(Mutex::new(Vec::new()));
    let mut device = SkiaDevice::with_sink_factory(width, height, Box::new(Capture(pages.clone())));
    device.set_page_background(background);

    let mut ctx = Context::new();
    stet_ops::build_system_dict(&mut ctx);
    ctx.exec_sync_fn = Some(stet_engine::eval::exec_sync);
    ctx.device = Some(Box::new(device));
    ctx.output_path = Some("unused".to_string());
    ctx.page_width = width;
    ctx.page_height = height;
    let ctm = Matrix {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: -1.0,
        tx: 0.0,
        ty: height as f64,
    };
    ctx.gstate.ctm = ctm;
    ctx.gstate.default_ctm = ctm;

    stet_engine::eval::parse_and_exec(&mut ctx, source.as_bytes()).expect("PS execution failed");
    ctx.device.as_mut().unwrap().finish().unwrap();

    let rgba = pages.lock().unwrap().clone();
    rgba.into_iter()
        .map(|rgba| Page {
            width,
            height,
            rgba,
        })
        .collect()
}

/// Three red stripes, each under a blend mode that would change it if the
/// page underneath were white.
const BLEND_STRIPES: &str = "\
    /Difference setblendmode 1 0 0 setrgbcolor 0 0 30 100 rectfill \
    /Screen setblendmode 30 0 30 100 rectfill \
    /Exclusion setblendmode 60 0 30 100 rectfill \
    showpage";

/// Over a blank page, a blend mode has nothing to blend with: red stays
/// red, rather than becoming cyan (Difference, Exclusion) or white (Screen)
/// against white paper.
#[test]
fn blend_modes_over_a_blank_page_leave_the_colour() {
    for (w, h) in SIZES {
        let pages = render(BLEND_STRIPES, w, h, PageBackground::White);
        for x in [15, 45, 75] {
            assert_eq!(pages[0].at(x, 50), [255, 0, 0, 255], "{w}x{h} at x = {x}");
        }
        assert_eq!(pages[0].at(95, 50), [255; 4], "{w}x{h}: paper");
    }
}

/// The same with a transparent background: the stripes are opaque red and
/// the rest of the page clear.
#[test]
fn blend_modes_over_a_transparent_page_leave_the_colour() {
    for (w, h) in SIZES {
        let pages = render(BLEND_STRIPES, w, h, PageBackground::Transparent);
        assert_eq!(pages[0].at(15, 50), [255, 0, 0, 255], "{w}x{h}");
        assert_eq!(pages[0].at(45, 50), [255, 0, 0, 255], "{w}x{h}");
        assert_eq!(pages[0].at(95, 50)[3], 0, "{w}x{h}: unpainted");
    }
}

/// Every page after the first starts clear too: the device erases between
/// pages, which used to mean painting it white.
#[test]
fn later_pages_start_clear() {
    let source = format!("showpage {BLEND_STRIPES}");
    for (w, h) in SIZES {
        let pages = render(&source, w, h, PageBackground::White);
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[1].at(15, 50), [255, 0, 0, 255], "{w}x{h}");
    }
}

/// In LanguageLevel 3, `copypage` erases the page after sending it (PLRM
/// 3e), so the next page shows only its own content.
#[test]
fn the_page_after_copypage_starts_blank() {
    let source = "1 0 0 setrgbcolor 0 0 30 100 rectfill copypage \
                  0 0 1 setrgbcolor 60 0 30 100 rectfill showpage";
    for (w, h) in SIZES {
        let pages = render(source, w, h, PageBackground::White);
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].at(15, 50), [255, 0, 0, 255], "{w}x{h}: copied");
        assert_eq!(pages[1].at(15, 50), [255; 4], "{w}x{h}: erased");
        assert_eq!(pages[1].at(75, 50), [0, 0, 255, 255], "{w}x{h}: its own");
    }
}

/// `flushpage` replays the page so far to the device; the page `showpage`
/// sends still paints each object once.
#[test]
fn flushpage_does_not_paint_the_page_twice() {
    let source = "0.5 setfillopacity 1 0 0 setrgbcolor 0 0 100 100 rectfill \
                  flushpage showpage";
    for (w, h) in SIZES {
        let pages = render(source, w, h, PageBackground::White);
        let [r, g, b, a] = pages[0].at(50, 50);
        assert_eq!((r, a), (255, 255), "{w}x{h}");
        assert!(
            g.abs_diff(128) <= 1 && b.abs_diff(128) <= 1,
            "{w}x{h}: {g}, {b}"
        );
    }
}
