// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! End-to-end behaviour of `--box`: rendering one area of each PDF page as
//! the page, to PNG and to PDF, and the inputs and devices it refuses.

use std::path::{Path, PathBuf};
use std::process::Command;

use stet_pdf_reader::PdfDocument;

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
            "stet-box-flag-{}-{}-{:?}",
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

    /// A 300×200 pt page with a crop box and a 120×100 pt art box.
    fn write_pdf(&self, name: &str) -> PathBuf {
        let content = "1 0 0 rg 20 20 100 60 re f";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] \
             /CropBox [10 10 290 190] /ArtBox [40 30 160 130] \
             /Contents 4 0 R /Resources << >> >>"
                .to_string(),
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            ),
        ];
        let mut pdf = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
        }
        let xref = pdf.len();
        pdf.extend(format!("xref\n0 {}\n0000000000 65535 f\r\n", objects.len() + 1).as_bytes());
        for off in offsets {
            pdf.extend(format!("{off:010} 00000 n\r\n").as_bytes());
        }
        pdf.extend(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        let path = self.0.join(name);
        std::fs::write(&path, pdf).expect("write pdf");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Run stet with `args` inside `dir`; returns (exit code, stdout+stderr).
fn run_in(dir: &TempDir, args: &[&str]) -> (i32, String) {
    let out = Command::new(stet_bin())
        .current_dir(dir.path())
        .args(args)
        .output()
        .expect("run stet");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), text)
}

/// Width and height from a PNG's IHDR chunk.
fn png_size(path: &Path) -> (u32, u32) {
    let data = std::fs::read(path).expect("read png");
    let be = |at: usize| u32::from_be_bytes(data[at..at + 4].try_into().unwrap());
    (be(16), be(20))
}

#[test]
fn png_renders_the_named_box() {
    let dir = TempDir::new("png-art");
    dir.write_pdf("in.pdf");
    let (code, out) = run_in(
        &dir,
        &[
            "--device", "png", "--dpi", "72", "--box", "art", "-o", "out.png", "in.pdf",
        ],
    );
    assert_eq!(code, 0, "{out}");
    assert_eq!(png_size(&dir.path().join("out.png")), (120, 100));
}

#[test]
fn png_renders_a_rectangle() {
    let dir = TempDir::new("png-rect");
    dir.write_pdf("in.pdf");
    let (code, out) = run_in(
        &dir,
        &[
            "--device",
            "png",
            "--dpi",
            "72",
            "--box",
            "0,0,100,50",
            "-o",
            "out.png",
            "in.pdf",
        ],
    );
    assert_eq!(code, 0, "{out}");
    assert_eq!(png_size(&dir.path().join("out.png")), (100, 50));
}

/// The box is the page, so `--width` scales the box, not the page.
#[test]
fn width_fits_the_box() {
    let dir = TempDir::new("png-width");
    dir.write_pdf("in.pdf");
    let (code, out) = run_in(
        &dir,
        &[
            "--device", "png", "--width", "240", "--box", "art", "-o", "out.png", "in.pdf",
        ],
    );
    assert_eq!(code, 0, "{out}");
    assert_eq!(png_size(&dir.path().join("out.png")), (240, 200));
}

#[test]
fn pdf_output_pages_are_the_box() {
    let dir = TempDir::new("pdf-art");
    dir.write_pdf("in.pdf");
    let (code, out) = run_in(
        &dir,
        &["--device", "pdf", "--box", "art", "-o", "out.pdf", "in.pdf"],
    );
    assert_eq!(code, 0, "{out}");
    let data = std::fs::read(dir.path().join("out.pdf")).expect("read output");
    let doc = PdfDocument::from_bytes(&data).expect("parse output");
    assert_eq!(doc.page_size(0).unwrap(), (120.0, 100.0));
}

#[test]
fn postscript_input_is_refused() {
    let dir = TempDir::new("ps");
    std::fs::write(dir.path().join("in.ps"), "%!PS\nshowpage\n").unwrap();
    let (code, out) = run_in(&dir, &["--device", "png", "--box", "art", "in.ps"]);
    assert_ne!(code, 0);
    assert!(out.contains("--box applies to PDF input only"), "{out}");
}

#[test]
fn other_devices_are_refused() {
    let dir = TempDir::new("null");
    dir.write_pdf("in.pdf");
    let (code, out) = run_in(&dir, &["--device", "null", "--box", "art", "in.pdf"]);
    assert_ne!(code, 0);
    assert!(out.contains("--box is only supported for"), "{out}");
}

#[test]
fn a_bad_value_is_refused() {
    let dir = TempDir::new("bad");
    dir.write_pdf("in.pdf");
    let (code, out) = run_in(&dir, &["--device", "png", "--box", "artbox", "in.pdf"]);
    assert_ne!(code, 0);
    assert!(out.contains("invalid --box value 'artbox'"), "{out}");
}

/// An area under half a pixel at the chosen resolution has no pixels to
/// write. That used to reach the PNG encoder as a zero-width image and
/// panic there; it is an error with a message now.
#[test]
fn an_area_smaller_than_a_pixel_is_an_error_not_a_panic() {
    let dir = TempDir::new("subpixel");
    dir.write_pdf("in.pdf");
    let (code, out) = run_in(
        &dir,
        &[
            "--device",
            "png",
            "--dpi",
            "72",
            "--box",
            "40,30,40.3,30.3",
            "-o",
            "out.png",
            "in.pdf",
        ],
    );
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("0x0 pixels at this resolution"), "{out}");
    assert!(!out.contains("panicked"), "{out}");
    assert!(!dir.path().join("out.png").exists());
}
