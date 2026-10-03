// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What PDF output reads fonts from: the interpreter's copies of them.
//!
//! The document is built at the end of the job, when a `restore` may have
//! reclaimed a font or reverted glyphs a page added to it, so PDF output
//! reads the copies the interpreter took as the text was shown
//! ([`stet_core::font_snapshot`]) rather than the VM.

use std::sync::Arc;

use stet_core::context::CidGlyphMetrics;
use stet_core::font_snapshot::{Frozen, FrozenDict, ResolvedFonts};
use stet_core::name::NameTable;
use stet_core::object::NameId;

/// The font copies of a job, with the name table their names index.
pub(crate) struct FontData<'a> {
    /// The interpreter's names. Never reclaimed, so valid at the end of the
    /// job.
    pub names: &'a NameTable,
    /// Every font instance of the job, copied.
    pub fonts: &'a ResolvedFonts,
}

impl<'a> FontData<'a> {
    /// `dict`'s value under the name `key`.
    pub fn get<'d>(&self, dict: &'d FrozenDict, key: &[u8]) -> Option<&'d Frozen> {
        dict.get_name(self.names.find(key)?)
    }

    /// The bytes of the name `id`.
    pub fn name(&self, id: NameId) -> &'a [u8] {
        self.names.get_bytes(id)
    }

    /// The id of the name `name`, if it exists.
    pub fn find(&self, name: &[u8]) -> Option<NameId> {
        self.names.find(name)
    }

    /// The metrics a show gave `cid` of `cidfont` through its `Metrics2` or
    /// `CDevProc`.
    pub fn cid_metrics(&self, cidfont: &Arc<FrozenDict>, cid: u16) -> Option<CidGlyphMetrics> {
        self.fonts.cid_metrics(cidfont, u32::from(cid))
    }
}

/// The glyph name at `code` of an encoding array, if it names one.
pub(crate) fn glyph_at(encoding: &[Frozen], code: u16) -> Option<NameId> {
    encoding.get(usize::from(code)).and_then(Frozen::as_name)
}

/// The bytes of each string in `array`; anything else gives no bytes.
pub(crate) fn strings(array: &[Frozen]) -> Vec<Vec<u8>> {
    array
        .iter()
        .map(|o| o.as_string().map(<[u8]>::to_vec).unwrap_or_default())
        .collect()
}
