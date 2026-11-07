// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `stet_fonts::host::deny_host_access` — a program rendering untrusted
//! documents keeps PostScript away from the computer.
//!
//! The switch is process-wide and one-way, so this file holds a single test
//! (each integration-test file is its own process).

use stet::Interpreter;

/// True when `proc` stops (raises an error) in a fresh interpreter. A bare
/// `stop` at the top level ends a job quietly, so running through is turned
/// into an error the job reports (`1 0 div`).
fn stops(proc: &str) -> bool {
    let mut interp = Interpreter::builder().suppress_output().build();
    interp
        .exec(format!("{{ {proc} }} stopped not {{ 1 0 div }} if").as_bytes())
        .is_ok()
}

#[test]
fn denied_host_access_refuses_files_and_runs() {
    let dir = std::env::temp_dir().join(format!("stet-deny-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("secret.ps");
    std::fs::write(&file, b"/secret 42 def\n").unwrap();
    let name = file.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/");

    // Allowed by default: the file opens and runs.
    assert!(!stops(&format!("({name}) (r) file closefile")));
    assert!(!stops(&format!("({name}) run")));

    stet_fonts::host::deny_host_access();
    assert!(!stet_fonts::host::host_access_allowed());

    assert!(stops(&format!("({name}) (r) file")), "file must refuse");
    assert!(stops(&format!("({name}) run")), "run must refuse");
    assert!(stops(&format!("({name}) deletefile")), "deletefile must refuse");
    assert!(file.exists(), "the file is still there");
    let out = dir.join("written.ps").to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/");
    assert!(stops(&format!("({out}) (w) file")), "file for writing must refuse");
    assert!(!dir.join("written.ps").exists());
    // status sees nothing on disk.
    let mut interp = Interpreter::builder().suppress_output().build();
    interp
        .exec(format!("({name}) status {{ 1 0 div }} if").as_bytes())
        .expect("status finds no host file");
    // %stdin reads as empty, %stdout swallows.
    interp
        .exec(b"(%stdin) (r) file read { 1 0 div } if (%stdout) (w) file (x) writestring")
        .expect("standard files are inert");

    std::fs::remove_dir_all(&dir).ok();
}
