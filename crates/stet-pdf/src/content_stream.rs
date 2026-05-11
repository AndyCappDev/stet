// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Convert a DisplayList into PDF content stream bytes.

use std::collections::{HashMap, HashSet};
use std::io::Write as IoWrite;

use crate::font_embedder;
use crate::font_tracker::FontTracker;
use crate::image_ops::{self, ImageXObject};
use crate::pdf_objects::PdfObj;
use crate::text_ops;
use stet_core::context::Context;
use stet_fonts::geometry::{Matrix, PsPath};
use stet_graphics::color::{DeviceColor, FillRule, LineCap, LineJoin};
use stet_graphics::device::{
    AxialShadingParams, BgUcrState, HalftoneState, MeshShadingParams, PatchShadingParams,
    PatternFillParams, RadialShadingParams, SpotColor, SpotColorSpace, StrokeParams, TextParams,
    TransferState,
};
use stet_graphics::display_list::{DisplayElement, DisplayList};

/// Result of generating a content stream from a display list.
pub struct ContentStreamResult {
    /// Raw content stream bytes (before compression).
    pub content: Vec<u8>,
    /// Image XObjects referenced by the content stream.
    pub images: Vec<ImageXObject>,
    /// Indices into the display list for shading elements, paired with
    /// the shading resource name index used in the content stream.
    pub shading_refs: Vec<ShadingRef>,
    /// PDF font names used on this page (e.g., ["F0", "F2"]).
    pub used_font_names: Vec<String>,
    /// ExtGState resource dicts used in the content stream.
    pub ext_gstate_dicts: Vec<ExtGStateDict>,
    /// Color space definitions used in the content stream (Separation/DeviceN).
    /// Vec of (resource_name, SpotColorSpace) pairs.
    pub color_spaces: Vec<(String, SpotColorSpace)>,
    /// ICCBased color space definitions used in the content stream (one
    /// per unique source profile, deduped by `IccColorSpace.profile_hash`).
    /// Vec of (resource_name, IccColorSpace) pairs.
    pub icc_color_spaces: Vec<(String, stet_graphics::device::IccColorSpace)>,
    /// Tiling pattern references used in the content stream.
    pub pattern_refs: Vec<PatternRef>,
    /// Color space entries for uncolored patterns (e.g., [/Pattern /DeviceRGB]).
    pub pattern_cs_entries: Vec<(String, PdfObj)>,
    /// Transfer function references to emit as PDF Type 0 function objects.
    pub transfer_refs: Vec<TransferFunctionRef>,
    /// Halftone references to emit as PDF halftone objects.
    pub halftone_refs: Vec<HalftoneRef>,
    /// Black generation / undercolor removal references for PDF output.
    pub bg_ucr_refs: Vec<BgUcrRef>,
    /// Soft-mask references to patch into ExtGState dicts at write time
    /// (the mask Form XObject's indirect ref is allocated in
    /// `pdf_device.rs`, not here).
    pub soft_mask_refs: Vec<SoftMaskRef>,
    /// Optional-content markers (`/OC /Pn BDC … EMC`) emitted into the
    /// content stream. `pdf_device.rs` builds the corresponding `/OCG`
    /// or `/OCMD` indirect objects and wires them through the page's
    /// `/Resources /Properties` dict + the Catalog's `/OCProperties`.
    pub ocg_marker_refs: Vec<OcgMarkerRef>,
    /// Form XObjects emitted for `DisplayElement::Group` and the content
    /// portion of `DisplayElement::SoftMasked`. The Forms inherit the
    /// page's `/Resources` (PDF 1.7 § 7.8.3), so they carry no `/Resources`
    /// dict of their own; whatever fonts / images / ext-gstates the Form's
    /// content stream references is already in the page-level
    /// resource lists above.
    pub form_xobjects: Vec<FormXObject>,
}

/// A PDF Form XObject — a self-contained content stream wrapped as an
/// indirect object so it can be invoked via `/Xn Do`. stet emits one for
/// each `DisplayElement::Group` and for the mask portion of
/// `DisplayElement::SoftMasked`. The Form's resources are inherited from
/// the enclosing page, so this struct carries only the per-form data.
pub struct FormXObject {
    /// Raw content stream bytes (before compression).
    pub content: Vec<u8>,
    /// `/BBox` in the enclosing content stream's user coordinate system
    /// (device space, the same units the parent's paths use).
    pub bbox: [f64; 4],
    /// Entries that go into the `/Group` transparency dict. `None` means
    /// emit no `/Group` entry — used for the mask Form on a `SoftMasked`
    /// element (the mask doesn't need its own transparency group), and
    /// for other Forms that don't need transparency-group semantics.
    pub group_dict_entries: Option<Vec<(Vec<u8>, PdfObj)>>,
}

/// Reference from an ExtGState to pre-sampled transfer function tables.
pub struct TransferFunctionRef {
    /// Index into ext_gstate_dicts.
    pub ext_gstate_idx: usize,
    /// For single transfer: Some(table) in index 0, rest None.
    /// For 4-component: [R, G, B, Gray], each Some or None (identity).
    pub tables: Vec<Option<std::sync::Arc<Vec<f64>>>>,
    /// True if this is a 4-component (color) transfer.
    pub is_color: bool,
}

/// Reference from an ExtGState to pre-computed halftone screen data.
pub struct HalftoneRef {
    /// Index into ext_gstate_dicts.
    pub ext_gstate_idx: usize,
    /// Halftone state captured at paint time.
    pub state: HalftoneState,
}

/// Reference from an ExtGState to pre-sampled black generation / UCR tables.
pub struct BgUcrRef {
    /// Index into ext_gstate_dicts.
    pub ext_gstate_idx: usize,
    /// BG/UCR state captured at paint time.
    pub state: BgUcrState,
}

/// One Optional-Content marker `(/OC /Pn BDC … EMC)` recorded by the
/// content stream. The Builder allocates the resource name `Pn` at
/// emission time; `pdf_device.rs` resolves the visibility predicate
/// into the right indirect object (a single `/OCG` for the `Single`
/// case, an `/OCMD` for `Membership` / `Expression`) and adds it to the
/// page's `/Resources /Properties` dict.
pub struct OcgMarkerRef {
    /// Resource name written into the content stream (`Pn`).
    pub resource_name: String,
    /// Visibility predicate from the source DisplayList.
    pub visibility: stet_graphics::display_list::OcgVisibility,
}

/// Reference from an ExtGState dict to a soft-mask Form XObject. The
/// content stream emits a placeholder ExtGState dict (just `/Type`)
/// during generation, then `pdf_device.rs` patches the dict with a real
/// `/SMask << /Type /Mask /S /Alpha|/Luminosity /G <form-ref> >>` entry
/// once the Form XObject's indirect object number is known.
pub struct SoftMaskRef {
    /// Index into `ext_gstate_dicts`.
    pub ext_gstate_idx: usize,
    /// Index into `form_xobjects` — the mask form.
    pub mask_form_idx: usize,
    /// `/S` entry on the `/SMask` dict.
    pub subtype: stet_graphics::display_list::SoftMaskSubtype,
    /// `/BC` backdrop color (used only for `/Luminosity` masks).
    pub backdrop_color: Option<[f64; 3]>,
    /// True when the mask values should be inverted — emitted as
    /// `/TR { 1 exch sub }` on the `/SMask` dict.
    pub transfer_invert: bool,
}

/// Reference to a tiling pattern that needs a PDF Pattern XObject.
pub struct PatternRef {
    /// Pre-rendered display list for a single tile.
    pub tile: DisplayList,
    /// Pattern matrix (pattern space → device space).
    pub pattern_matrix: Matrix,
    /// Bounding box of one tile in pattern space [llx, lly, urx, ury].
    pub bbox: [f64; 4],
    /// Horizontal step between tile origins.
    pub xstep: f64,
    /// Vertical step between tile origins.
    pub ystep: f64,
    /// Paint type: 1 = colored, 2 = uncolored.
    pub paint_type: i32,
}

/// An ExtGState resource used in the content stream.
pub struct ExtGStateDict {
    /// PDF dict entries (key-value pairs for the ExtGState resource).
    pub entries: Vec<(Vec<u8>, PdfObj)>,
}

/// Reference to a shading element that needs a PDF shading resource.
pub enum ShadingRef {
    Axial(AxialShadingParams),
    Radial(RadialShadingParams),
    Mesh(MeshShadingParams),
    Patch(PatchShadingParams),
}

/// Cached graphics state for suppressing redundant PDF operators.
#[derive(Clone)]
struct GState {
    fill_color: Option<PdfColor>,
    stroke_color: Option<PdfColor>,
    line_width: f64,
    line_cap: i32,
    line_join: i32,
    miter_limit: f64,
    dash_array: Vec<f64>,
    dash_offset: f64,
    overprint: bool,
    /// Overprint mode currently set in the PDF graphics state. -1 = no
    /// /OPM emitted yet (PDF default of 0); otherwise the most-recent
    /// value pushed. Tracked separately from `overprint` because OPM is
    /// a distinct graphics-state entry and stet has to round-trip
    /// `params.overprint_mode` from the DisplayList faithfully (the
    /// previous hardcoded /OPM 1 silently broke GWG K-knockouts).
    overprint_mode: i32,
    /// Most-recent /ca (non-stroking alpha) emitted. Tracked separately
    /// from /CA because PDF allows them to be set independently on an
    /// ExtGState dict.
    fill_alpha: f64,
    /// Most-recent /CA (stroking alpha) emitted.
    stroke_alpha: f64,
    /// Most-recent /BM (blend mode) emitted. Blend mode is a shared
    /// graphics-state entry — fills and strokes use the same one.
    blend_mode: u8,
    /// Current fill color space resource name (e.g., "CS0") for Separation/DeviceN.
    fill_cs_name: Option<String>,
    /// Current stroke color space resource name.
    stroke_cs_name: Option<String>,
    /// Rendering intent (0=RelativeColorimetric, 1=Absolute, 2=Perceptual, 3=Saturation).
    rendering_intent: u8,
    /// Dedup key for current transfer state.
    transfer_key: Vec<u8>,
    /// Dedup key for current halftone state.
    halftone_key: Vec<u8>,
    /// Dedup key for current BG/UCR state.
    bg_ucr_key: Vec<u8>,
}

impl GState {
    fn new() -> Self {
        Self {
            fill_color: None,
            stroke_color: None,
            line_width: -1.0, // force first emission
            line_cap: -1,
            line_join: -1,
            miter_limit: -1.0,
            dash_array: Vec::new(),
            dash_offset: -1.0,
            overprint: false,
            overprint_mode: -1,
            fill_alpha: 1.0,
            stroke_alpha: 1.0,
            blend_mode: 0,
            fill_cs_name: None,
            stroke_cs_name: None,
            rendering_intent: 0,
            transfer_key: Vec::new(),
            halftone_key: Vec::new(),
            bg_ucr_key: Vec::new(),
        }
    }

    fn reset(&mut self) {
        *self = Self::new();
    }
}

#[derive(Clone, Debug, PartialEq)]
enum PdfColor {
    Gray(u16),          // quantized to 0..10000
    Rgb(u16, u16, u16), // quantized
    Cmyk(u16, u16, u16, u16),
}

fn quantize(v: f64) -> u16 {
    (v.clamp(0.0, 1.0) * 10000.0) as u16
}

#[cfg(test)]
fn color_to_pdf(c: &DeviceColor) -> PdfColor {
    color_to_pdf_with_channels(c, 0)
}

/// Same as [`color_to_pdf`] but honors the source paint's CMYK channel mask.
///
/// When `painted_channels == CMYK_K` and `native_cmyk = (0, 0, 0, k)`, the
/// paint originated as DeviceGray and was promoted to K-only DeviceCMYK by
/// the PDF reader's `gray_paint_for_gstate`. PDF's OPM semantics differ
/// between DeviceGray and DeviceCMYK (OPM only applies to DeviceCMYK
/// colorants), so a round-trip must emit it back as `g` — emitting `k`
/// would let OPM-1 preserve the underlying CMY components and let the
/// background leak through.
fn color_to_pdf_with_channels(c: &DeviceColor, painted_channels: u8) -> PdfColor {
    use stet_graphics::device::CMYK_K;
    if let Some((c_val, m, y, k)) = c.native_cmyk {
        if painted_channels == CMYK_K && c_val == 0.0 && m == 0.0 && y == 0.0 {
            let g = (1.0 - k).clamp(0.0, 1.0);
            return PdfColor::Gray(quantize(g));
        }
        PdfColor::Cmyk(quantize(c_val), quantize(m), quantize(y), quantize(k))
    } else if c.r == c.g && c.g == c.b {
        PdfColor::Gray(quantize(c.r))
    } else {
        PdfColor::Rgb(quantize(c.r), quantize(c.g), quantize(c.b))
    }
}

/// Stateful builder for one content stream — a page or a tile pattern.
/// Owns the mutable state per-element emission needs, and exposes
/// `emit_list` so nested DisplayLists (the children of Group /
/// SoftMasked / OcgGroup) can be processed recursively with the same
/// state.
struct Builder<'tracker> {
    buf: Vec<u8>,
    images: Vec<ImageXObject>,
    shading_refs: Vec<ShadingRef>,
    clip_depth: u32,
    /// Parallel stack to PDF's q/Q gstate save frames, one entry per
    /// open `Clip` (each emits `q`). InitClip pops in reverse, restoring
    /// `self.gs` to the value saved at the matching Clip's `q`. Without
    /// this, the tracker resets to default after InitClip but PDF's
    /// real gstate restores to whatever was active at the matching q —
    /// so the next paint's emit_overprint/emit_paint_alpha_blend
    /// disagree with reality and skip the operator that would fix the
    /// PDF state (visible as overprint leaking past clip scopes).
    clip_gs_stack: Vec<GState>,
    /// q-scopes the writer has open. Each scope is the list of clip
    /// paths added to that scope. Multiple Clips in one scope emit
    /// `path W n` operations one after another in the same `q...Q` block
    /// rather than nesting a new q for each — that's what PDF source
    /// streams typically look like and what avoids round-trip growth
    /// from the reader's `InitClip + replay-of-outer-clips` pattern.
    clip_scopes: Vec<Vec<stet_fonts::geometry::PsPath>>,
    /// Paths queued for skip-on-emit after the most recent InitClip.
    /// Each InitClip seeds this with the still-active clips (i.e. the
    /// flattened `clip_scopes`). Subsequent Clip elements consume the
    /// front of the queue when paths match; once drained or mismatched,
    /// emission resumes normally — additional clips go into the
    /// currently-open scope without opening a fresh `q`.
    pending_replay_clips: std::collections::VecDeque<stet_fonts::geometry::PsPath>,
    gs: GState,
    ext_gstates: Vec<ExtGStateDict>,
    ext_gstate_map: HashMap<Vec<u8>, usize>,
    color_spaces: Vec<(String, SpotColorSpace)>,
    cs_name_map: HashMap<Vec<u8>, String>,
    icc_color_spaces: Vec<(String, stet_graphics::device::IccColorSpace)>,
    icc_cs_name_map: HashMap<Vec<u8>, String>,
    pattern_refs: Vec<PatternRef>,
    pattern_map: HashMap<u32, usize>,
    pattern_cs_names: Vec<(String, PdfObj)>,
    pattern_cs_set: HashSet<String>,
    transfer_refs: Vec<TransferFunctionRef>,
    halftone_refs: Vec<HalftoneRef>,
    bg_ucr_refs: Vec<BgUcrRef>,
    soft_mask_refs: Vec<SoftMaskRef>,
    ocg_marker_refs: Vec<OcgMarkerRef>,
    form_xobjects: Vec<FormXObject>,
    page_font_names: HashSet<String>,
    has_text_elements: bool,
    page_w: u32,
    page_h: u32,
    /// True when emitting a Pattern XObject tile. `ErasePage` is a no-op
    /// in this mode — tiles have no page background.
    in_tile: bool,
    font_tracker: &'tracker mut FontTracker,
}

impl<'tracker> Builder<'tracker> {
    fn new(
        font_tracker: &'tracker mut FontTracker,
        page_w: u32,
        page_h: u32,
        has_text_elements: bool,
        in_tile: bool,
    ) -> Self {
        Self {
            buf: Vec::with_capacity(if in_tile { 1024 } else { 4096 }),
            images: Vec::new(),
            shading_refs: Vec::new(),
            clip_depth: 0,
            clip_gs_stack: Vec::new(),
            clip_scopes: Vec::new(),
            pending_replay_clips: std::collections::VecDeque::new(),
            gs: GState::new(),
            ext_gstates: Vec::new(),
            ext_gstate_map: HashMap::new(),
            color_spaces: Vec::new(),
            cs_name_map: HashMap::new(),
            icc_color_spaces: Vec::new(),
            icc_cs_name_map: HashMap::new(),
            pattern_refs: Vec::new(),
            pattern_map: HashMap::new(),
            pattern_cs_names: Vec::new(),
            pattern_cs_set: HashSet::new(),
            transfer_refs: Vec::new(),
            halftone_refs: Vec::new(),
            bg_ucr_refs: Vec::new(),
            soft_mask_refs: Vec::new(),
            ocg_marker_refs: Vec::new(),
            form_xobjects: Vec::new(),
            page_font_names: HashSet::new(),
            has_text_elements,
            page_w,
            page_h,
            in_tile,
            font_tracker,
        }
    }

    /// Walk a DisplayList and emit each element. May recurse via the
    /// Group / SoftMasked / OcgGroup arms.
    fn emit_list<'a>(&mut self, list: &'a DisplayList) {
        let mut text_batch: Vec<&'a TextParams> = Vec::new();
        let mut batch_font: Option<u32> = None;
        for element in list.elements() {
            self.emit_element(element, &mut text_batch, &mut batch_font);
        }
        flush_text_batch(&text_batch, self.font_tracker, &mut self.buf, &mut self.gs);
    }

    fn emit_element<'a>(
        &mut self,
        element: &'a DisplayElement,
        text_batch: &mut Vec<&'a TextParams>,
        batch_font: &mut Option<u32>,
    ) {
        // Text-batching prelude. Text accumulates into the current
        // batch; non-Text elements flush the batch before processing.
        // Glyph-path Fill/Stroke elements paired with Text in the PS
        // pipeline get skipped without disturbing the batch; the same
        // element shapes from the PDF reader (no Text companion) fall
        // through to the main match.
        match element {
            DisplayElement::Text { params } => {
                if *batch_font == Some(params.font_entity) {
                    text_batch.push(params);
                } else {
                    flush_text_batch(text_batch, self.font_tracker, &mut self.buf, &mut self.gs);
                    text_batch.clear();
                    text_batch.push(params);
                    *batch_font = Some(params.font_entity);
                }
                return;
            }
            DisplayElement::Fill { params, .. }
                if params.is_text_glyph && self.has_text_elements =>
            {
                return;
            }
            DisplayElement::Stroke { params, .. }
                if params.is_text_glyph && self.has_text_elements =>
            {
                return;
            }
            _ => {
                flush_text_batch(text_batch, self.font_tracker, &mut self.buf, &mut self.gs);
                text_batch.clear();
                *batch_font = None;
            }
        }

        match element {
            DisplayElement::ErasePage => {
                if self.in_tile {
                    // Tiles have no page background — skip.
                    return;
                }
                self.buf.extend(b"1 g 0 0 ");
                fmt_num(&mut self.buf, self.page_w as f64);
                self.buf.push(b' ');
                fmt_num(&mut self.buf, self.page_h as f64);
                self.buf.extend(b" re f\n");
                self.gs.fill_color = Some(PdfColor::Gray(10000));
            }
            DisplayElement::Fill { path, params } => {
                if params.is_text_glyph && self.has_text_elements {
                    return;
                }
                emit_transfer(
                    &mut self.buf,
                    &params.transfer,
                    &mut self.gs,
                    &mut self.ext_gstates,
                    &mut self.ext_gstate_map,
                    &mut self.transfer_refs,
                );
                emit_halftone(
                    &mut self.buf,
                    &params.halftone,
                    &mut self.gs,
                    &mut self.ext_gstates,
                    &mut self.ext_gstate_map,
                    &mut self.halftone_refs,
                );
                emit_bg_ucr(
                    &mut self.buf,
                    &params.bg_ucr,
                    &mut self.gs,
                    &mut self.ext_gstates,
                    &mut self.ext_gstate_map,
                    &mut self.bg_ucr_refs,
                );
                emit_rendering_intent(&mut self.buf, params.rendering_intent, &mut self.gs);
                emit_overprint(
                    &mut self.buf,
                    params.overprint,
                    params.overprint_mode,
                    &mut self.gs,
                    &mut self.ext_gstates,
                    &mut self.ext_gstate_map,
                );
                emit_paint_alpha_blend(
                    &mut self.buf,
                    false,
                    params.alpha,
                    params.blend_mode,
                    &mut self.gs,
                    &mut self.ext_gstates,
                    &mut self.ext_gstate_map,
                );
                if let Some(spot) = &params.spot_color {
                    emit_fill_color_spot(
                        &mut self.buf,
                        spot,
                        &mut self.gs,
                        &mut self.cs_name_map,
                        &mut self.color_spaces,
                    );
                } else if let Some(icc) = &params.icc_color {
                    emit_fill_color_icc(
                        &mut self.buf,
                        icc,
                        &mut self.gs,
                        &mut self.icc_cs_name_map,
                        &mut self.icc_color_spaces,
                    );
                } else {
                    if self.gs.fill_cs_name.is_some() {
                        self.gs.fill_cs_name = None;
                        self.gs.fill_color = None;
                    }
                    emit_fill_color(
                        &mut self.buf,
                        &params.color,
                        params.painted_channels,
                        &mut self.gs,
                    );
                }
                emit_path(&mut self.buf, path);
                if params.fill_rule == FillRule::EvenOdd {
                    self.buf.extend(b"f*\n");
                } else {
                    self.buf.extend(b"f\n");
                }
            }
            DisplayElement::Stroke { path, params } => {
                if params.is_text_glyph && self.has_text_elements {
                    return;
                }
                let has_ctm = !is_identity(&params.ctm);
                // PDF `q` saves the gstate and `Q` restores it. Mirror that
                // in our tracker by snapshotting `self.gs` before `q` and
                // restoring after `Q` — otherwise state changes we emit
                // inside the q/Q block (CTM, color, line, overprint, …)
                // would leak past the `Q` in the tracker's view, making
                // the next paint think e.g. overprint is still on when
                // PDF has already popped it.
                let saved_gs = if has_ctm {
                    let saved = self.gs.clone();
                    self.buf.extend(b"q\n");
                    emit_cm(&mut self.buf, &params.ctm);
                    Some(saved)
                } else {
                    None
                };
                emit_transfer(
                    &mut self.buf,
                    &params.transfer,
                    &mut self.gs,
                    &mut self.ext_gstates,
                    &mut self.ext_gstate_map,
                    &mut self.transfer_refs,
                );
                emit_halftone(
                    &mut self.buf,
                    &params.halftone,
                    &mut self.gs,
                    &mut self.ext_gstates,
                    &mut self.ext_gstate_map,
                    &mut self.halftone_refs,
                );
                emit_bg_ucr(
                    &mut self.buf,
                    &params.bg_ucr,
                    &mut self.gs,
                    &mut self.ext_gstates,
                    &mut self.ext_gstate_map,
                    &mut self.bg_ucr_refs,
                );
                emit_rendering_intent(&mut self.buf, params.rendering_intent, &mut self.gs);
                emit_overprint(
                    &mut self.buf,
                    params.overprint,
                    params.overprint_mode,
                    &mut self.gs,
                    &mut self.ext_gstates,
                    &mut self.ext_gstate_map,
                );
                emit_paint_alpha_blend(
                    &mut self.buf,
                    true,
                    params.alpha,
                    params.blend_mode,
                    &mut self.gs,
                    &mut self.ext_gstates,
                    &mut self.ext_gstate_map,
                );
                if let Some(spot) = &params.spot_color {
                    emit_stroke_color_spot(
                        &mut self.buf,
                        spot,
                        &mut self.gs,
                        &mut self.cs_name_map,
                        &mut self.color_spaces,
                    );
                } else if let Some(icc) = &params.icc_color {
                    emit_stroke_color_icc(
                        &mut self.buf,
                        icc,
                        &mut self.gs,
                        &mut self.icc_cs_name_map,
                        &mut self.icc_color_spaces,
                    );
                } else {
                    if self.gs.stroke_cs_name.is_some() {
                        self.gs.stroke_cs_name = None;
                        self.gs.stroke_color = None;
                    }
                    emit_stroke_color(
                        &mut self.buf,
                        &params.color,
                        params.painted_channels,
                        &mut self.gs,
                    );
                }
                emit_line_state(&mut self.buf, params, &mut self.gs);
                emit_path(&mut self.buf, path);
                self.buf.extend(b"S\n");
                if let Some(saved) = saved_gs {
                    self.buf.extend(b"Q\n");
                    self.gs = saved;
                }
            }
            DisplayElement::Clip { path, params } => {
                // Replay detection: the reader emits one Clip per still-
                // active clip after every Q (see `restore_clip_from_stack`
                // in stet-pdf-reader). These are state descriptions, not
                // new clips. Consume them silently.
                if let Some(expected) = self.pending_replay_clips.front()
                    && paths_equal(expected, path)
                {
                    self.pending_replay_clips.pop_front();
                    return;
                }
                // Unmatched at this point — discard the rest of the
                // queue so subsequent real clips don't get accidentally
                // dropped.
                self.pending_replay_clips.clear();
                // Open a fresh q scope only when none is currently open;
                // additional clips go into the existing scope by emitting
                // just `path W n`. PDF source streams routinely chain
                // multiple `W n` operations in a single `q ... Q` block
                // (e.g. `q W(A) W(B) Q`), and mirroring that here keeps
                // the round-trip stream-shape-stable.
                if self.clip_scopes.is_empty() {
                    self.clip_gs_stack.push(self.gs.clone());
                    self.buf.extend(b"q\n");
                    self.clip_depth += 1;
                    self.clip_scopes.push(Vec::new());
                }
                emit_path(&mut self.buf, path);
                if params.fill_rule == FillRule::EvenOdd {
                    self.buf.extend(b"W* n\n");
                } else {
                    self.buf.extend(b"W n\n");
                }
                if let Some(scope) = self.clip_scopes.last_mut() {
                    scope.push(path.clone());
                }
            }
            DisplayElement::InitClip => {
                if let Some(_popped) = self.clip_scopes.pop() {
                    self.buf.extend(b"Q\n");
                    if let Some(prev) = self.clip_gs_stack.pop() {
                        self.gs = prev;
                    } else {
                        self.gs.reset();
                    }
                    if self.clip_depth > 0 {
                        self.clip_depth -= 1;
                    }
                }
                // Seed the replay queue with all clips still active
                // (every path in every remaining open scope). The
                // reader's restore_clip_from_stack emits one Clip per
                // entry in `clip_stack` (outermost first); the next K
                // Clip arms consume those.
                self.pending_replay_clips = self
                    .clip_scopes
                    .iter()
                    .flat_map(|s| s.iter().cloned())
                    .collect();
            }
            DisplayElement::Image {
                sample_data,
                params,
            } => {
                let img_idx = self.images.len();
                let xobj = image_ops::convert_image(sample_data, params);
                let m = compute_image_matrix(params);
                let pre_q_gs = self.gs.clone();
                self.buf.extend(b"q ");
                emit_matrix(&mut self.buf, &m);
                self.buf.extend(b" cm\n");
                emit_overprint(
                    &mut self.buf,
                    params.overprint,
                    params.overprint_mode,
                    &mut self.gs,
                    &mut self.ext_gstates,
                    &mut self.ext_gstate_map,
                );
                emit_paint_alpha_blend(
                    &mut self.buf,
                    false,
                    params.alpha,
                    params.blend_mode,
                    &mut self.gs,
                    &mut self.ext_gstates,
                    &mut self.ext_gstate_map,
                );
                if xobj.is_imagemask
                    && let Some(ref mask_color) = xobj.mask_color
                {
                    // Emit the imagemask's fill color through the same
                    // DeviceColor-aware path the regular Fill arm uses, so a
                    // CMYK source paint (native_cmyk = Some) round-trips as
                    // `c m y k k` rather than getting downconverted to
                    // `r g b rg`. Without this the imagemask's color reads
                    // back as DeviceRGB and OPM-1 overprint with a CMYK
                    // backdrop misbehaves (visible on PDFX-Output-Test
                    // GWG 2.0 image / mask cells).
                    if self.gs.fill_cs_name.is_some() {
                        self.gs.fill_cs_name = None;
                        self.gs.fill_color = None;
                    }
                    emit_fill_color(
                        &mut self.buf,
                        mask_color,
                        params.painted_channels,
                        &mut self.gs,
                    );
                }
                writeln!(self.buf, "/Im{} Do Q", img_idx).unwrap();
                self.gs = pre_q_gs;
                self.images.push(xobj);
            }
            DisplayElement::AxialShading { params } => {
                let sh_idx = self.shading_refs.len();
                let pre_q_gs = self.gs.clone();
                self.buf.extend(b"q\n");
                if !is_identity(&params.ctm) {
                    emit_matrix(&mut self.buf, &params.ctm);
                    self.buf.extend(b" cm\n");
                }
                emit_overprint(
                    &mut self.buf,
                    params.overprint,
                    0,
                    &mut self.gs,
                    &mut self.ext_gstates,
                    &mut self.ext_gstate_map,
                );
                emit_paint_alpha_blend(
                    &mut self.buf,
                    false,
                    params.alpha,
                    params.blend_mode,
                    &mut self.gs,
                    &mut self.ext_gstates,
                    &mut self.ext_gstate_map,
                );
                writeln!(self.buf, "/Sh{} sh Q", sh_idx).unwrap();
                self.gs = pre_q_gs;
                self.shading_refs.push(ShadingRef::Axial(params.clone()));
            }
            DisplayElement::RadialShading { params } => {
                let sh_idx = self.shading_refs.len();
                let pre_q_gs = self.gs.clone();
                self.buf.extend(b"q\n");
                if !is_identity(&params.ctm) {
                    emit_matrix(&mut self.buf, &params.ctm);
                    self.buf.extend(b" cm\n");
                }
                emit_overprint(
                    &mut self.buf,
                    params.overprint,
                    0,
                    &mut self.gs,
                    &mut self.ext_gstates,
                    &mut self.ext_gstate_map,
                );
                emit_paint_alpha_blend(
                    &mut self.buf,
                    false,
                    params.alpha,
                    params.blend_mode,
                    &mut self.gs,
                    &mut self.ext_gstates,
                    &mut self.ext_gstate_map,
                );
                writeln!(self.buf, "/Sh{} sh Q", sh_idx).unwrap();
                self.gs = pre_q_gs;
                self.shading_refs.push(ShadingRef::Radial(params.clone()));
            }
            DisplayElement::MeshShading { params } => {
                let sh_idx = self.shading_refs.len();
                let pre_q_gs = self.gs.clone();
                self.buf.extend(b"q\n");
                if !is_identity(&params.ctm) {
                    emit_matrix(&mut self.buf, &params.ctm);
                    self.buf.extend(b" cm\n");
                }
                emit_overprint(
                    &mut self.buf,
                    params.overprint,
                    0,
                    &mut self.gs,
                    &mut self.ext_gstates,
                    &mut self.ext_gstate_map,
                );
                emit_paint_alpha_blend(
                    &mut self.buf,
                    false,
                    params.alpha,
                    params.blend_mode,
                    &mut self.gs,
                    &mut self.ext_gstates,
                    &mut self.ext_gstate_map,
                );
                writeln!(self.buf, "/Sh{} sh Q", sh_idx).unwrap();
                self.gs = pre_q_gs;
                self.shading_refs.push(ShadingRef::Mesh(params.clone()));
            }
            DisplayElement::PatchShading { params } => {
                let sh_idx = self.shading_refs.len();
                let pre_q_gs = self.gs.clone();
                self.buf.extend(b"q\n");
                if !is_identity(&params.ctm) {
                    emit_matrix(&mut self.buf, &params.ctm);
                    self.buf.extend(b" cm\n");
                }
                emit_overprint(
                    &mut self.buf,
                    params.overprint,
                    0,
                    &mut self.gs,
                    &mut self.ext_gstates,
                    &mut self.ext_gstate_map,
                );
                emit_paint_alpha_blend(
                    &mut self.buf,
                    false,
                    params.alpha,
                    params.blend_mode,
                    &mut self.gs,
                    &mut self.ext_gstates,
                    &mut self.ext_gstate_map,
                );
                writeln!(self.buf, "/Sh{} sh Q", sh_idx).unwrap();
                self.gs = pre_q_gs;
                self.shading_refs.push(ShadingRef::Patch(params.clone()));
            }
            DisplayElement::PatternFill { params } => {
                emit_pattern_fill(
                    &mut self.buf,
                    params,
                    &mut self.gs,
                    &mut self.pattern_refs,
                    &mut self.pattern_map,
                    &mut self.pattern_cs_names,
                    &mut self.pattern_cs_set,
                );
            }
            DisplayElement::Text { .. } => unreachable!(), // handled in prelude
            DisplayElement::Group { elements, params } => {
                // Build the Form XObject's content stream by swapping in
                // a fresh buffer and recursing. Resources (images, fonts,
                // ext_gstates, …) stay on the page-level Builder; the
                // Form inherits the page's /Resources per PDF 1.7
                // § 7.8.3, so we never have to duplicate them.
                let saved_buf = std::mem::take(&mut self.buf);
                let saved_gs = std::mem::replace(&mut self.gs, GState::new());
                let saved_clip_depth = std::mem::replace(&mut self.clip_depth, 0);
                let saved_clip_gs_stack = std::mem::take(&mut self.clip_gs_stack);

                self.emit_list(elements);

                // Close any clips still open at the end of the form's
                // content stream — the implicit Q after the form's body
                // would balance them out anyway, but explicit close keeps
                // the stream well-formed under viewer inspection.
                for _ in 0..self.clip_depth {
                    self.buf.extend(b"Q\n");
                }

                let form_content = std::mem::replace(&mut self.buf, saved_buf);
                self.gs = saved_gs;
                self.clip_depth = saved_clip_depth;
                self.clip_gs_stack = saved_clip_gs_stack;

                let form_idx = self.form_xobjects.len();
                let form_name = format!("X{}", form_idx);

                let mut group_entries: Vec<(Vec<u8>, PdfObj)> = vec![
                    (b"Type".to_vec(), PdfObj::name("Group")),
                    (b"S".to_vec(), PdfObj::name("Transparency")),
                ];
                if params.isolated {
                    group_entries.push((b"I".to_vec(), PdfObj::Bool(true)));
                }
                if params.knockout {
                    group_entries.push((b"K".to_vec(), PdfObj::Bool(true)));
                }
                let cs_name: Option<&str> = match params.color_space {
                    stet_graphics::display_list::GroupColorSpace::DeviceGray => Some("DeviceGray"),
                    stet_graphics::display_list::GroupColorSpace::DeviceRGB => Some("DeviceRGB"),
                    stet_graphics::display_list::GroupColorSpace::DeviceCMYK => Some("DeviceCMYK"),
                    stet_graphics::display_list::GroupColorSpace::Inherited => None,
                };
                if let Some(name) = cs_name {
                    group_entries.push((b"CS".to_vec(), PdfObj::name(name)));
                }

                self.form_xobjects.push(FormXObject {
                    content: form_content,
                    bbox: params.bbox,
                    group_dict_entries: Some(group_entries),
                });

                // Invoke the Form on the parent stream. Wrap in q/Q so
                // any /CA/ca/BM ExtGState we push is local to this /Do.
                // Snapshot and restore the gs tracker around the q/Q so
                // the alpha/blend pushed inside (and any state the Form
                // changed and tried to leak) doesn't fool the tracker
                // into thinking PDF state changed past the `Q`.
                let pre_q_gs = self.gs.clone();
                self.buf.extend(b"q\n");
                if params.alpha < 1.0 || params.blend_mode != 0 {
                    emit_group_composite_gs(
                        &mut self.buf,
                        params.alpha,
                        params.blend_mode,
                        &mut self.ext_gstates,
                        &mut self.ext_gstate_map,
                    );
                }
                writeln!(self.buf, "/{} Do", form_name).unwrap();
                self.buf.extend(b"Q\n");
                self.gs = pre_q_gs;
            }
            DisplayElement::SoftMasked {
                mask,
                content,
                params,
                ..
            } => {
                // Mask → Form XObject (transparency group, /CS DeviceGray
                // so luminosity extraction works on Luminosity SMasks; the
                // CS choice is harmless for Alpha SMasks since only the
                // alpha channel is read).
                let saved_buf = std::mem::take(&mut self.buf);
                let saved_gs = std::mem::replace(&mut self.gs, GState::new());
                let saved_clip_depth = std::mem::replace(&mut self.clip_depth, 0);
                let saved_clip_gs_stack = std::mem::take(&mut self.clip_gs_stack);

                self.emit_list(mask);

                for _ in 0..self.clip_depth {
                    self.buf.extend(b"Q\n");
                }

                let mask_content = std::mem::replace(&mut self.buf, saved_buf);
                self.gs = saved_gs;
                self.clip_depth = saved_clip_depth;
                self.clip_gs_stack = saved_clip_gs_stack;

                let mask_form_idx = self.form_xobjects.len();
                let mask_group_entries: Vec<(Vec<u8>, PdfObj)> = vec![
                    (b"Type".to_vec(), PdfObj::name("Group")),
                    (b"S".to_vec(), PdfObj::name("Transparency")),
                    (b"CS".to_vec(), PdfObj::name("DeviceGray")),
                ];
                self.form_xobjects.push(FormXObject {
                    content: mask_content,
                    bbox: params.bbox,
                    group_dict_entries: Some(mask_group_entries),
                });

                // Placeholder ExtGState — pdf_device.rs patches in the
                // real /SMask entry once the mask form's indirect ref
                // exists.
                let ext_gstate_idx = self.ext_gstates.len();
                self.ext_gstates.push(ExtGStateDict {
                    entries: vec![(b"Type".to_vec(), PdfObj::name("ExtGState"))],
                });
                self.soft_mask_refs.push(SoftMaskRef {
                    ext_gstate_idx,
                    mask_form_idx,
                    subtype: params.subtype.clone(),
                    backdrop_color: params.backdrop_color,
                    transfer_invert: params.transfer_invert,
                });

                // Scope: q + /GSn gs (push SMask) + content + Q (pops the
                // SMask along with the rest of the gstate). After the
                // outer Q, the gstate tracker has to return to whatever
                // PDF state existed before the q — snapshot it.
                let pre_q_gs = self.gs.clone();
                self.buf.extend(b"q\n");
                writeln!(self.buf, "/GS{} gs", ext_gstate_idx).unwrap();
                self.gs = GState::new();
                let saved_clip_depth = std::mem::replace(&mut self.clip_depth, 0);
                let saved_clip_gs_stack = std::mem::take(&mut self.clip_gs_stack);

                self.emit_list(content);

                for _ in 0..self.clip_depth {
                    self.buf.extend(b"Q\n");
                }

                self.clip_depth = saved_clip_depth;
                self.clip_gs_stack = saved_clip_gs_stack;
                self.buf.extend(b"Q\n");
                self.gs = pre_q_gs;
            }
            DisplayElement::OcgGroup {
                elements,
                visibility,
            } => {
                // Optional-content marker. The visibility predicate is
                // resolved into an /OCG or /OCMD indirect object by
                // pdf_device.rs; here we just allocate the resource
                // name, emit the BDC/EMC bracket inline, and recurse
                // into the children. Skipping the marker for an empty
                // child list would still pollute /OCProperties, so we
                // record the OcgMarkerRef even when `elements` is empty.
                let resource_name = format!("P{}", self.ocg_marker_refs.len());
                self.ocg_marker_refs.push(OcgMarkerRef {
                    resource_name: resource_name.clone(),
                    visibility: visibility.clone(),
                });
                writeln!(self.buf, "/OC /{} BDC", resource_name).unwrap();
                self.emit_list(elements);
                self.buf.extend(b"EMC\n");
            }
            _ => {}
        }
    }

    fn finish(self) -> ContentStreamResult {
        ContentStreamResult {
            content: self.buf,
            images: self.images,
            shading_refs: self.shading_refs,
            used_font_names: self.page_font_names.into_iter().collect(),
            ext_gstate_dicts: self.ext_gstates,
            color_spaces: self.color_spaces,
            icc_color_spaces: self.icc_color_spaces,
            pattern_refs: self.pattern_refs,
            pattern_cs_entries: self.pattern_cs_names,
            transfer_refs: self.transfer_refs,
            halftone_refs: self.halftone_refs,
            bg_ucr_refs: self.bg_ucr_refs,
            soft_mask_refs: self.soft_mask_refs,
            ocg_marker_refs: self.ocg_marker_refs,
            form_xobjects: self.form_xobjects,
        }
    }
}

/// PDF blend-mode name for `GroupParams::blend_mode` u8 codes. These
/// match the encoding used by `stet-graphics`'s `BlendMode`.
fn blend_mode_name(code: u8) -> &'static [u8] {
    match code {
        0 => b"Normal",
        1 => b"Multiply",
        2 => b"Screen",
        3 => b"Overlay",
        4 => b"Darken",
        5 => b"Lighten",
        6 => b"ColorDodge",
        7 => b"ColorBurn",
        8 => b"HardLight",
        9 => b"SoftLight",
        10 => b"Difference",
        11 => b"Exclusion",
        12 => b"Hue",
        13 => b"Saturation",
        14 => b"Color",
        15 => b"Luminosity",
        _ => b"Normal",
    }
}

/// Emit a `/GSn gs` setting `/ca` (non-stroking alpha) or `/CA`
/// (stroking alpha), and `/BM` when the blend mode changes. Used by
/// the Fill and Stroke arms so per-paint transparency on the
/// DisplayList round-trips into the output PDF. Deduplicates so
/// identical (paint-side, alpha, blend) combos share one ExtGState.
fn emit_paint_alpha_blend(
    buf: &mut Vec<u8>,
    is_stroke: bool,
    alpha: f64,
    blend_mode: u8,
    gs: &mut GState,
    ext_gstates: &mut Vec<ExtGStateDict>,
    ext_gstate_map: &mut HashMap<Vec<u8>, usize>,
) {
    let cached_alpha = if is_stroke {
        gs.stroke_alpha
    } else {
        gs.fill_alpha
    };
    if (cached_alpha - alpha).abs() < 1e-6 && gs.blend_mode == blend_mode {
        return;
    }
    if is_stroke {
        gs.stroke_alpha = alpha;
    } else {
        gs.fill_alpha = alpha;
    }
    gs.blend_mode = blend_mode;

    let alpha_q = (alpha.clamp(0.0, 1.0) * 10000.0) as u16;
    let kind = if is_stroke { b'S' } else { b'F' };
    let key = format!("PA-{}-a{}-b{}", kind as char, alpha_q, blend_mode).into_bytes();

    let idx = if let Some(&idx) = ext_gstate_map.get(&key) {
        idx
    } else {
        let idx = ext_gstates.len();
        let mut entries: Vec<(Vec<u8>, PdfObj)> =
            vec![(b"Type".to_vec(), PdfObj::name("ExtGState"))];
        if is_stroke {
            entries.push((b"CA".to_vec(), PdfObj::Real(alpha)));
        } else {
            entries.push((b"ca".to_vec(), PdfObj::Real(alpha)));
        }
        if blend_mode != 0 {
            entries.push((
                b"BM".to_vec(),
                PdfObj::Name(blend_mode_name(blend_mode).to_vec()),
            ));
        } else {
            // Normal explicitly so re-pushing the same dict doesn't
            // inherit a stale BM from a previous gstate.
            entries.push((b"BM".to_vec(), PdfObj::name("Normal")));
        }
        ext_gstates.push(ExtGStateDict { entries });
        ext_gstate_map.insert(key, idx);
        idx
    };
    writeln!(buf, "/GS{} gs", idx).unwrap();
}

/// Emit a `/GSn gs` setting `/CA`, `/ca`, and `/BM` for a transparency
/// group's composite. Used before a `/Xn Do` referencing a Form XObject
/// when alpha != 1.0 or the blend mode is non-Normal. Deduplicates so
/// identical group composites share one ExtGState resource.
fn emit_group_composite_gs(
    buf: &mut Vec<u8>,
    alpha: f64,
    blend_mode: u8,
    ext_gstates: &mut Vec<ExtGStateDict>,
    ext_gstate_map: &mut HashMap<Vec<u8>, usize>,
) {
    let alpha_q = (alpha.clamp(0.0, 1.0) * 10000.0) as u16;
    let key = format!("GRP-a{}-b{}", alpha_q, blend_mode).into_bytes();

    let idx = if let Some(&idx) = ext_gstate_map.get(&key) {
        idx
    } else {
        let idx = ext_gstates.len();
        let mut entries: Vec<(Vec<u8>, PdfObj)> =
            vec![(b"Type".to_vec(), PdfObj::name("ExtGState"))];
        if alpha < 1.0 {
            entries.push((b"CA".to_vec(), PdfObj::Real(alpha)));
            entries.push((b"ca".to_vec(), PdfObj::Real(alpha)));
        }
        if blend_mode != 0 {
            entries.push((
                b"BM".to_vec(),
                PdfObj::Name(blend_mode_name(blend_mode).to_vec()),
            ));
        }
        ext_gstates.push(ExtGStateDict { entries });
        ext_gstate_map.insert(key, idx);
        idx
    };
    writeln!(buf, "/GS{} gs", idx).unwrap();
}

/// Pre-pass: register fonts referenced by Text elements and detect
/// whether the list has any Text at all. The page-side path emits paired
/// `DisplayElement::Text` + glyph-path `Fill`; the PDF reader path emits
/// only the glyph-path Fill. When the list has no Text, glyph fills fall
/// through to be emitted as filled paths instead of being skipped.
fn scan_text_elements(
    list: &DisplayList,
    font_tracker: &mut FontTracker,
    page_font_names: &mut HashSet<String>,
) -> bool {
    let mut has_text = false;
    for element in list.elements() {
        if let DisplayElement::Text { params } = element {
            has_text = true;
            let name = font_tracker.track(params).to_string();
            page_font_names.insert(name);
        }
    }
    has_text
}

/// Generate PDF content stream bytes from a display list.
///
/// Uses a shared document-level `FontTracker` to register fonts across pages.
/// When `ctx` is available, pre-computes font widths for TJ kern values
/// and batches consecutive same-font text elements into single BT/ET blocks.
pub fn build_content_stream(
    list: &DisplayList,
    page_w: u32,
    page_h: u32,
    dpi: f64,
    ctx: Option<&Context>,
    font_tracker: &mut FontTracker,
) -> ContentStreamResult {
    let scale = 72.0 / dpi;
    let page_h_pts = page_h as f64 * scale;

    let mut page_font_names: HashSet<String> = HashSet::new();
    let has_text_elements = scan_text_elements(list, font_tracker, &mut page_font_names);

    // Pre-compute glyph widths for TJ kern values when Context is available
    if let Some(c) = ctx {
        for usage in font_tracker.fonts_mut() {
            if usage.widths.is_empty() {
                usage.widths = font_embedder::extract_widths(usage, c);
            }
        }
    }

    let mut builder = Builder::new(font_tracker, page_w, page_h, has_text_elements, false);
    builder.page_font_names = page_font_names;

    // Initial CTM: device space (Y-down, pixels) → PDF space (Y-up, points)
    fmt_num(&mut builder.buf, scale);
    builder.buf.extend(b" 0 0 ");
    fmt_num(&mut builder.buf, -scale);
    builder.buf.extend(b" 0 ");
    fmt_num(&mut builder.buf, page_h_pts);
    builder.buf.extend(b" cm\n");

    // Clip to page bounds (device coordinates). The rasterizer implicitly
    // clips to the pixmap, but PDF has no implicit page clip.
    builder.buf.extend(b"0 0 ");
    fmt_num(&mut builder.buf, page_w as f64);
    builder.buf.push(b' ');
    fmt_num(&mut builder.buf, page_h as f64);
    builder.buf.extend(b" re W n\n");

    builder.emit_list(list);

    // Close remaining clips left open at the end of the list.
    for _ in 0..builder.clip_depth {
        builder.buf.extend(b"Q\n");
    }

    // Transform pattern matrices from device pixel space → PDF initial
    // coordinate space. The content stream's initial `cm` maps device
    // pixels → PDF points. The PDF spec says Pattern /Matrix maps pattern
    // space → the initial (pre-cm) coordinate system. So: pdf_matrix =
    // pattern_matrix × initial_cm (row-vector convention).
    let initial_cm = Matrix::new(scale, 0.0, 0.0, -scale, 0.0, page_h_pts);
    for pat_ref in &mut builder.pattern_refs {
        pat_ref.pattern_matrix = initial_cm.concat(&pat_ref.pattern_matrix);
    }

    builder.finish()
}

/// Generate PDF content stream bytes from a tile display list (for Pattern XObjects).
///
/// Unlike `build_content_stream`, this emits no initial CTM or page clip —
/// tile coordinates are already in pattern space.
pub fn build_tile_content_stream(
    list: &DisplayList,
    font_tracker: &mut FontTracker,
) -> ContentStreamResult {
    let mut page_font_names: HashSet<String> = HashSet::new();
    let has_text_elements = scan_text_elements(list, font_tracker, &mut page_font_names);

    let mut builder = Builder::new(font_tracker, 0, 0, has_text_elements, true);
    builder.page_font_names = page_font_names;

    builder.emit_list(list);

    for _ in 0..builder.clip_depth {
        builder.buf.extend(b"Q\n");
    }

    builder.finish()
}

/// Flush accumulated text batch as optimized BT/ET blocks.
fn flush_text_batch(
    batch: &[&TextParams],
    font_tracker: &FontTracker,
    buf: &mut Vec<u8>,
    gs: &mut GState,
) {
    if batch.is_empty() {
        return;
    }
    text_ops::emit_text_batch(buf, batch, font_tracker);
    // Text blocks emit color operators (g/rg/k) that change the PDF's current
    // color space. Reset all color tracking to force re-emission.
    gs.fill_color = None;
    gs.fill_cs_name = None;
    gs.stroke_color = None;
    gs.stroke_cs_name = None;
}

/// Emit a non-stroking (fill) color command.
fn emit_fill_color(buf: &mut Vec<u8>, color: &DeviceColor, painted_channels: u8, gs: &mut GState) {
    let pc = color_to_pdf_with_channels(color, painted_channels);
    if gs.fill_color.as_ref() == Some(&pc) {
        return;
    }
    match &pc {
        PdfColor::Cmyk(c, m, y, k) => {
            fmt_num(buf, *c as f64 / 10000.0);
            buf.push(b' ');
            fmt_num(buf, *m as f64 / 10000.0);
            buf.push(b' ');
            fmt_num(buf, *y as f64 / 10000.0);
            buf.push(b' ');
            fmt_num(buf, *k as f64 / 10000.0);
            buf.extend(b" k\n");
        }
        PdfColor::Gray(g) => {
            fmt_num(buf, *g as f64 / 10000.0);
            buf.extend(b" g\n");
        }
        PdfColor::Rgb(r, g, b) => {
            fmt_num(buf, *r as f64 / 10000.0);
            buf.push(b' ');
            fmt_num(buf, *g as f64 / 10000.0);
            buf.push(b' ');
            fmt_num(buf, *b as f64 / 10000.0);
            buf.extend(b" rg\n");
        }
    }
    gs.fill_color = Some(pc);
}

/// Emit a non-stroking RGB color (for imagemask fill color).
fn emit_fill_color_rgb(buf: &mut Vec<u8>, r: f64, g: f64, b: f64) {
    fmt_num(buf, r);
    buf.push(b' ');
    fmt_num(buf, g);
    buf.push(b' ');
    fmt_num(buf, b);
    buf.extend(b" rg ");
}

/// Emit a stroking color command.
fn emit_stroke_color(
    buf: &mut Vec<u8>,
    color: &DeviceColor,
    painted_channels: u8,
    gs: &mut GState,
) {
    let pc = color_to_pdf_with_channels(color, painted_channels);
    if gs.stroke_color.as_ref() == Some(&pc) {
        return;
    }
    match &pc {
        PdfColor::Cmyk(c, m, y, k) => {
            fmt_num(buf, *c as f64 / 10000.0);
            buf.push(b' ');
            fmt_num(buf, *m as f64 / 10000.0);
            buf.push(b' ');
            fmt_num(buf, *y as f64 / 10000.0);
            buf.push(b' ');
            fmt_num(buf, *k as f64 / 10000.0);
            buf.extend(b" K\n");
        }
        PdfColor::Gray(g) => {
            fmt_num(buf, *g as f64 / 10000.0);
            buf.extend(b" G\n");
        }
        PdfColor::Rgb(r, g, b) => {
            fmt_num(buf, *r as f64 / 10000.0);
            buf.push(b' ');
            fmt_num(buf, *g as f64 / 10000.0);
            buf.push(b' ');
            fmt_num(buf, *b as f64 / 10000.0);
            buf.extend(b" RG\n");
        }
    }
    gs.stroke_color = Some(pc);
}

/// Compute a dedup key for a SpotColorSpace.
fn spot_cs_key(spot: &SpotColor) -> Vec<u8> {
    match &spot.color_space {
        SpotColorSpace::Separation { name, .. } => {
            let mut key = b"Sep:".to_vec();
            key.extend(name);
            key
        }
        SpotColorSpace::DeviceN { names, .. } => {
            let mut key = b"DN:".to_vec();
            let mut sorted: Vec<&Vec<u8>> = names.iter().collect();
            sorted.sort();
            for (i, n) in sorted.iter().enumerate() {
                if i > 0 {
                    key.push(b',');
                }
                key.extend(*n);
            }
            key
        }
        _ => b"unknown".to_vec(),
    }
}

/// Get or create a color space resource name for a spot color.
fn get_or_create_cs_name(
    spot: &SpotColor,
    cs_map: &mut HashMap<Vec<u8>, String>,
    color_spaces: &mut Vec<(String, SpotColorSpace)>,
) -> String {
    let key = spot_cs_key(spot);
    if let Some(name) = cs_map.get(&key) {
        return name.clone();
    }
    let name = format!("CS{}", color_spaces.len());
    cs_map.insert(key, name.clone());
    color_spaces.push((name.clone(), spot.color_space.clone()));
    name
}

/// Emit a non-stroking Separation/DeviceN color: `/CSn cs` + `tint scn`.
fn emit_fill_color_spot(
    buf: &mut Vec<u8>,
    spot: &SpotColor,
    gs: &mut GState,
    cs_map: &mut HashMap<Vec<u8>, String>,
    color_spaces: &mut Vec<(String, SpotColorSpace)>,
) {
    let cs_name = get_or_create_cs_name(spot, cs_map, color_spaces);
    if gs.fill_cs_name.as_deref() != Some(&cs_name) {
        writeln!(buf, "/{} cs", cs_name).unwrap();
        gs.fill_cs_name = Some(cs_name);
        gs.fill_color = None; // force re-emit tint values
    }
    for v in &spot.tint_values {
        fmt_num(buf, *v);
        buf.push(b' ');
    }
    buf.extend(b"scn\n");
}

/// Emit a stroking Separation/DeviceN color: `/CSn CS` + `tint SCN`.
fn emit_stroke_color_spot(
    buf: &mut Vec<u8>,
    spot: &SpotColor,
    gs: &mut GState,
    cs_map: &mut HashMap<Vec<u8>, String>,
    color_spaces: &mut Vec<(String, SpotColorSpace)>,
) {
    let cs_name = get_or_create_cs_name(spot, cs_map, color_spaces);
    if gs.stroke_cs_name.as_deref() != Some(&cs_name) {
        writeln!(buf, "/{} CS", cs_name).unwrap();
        gs.stroke_cs_name = Some(cs_name);
        gs.stroke_color = None;
    }
    for v in &spot.tint_values {
        fmt_num(buf, *v);
        buf.push(b' ');
    }
    buf.extend(b"SCN\n");
}

/// Get or create a color space resource name for an ICCBased color,
/// deduplicating by profile hash so identical profiles share one
/// `/ICCBased` stream in the output.
fn get_or_create_icc_cs_name(
    icc: &stet_graphics::device::IccColor,
    cs_map: &mut HashMap<Vec<u8>, String>,
    icc_color_spaces: &mut Vec<(String, stet_graphics::device::IccColorSpace)>,
) -> String {
    let key = icc.color_space.profile_hash.to_vec();
    if let Some(name) = cs_map.get(&key) {
        return name.clone();
    }
    let name = format!("ICC{}", icc_color_spaces.len());
    cs_map.insert(key, name.clone());
    icc_color_spaces.push((name.clone(), icc.color_space.clone()));
    name
}

/// Emit a non-stroking ICCBased color: `/ICCn cs` + `c1 c2 c3 scn`.
fn emit_fill_color_icc(
    buf: &mut Vec<u8>,
    icc: &stet_graphics::device::IccColor,
    gs: &mut GState,
    icc_cs_map: &mut HashMap<Vec<u8>, String>,
    icc_color_spaces: &mut Vec<(String, stet_graphics::device::IccColorSpace)>,
) {
    let cs_name = get_or_create_icc_cs_name(icc, icc_cs_map, icc_color_spaces);
    if gs.fill_cs_name.as_deref() != Some(&cs_name) {
        writeln!(buf, "/{} cs", cs_name).unwrap();
        gs.fill_cs_name = Some(cs_name);
        gs.fill_color = None; // force re-emit component values
    }
    for v in &icc.components {
        fmt_num(buf, *v);
        buf.push(b' ');
    }
    buf.extend(b"scn\n");
}

/// Emit a stroking ICCBased color: `/ICCn CS` + `c1 c2 c3 SCN`.
fn emit_stroke_color_icc(
    buf: &mut Vec<u8>,
    icc: &stet_graphics::device::IccColor,
    gs: &mut GState,
    icc_cs_map: &mut HashMap<Vec<u8>, String>,
    icc_color_spaces: &mut Vec<(String, stet_graphics::device::IccColorSpace)>,
) {
    let cs_name = get_or_create_icc_cs_name(icc, icc_cs_map, icc_color_spaces);
    if gs.stroke_cs_name.as_deref() != Some(&cs_name) {
        writeln!(buf, "/{} CS", cs_name).unwrap();
        gs.stroke_cs_name = Some(cs_name);
        gs.stroke_color = None;
    }
    for v in &icc.components {
        fmt_num(buf, *v);
        buf.push(b' ');
    }
    buf.extend(b"SCN\n");
}

/// Emit line state commands (width, cap, join, miter limit, dash).
fn emit_line_state(buf: &mut Vec<u8>, params: &StrokeParams, gs: &mut GState) {
    if gs.line_width != params.line_width {
        fmt_num(buf, params.line_width);
        buf.extend(b" w\n");
        gs.line_width = params.line_width;
    }

    let lc = match params.line_cap {
        LineCap::Butt => 0,
        LineCap::Round => 1,
        LineCap::Square => 2,
        _ => 0,
    };
    if gs.line_cap != lc {
        writeln!(buf, "{} J", lc).unwrap();
        gs.line_cap = lc;
    }

    let lj = match params.line_join {
        LineJoin::Miter => 0,
        LineJoin::Round => 1,
        LineJoin::Bevel => 2,
        _ => 0,
    };
    if gs.line_join != lj {
        writeln!(buf, "{} j", lj).unwrap();
        gs.line_join = lj;
    }

    if gs.miter_limit != params.miter_limit {
        fmt_num(buf, params.miter_limit);
        buf.extend(b" M\n");
        gs.miter_limit = params.miter_limit;
    }

    let dash = &params.dash_pattern;
    if gs.dash_array != dash.array || gs.dash_offset != dash.offset {
        buf.push(b'[');
        for (i, &d) in dash.array.iter().enumerate() {
            if i > 0 {
                buf.push(b' ');
            }
            fmt_num(buf, d);
        }
        buf.extend(b"] ");
        fmt_num(buf, dash.offset);
        buf.extend(b" d\n");
        gs.dash_array.clone_from(&dash.array);
        gs.dash_offset = dash.offset;
    }
}

/// Emit a `gs` operator to set overprint mode when it changes.
///
/// Deduplicates ExtGState dicts — identical overprint settings share one resource.
fn emit_overprint(
    buf: &mut Vec<u8>,
    overprint: bool,
    overprint_mode: i32,
    gs: &mut GState,
    ext_gstates: &mut Vec<ExtGStateDict>,
    ext_gstate_map: &mut HashMap<Vec<u8>, usize>,
) {
    // OPM only affects rendering when overprint is on, but we still
    // round-trip the requested mode so a subsequent overprint=true
    // emission picks up the right value. When overprint is off, the
    // mode entry on the gstate is irrelevant for the next paint.
    let effective_mode = if overprint {
        overprint_mode
    } else {
        gs.overprint_mode
    };
    if gs.overprint == overprint && gs.overprint_mode == effective_mode {
        return;
    }
    gs.overprint = overprint;
    gs.overprint_mode = effective_mode;

    let key = format!("OP{}-M{}", overprint as u8, effective_mode).into_bytes();

    let idx = if let Some(&idx) = ext_gstate_map.get(&key) {
        idx
    } else {
        let idx = ext_gstates.len();
        let mut entries = vec![
            (b"Type".to_vec(), PdfObj::name("ExtGState")),
            (b"OP".to_vec(), PdfObj::Bool(overprint)),
            (b"op".to_vec(), PdfObj::Bool(overprint)),
        ];
        if overprint {
            entries.push((b"OPM".to_vec(), PdfObj::Int(overprint_mode.into())));
        }
        ext_gstates.push(ExtGStateDict { entries });
        ext_gstate_map.insert(key, idx);
        idx
    };

    writeln!(buf, "/GS{} gs", idx).unwrap();
}

/// Emit rendering intent operator if it changed.
fn emit_rendering_intent(buf: &mut Vec<u8>, intent: u8, gs: &mut GState) {
    if gs.rendering_intent == intent {
        return;
    }
    gs.rendering_intent = intent;
    let name = match intent {
        0 => b"RelativeColorimetric" as &[u8],
        1 => b"AbsoluteColorimetric",
        2 => b"Perceptual",
        3 => b"Saturation",
        _ => return,
    };
    buf.push(b'/');
    buf.extend_from_slice(name);
    buf.extend(b" ri\n");
}

/// Build a dedup key from a TransferState based on Arc pointer identity.
fn build_transfer_key(transfer: &TransferState) -> Vec<u8> {
    use std::sync::Arc;
    let mut key = Vec::new();
    if let Some(ref color) = transfer.color {
        key.extend(b"C");
        for table in color {
            if let Some(t) = table {
                let ptr = Arc::as_ptr(t) as usize;
                key.extend(ptr.to_le_bytes());
            } else {
                key.extend(b"I"); // identity
            }
        }
    } else if let Some(ref gray) = transfer.gray {
        key.extend(b"G");
        let ptr = Arc::as_ptr(gray) as usize;
        key.extend(ptr.to_le_bytes());
    }
    // Empty key = identity (no transfer)
    key
}

/// Emit a `gs` operator to set transfer function when it changes.
fn emit_transfer(
    buf: &mut Vec<u8>,
    transfer: &TransferState,
    gs: &mut GState,
    ext_gstates: &mut Vec<ExtGStateDict>,
    ext_gstate_map: &mut HashMap<Vec<u8>, usize>,
    transfer_refs: &mut Vec<TransferFunctionRef>,
) {
    let key = build_transfer_key(transfer);
    if key == gs.transfer_key {
        return;
    }
    gs.transfer_key = key.clone();

    // Identity transfer — no ExtGState needed
    if key.is_empty() {
        return;
    }

    if let Some(&idx) = ext_gstate_map.get(&key) {
        writeln!(buf, "/GS{} gs", idx).unwrap();
        return;
    }

    let idx = ext_gstates.len();
    // Placeholder entries — actual /TR2 value set by pdf_device when building function objects
    let entries = vec![(b"Type".to_vec(), PdfObj::name("ExtGState"))];
    ext_gstates.push(ExtGStateDict { entries });
    ext_gstate_map.insert(key, idx);

    // Collect the actual sample data
    let (tables, is_color) = if let Some(ref color) = transfer.color {
        (color.to_vec(), true)
    } else if let Some(ref gray) = transfer.gray {
        (vec![Some(gray.clone())], false)
    } else {
        (vec![], false)
    };

    transfer_refs.push(TransferFunctionRef {
        ext_gstate_idx: idx,
        tables,
        is_color,
    });
    writeln!(buf, "/GS{} gs", idx).unwrap();
}

/// Build a dedup key from a HalftoneState based on Arc pointer identity.
fn build_halftone_key(halftone: &HalftoneState) -> Vec<u8> {
    use std::sync::Arc;
    let mut key = Vec::new();
    if let Some(ref color) = halftone.color {
        key.extend(b"C");
        for screen in color {
            if let Some(s) = screen {
                let ptr = Arc::as_ptr(s) as usize;
                key.extend(ptr.to_le_bytes());
            } else {
                key.extend(b"D"); // default
            }
        }
    } else if let Some(ref gray) = halftone.gray {
        key.extend(b"G");
        let ptr = Arc::as_ptr(gray) as usize;
        key.extend(ptr.to_le_bytes());
    }
    // Empty key = default (no halftone)
    key
}

/// Emit a `gs` operator to set halftone when it changes.
fn emit_halftone(
    buf: &mut Vec<u8>,
    halftone: &HalftoneState,
    gs: &mut GState,
    ext_gstates: &mut Vec<ExtGStateDict>,
    ext_gstate_map: &mut HashMap<Vec<u8>, usize>,
    halftone_refs: &mut Vec<HalftoneRef>,
) {
    let key = build_halftone_key(halftone);
    if key == gs.halftone_key {
        return;
    }
    gs.halftone_key = key.clone();

    // Default halftone — no ExtGState needed
    if key.is_empty() {
        return;
    }

    // Prefix key for dedup namespace separation from transfer keys
    let mut map_key = b"HT:".to_vec();
    map_key.extend(&key);

    if let Some(&idx) = ext_gstate_map.get(&map_key) {
        writeln!(buf, "/GS{} gs", idx).unwrap();
        return;
    }

    let idx = ext_gstates.len();
    let entries = vec![(b"Type".to_vec(), PdfObj::name("ExtGState"))];
    ext_gstates.push(ExtGStateDict { entries });
    ext_gstate_map.insert(map_key, idx);

    halftone_refs.push(HalftoneRef {
        ext_gstate_idx: idx,
        state: halftone.clone(),
    });
    writeln!(buf, "/GS{} gs", idx).unwrap();
}

/// Build a dedup key from a BgUcrState based on Arc pointer identity.
fn build_bg_ucr_key(state: &BgUcrState) -> Vec<u8> {
    use std::sync::Arc;
    let mut key = Vec::new();
    if let Some(ref bg) = state.bg {
        key.extend(b"B");
        let ptr = Arc::as_ptr(bg) as usize;
        key.extend(ptr.to_le_bytes());
    }
    if let Some(ref ucr) = state.ucr {
        key.extend(b"U");
        let ptr = Arc::as_ptr(ucr) as usize;
        key.extend(ptr.to_le_bytes());
    }
    key
}

/// Emit a `gs` operator to set BG/UCR when it changes.
fn emit_bg_ucr(
    buf: &mut Vec<u8>,
    state: &BgUcrState,
    gs: &mut GState,
    ext_gstates: &mut Vec<ExtGStateDict>,
    ext_gstate_map: &mut HashMap<Vec<u8>, usize>,
    bg_ucr_refs: &mut Vec<BgUcrRef>,
) {
    let key = build_bg_ucr_key(state);
    if key == gs.bg_ucr_key {
        return;
    }
    gs.bg_ucr_key = key.clone();

    if key.is_empty() {
        return;
    }

    let mut map_key = b"BU:".to_vec();
    map_key.extend(&key);

    if let Some(&idx) = ext_gstate_map.get(&map_key) {
        writeln!(buf, "/GS{} gs", idx).unwrap();
        return;
    }

    let idx = ext_gstates.len();
    let entries = vec![(b"Type".to_vec(), PdfObj::name("ExtGState"))];
    ext_gstates.push(ExtGStateDict { entries });
    ext_gstate_map.insert(map_key, idx);

    bg_ucr_refs.push(BgUcrRef {
        ext_gstate_idx: idx,
        state: state.clone(),
    });
    writeln!(buf, "/GS{} gs", idx).unwrap();
}

/// Coordinate-wise equality for two paths. Used by the Clip arm to
/// detect "replay" Clip elements the reader emits after each
/// `restore_clip_from_stack`. Path segment counts and corresponding
/// segment shapes/coords must match.
fn paths_equal(a: &PsPath, b: &PsPath) -> bool {
    use stet_fonts::geometry::PathSegment;
    if a.segments.len() != b.segments.len() {
        return false;
    }
    for (sa, sb) in a.segments.iter().zip(b.segments.iter()) {
        let eq = match (sa, sb) {
            (PathSegment::MoveTo(x1, y1), PathSegment::MoveTo(x2, y2))
            | (PathSegment::LineTo(x1, y1), PathSegment::LineTo(x2, y2)) => {
                (x1 - x2).abs() < 1e-6 && (y1 - y2).abs() < 1e-6
            }
            (
                PathSegment::CurveTo {
                    x1: a1,
                    y1: b1,
                    x2: c1,
                    y2: d1,
                    x3: e1,
                    y3: f1,
                },
                PathSegment::CurveTo {
                    x1: a2,
                    y1: b2,
                    x2: c2,
                    y2: d2,
                    x3: e2,
                    y3: f2,
                },
            ) => {
                (a1 - a2).abs() < 1e-6
                    && (b1 - b2).abs() < 1e-6
                    && (c1 - c2).abs() < 1e-6
                    && (d1 - d2).abs() < 1e-6
                    && (e1 - e2).abs() < 1e-6
                    && (f1 - f2).abs() < 1e-6
            }
            (PathSegment::ClosePath, PathSegment::ClosePath) => true,
            _ => false,
        };
        if !eq {
            return false;
        }
    }
    true
}

/// Emit path segments as PDF path operators.
fn emit_path(buf: &mut Vec<u8>, path: &PsPath) {
    use stet_fonts::geometry::PathSegment;
    for seg in &path.segments {
        match seg {
            PathSegment::MoveTo(x, y) => {
                fmt_num(buf, *x);
                buf.push(b' ');
                fmt_num(buf, *y);
                buf.extend(b" m\n");
            }
            PathSegment::LineTo(x, y) => {
                fmt_num(buf, *x);
                buf.push(b' ');
                fmt_num(buf, *y);
                buf.extend(b" l\n");
            }
            PathSegment::CurveTo {
                x1,
                y1,
                x2,
                y2,
                x3,
                y3,
            } => {
                fmt_num(buf, *x1);
                buf.push(b' ');
                fmt_num(buf, *y1);
                buf.push(b' ');
                fmt_num(buf, *x2);
                buf.push(b' ');
                fmt_num(buf, *y2);
                buf.push(b' ');
                fmt_num(buf, *x3);
                buf.push(b' ');
                fmt_num(buf, *y3);
                buf.extend(b" c\n");
            }
            PathSegment::ClosePath => {
                buf.extend(b"h\n");
            }
        }
    }
}

/// Compute the PDF cm matrix for image placement.
///
/// Maps the unit square (0,0)-(1,0)-(0,1) to the image rectangle in device space.
/// The content stream's base CTM then maps device space to PDF space.
fn compute_image_matrix(params: &stet_graphics::device::ImageParams) -> Matrix {
    // image_matrix maps user space → image pixel space (PostScript convention)
    // Its inverse maps image pixels → user space
    // CTM maps user space → device space
    // M_scale maps unit square → image pixel grid
    let m_scale = Matrix::new(
        params.width as f64,
        0.0,
        0.0,
        params.height as f64,
        0.0,
        0.0,
    );
    let inv_im = params.image_matrix.invert().unwrap_or(Matrix::identity());

    // PDF image coordinate (0,0) is bottom-left, but data is stored top-to-bottom.
    // The content stream CTM flips Y (device→PDF), which would flip the image.
    // Compensate by flipping the v-axis of the unit square: (u,v) → (u, 1-v).
    let flip_v = Matrix::new(1.0, 0.0, 0.0, -1.0, 0.0, 1.0);

    // Row-vector composition: flip_v × M_scale × inv_im × CTM
    // Using concat: ctm.concat(&inv_im).concat(&m_scale).concat(&flip_v)
    params.ctm.concat(&inv_im).concat(&m_scale).concat(&flip_v)
}

/// Emit a 6-element matrix as PDF `a b c d e f`.
fn emit_matrix(buf: &mut Vec<u8>, m: &Matrix) {
    fmt_num(buf, m.a);
    buf.push(b' ');
    fmt_num(buf, m.b);
    buf.push(b' ');
    fmt_num(buf, m.c);
    buf.push(b' ');
    fmt_num(buf, m.d);
    buf.push(b' ');
    fmt_num(buf, m.tx);
    buf.push(b' ');
    fmt_num(buf, m.ty);
}

/// Emit a `cm` (concat matrix) operator.
fn emit_cm(buf: &mut Vec<u8>, m: &Matrix) {
    emit_matrix(buf, m);
    buf.extend(b" cm\n");
}

/// Check if a matrix is (approximately) identity.
fn is_identity(m: &Matrix) -> bool {
    (m.a - 1.0).abs() < 1e-10
        && m.b.abs() < 1e-10
        && m.c.abs() < 1e-10
        && (m.d - 1.0).abs() < 1e-10
        && m.tx.abs() < 1e-10
        && m.ty.abs() < 1e-10
}

/// Emit a tiling pattern fill: set pattern color space, select pattern, emit path + fill.
fn emit_pattern_fill(
    buf: &mut Vec<u8>,
    params: &PatternFillParams,
    gs: &mut GState,
    pattern_refs: &mut Vec<PatternRef>,
    pattern_map: &mut HashMap<u32, usize>,
    pattern_cs_names: &mut Vec<(String, PdfObj)>,
    pattern_cs_set: &mut HashSet<String>,
) {
    // Dedup by pattern_id — each makepattern call gets a unique ID,
    // so same-pattern reuses share one Pattern XObject.
    let pat_idx = if let Some(&idx) = pattern_map.get(&params.pattern_id) {
        idx
    } else {
        let idx = pattern_refs.len();
        pattern_refs.push(PatternRef {
            tile: params.tile.clone(),
            pattern_matrix: params.pattern_matrix,
            bbox: params.bbox,
            xstep: params.xstep,
            ystep: params.ystep,
            paint_type: params.paint_type,
        });
        pattern_map.insert(params.pattern_id, idx);
        idx
    };

    let pat_name = format!("P{}", pat_idx);

    if params.paint_type == 2 {
        // Uncolored pattern: need [/Pattern /DeviceXxx] color space
        let base_cs = match &params.underlying_color {
            Some(c) if c.native_cmyk.is_some() => "DeviceCMYK",
            Some(c) if c.r == c.g && c.g == c.b => "DeviceGray",
            _ => "DeviceRGB",
        };
        let cs_name = format!("CSP{}", pat_idx);
        if !pattern_cs_set.contains(&cs_name) {
            pattern_cs_names.push((
                cs_name.clone(),
                PdfObj::Array(vec![PdfObj::name("Pattern"), PdfObj::name(base_cs)]),
            ));
            pattern_cs_set.insert(cs_name.clone());
        }
        // Set color space
        if gs.fill_cs_name.as_deref() != Some(&cs_name) {
            writeln!(buf, "/{} cs", cs_name).unwrap();
            gs.fill_cs_name = Some(cs_name);
            gs.fill_color = None;
        }
        // Emit underlying color components + pattern name
        if let Some(color) = &params.underlying_color {
            if let Some((c, m, y, k)) = color.native_cmyk {
                fmt_num(buf, c);
                buf.push(b' ');
                fmt_num(buf, m);
                buf.push(b' ');
                fmt_num(buf, y);
                buf.push(b' ');
                fmt_num(buf, k);
            } else if color.r == color.g && color.g == color.b {
                fmt_num(buf, color.r);
            } else {
                fmt_num(buf, color.r);
                buf.push(b' ');
                fmt_num(buf, color.g);
                buf.push(b' ');
                fmt_num(buf, color.b);
            }
            buf.push(b' ');
        }
        writeln!(buf, "/{} scn", pat_name).unwrap();
    } else {
        // Colored pattern (PaintType 1)
        if gs.fill_cs_name.as_deref() != Some("Pattern") {
            buf.extend(b"/Pattern cs\n");
            gs.fill_cs_name = Some("Pattern".to_string());
            gs.fill_color = None;
        }
        writeln!(buf, "/{} scn", pat_name).unwrap();
    }

    emit_path(buf, &params.path);
    if params.fill_rule == FillRule::EvenOdd {
        buf.extend(b"f*\n");
    } else {
        buf.extend(b"f\n");
    }
}

/// Format a number compactly for PDF content streams.
pub(crate) fn fmt_num(buf: &mut Vec<u8>, v: f64) {
    if v == 0.0 {
        buf.push(b'0');
    } else if v == v.round() && v.abs() < 2_147_483_647.0 {
        write!(buf, "{}", v as i64).unwrap();
    } else {
        // 4 decimal places — enough precision for sub-pixel coordinates
        let s = format!("{:.4}", v);
        let s = s.trim_end_matches('0');
        let s = s.trim_end_matches('.');
        buf.extend(s.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fmt_num() {
        let mut buf = Vec::new();
        fmt_num(&mut buf, 0.0);
        assert_eq!(&buf, b"0");

        buf.clear();
        fmt_num(&mut buf, 1.0);
        assert_eq!(&buf, b"1");

        buf.clear();
        fmt_num(&mut buf, -42.0);
        assert_eq!(&buf, b"-42");

        buf.clear();
        fmt_num(&mut buf, 1.5);
        assert_eq!(&buf, b"1.5");

        buf.clear();
        fmt_num(&mut buf, 0.001);
        assert_eq!(&buf, b"0.001");
    }

    #[test]
    fn test_color_to_pdf() {
        let gray = DeviceColor::from_gray(0.5);
        assert_eq!(color_to_pdf(&gray), PdfColor::Gray(5000));

        let rgb = DeviceColor::from_rgb(1.0, 0.0, 0.5);
        assert_eq!(color_to_pdf(&rgb), PdfColor::Rgb(10000, 0, 5000));
    }
}
