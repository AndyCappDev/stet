// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF graphics state for content stream interpretation.

use stet_graphics::device::{BgUcrState, FillParams, HalftoneState, StrokeParams, TransferState};
use stet_graphics::display_list::{DisplayList, SoftMaskSubtype};
use stet_fonts::geometry::{Matrix, PsPath};
use stet_graphics::color::{DashPattern, DeviceColor, FillRule, LineCap, LineJoin};

/// Wrapper for a shading pattern's display list (Debug-friendly).
#[derive(Clone)]
pub struct ShadingPatternDL(pub DisplayList);

impl std::fmt::Debug for ShadingPatternDL {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShadingPatternDL")
            .field("elements", &self.0.len())
            .finish()
    }
}

/// A resolved tiling pattern ready to be applied at fill/stroke time.
#[derive(Clone)]
pub struct TilingPattern {
    /// Pre-rendered display list for a single tile.
    pub tile: DisplayList,
    /// Bounding box of one tile in pattern space.
    pub bbox: [f64; 4],
    /// Horizontal step between tile origins.
    pub x_step: f64,
    /// Vertical step between tile origins.
    pub y_step: f64,
    /// Combined pattern matrix (CTM x pattern_matrix at scn time).
    pub pattern_matrix: Matrix,
    /// Paint type: 1 = colored, 2 = uncolored.
    pub paint_type: i32,
    /// Unique pattern ID for dedup.
    pub pattern_id: u32,
}

impl std::fmt::Debug for TilingPattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TilingPattern")
            .field("bbox", &self.bbox)
            .field("x_step", &self.x_step)
            .field("y_step", &self.y_step)
            .field("paint_type", &self.paint_type)
            .field("pattern_id", &self.pattern_id)
            .finish()
    }
}

/// A resolved soft mask from ExtGState /SMask.
#[derive(Clone)]
pub struct SoftMask {
    /// Pre-rendered mask form display list.
    pub mask_list: DisplayList,
    /// How to extract the mask (alpha or luminosity).
    pub subtype: SoftMaskSubtype,
    /// Device-space bounding box.
    pub bbox: [f64; 4],
    /// Backdrop color for luminosity masks (RGB, 0.0–1.0).
    pub backdrop_color: Option<[f64; 3]>,
    /// Whether the mask values should be inverted (from /TR `{1 exch sub}`).
    pub transfer_invert: bool,
}

impl std::fmt::Debug for SoftMask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SoftMask")
            .field("subtype", &self.subtype)
            .field("bbox", &self.bbox)
            .finish()
    }
}

/// Reference to a color space (resolved lazily from resources).
#[derive(Clone, Debug)]
pub enum ColorSpaceRef {
    DeviceGray,
    DeviceRGB,
    DeviceCMYK,
    /// Named color space from page resources (e.g. ICCBased, CalRGB, Indexed, etc.).
    Named(Vec<u8>),
}

impl ColorSpaceRef {
    /// Number of components for the simple device color spaces.
    pub fn num_components(&self) -> Option<usize> {
        match self {
            Self::DeviceGray => Some(1),
            Self::DeviceRGB => Some(3),
            Self::DeviceCMYK => Some(4),
            Self::Named(_) => None,
        }
    }
}

/// PDF graphics state — self-contained, no VM/Context dependencies.
#[derive(Clone, Debug)]
pub struct PdfGraphicsState {
    pub ctm: Matrix,
    pub fill_color: DeviceColor,
    pub stroke_color: DeviceColor,
    pub line_width: f64,
    pub line_cap: LineCap,
    pub line_join: LineJoin,
    pub miter_limit: f64,
    pub dash_pattern: DashPattern,
    pub rendering_intent: u8,
    pub stroke_adjust: bool,
    pub overprint: bool,
    pub overprint_stroke: bool,
    /// Overprint mode: 0 = all components painted, 1 = only non-zero components painted.
    pub overprint_mode: i32,
    /// CMYK channel bitmask for fill overprint (which channels the current fill color space paints).
    pub fill_painted_channels: u8,
    /// True when fill color space is DeviceCMYK or ICCBased(4) — OPM 1 only applies to these.
    pub fill_is_device_cmyk: bool,
    /// CMYK channel bitmask for stroke overprint.
    pub stroke_painted_channels: u8,
    pub flatness: f64,
    pub fill_color_space: ColorSpaceRef,
    pub stroke_color_space: ColorSpaceRef,
    /// Pending clip: set by W/W*, applied after next paint op.
    pub pending_clip: Option<(PsPath, FillRule)>,
    /// Current clip path (for restoring on Q).
    pub clip_path: Option<PsPath>,
    /// Clip version counter — incremented on each W/W* application.
    pub clip_path_version: u32,
    pub fill_alpha: f64,
    pub stroke_alpha: f64,
    /// Blend mode (0=Normal, 1=Multiply, ..., 11=Exclusion).
    pub blend_mode: u8,
    // Text state
    pub text_matrix: Matrix,
    pub text_line_matrix: Matrix,
    pub font_size: f64,
    pub char_spacing: f64,
    pub word_spacing: f64,
    pub text_leading: f64,
    pub text_rise: f64,
    /// Horizontal scaling factor (Tz / 100). Default 1.0 = 100%.
    pub horizontal_scaling: f64,
    pub text_rendering_mode: i32,
    pub text_font_name: Vec<u8>,
    /// Active tiling pattern for fill (set by scn with Pattern color space).
    pub fill_pattern: Option<TilingPattern>,
    /// Active shading pattern for fill (PatternType 2).
    /// Stored as `Option<Box<DisplayList>>` so PdfGraphicsState can derive Debug
    /// (DisplayList doesn't implement Debug).
    pub fill_shading_pattern: Option<Box<ShadingPatternDL>>,
    /// Active tiling pattern for stroke (set by SCN with Pattern color space).
    pub stroke_pattern: Option<TilingPattern>,
    /// Active shading pattern for stroke (PatternType 2).
    pub stroke_shading_pattern: Option<Box<ShadingPatternDL>>,
    /// Counter for unique pattern IDs.
    pub next_pattern_id: u32,
    /// Transfer function state.
    pub transfer: TransferState,
    /// Active soft mask from ExtGState /SMask.
    pub soft_mask: Option<SoftMask>,
}

impl PdfGraphicsState {
    /// Create a new graphics state with PDF defaults.
    pub fn new(initial_ctm: Matrix) -> Self {
        Self {
            ctm: initial_ctm,
            fill_color: DeviceColor::black(),
            stroke_color: DeviceColor::black(),
            line_width: 1.0,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            miter_limit: 10.0,
            dash_pattern: DashPattern::solid(),
            rendering_intent: 0,
            stroke_adjust: false,
            overprint: false,
            overprint_stroke: false,
            overprint_mode: 0,
            fill_painted_channels: 0,
            fill_is_device_cmyk: false,
            stroke_painted_channels: 0,
            flatness: 1.0,
            fill_color_space: ColorSpaceRef::DeviceGray,
            stroke_color_space: ColorSpaceRef::DeviceGray,
            pending_clip: None,
            clip_path: None,
            clip_path_version: 0,
            fill_alpha: 1.0,
            stroke_alpha: 1.0,
            blend_mode: 0,
            text_matrix: Matrix::identity(),
            text_line_matrix: Matrix::identity(),
            font_size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            text_leading: 0.0,
            text_rise: 0.0,
            horizontal_scaling: 1.0,
            text_rendering_mode: 0,
            text_font_name: Vec::new(),
            fill_pattern: None,
            fill_shading_pattern: None,
            stroke_pattern: None,
            stroke_shading_pattern: None,
            next_pattern_id: 0,
            transfer: TransferState::default(),
            soft_mask: None,
        }
    }

    /// Build FillParams from current state.
    pub fn fill_params(&self, fill_rule: FillRule) -> FillParams {
        FillParams {
            color: self.fill_color.clone(),
            fill_rule,
            ctm: Matrix::identity(),
            is_text_glyph: false,
            overprint: self.overprint,
            overprint_mode: self.overprint_mode,
            painted_channels: self.fill_painted_channels,
            is_device_cmyk: self.fill_is_device_cmyk,
            spot_color: None,
            rendering_intent: self.rendering_intent,
            transfer: self.transfer.clone(),
            halftone: HalftoneState::default(),
            bg_ucr: BgUcrState::default(),
            alpha: self.fill_alpha,
            blend_mode: self.blend_mode,
        }
    }

    /// Build StrokeParams from current state with CTM scale applied.
    pub fn stroke_params(&self) -> StrokeParams {
        let scale = self.ctm_scale_factor();
        let scaled_dash = DashPattern {
            array: self.dash_pattern.array.iter().map(|d| d * scale).collect(),
            offset: self.dash_pattern.offset * scale,
        };
        StrokeParams {
            color: self.stroke_color.clone(),
            line_width: self.line_width * scale,
            line_cap: self.line_cap,
            line_join: self.line_join,
            miter_limit: self.miter_limit,
            dash_pattern: scaled_dash,
            ctm: Matrix::identity(),
            stroke_adjust: self.stroke_adjust,
            is_text_glyph: false,
            overprint: self.overprint_stroke,
            overprint_mode: self.overprint_mode,
            painted_channels: self.stroke_painted_channels,
            spot_color: None,
            rendering_intent: self.rendering_intent,
            transfer: self.transfer.clone(),
            halftone: HalftoneState::default(),
            bg_ucr: BgUcrState::default(),
            alpha: self.stroke_alpha,
            blend_mode: self.blend_mode,
        }
    }

    /// Build StrokeParams with the CTM applied by the renderer (not pre-scaled).
    /// Used for correct anisotropic strokes where the CTM has non-uniform scaling.
    pub fn stroke_params_with_ctm(&self) -> StrokeParams {
        StrokeParams {
            color: self.stroke_color.clone(),
            line_width: self.line_width,
            line_cap: self.line_cap,
            line_join: self.line_join,
            miter_limit: self.miter_limit,
            dash_pattern: self.dash_pattern.clone(),
            ctm: Matrix::identity(), // caller sets this
            stroke_adjust: self.stroke_adjust,
            is_text_glyph: false,
            overprint: self.overprint_stroke,
            overprint_mode: self.overprint_mode,
            painted_channels: self.stroke_painted_channels,
            spot_color: None,
            rendering_intent: self.rendering_intent,
            transfer: self.transfer.clone(),
            halftone: HalftoneState::default(),
            bg_ucr: BgUcrState::default(),
            alpha: self.stroke_alpha,
            blend_mode: self.blend_mode,
        }
    }

    /// CTM scale factor: sqrt(a^2 + b^2).
    pub fn ctm_scale_factor(&self) -> f64 {
        (self.ctm.a * self.ctm.a + self.ctm.b * self.ctm.b).sqrt()
    }
}
