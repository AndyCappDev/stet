// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Debug helper: sweep global VM for PLRM 3.7.2 violations.
//!
//! PLRM 3.7.2 forbids a composite object in global VM from referencing an
//! object in local VM, because `restore` may release the local object and
//! leave the global one holding a dangling reference. This example reports
//! every such reference after interpreter bootstrap, and again after running
//! each PostScript file given on the command line.
//!
//! References whose target was allocated before any `save` cannot be
//! reclaimed by `restore` and so cannot dangle — that is the PLRM 3.7.5
//! `systemdict`/`userdict` exception. Those are printed only under
//! `--show-permanent`; by default just the reclaimable ones are listed, since
//! those are the genuine use-after-free hazards.
//!
//! Usage: cargo run --release --example audit_vm -- [--show-permanent] [file.ps …]

use stet_core::vm_audit::{Violation, audit_global_vm};

fn report(label: &str, violations: &[Violation], show_permanent: bool) -> usize {
    let (reclaimable, permanent): (Vec<_>, Vec<_>) =
        violations.iter().partition(|v| v.target_reclaimable);

    println!("== {label} ==");
    println!(
        "   {} global->local reference(s): {} reclaimable, {} permanent",
        violations.len(),
        reclaimable.len(),
        permanent.len()
    );
    for v in &reclaimable {
        println!("   UNSAFE  {v}");
    }
    if show_permanent {
        for v in &permanent {
            println!("   ok      {v}");
        }
    }
    println!();
    reclaimable.len()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let show_permanent = args.iter().any(|a| a == "--show-permanent");
    let files: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();

    let mut interp = stet::Interpreter::builder().suppress_output().build();
    let ctx = interp.context();

    let mut unsafe_total = report("after bootstrap", &audit_global_vm(ctx), show_permanent);

    for path in files {
        let source = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) => {
                eprintln!("{path}: {e}");
                continue;
            }
        };
        // Errors are expected for some corpus files; the audit is still valid.
        let ctx = interp.context();
        if let Err(e) = stet_engine::eval::parse_and_exec_file(ctx, &source, path) {
            eprintln!("{path}: execution stopped: {e:?}");
        }
        unsafe_total = report(
            &format!("after {path}"),
            &audit_global_vm(ctx),
            show_permanent,
        );
    }

    if unsafe_total > 0 {
        std::process::exit(1);
    }
}
