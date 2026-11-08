// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Whether stet may touch the host computer.
//!
//! By default stet behaves like a desktop interpreter: PostScript `file`, `run`, `deletefile` and
//! `renamefile` reach the real file system, `%stdin`/`%stdout`/`%stderr` are the process's, and fonts,
//! CMaps and ICC profiles are looked up in the system folders.
//!
//! A program that renders untrusted documents calls [`deny_host_access`] once, before the first document.
//! From then on, in this process:
//!
//! * `file` opens only embedded resources and in-memory sources; any other name is `invalidfileaccess`;
//! * `run`, `deletefile`, `renamefile` refuse; `status` and `filenameforall` see only embedded resources;
//! * `%stdin` reads as empty, `%stdout`/`%stderr` swallow what is written;
//! * system fonts, CMaps and ICC profiles are not looked up: only the bundled ones are used.
//!
//! The switch is one-way: once denied, access cannot be granted again in the same process.
use std::sync::atomic::{AtomicBool, Ordering};

static DENIED: AtomicBool = AtomicBool::new(false);

/// Deny every host access for the rest of the process (see the module documentation).
pub fn deny_host_access() {
    DENIED.store(true, Ordering::SeqCst);
}

/// True unless [`deny_host_access`] was called.
pub fn host_access_allowed() -> bool {
    !DENIED.load(Ordering::SeqCst)
}

fn denied(path: &std::path::Path) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        format!("host access denied: {}", path.display()),
    )
}

/// `std::fs::read`, refused when host access is denied.
pub fn read(path: impl AsRef<std::path::Path>) -> std::io::Result<Vec<u8>> {
    let path = path.as_ref();
    if !host_access_allowed() {
        return Err(denied(path));
    }
    std::fs::read(path)
}
