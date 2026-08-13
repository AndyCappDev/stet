// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PLRM 3.7.2 invariant: nothing in global VM may reference a *reclaimable*
//! object in local VM.
//!
//! The operator-level half of the rule (`put`, `def`, `store`, `astore`,
//! `putinterval`, `copy`, `[`…`]`, `<<`…`>>` raising `invalidaccess`) is
//! covered by `unit_tests/vm_global_local_tests.ps`. These tests cover the
//! half that PostScript cannot reach: stores the interpreter's own Rust code
//! makes directly into dictionaries and arrays, which pass no operator check.
//!
//! This is the invariant that gates reclaiming local VM on `restore`. While
//! nothing is ever freed a stale global→local reference is inert; once
//! `restore` releases local VM it becomes a use-after-free.

use stet_core::vm_audit::{audit_global_vm, audit_global_vm_unsafe_only};

/// Build an interpreter and run `source` through it, ignoring PostScript-level
/// errors — a program that fails partway can still have left global VM dirty,
/// and that is exactly what we want to inspect.
fn run(source: &[u8]) -> stet::Interpreter {
    let mut interp = stet::Interpreter::builder().suppress_output().build();
    let _ = stet_engine::eval::parse_and_exec(interp.context(), source);
    interp
}

#[test]
fn bootstrap_leaves_no_reclaimable_global_local_refs() {
    let mut interp = stet::Interpreter::builder().suppress_output().build();
    let ctx = interp.context();

    // The interpreter does hold global→local references after bootstrap: PLRM
    // 3.7.5 explicitly sanctions them for `systemdict`, whose entries include
    // the local `userdict`, `errordict`, `$error`, `statusdict`, and
    // `FontDirectory`. They are safe because they predate every `save`.
    assert!(
        !audit_global_vm(ctx).is_empty(),
        "expected the PLRM 3.7.5 systemdict exception to be present"
    );
    let unsafe_refs = audit_global_vm_unsafe_only(ctx);
    assert!(
        unsafe_refs.is_empty(),
        "bootstrap left reclaimable global->local references: {:#?}",
        unsafe_refs
    );
}

/// `setpagedevice` promotes the page device dictionary into global VM, so
/// every value it writes has to be allocated there too. `MediaSize` used to be
/// allocated per the ambient `currentglobal` — local — which left the global
/// page device pointing into storage the next `restore` would release.
#[test]
fn setpagedevice_keeps_the_page_device_in_global_vm() {
    let interp = run(b"<< /PageSize [612 792] >> setpagedevice\n");
    let mut interp = interp;
    let unsafe_refs = audit_global_vm_unsafe_only(interp.context());
    assert!(
        unsafe_refs.is_empty(),
        "setpagedevice left reclaimable global->local references: {:#?}",
        unsafe_refs
    );
}

/// The reproducer that identified the problem: a `setpagedevice` inside a
/// save bracket, then another after the `restore`. With `MediaSize` allocated
/// locally the second call read an entity the restore had released.
#[test]
fn setpagedevice_across_save_restore_is_clean() {
    let mut interp = run(b"/s save def\n\
         << /PageSize [612 792] >> setpagedevice\n\
         s restore\n\
         << /PageSize [595 842] >> setpagedevice\n");
    let unsafe_refs = audit_global_vm_unsafe_only(interp.context());
    assert!(
        unsafe_refs.is_empty(),
        "setpagedevice across save/restore left reclaimable refs: {:#?}",
        unsafe_refs
    );
}

/// Repeating the bracket must not accumulate references either — a per-call
/// leak would only show up over several iterations.
#[test]
fn repeated_save_restore_page_device_cycles_stay_clean() {
    let mut interp = run(b"20 { /s save def \
         << /PageSize [612 792] >> setpagedevice \
         s restore } repeat\n");
    let unsafe_refs = audit_global_vm_unsafe_only(interp.context());
    assert!(
        unsafe_refs.is_empty(),
        "repeated save/restore cycles left reclaimable refs: {:#?}",
        unsafe_refs
    );
}

/// Operator-level enforcement must survive an error caught by `stopped`.
/// `.error` switches to local VM to build its stack snapshots; if it does not
/// put the caller's allocation mode back, later global-mode allocations
/// silently land in local VM and the enforcement checks stop firing.
#[test]
fn error_handling_restores_vm_allocation_mode() {
    let mut interp = run(b"/lstr (local) def\n\
         true setglobal\n\
         { [lstr] pop } stopped pop\n\
         /after currentglobal def\n\
         false setglobal\n");
    let ctx = interp.context();

    let name = ctx
        .names
        .find(b"after")
        .expect("`after` should be interned");
    let value = ctx
        .dict_load(&stet_core::dict::DictKey::Name(name))
        .expect("`after` should be defined");
    assert!(
        matches!(value.value, stet_core::object::PsValue::Bool(true)),
        "an error caught by `stopped` reset the VM allocation mode: {:?}",
        value.value
    );
}
