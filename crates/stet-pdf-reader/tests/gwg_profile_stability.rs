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
/// Swatch bboxes discovered by scanning rendered x4-p{1..4}-gsdefault.png
/// (at 300 DPI) for horizontal strips where the "left/right column" region is
/// non-white *and* horizontally varied — i.e. rows filled with colored
/// swatches. Coordinates are the outer union of the colored strip only; text
/// labels above/below are excluded, so the absolute score reflects colour
/// drift alone rather than label anti-aliasing.
///
/// Swatch-to-GWG-section assignments are approximate — the GWG booklet labels
/// sit above each strip and can't be anchored programmatically. The Δ column
/// (OutputInt − GS-default) is independent of the mapping.
const X4_SWATCHES: &[Swatch] = &[
    // Page 1 — top two GWG sections (left and right columns).
    Swatch {
        page: 1,
        name: "p1 top-left strip",
        x: 210,
        y: 972,
        w: 1080,
        h: 123,
    },
    Swatch {
        page: 1,
        name: "p1 top-right strip",
        x: 1300,
        y: 972,
        w: 1080,
        h: 123,
    },
    // Page 1 — middle-left (includes GWG 19.x overprint swatches).
    Swatch {
        page: 1,
        name: "p1 mid-left strip",
        x: 210,
        y: 1149,
        w: 1080,
        h: 123,
    },
    Swatch {
        page: 1,
        name: "p1 mid-right upper strip",
        x: 1300,
        y: 1621,
        w: 1080,
        h: 95,
    },
    Swatch {
        page: 1,
        name: "p1 mid-right lower strip",
        x: 1300,
        y: 1787,
        w: 1080,
        h: 97,
    },
    // Page 2 — GWG 16.0 (non-knockout) / 16.1 (knockout) / 16.2 (isolated).
    // User confirmed 16.2 in the earlier session.
    Swatch {
        page: 2,
        name: "GWG 16.0 (CMYK, Non-Knockout) — top row",
        x: 210,
        y: 956,
        w: 1080,
        h: 96,
    },
    Swatch {
        page: 2,
        name: "GWG 16.0 (CMYK, Non-Knockout) — bottom row",
        x: 210,
        y: 1098,
        w: 1080,
        h: 97,
    },
    Swatch {
        page: 2,
        name: "GWG 16.1 (CMYK, Knockout) — top row",
        x: 1300,
        y: 956,
        w: 1080,
        h: 96,
    },
    Swatch {
        page: 2,
        name: "GWG 16.1 (CMYK, Knockout) — bottom row",
        x: 1300,
        y: 1098,
        w: 1080,
        h: 97,
    },
    Swatch {
        page: 2,
        name: "GWG 16.2 (CMYK, Isolated) — top row",
        x: 210,
        y: 1558,
        w: 1080,
        h: 96,
    },
    Swatch {
        page: 2,
        name: "GWG 16.2 (CMYK, Isolated) — bottom row",
        x: 210,
        y: 1700,
        w: 1080,
        h: 96,
    },
    // Page 3 — GWG 17.x image softmasks (right column has tall strips).
    Swatch {
        page: 3,
        name: "p3 mid-right image-softmask strip",
        x: 1300,
        y: 2204,
        w: 1080,
        h: 293,
    },
    // Page 4 — GWG 1.1 / 4.1 overprint + DeviceN strips (top rows).
    Swatch {
        page: 4,
        name: "p4 top-left strip (1.1)",
        x: 210,
        y: 1044,
        w: 1080,
        h: 96,
    },
    Swatch {
        page: 4,
        name: "p4 top-right strip (4.1)",
        x: 1300,
        y: 1001,
        w: 1080,
        h: 242,
    },
];

/// Sum-across-channels of the mean per-pixel local deviation within a
/// swatch region (×1000 for readability as an integer score).
///
/// For each interior pixel we compute the max |channel-delta| to its 4-
/// neighbours (N, S, E, W), summed over the three RGB channels. We then
/// take the mean across the region.
///
/// Why a local metric: each "swatch strip" in the GWG booklet is a row of
/// ~10 differently-coloured squares side-by-side, so a global spread metric
/// (p95−p5) sees the whole colour range and gives a huge number dominated
/// by the between-swatch colour change, not by within-swatch uniformity
/// violations. A local metric is near-zero on solid-colour interiors,
/// contributes a small amount on the ~1-pixel-thick boundary between
/// adjacent swatches (bounded by boundary-pixels ÷ region-pixels), and
/// grows proportionally to any faint systematic "X pattern" drift, which
/// is what the GWG test is designed to reveal.
///
/// Returned as floor(mean × 1000). Perfectly uniform columns of solid
/// swatches with only the boundary transitions score in the low tens;
/// swatches with visible X artefacts score higher, and the Δ column
/// (OutputInt − GS) isolates the profile-sensitivity contribution.
fn uniformity_score(rgba: &[u8], pixel_w: u32, pixel_h: u32, sw: &Swatch) -> u32 {
    let x0 = sw.x.min(pixel_w);
    let y0 = sw.y.min(pixel_h);
    let x1 = (sw.x + sw.w).min(pixel_w);
    let y1 = (sw.y + sw.h).min(pixel_h);
    if x1 <= x0 + 2 || y1 <= y0 + 2 {
        return 0;
    }
    let stride = pixel_w as usize * 4;
    let mut total = 0u64;
    let mut n = 0u64;
    for y in (y0 + 1)..(y1 - 1) {
        let row = (y as usize) * stride;
        let up = ((y - 1) as usize) * stride;
        let dn = ((y + 1) as usize) * stride;
        for x in (x0 + 1)..(x1 - 1) {
            let c = row + (x as usize) * 4;
            let l = row + ((x - 1) as usize) * 4;
            let r = row + ((x + 1) as usize) * 4;
            let u = up + (x as usize) * 4;
            let d = dn + (x as usize) * 4;
            let mut sum = 0u32;
            for ch in 0..3 {
                let cv = rgba[c + ch] as i32;
                let mx = [
                    (cv - rgba[l + ch] as i32).abs(),
                    (cv - rgba[r + ch] as i32).abs(),
                    (cv - rgba[u + ch] as i32).abs(),
                    (cv - rgba[d + ch] as i32).abs(),
                ]
                .into_iter()
                .max()
                .unwrap_or(0);
                sum += mx as u32;
            }
            total += sum as u64;
            n += 1;
        }
    }
    if n == 0 {
        return 0;
    }
    ((total * 1000) / n) as u32
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
