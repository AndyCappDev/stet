// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Convert a DisplayList into PDF content stream bytes.

use std::collections::{HashMap, HashSet};
use std::io::Write as IoWrite;

use stet_core::context::Context;
use stet_core::device::{
    AxialShadingParams, MeshShadingParams, PatchShadingParams, PatternFillParams,
    RadialShadingParams, SpotColor, SpotColorSpace, StrokeParams, TextParams, TransferState,
};
use stet_core::display_list::{DisplayElement, DisplayList};
use stet_core::graphics_state::{DeviceColor, FillRule, LineCap, LineJoin, Matrix, PsPath};
use stet_core::object::EntityId;

use crate::font_embedder;
use crate::font_tracker::FontTracker;
use crate::image_ops::{self, ImageXObject};
use crate::pdf_objects::PdfObj;
use crate::text_ops;

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
    /// Tiling pattern references used in the content stream.
    pub pattern_refs: Vec<PatternRef>,
    /// Color space entries for uncolored patterns (e.g., [/Pattern /DeviceRGB]).
    pub pattern_cs_entries: Vec<(String, PdfObj)>,
    /// Transfer function references to emit as PDF Type 0 function objects.
    pub transfer_refs: Vec<TransferFunctionRef>,
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
    /// Current fill color space resource name (e.g., "CS0") for Separation/DeviceN.
    fill_cs_name: Option<String>,
    /// Current stroke color space resource name.
    stroke_cs_name: Option<String>,
    /// Rendering intent (0=RelativeColorimetric, 1=Absolute, 2=Perceptual, 3=Saturation).
    rendering_intent: u8,
    /// Dedup key for current transfer state.
    transfer_key: Vec<u8>,
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
            fill_cs_name: None,
            stroke_cs_name: None,
            rendering_intent: 0,
            transfer_key: Vec::new(),
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

fn color_to_pdf(c: &DeviceColor) -> PdfColor {
    if let Some((c_val, m, y, k)) = c.native_cmyk {
        PdfColor::Cmyk(quantize(c_val), quantize(m), quantize(y), quantize(k))
    } else if c.r == c.g && c.g == c.b {
        PdfColor::Gray(quantize(c.r))
    } else {
        PdfColor::Rgb(quantize(c.r), quantize(c.g), quantize(c.b))
    }
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

    let mut buf = Vec::with_capacity(4096);
    let mut images: Vec<ImageXObject> = Vec::new();
    let mut shading_refs: Vec<ShadingRef> = Vec::new();
    let mut clip_depth: u32 = 0;
    let mut gs = GState::new();
    let mut ext_gstates: Vec<ExtGStateDict> = Vec::new();
    let mut ext_gstate_map: HashMap<Vec<u8>, usize> = HashMap::new();
    let mut color_spaces: Vec<(String, SpotColorSpace)> = Vec::new();
    let mut cs_name_map: HashMap<Vec<u8>, String> = HashMap::new();
    let mut pattern_refs: Vec<PatternRef> = Vec::new();
    let mut pattern_map: HashMap<u32, usize> = HashMap::new();
    let mut pattern_cs_names: Vec<(String, PdfObj)> = Vec::new();
    let mut pattern_cs_set: HashSet<String> = HashSet::new();
    let mut transfer_refs: Vec<TransferFunctionRef> = Vec::new();

    // Track which fonts this page uses (for per-page Resources/Font dict)
    let mut page_font_names: HashSet<String> = HashSet::new();

    // First pass: scan Text elements to register fonts
    for element in list.elements() {
        if let DisplayElement::Text { params } = element {
            let name = font_tracker.track(params).to_string();
            page_font_names.insert(name);
        }
    }

    // Pre-compute glyph widths for TJ kern values when Context is available
    if let Some(c) = ctx {
        for usage in font_tracker.fonts_mut() {
            if usage.widths.is_empty() {
                usage.widths = font_embedder::extract_widths(usage, c);
            }
        }
    }

    // Initial CTM: device space (Y-down, pixels) → PDF space (Y-up, points)
    fmt_num(&mut buf, scale);
    buf.extend(b" 0 0 ");
    fmt_num(&mut buf, -scale);
    buf.extend(b" 0 ");
    fmt_num(&mut buf, page_h_pts);
    buf.extend(b" cm\n");

    // Clip to page bounds (device coordinates). The rasterizer implicitly
    // clips to the pixmap, but PDF has no implicit page clip.
    buf.extend(b"0 0 ");
    fmt_num(&mut buf, page_w as f64);
    buf.push(b' ');
    fmt_num(&mut buf, page_h as f64);
    buf.extend(b" re W n\n");

    // Text batch accumulator: consecutive same-font text elements
    let mut text_batch: Vec<&TextParams> = Vec::new();
    let mut batch_font: Option<EntityId> = None;

    for element in list.elements() {
        match element {
            DisplayElement::Text { params } => {
                if batch_font == Some(params.font_entity) {
                    text_batch.push(params);
                } else {
                    flush_text_batch(&text_batch, &font_tracker, &mut buf, &mut gs);
                    text_batch.clear();
                    text_batch.push(params);
                    batch_font = Some(params.font_entity);
                }
                continue;
            }
            // Skip glyph fills/strokes without flushing the text batch —
            // these are interleaved with Text elements in the display list
            DisplayElement::Fill { params, .. } if params.is_text_glyph => continue,
            DisplayElement::Stroke { params, .. } if params.is_text_glyph => continue,
            _ => {
                // Flush any pending text batch before non-text element
                flush_text_batch(&text_batch, &font_tracker, &mut buf, &mut gs);
                text_batch.clear();
                batch_font = None;
            }
        }

        match element {
            DisplayElement::ErasePage => {
                // Fill entire page with white (device coordinates)
                buf.extend(b"1 g 0 0 ");
                fmt_num(&mut buf, page_w as f64);
                buf.push(b' ');
                fmt_num(&mut buf, page_h as f64);
                buf.extend(b" re f\n");
                gs.fill_color = Some(PdfColor::Gray(10000));
            }
            DisplayElement::Fill { path, params } => {
                // Skip glyph fills when we have Text elements for them
                if params.is_text_glyph {
                    continue;
                }
                emit_transfer(&mut buf, &params.transfer, &mut gs, &mut ext_gstates, &mut ext_gstate_map, &mut transfer_refs);
                emit_rendering_intent(&mut buf, params.rendering_intent, &mut gs);
                emit_overprint(&mut buf, params.overprint, &mut gs, &mut ext_gstates, &mut ext_gstate_map);
                if let Some(spot) = &params.spot_color {
                    emit_fill_color_spot(&mut buf, spot, &mut gs, &mut cs_name_map, &mut color_spaces);
                } else {
                    if gs.fill_cs_name.is_some() {
                        gs.fill_cs_name = None;
                        gs.fill_color = None;
                    }
                    emit_fill_color(&mut buf, &params.color, &mut gs);
                }
                emit_path(&mut buf, path);
                if params.fill_rule == FillRule::EvenOdd {
                    buf.extend(b"f*\n");
                } else {
                    buf.extend(b"f\n");
                }
            }
            DisplayElement::Stroke { path, params } => {
                // Skip text glyph strokes (PaintType 2) when Text elements cover them
                if params.is_text_glyph {
                    continue;
                }
                let has_ctm = !is_identity(&params.ctm);
                if has_ctm {
                    buf.extend(b"q\n");
                    emit_cm(&mut buf, &params.ctm);
                }
                emit_transfer(&mut buf, &params.transfer, &mut gs, &mut ext_gstates, &mut ext_gstate_map, &mut transfer_refs);
                emit_rendering_intent(&mut buf, params.rendering_intent, &mut gs);
                emit_overprint(&mut buf, params.overprint, &mut gs, &mut ext_gstates, &mut ext_gstate_map);
                if let Some(spot) = &params.spot_color {
                    emit_stroke_color_spot(&mut buf, spot, &mut gs, &mut cs_name_map, &mut color_spaces);
                } else {
                    if gs.stroke_cs_name.is_some() {
                        gs.stroke_cs_name = None;
                        gs.stroke_color = None;
                    }
                    emit_stroke_color(&mut buf, &params.color, &mut gs);
                }
                emit_line_state(&mut buf, params, &mut gs);
                emit_path(&mut buf, path);
                buf.extend(b"S\n");
                if has_ctm {
                    buf.extend(b"Q\n");
                    gs = GState::new();
                }
            }
            DisplayElement::Clip { path, params } => {
                buf.extend(b"q\n");
                emit_path(&mut buf, path);
                if params.fill_rule == FillRule::EvenOdd {
                    buf.extend(b"W* n\n");
                } else {
                    buf.extend(b"W n\n");
                }
                clip_depth += 1;
                // Don't reset gs — colors/line state carry into clip scope
            }
            DisplayElement::InitClip => {
                for _ in 0..clip_depth {
                    buf.extend(b"Q\n");
                }
                clip_depth = 0;
                gs.reset();
            }
            DisplayElement::Image {
                sample_data,
                params,
            } => {
                let img_idx = images.len();
                let xobj = image_ops::convert_image(sample_data, params);

                // Compute placement matrix: unit square → device space
                let m = compute_image_matrix(params);

                buf.extend(b"q ");
                emit_matrix(&mut buf, &m);
                buf.extend(b" cm ");
                if xobj.is_imagemask {
                    // Set fill color for imagemask
                    if let Some((r, g, b)) = xobj.mask_color {
                        emit_fill_color_rgb(&mut buf, r, g, b);
                    }
                }
                writeln!(buf, "/Im{} Do Q", img_idx).unwrap();

                images.push(xobj);
            }
            DisplayElement::AxialShading { params } => {
                let sh_idx = shading_refs.len();
                buf.extend(b"q ");
                // Apply shading CTM if not identity
                if !is_identity(&params.ctm) {
                    emit_matrix(&mut buf, &params.ctm);
                    buf.extend(b" cm ");
                }
                writeln!(buf, "/Sh{} sh Q", sh_idx).unwrap();
                shading_refs.push(ShadingRef::Axial(params.clone()));
            }
            DisplayElement::RadialShading { params } => {
                let sh_idx = shading_refs.len();
                buf.extend(b"q ");
                if !is_identity(&params.ctm) {
                    emit_matrix(&mut buf, &params.ctm);
                    buf.extend(b" cm ");
                }
                writeln!(buf, "/Sh{} sh Q", sh_idx).unwrap();
                shading_refs.push(ShadingRef::Radial(params.clone()));
            }
            DisplayElement::MeshShading { params } => {
                let sh_idx = shading_refs.len();
                buf.extend(b"q ");
                if !is_identity(&params.ctm) {
                    emit_matrix(&mut buf, &params.ctm);
                    buf.extend(b" cm ");
                }
                writeln!(buf, "/Sh{} sh Q", sh_idx).unwrap();
                shading_refs.push(ShadingRef::Mesh(params.clone()));
            }
            DisplayElement::PatchShading { params } => {
                let sh_idx = shading_refs.len();
                buf.extend(b"q ");
                if !is_identity(&params.ctm) {
                    emit_matrix(&mut buf, &params.ctm);
                    buf.extend(b" cm ");
                }
                writeln!(buf, "/Sh{} sh Q", sh_idx).unwrap();
                shading_refs.push(ShadingRef::Patch(params.clone()));
            }
            DisplayElement::PatternFill { params } => {
                emit_pattern_fill(
                    &mut buf,
                    params,
                    &mut gs,
                    &mut pattern_refs,
                    &mut pattern_map,
                    &mut pattern_cs_names,
                    &mut pattern_cs_set,
                );
            }
            DisplayElement::Text { .. } => unreachable!(), // handled above
        }
    }

    // Flush any remaining text batch
    flush_text_batch(&text_batch, &font_tracker, &mut buf, &mut gs);

    // Close remaining clips
    for _ in 0..clip_depth {
        buf.extend(b"Q\n");
    }

    // Transform pattern matrices from device pixel space → PDF initial coordinate space.
    // The content stream's initial `cm` maps device pixels → PDF points. The PDF spec
    // says Pattern /Matrix maps pattern space → the initial (pre-cm) coordinate system.
    // So: pdf_matrix = pattern_matrix × initial_cm (row-vector convention).
    let initial_cm = Matrix::new(scale, 0.0, 0.0, -scale, 0.0, page_h_pts);
    for pat_ref in &mut pattern_refs {
        pat_ref.pattern_matrix = initial_cm.concat(&pat_ref.pattern_matrix);
    }

    let pattern_cs_entries = pattern_cs_names;

    ContentStreamResult {
        content: buf,
        images,
        shading_refs,
        used_font_names: page_font_names.into_iter().collect(),
        ext_gstate_dicts: ext_gstates,
        color_spaces,
        pattern_refs,
        pattern_cs_entries,
        transfer_refs,
    }
}

/// Generate PDF content stream bytes from a tile display list (for Pattern XObjects).
///
/// Unlike `build_content_stream`, this emits no initial CTM or page clip —
/// tile coordinates are already in pattern space.
pub fn build_tile_content_stream(
    list: &DisplayList,
    font_tracker: &mut FontTracker,
) -> ContentStreamResult {
    let mut buf = Vec::with_capacity(1024);
    let mut images: Vec<ImageXObject> = Vec::new();
    let mut shading_refs: Vec<ShadingRef> = Vec::new();
    let mut clip_depth: u32 = 0;
    let mut gs = GState::new();
    let mut ext_gstates: Vec<ExtGStateDict> = Vec::new();
    let mut ext_gstate_map: HashMap<Vec<u8>, usize> = HashMap::new();
    let mut color_spaces: Vec<(String, SpotColorSpace)> = Vec::new();
    let mut cs_name_map: HashMap<Vec<u8>, String> = HashMap::new();
    let mut pattern_refs: Vec<PatternRef> = Vec::new();
    let mut pattern_map: HashMap<u32, usize> = HashMap::new();
    let mut pattern_cs_names: Vec<(String, PdfObj)> = Vec::new();
    let mut pattern_cs_set: HashSet<String> = HashSet::new();
    let mut transfer_refs: Vec<TransferFunctionRef> = Vec::new();
    let mut page_font_names: HashSet<String> = HashSet::new();

    // Register fonts used in tile
    for element in list.elements() {
        if let DisplayElement::Text { params } = element {
            let name = font_tracker.track(params).to_string();
            page_font_names.insert(name);
        }
    }

    // No initial CTM, no page clip — tile paths are in pattern space

    for element in list.elements() {
        match element {
            DisplayElement::ErasePage => {} // skip in tile context
            DisplayElement::Fill { path, params } => {
                if params.is_text_glyph {
                    continue;
                }
                emit_transfer(&mut buf, &params.transfer, &mut gs, &mut ext_gstates, &mut ext_gstate_map, &mut transfer_refs);
                emit_rendering_intent(&mut buf, params.rendering_intent, &mut gs);
                emit_overprint(
                    &mut buf,
                    params.overprint,
                    &mut gs,
                    &mut ext_gstates,
                    &mut ext_gstate_map,
                );
                if let Some(spot) = &params.spot_color {
                    emit_fill_color_spot(
                        &mut buf,
                        spot,
                        &mut gs,
                        &mut cs_name_map,
                        &mut color_spaces,
                    );
                } else {
                    if gs.fill_cs_name.is_some() {
                        gs.fill_cs_name = None;
                        gs.fill_color = None;
                    }
                    emit_fill_color(&mut buf, &params.color, &mut gs);
                }
                emit_path(&mut buf, path);
                if params.fill_rule == FillRule::EvenOdd {
                    buf.extend(b"f*\n");
                } else {
                    buf.extend(b"f\n");
                }
            }
            DisplayElement::Stroke { path, params } => {
                if params.is_text_glyph {
                    continue;
                }
                let has_ctm = !is_identity(&params.ctm);
                if has_ctm {
                    buf.extend(b"q\n");
                    emit_cm(&mut buf, &params.ctm);
                }
                emit_transfer(&mut buf, &params.transfer, &mut gs, &mut ext_gstates, &mut ext_gstate_map, &mut transfer_refs);
                emit_rendering_intent(&mut buf, params.rendering_intent, &mut gs);
                emit_overprint(
                    &mut buf,
                    params.overprint,
                    &mut gs,
                    &mut ext_gstates,
                    &mut ext_gstate_map,
                );
                if let Some(spot) = &params.spot_color {
                    emit_stroke_color_spot(
                        &mut buf,
                        spot,
                        &mut gs,
                        &mut cs_name_map,
                        &mut color_spaces,
                    );
                } else {
                    if gs.stroke_cs_name.is_some() {
                        gs.stroke_cs_name = None;
                        gs.stroke_color = None;
                    }
                    emit_stroke_color(&mut buf, &params.color, &mut gs);
                }
                emit_line_state(&mut buf, params, &mut gs);
                emit_path(&mut buf, path);
                buf.extend(b"S\n");
                if has_ctm {
                    buf.extend(b"Q\n");
                    gs = GState::new();
                }
            }
            DisplayElement::Clip { path, params } => {
                buf.extend(b"q\n");
                emit_path(&mut buf, path);
                if params.fill_rule == FillRule::EvenOdd {
                    buf.extend(b"W* n\n");
                } else {
                    buf.extend(b"W n\n");
                }
                clip_depth += 1;
            }
            DisplayElement::InitClip => {
                for _ in 0..clip_depth {
                    buf.extend(b"Q\n");
                }
                clip_depth = 0;
                gs.reset();
            }
            DisplayElement::Image {
                sample_data,
                params,
            } => {
                let img_idx = images.len();
                let xobj = image_ops::convert_image(sample_data, params);
                let m = compute_image_matrix(params);
                buf.extend(b"q ");
                emit_matrix(&mut buf, &m);
                buf.extend(b" cm ");
                if xobj.is_imagemask {
                    if let Some((r, g, b)) = xobj.mask_color {
                        emit_fill_color_rgb(&mut buf, r, g, b);
                    }
                }
                writeln!(buf, "/Im{} Do Q", img_idx).unwrap();
                images.push(xobj);
            }
            DisplayElement::AxialShading { params } => {
                let sh_idx = shading_refs.len();
                buf.extend(b"q ");
                if !is_identity(&params.ctm) {
                    emit_matrix(&mut buf, &params.ctm);
                    buf.extend(b" cm ");
                }
                writeln!(buf, "/Sh{} sh Q", sh_idx).unwrap();
                shading_refs.push(ShadingRef::Axial(params.clone()));
            }
            DisplayElement::RadialShading { params } => {
                let sh_idx = shading_refs.len();
                buf.extend(b"q ");
                if !is_identity(&params.ctm) {
                    emit_matrix(&mut buf, &params.ctm);
                    buf.extend(b" cm ");
                }
                writeln!(buf, "/Sh{} sh Q", sh_idx).unwrap();
                shading_refs.push(ShadingRef::Radial(params.clone()));
            }
            DisplayElement::MeshShading { params } => {
                let sh_idx = shading_refs.len();
                buf.extend(b"q ");
                if !is_identity(&params.ctm) {
                    emit_matrix(&mut buf, &params.ctm);
                    buf.extend(b" cm ");
                }
                writeln!(buf, "/Sh{} sh Q", sh_idx).unwrap();
                shading_refs.push(ShadingRef::Mesh(params.clone()));
            }
            DisplayElement::PatchShading { params } => {
                let sh_idx = shading_refs.len();
                buf.extend(b"q ");
                if !is_identity(&params.ctm) {
                    emit_matrix(&mut buf, &params.ctm);
                    buf.extend(b" cm ");
                }
                writeln!(buf, "/Sh{} sh Q", sh_idx).unwrap();
                shading_refs.push(ShadingRef::Patch(params.clone()));
            }
            DisplayElement::PatternFill { params } => {
                emit_pattern_fill(
                    &mut buf,
                    params,
                    &mut gs,
                    &mut pattern_refs,
                    &mut pattern_map,
                    &mut pattern_cs_names,
                    &mut pattern_cs_set,
                );
            }
            DisplayElement::Text { params } => {
                let name = font_tracker.track(params).to_string();
                page_font_names.insert(name);
                // Simple single-element text emission for tiles
                text_ops::emit_text_batch(&mut buf, &[params], font_tracker);
                gs.fill_color = None;
            }
        }
    }

    // Close remaining clips
    for _ in 0..clip_depth {
        buf.extend(b"Q\n");
    }

    ContentStreamResult {
        content: buf,
        images,
        shading_refs,
        used_font_names: page_font_names.into_iter().collect(),
        ext_gstate_dicts: ext_gstates,
        color_spaces,
        pattern_refs,
        pattern_cs_entries: pattern_cs_names,
        transfer_refs,
    }
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
fn emit_fill_color(buf: &mut Vec<u8>, color: &DeviceColor, gs: &mut GState) {
    let pc = color_to_pdf(color);
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
fn emit_stroke_color(buf: &mut Vec<u8>, color: &DeviceColor, gs: &mut GState) {
    let pc = color_to_pdf(color);
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
        write!(buf, "/{} cs\n", cs_name).unwrap();
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
        write!(buf, "/{} CS\n", cs_name).unwrap();
        gs.stroke_cs_name = Some(cs_name);
        gs.stroke_color = None;
    }
    for v in &spot.tint_values {
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
    };
    if gs.line_cap != lc {
        writeln!(buf, "{} J", lc).unwrap();
        gs.line_cap = lc;
    }

    let lj = match params.line_join {
        LineJoin::Miter => 0,
        LineJoin::Round => 1,
        LineJoin::Bevel => 2,
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
    gs: &mut GState,
    ext_gstates: &mut Vec<ExtGStateDict>,
    ext_gstate_map: &mut HashMap<Vec<u8>, usize>,
) {
    if gs.overprint == overprint {
        return;
    }
    gs.overprint = overprint;

    // Build dedup key
    let key = format!("OP{}", overprint as u8).into_bytes();

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
            entries.push((b"OPM".to_vec(), PdfObj::Int(1)));
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
    let entries = vec![
        (b"Type".to_vec(), PdfObj::name("ExtGState")),
    ];
    ext_gstates.push(ExtGStateDict { entries });
    ext_gstate_map.insert(key, idx);

    // Collect the actual sample data
    let (tables, is_color) = if let Some(ref color) = transfer.color {
        (color.iter().map(|t| t.clone()).collect(), true)
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

/// Emit path segments as PDF path operators.
fn emit_path(buf: &mut Vec<u8>, path: &PsPath) {
    use stet_core::graphics_state::PathSegment;
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
fn compute_image_matrix(params: &stet_core::device::ImageParams) -> Matrix {
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
