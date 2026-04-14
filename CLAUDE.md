# CLAUDE.md

This file provides guidance to Claude Code when working with the stet codebase.

## Project Overview

**stet** is a PostScript Level 3 interpreter written in Rust, targeting production-grade performance. It is a ground-up reimplementation — NOT a port — using two sister projects as references:

- **PostForge** (`~/Projects/postforge`) — A complete PostScript Level 3 interpreter in Python (345 operators, 49 test suites, 5 output devices). This is the **reference implementation and test oracle**. When questions arise about PostScript operator behavior, consult PostForge's implementation first.
- **xpost** (`~/Projects/xpost`) — A PostScript Level 1-2 interpreter in C. This provides **architectural inspiration**, particularly the arena/entity indirection memory model and dual VM design.

## Project Documentation

- **`docs/ARCHITECTURE.md`** — Crate architecture, pipelines, OutputDevice trait, rendering stages
- **`docs/DISPLAY-LIST.md`** — Display list element reference with all field types and code examples
- **`docs/VIEWER-GUIDE.md`** — Viewer keyboard/mouse controls, DPI, minimap

## PostScript Reference Manuals

The authoritative source for PostScript specifications lives in the PostForge project:
- **PRIMARY**: `~/Projects/postforge/docs/reference/PostScript Language Reference Manual Second Edition.pdf` (more thorough descriptions)
- **SECONDARY**: `~/Projects/postforge/docs/reference/PostScript Language Reference Manual Third Edition.txt` (updates for Level 3)

Always consult these manuals for operator behavior, error conditions, and language semantics.

## Architecture

stet uses a **Cargo workspace** with four crates:

- **`stet-fonts`** — Font parsing and geometry types (Matrix, PsPath, Type 1/CFF/TrueType parsers, encoding tables, AGL)
- **`stet-graphics`** — Graphics types, display list, ICC color, mesh shading (DeviceColor, FillParams, DisplayList, IccCache)
- **`stet-core`** — PS interpreter infrastructure: type system, arena allocator, VM, tokenizer, context, errors. Depends on and re-exports stet-fonts/stet-graphics
- **`stet-ops`** — All PostScript operator implementations
- **`stet-engine`** — Execution engine (the core eval loop)
- **`stet-cli`** — Binary entry point (file input and interactive REPL)

### Key Design Decisions

- **Rust enum for PostScript objects** — `PsValue` enum with variants for each PS type. `PsObject` is `Clone + Copy` (value type with arena indices, not heap references).
- **Arena + entity indirection** — Composite objects (strings, arrays, dicts) store data in centralized stores (`StringStore`, `ArrayStore`, `DictStore`), referenced by `EntityId` indices. This enables future save/restore via entity swapping.
- **Name interning** — `NameTable` maps byte sequences to `NameId` values. Names persist forever (not subject to save/restore or GC).
- **PostScript integers are i32** — Per PLRM spec: -2,147,483,648 to 2,147,483,647. Overflow promotes to `Real(f64)`.
- **Validate before popping** — All operators must validate stack depth, types, access, and ranges BEFORE popping any operands. This matches PostForge's pattern and PLRM requirements.

## Development Commands

```bash
cargo build                    # Build all crates
cargo test                     # Run all tests
cargo run -- file.ps           # Execute a PostScript file
cargo run                      # Interactive REPL
cargo clippy                   # Lint
cargo fmt                      # Format
```

### Rendering PDFs for Debugging

When rendering PDF pages for visual comparison, always use `--device png` and `--pages` to limit output:

```bash
cargo run -- --device png --pages 1 file.pdf       # Render page 1 only
cargo run -- --device png --pages 1-3 file.pdf     # Render pages 1-3
```

Output goes to `out_page1.png`, `out_page2.png`, etc. in the current directory.

## Fix Philosophy

Always implement the **proper, long-term fix** for issues. Never settle for a "quick hack" or "cleanest shortcut" when a correct solution exists. The proper fix may also be the simplest — that's fine — but correctness and durability are the priority, not expediency. Don't describe fixes as "clean" or "simple"; just implement them correctly.

## Development Conventions

### Documentation Maintenance

When modifying `DisplayElement`, `DisplayList`, or any of the param structs in `stet-graphics/src/device.rs` (e.g., `FillParams`, `StrokeParams`, `ImageParams`, `ImageColorSpace`, `ShadingColorSpace`, `SpotColorSpace`), **you must update `docs/DISPLAY-LIST.md`** to reflect the change. This includes adding, removing, or renaming element variants, fields, or color space types.

Also keep the debug helpers in sync:
- `debug_bbox_lines` in `crates/stet-render/src/skia_device.rs` — pattern-matches every `DisplayElement` variant to label kinds and recurse into container variants (`Group`, `SoftMasked`, `OcgGroup`). If you add, remove, or rename a variant, update this function.
- `crates/stet-cli/examples/dump_bboxes.rs` — debug binary that invokes `debug_bbox_comparison`. No changes typically needed, but verify it still builds after variant changes.
- The bbox computations `precompute_bboxes` (Y-only, banded) and `precompute_full_bboxes` (2D, viewport) in the same file must both handle every variant consistently — divergence between the two produced the `1915_1.pdf` viewport-pipeline bug.

### Code Style

- Use `cargo fmt` (rustfmt) for formatting
- Use `cargo clippy` and fix all warnings
- All public types and functions must have doc comments
- Operator functions follow this exact pattern:
  ```rust
  pub fn op_name(ctx: &mut Context) -> Result<(), PsError> {
      // 1. Validate stack depth
      // 2. Validate types (peek, don't pop)
      // 3. Validate access/ranges
      // 4. ONLY NOW pop operands
      // 5. Execute
      // 6. Push result
  }
  ```

### Copyright Headers

All new Rust source files must include:

```rust
// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT
```

### Git Configuration

- **User email**: scott@bowmans.org
- **User name**: Scott Bowman
- **Branch strategy**: `main` + feature branches
- **Remote**: NAS (primary) — `ssh://scott@nas/volume1/git/stet`

### Git Commit Rules

- **CRITICAL — No Claude Attribution**: NEVER include "Co-Authored-By: Claude" or any reference to Claude/Anthropic in commit messages or any git metadata.
- Always check `git status` and `git diff` before committing
- Ask the user about including unrelated modified files
- Write concise commit messages focused on the "why"

## Testing

### Rust Tests

- Unit tests in each module (`#[cfg(test)] mod tests`)
- Integration tests in `tests/integration/`
- Each operator needs tests for: happy path, each PLRM error condition, type edge cases

### PostScript Test Suites

PostForge's test suites (`~/Projects/postforge/unit_tests/*.ps`) serve as integration tests. These use a custom `assert` framework defined in `unittest.ps`. Port and run these as stet matures.

### Test Execution

```bash
cargo test                              # All Rust tests
cargo test --package stet-ops         # Operator tests only
cargo test --package stet-core        # Core type/tokenizer tests only
cargo run -- tests/ps/some_test.ps      # Run a PostScript test file
```

## Reference Lookup Patterns

When implementing a PostScript operator:

1. **Check PostForge's implementation** in `~/Projects/postforge/postforge/operators/` for the Python reference
2. **Check the PLRM** for authoritative specification
3. **Check PostForge's tests** in `~/Projects/postforge/unit_tests/` for expected behavior

## Reusable Assets from PostForge

These PostScript resource files will be copied into stet's `resources/` directory when needed (Phase 6+):

| Asset | Source Path |
|-------|-------------|
| Init scripts | `~/Projects/postforge/resources/Init/*.ps` |
| Type 1 fonts | `~/Projects/postforge/resources/Font/*.t1` |
| Encodings | `~/Projects/postforge/resources/Encoding/*.ps` |
| CID fonts/CMaps | `~/Projects/postforge/resources/CIDFont/`, `resources/CMap/` |
| Test suites | `~/Projects/postforge/unit_tests/*.ps` |
| Sample files | `~/Projects/postforge/samples/*.ps` |
