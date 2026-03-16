// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Graphics types, display list, and ICC color support for the stet PostScript interpreter.
//!
//! This crate provides:
//! - Device color types and CIE color space parameters (`color`)
//! - Rendering parameter structs and output traits (`device`)
//! - Display list for deferred drawing operations (`display_list`)
//! - ICC color profile management (`icc`)
//! - Mesh shading binary parsers (`mesh_shading`)

pub mod color;
pub mod device;
pub mod display_list;
pub mod icc;
pub mod mesh_shading;
