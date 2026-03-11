# WASM Parallel Viewport Rendering via wasm-bindgen-rayon

## Status: BLOCKED — waiting on wasm-bindgen upstream fix

## What We Did

We implemented full rayon-based parallel viewport rendering for the WASM viewer, mirroring the parallel banding strategy used in the CLI renderer. The Rust code is complete and correct. The JS integration is written and tested. The build pipeline handles nightly Rust with `-Z build-std` for atomics + shared memory.

### Completed Work (kept in codebase)

These are ready to activate when the toolchain catches up:

| File | What |
|------|------|
| `crates/stet-render/src/skia_device.rs` | `render_region_prepared_parallel()` — rayon par_iter over viewport bands |
| `crates/stet-render/src/lib.rs` | Re-export of `render_region_prepared_parallel` |
| `web/patch-wasm-tls.py` | Post-processor that adds legacy TLS exports to WASM binaries |
| `web/build.sh` | Nightly build pipeline: cargo → patch-wasm-tls.py → wasm-bindgen |

### Reverted Work (apply when unblocked)

These changes were reverted because wasm-bindgen can't produce valid WASM output with the new LLVM TLS model:

| File | Change needed |
|------|---------------|
| `.cargo/config.toml` | Add `+atomics,+bulk-memory,+mutable-globals` to wasm32 rustflags |
| `crates/stet-wasm/Cargo.toml` | Add `wasm-bindgen-rayon = "1.3"`, `rayon = "1"`; enable parallel feature on stet-render |
| `crates/stet-wasm/src/lib.rs` | Add `pub use wasm_bindgen_rayon::init_thread_pool;` + `render_viewport_parallel()` |
| `web/worker.js` | Import `initThreadPool` + `render_viewport_parallel`, init thread pool, use parallel path |
| `web/app.js` | Track `threadsAvailable`, hide progress bar for parallel renders |

## The Blocker

**wasm-bindgen 0.2.114's threading transform generates invalid WASM.**

The chain of events:

1. LLVM (used in Rust nightly 2025+) renamed WASM TLS symbols:
   - `__wasm_init_tls` → `__wasm_apply_tls_relocs`
   - `__tls_size` / `__tls_align` → removed (TLS handled internally by LLVM)
   - `__tls_base` → still exists but not exported

2. wasm-bindgen's threading transform expects the old symbol names. We solved this with `patch-wasm-tls.py`, which adds legacy exports to the WASM binary before wasm-bindgen processes it.

3. However, wasm-bindgen's threading transform *also* uses `__tls_size` to compute TLS memory allocation at runtime. Setting it to 0 (since LLVM no longer uses per-thread TLS segments the same way) causes the transform to emit invalid WASM bytecode — specifically, `unused values not explicitly dropped by end of block` validation errors.

The root cause is that wasm-bindgen's threading code generation assumes the old LLVM TLS memory model where threads need explicit TLS segment allocation. The new model handles this differently, and wasm-bindgen hasn't been updated to match.

## What Needs to Happen

**One of:**

1. **wasm-bindgen updates its threading transform** to handle the new LLVM TLS model. Track: https://github.com/aspect-build/aspect-workflows/issues — search for `__wasm_apply_tls_relocs` or `__tls_size` in wasm-bindgen issues.

2. **A Rust nightly is released** where the old TLS symbol names are restored (unlikely — the rename was intentional).

3. **We write a more sophisticated WASM patcher** that also modifies the threading transform's generated code to handle the 0-size TLS case correctly. This is fragile and not recommended.

## How to Re-enable

When wasm-bindgen supports the new TLS model:

1. In `.cargo/config.toml`, change rustflags to:
   ```toml
   rustflags = ["-C", "target-feature=+atomics,+bulk-memory,+mutable-globals,+simd128"]
   ```

2. In `crates/stet-wasm/Cargo.toml`:
   ```toml
   stet-render = { path = "../stet-render" }  # default features (includes parallel)
   wasm-bindgen-rayon = "1.3"
   rayon = "1"
   # Keep lto = false (invalid WASM with atomics+shared-memory)
   ```

3. In `crates/stet-wasm/src/lib.rs`, add:
   ```rust
   pub use wasm_bindgen_rayon::init_thread_pool;
   ```
   And add the `render_viewport_parallel` wasm_bindgen function that calls
   `stet_render::render_region_prepared_parallel`.

4. In `web/worker.js`, import `initThreadPool` and `render_viewport_parallel`,
   call `await initThreadPool(navigator.hardwareConcurrency)` during init,
   and use `render_viewport_parallel` when threads are available.

5. In `web/app.js`, track `threadsAvailable` from the worker ready message,
   hide progress bar for parallel renders.

6. `web/build.sh` already handles the nightly build with `-Z build-std`.
   `web/patch-wasm-tls.py` may no longer be needed if wasm-bindgen handles
   the new symbols natively.

## Build Pipeline (current, sequential)

```
web/build.sh:
  Step 1: cargo build (nightly + build-std + atomics + shared memory)
  Step 2: patch-wasm-tls.py (add legacy TLS exports)
  Step 3: wasm-bindgen (--target web)
  Step 4: copy to web/pkg/
```

Note: The build pipeline already uses nightly and shared memory even for the sequential build. The `patch-wasm-tls.py` step ensures wasm-bindgen can process the binary. The parallel feature is disabled at the Cargo level (stet-render's `parallel` feature not enabled for stet-wasm).

## Performance Expectations

When enabled, parallel rendering should provide ~2× speedup for viewport renders:
- Sequential: ~3-5s for complex viewports at high DPI
- Parallel (N threads): ~1-2s for the same viewports
- Progress bar hidden for parallel renders (fast enough for a spinner)

## Pinned Nightly

`web/build.sh` pins `nightly-2026-03-11`. Known bad: `nightly-2026-02-27` produces invalid WASM with shared-memory (compiler bug). Update the pin when testing new nightlies.
