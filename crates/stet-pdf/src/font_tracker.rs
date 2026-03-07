// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Font usage tracking for PDF text output.

use std::collections::{HashMap, HashSet};

use stet_core::device::TextParams;
use stet_core::object::EntityId;

/// Standard 14 PDF font names that don't require embedding.
const STANDARD_14: &[&[u8]] = &[
    b"Times-Roman",
    b"Times-Bold",
    b"Times-Italic",
    b"Times-BoldItalic",
    b"Helvetica",
    b"Helvetica-Bold",
    b"Helvetica-Oblique",
    b"Helvetica-BoldOblique",
    b"Courier",
    b"Courier-Bold",
    b"Courier-Oblique",
    b"Courier-BoldOblique",
    b"Symbol",
    b"ZapfDingbats",
];

/// Usage info for a single font.
#[allow(dead_code)]
pub struct FontUsage {
    /// PDF resource name (e.g., "F0", "F1").
    pub pdf_name: String,
    /// Font name bytes from the font dict.
    pub font_name: Vec<u8>,
    /// FontType.
    pub font_type: i32,
    /// Font dict entity ID.
    pub font_entity: EntityId,
    /// Set of character codes (or CIDs) used.
    pub used_codes: HashSet<u16>,
    /// Whether this is a Standard 14 font (skip embedding).
    pub is_standard_14: bool,
    /// First TextParams seen (used for font_size, ctm, font_matrix).
    pub sample_params: TextParams,
}

/// Tracks font usage across all pages in a PDF job.
pub struct FontTracker {
    /// Map from font entity → font usage.
    fonts: HashMap<EntityId, FontUsage>,
    /// Next font index for naming.
    next_idx: usize,
}

impl FontTracker {
    pub fn new() -> Self {
        Self {
            fonts: HashMap::new(),
            next_idx: 0,
        }
    }

    /// Register a Text element's font and record character usage.
    /// Returns the PDF resource name for this font.
    pub fn track(&mut self, params: &TextParams) -> &str {
        let entity = params.font_entity;
        let usage = self.fonts.entry(entity).or_insert_with(|| {
            let idx = self.next_idx;
            self.next_idx += 1;
            let is_std14 = STANDARD_14.iter().any(|n| *n == params.font_name.as_slice());
            FontUsage {
                pdf_name: format!("F{}", idx),
                font_name: params.font_name.clone(),
                font_type: params.font_type,
                font_entity: entity,
                used_codes: HashSet::new(),
                is_standard_14: is_std14,
                sample_params: params.clone(),
            }
        });

        // Record used character codes
        if params.font_type == 0 {
            // CID font: 2-byte codes
            for chunk in params.text.chunks(2) {
                if chunk.len() == 2 {
                    let cid = ((chunk[0] as u16) << 8) | chunk[1] as u16;
                    usage.used_codes.insert(cid);
                }
            }
        } else {
            for &b in &params.text {
                usage.used_codes.insert(b as u16);
            }
        }

        &usage.pdf_name
    }

    /// Look up the PDF resource name for a font entity.
    pub fn get_pdf_name(&self, entity: EntityId) -> Option<&str> {
        self.fonts.get(&entity).map(|u| u.pdf_name.as_str())
    }

    /// Iterate over all tracked fonts.
    pub fn fonts(&self) -> impl Iterator<Item = &FontUsage> {
        self.fonts.values()
    }
}
