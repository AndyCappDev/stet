// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Regression tests for the interpreter's recursion and time limits.
//
// PostScript is Turing-complete, so nothing static bounds what a program does.
// Three separate limits exist, and they cover different failure shapes:
//
//   - procedure `{`-nesting, capped while parsing;
//   - `exec_sync` re-entrancy, capped at call time;
//   - a wall-clock deadline, for a program that makes progress forever.
//
// The first two abort the process when they regress — a stack overflow is a
// SIGSEGV-backed abort, not a panic, so `catch_unwind` cannot contain it and
// the whole test binary dies. That is the intended signal.

use std::time::{Duration, Instant};
use stet_core::context::Context;
use stet_core::error::PsError;

/// Run a PostScript source string, with an optional wall-clock limit.
fn run(source: &str, timeout: Option<Duration>) -> Result<(), PsError> {
    let mut ctx = Context::new();
    stet_ops::build_system_dict(&mut ctx);
    ctx.exec_sync_fn = Some(stet_engine::eval::exec_sync);
    ctx.set_timeout(timeout);
    stet_engine::eval::parse_and_exec(&mut ctx, source.as_bytes())
}

/// Deeply nested `{` in a procedure. `parse_procedure` is recursive descent,
/// so each brace is a native stack frame; 200000 of them is a 400 KB file that
/// aborted the process.
#[test]
fn deeply_nested_procedure_does_not_overflow_the_stack() {
    let source = format!("{}{} pop", "{".repeat(200_000), "}".repeat(200_000));
    // A limitcheck is the expected outcome; anything that returns is a pass.
    let _ = run(&source, None);
}

/// A `/Separation` colour space whose tint transform sets that same colour
/// space. Each round trip is another `exec_sync` frame, and roughly 200 bytes
/// of input aborted the process before the depth cap.
#[test]
fn self_referential_tint_transform_does_not_overflow_the_stack() {
    let source = "\
        /tintproc { } def\n\
        /CS [ /Separation /Spot /DeviceGray { tintproc } ] def\n\
        /tintproc { 0.5 CS setcolorspace 0.5 setcolor } def\n\
        CS setcolorspace 0.5 setcolor\n";
    let _ = run(source, None);
}

/// A program that never terminates but never recurses either — no depth or
/// allocation guard applies, only the deadline.
#[test]
fn infinite_loop_stops_at_the_deadline() {
    let start = Instant::now();
    let err = run("{ } loop", Some(Duration::from_millis(500)));
    let elapsed = start.elapsed();

    assert!(
        matches!(err, Err(PsError::Timeout)),
        "expected Timeout, got {err:?}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "deadline was not honoured; ran for {elapsed:?}"
    );
}

/// A program doing unbounded *arithmetic* rather than looping on an empty
/// procedure — the deadline has to be checked in the dispatch path, not just
/// on a particular operator.
#[test]
fn unbounded_arithmetic_stops_at_the_deadline() {
    let err = run(
        "/n 0 def { /n n 1 add def } loop",
        Some(Duration::from_millis(500)),
    );
    assert!(
        matches!(err, Err(PsError::Timeout)),
        "expected Timeout, got {err:?}"
    );
}

/// With no deadline set, a terminating program must run to completion — the
/// limit must not leak into the default path.
#[test]
fn no_deadline_means_no_limit() {
    assert!(
        run("/n 0 def 1 1 200000 { pop /n n 1 add def } for", None).is_ok(),
        "a long but terminating program must complete when no timeout is set"
    );
}

/// Legitimate nesting must still work — a regression here means a cap was
/// tightened into real territory.
#[test]
fn ordinary_nesting_still_runs() {
    assert!(run("{ { { 1 2 add } exec } exec } exec pop", None).is_ok());
}
