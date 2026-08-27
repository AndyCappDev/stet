// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Regression tests for non-finite numbers and integer-overflow traps.
//
// PLRM: "A numeric computation would produce a meaningless result or one that
// cannot be represented as a number. Possible causes include numeric overflow
// or underflow, division by 0, or inverse transformation of a noninvertible
// matrix." That error is `undefinedresult`, and every arithmetic operator that
// can produce a real lists it.
//
// Three separate defects lived here, and they need different guards:
//
//   - `-9223372036854775808 -1 idiv` *panicked in release*. Integer division
//     overflow is a trap in Rust's semantics, not something `overflow-checks`
//     turns on, so this was a hard crash from 30 bytes of PostScript.
//   - Real overflow yielded `inf` (and then `NaN`) instead of an error, so a
//     program could hand non-finite values to coordinates, line widths, and
//     colour components. Ghostscript raises `undefinedresult` at the
//     equivalent boundary.
//   - A literal `1e999` parsed straight to `inf`, introducing one with no
//     arithmetic at all. It is now declined by the scanner and falls through
//     to the name scanner, so the program gets `undefined` rather than a
//     value. Ghostscript raises `limitcheck`; stet does not, because doing so
//     broke a corpus file whose hex image data contains byte runs that are
//     syntactically reals with 500-digit exponents. See `finite_real_token`.
//
// A `NaN` that reaches geometry does not crash — it makes every comparison
// against it false, so bounds, banding, and winding quietly take the wrong
// branch. Silent wrong output is the failure mode being tested for, which is
// why these assert on the error rather than on surviving.

use stet::Interpreter;

/// Run a PostScript program on a fully bootstrapped interpreter and report
/// whether it left `true` on the operand stack.
///
/// The facade is used rather than a bare `Context` because `errordict` and
/// `$error` come from the Init resources it loads; without them every error
/// collapses to the generic `PsError::Stop` and the specific name is lost.
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

/// Whether `source` raises the named PostScript error.
///
/// Asked in PostScript, which keeps the assertion at the level the PLRM
/// specifies the behaviour at rather than at whatever `PsError` the error
/// machinery happens to surface.
fn raises(source: &str, errorname: &str) -> bool {
    probe(&format!(
        "{{ {source} }} stopped {{ $error /errorname get /{errorname} eq }} {{ false }} ifelse"
    ))
}

fn assert_undefined_result(source: &str) {
    assert!(
        raises(source, "undefinedresult"),
        "expected undefinedresult from `{source}`"
    );
}

/// `source` must complete without error and leave nothing non-finite behind.
///
/// The finiteness half matters as much as the success half: an operator that
/// quietly returned `inf` instead of erroring would otherwise pass. It is
/// checked from Rust because PostScript can no longer name a non-finite value
/// to compare against — which is the point of the change under test.
fn assert_ok_and_finite(source: &str) {
    let mut interp = Interpreter::builder().suppress_output().build();
    let ctx = interp.context();
    assert!(
        stet_engine::eval::parse_and_exec(ctx, source.as_bytes()).is_ok(),
        "`{source}` should succeed"
    );
    assert_stack_is_finite(&mut interp, source);
}

/// Assert that no real left on the operand stack is `inf` or `NaN`.
fn assert_stack_is_finite(interp: &mut Interpreter, source: &str) {
    let ctx = interp.context();
    for i in 0..ctx.o_stack.len() {
        if let Ok(obj) = ctx.o_stack.peek(i)
            && let stet_core::object::PsValue::Real(v) = obj.value
        {
            assert!(v.is_finite(), "`{source}` left {v} on the stack");
        }
    }
}

// --- Integer overflow traps ---

/// `i64::MIN / -1` is the one pair that overflows, and integer division
/// overflow panics in release as well as debug. This aborted the process.
#[test]
fn idiv_of_int_min_by_minus_one_does_not_panic() {
    assert_undefined_result("-9223372036854775808 -1 idiv");
}

/// `%` traps identically to `/` on the same pair.
#[test]
fn mod_of_int_min_by_minus_one_does_not_panic() {
    assert_undefined_result("-9223372036854775808 -1 mod");
}

/// Ordinary integer division is untouched.
#[test]
fn ordinary_integer_division_still_works() {
    for source in [
        "7 2 idiv 3 eq { } { (wrong) = } ifelse",
        "7 2 mod 1 eq { } { (wrong) = } ifelse",
        "-9223372036854775808 1 idiv",
        "-9223372036854775808 2 idiv",
        "-9223372036854775808 -2 idiv",
    ] {
        assert_ok_and_finite(source);
    }
}

/// `abs` and `neg` promote to real at the boundary rather than erroring —
/// stet's documented policy for integer overflow, and what Ghostscript does.
#[test]
fn abs_and_neg_promote_at_the_integer_boundary() {
    assert_ok_and_finite("-9223372036854775808 abs");
    assert_ok_and_finite("-9223372036854775808 neg");
}

// --- Real overflow ---

#[test]
fn arithmetic_overflow_raises_undefined_result() {
    for source in [
        "1e308 1e308 mul",
        "1e308 10 mul",
        "1e308 1e308 add",
        "-1e308 -1e308 add",
        "1e308 -1e308 sub",
        "1e308 1e-308 div",
        "2 10000 exp",
    ] {
        assert_undefined_result(source);
    }
}

/// Division by zero was already handled; it must stay handled.
#[test]
fn division_by_zero_still_raises_undefined_result() {
    assert_undefined_result("1 0 div");
    assert_undefined_result("1 0 idiv");
    assert_undefined_result("1 0 mod");
}

/// Arithmetic that stays in range is unaffected — the guard must not be
/// reachable by ordinary computation.
#[test]
fn ordinary_arithmetic_is_unaffected() {
    for source in [
        "1e308 2 div",
        "1e-308 1e308 mul",
        "1.5 2.5 mul",
        "2 1000 exp",
        "3 4 add 7 eq { } { (wrong) = } ifelse",
    ] {
        assert_ok_and_finite(source);
    }
}

// --- Scanner ---

/// A literal past the `f64` range must not reach the stack as `inf`. The
/// scanner declines it, so it becomes an undefined name and the program
/// errors. The token-level assertion lives in `stet-core`'s tokenizer tests;
/// what matters here is the end-to-end outcome.
#[test]
fn out_of_range_literal_does_not_reach_the_stack() {
    for source in ["1e999", "-1e999", "1.5e400", "1e999 1 add"] {
        let mut interp = Interpreter::builder().suppress_output().build();
        let ctx = interp.context();
        assert!(
            stet_engine::eval::parse_and_exec(ctx, source.as_bytes()).is_err(),
            "`{source}` should not run"
        );
    }
}

/// The boundary is f64's, not Ghostscript's narrower single-precision one:
/// `1e308` is representable here and must keep working.
#[test]
fn representable_literals_still_scan() {
    for source in ["1e308", "-1e308", "1e-308", "1e38", "0.0", "123456789"] {
        assert_ok_and_finite(source);
    }
}

/// `cvr` runs the scanner over string contents, so it is the same ingress by
/// another route — and unlike a source literal, this one is catchable, since
/// the string is scanned at run time rather than while reading the program.
#[test]
fn cvr_of_an_out_of_range_literal_does_not_yield_infinity() {
    let mut interp = Interpreter::builder().suppress_output().build();
    let ctx = interp.context();
    let _ = stet_engine::eval::parse_and_exec(ctx, b"(1e999) cvr");
    assert_stack_is_finite(&mut interp, "(1e999) cvr");
}

// --- Geometry ---

/// A CTM scaled past the representable range makes every transformed
/// coordinate non-finite. PLRM assigns `undefinedresult` to graphics
/// operators under a CTM that cannot be used.
#[test]
fn path_operators_reject_non_finite_coordinates() {
    let blow = "1e300 1e300 scale 1e300 1e300 scale";
    for op in [
        "1 1 moveto",
        "0 0 moveto 1 1 lineto",
        "0 0 moveto 1 1 1 1 1 1 curveto",
        "0 0 1 0 360 arc",
        "0 0 1 360 0 arcn",
        "0 0 moveto 1 1 2 2 1 arcto",
        "0 0 moveto 1 1 2 2 1 arct",
    ] {
        assert_undefined_result(&format!("{blow} {op}"));
    }
    // The relative operators take the current point from before the blow-up,
    // so they need it established first.
    for op in ["1 1 rlineto", "1 1 1 1 1 1 rcurveto"] {
        assert_undefined_result(&format!("0 0 moveto {blow} {op}"));
    }
}

/// Extreme but representable scaling is legitimate and must still draw.
#[test]
fn ordinary_and_extreme_finite_geometry_still_works() {
    for source in [
        "100 100 moveto 200 200 lineto 250 100 150 50 100 100 curveto closepath",
        "100 100 50 0 360 arc",
        "100 100 50 360 0 arcn",
        "100 100 moveto 200 100 200 200 50 arcto pop pop pop pop",
        "100 100 moveto 200 100 200 200 50 arct",
        "1e6 1e6 scale 1 1 moveto 2 2 lineto",
        "1e-6 1e-6 scale 1 1 moveto 2 2 lineto",
    ] {
        assert_ok_and_finite(source);
    }
}

/// `arc_segments` normalises with `while stop < start { stop += 360.0 }`.
/// Negative infinity satisfies that forever while the addition never changes
/// it, so an unguarded non-finite angle is an infinite loop rather than bad
/// geometry. Reaching it needs the scanner guard bypassed, which is why the
/// angle check is separate from the coordinate check.
#[test]
fn arc_with_non_finite_angle_terminates() {
    // Both routes to a non-finite angle are closed, so this must report an
    // error from the scanner or the arithmetic — and above all, must return.
    let mut interp = Interpreter::builder().suppress_output().build();
    let ctx = interp.context();
    assert!(stet_engine::eval::parse_and_exec(ctx, b"0 0 1 0 1e999 arc").is_err());
    assert_undefined_result("0 0 1 1e308 1e308 mul 360 arc");
}
