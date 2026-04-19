# stet-engine

[![crates.io](https://img.shields.io/crates/v/stet-engine.svg)](https://crates.io/crates/stet-engine)
[![docs.rs](https://img.shields.io/docsrs/stet-engine)](https://docs.rs/stet-engine)

Execution engine for the stet PostScript interpreter.

This is a low-level crate. Most users should use the [`stet`](https://crates.io/crates/stet)
facade crate instead.

## Contents

- **`eval`** — The core eval loop that drives the interpreter
  - `parse_and_exec(ctx, source)` — Parse and execute PostScript from a byte slice
  - `parse_and_exec_file(ctx, source, path)` — Same, with canonical file path for relative resolution
  - `exec_sync(ctx, proc_obj)` — Synchronous PS procedure execution (used by operators like `if`, `for`, etc.)

The eval loop processes the execution stack one object at a time:
executable names are looked up in the dictionary stack, operators are
dispatched, and procedures (`{...}`) are stepped through element by element.

## Usage

```rust
use stet_engine::eval::{parse_and_exec, exec_sync};

// After Context is initialized with operators and init scripts:
parse_and_exec(&mut ctx, b"1 2 add =").unwrap();
```

## License

Apache-2.0 OR MIT
