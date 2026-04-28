// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Output device parameter types — pure data structures for rendering operations.

use crate::color::{DashPattern, DeviceColor, FillRule, LineCap, LineJoin};
use crate::display_list::DisplayList;
use crate::icc::ProfileHash;
use std::sync::Arc;
use stet_fonts::geometry::{Matrix, PsPath};

/// Pre-sampled transfer function (256 samples, domain `[0,1]` → range `[0,1]`).
/// Arc for cheap clone across display list elements.
pub type TransferTable = Arc<Vec<f64>>;

/// Transfer function state captured at paint time.
#[derive(Clone, Debug, Default)]
pub struct TransferState {
    /// Single-component transfer (from settransfer). None = identity.
    pub gray: Option<TransferTable>,
    /// Per-component color transfer \[R, G, B, Gray\] (from setcolortransfer).
    /// When set, overrides `gray`.
    pub color: Option<[Option<TransferTable>; 4]>,
}

impl TransferState {
    /// Returns true if any non-identity transfer function is set.
    pub fn has_functions(&self) -> bool {
        if self.gray.is_some() {
            return true;
        }
        if let Some(ref color) = self.color {
            return color.iter().any(|t| t.is_some());
        }
        false
    }
}

/// A pre-computed halftone screen for PDF output.
#[derive(Clone, Debug)]
pub struct HalftoneScreen {
    pub frequency: f64,
    pub angle: f64,
    /// Spot function as PDF Type 4 calculator bytes (e.g., b"{ dup mul exch dup mul add 1 exch sub }").
    /// None if conversion failed (falls back to sampled_2d).
    pub type4_tokens: Option<Arc<Vec<u8>>>,
    /// Spot function sampled on a 64×64 grid (4096 f64 values, domain `[-1,1]²`, range `[0,1]`).
    /// Used when Type 4 decompilation fails.
    pub sampled_2d: Option<Arc<Vec<f64>>>,
}

/// Pre-sampled black generation / undercolor removal state for PDF output.
#[derive(Clone, Debug, Default)]
pub struct BgUcrState {
    /// Black generation function (256 samples, domain `[0,1]` → range `[0,1]`).
    pub bg: Option<Arc<Vec<f64>>>,
    /// Undercolor removal function (256 samples, domain `[0,1]` → range `[-1,1]`).
    pub ucr: Option<Arc<Vec<f64>>>,
}

/// Pre-computed halftone state captured at paint time.
#[derive(Clone, Debug, Default)]
pub struct HalftoneState {
    /// Single-component halftone (from setscreen). None = default (suppress).
    pub gray: Option<Arc<HalftoneScreen>>,
    /// Per-component \[R, G, B, Gray\] (from setcolorscreen). Emits Type 5 composite.
    pub color: Option<[Option<Arc<HalftoneScreen>>; 4]>,
}

/// Native Separation/DeviceN color info for PDF output.
#[derive(Clone, Debug)]
pub struct SpotColor {
    /// Tint values from the most recent setcolor (1 for Separation, N for DeviceN).
    pub tint_values: Vec<f64>,
    /// Color space definition for this spot color.
    pub color_space: SpotColorSpace,
}

/// Separation or DeviceN color space with pre-sampled tint function.
#[derive(Clone, Debug)]
pub enum SpotColorSpace {
    Separation {
        name: Vec<u8>,
        alt: SimpleColorSpace,
        tint_table: Arc<TintLookupTable>,
    },
    DeviceN {
        names: Vec<Vec<u8>>,
        alt: SimpleColorSpace,
        tint_table: Arc<TintLookupTable>,
    },
}

/// Simple device color space for alt-space references.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SimpleColorSpace {
    DeviceGray,
    DeviceRGB,
    DeviceCMYK,
}

/// Bitmask of CMYK channels painted by an overprint operation.
/// Bits: 0=Cyan, 1=Magenta, 2=Yellow, 3=Black.
pub const CMYK_C: u8 = 1 << 0;
pub const CMYK_M: u8 = 1 << 1;
pub const CMYK_Y: u8 = 1 << 2;
pub const CMYK_K: u8 = 1 << 3;
pub const CMYK_ALL: u8 = CMYK_C | CMYK_M | CMYK_Y | CMYK_K;

/// Map a CMYK process color name to its channel bit.
pub fn cmyk_channel_for_name(name: &[u8]) -> u8 {
    match name {
        b"Cyan" => CMYK_C,
        b"Magenta" => CMYK_M,
        b"Yellow" => CMYK_Y,
        b"Black" => CMYK_K,
        b"All" => CMYK_ALL,
        b"None" => 0,
        _ => 0,
    }
}

/// Parameters for filling a path.
#[derive(Clone, Debug)]
pub struct FillParams {
    pub color: DeviceColor,
    pub fill_rule: FillRule,
    pub ctm: Matrix,
    /// True when this fill is a text glyph from a show operator.
    /// PDF device skips these (uses Text elements instead).
    pub is_text_glyph: bool,
    /// Overprint flag from graphics state (used by PDF output).
    pub overprint: bool,
    /// Overprint mode (0 or 1). With OPM 1 + DeviceCMYK, only non-zero channels are painted.
    pub overprint_mode: i32,
    /// True when /OPM was set together with /op or /OP in the same ExtGState
    /// dict that configured this fill. Enables strict OPM-1 "preserve zero
    /// components" behavior; when false, an all-zero CMYK source still
    /// performs a full knockout (legacy Adobe compatibility).
    pub opm_paired: bool,
    /// Which CMYK channels this fill paints (bitmask of CMYK_C/M/Y/K).
    pub painted_channels: u8,
    /// True when color space is DeviceCMYK or ICCBased(4).
    pub is_device_cmyk: bool,
    /// Separation/DeviceN color for PDF output. None for device color spaces.
    pub spot_color: Option<SpotColor>,
    /// Rendering intent (0=RelativeColorimetric, 1=Absolute, 2=Perceptual, 3=Saturation).
    pub rendering_intent: u8,
    /// Pre-sampled transfer function state for PDF output.
    pub transfer: TransferState,
    /// Pre-computed halftone screen state for PDF output.
    pub halftone: HalftoneState,
    /// Pre-sampled black generation / undercolor removal for PDF output.
    pub bg_ucr: BgUcrState,
    /// Fill opacity (0.0–1.0, default 1.0). Used by PDF transparency.
    pub alpha: f64,
    /// Blend mode (0=Normal, 1=Multiply, ..., 11=Exclusion). Default 0.
    pub blend_mode: u8,
    /// PDF `AIS` (alpha-is-shape). When true, the source is interpreted as
    /// shape rather than opacity. Default false.
    pub alpha_is_shape: bool,
}

/// Parameters for a text element emitted by show operators.
///
/// The PDF device uses these for BT/ET/Tf/Tj text operators.
/// The raster device ignores them (uses Fill elements for glyph paths).
#[derive(Clone, Debug)]
pub struct TextParams {
    /// Character bytes (or 2-byte CID values for Type 0).
    pub text: Vec<u8>,
    /// Device-space X position at start of string.
    pub start_x: f64,
    /// Device-space Y position at start of string.
    pub start_y: f64,
    /// Font dict entity ID (raw u32 for VM independence).
    pub font_entity: u32,
    /// FontName bytes (e.g., b"Times-Roman").
    pub font_name: Vec<u8>,
    /// FontType (0, 1, 2, 3, 42).
    pub font_type: i32,
    /// Effective device-space font size.
    pub font_size: f64,
    /// Fill color at render time.
    pub color: DeviceColor,
    /// CTM at render time.
    pub ctm: [f64; 6],
    /// User-space font matrix (scaled to point units).
    pub font_matrix: [f64; 6],
    /// PaintType: 0 = fill (default), 2 = stroke (outlined glyphs).
    pub paint_type: i32,
    /// Device-space stroke width for PaintType 2 fonts.
    pub stroke_width: f64,
    /// Separation/DeviceN color for PDF output. None for device color spaces.
    pub spot_color: Option<SpotColor>,
    /// Rendering intent (0=RelativeColorimetric, 1=Absolute, 2=Perceptual, 3=Saturation).
    pub rendering_intent: u8,
    /// Pre-sampled transfer function state for PDF output.
    pub transfer: TransferState,
    /// Pre-computed halftone screen state for PDF output.
    pub halftone: HalftoneState,
    /// Pre-sampled black generation / undercolor removal for PDF output.
    pub bg_ucr: BgUcrState,
    /// Fill opacity (0.0–1.0, default 1.0). Used by PDF transparency.
    pub fill_opacity: f64,
    /// Stroke opacity (0.0–1.0, default 1.0). Applies to PaintType-2 fonts.
    pub stroke_opacity: f64,
    /// Blend mode (0=Normal, 1=Multiply, …, 15=Luminosity). Default 0.
    pub blend_mode: u8,
    /// Alpha-is-shape (PDF `AIS`). Default false.
    pub alpha_is_shape: bool,
    /// Text knockout (PDF `TK`). Default true.
    pub text_knockout: bool,
}

/// Parameters for stroking a path.
#[derive(Clone, Debug)]
pub struct StrokeParams {
    pub color: DeviceColor,
    pub line_width: f64,
    pub line_cap: LineCap,
    pub line_join: LineJoin,
    pub miter_limit: f64,
    pub dash_pattern: DashPattern,
    pub ctm: Matrix,
    /// When true, snap thin stroke coordinates to device pixel centers.
    pub stroke_adjust: bool,
    /// True when this stroke is a text glyph from a show operator (PaintType 2).
    pub is_text_glyph: bool,
    /// Overprint flag from graphics state (used by PDF output).
    pub overprint: bool,
    /// Overprint mode (0 or 1).
    pub overprint_mode: i32,
    /// See FillParams::opm_paired. Strict OPM-1 preserve requires both
    /// /OPM and /op|/OP set in the same ExtGState dict.
    pub opm_paired: bool,
    /// Which CMYK channels this stroke paints (bitmask of CMYK_C/M/Y/K).
    pub painted_channels: u8,
    /// True when stroke color space is DeviceCMYK or ICCBased(4) — OPM 1 only applies to these.
    pub is_device_cmyk: bool,
    /// Separation/DeviceN color for PDF output. None for device color spaces.
    pub spot_color: Option<SpotColor>,
    /// Rendering intent (0=RelativeColorimetric, 1=Absolute, 2=Perceptual, 3=Saturation).
    pub rendering_intent: u8,
    /// Pre-sampled transfer function state for PDF output.
    pub transfer: TransferState,
    /// Pre-computed halftone screen state for PDF output.
    pub halftone: HalftoneState,
    /// Pre-sampled black generation / undercolor removal for PDF output.
    pub bg_ucr: BgUcrState,
    /// Stroke opacity (0.0–1.0, default 1.0). Used by PDF transparency.
    pub alpha: f64,
    /// Blend mode (0=Normal, 1=Multiply, ..., 11=Exclusion). Default 0.
    pub blend_mode: u8,
    /// PDF `AIS` (alpha-is-shape). When true, the source is interpreted as
    /// shape rather than opacity. Default false.
    pub alpha_is_shape: bool,
}

/// Parameters for clipping.
#[derive(Clone, Debug)]
pub struct ClipParams {
    pub fill_rule: FillRule,
    pub ctm: Matrix,
    /// For stroke-based clips: stroke parameters to expand the clip path
    /// from a centerline to a stroke outline before rasterizing.
    pub stroke_params: Option<StrokeParams>,
}

/// Pre-sampled tint transform: maps input tint values to alt-space components.
#[derive(Clone, Debug)]
pub struct TintLookupTable {
    /// Number of input components (1 for Separation, N for DeviceN).
    pub num_inputs: u32,
    /// Number of output components (matches alternative space: 1/3/4).
    pub num_outputs: u32,
    /// Number of samples per dimension.
    pub samples_per_dim: u32,
    /// Flattened f32 data, row-major order. Length = samples_per_dim^num_inputs × num_outputs.
    pub data: Vec<f32>,
}

impl TintLookupTable {
    /// Linear interpolation lookup for 1D (Separation) tint transforms.
    #[inline]
    pub fn lookup_1d(&self, tint: f32, out: &mut [f32]) {
        let n = self.samples_per_dim as usize;
        let no = self.num_outputs as usize;
        let idx = tint * (n - 1) as f32;
        let i0 = (idx as usize).min(n - 2);
        let frac = idx - i0 as f32;
        let base0 = i0 * no;
        let base1 = (i0 + 1) * no;
        for (c, out_val) in out[..no].iter_mut().enumerate() {
            *out_val = self.data[base0 + c] * (1.0 - frac) + self.data[base1 + c] * frac;
        }
    }

    /// Multilinear interpolation lookup for N-D (DeviceN) tint transforms.
    pub fn lookup_nd(&self, inputs: &[f32], out: &mut [f32]) {
        let ni = self.num_inputs as usize;
        let no = self.num_outputs as usize;
        let n = self.samples_per_dim as usize;

        let mut idx = [0usize; 8];
        let mut frac = [0.0f32; 8];
        for d in 0..ni {
            let fi = inputs[d] * (n - 1) as f32;
            idx[d] = (fi as usize).min(n - 2);
            frac[d] = fi - idx[d] as f32;
        }

        let corners = 1usize << ni;
        for out_val in out[..no].iter_mut() {
            *out_val = 0.0;
        }
        for corner in 0..corners {
            let mut weight = 1.0f32;
            let mut linear_idx = 0usize;
            for d in 0..ni {
                let bit = (corner >> d) & 1;
                let dim_idx = idx[d] + bit;
                weight *= if bit == 1 { frac[d] } else { 1.0 - frac[d] };
                let stride = n.pow((ni - 1 - d) as u32);
                linear_idx += dim_idx * stride;
            }
            let base = linear_idx * no;
            for (c, out_val) in out[..no].iter_mut().enumerate() {
                *out_val += weight * self.data.get(base + c).copied().unwrap_or(0.0);
            }
        }
    }
}

/// VM-free color space enum for images stored in the display list.
#[derive(Clone, Debug)]
pub enum ImageColorSpace {
    DeviceGray,
    DeviceRGB,
    DeviceCMYK,
    ICCBased {
        n: u32,
        profile_hash: ProfileHash,
        profile_data: Arc<Vec<u8>>,
    },
    Indexed {
        base: Box<ImageColorSpace>,
        hival: u32,
        lookup: Vec<u8>,
    },
    CIEBasedABC {
        params: Arc<crate::color::CieAbcParams>,
    },
    CIEBasedA {
        params: Arc<crate::color::CieAParams>,
    },
    /// CIE L*a*b* color space (PDF /Lab or ICCBased Lab alternate).
    ///
    /// Sample byte layout: 3 components (L, a, b), 8-bit each. Decode
    /// scales bytes: L = byte/255 × 100; a = byte/255 × (`range[1]`-`range[0]`) + `range[0]`;
    /// b = byte/255 × (`range[3]`-`range[2]`) + `range[2]`.
    Lab {
        white_point: [f64; 3],
        range: [f64; 4],
    },
    Separation {
        name: Vec<u8>,
        alt_space: Box<ImageColorSpace>,
        tint_table: Arc<TintLookupTable>,
    },
    DeviceN {
        names: Vec<Vec<u8>>,
        alt_space: Box<ImageColorSpace>,
        tint_table: Arc<TintLookupTable>,
    },
    Mask {
        color: DeviceColor,
        polarity: bool,
    },
    PreconvertedRGBA,
}

impl ImageColorSpace {
    /// Number of components per sample.
    pub fn num_components(&self) -> u32 {
        match self {
            ImageColorSpace::DeviceGray => 1,
            ImageColorSpace::DeviceRGB => 3,
            ImageColorSpace::DeviceCMYK => 4,
            ImageColorSpace::ICCBased { n, .. } => *n,
            ImageColorSpace::Indexed { .. } => 1,
            ImageColorSpace::CIEBasedABC { .. } => 3,
            ImageColorSpace::CIEBasedA { .. } => 1,
            ImageColorSpace::Lab { .. } => 3,
            ImageColorSpace::Separation { .. } => 1,
            ImageColorSpace::DeviceN { tint_table, .. } => tint_table.num_inputs,
            ImageColorSpace::Mask { .. } => 1,
            ImageColorSpace::PreconvertedRGBA => 4,
        }
    }
}

/// Parameters for drawing an image.
#[derive(Clone, Debug)]
pub struct ImageParams {
    pub width: u32,
    pub height: u32,
    pub color_space: ImageColorSpace,
    pub bits_per_component: u8,
    pub ctm: Matrix,
    pub image_matrix: Matrix,
    pub interpolate: bool,
    pub mask_color: Option<Vec<u8>>,
    pub alpha: f64,
    pub blend_mode: u8,
    pub overprint: bool,
    pub overprint_mode: i32,
    /// See FillParams::opm_paired.
    pub opm_paired: bool,
    pub painted_channels: u8,
    /// PDF `AIS` (alpha-is-shape). Default false.
    pub alpha_is_shape: bool,
}

/// Color space carried through the display list for native shading output.
#[derive(Clone, Debug)]
pub enum ShadingColorSpace {
    DeviceGray,
    DeviceRGB,
    DeviceCMYK,
    ICCBased {
        n: u32,
        profile_hash: ProfileHash,
        profile_data: Arc<Vec<u8>>,
    },
    CalRGB {
        white_point: [f64; 3],
        matrix: Option<[f64; 9]>,
        gamma: Option<[f64; 3]>,
    },
    CalGray {
        white_point: [f64; 3],
        gamma: Option<f64>,
    },
}

impl ShadingColorSpace {
    /// Number of color components in this color space.
    pub fn num_components(&self) -> usize {
        match self {
            ShadingColorSpace::DeviceGray | ShadingColorSpace::CalGray { .. } => 1,
            ShadingColorSpace::DeviceRGB | ShadingColorSpace::CalRGB { .. } => 3,
            ShadingColorSpace::DeviceCMYK => 4,
            ShadingColorSpace::ICCBased { n, .. } => *n as usize,
        }
    }
}

/// A single color stop in a gradient.
#[derive(Clone, Debug)]
pub struct ColorStop {
    pub position: f64,
    pub color: DeviceColor,
    pub raw_components: Vec<f64>,
}

/// Parameters for axial (linear) gradient shading (Type 2).
#[derive(Clone, Debug)]
pub struct AxialShadingParams {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
    pub color_stops: Vec<ColorStop>,
    pub extend_start: bool,
    pub extend_end: bool,
    pub ctm: Matrix,
    pub bbox: Option<[f64; 4]>,
    pub color_space: ShadingColorSpace,
    pub overprint: bool,
    pub painted_channels: u8,
    /// Fill alpha from graphics state (0.0–1.0).
    pub alpha: f64,
    /// Blend mode (0=Normal, …, 15=Luminosity). Default 0.
    pub blend_mode: u8,
    /// PDF `AIS` (alpha-is-shape). Default false.
    pub alpha_is_shape: bool,
    /// True when this shading uses a Separation/DeviceN color space with a
    /// CMYK alternate AND at least one non-process spot colorant.  The
    /// renderer composites the per-pixel CMYK from the gradient stops with
    /// the tracked CMYK buffer multiplicatively, preserving underlying CMYK
    /// paints under the gradient (e.g. green checkmarks under a green→cyan
    /// DeviceN strip survive).
    pub spot_tint_blend: bool,
}

/// Parameters for radial gradient shading (Type 3).
#[derive(Clone, Debug)]
pub struct RadialShadingParams {
    pub x0: f64,
    pub y0: f64,
    pub r0: f64,
    pub x1: f64,
    pub y1: f64,
    pub r1: f64,
    pub color_stops: Vec<ColorStop>,
    pub extend_start: bool,
    pub extend_end: bool,
    pub ctm: Matrix,
    pub bbox: Option<[f64; 4]>,
    pub color_space: ShadingColorSpace,
    pub overprint: bool,
    pub painted_channels: u8,
    /// Fill alpha from graphics state (0.0–1.0).
    pub alpha: f64,
    /// Blend mode (0=Normal, …, 15=Luminosity). Default 0.
    pub blend_mode: u8,
    /// PDF `AIS` (alpha-is-shape). Default false.
    pub alpha_is_shape: bool,
    /// See [`AxialShadingParams::spot_tint_blend`].
    pub spot_tint_blend: bool,
}

/// A vertex in a shading triangle mesh.
#[derive(Clone, Debug)]
pub struct ShadingVertex {
    pub x: f64,
    pub y: f64,
    pub color: DeviceColor,
    pub raw_components: Vec<f64>,
}

/// A triangle in a shading mesh.
#[derive(Clone, Debug)]
pub struct ShadingTriangle {
    pub v0: ShadingVertex,
    pub v1: ShadingVertex,
    pub v2: ShadingVertex,
}

/// Parameters for Gouraud-shaded triangle mesh shading (Types 4 & 5).
#[derive(Clone, Debug)]
pub struct MeshShadingParams {
    pub triangles: Vec<ShadingTriangle>,
    pub ctm: Matrix,
    pub bbox: Option<[f64; 4]>,
    pub color_space: ShadingColorSpace,
    pub overprint: bool,
    pub painted_channels: u8,
    /// Pre-sampled color LUT for function-based mesh shadings.
    /// When present, vertex `raw_components[0]` holds a normalized `[0,1]`
    /// function input. The renderer interpolates this per-pixel, then
    /// indexes the LUT instead of Gouraud-interpolating DeviceColor.
    pub color_lut: Option<Arc<Vec<DeviceColor>>>,
    /// Fill alpha from graphics state (0.0–1.0). Default 1.0.
    pub alpha: f64,
    /// Blend mode (0=Normal, …, 15=Luminosity). Default 0.
    pub blend_mode: u8,
    /// PDF `AIS` (alpha-is-shape). Default false.
    pub alpha_is_shape: bool,
}

/// A patch in a Coons or tensor-product patch mesh.
#[derive(Clone, Debug)]
pub struct ShadingPatch {
    pub points: Vec<(f64, f64)>,
    pub colors: [DeviceColor; 4],
    pub raw_colors: [Vec<f64>; 4],
}

/// Parameters for Coons/tensor-product patch mesh shading (Types 6 & 7).
#[derive(Clone, Debug)]
pub struct PatchShadingParams {
    pub patches: Vec<ShadingPatch>,
    pub ctm: Matrix,
    pub bbox: Option<[f64; 4]>,
    pub color_space: ShadingColorSpace,
    pub overprint: bool,
    pub painted_channels: u8,
    /// When present, vertex `raw_colors[i][0]` holds a normalized `[0,1]`
    /// function input. The renderer interpolates this per-pixel, then
    /// indexes the LUT for per-pixel non-linear function evaluation.
    pub color_lut: Option<Arc<Vec<DeviceColor>>>,
    /// Fill alpha from graphics state (0.0–1.0). Default 1.0.
    pub alpha: f64,
    /// Blend mode (0=Normal, …, 15=Luminosity). Default 0.
    pub blend_mode: u8,
    /// PDF `AIS` (alpha-is-shape). Default false.
    pub alpha_is_shape: bool,
}

/// Parameters for a tiled pattern fill.
#[derive(Clone)]
pub struct PatternFillParams {
    /// The path to fill with the pattern.
    pub path: PsPath,
    /// Fill rule for the path.
    pub fill_rule: FillRule,
    /// Pre-rendered display list for a single tile.
    pub tile: DisplayList,
    /// Pattern matrix (pattern space → device space).
    pub pattern_matrix: Matrix,
    /// Bounding box of one tile in pattern space.
    pub bbox: [f64; 4],
    /// Horizontal step between tile origins.
    pub xstep: f64,
    /// Vertical step between tile origins.
    pub ystep: f64,
    /// Paint type: 1 = colored, 2 = uncolored.
    pub paint_type: i32,
    /// For uncolored patterns, the fill color.
    pub underlying_color: Option<DeviceColor>,
    /// Unique pattern ID from pattern_store (for dedup in PDF output).
    pub pattern_id: u32,
    /// When true, tile display list elements have CTMs in device space
    /// (the pattern matrix is already baked into element transforms).
    /// When false, elements are in pattern space and the renderer applies
    /// the pattern_matrix during rendering.
    pub device_space_tile: bool,
    /// When true, the tile content was designed for a Y-flipped coordinate
    /// system (pattern matrix had negative d). The pre-rendered tile must
    /// be vertically flipped before stamping.
    pub flip_tile_y: bool,
    /// For pattern strokes: stroke parameters to expand the centerline path
    /// into a fill outline for masking. When Some, `path` is a user-space
    /// stroke centerline rather than a fill path.
    pub stroke_params: Option<StrokeParams>,
    /// PDF overprint mode (0 or 1). When 1, CMYK(0,0,0,0) pixels in tile
    /// images are transparent (no ink = don't paint).
    pub overprint_mode: i32,
}

/// Trait for consuming rendered page pixel data.
pub trait PageSink: Send {
    /// Start a new page with the given pixel dimensions.
    fn begin_page(&mut self, width: u32, height: u32) -> Result<(), String>;

    /// Write one or more rows of RGBA pixel data (4 bytes per pixel, row-major).
    fn write_rows(&mut self, rgba_rows: &[u8], num_rows: u32) -> Result<(), String>;

    /// Finish the current page. May block (e.g., viewer waits for user input).
    fn end_page(&mut self) -> Result<(), String>;
}

/// Factory for creating per-page sinks.
pub trait PageSinkFactory: Send + Sync {
    /// Create a new sink for a single page.
    fn create_sink(&self, output_path: &str) -> Result<Box<dyn PageSink>, String>;
}
