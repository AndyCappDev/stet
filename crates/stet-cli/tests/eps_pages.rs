// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! An EPS renders as one PNG, whether or not it calls `showpage` itself.
//! The CLI adds a `showpage` for an EPS that leaves it out, and used to add
//! one regardless, writing a blank second page for one that does not.

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
            "stet-eps-pages-{}-{}-{:?}",
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

/// The PNGs the CLI writes for an EPS, with its bounding box at the origin
/// or away from it.
fn pngs_for(showpage: bool, llx: u32) -> usize {
    let dir = TempDir::new(&format!("{showpage}-{llx}"));
    let eps = format!(
        "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: {llx} 0 {} 100\n\
         1 0 0 setrgbcolor {llx} 0 50 50 rectfill {}\n",
        llx + 100,
        if showpage { "showpage" } else { "" }
    );
    std::fs::write(dir.path().join("in.eps"), eps).unwrap();
    let status = Command::new(stet_bin())
        .args(["--device", "png", "--dpi", "72", "in.eps"])
        .current_dir(dir.path())
        .output()
        .expect("run stet")
        .status;
    assert!(status.success());
    std::fs::read_dir(dir.path())
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|x| x == "png")
        })
        .count()
}

#[test]
fn an_eps_renders_as_one_png() {
    for showpage in [true, false] {
        for llx in [0, 20] {
            assert_eq!(
                pngs_for(showpage, llx),
                1,
                "showpage: {showpage}, llx {llx}"
            );
        }
    }
}
