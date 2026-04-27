// Integration tests for stet-pdf-reader.
// Tests that depend on sample PDFs skip gracefully when the file is absent.

use stet_pdf_reader::PdfDocument;

fn try_load_pdf(name: &str) -> Option<Vec<u8>> {
    let path = format!(
        "{}/ps_samples/{name}",
        env!("CARGO_MANIFEST_DIR").replace("/crates/stet-pdf-reader", "")
    );
    match std::fs::read(&path) {
        Ok(data) => Some(data),
        Err(_) => {
            eprintln!("Skipping {name} — not found");
            None
        }
    }
}

#[test]
fn parse_hospital_pdf() {
    let Some(data) = try_load_pdf("hospital.pdf") else {
        return;
    };
    let doc = PdfDocument::from_bytes(&data).unwrap();
    assert_eq!(doc.page_count(), 1);

    let (w, h) = doc.page_size(0).unwrap();
    assert!(w > 100.0 && w < 2000.0, "width = {w}");
    assert!(h > 100.0 && h < 2000.0, "height = {h}");

    let contents = doc.page_contents(0).unwrap();
    assert!(!contents.is_empty(), "page should have content stream");
}

#[test]
fn parse_other_sample_pdfs() {
    for name in &["10-ch8.pdf", "ppst32.pdf"] {
        let Some(data) = try_load_pdf(name) else {
            continue;
        };
        let doc = PdfDocument::from_bytes(&data).unwrap_or_else(|e| {
            panic!("failed to parse {name}: {e}");
        });
        assert!(doc.page_count() > 0, "{name} should have pages");

        let (w, h) = doc.page_size(0).unwrap();
        assert!(w > 0.0 && h > 0.0, "{name}: invalid page size {w}x{h}");
    }
}

#[test]
fn render_hospital_pdf() {
    let Some(data) = try_load_pdf("hospital.pdf") else {
        return;
    };
    let doc = PdfDocument::from_bytes(&data).unwrap();

    let display_list = doc.render_page(0, 72.0).unwrap();
    assert!(!display_list.is_empty(), "display list should not be empty");
}

#[test]
fn render_hospital_pdf_to_rgba() {
    let Some(data) = try_load_pdf("hospital.pdf") else {
        return;
    };
    let doc = PdfDocument::from_bytes(&data).unwrap();

    let (rgba, w, h) = doc.render_page_to_rgba(0, 72.0).unwrap();
    assert!(w > 0 && h > 0, "pixel dimensions should be positive");
    assert_eq!(rgba.len(), (w * h * 4) as usize, "RGBA data size mismatch");
}

#[test]
fn render_hospital_pdf_to_png() {
    let Some(data) = try_load_pdf("hospital.pdf") else {
        return;
    };
    let doc = PdfDocument::from_bytes(&data).unwrap();

    let (rgba, w, h) = doc.render_page_to_rgba(0, 150.0).unwrap();

    let out_path = format!(
        "{}/ps_samples/hospital-pdf-render.png",
        env!("CARGO_MANIFEST_DIR").replace("/crates/stet-pdf-reader", "")
    );
    let file = std::fs::File::create(&out_path).unwrap();
    let bw = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(bw, w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&rgba).unwrap();
}

#[test]
#[ignore]
fn dump_display_list_stats() {
    use stet_graphics::display_list::DisplayElement;

    let Some(data) = try_load_pdf("hospital.pdf") else {
        return;
    };
    let doc = PdfDocument::from_bytes(&data).unwrap();
    let dl = doc.render_page(0, 72.0).unwrap();

    let mut fills = 0u32;
    let mut strokes = 0u32;
    let mut clips = 0u32;
    let mut images = 0u32;

    for elem in dl.elements() {
        match elem {
            DisplayElement::Fill { .. } => fills += 1,
            DisplayElement::Stroke { path, .. } => {
                strokes += 1;
                let _ = path;
            }
            DisplayElement::Clip { .. } => clips += 1,
            DisplayElement::Image { .. } => images += 1,
            _ => {}
        }
    }

    eprintln!(
        "hospital.pdf: {} elements ({fills} fills, {strokes} strokes, {clips} clips, {images} images)",
        dl.len()
    );
}

/// Test against external PDFs if available (e.g., downloaded or from /tmp).
#[test]
fn parse_external_pdfs() {
    let paths: Vec<_> = std::fs::read_dir("/tmp")
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "pdf"))
        .map(|e| e.path())
        .collect();

    let mut parsed = 0;
    let mut failed = 0;
    for path in &paths {
        if let Ok(data) = std::fs::read(path) {
            match PdfDocument::from_bytes(&data) {
                Ok(_) => parsed += 1,
                Err(stet_pdf_reader::PdfError::PasswordRequired) => parsed += 1,
                Err(_e) => {
                    failed += 1;
                    eprintln!("  FAIL: {}: {_e}", path.display());
                }
            }
        }
    }
    if !paths.is_empty() {
        eprintln!(
            "External PDFs: {parsed} parsed, {failed} failed out of {} total",
            paths.len()
        );
    }
}

/// Outline-tree extraction must never panic and must return a cached
/// reference. PDFs without bookmarks return an empty slice.
#[test]
fn outline_no_panic() {
    let candidates = ["hospital.pdf", "10-ch8.pdf", "ppst32.pdf"];
    for name in candidates {
        let Some(data) = try_load_pdf(name) else {
            continue;
        };
        let doc = PdfDocument::from_bytes(&data).unwrap();
        let a = doc.outline();
        let b = doc.outline();
        assert!(std::ptr::eq(a, b), "{name}: outline() not cached");
    }
}

/// Calling `metadata()` and `viewer_preferences()` on real-world PDFs must
/// never panic, regardless of how the document populates (or fails to
/// populate) those fields. The accessors are also cached behind
/// `OnceCell`; calling twice should produce the same reference.
#[test]
fn metadata_and_viewer_prefs_no_panic() {
    let candidates = ["hospital.pdf", "10-ch8.pdf", "ppst32.pdf"];
    let mut visited = 0;
    for name in candidates {
        let Some(data) = try_load_pdf(name) else {
            continue;
        };
        let doc = PdfDocument::from_bytes(&data).unwrap();

        let m1 = doc.metadata();
        let m2 = doc.metadata();
        assert!(std::ptr::eq(m1, m2), "{name}: metadata() not cached");

        let v1 = doc.viewer_preferences();
        let v2 = doc.viewer_preferences();
        assert!(
            std::ptr::eq(v1, v2),
            "{name}: viewer_preferences() not cached"
        );

        // Producer is set by nearly every PDF generator; use it as a
        // soft sanity check that *something* was parsed when the file
        // does have an /Info dict.
        if let Some(producer) = m1.producer.as_deref() {
            assert!(!producer.is_empty(), "{name}: parsed empty producer string");
        }
        visited += 1;
    }
    if visited == 0 {
        eprintln!("(no sample PDFs available to exercise metadata API)");
    }
}
