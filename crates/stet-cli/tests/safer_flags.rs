// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `--safer` and the `--permit-…` flags reach the job: a program is held
//! to what is permitted, a permit names a folder that exists, and stet's
//! own output is not restricted.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A scratch folder with `data/secret.txt`, `jobs/`, and `out/`, removed
/// afterwards.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("stet-safer-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        for dir in ["data", "jobs", "out"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("data/secret.txt"), b"secret").unwrap();
        Self(root)
    }

    /// Write the job `name` under `jobs/`; returns its path.
    fn job(&self, name: &str, body: &str) -> PathBuf {
        let path = self.0.join("jobs").join(name);
        std::fs::write(
            &path,
            format!("%!PS\n{body}\n0 0 10 10 rectfill showpage\n"),
        )
        .unwrap();
        path
    }

    /// `path` as a PostScript string's contents.
    fn ps(&self, path: &str) -> String {
        self.0
            .join(path)
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn stet(args: &[&str], input: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_stet"))
        .args(args)
        .arg(input)
        .output()
        .expect("run stet")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn safer_refuses_a_file_the_job_was_not_permitted() {
    let s = Scratch::new("refuse");
    let job = s.job(
        "read.ps",
        &format!("({}) (r) file pop", s.ps("data/secret.txt")),
    );
    let full = stet(&["--device", "null"], &job);
    assert_eq!(full.status.code(), Some(0), "{}", stderr(&full));

    let safer = stet(&["--device", "null", "--safer"], &job);
    assert_eq!(safer.status.code(), Some(1));
    let out = String::from_utf8_lossy(&safer.stdout).into_owned() + &stderr(&safer);
    assert!(out.contains("invalidfileaccess"), "{out}");
}

#[test]
fn a_permit_lets_the_job_read_and_implies_safer() {
    let s = Scratch::new("permit");
    let data = s.0.join("data");
    let data = data.to_str().unwrap();
    let job = s.job(
        "read.ps",
        &format!(
            "({}) (r) file pop \
             {{ ({}) (r) file }} stopped not {{ 1 0 div }} if",
            s.ps("data/secret.txt"),
            s.ps("jobs/read.ps"),
        ),
    );
    let output = stet(&["--device", "null", "--permit-read", data], &job);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
}

#[test]
fn a_write_permit_creates_its_file_and_output_needs_none() {
    let s = Scratch::new("write");
    let target = s.0.join("out/made.txt");
    let job = s.job(
        "write.ps",
        &format!(
            "({}) (w) file dup (made) writestring closefile",
            s.ps("out/made.txt")
        ),
    );
    let png = s.0.join("out/page.png");
    let output = stet(
        &[
            "--device",
            "png",
            "--dpi",
            "18",
            "--permit-write",
            target.to_str().unwrap(),
            "-o",
            png.to_str().unwrap(),
        ],
        &job,
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(std::fs::read(&target).unwrap(), b"made");
    assert!(png.exists(), "-o is stet's own output, not the program's");
}

#[test]
fn a_permit_for_a_missing_folder_stops_the_command() {
    let s = Scratch::new("missing");
    let job = s.job("ok.ps", "");
    let missing = s.0.join("nowhere/x");
    for flag in [
        "--permit-read",
        "--permit-write",
        "--permit-control",
        "--permit-all",
    ] {
        let output = stet(&["--device", "null", flag, missing.to_str().unwrap()], &job);
        assert_eq!(output.status.code(), Some(1), "{flag}");
        assert!(
            stderr(&output).contains(flag),
            "{flag}: {}",
            stderr(&output)
        );
    }
}

#[test]
fn stet_text_takes_the_flags_too() {
    let s = Scratch::new("text");
    let job = s.job(
        "text.ps",
        &format!(
            "({}) (r) file pop /Helvetica findfont 12 scalefont setfont 10 10 moveto (hello) show",
            s.ps("data/secret.txt")
        ),
    );
    let text = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_stet"))
            .arg("text")
            .args(args)
            .arg(&job)
            .output()
            .expect("run stet text")
    };
    let full = text(&[]);
    assert_eq!(full.status.code(), Some(0), "{}", stderr(&full));
    assert!(String::from_utf8_lossy(&full.stdout).contains("hello"));
    assert_eq!(text(&["--safer"]).status.code(), Some(1));
    let data = s.0.join("data");
    let permitted = text(&["--permit-read", data.to_str().unwrap()]);
    assert_eq!(permitted.status.code(), Some(0), "{}", stderr(&permitted));
}
