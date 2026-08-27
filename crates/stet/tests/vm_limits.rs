// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Regression tests for how the PostScript VM ceiling is reported and raised.
//
// These need the full interpreter rather than a bare `Context`: `errordict`
// and `$error` come from the Init resources, and both defects here were
// invisible without them.
//
//   - `errordict` registered the handler under `/VMError` while
//     `PsError::VMError` displays as `VMerror` (PLRM's spelling, 35 times, and
//     Ghostscript's). The lookup missed, so the interpreter printed the error
//     and *carried on past the failed allocation* — worse than uncatchable.
//     Harmless while the variant had no producer; reachable the moment
//     `--max-vm` started raising it.
//   - `currentuserparams` copied the seeded dict, so it answered 0 for
//     `MaxLocalVM` unless a program had set it via `setuserparams` — telling
//     a program there was no limit moments before it hit one.

use stet::Interpreter;

/// Run a program on a fully bootstrapped interpreter under a VM ceiling, and
/// return whether the top of the operand stack is `true`.
fn probe_with_cap(source: &str, max_local_vm: usize) -> bool {
    let mut interp = Interpreter::builder().suppress_output().build();
    let ctx = interp.context();
    ctx.max_local_vm = max_local_vm;
    if stet_engine::eval::parse_and_exec(ctx, source.as_bytes()).is_err() {
        return false;
    }
    matches!(
        ctx.o_stack.peek(0).map(|o| o.value),
        Ok(stet_core::object::PsValue::Bool(true))
    )
}

/// The ceiling must raise an error a program can actually catch, under the
/// name PLRM gives it.
#[test]
fn exceeding_the_ceiling_raises_a_catchable_vmerror() {
    assert!(
        probe_with_cap(
            "{ 64000000 array pop } stopped { $error /errorname get /VMerror eq } { false } ifelse",
            64 * 1024 * 1024,
        ),
        "an oversized allocation must raise a catchable /VMerror"
    );
}

/// The failure must *stop* the program. With the handler unreachable, the
/// interpreter printed the error and ran on, leaving the program to continue
/// as though the allocation had succeeded.
#[test]
fn exceeding_the_ceiling_halts_execution() {
    assert!(
        !probe_with_cap("64000000 array pop true", 64 * 1024 * 1024),
        "execution must not continue past a failed allocation"
    );
}

/// Allocation inside the ceiling is untouched.
#[test]
fn allocation_within_the_ceiling_is_unaffected() {
    assert!(
        probe_with_cap("1000000 string pop 100000 array pop true", 64 * 1024 * 1024),
        "a 1 MB string and a 100k array are ordinary"
    );
}

/// `currentuserparams` must report the ceiling in force. It can be set three
/// ways — the built-in default, the CLI's `--max-vm`, and `setuserparams` —
/// and only the last writes the dict the query used to read.
#[test]
fn currentuserparams_reports_the_live_ceiling() {
    assert!(
        probe_with_cap(
            "currentuserparams /MaxLocalVM get 67108864 eq",
            64 * 1024 * 1024,
        ),
        "currentuserparams must report the ceiling actually in force"
    );
}

/// The default must be reported as something generous rather than as the
/// dict's 0 seed, which reads as "no limit".
#[test]
fn the_default_ceiling_is_reported_not_zero() {
    let mut interp = Interpreter::builder().suppress_output().build();
    let ctx = interp.context();
    let expected = ctx.max_local_vm as i64;
    assert!(expected > 0, "the default ceiling must be positive");
    assert!(
        stet_engine::eval::parse_and_exec(
            ctx,
            format!("currentuserparams /MaxLocalVM get {expected} eq").as_bytes(),
        )
        .is_ok()
    );
    assert!(
        matches!(
            ctx.o_stack.peek(0).map(|o| o.value),
            Ok(stet_core::object::PsValue::Bool(true))
        ),
        "default MaxLocalVM must be reported as {expected}, not 0"
    );
}

/// `setuserparams` must still win, and must still be reported.
#[test]
fn setuserparams_still_sets_and_reports_the_ceiling() {
    assert!(
        probe_with_cap(
            "<< /MaxLocalVM 33554432 >> setuserparams \
             currentuserparams /MaxLocalVM get 33554432 eq",
            64 * 1024 * 1024,
        ),
        "setuserparams must override the ceiling and be reported back"
    );
}
