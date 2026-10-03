// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Font usage tracking for PDF text output.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use stet_core::font_snapshot::{Frozen, FrozenDict, FrozenFont};
use stet_core::object::NameId;
use stet_graphics::device::TextParams;

use crate::font_data::FontData;

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
    /// The first instance seen.
    pub font: FontId,
    /// The first instance's font dictionary, as the interpreter copied it —
    /// what CharStrings and Private are read from. `None` without a copy.
    pub root: Option<Arc<FrozenDict>>,
    /// The copied font dictionaries of the instances this resource draws
    /// with: every instance of the font whose encoding agrees with the
    /// others'. dvips creates several re-encoded instances of one base font,
    /// each encoding a different subset, so the resource's encoding, widths
    /// and ToUnicode map merge all of them.
    pub all_fonts: Vec<Arc<FrozenDict>>,
    /// The copies of every instance of the font, whatever its encoding: the
    /// glyphs to embed. Instances whose encodings conflict become separate
    /// resources but share this set, since a dvips instance may define
    /// glyphs another one encodes.
    pub program_fonts: Vec<Arc<FrozenDict>>,
    /// The glyph name at each code, merged over `all_entities` (`None`
    /// where none names a glyph). Empty when the encoding could not be
    /// read, which agrees with any other.
    encoding: Vec<Option<NameId>>,
    /// Writing mode of a Type 0 font: 1 for vertical, 0 otherwise. A PDF
    /// font has one, set by its CMap, so the horizontal and vertical
    /// instances of a CIDFont are separate resources.
    pub wmode: u8,
    /// Whether `wmode` came from a font dict, rather than being assumed
    /// for an instance registered without its copy.
    wmode_known: bool,
    /// Set of character codes (or CIDs) used.
    pub used_codes: HashSet<u16>,
    /// Whether this is a Standard 14 font (skip embedding).
    pub is_standard_14: bool,
    /// First TextParams seen (used for font_size, ctm, font_matrix).
    pub sample_params: TextParams,
    /// Glyph widths in 1000ths of a unit (char_code → width), for every
    /// used code: set by `font_embedder::glyph_widths` once every page's text
    /// is tracked, before any content stream is built.
    pub widths: HashMap<u16, i32>,
}

/// Font deduplication key: (font_name, font_type).
/// Fonts with the same name and type share one font program regardless of
/// which page or scalefont/makefont created the font dict.
type FontKey = (Vec<u8>, i32);

/// A font instance as PDF output tells them apart: by the interpreter's copy
/// of it, when there is one — the same font dictionary, unchanged, is one
/// copy, while a font that reused a reclaimed one's id, or that a page
/// changed, is another — else by its font dictionary's id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FontId {
    /// The copy's address, stable while the job's copies are held.
    Copy(usize),
    /// The font dictionary's id, for text without a copy.
    Entity(u32),
}

impl FontId {
    fn of_copy(font: &FrozenFont) -> Self {
        FontId::Copy(Arc::as_ptr(&font.root) as usize)
    }
}

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
    /// Font instance → index of its resource (fast lookup during content
    /// stream generation).
    instance_to_font: HashMap<FontId, usize>,
    /// Each copied instance a `Text` element names
    /// ([`TextParams::font_snapshot`]), as told apart.
    snapshot_ids: HashMap<u32, FontId>,
}

impl FontTracker {
    pub fn new() -> Self {
        Self {
            fonts: Vec::new(),
            by_key: HashMap::new(),
            instance_to_font: HashMap::new(),
            snapshot_ids: HashMap::new(),
        }
    }

    /// The instance a `Text` element's font is: its copy, once tracked,
    /// else its font dictionary's id.
    pub fn font_id(&self, params: &TextParams) -> FontId {
        params
            .font_snapshot
            .and_then(|s| self.snapshot_ids.get(&s))
            .copied()
            .unwrap_or(FontId::Entity(params.font_entity))
    }

    /// Register a Text element's font and record character usage.
    /// Returns the PDF resource name for this font.
    ///
    /// `fonts` supplies the font's copy, whose encoding and writing mode
    /// decide whether it can share a resource with another instance of the
    /// same font; when `read` is false — or without a copy — the instance
    /// joins the font's first resource.
    pub fn track(&mut self, params: &TextParams, fonts: Option<&FontData>, read: bool) -> &str {
        let copy = fonts.and_then(|f| f.fonts.font(params.font_snapshot?).map(|c| (f, c)));
        let id = match copy {
            Some((_, c)) => FontId::of_copy(c),
            None => self.font_id(params),
        };
        if let (Some(s), Some(_)) = (params.font_snapshot, copy) {
            self.snapshot_ids.insert(s, id);
        }
        let idx = match self.instance_to_font.get(&id) {
            Some(&idx) => idx,
            None => self.add_instance(params, id, copy.filter(|_| read), copy.map(|(_, c)| c)),
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
    /// of its font whose encoding agrees with it, or in a new one. `read` is
    /// the copy to read the encoding and writing mode from, `copy` the copy
    /// the instance draws with.
    fn add_instance(
        &mut self,
        params: &TextParams,
        id: FontId,
        read: Option<(&FontData, &FrozenFont)>,
        copy: Option<&FrozenFont>,
    ) -> usize {
        let key = (params.font_name.clone(), params.font_type);
        // A Type 0 font's Encoding picks descendant fonts, not glyphs.
        let encoding = match read {
            Some((f, c)) if params.font_type != 0 => glyph_encoding(f, &c.root),
            _ => Vec::new(),
        };
        let wmode = match read {
            Some((f, c)) if params.font_type == 0 => Some(writing_mode(f, &c.root)),
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
                if let Some(c) = copy {
                    usage.all_fonts.push(c.root.clone());
                }
                i
            }
            None => {
                let idx = self.fonts.len();
                self.fonts.push(FontUsage {
                    pdf_name: format!("F{idx}"),
                    font_name: params.font_name.clone(),
                    font_type: params.font_type,
                    font: id,
                    root: copy.map(|c| c.root.clone()),
                    all_fonts: copy.map(|c| c.root.clone()).into_iter().collect(),
                    program_fonts: Vec::new(),
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
        let program: Vec<Arc<FrozenDict>> = group
            .iter()
            .flat_map(|&i| self.fonts[i].all_fonts.iter().cloned())
            .collect();
        for &i in group.iter() {
            self.fonts[i].program_fonts.clone_from(&program);
        }
        self.instance_to_font.insert(id, idx);
        idx
    }

    /// Look up the PDF resource name for a font instance.
    pub fn get_pdf_name(&self, font: FontId) -> Option<&str> {
        self.instance_to_font
            .get(&font)
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

    /// The writing mode of a font instance's resource: 1 for vertical.
    pub fn wmode(&self, font: FontId) -> u8 {
        self.instance_to_font
            .get(&font)
            .map_or(0, |&i| self.fonts[i].wmode)
    }

    /// Whether a font instance's resource has widths to kern its text with.
    /// Any instance of the resource, not only the first.
    pub fn has_widths(&self, font: FontId) -> bool {
        self.instance_to_font
            .get(&font)
            .is_some_and(|&i| !self.fonts[i].widths.is_empty())
    }

    /// Look up a glyph width for a font instance and character code.
    /// Returns width in 1000ths of a unit, or None if unavailable.
    pub fn get_glyph_width(&self, font: FontId, code: u16) -> Option<i32> {
        let &i = self.instance_to_font.get(&font)?;
        self.fonts[i].widths.get(&code).copied()
    }
}

/// The glyph name a font dict's `Encoding` gives each code, `None` for
/// `.notdef` and anything that is not a name. Empty when the font has no
/// encoding array.
fn glyph_encoding(fonts: &FontData, font: &FrozenDict) -> Vec<Option<NameId>> {
    let Some(encoding) = fonts.get(font, b"Encoding").and_then(Frozen::as_array) else {
        return Vec::new();
    };
    let notdef = fonts.find(b".notdef");
    encoding
        .iter()
        .take(256)
        .map(|o| o.as_name().filter(|&id| Some(id) != notdef))
        .collect()
}

/// A Type 0 font's writing mode as the interpreter sets its text: 1 when
/// the root font's `WMode` is 1 and its CIDFont has writing-mode-1
/// metrics, a `Metrics2` or a `CDevProc`; else 0. Without them "the WMode
/// parameter is ignored" (PLRM 5.4), and the text runs horizontally.
fn writing_mode(fonts: &FontData, font: &FrozenDict) -> u8 {
    let wmode = fonts.get(font, b"WMode").and_then(Frozen::as_i32);
    let cidfont = fonts
        .get(font, b"FDepVector")
        .and_then(Frozen::as_array)
        .and_then(<[Frozen]>::first)
        .and_then(Frozen::as_dict);
    let vertical_metrics = cidfont.is_some_and(|d| {
        fonts.get(d, b"Metrics2").is_some() || fonts.get(d, b"CDevProc").is_some()
    });
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
