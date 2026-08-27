// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Split a PDF page's memory cost between parsing, display-list construction,
//! and rasterization, and account for what the display list actually holds.
//!
//! Written to investigate `pdf_samples/5447.pdf`, which peaks at 16.2 GB
//! rendering one page. The per-stage split is what disproved the first guess
//! (images): parsing and display-list construction cost 70 MB and do not move
//! with DPI, while rasterization goes from +318 MB at 72 dpi to +9.0 GB at
//! 300 dpi against a 64.8 MB canvas.
//!
//! The patch-shading census at the end predicts the dominant cost without
//! needing a profiler. `render_patch_shading` triangulates every patch on the
//! page, per band, with no culling and no reuse, and `render_banded_to_sink`
//! runs `rayon::current_num_threads()` bands concurrently — so the reported
//! per-build figure is multiplied by the core count at run time.
//!
//! Usage: cargo run --release -p stet-cli --example profile_images -- FILE [PAGE] [DPI]

use std::sync::Arc;
use stet_graphics::display_list::{DisplayElement, DisplayList};

/// Resident set size in bytes, read from /proc.
fn rss() -> u64 {
    let s = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
    let pages: u64 = s
        .split_whitespace()
        .nth(1)
        .unwrap_or("0")
        .parse()
        .unwrap_or(0);
    pages * 4096
}

fn mb(bytes: u64) -> String {
    format!("{:>9.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

#[derive(Default, Clone)]
struct Tally {
    /// Device-pixel scale, `dpi / 72`. Patch subdivision is chosen in device
    /// space, so the triangulation estimate is meaningless without it.
    scale: f64,
    elements: usize,
    images: usize,
    image_bytes: u64,
    image_pixels: u64,
    fills: usize,
    strokes: usize,
    text: usize,
    groups: usize,
    clips: usize,
    shadings: usize,
    other: usize,
    image_ptrs: std::collections::HashSet<usize>,
    unique_image_bytes: std::collections::HashMap<usize, u64>,
    /// Per-`PatchShading` element: (patch count, triangle count, bytes).
    patch_shadings: Vec<(usize, u64, u64)>,
}

impl Tally {
    fn walk(&mut self, list: &DisplayList) {
        for el in list.elements() {
            self.elements += 1;
            match el {
                DisplayElement::Image {
                    sample_data,
                    params,
                } => {
                    self.images += 1;
                    self.image_bytes += sample_data.len() as u64;
                    self.image_pixels += u64::from(params.width) * u64::from(params.height);
                    self.image_ptrs.insert(Arc::as_ptr(sample_data) as usize);
                    self.unique_image_bytes
                        .entry(Arc::as_ptr(sample_data) as usize)
                        .or_insert(sample_data.len() as u64);
                }
                DisplayElement::Fill { .. } => self.fills += 1,
                DisplayElement::Stroke { .. } => self.strokes += 1,
                DisplayElement::Text { .. } => self.text += 1,
                DisplayElement::Clip { .. } => self.clips += 1,
                DisplayElement::Group { elements, .. } => {
                    self.groups += 1;
                    self.walk(elements);
                }
                DisplayElement::SoftMasked { mask, content, .. } => {
                    self.groups += 1;
                    self.walk(content);
                    self.walk(mask);
                }
                DisplayElement::OcgGroup { elements, .. } => {
                    self.groups += 1;
                    self.walk(elements);
                }
                DisplayElement::PatchShading { params } => {
                    self.shadings += 1;
                    self.tally_patches(params);
                }
                DisplayElement::AxialShading { .. }
                | DisplayElement::RadialShading { .. }
                | DisplayElement::MeshShading { .. } => self.shadings += 1,
                _ => self.other += 1,
            }
        }
    }

    /// Predict what `render_patch_shading` will allocate for one element.
    ///
    /// Mirrors its subdivision rule exactly — `n = clamp(extent / 2, 8, 64)`
    /// device pixels per boundary segment, `2 * n * n` triangles per patch —
    /// so a change there must be mirrored here or this silently under-reports.
    fn tally_patches(&mut self, params: &stet_graphics::device::PatchShadingParams) {
        let scale = self.scale;
        let mut triangles = 0u64;
        for patch in &params.patches {
            if patch.points.len() < 12 {
                continue;
            }
            let (mut x0, mut y0) = (f64::INFINITY, f64::INFINITY);
            let (mut x1, mut y1) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
            for &(px, py) in &patch.points {
                let (dx, dy) = params.ctm.transform_point(px, py);
                x0 = x0.min(dx);
                y0 = y0.min(dy);
                x1 = x1.max(dx);
                y1 = y1.max(dy);
            }
            let extent = (x1 - x0).max(y1 - y0).abs() * scale;
            let n = (extent / 2.0).ceil().clamp(8.0, 64.0) as u64;
            triangles += 2 * n * n;
        }
        let bytes = triangles * size_of::<stet_graphics::device::ShadingTriangle>() as u64;
        self.patch_shadings
            .push((params.patches.len(), triangles, bytes));
    }

    fn report(&self) {
        println!("  display list elements : {}", self.elements);
        println!(
            "    images              : {:>6}  data {}  ({:.1} Mpx)",
            self.images,
            mb(self.image_bytes),
            self.image_pixels as f64 / 1e6
        );
        println!("    fills               : {:>6}", self.fills);
        println!("    strokes             : {:>6}", self.strokes);
        println!("    text runs           : {:>6}", self.text);
        println!("    clips               : {:>6}", self.clips);
        println!("    groups              : {:>6}", self.groups);
        println!("    shadings            : {:>6}", self.shadings);
        println!("    other               : {:>6}", self.other);
        let unique: u64 = self.unique_image_bytes.values().sum();
        println!(
            "    distinct image bufs : {:>6}  data {}  (sharing saves {})",
            self.image_ptrs.len(),
            mb(unique),
            mb(self.image_bytes.saturating_sub(unique))
        );

        if self.patch_shadings.is_empty() {
            return;
        }
        let total: u64 = self.patch_shadings.iter().map(|r| r.2).sum();
        let tris: u64 = self.patch_shadings.iter().map(|r| r.1).sum();
        let patches: usize = self.patch_shadings.iter().map(|r| r.0).sum();
        let threads = rayon::current_num_threads().max(1) as u64;
        println!();
        println!(
            "  patch shadings        : {} elements, {} patches, {} triangles",
            self.patch_shadings.len(),
            patches,
            tris
        );
        println!(
            "    triangulation       : {} per build  ({} bytes/triangle)",
            mb(total),
            size_of::<stet_graphics::device::ShadingTriangle>()
        );
        println!(
            "    x{} concurrent bands : {}   <-- rebuilt per band, unculled",
            threads,
            mb(total.saturating_mul(threads))
        );
        let mut rows = self.patch_shadings.clone();
        rows.sort_by_key(|r| std::cmp::Reverse(r.2));
        for (p, t, b) in rows.iter().take(5) {
            println!("      {p:>6} patches  {t:>10} tris  {}", mb(*b));
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .expect("usage: profile_images FILE [PAGE] [DPI]");
    let page: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let dpi: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(72.0);

    let base = rss();
    println!("{:<26} {}", "start", mb(base));

    let data = std::fs::read(&path).expect("read input");
    println!(
        "{:<26} {}   (file is {})",
        "after read",
        mb(rss()),
        mb(data.len() as u64)
    );

    let doc = stet_pdf_reader::PdfDocument::from_bytes(&data).expect("parse");
    let after_parse = rss();
    println!("{:<26} {}", "after from_bytes", mb(after_parse));

    let t0 = std::time::Instant::now();
    let list = doc.render_page(page, dpi).expect("build display list");
    let after_list = rss();
    println!(
        "{:<26} {}   (+{}, {:.1}s)",
        "after render_page",
        mb(after_list),
        mb(after_list.saturating_sub(after_parse)),
        t0.elapsed().as_secs_f64()
    );

    let mut tally = Tally {
        scale: dpi / 72.0,
        ..Default::default()
    };
    tally.walk(&list);
    tally.report();

    let t1 = std::time::Instant::now();
    let scale = dpi / 72.0;
    let (page_w, page_h) = doc.page_size(page).expect("page size");
    let w = (page_w * scale).round().max(1.0) as u32;
    let h = (page_h * scale).round().max(1.0) as u32;
    let rgba = stet_render::render_to_rgba(&list, w, h, dpi, Some(doc.icc_cache()), false);
    let after_raster = rss();
    println!(
        "{:<26} {}   (+{}, {:.1}s)",
        "after rasterize",
        mb(after_raster),
        mb(after_raster.saturating_sub(after_list)),
        t1.elapsed().as_secs_f64()
    );
    println!("  canvas {}x{} = {}", w, h, mb((w as u64) * (h as u64) * 4));
    std::hint::black_box(&rgba);
}
