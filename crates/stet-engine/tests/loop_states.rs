// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The state of a loop that has ended is dropped.
//!
//! ImageMagick writes an image as `{ DirectClassPacket } false 3 colorimage`,
//! a procedure called once per pixel that runs a `for` each time: 31 million
//! loops for an A3 page at 400 dpi. Every loop state used to be kept until the
//! job ended (some 300 bytes each), so such a page ran out of memory.

use stet_core::context::Context;
use stet_core::object::PsValue;

fn ctx() -> Context {
    let mut ctx = Context::new();
    stet_ops::build_system_dict(&mut ctx);
    ctx.exec_sync_fn = Some(stet_engine::eval::exec_sync);
    ctx
}

fn run(ctx: &mut Context, src: &str) {
    stet_engine::eval::parse_and_exec(ctx, src.as_bytes())
        .unwrap_or_else(|e| panic!("PS execution failed: {e:?}\nsource:\n{src}"));
}

fn top_int(ctx: &mut Context) -> i64 {
    match ctx.o_stack.pop().expect("operand expected").value {
        PsValue::Int(v) => v,
        other => panic!("expected an integer, got {other:?}"),
    }
}

/// A loop run inside another loop many times leaves no states behind.
#[test]
fn ended_loops_are_dropped() {
    let mut c = ctx();
    run(
        &mut c,
        "/n 0 def 1 1 100000 { pop 0 1 2 { pop /n n 1 add def } for } for n\n",
    );
    assert_eq!(top_int(&mut c), 300_000);
    assert!(c.loops.len() <= 2048, "{} loop states kept", c.loops.len());
}

/// Dropping ended states while loops are running keeps the running ones
/// intact: every level counts what it should, with exits and `stopped` in
/// between.
#[test]
fn running_loops_survive_a_drop() {
    let mut c = ctx();
    run(
        &mut c,
        "/a 0 def /b 0 def /c 0 def /d 0 def
         [ 1 2 3 ] {
           pop /a a 1 add def
           1 1 3000 {
             pop /b b 1 add def
             0 { 1 add dup 2 eq { exit } if } loop pop
             { 0 1 1 { pop /c c 1 add def } for stop } stopped pop
             2 { /d d 1 add def } repeat
           } for
         } forall
         a b c d\n",
    );
    assert_eq!(top_int(&mut c), 18_000);
    assert_eq!(top_int(&mut c), 18_000);
    assert_eq!(top_int(&mut c), 9_000);
    assert_eq!(top_int(&mut c), 3);
    assert!(c.loops.len() <= 2048, "{} loop states kept", c.loops.len());
}
