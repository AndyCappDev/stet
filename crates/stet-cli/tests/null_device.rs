// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `--device null` sends each page to a device that discards it, and erases
//! the page as any other device does.
//!
//! Its `EndPage` once declined every page. Since a declined page is neither
//! sent nor erased (PLRM 6.2.6), every page's marks then stayed in the
//! display list: a whole document accumulated in memory, and the end of the
//! job reported a final page dropped for want of a `showpage` the program
//! had in fact executed.

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
            "stet-null-device-{}-{}-{:?}",
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

/// Run `source` under `--device null`, returning stdout and stderr together.
fn run_null(tag: &str, source: &str) -> String {
    let dir = TempDir::new(tag);
    let file = dir.path().join("job.ps");
    std::fs::write(&file, source).expect("write job");
    let out = Command::new(stet_bin())
        .args(["--device", "null"])
        .arg(&file)
        .output()
        .expect("run stet");
    assert!(out.status.success(), "stet failed: {out:?}");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn pages_are_sent_and_erased() {
    let output = run_null(
        "pages",
        "%!PS\n1 1 3 { pop 72 72 moveto 100 100 lineto stroke showpage \
         (PageCount ) print currentpagedevice /PageCount get == } for\n",
    );
    assert!(
        !output.contains("without a matching `showpage`"),
        "pages ended by showpage reported as dropped:\n{output}"
    );
    let counts: Vec<&str> = output
        .lines()
        .map(str::trim_end)
        .filter(|l| l.starts_with("PageCount "))
        .collect();
    assert_eq!(
        counts,
        ["PageCount 1", "PageCount 2", "PageCount 3"],
        "{output}"
    );
}

#[test]
fn an_unfinished_page_is_still_reported() {
    let output = run_null(
        "unfinished",
        "%!PS\n72 72 moveto 100 100 lineto stroke showpage\n\
         72 72 moveto 200 200 lineto stroke\n",
    );
    assert!(
        output.contains("painted 1 object(s) and then ended without a matching `showpage`"),
        "the marks after the last showpage, and only those, should be reported:\n{output}"
    );
}
