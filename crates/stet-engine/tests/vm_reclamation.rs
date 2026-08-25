// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `restore` must actually reclaim the local VM a save level allocated.
//!
//! PLRM 3.7 gives PostScript two reclamation mechanisms: stack-disciplined
//! `save`/`restore`, and garbage collection (reachability-based, controlled by
//! `vmreclaim`). These tests cover the first. Without reclamation the arena
//! grew for the lifetime of the process, so any program allocating in a loop
//! -- which is most generated PostScript -- grew without bound.
//!
//! The correctness half matters as much as the reclamation half: truncating
//! the stores must not disturb copy-on-write rollback, and it makes `EntityId`s
//! reusable, so a later object must never inherit a dead one's identity.

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

/// Arrays allocated inside a save level are released by the matching restore.
#[test]
fn restore_reclaims_arrays() {
    let mut c = ctx();
    run(&mut c, "/warm [1 2 3] def\n");
    let before = c.arrays.local.allocated_objects();
    let entities_before = c.arrays.local.entities.len();

    run(&mut c, "save [ 0 1 2 3 4 5 6 7 8 9 ] pop restore\n");

    assert_eq!(
        c.arrays.local.allocated_objects(),
        before,
        "array slots allocated under the save should be reclaimed"
    );
    assert_eq!(
        c.arrays.local.entities.len(),
        entities_before,
        "array entities allocated under the save should be reclaimed"
    );
}

/// Same for strings and dictionaries.
#[test]
fn restore_reclaims_strings_and_dicts() {
    let mut c = ctx();
    run(&mut c, "/warm (x) def\n");
    let str_before = c.strings.local.data_len();
    let dict_before = c.dicts.local.dict_slots();

    run(&mut c, "save 4096 string pop 32 dict pop restore\n");

    assert_eq!(
        c.strings.local.data_len(),
        str_before,
        "string bytes allocated under the save should be reclaimed"
    );
    assert_eq!(
        c.dicts.local.dict_slots(),
        dict_before,
        "dict slots allocated under the save should be reclaimed"
    );
}

/// Repeating the allocate/restore cycle must not accumulate.
///
/// This is the shape that made stet grow without bound: transient composites
/// built in a loop, with the arena never handing the space back.
#[test]
fn repeated_save_restore_cycles_do_not_accumulate() {
    let mut c = ctx();
    run(
        &mut c,
        "/cycle { save [ 0 1 2 3 4 5 6 7 8 9 ] pop 64 string pop restore } def\n",
    );

    // Each `parse_and_exec` allocates the `{ cycle }` procedure it is handed,
    // which lives outside any save and legitimately persists. That cost is per
    // *call*, so comparing calls with wildly different iteration counts
    // isolates it from anything the iterations themselves might leak.
    run(&mut c, "100 { cycle } repeat\n");
    let a = c.arrays.local.allocated_objects();
    run(&mut c, "100 { cycle } repeat\n");
    let b = c.arrays.local.allocated_objects();
    run(&mut c, "20000 { cycle } repeat\n");
    let d = c.arrays.local.allocated_objects();

    assert_eq!(
        d - b,
        b - a,
        "20000 cycles must cost no more arena than 100 -- growth should come \
         only from parsing the procedure, not from the iterations ({} vs {})",
        d - b,
        b - a
    );
}

/// Truncation must not disturb copy-on-write rollback.
///
/// `cow_copy` leaves the surviving pre-save data at its original offset (below
/// the save's mark) and parks the discarded mutated copy above it. If that
/// relationship were the other way round, truncating would destroy the very
/// data the restore just reinstated.
#[test]
fn restore_reverts_mutation_of_pre_save_array() {
    let mut c = ctx();
    run(&mut c, "/a [1 2 3] def\n");
    run(&mut c, "save a 0 99 put restore\n");
    run(&mut c, "a 0 get\n");
    assert_eq!(top_int(&mut c), 1, "put under a save must be rolled back");
    run(&mut c, "a 2 get\n");
    assert_eq!(top_int(&mut c), 3, "untouched elements must survive");
}

/// The same, for a string and a dictionary.
#[test]
fn restore_reverts_mutation_of_pre_save_string_and_dict() {
    let mut c = ctx();
    run(&mut c, "/s (abc) def\n/d 4 dict def\nd /k 1 put\n");
    run(&mut c, "save s 0 (Z) 0 get put d /k 42 put restore\n");
    run(&mut c, "s 0 get\n");
    assert_eq!(top_int(&mut c), b'a' as i64, "string put must roll back");
    run(&mut c, "d /k get\n");
    assert_eq!(top_int(&mut c), 1, "dict put must roll back");
}

/// Restoring to an outer level reclaims every level above it, and leaves data
/// from before the outer save intact.
#[test]
fn nested_restore_to_outer_level_reclaims_all_levels() {
    let mut c = ctx();
    run(&mut c, "/a [1 2 3] def\n");
    let before = c.arrays.local.allocated_objects();

    run(
        &mut c,
        "save                 % outer
           [ 0 1 2 3 ] pop
           a 0 50 put
           save               % inner
             [ 4 5 6 7 ] pop
             a 1 60 put
           pop                % discard inner save object
         restore              % restore straight to the outer level
        ",
    );

    assert_eq!(
        c.arrays.local.allocated_objects(),
        before,
        "restoring to the outer level should reclaim both levels"
    );
    run(&mut c, "a 0 get\n");
    assert_eq!(top_int(&mut c), 1, "outer mutation rolled back");
    run(&mut c, "a 1 get\n");
    assert_eq!(top_int(&mut c), 2, "inner mutation rolled back");
}

/// Reclaiming makes `EntityId`s reusable, so a later object must never inherit
/// a dead one's contents or identity.
#[test]
fn entities_allocated_after_restore_do_not_alias_reclaimed_ones() {
    let mut c = ctx();
    run(&mut c, "/keep [7 7 7] def\n");

    run(&mut c, "save /gone [1 2 3] def restore\n");
    // `gone` was defined in userdict under the save, so the definition is gone.
    run(&mut c, "userdict /gone known\n");
    let known = matches!(
        c.o_stack.pop().expect("bool expected").value,
        PsValue::Bool(true)
    );
    assert!(
        !known,
        "a definition made under the save should not survive"
    );

    // A fresh array very likely reuses the reclaimed EntityId. It must be its
    // own object with its own contents.
    run(&mut c, "/fresh [8 8 8] def\nfresh 0 get\n");
    assert_eq!(
        top_int(&mut c),
        8,
        "reused entity must hold the new contents"
    );
    run(&mut c, "keep 0 get\n");
    assert_eq!(top_int(&mut c), 7, "unrelated pre-save object is untouched");
    run(&mut c, "fresh length\n");
    assert_eq!(top_int(&mut c), 3);
}

/// A restore that PLRM forbids must still be refused -- and must not reclaim.
///
/// `check_invalidrestore` is the precondition the truncation relies on, so a
/// failing restore has to leave the VM completely alone.
#[test]
fn invalidrestore_is_refused_and_reclaims_nothing() {
    let mut c = ctx();
    run(&mut c, "/warm [1] def\n");

    // Leave a composite created after the save on the operand stack, then try
    // to restore across it: PLRM 3.7.3.2 makes this invalidrestore.
    let src = "save [ 1 2 3 ] exch restore\n";
    let err = stet_engine::eval::parse_and_exec(&mut c, src.as_bytes());
    assert!(err.is_err(), "restore across a newer composite must fail");

    // The array is still reachable from the operand stack, so it must still be
    // intact and readable.
    run(&mut c, "count 0 gt { } if\n");
}
