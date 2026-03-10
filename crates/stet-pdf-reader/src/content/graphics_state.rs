// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF graphics state for content stream interpretation.

use stet_core::device::{BgUcrState, FillParams, HalftoneState, StrokeParams, TransferState};
use stet_core::graphics_state::{
    DashPattern, DeviceColor, FillRule, LineCap, LineJoin, Matrix, PsPath,
};

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
    // Text state
    pub text_matrix: Matrix,
    pub text_line_matrix: Matrix,
    pub font_size: f64,
    pub char_spacing: f64,
    pub word_spacing: f64,
    pub text_leading: f64,
    pub text_rise: f64,
    pub text_rendering_mode: i32,
    pub text_font_name: Vec<u8>,
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
            flatness: 1.0,
            fill_color_space: ColorSpaceRef::DeviceGray,
            stroke_color_space: ColorSpaceRef::DeviceGray,
            pending_clip: None,
            clip_path: None,
            clip_path_version: 0,
            fill_alpha: 1.0,
            stroke_alpha: 1.0,
            text_matrix: Matrix::identity(),
            text_line_matrix: Matrix::identity(),
            font_size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            text_leading: 0.0,
            text_rise: 0.0,
            text_rendering_mode: 0,
            text_font_name: Vec::new(),
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
            spot_color: None,
            rendering_intent: self.rendering_intent,
            transfer: TransferState::default(),
            halftone: HalftoneState::default(),
            bg_ucr: BgUcrState::default(),
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
            spot_color: None,
            rendering_intent: self.rendering_intent,
            transfer: TransferState::default(),
            halftone: HalftoneState::default(),
            bg_ucr: BgUcrState::default(),
        }
    }

    /// CTM scale factor: sqrt(a² + b²).
    pub fn ctm_scale_factor(&self) -> f64 {
        (self.ctm.a * self.ctm.a + self.ctm.b * self.ctm.b).sqrt()
    }
}
