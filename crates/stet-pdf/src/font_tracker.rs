// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Font usage tracking for PDF text output.

use std::collections::{HashMap, HashSet};

use stet_core::context::Context;
use stet_core::dict::DictKey;
use stet_core::object::{EntityId, NameId, PsValue};
use stet_graphics::device::TextParams;

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
    /// The font dict instances this resource draws with: every instance of
    /// the font whose encoding agrees with the others'. dvips creates
    /// several re-encoded instances of one base font, each encoding a
    /// different subset, so the resource's encoding, widths and ToUnicode
    /// map merge all of them.
    pub all_entities: Vec<EntityId>,
    /// Every instance of the font, whatever its encoding: the glyphs to
    /// embed. Instances whose encodings conflict become separate resources
    /// but share this set, since a dvips instance may define glyphs another
    /// one encodes.
    pub program_entities: Vec<EntityId>,
    /// The glyph name at each code, merged over `all_entities` (`None`
    /// where none names a glyph). Empty when the encoding could not be
    /// read, which agrees with any other.
    encoding: Vec<Option<NameId>>,
    /// Writing mode of a Type 0 font: 1 for vertical, 0 otherwise. A PDF
    /// font has one, set by its CMap, so the horizontal and vertical
    /// instances of a CIDFont are separate resources.
    pub wmode: u8,
    /// Whether `wmode` came from a font dict, rather than being assumed
    /// for an instance registered without a [`Context`].
    wmode_known: bool,
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
/// Fonts with the same name and type share one font program regardless of
/// which page or scalefont/makefont created the font dict.
type FontKey = (Vec<u8>, i32);

/// Tracks font usage across all pages in a PDF job.
///
/// One PDF font resource serves every instance of a font whose encodings
/// agree. Instances that map some code to different glyphs — pdftops
/// re-encodes ZapfDingbats beside the standard one, for example — each need
/// their own resource, since a PDF font has one encoding; they still share
/// the font program.
pub struct FontTracker {
    /// Every font resource, in creation order.
    fonts: Vec<FontUsage>,
    /// Font key → indices of its resources in `fonts`.
    by_key: HashMap<FontKey, Vec<usize>>,
    /// Font entity → index of its resource (fast lookup during content
    /// stream generation).
    entity_to_font: HashMap<EntityId, usize>,
}

impl FontTracker {
    pub fn new() -> Self {
        Self {
            fonts: Vec::new(),
            by_key: HashMap::new(),
            entity_to_font: HashMap::new(),
        }
    }

    /// Register a Text element's font and record character usage.
    /// Returns the PDF resource name for this font.
    ///
    /// `ctx` supplies the font's encoding and writing mode, which decide
    /// whether it can share a resource with another instance of the same
    /// font; without it the instance joins the font's first resource.
    pub fn track(&mut self, params: &TextParams, ctx: Option<&Context>) -> &str {
        let entity = EntityId(params.font_entity);
        let idx = match self.entity_to_font.get(&entity) {
            Some(&idx) => idx,
            None => self.add_instance(params, entity, ctx),
        };
        let usage = &mut self.fonts[idx];

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

    /// Place a font instance seen for the first time: in the first resource
    /// of its font whose encoding agrees with it, or in a new one.
    fn add_instance(
        &mut self,
        params: &TextParams,
        entity: EntityId,
        ctx: Option<&Context>,
    ) -> usize {
        let key = (params.font_name.clone(), params.font_type);
        // A Type 0 font's Encoding picks descendant fonts, not glyphs.
        let encoding = match ctx {
            Some(ctx) if params.font_type != 0 => glyph_encoding(ctx, entity),
            _ => Vec::new(),
        };
        let wmode = match ctx {
            Some(ctx) if params.font_type == 0 => Some(writing_mode(ctx, entity)),
            _ => None,
        };
        let group = self.by_key.entry(key).or_default();
        let idx = match group.iter().copied().find(|&i| {
            let usage = &self.fonts[i];
            encodings_agree(&usage.encoding, &encoding)
                && (!usage.wmode_known || wmode.is_none_or(|w| w == usage.wmode))
        }) {
            Some(i) => {
                let usage = &mut self.fonts[i];
                merge_encoding(&mut usage.encoding, &encoding);
                if let Some(w) = wmode {
                    usage.wmode = w;
                    usage.wmode_known = true;
                }
                usage.all_entities.push(entity);
                i
            }
            None => {
                let idx = self.fonts.len();
                self.fonts.push(FontUsage {
                    pdf_name: format!("F{idx}"),
                    font_name: params.font_name.clone(),
                    font_type: params.font_type,
                    font_entity: entity,
                    all_entities: vec![entity],
                    program_entities: Vec::new(),
                    encoding,
                    wmode: wmode.unwrap_or(0),
                    wmode_known: wmode.is_some(),
                    used_codes: HashSet::new(),
                    is_standard_14: STANDARD_14.contains(&params.font_name.as_slice()),
                    sample_params: params.clone(),
                    widths: HashMap::new(),
                });
                group.push(idx);
                idx
            }
        };
        // Every resource of the font sees every instance's glyphs; a new
        // resource starts with those of the instances before it.
        let program: Vec<EntityId> = group
            .iter()
            .flat_map(|&i| self.fonts[i].all_entities.iter().copied())
            .collect();
        for &i in group.iter() {
            self.fonts[i].program_entities.clone_from(&program);
        }
        self.entity_to_font.insert(entity, idx);
        idx
    }

    /// Look up the PDF resource name for a font entity.
    pub fn get_pdf_name(&self, entity: EntityId) -> Option<&str> {
        self.entity_to_font
            .get(&entity)
            .map(|&i| self.fonts[i].pdf_name.as_str())
    }

    /// Iterate over all tracked fonts.
    pub fn fonts(&self) -> impl Iterator<Item = &FontUsage> {
        self.fonts.iter()
    }

    /// Iterate over all tracked fonts mutably.
    pub fn fonts_mut(&mut self) -> impl Iterator<Item = &mut FontUsage> {
        self.fonts.iter_mut()
    }

    /// The writing mode of a font entity's resource: 1 for vertical.
    pub fn wmode(&self, font_entity: EntityId) -> u8 {
        self.entity_to_font
            .get(&font_entity)
            .map_or(0, |&i| self.fonts[i].wmode)
    }

    /// Look up a glyph width for a font entity and character code.
    /// Returns width in 1000ths of a unit, or None if unavailable.
    pub fn get_glyph_width(&self, font_entity: EntityId, code: u16) -> Option<i32> {
        let &i = self.entity_to_font.get(&font_entity)?;
        self.fonts[i].widths.get(&code).copied()
    }
}

/// The glyph name a font dict's `Encoding` gives each code, `None` for
/// `.notdef` and anything that is not a name. Empty when the font has no
/// encoding array.
fn glyph_encoding(ctx: &Context, font: EntityId) -> Vec<Option<NameId>> {
    let Some(PsValue::Array { entity, start, len }) = ctx
        .dicts
        .get(font, &DictKey::Name(ctx.name_cache.n_encoding))
        .map(|o| o.value)
    else {
        return Vec::new();
    };
    let notdef = ctx.names.find(b".notdef");
    (0..len.min(256))
        .map(|i| match ctx.arrays.get_element(entity, start + i).value {
            PsValue::Name(id) if Some(id) != notdef => Some(id),
            _ => None,
        })
        .collect()
}

/// A Type 0 font's writing mode as the interpreter sets its text: 1 when
/// the root font's `WMode` is 1 and its CIDFont has writing-mode-1
/// metrics, a `Metrics2` or a `CDevProc`; else 0. Without them "the WMode
/// parameter is ignored" (PLRM 5.4), and the text runs horizontally.
fn writing_mode(ctx: &Context, font: EntityId) -> u8 {
    let get = |dict: EntityId, key: &[u8]| {
        ctx.names
            .find(key)
            .and_then(|id| ctx.dicts.get(dict, &DictKey::Name(id)))
    };
    let wmode = get(font, b"WMode").and_then(|o| o.as_i32());
    let cidfont = match get(font, b"FDepVector").map(|o| o.value) {
        Some(PsValue::Array { entity, start, len }) if len > 0 => {
            match ctx.arrays.get_element(entity, start).value {
                PsValue::Dict(d) => Some(d),
                _ => None,
            }
        }
        _ => None,
    };
    let vertical_metrics =
        cidfont.is_some_and(|d| get(d, b"Metrics2").is_some() || get(d, b"CDevProc").is_some());
    u8::from(wmode == Some(1) && vertical_metrics)
}

/// Whether no code names different glyphs in `a` and `b`.
fn encodings_agree(a: &[Option<NameId>], b: &[Option<NameId>]) -> bool {
    a.iter().zip(b).all(|pair| match pair {
        (Some(x), Some(y)) => x == y,
        _ => true,
    })
}

/// Fill the codes `into` leaves unnamed from `from`.
fn merge_encoding(into: &mut Vec<Option<NameId>>, from: &[Option<NameId>]) {
    if into.len() < from.len() {
        into.resize(from.len(), None);
    }
    for (slot, name) in into.iter_mut().zip(from) {
        if slot.is_none() {
            *slot = *name;
        }
    }
}
