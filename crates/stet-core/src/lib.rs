// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Core type system, storage, tokenizer, and runtime context for the stet
//! PostScript interpreter.
//!
//! This crate provides the PostScript VM building blocks: `PsObject` /
//! `PsValue`, the interpreter [`Context`](context::Context) (operand /
//! execution / dictionary stacks, graphics state, VM stores), the arena-
//! backed `StringStore` / `ArrayStore` / `DictStore` with copy-on-write
//! save/restore semantics, the `NameTable` name-interning system, the PS
//! tokenizer, the [`OutputDevice`](device::OutputDevice) trait, file
//! handles, and the error hierarchy.
//!
//! Most users should use the [`stet`](https://crates.io/crates/stet) facade
//! crate rather than depending on `stet-core` directly. Pull this crate in
//! only when you need to build alternative PS-interpreter tooling or
//! implement a custom [`OutputDevice`](device::OutputDevice).
//!
//! See the [Architecture Guide](https://github.com/AndyCappDev/stet/blob/main/docs/ARCHITECTURE.md)
//! for how this crate fits into the broader workspace.

// ── Re-exports from stet-fonts ──────────────────────────────────────────────
// These modules now live in stet-fonts but are re-exported here for backward
// compatibility with existing downstream crates.
pub use stet_fonts::agl;
pub use stet_fonts::cff_parser;
pub use stet_fonts::charstring;
pub use stet_fonts::encoding;
pub use stet_fonts::geometry;
pub use stet_fonts::system_fonts;
pub use stet_fonts::truetype;
pub use stet_fonts::type1_parser;
pub use stet_fonts::type2_charstring;

// ── Re-exports from stet-graphics ───────────────────────────────────────────
pub use stet_graphics::color;
pub use stet_graphics::display_list;
pub use stet_graphics::icc;
pub use stet_graphics::mesh_shading;

// ── Modules that remain in stet-core ────────────────────────────────────────
pub mod array_store;
pub mod binary_token;
pub mod context;
pub mod device;
pub mod dict;
pub mod dual_array_store;
pub mod dual_dict_store;
pub mod dual_string_store;
pub mod entity_table;
pub mod eps;
pub mod error;
pub mod file_store;
pub mod font_loader;
pub mod glyph_cache;
pub mod graphics_state;
pub mod name;
pub mod object;
pub mod pdfmark;
pub mod save_stack;
pub mod stack;
pub mod string_store;
pub mod system_font_loader;
pub mod tokenizer;
