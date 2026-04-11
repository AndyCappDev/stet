// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Native repro of the WASM `render_pdf` path for pdf_samples/1321.pdf.
//!
//! Mirrors `stet_wasm::render_pdf` exactly: embedded CMYK ICC, embedded font
//! provider, dpi=300, same `PdfDocument::from_bytes_with_icc` + `render_page`
//! API. Run with `RUST_BACKTRACE=1 cargo run -p stet-wasm --example repro_1321
//! -- <path-to-pdf>` to capture panics with a real backtrace.

use std::env;
use std::fs;

use stet_graphics::icc::IccCache;
use stet_render::ImageCache;
use stet_wasm::embedded_resources;

const DEFAULT_CMYK_ICC: &[u8] = include_bytes!("../src/default_cmyk.icc");

fn main() {
    let args: Vec<String> = env::args().collect();
    let path = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "pdf_samples/1321.pdf".to_string());

    eprintln!("repro: reading {}", path);
    let pdf_data = fs::read(&path).expect("read pdf");

    let dpi = 300.0f64;

    let mut icc_cache = IccCache::new();
    icc_cache.load_cmyk_profile_bytes(DEFAULT_CMYK_ICC);

    eprintln!("repro: parsing PDF…");
    let mut doc = stet_pdf_reader::PdfDocument::from_bytes_with_icc(&pdf_data, icc_cache)
        .expect("PDF parse");
    doc.set_font_provider(embedded_resources::build_font_provider());

    let cmyk_bytes: std::sync::Arc<Vec<u8>> = std::sync::Arc::new(DEFAULT_CMYK_ICC.to_vec());

    for page_idx in 0..doc.page_count() {
        eprintln!("repro: rendering page {}…", page_idx);
        let dl = match doc.render_page(page_idx, dpi) {
            Ok(dl) => dl,
            Err(e) => {
                eprintln!("repro: page {} render_page Err: {}", page_idx, e);
                continue;
            }
        };
        eprintln!(
            "repro: page {} ok — {} display elements",
            page_idx,
            dl.elements().len()
        );

        let (page_w, page_h) = doc.page_size(page_idx).expect("page_size");
        let scale = dpi / 72.0;
        let pixel_w = (page_w * scale).round() as u32;
        let pixel_h = (page_h * scale).round() as u32;
        eprintln!("repro: page {} dims = {}x{}", page_idx, pixel_w, pixel_h);

        eprintln!("repro: page {} prepare_display_list…", page_idx);
        let prepared = stet_render::prepare_display_list(&dl);

        eprintln!("repro: page {} build_icc_cache_for_list…", page_idx);
        let _icc = stet_render::build_icc_cache_for_list(&dl, Some(&cmyk_bytes));

        eprintln!("repro: page {} ImageCache::build…", page_idx);
        let image_cache = ImageCache::build(&dl, Some(&_icc));

        // Exercise the viewport render path at 1x zoom (matches worker.js first draw).
        let vp_x = 0.0f64;
        let vp_y = 0.0f64;
        let vp_w = pixel_w as f64;
        let vp_h = pixel_h as f64;

        eprintln!("repro: page {} render_region_prepared 1x…", page_idx);
        let _rgba = stet_render::render_region_prepared(
            &dl,
            &prepared,
            vp_x,
            vp_y,
            vp_w,
            vp_h,
            pixel_w,
            pixel_h,
            dpi,
            None,
            Some(&image_cache),
            false,
        );
        eprintln!("repro: page {} render_region_prepared ok", page_idx);

        // Also try the banded single-band path.
        let (num_bands, band_h) = stet_render::viewport_band_count(pixel_w, pixel_h);
        eprintln!(
            "repro: page {} banded render {} bands × {}px…",
            page_idx, num_bands, band_h
        );
        for b in 0..num_bands {
            let _bandrgba = stet_render::render_region_single_band(
                &dl,
                &prepared,
                vp_x,
                vp_y,
                vp_w,
                vp_h,
                pixel_w,
                pixel_h,
                b,
                band_h,
                num_bands,
                dpi,
                None,
                Some(&image_cache),
                false,
            );
        }
        eprintln!("repro: page {} banded render ok", page_idx);
    }
    eprintln!("repro: done");
}
