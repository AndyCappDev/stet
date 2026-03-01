# CLAUDE.md

This file provides guidance to Claude Code when working with the stet codebase.

## Project Overview

**stet** is a PostScript Level 3 interpreter written in Rust, targeting production-grade performance. It is a ground-up reimplementation — NOT a port — using two sister projects as references:

- **PostForge** (`~/Projects/postforge`) — A complete PostScript Level 3 interpreter in Python (345 operators, 49 test suites, 5 output devices). This is the **reference implementation and test oracle**. When questions arise about PostScript operator behavior, consult PostForge's implementation first.
- **xpost** (`~/Projects/xpost`) — A PostScript Level 1-2 interpreter in C. This provides **architectural inspiration**, particularly the arena/entity indirection memory model and dual VM design.

## Project Plans

- **`docs/ROADMAP.md`** — Full 8-phase roadmap from empty project to Level 3 compliance
- **`docs/PHASE1-PLAN.md`** — Detailed Phase 1 implementation plan with Rust APIs, type system design, operator specifications, and implementation order

**Always consult these plan files before making architectural decisions.** They contain carefully researched designs based on deep analysis of both PostForge and xpost.

## PostScript Reference Manuals

The authoritative source for PostScript specifications lives in the PostForge project:
- **PRIMARY**: `~/Projects/postforge/docs/reference/PostScript Language Reference Manual Second Edition.pdf` (more thorough descriptions)
- **SECONDARY**: `~/Projects/postforge/docs/reference/PostScript Language Reference Manual Third Edition.txt` (updates for Level 3)

Always consult these manuals for operator behavior, error conditions, and language semantics.

## Architecture

stet uses a **Cargo workspace** with four crates:

- **`stet-core`** — Type system, arena allocator, VM, tokenizer, context, errors
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

## Development Conventions

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
// SPDX-License-Identifier: AGPL-3.0-or-later
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

1. **Check PHASE1-PLAN.md** for the operator specification (stack signature, error conditions, edge cases)
2. **Check PostForge's implementation** in `~/Projects/postforge/postforge/operators/` for the Python reference
3. **Check the PLRM** for authoritative specification
4. **Check PostForge's tests** in `~/Projects/postforge/unit_tests/` for expected behavior

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
