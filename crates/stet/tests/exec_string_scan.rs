// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Scanning tokens out of an executable string (`(...) cvx exec`).
//!
//! All strings share one byte arena, and scanning a token can allocate in it:
//! a string literal, or the strings inside a procedure. Growing the arena
//! moves its bytes. The scanner used to read the executable string through a
//! raw pointer into that arena, so a procedure holding enough string literals
//! to grow it was read from freed memory — a segfault in release builds on a
//! valid 2 MB file (PR #20).
//!
//! The scanner now reads a copied window of the string and widens the window
//! when a token may run past it. The tests below assert what was scanned, not
//! just that the job survived, since freed memory does not always fault: it
//! can just as well be read back as the wrong bytes. The remaining tests pin
//! the window edges: tokens that straddle a window boundary, and a token
//! longer than any window but the last.

use stet::Interpreter;

/// Run a PostScript program on a fully bootstrapped interpreter and report
/// whether it left `true` on the operand stack.
fn probe(source: &str) -> bool {
    let mut interp = Interpreter::builder().suppress_output().build();
    let ctx = interp.context();
    if stet_engine::eval::parse_and_exec(ctx, source.as_bytes()).is_err() {
        return false;
    }
    matches!(
        ctx.o_stack.peek(0).map(|o| o.value),
        Ok(stet_core::object::PsValue::Bool(true))
    )
}

/// A procedure whose string literals grow the string arena while the
/// executable string holding it is being scanned.
///
/// The literals are ASCII85 `z` runs, each source byte decoding to four zero
/// bytes, so the procedure allocates four times the executable string's own
/// size: more than the spare capacity the arena can have left after storing
/// that string. Plain `(...)` literals allocate no more than their source,
/// which spare capacity can absorb, and did not reliably reproduce the fault.
#[test]
fn a_procedure_that_grows_the_string_store_is_scanned_intact() {
    const LITERALS: usize = 8;
    const ZS: usize = 262_144;
    let literal = format!("<~{}~> ", "z".repeat(ZS));
    let src = format!(
        "( {{ {} }} ) cvx exec\n\
         dup length {LITERALS} eq\n\
         1 index {last} get length {bytes} eq and\n\
         1 index {last} get {end} get 0 eq and\n\
         exch pop\n",
        literal.repeat(LITERALS),
        last = LITERALS - 1,
        bytes = ZS * 4,
        end = ZS * 4 - 1,
    );
    assert!(probe(&src));
}

/// Many short tokens, of varying lengths so that some straddle each
/// window boundary.
#[test]
fn short_tokens_across_window_boundaries_are_scanned_whole() {
    let mut body = String::from("0 ");
    let mut expected: i64 = 0;
    for i in 0..5_000i64 {
        let n = i * 37 % 100_003;
        body.push_str(&format!("{n} add "));
        expected += n;
    }
    let src = format!("({body}) cvx exec {expected} eq");
    assert!(probe(&src));
}

/// A string literal and a procedure each longer than the first two windows
/// (256 and 2048 bytes), past a run of short tokens.
#[test]
fn a_token_longer_than_the_window_is_scanned_whole() {
    let long = "x".repeat(5_000);
    let proc_body = "1 add ".repeat(1_000);
    let src = format!(
        "(1 2 3 pop pop pop ({long}) {{ {proc_body} }}) cvx exec\n\
         0 exch exec 1000 eq\n\
         exch length 5000 eq and"
    );
    assert!(probe(&src));
}
