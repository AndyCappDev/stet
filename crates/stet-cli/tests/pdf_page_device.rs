// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `--device pdf` keeps one document per job across `setpagedevice`.
//!
//! `setpagedevice` replaced the PDF device, so a job lost every page before
//! its last call, and an EPS lost the TrimBox the CLI sets on the device it
//! installs. Now that the device stays, the CLI gives each job its own, or
//! the second file of a run would carry the first one's pages.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Path to the `stet` binary built alongside this test.
fn stet_bin() -> PathBuf {
    // target/<profile>/deps/<test> → target/<profile>/stet
    let mut p = std::env::current_exe().expect("test exe path");
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.join(if cfg!(windows) { "stet.exe" } else { "stet" })
}

/// A scratch directory that cleans itself up.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "stet-pdf-page-device-{}-{}-{:?}",
            tag,
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("create temp dir");
        Self(p)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Run `stet --device pdf` on `inputs`, each written to the directory under
/// its name, and return the directory.
fn run(tag: &str, inputs: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new(tag);
    let mut cmd = Command::new(stet_bin());
    cmd.args(["--device", "pdf"]);
    for (name, body) in inputs {
        let path = dir.path().join(name);
        std::fs::write(&path, body).unwrap();
        cmd.arg(path);
    }
    let out = cmd.output().expect("run stet");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stet failed: {stderr}");
    dir
}

/// The bytes of the PDF `name` in `dir`.
fn pdf(dir: &TempDir, name: &str) -> Vec<u8> {
    std::fs::read(dir.path().join(name)).expect("the PDF was written")
}

fn page_count(bytes: &[u8]) -> usize {
    stet_pdf_reader::PdfDocument::from_bytes(bytes)
        .unwrap()
        .page_count()
}

/// Two files in one run: each PDF has its own pages only.
#[test]
fn each_file_of_a_run_gets_its_own_document() {
    let page = "<< /PageSize [612 792] >> setpagedevice showpage\n";
    let dir = run(
        "jobs",
        &[
            ("three.ps", &format!("%!PS\n{page}{page}{page}")),
            ("two.ps", &format!("%!PS\n{page}{page}")),
        ],
    );
    assert_eq!(page_count(&pdf(&dir, "three.pdf")), 3);
    assert_eq!(page_count(&pdf(&dir, "two.pdf")), 2);
}

/// An EPS whose program calls `setpagedevice` keeps the TrimBox of its
/// bounding box.
#[test]
fn an_eps_keeps_its_trim_box_across_setpagedevice() {
    let eps = "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 200 100\n\
               << /PageSize [200 100] >> setpagedevice\n\
               0 0 moveto 200 100 lineto stroke\n";
    let dir = run("eps", &[("figure.eps", eps)]);
    let bytes = pdf(&dir, "figure.pdf");
    let boxes = stet_pdf_reader::PdfDocument::from_bytes(&bytes)
        .unwrap()
        .page_boxes(0)
        .unwrap();
    assert_eq!(boxes.trim_box, Some([0.0, 0.0, 200.0, 100.0]));
}

/// Each fill's bounding box on page 0 of `bytes`, rendered at 72 dpi.
fn fill_boxes(bytes: &[u8]) -> Vec<[f64; 4]> {
    use stet_fonts::geometry::PathSegment;
    use stet_graphics::display_list::DisplayElement;
    let doc = stet_pdf_reader::PdfDocument::from_bytes(bytes).unwrap();
    doc.render_page(0, 72.0)
        .unwrap()
        .elements()
        .iter()
        .filter_map(|e| match e {
            DisplayElement::Fill { path, .. } => {
                let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
                for s in &path.segments {
                    let pts: &[(f64, f64)] = match s {
                        PathSegment::MoveTo(x, y) | PathSegment::LineTo(x, y) => &[(*x, *y)],
                        PathSegment::CurveTo { x3, y3, .. } => &[(*x3, *y3)],
                        PathSegment::ClosePath => &[],
                    };
                    for &(x, y) in pts {
                        b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
                    }
                }
                Some(b)
            }
            _ => None,
        })
        .collect()
}

/// PDF → PDF keeps a page that is not whole points — it was rounded to
/// them, MediaBox and content — with the content where the source has it.
#[test]
fn pdf_to_pdf_keeps_the_exact_page_size() {
    let job = "%!PS\n<< /PageSize [595.276 841.89] >> setpagedevice\n\
               /Times-Roman findfont 20 scalefont setfont\n\
               10 10 moveto (low) show 535 817 moveto (high) show showpage\n";
    let dir = run("pdf2pdf", &[("source.ps", job)]);
    let source = pdf(&dir, "source.pdf");
    let output = dir.path().join("copy.pdf");
    let out = Command::new(stet_bin())
        .args(["--device", "pdf", "-o"])
        .arg(&output)
        .arg(dir.path().join("source.pdf"))
        .output()
        .expect("run stet");
    assert!(
        out.status.success(),
        "stet failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let copy = pdf(&dir, "copy.pdf");
    let media_box = |bytes: &[u8]| {
        stet_pdf_reader::PdfDocument::from_bytes(bytes)
            .unwrap()
            .page_boxes(0)
            .unwrap()
            .media_box
    };
    assert_eq!(media_box(&source), [0.0, 0.0, 595.276, 841.89]);
    assert_eq!(media_box(&copy), [0.0, 0.0, 595.276, 841.89]);
    let (a, b) = (fill_boxes(&source), fill_boxes(&copy));
    assert_eq!(a.len(), b.len());
    for (a, b) in a.iter().zip(&b) {
        assert!(
            a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.02),
            "source {a:?}, copy {b:?}"
        );
    }
}
