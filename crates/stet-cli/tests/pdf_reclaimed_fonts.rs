// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `--device pdf` embeds fonts that a `restore` reclaimed before the end of
//! the job. It panicked building the document, reading such a font out of
//! the VM by an id that no longer existed.

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
            "stet-pdf-reclaimed-fonts-{}-{}-{:?}",
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

/// Two pages, each placing a figure whose font its `save … restore`
/// defines, the second reusing the first one's reclaimed font id.
const JOB: &str = "%!PS
/figure {
  /b4_inc_state save def
  /Base /Times-Roman findfont def
  /Fig Base dup length dict begin
    { 1 index /FID ne { def } { pop pop } ifelse } forall
    /FontName /Fig def
  currentdict end definefont pop
  /Fig findfont 24 scalefont setfont 72 600 moveto show
  b4_inc_state restore
} def
(Figure one) figure showpage
(Figure two) figure showpage
";

#[test]
fn fonts_reclaimed_before_the_end_of_the_job_are_embedded() {
    let dir = TempDir::new("eps");
    let input = dir.path().join("job.ps");
    let output = dir.path().join("job.pdf");
    std::fs::write(&input, JOB).unwrap();
    let out = Command::new(stet_bin())
        .args(["--device", "pdf", "-o"])
        .arg(&output)
        .arg(&input)
        .output()
        .expect("run stet");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stet failed: {stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
    let pdf = std::fs::read(&output).expect("the PDF was written");
    assert!(
        pdf.windows(b"/FontFile".len()).any(|w| w == b"/FontFile"),
        "the figure's font is embedded"
    );
}
