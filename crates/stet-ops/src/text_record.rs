// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Recording `TextRun` elements from the show operators, for text
//! extraction (`Context::extract_text`).
//!
//! A show operator brackets its work with [`begin_show`] / [`end_show`], and
//! each rendering loop reports every glyph it shows through
//! [`record_glyph`] just before advancing the current point. With extraction
//! off, or outside a show operator, `record_glyph` returns at once.

use stet_core::context::{Context, OpenTextRun, TextCapture};
use stet_core::dict::DictKey;
use stet_core::graphics_state::Matrix;
use stet_core::object::{EntityId, NameId, PsValue};
use stet_fonts::cid_unicode::UnicodeCMap;
use stet_graphics::device::{ShownGlyph, TextRunParams, UnicodeSource};
use stet_graphics::display_list::DisplayElement;

/// Ascent and descent in a 1000-unit em, for a font with no usable
/// `FontBBox` or `hhea` table.
const DEFAULT_ASCENT: f64 = 800.0;
const DEFAULT_DESCENT: f64 = -200.0;

/// What a show operator set aside when it began: see [`begin_show`].
#[must_use = "pass to end_show"]
pub(crate) struct ShowRecording {
    /// Whether this operator records at all.
    active: bool,
    /// The enclosing operator's recording, for a show inside a `kshow`
    /// procedure.
    outer: Option<TextCapture>,
}

/// Start recording for a show operator, when text extraction is on and not
/// suspended. A show nested in another's procedure (`kshow`) closes the
/// outer run so far — its glyphs precede this operator's on the page — and
/// sets the outer recording aside until [`end_show`].
pub(crate) fn begin_show(ctx: &mut Context) -> ShowRecording {
    if !ctx.extract_text || ctx.text_suspended > 0 {
        return ShowRecording {
            active: false,
            outer: None,
        };
    }
    let outer = ctx.text_capture.take().map(|mut outer| {
        flush_run(ctx, &mut outer);
        outer
    });
    ctx.text_capture = Some(TextCapture::default());
    ShowRecording {
        active: true,
        outer,
    }
}

/// Finish a show operator's recording, adding its last run to the display
/// list, and restore the enclosing recording. Called whether or not the
/// operator succeeded.
pub(crate) fn end_show(ctx: &mut Context, recording: ShowRecording) {
    if !recording.active {
        return;
    }
    if let Some(mut capture) = ctx.text_capture.take() {
        flush_run(ctx, &mut capture);
    }
    ctx.text_capture = recording.outer;
}

/// Stop recording while a Type 3 font's `BuildChar` / `BuildGlyph` runs:
/// what it shows draws the glyph, and the glyph is the text. Pass the
/// result to [`resume`].
pub(crate) fn suspend(ctx: &mut Context) -> Option<TextCapture> {
    ctx.text_suspended += 1;
    ctx.text_capture.take()
}

/// Undo [`suspend`].
pub(crate) fn resume(ctx: &mut Context, saved: Option<TextCapture>) {
    ctx.text_suspended -= 1;
    ctx.text_capture = saved;
}

/// Where a shown glyph's text comes from.
pub(crate) enum GlyphText {
    /// The glyph's name, through the Adobe Glyph List.
    Name(NameId),
    /// A CID-keyed font's glyph: its code, when the CMap's codes are
    /// Unicode, else its CID through the CIDFont's character collection.
    Cid { cid: i32, source: CidText },
    /// Nothing is known.
    None,
}

/// What a CID-keyed font offers for its glyphs' text: see
/// [`cid_text_source`].
#[derive(Clone, Copy, Default)]
pub(crate) struct CidText {
    /// The CMap is a predefined `Uni…` one, whose codes are the text.
    unicode_cmap: Option<UnicodeCMap>,
    /// The CIDFont's Adobe character collection (`Japan1`, …), whose table
    /// gives each CID's text.
    ordering: Option<&'static [u8]>,
}

/// The text sources of a Type 0 font `type0` using a CMap, over CIDFont
/// `cidfont`: the CMap's name, for the `Uni…` CMaps, and the CIDFont's
/// `CIDSystemInfo`, for Adobe's four CJK collections.
pub(crate) fn cid_text_source(ctx: &Context, type0: EntityId, cidfont: EntityId) -> CidText {
    let name_key = |key: &[u8]| ctx.names.find(key).map(DictKey::Name);
    let bytes_of = |value: PsValue| -> Option<Vec<u8>> {
        match value {
            PsValue::Name(id) => Some(ctx.names.get_bytes(id).to_vec()),
            PsValue::String { entity, start, len } => {
                Some(ctx.strings.get(entity, start, len).to_vec())
            }
            _ => None,
        }
    };
    let unicode_cmap = name_key(b"CMap")
        .and_then(|key| ctx.dicts.get(type0, &key))
        .and_then(|cmap| match cmap.value {
            PsValue::Dict(cmap) => Some(cmap),
            _ => None,
        })
        .and_then(|cmap| ctx.dicts.get(cmap, &name_key(b"CMapName")?))
        .and_then(|name| bytes_of(name.value))
        .and_then(|name| UnicodeCMap::from_cmap_name(&name));
    let system_info = name_key(b"CIDSystemInfo")
        .and_then(|key| ctx.dicts.get(cidfont, &key))
        .and_then(|info| match info.value {
            PsValue::Dict(info) => Some(info),
            _ => None,
        });
    let entry = |key: &[u8]| {
        system_info
            .and_then(|info| ctx.dicts.get(info, &name_key(key)?))
            .and_then(|value| bytes_of(value.value))
    };
    let ordering = (entry(b"Registry").as_deref() == Some(b"Adobe"))
        .then(|| entry(b"Ordering"))
        .flatten()
        .and_then(|ordering| -> Option<&'static [u8]> {
            match &ordering[..] {
                b"Japan1" => Some(b"Japan1"),
                b"CNS1" => Some(b"CNS1"),
                b"GB1" => Some(b"GB1"),
                b"Korea1" => Some(b"Korea1"),
                _ => None,
            }
        });
    CidText {
        unicode_cmap,
        ordering,
    }
}

/// Where a font's ascent and descent come from, in its glyph space.
pub(crate) enum GlyphMetrics {
    /// The font dictionary's `FontBBox`, else 0.8 / -0.2 of a 1000-unit em.
    FontBBox,
    /// Supplied by the caller (a TrueType font's `hhea` table).
    Given { ascent: f64, descent: f64 },
    /// A Type 3 font's `FontBBox`; when that is empty (dvips writes
    /// `[0 0 0 0]`), `glyph_box`, the glyph's `setcachedevice` box, with the
    /// run widened to cover each of its glyphs' boxes; else the defaults.
    /// A Type 3 font's glyph space is its own, so the defaults, which
    /// assume 1000 units per em, can be far off.
    Type3 { glyph_box: Option<[f64; 4]> },
}

/// The ascent and descent a glyph box `[llx, lly, urx, ury]` gives, if it
/// has any height.
fn box_extent(glyph_box: Option<[f64; 4]>) -> Option<(f64, f64)> {
    let [_, y0, _, y1] = glyph_box?;
    (y0.max(y1) > y0.min(y1)).then_some((y0.max(y1), y0.min(y1)))
}

/// One glyph a rendering loop shows.
pub(crate) struct Glyph {
    /// The font dictionary the glyph comes from: its `FontName`, `Encoding`
    /// and `FontBBox`.
    pub font: EntityId,
    /// Glyph space → user space: the matrix the loop draws the outline with.
    pub glyph_space: Matrix,
    pub metrics: GlyphMetrics,
    /// The character code shown.
    pub code: u32,
    pub text: GlyphText,
    /// The glyph's origin in user space.
    pub origin: (f64, f64),
    /// The glyph's width in user space, before any spacing an operator adds:
    /// its vertical advance in vertical writing.
    pub width: (f64, f64),
    /// Vertical writing (WMode 1): `origin` is the glyph's vertical origin.
    pub vertical: bool,
}

/// Record `glyph` in the open run, starting a new run when it is the
/// first, or its font or glyph-space matrix differs from the run's.
pub(crate) fn record_glyph(ctx: &mut Context, glyph: Glyph) {
    let Some(mut capture) = ctx.text_capture.take() else {
        return;
    };
    let ctm = ctx.gstate.ctm;
    let to_device = ctm.concat(&glyph.glyph_space);
    let linear = [to_device.a, to_device.b, to_device.c, to_device.d];
    let joins = capture.run.as_ref().is_some_and(|run| {
        run.font == glyph.font
            && run.params.vertical == glyph.vertical
            && run
                .linear
                .iter()
                .zip(linear)
                .all(|(a, b)| (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0))
    });
    if !joins {
        flush_run(ctx, &mut capture);
        capture.run = Some(open_run(ctx, &glyph, linear));
    }
    let run = capture.run.as_mut().expect("a run was just opened");
    if run.extent_from_glyphs
        && let GlyphMetrics::Type3 { glyph_box } = glyph.metrics
        && let Some((ascent, descent)) = box_extent(glyph_box)
    {
        if run.glyph_box_seen {
            run.params.ascent = run.params.ascent.max(ascent);
            run.params.descent = run.params.descent.min(descent);
        } else {
            (run.params.ascent, run.params.descent) = (ascent, descent);
            run.glyph_box_seen = true;
        }
    }

    let text = &mut run.params.text;
    let start = text.len() as u32;
    let source = match glyph.text {
        GlyphText::Name(id) => {
            push_name_text(ctx, id, run.zapf_dingbats, run.hex_glyph_names, text)
        }
        GlyphText::Cid { cid, source } => push_cid_text(glyph.code, cid, source, text),
        GlyphText::None => UnicodeSource::Unmapped,
    };
    let end = text.len() as u32;
    run.params.glyphs.push(ShownGlyph {
        text_range: start..end,
        origin: ctm.transform_point(glyph.origin.0, glyph.origin.1),
        advance: ctm.transform_delta(glyph.width.0, glyph.width.1),
        code: glyph.code,
        source,
    });
    ctx.text_capture = Some(capture);
}

/// Open a run for `glyph`, whose glyph space maps to device space with
/// `linear` (its glyph-space matrix after the CTM).
fn open_run(ctx: &Context, glyph: &Glyph, linear: [f64; 4]) -> OpenTextRun {
    // Glyph (0, 0) sits at the glyph's origin, less any translation in
    // the glyph-space matrix (a FontMatrix offset).
    let glyph_to_device = ctx
        .gstate
        .ctm
        .concat(&Matrix::translate(glyph.origin.0, glyph.origin.1))
        .concat(&glyph.glyph_space);
    let defaults = (DEFAULT_ASCENT, DEFAULT_DESCENT);
    // Set from the first glyph's box by `record_glyph`.
    let mut extent_from_glyphs = false;
    let (ascent, descent) = match glyph.metrics {
        GlyphMetrics::Given { ascent, descent } => (ascent, descent),
        GlyphMetrics::FontBBox => font_bbox_extent(ctx, glyph.font).unwrap_or(defaults),
        GlyphMetrics::Type3 { .. } => font_bbox_extent(ctx, glyph.font).unwrap_or_else(|| {
            extent_from_glyphs = true;
            defaults
        }),
    };
    let font_name = font_name(ctx, glyph.font);
    let zapf_dingbats = font_name == "ZapfDingbats";
    OpenTextRun {
        params: TextRunParams {
            glyph_to_device,
            ascent,
            descent,
            font_name,
            vertical: glyph.vertical,
            ..TextRunParams::default()
        },
        font: glyph.font,
        linear,
        zapf_dingbats,
        hex_glyph_names: encoding_names_are_hex(ctx, glyph.font),
        extent_from_glyphs,
        glyph_box_seen: false,
    }
}

/// Add the open run to the display list, if it recorded a glyph.
fn flush_run(ctx: &mut Context, capture: &mut TextCapture) {
    if let Some(run) = capture.run.take()
        && !run.params.glyphs.is_empty()
    {
        ctx.current_display_list_mut()
            .push(DisplayElement::TextRun { params: run.params });
    }
}

/// Append the text glyph name `id` gives, and say where it came from.
fn push_name_text(
    ctx: &Context,
    id: NameId,
    zapf_dingbats: bool,
    hex_glyph_names: bool,
    out: &mut String,
) -> UnicodeSource {
    let Ok(name) = std::str::from_utf8(ctx.names.get_bytes(id)) else {
        return UnicodeSource::Unmapped;
    };
    let text = if zapf_dingbats {
        stet_fonts::agl::zapf_dingbats_glyph_name_to_text(name)
    } else {
        stet_fonts::agl::glyph_name_to_text(name)
    };
    if let Some(text) = text {
        out.push_str(&text);
        return UnicodeSource::GlyphName;
    }
    if let Some(ch) = stet_fonts::agl::numeric_glyph_name_to_char(name, hex_glyph_names) {
        out.push(ch);
        return UnicodeSource::GlyphName;
    }
    UnicodeSource::Unmapped
}

/// Append the text of CID `cid`, shown as `code`, and say where it came
/// from.
fn push_cid_text(code: u32, cid: i32, source: CidText, out: &mut String) -> UnicodeSource {
    if let Some(ch) = source.unicode_cmap.and_then(|cmap| cmap.code_to_char(code)) {
        out.push(ch);
        return UnicodeSource::CidOrdering;
    }
    if let (Some(ordering), Ok(cid)) = (source.ordering, u16::try_from(cid))
        && let Some(text) = stet_fonts::cid_unicode::cid_to_text(ordering, cid)
    {
        out.push_str(text);
        return UnicodeSource::CidOrdering;
    }
    UnicodeSource::Unmapped
}

/// The font's `FontName` — a CIDFont's `CIDFontName` — or empty.
fn font_name(ctx: &Context, font: EntityId) -> String {
    let keys = [
        Some(ctx.name_cache.n_font_name),
        ctx.names.find(b"CIDFontName"),
    ];
    for key in keys.into_iter().flatten() {
        match ctx
            .dicts
            .get(font, &DictKey::Name(key))
            .map(|obj| obj.value)
        {
            Some(PsValue::Name(id)) => {
                return String::from_utf8_lossy(ctx.names.get_bytes(id)).into_owned();
            }
            Some(PsValue::String { entity, start, len }) => {
                return String::from_utf8_lossy(ctx.strings.get(entity, start, len)).into_owned();
            }
            _ => {}
        }
    }
    String::new()
}

/// The y extent of the font's `FontBBox`, when it has a non-empty one.
fn font_bbox_extent(ctx: &Context, font: EntityId) -> Option<(f64, f64)> {
    let obj = ctx
        .dicts
        .get(font, &DictKey::Name(ctx.name_cache.n_font_bbox))?;
    let PsValue::Array { entity, start, len } = obj.value else {
        return None;
    };
    let values: Vec<f64> = ctx
        .arrays
        .get(entity, start, len)
        .iter()
        .filter_map(|o| o.as_f64())
        .collect();
    let [_, y0, _, y1] = values[..] else {
        return None;
    };
    (y0.max(y1) > y0.min(y1)).then_some((y0.max(y1), y0.min(y1)))
}

/// Whether the font's `Encoding` names its glyphs with hex numbers: see
/// [`stet_fonts::agl::glyph_names_are_hex`].
fn encoding_names_are_hex(ctx: &Context, font: EntityId) -> bool {
    let Some(obj) = ctx
        .dicts
        .get(font, &DictKey::Name(ctx.name_cache.n_encoding))
    else {
        return false;
    };
    let PsValue::Array { entity, start, len } = obj.value else {
        return false;
    };
    let names = ctx.arrays.get(entity, start, len).iter().filter_map(|o| {
        let PsValue::Name(id) = o.value else {
            return None;
        };
        std::str::from_utf8(ctx.names.get_bytes(id)).ok()
    });
    stet_fonts::agl::glyph_names_are_hex(names)
}

/// A TrueType font's ascent and descent, in font units (its glyph space),
/// from its `hhea` table; the defaults, scaled to its em, without one.
pub(crate) fn truetype_metrics(font_data: Option<&[u8]>) -> GlyphMetrics {
    use stet_fonts::truetype::{find_table, get_units_per_em, read_i16};
    let Some(data) = font_data else {
        return GlyphMetrics::Given {
            ascent: DEFAULT_ASCENT,
            descent: DEFAULT_DESCENT,
        };
    };
    if let Some((offset, len)) = find_table(data, b"hhea")
        && len >= 8
        && offset + 8 <= data.len()
    {
        let ascent = read_i16(data, offset + 4) as f64;
        let descent = read_i16(data, offset + 6) as f64;
        if ascent > descent {
            return GlyphMetrics::Given { ascent, descent };
        }
    }
    let em = get_units_per_em(data) as f64 / 1000.0;
    GlyphMetrics::Given {
        ascent: DEFAULT_ASCENT * em,
        descent: DEFAULT_DESCENT * em,
    }
}
