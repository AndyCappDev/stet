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
    /// Font dict entity ID (first instance seen — used for CharStrings/Private).
    pub font_entity: EntityId,
    /// All unique font dict entity IDs seen for this font name.
    /// dvips creates multiple re-encoded instances of the same base font,
    /// each with a different encoding subset. We need all of them to build
    /// a complete ToUnicode map.
    pub all_entities: Vec<EntityId>,
    /// Set of character codes (or CIDs) used.
    pub used_codes: HashSet<u16>,
    /// Whether this is a Standard 14 font (skip embedding).
    pub is_standard_14: bool,
    /// First TextParams seen (used for font_size, ctm, font_matrix).
    pub sample_params: TextParams,
    /// Glyph widths in 1000ths of a unit (char_code → width).
    /// Populated by font_embedder::extract_widths() before content stream generation.
    pub widths: HashMap<u16, i32>,
}

/// Font deduplication key: (font_name, font_type).
/// Fonts with the same name and type produce the same embedded font program
/// regardless of which page or scalefont/makefont created the font dict.
type FontKey = (Vec<u8>, i32);

/// Tracks font usage across all pages in a PDF job.
pub struct FontTracker {
    /// Map from font key → font usage (document-level dedup).
    fonts: HashMap<FontKey, FontUsage>,
    /// Map from font entity → PDF name (for fast lookup by entity during content stream gen).
    entity_to_name: HashMap<EntityId, String>,
    /// Next font index for naming.
    next_idx: usize,
}

impl FontTracker {
    pub fn new() -> Self {
        Self {
            fonts: HashMap::new(),
            entity_to_name: HashMap::new(),
            next_idx: 0,
        }
    }

    /// Register a Text element's font and record character usage.
    /// Returns the PDF resource name for this font.
    pub fn track(&mut self, params: &TextParams) -> &str {
        let key = (params.font_name.clone(), params.font_type);
        let entity = params.font_entity;

        let usage = self.fonts.entry(key).or_insert_with(|| {
            let idx = self.next_idx;
            self.next_idx += 1;
            let is_std14 = STANDARD_14
                .iter()
                .any(|n| *n == params.font_name.as_slice());
            FontUsage {
                pdf_name: format!("F{}", idx),
                font_name: params.font_name.clone(),
                font_type: params.font_type,
                font_entity: entity,
                all_entities: vec![entity],
                used_codes: HashSet::new(),
                is_standard_14: is_std14,
                sample_params: params.clone(),
                widths: HashMap::new(),
            }
        });

        // Track all unique font entities (dvips creates multiple re-encoded instances)
        if !usage.all_entities.contains(&entity) {
            usage.all_entities.push(entity);
        }

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

        // Cache entity→name mapping for fast lookup
        let name = usage.pdf_name.clone();
        self.entity_to_name.insert(entity, name);

        &usage.pdf_name
    }

    /// Look up the PDF resource name for a font entity.
    pub fn get_pdf_name(&self, entity: EntityId) -> Option<&str> {
        self.entity_to_name.get(&entity).map(|s| s.as_str())
    }

    /// Iterate over all tracked fonts.
    pub fn fonts(&self) -> impl Iterator<Item = &FontUsage> {
        self.fonts.values()
    }

    /// Iterate over all tracked fonts mutably.
    pub fn fonts_mut(&mut self) -> impl Iterator<Item = &mut FontUsage> {
        self.fonts.values_mut()
    }

    /// Look up a glyph width for a font entity and character code.
    /// Returns width in 1000ths of a unit, or None if unavailable.
    pub fn get_glyph_width(&self, font_entity: EntityId, code: u16) -> Option<i32> {
        let pdf_name = self.entity_to_name.get(&font_entity)?;
        self.fonts
            .values()
            .find(|u| u.pdf_name == *pdf_name)
            .and_then(|u| u.widths.get(&code).copied())
    }
}
