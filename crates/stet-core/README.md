# stet-core

Core type system, storage, tokenizer, and interpreter context for the stet PostScript interpreter.

This is a low-level crate. Most users should use the [`stet`](https://crates.io/crates/stet)
facade crate instead.

## Contents

- **`context`** — `Context`, the central interpreter state (stacks, stores, graphics state)
- **`object`** — `PsObject` and `PsValue`, the PostScript value representation
- **`tokenizer`** — PostScript source tokenizer
- **`device`** — `OutputDevice` trait and `NullDevice`
- **`error`** — `PsError` error type
- **`graphics_state`** — `GraphicsState`, current transformation matrix, color, line style
- **Stores** — `StringStore`, `ArrayStore`, `DictStore` with arena allocation and entity indirection
- **`entity_table`** — Copy-on-write entity indirection for save/restore
- **`file_store`** — File handle management with virtual filesystem support
- **`name`** — Name interning table (`NameId`)
- **`eps`** — EPS header parsing and bounding box extraction
- **`font_loader`** / **`glyph_cache`** — Font loading and glyph caching infrastructure

Also re-exports all modules from `stet-fonts` and `stet-graphics` for backward compatibility.

## Usage

```rust
use stet_core::context::Context;
use stet_core::object::{PsObject, PsValue};
use stet_core::error::PsError;
use stet_core::device::OutputDevice;
```

## License

Apache-2.0 OR MIT
