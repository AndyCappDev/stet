// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Attribute a render's peak heap to the call sites holding it.
//!
//! Wraps the system allocator, records a backtrace for every allocation at or
//! above `STET_PROFILE_MIN` bytes (default 1 MiB), and snapshots the live set
//! when it passes the running peak by more than `STET_PROFILE_JUMP` bytes
//! (default 256 MiB). Small allocations are counted but not attributed, so the
//! snapshot accounts for the bulk without paying a backtrace on every `Vec`
//! push.
//!
//! **If it reports an empty snapshot against a large peak, lower
//! `STET_PROFILE_JUMP`.** The default margin is tuned for a peak dominated by
//! one huge allocation; a peak accumulated out of many mid-sized ones never
//! clears it, and the tool then says "0 allocations" where the honest answer
//! is "the peak did not arrive in one step". That reads as "nothing to see"
//! and is how an investigation gets abandoned early. Attributing `4245.pdf`
//! at 24 threads needed `STET_PROFILE_JUMP=16777216`.
//!
//! Written to investigate `pdf_samples/5447.pdf`, whose rasterization costs
//! ~140x the size of the canvas it produces.
//!
//! Usage:
//!   cargo run --release -p stet-cli --example profile_alloc -- FILE [PAGE] [DPI]
//!
//! Release mode needs frame pointers for a usable backtrace:
//!   RUSTFLAGS="-C force-frame-pointers=yes" cargo run --release ...

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static ARMED: AtomicBool = AtomicBool::new(false);

/// Allocations at or above this size get a backtrace.
static MIN_TRACKED: AtomicUsize = AtomicUsize::new(1 << 20);

/// How far the live set must exceed the running peak before a snapshot is
/// worth the walk. A single dominant allocation clears this easily; a peak
/// built out of many mid-sized ones never does, and reports an empty snapshot
/// against a large peak. Lower it via `STET_PROFILE_JUMP` when that happens.
static SNAPSHOT_JUMP: AtomicUsize = AtomicUsize::new(256 << 20);

/// Live tracked allocations: pointer -> (size, backtrace).
static TRACKED: Mutex<Option<HashMap<usize, (usize, String)>>> = Mutex::new(None);

/// The live set as it stood at the high-water mark.
static SNAPSHOT: Mutex<Option<Vec<(usize, String)>>> = Mutex::new(None);

thread_local! {
    /// Set while we are inside our own bookkeeping, so the allocations that
    /// bookkeeping performs do not recurse back into it.
    static IN_HOOK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

struct Tracking;

unsafe impl GlobalAlloc for Tracking {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            note_alloc(ptr as usize, layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        note_dealloc(ptr as usize, layout.size());
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new = unsafe { System.realloc(ptr, layout, new_size) };
        if !new.is_null() {
            note_dealloc(ptr as usize, layout.size());
            note_alloc(new as usize, new_size);
        }
        new
    }
}

fn note_alloc(ptr: usize, size: usize) {
    let live = LIVE.fetch_add(size, Ordering::Relaxed) + size;
    if !ARMED.load(Ordering::Relaxed) {
        return;
    }
    if size < MIN_TRACKED.load(Ordering::Relaxed) {
        maybe_snapshot(live);
        return;
    }
    IN_HOOK.with(|g| {
        if g.get() {
            return;
        }
        g.set(true);
        let bt = format!("{}", std::backtrace::Backtrace::force_capture());
        if let Ok(mut guard) = TRACKED.lock()
            && let Some(map) = guard.as_mut()
        {
            map.insert(ptr, (size, condense(&bt)));
        }
        g.set(false);
    });
    maybe_snapshot(live);
}

fn note_dealloc(ptr: usize, size: usize) {
    LIVE.fetch_sub(size, Ordering::Relaxed);
    if !ARMED.load(Ordering::Relaxed) || size < MIN_TRACKED.load(Ordering::Relaxed) {
        return;
    }
    IN_HOOK.with(|g| {
        if g.get() {
            return;
        }
        g.set(true);
        if let Ok(mut guard) = TRACKED.lock()
            && let Some(map) = guard.as_mut()
        {
            map.remove(&ptr);
        }
        g.set(false);
    });
}

/// Record the live set when it passes the previous maximum by a wide enough
/// margin to be worth the walk.
fn maybe_snapshot(live: usize) {
    let peak = PEAK.load(Ordering::Relaxed);
    if live <= peak.saturating_add(SNAPSHOT_JUMP.load(Ordering::Relaxed)) {
        if live > peak {
            PEAK.store(live, Ordering::Relaxed);
        }
        return;
    }
    PEAK.store(live, Ordering::Relaxed);
    IN_HOOK.with(|g| {
        if g.get() {
            return;
        }
        g.set(true);
        if let Ok(guard) = TRACKED.lock()
            && let Some(map) = guard.as_ref()
        {
            let snap: Vec<(usize, String)> = map.values().map(|(s, b)| (*s, b.clone())).collect();
            if let Ok(mut out) = SNAPSHOT.lock() {
                *out = Some(snap);
            }
        }
        g.set(false);
    });
}

/// Reduce a backtrace to the stet frames that identify the call site.
fn condense(bt: &str) -> String {
    let mut frames: Vec<String> = Vec::new();
    for line in bt.lines() {
        let t = line.trim();
        if !t.starts_with("at ") && t.contains("stet") {
            let name = t
                .split_once(": ")
                .map(|(_, r)| r)
                .unwrap_or(t)
                .split("::{{")
                .next()
                .unwrap_or(t)
                .to_string();
            if !name.contains("profile_alloc") && !frames.contains(&name) {
                frames.push(name);
            }
        }
        if frames.len() >= 4 {
            break;
        }
    }
    if frames.is_empty() {
        "<no stet frames>".into()
    } else {
        frames.join(" <- ")
    }
}

#[global_allocator]
static ALLOC: Tracking = Tracking;

fn mb(bytes: usize) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: profile_alloc FILE [PAGE] [DPI]");
    let page: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let dpi: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(300.0);
    if let Ok(v) = std::env::var("STET_PROFILE_MIN")
        && let Ok(n) = v.parse()
    {
        MIN_TRACKED.store(n, Ordering::Relaxed);
    }
    if let Ok(v) = std::env::var("STET_PROFILE_JUMP")
        && let Ok(n) = v.parse()
    {
        SNAPSHOT_JUMP.store(n, Ordering::Relaxed);
    }

    let data = std::fs::read(&path).expect("read input");
    let doc = stet_pdf_reader::PdfDocument::from_bytes(&data).expect("parse");
    let list = doc.render_page(page, dpi).expect("display list");
    let (page_w, page_h) = doc.page_size(page).expect("page size");
    let scale = dpi / 72.0;
    let w = (page_w * scale).round().max(1.0) as u32;
    let h = (page_h * scale).round().max(1.0) as u32;

    // Only arm for the rasterize step — that is where the memory goes.
    *TRACKED.lock().unwrap() = Some(HashMap::new());
    PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
    ARMED.store(true, Ordering::Relaxed);

    let rgba = stet_render::render_to_rgba(&list, w, h, dpi, Some(doc.icc_cache()), false);

    ARMED.store(false, Ordering::Relaxed);
    std::hint::black_box(&rgba);

    println!(
        "\ncanvas {w}x{h}, peak live heap {}",
        mb(PEAK.load(Ordering::Relaxed))
    );
    println!(
        "tracked allocations >= {}\n",
        mb(MIN_TRACKED.load(Ordering::Relaxed))
    );

    let snap = SNAPSHOT.lock().unwrap().take().unwrap_or_default();
    let mut by_site: HashMap<String, (usize, usize)> = HashMap::new();
    for (size, site) in &snap {
        let e = by_site.entry(site.clone()).or_insert((0, 0));
        e.0 += size;
        e.1 += 1;
    }
    let mut rows: Vec<_> = by_site.into_iter().collect();
    rows.sort_by_key(|(_, (bytes, _))| std::cmp::Reverse(*bytes));

    let total: usize = snap.iter().map(|(s, _)| s).sum();
    println!(
        "live tracked bytes at peak: {} in {} allocations\n",
        mb(total),
        snap.len()
    );
    for (site, (bytes, count)) in rows.iter().take(20) {
        println!("{:>12}  x{:<6} {}", mb(*bytes), count, site);
    }
}
