// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! End-to-end behaviour of `-o` / `--output`.
//!
//! The flag's contract is that the *template* decides per-page naming, not the
//! page count — which is what lets PostScript and PDF input behave alike, given
//! that a PostScript page count is unknowable until the program has run. These
//! tests drive the built binary because the interesting behaviour (which files
//! land on disk, what the exit status is) only exists at that level.

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
            "stet-output-flag-{}-{}-{:?}",
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

    /// Write a PostScript file of `pages` solid-colour pages.
    fn write_ps(&self, name: &str, pages: usize) -> PathBuf {
        let mut src = String::from("%!PS\n");
        for i in 0..pages {
            let shade = i as f64 / pages.max(1) as f64;
            src.push_str(&format!(
                "{} setgray 0 0 612 792 rectfill showpage\n",
                shade
            ));
        }
        let path = self.0.join(name);
        std::fs::write(&path, src).expect("write ps");
        path
    }

    /// Names of the files present, sorted.
    fn entries(&self) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(&self.0)
            .expect("read temp dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
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

#[test]
fn ps_single_page_without_token_uses_the_exact_path() {
    let dir = TempDir::new("ps-exact");
    dir.write_ps("in.ps", 1);
    let (code, out) = run_in(&dir, &["--device", "png", "-o", "chosen.png", "in.ps"]);
    assert_eq!(code, 0, "{}", out);
    // Exactly the requested name — no `-0001`, no extension mangling.
    assert!(
        dir.entries().contains(&"chosen.png".to_string()),
        "{:?}",
        dir.entries()
    );
}

#[test]
fn ps_token_expands_even_for_a_single_page() {
    // Matches Ghostscript: a `%d` template expands whenever it is present.
    let dir = TempDir::new("ps-token-single");
    dir.write_ps("in.ps", 1);
    let (code, out) = run_in(&dir, &["--device", "png", "-o", "p-%03d.png", "in.ps"]);
    assert_eq!(code, 0, "{}", out);
    assert!(
        dir.entries().contains(&"p-001.png".to_string()),
        "{:?}",
        dir.entries()
    );
}

#[test]
fn ps_token_numbers_every_page() {
    let dir = TempDir::new("ps-token-multi");
    dir.write_ps("in.ps", 3);
    let (code, out) = run_in(&dir, &["--device", "png", "-o", "p-%03d.png", "in.ps"]);
    assert_eq!(code, 0, "{}", out);
    let files = dir.entries();
    for want in ["p-001.png", "p-002.png", "p-003.png"] {
        assert!(
            files.contains(&want.to_string()),
            "missing {}: {:?}",
            want,
            files
        );
    }
}

#[test]
fn ps_multipage_without_token_fails_and_keeps_page_one() {
    // Ghostscript streams every page into the one path, silently leaving
    // several concatenated images in a single file. stet stops instead.
    let dir = TempDir::new("ps-multi-notoken");
    dir.write_ps("in.ps", 3);
    let (code, out) = run_in(&dir, &["--device", "png", "-o", "out.png", "in.ps"]);
    assert_ne!(code, 0, "expected a failing exit status\n{}", out);
    assert!(
        out.contains("no '%d' page-number token"),
        "message should say what is wrong:\n{}",
        out
    );
    assert!(
        out.contains("out-%03d.png"),
        "message should suggest a usable template:\n{}",
        out
    );
    // Page 1 is left intact, and no later page overwrote it.
    let files = dir.entries();
    assert!(files.contains(&"out.png".to_string()), "{:?}", files);
    assert_eq!(
        files.iter().filter(|f| f.ends_with(".png")).count(),
        1,
        "only page 1 should exist: {:?}",
        files
    );
}

#[test]
fn ps_default_naming_is_unchanged_without_the_flag() {
    // The historical convention that every existing script and the visual
    // suites depend on: four digits, applied even to a one-page job.
    let dir = TempDir::new("ps-default");
    dir.write_ps("in.ps", 1);
    let (code, out) = run_in(&dir, &["--device", "png", "in.ps"]);
    assert_eq!(code, 0, "{}", out);
    assert!(
        dir.entries().contains(&"in-0001.png".to_string()),
        "{:?}",
        dir.entries()
    );
}

#[test]
fn ps_to_pdf_writes_the_exact_path_for_a_multipage_job() {
    // PDF accumulates every page into one file, so one name serves the job
    // and the multi-page rule must not fire.
    let dir = TempDir::new("ps-pdf");
    dir.write_ps("in.ps", 3);
    let (code, out) = run_in(&dir, &["--device", "pdf", "-o", "book.pdf", "in.ps"]);
    assert_eq!(code, 0, "{}", out);
    assert!(
        dir.entries().contains(&"book.pdf".to_string()),
        "{:?}",
        dir.entries()
    );
}

#[test]
fn ps_to_pdf_honours_a_path_with_no_extension() {
    // The name is used verbatim; stet must not append `.pdf` to it.
    let dir = TempDir::new("ps-pdf-noext");
    dir.write_ps("in.ps", 1);
    let (code, out) = run_in(&dir, &["--device", "pdf", "-o", "bare", "in.ps"]);
    assert_eq!(code, 0, "{}", out);
    let files = dir.entries();
    assert!(files.contains(&"bare".to_string()), "{:?}", files);
    assert!(!files.contains(&"bare.pdf".to_string()), "{:?}", files);
}

#[test]
fn pdf_device_rejects_a_page_token() {
    let dir = TempDir::new("pdf-token");
    dir.write_ps("in.ps", 2);
    let (code, out) = run_in(&dir, &["--device", "pdf", "-o", "x-%03d.pdf", "in.ps"]);
    assert_ne!(code, 0, "{}", out);
    assert!(out.contains("writes all pages to one file"), "{}", out);
}

#[test]
fn rejects_multiple_input_files() {
    let dir = TempDir::new("multi-input");
    dir.write_ps("a.ps", 1);
    dir.write_ps("b.ps", 1);
    let (code, out) = run_in(&dir, &["--device", "png", "-o", "out.png", "a.ps", "b.ps"]);
    assert_ne!(code, 0, "{}", out);
    assert!(out.contains("single input file"), "{}", out);
}

#[test]
fn rejects_a_malformed_template_before_rendering() {
    let dir = TempDir::new("bad-template");
    dir.write_ps("in.ps", 1);
    let (code, out) = run_in(&dir, &["--device", "png", "-o", "out-%s.png", "in.ps"]);
    assert_ne!(code, 0, "{}", out);
    assert!(out.contains("unsupported conversion"), "{}", out);
    // Nothing was rendered: the input is the only file present.
    assert_eq!(
        dir.entries(),
        vec!["in.ps".to_string()],
        "{:?}",
        dir.entries()
    );
}

#[test]
fn rejects_devices_that_write_no_file() {
    let dir = TempDir::new("null-device");
    dir.write_ps("in.ps", 1);
    let (code, out) = run_in(&dir, &["--device", "null", "-o", "out.png", "in.ps"]);
    assert_ne!(code, 0, "{}", out);
    assert!(out.contains("writes no file"), "{}", out);
}

#[test]
fn stdout_target_is_refused_with_a_clear_message() {
    // Not implemented yet; it must not be silently treated as a filename.
    let dir = TempDir::new("stdout");
    dir.write_ps("in.ps", 1);
    let (code, out) = run_in(&dir, &["--device", "png", "-o", "-", "in.ps"]);
    assert_ne!(code, 0, "{}", out);
    assert!(out.contains("not supported yet"), "{}", out);
}
