// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `stet` says so when a PDF page saves more graphics states at once than
//! the reader keeps.
//!
//! Past the limit a page's `q` is skipped and its matching `Q` restores
//! nothing, so what is drawn may differ from what the file intends. The
//! reader reports it as `ParsePhase::SavedStateLimit`, and the command
//! line prints that warning as it prints the other kinds that mean the
//! output may not be the whole document as it is.

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
            "stet-pdf-saved-state-{}-{}-{:?}",
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

/// A one-page PDF whose content is `content`.
fn one_page_pdf(content: &[u8]) -> Vec<u8> {
    let mut stream = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
    stream.extend_from_slice(content);
    stream.extend_from_slice(b"\nendstream");
    let objects: [Vec<u8>; 4] = [
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>".to_vec(),
        stream,
    ];
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend(format!("{} 0 obj\n", i + 1).as_bytes());
        pdf.extend_from_slice(body);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref = pdf.len();
    pdf.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for offset in offsets {
        pdf.extend(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}

/// Render `pdf` to a PNG with `stet`, returning whether it succeeded,
/// standard error, and whether the PNG was written.
fn render(tag: &str, pdf: &[u8]) -> (bool, String, bool) {
    let dir = TempDir::new(tag);
    let input = dir.path().join("page.pdf");
    let output = dir.path().join("page.png");
    std::fs::write(&input, pdf).expect("write input");
    let run = Command::new(stet_bin())
        .args(["--device", "png", "-o"])
        .arg(&output)
        .arg(&input)
        .output()
        .expect("run stet");
    (
        run.status.success(),
        String::from_utf8_lossy(&run.stderr).into_owned(),
        output.exists(),
    )
}

const WARNING: &str = "graphics states saved at once";

#[test]
fn a_page_past_the_saved_state_limit_is_drawn_and_warned_about_once() {
    let mut content = b"q\n".repeat(1100);
    content.extend_from_slice(b"0 0 100 100 re f\n");
    let (ok, stderr, written) = render("past", &one_page_pdf(&content));
    assert!(ok, "{stderr}");
    assert!(written, "no PNG written: {stderr}");
    assert_eq!(stderr.matches(WARNING).count(), 1, "{stderr}");
}

#[test]
fn a_page_within_the_saved_state_limit_has_no_such_warning() {
    let mut content = b"q\n".repeat(1000);
    content.extend_from_slice(b"0 0 100 100 re f\n");
    let (ok, stderr, written) = render("within", &one_page_pdf(&content));
    assert!(ok, "{stderr}");
    assert!(written, "no PNG written: {stderr}");
    assert!(!stderr.contains(WARNING), "{stderr}");
}
