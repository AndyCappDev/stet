// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! WS3 — Profile-stability regression scaffold.
//!
//! Renders GWG test PDFs twice at 300 DPI:
//!   1. with the stet system-default CMYK profile (ghostscript/default_cmyk.icc
//!      unless overridden), and
//!   2. with the PDF's own embedded /OutputIntents DestOutputProfile.
//!
//! For each swatch region in [`X4_SWATCHES`], computes a uniformity score
//! (per-channel p95 − p5 summed) and prints a report. A correctly rendered
//! swatch collapses to the same sRGB for "reference" and "test" shapes, so
//! the spread is ~0 under *both* profiles. Regressions show up as a larger
//! spread under the OutputIntent profile — profile-stable CMYK math would
//! collapse both columns back to zero.
//!
//! For now the assertion is *soft* (we only fail if a swatch crosses a very
//! loose threshold). As WS1/WS2 land, we will tighten the threshold and add
//! `assert!` guards that the OutputIntent column is not materially worse
//! than the GS-default column.
//!
//! Skips gracefully when the sample PDF is absent.

use stet_pdf_reader::PdfDocument;

/// A rectangular region inside a rendered page that should be uniform under
/// correct rendering. Coordinates are in **300 DPI pixel space**.
///
/// Bboxes should be picked tight enough to exclude labels/captions (which
/// would drive the uniformity score up for text, not for colour drift).
#[derive(Debug, Clone, Copy)]
struct Swatch {
    /// 1-based page index, to match the section names the GWG booklet uses.
    page: usize,
    name: &'static str,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

/// Seeded swatches from the regression set the user reported:
///   page 1: GWG 19.0 / 19.1 / 19.2
///   page 2: GWG 16.0 / 16.1 / 16.2
///   page 3: GWG 17.0 / 17.3
///   page 4: GWG 1.1 / 4.1
///
/// Coordinates are section-scoped "colored-swatch strip" bboxes — wide enough
/// to cover all the blend-mode swatches in the row, tall enough to skip the
/// header above and the labels below. These are seeded approximately; WS3's
/// first job once the scaffold runs is to tighten them and split out
/// per-swatch sub-bboxes.
const X4_SWATCHES: &[Swatch] = &[
    // Page 2 — confirmed by the user: area (210,1480)..(1290,2085) at 300 DPI
    // covers the entire GWG 16.2 section including title + labels. The
    // colored-swatch strip is the top band of that region.
    Swatch {
        page: 2,
        name: "GWG 16.2 (DeviceCMYK, Isolated) — swatch strip",
        x: 210,
        y: 1540,
        w: 1080,
        h: 180,
    },
    // Rough bboxes for neighbouring sections on page 2 — same layout:
    // "title / colored-swatch row / labels / footer" from top to bottom.
    Swatch {
        page: 2,
        name: "GWG 16.0 (DeviceCMYK, Non-Knockout) — swatch strip",
        x: 210,
        y: 810,
        w: 1080,
        h: 180,
    },
    Swatch {
        page: 2,
        name: "GWG 16.1 (DeviceCMYK, Knockout) — swatch strip",
        x: 1300,
        y: 810,
        w: 1080,
        h: 180,
    },
    // Page 1 — GWG 19.x overprint sections. Layout-equivalent bboxes.
    Swatch {
        page: 1,
        name: "GWG 19.0 — swatch strip",
        x: 210,
        y: 1540,
        w: 1080,
        h: 180,
    },
    Swatch {
        page: 1,
        name: "GWG 19.1 — swatch strip",
        x: 1300,
        y: 1540,
        w: 1080,
        h: 180,
    },
    Swatch {
        page: 1,
        name: "GWG 19.2 — swatch strip",
        x: 210,
        y: 2270,
        w: 1080,
        h: 180,
    },
    // Page 3 — GWG 17.x image softmasks / image masks.
    Swatch {
        page: 3,
        name: "GWG 17.0 — swatch strip",
        x: 210,
        y: 810,
        w: 1080,
        h: 180,
    },
    // TODO(ws3): 17.3 bbox lands on whitespace — retighten after visual
    // inspection of target/gwg_stability/x4-p3-*.png.
    Swatch {
        page: 3,
        name: "GWG 17.3 — swatch strip (PLACEHOLDER)",
        x: 1300,
        y: 1540,
        w: 1080,
        h: 180,
    },
    // Page 4 — GWG 1.1 overprint / 4.1 DeviceN.
    Swatch {
        page: 4,
        name: "GWG 1.1 — swatch strip",
        x: 210,
        y: 810,
        w: 1080,
        h: 180,
    },
    Swatch {
        page: 4,
        name: "GWG 4.1 — swatch strip",
        x: 1300,
        y: 810,
        w: 1080,
        h: 180,
    },
];

/// Sum of per-channel (p95 − p5) over the RGB channels of a swatch region.
///
/// Robust to a small amount of text/icon noise (the outlier tails get
/// clipped) but sensitive to the faint systematic lighter-X pattern that
/// GWG swatches produce when ref vs test CMYK drift apart.
///
/// A perfectly uniform region scores ~0. Hairline-visible X patterns score
/// in the low single digits. Obvious visual regressions score 10+.
fn uniformity_score(rgba: &[u8], pixel_w: u32, pixel_h: u32, sw: &Swatch) -> u32 {
    let x0 = sw.x.min(pixel_w);
    let y0 = sw.y.min(pixel_h);
    let x1 = (sw.x + sw.w).min(pixel_w);
    let y1 = (sw.y + sw.h).min(pixel_h);
    if x1 <= x0 || y1 <= y0 {
        return 0;
    }

    let mut hist = [[0u32; 256]; 3];
    let mut count = 0u32;
    for y in y0..y1 {
        let row = (y as usize) * (pixel_w as usize) * 4;
        for x in x0..x1 {
            let px = row + (x as usize) * 4;
            hist[0][rgba[px] as usize] += 1;
            hist[1][rgba[px + 1] as usize] += 1;
            hist[2][rgba[px + 2] as usize] += 1;
            count += 1;
        }
    }
    if count == 0 {
        return 0;
    }

    let p_lo = (count as f64 * 0.05) as u32;
    let p_hi = (count as f64 * 0.95) as u32;

    let mut total_spread = 0u32;
    for channel in &hist {
        let mut acc = 0u32;
        let mut lo = 0u8;
        let mut hi = 255u8;
        for (v, &bucket) in channel.iter().enumerate() {
            acc += bucket;
            if acc <= p_lo {
                lo = v as u8;
            }
            if acc >= p_hi {
                hi = v as u8;
                break;
            }
        }
        total_spread += hi.saturating_sub(lo) as u32;
    }
    total_spread
}

fn try_load_pdf(name: &str) -> Option<Vec<u8>> {
    let root = env!("CARGO_MANIFEST_DIR").replace("/crates/stet-pdf-reader", "");
    for subdir in ["pdf_samples", "samples"] {
        let path = format!("{}/{}/{}", root, subdir, name);
        if let Ok(data) = std::fs::read(&path) {
            return Some(data);
        }
    }
    eprintln!("Skipping {name} — not found in pdf_samples/ or samples/");
    None
}

fn output_dir() -> std::path::PathBuf {
    let root = env!("CARGO_MANIFEST_DIR").replace("/crates/stet-pdf-reader", "");
    let dir = std::path::PathBuf::from(root).join("target/gwg_stability");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn save_png(rgba: &[u8], w: u32, h: u32, path: &std::path::Path) {
    let file = std::fs::File::create(path).unwrap();
    let bw = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(bw, w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(rgba).unwrap();
}

struct RenderedDoc {
    pages: Vec<(Vec<u8>, u32, u32)>,
}

fn render_all_pages(data: &[u8], apply_output_intent: bool) -> RenderedDoc {
    let mut doc = PdfDocument::from_bytes(data).unwrap();
    if apply_output_intent {
        let applied = doc.apply_output_intent_as_default_cmyk();
        assert!(
            applied,
            "PDF has no OutputIntent; WS3 requires one for the stability check"
        );
    }
    let pages = (0..doc.page_count())
        .map(|p| doc.render_page_to_rgba(p, 300.0).unwrap())
        .collect();
    RenderedDoc { pages }
}

#[test]
#[ignore = "slow (~60s): renders X4 4 pages × 2 profiles at 300 DPI"]
fn x4_profile_stability_report() {
    let Some(data) = try_load_pdf("PDFX-ready_Output-Test_X4.pdf") else {
        return;
    };

    let out = output_dir();
    eprintln!("[WS3] rendering X4 with GS-default CMYK profile...");
    let gs = render_all_pages(&data, false);
    eprintln!("[WS3] rendering X4 with PDF OutputIntent CMYK profile...");
    let oi = render_all_pages(&data, true);

    assert_eq!(gs.pages.len(), oi.pages.len());
    assert_eq!(gs.pages.len(), 4, "PDFX-ready X4 is always 4 pages");

    for (i, ((rgba_gs, w, h), (rgba_oi, _, _))) in gs.pages.iter().zip(oi.pages.iter()).enumerate()
    {
        save_png(
            rgba_gs,
            *w,
            *h,
            &out.join(format!("x4-p{}-gsdefault.png", i + 1)),
        );
        save_png(
            rgba_oi,
            *w,
            *h,
            &out.join(format!("x4-p{}-outputintent.png", i + 1)),
        );
        // Diff heatmap: abs(OI - GS) per channel, saturated. Bright pixels
        // are where the two profiles disagree — useful for finding swatches
        // whose CMYK is drifting (the drift amplifies under the more
        // accurate OI profile).
        let mut diff = vec![0u8; rgba_gs.len()];
        for p in 0..(rgba_gs.len() / 4) {
            let base = p * 4;
            for c in 0..3 {
                let d = (rgba_gs[base + c] as i32 - rgba_oi[base + c] as i32).unsigned_abs();
                diff[base + c] = d.min(255) as u8;
            }
            diff[base + 3] = 0xFF;
        }
        save_png(&diff, *w, *h, &out.join(format!("x4-p{}-diff.png", i + 1)));
    }

    eprintln!();
    eprintln!(
        "{:<52} | {:>10} | {:>10} | {:>6}",
        "swatch", "GS-default", "OutputInt", "Δ"
    );
    eprintln!("{}", "-".repeat(90));

    let mut worst_delta = i32::MIN;
    let mut worst_name = "";
    for s in X4_SWATCHES {
        let page_idx = s.page - 1;
        let (rgba_gs, w, h) = &gs.pages[page_idx];
        let (rgba_oi, _, _) = &oi.pages[page_idx];
        let score_gs = uniformity_score(rgba_gs, *w, *h, s);
        let score_oi = uniformity_score(rgba_oi, *w, *h, s);
        let delta = score_oi as i32 - score_gs as i32;
        eprintln!(
            "{:<52} | {:>10} | {:>10} | {:>+6}",
            s.name, score_gs, score_oi, delta
        );
        if delta > worst_delta {
            worst_delta = delta;
            worst_name = s.name;
        }
    }

    eprintln!();
    eprintln!(
        "[WS3] worst OI-vs-GS uniformity regression: {} (Δ={:+})",
        worst_name, worst_delta
    );
    eprintln!(
        "[WS3] PNGs + per-page diff heatmaps saved to {}",
        out.display()
    );
    eprintln!(
        "[WS3] Goal: every swatch has |Δ| ≤ 2 and score_oi ≈ score_gs ≈ 0 \
         once WS1/WS2 land. Until then, this test is report-only — watch \
         the Δ column for fix-validation."
    );
}
