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

// === PostScript image dimensions ==========================================
//
// `image`, `imagemask`, and `colorimage` take their dimensions off the
// operand stack and derive every buffer size from them. Only the lower bound
// was checked, so sixty bytes of PostScript could request a 4 x 10^18 byte
// allocation — which aborts the process rather than failing, since a failed
// allocation is not a catchable error.

/// Run a program against a real device and report whether any image reached
/// the display list.
///
/// This is the observable that matters: the interpreter handles a
/// `limitcheck` through its own error handler, so `parse_and_exec` returns
/// `Ok` whether the image drew or was refused.
fn draws_an_image(source: &str) -> bool {
    use stet_core::display_list::DisplayElement;

    let mut ctx = Context::new();
    stet_ops::build_system_dict(&mut ctx);
    ctx.exec_sync_fn = Some(stet_engine::eval::exec_sync);
    let device = stet_render::SkiaDevice::new(64, 64);
    ctx.device = Some(Box::new(device));
    let _ = stet_engine::eval::parse_and_exec(&mut ctx, source.as_bytes());

    ctx.display_list
        .elements()
        .iter()
        .any(|e| matches!(e, DisplayElement::Image { .. }))
}

/// Each operator, asked for an absurd size, must refuse rather than attempt
/// the allocation. A failed allocation aborts the process and is not
/// catchable, so a regression here takes the test binary down.
#[test]
fn oversized_ps_images_are_refused() {
    for (label, source) in [
        (
            "image",
            "2000000000 2000000000 8 [1 0 0 1 0 0] { <00> } image",
        ),
        (
            "imagemask",
            "2000000000 2000000000 true [1 0 0 1 0 0] { <00> } imagemask",
        ),
        (
            "colorimage",
            "2000000000 2000000000 8 [1 0 0 1 0 0] { <00> } false 3 colorimage",
        ),
    ] {
        assert!(
            !draws_an_image(source),
            "{label} must refuse a 2e9 x 2e9 image, not attempt it"
        );
    }
}

/// A prepress-scale image must still be accepted — a 40x28 inch press sheet
/// at 600 dpi is 403M pixels, and an earlier ceiling of 400M rejected it.
#[test]
fn prepress_scale_ps_image_is_accepted() {
    let source = "\
        /row 24000 string def\n\
        0 1 23999 { row exch 128 put } for\n\
        24000 16800 8 [24000 0 0 -16800 0 16800] { row } image\n";
    assert!(
        draws_an_image(source),
        "a 24000x16800 image (403M px) is ordinary prepress and must draw"
    );
}

// === VM ceiling ===========================================================
//
// A failed allocation aborts the process — Rust's allocator does not return
// on OOM — so an oversized request had to be refused *before* it reached the
// allocator. `500000000 array` asked for 16 GB and took stet down.

/// Run with an explicit VM ceiling, returning whether the program completed
/// without the interpreter raising an error.
fn run_with_vm_cap(source: &str, max_local_vm: usize) -> bool {
    let mut ctx = Context::new();
    stet_ops::build_system_dict(&mut ctx);
    ctx.exec_sync_fn = Some(stet_engine::eval::exec_sync);
    ctx.max_local_vm = max_local_vm;
    // A tight deadline as a backstop: the point of these cases is memory, and
    // a regression that turns one into a spin should fail rather than hang.
    ctx.set_timeout(Some(Duration::from_secs(20)));
    stet_engine::eval::parse_and_exec(&mut ctx, source.as_bytes()).is_ok()
}

/// A single enormous request must be refused rather than attempted.
#[test]
fn oversized_single_allocation_is_refused() {
    // 64 MiB ceiling, 256 MB request.
    assert!(
        !run_with_vm_cap("256000000 string pop", 64 * 1024 * 1024),
        "a string far past the ceiling must raise VMerror"
    );
    assert!(
        !run_with_vm_cap("64000000 array pop", 64 * 1024 * 1024),
        "an array far past the ceiling must raise VMerror"
    );
}

/// Accumulation, where no single request is large. Only the running total
/// reveals this, so a per-allocation check would miss it entirely.
#[test]
fn accumulated_allocation_is_refused() {
    assert!(
        !run_with_vm_cap("{ 1000000 string pop } loop", 64 * 1024 * 1024),
        "repeated small allocations must eventually hit the ceiling"
    );
}

/// `true setglobal` must not sidestep the ceiling — stet applies it to total
/// PostScript VM for exactly this reason.
#[test]
fn global_vm_does_not_bypass_the_ceiling() {
    assert!(
        !run_with_vm_cap(
            "true setglobal { 1000000 string pop } loop",
            64 * 1024 * 1024
        ),
        "allocating in global VM must still count against the ceiling"
    );
}

/// Ordinary allocation well inside the ceiling must be unaffected.
#[test]
fn allocation_within_the_ceiling_succeeds() {
    assert!(
        run_with_vm_cap("1000000 string pop 100000 array pop", 64 * 1024 * 1024),
        "a 1 MB string and a 100k array are ordinary and must not be refused"
    );
}

/// The shipped default has to leave real work alone. 8 GiB bounds what may be
/// asked of the allocator; steady growth stops around half that.
#[test]
fn default_ceiling_admits_substantial_allocation() {
    let mut ctx = Context::new();
    stet_ops::build_system_dict(&mut ctx);
    ctx.exec_sync_fn = Some(stet_engine::eval::exec_sync);
    assert!(
        ctx.max_local_vm >= 8 * 1024 * 1024 * 1024,
        "default MaxLocalVM should be generous; got {}",
        ctx.max_local_vm
    );
    // 64 MB of strings under the default ceiling.
    assert!(
        stet_engine::eval::parse_and_exec(&mut ctx, b"0 1 63 { pop 1000000 string pop } for")
            .is_ok(),
        "64 MB of allocation must be fine under the default ceiling"
    );
}
