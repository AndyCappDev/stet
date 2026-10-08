// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The exit status says whether every job succeeded.
//!
//! A job stopped by a PostScript error was reported on standard error as
//! `Job N FAILED` while the process exited 0, so a script, a `make` rule or
//! a CI step driving `stet` took a failed conversion for a good one.

use std::path::PathBuf;
use std::process::{Command, Output};

const GOOD: &[u8] = b"%!PS\n0 0 10 10 rectfill showpage\n";
/// Paints a page, then raises `undefined`.
const BAD: &[u8] = b"%!PS\n0 0 10 10 rectfill showpage\nnosuchoperator\n";

/// A file in a scratch directory that is removed afterwards.
struct TempFile(PathBuf);

impl TempFile {
    fn new(name: &str, data: &[u8]) -> Self {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "stet-exit-{}-{:?}-{name}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&p, data).expect("write input");
        Self(p)
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn run(args: &[&str], inputs: &[&TempFile]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_stet"))
        .args(["--device", "null"])
        .args(args)
        .args(inputs.iter().map(|f| &f.0))
        .output()
        .expect("run stet")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_job_that_succeeds_exits_zero() {
    let good = TempFile::new("good.ps", GOOD);
    let output = run(&[], &[&good]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
}

#[test]
fn a_postscript_error_exits_one() {
    let bad = TempFile::new("bad.ps", BAD);
    let output = run(&[], &[&bad]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(stderr(&output).contains("Job 1 FAILED"));
    assert!(stderr(&output).contains("Processed 1 job, 1 failed"));
}

#[test]
fn one_failed_job_fails_the_run_and_the_others_still_run() {
    let good = TempFile::new("good.ps", GOOD);
    let bad = TempFile::new("bad.ps", BAD);
    for inputs in [[&bad, &good], [&good, &bad]] {
        let output = run(&[], &inputs);
        let err = stderr(&output);
        assert_eq!(output.status.code(), Some(1), "{err}");
        assert!(err.contains("completed successfully"), "{err}");
        assert!(err.contains("Processed 2 jobs, 1 failed"), "{err}");
    }
}

#[test]
fn a_timeout_exits_one() {
    let endless = TempFile::new("loop.ps", b"%!PS\n{ } loop\n");
    let output = run(&["--timeout", "1"], &[&endless]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
}

#[test]
fn quit_is_not_a_failure() {
    let quits = TempFile::new("quit.ps", b"%!PS\n0 0 10 10 rectfill showpage quit\n");
    let output = run(&[], &[&quits]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
}

#[test]
fn a_status_the_program_asks_for_wins() {
    let bad = TempFile::new("bad.ps", BAD);
    let asks = TempFile::new("code.ps", b"%!PS\n3 .quitwithcode\n");
    let output = run(&[], &[&bad, &asks]);
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
}
