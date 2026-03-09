// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Output device trait — abstraction boundary for rendering backends.

use crate::display_list::DisplayList;
use crate::graphics_state::{
    CieAParams, CieAbcParams, DashPattern, DeviceColor, FillRule, LineCap, LineJoin, Matrix, PsPath,
};
use crate::icc::ProfileHash;
use crate::object::EntityId;
use std::sync::Arc;

/// Parameters for filling a path.
#[derive(Clone, Debug)]
pub struct FillParams {
    pub color: DeviceColor,
    pub fill_rule: FillRule,
    pub ctm: Matrix,
    /// True when this fill is a text glyph from a show operator.
    /// PDF device skips these (uses Text elements instead).
    pub is_text_glyph: bool,
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
    /// Font dict entity ID.
    pub font_entity: EntityId,
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
    /// When true, snap thin stroke coordinates to device pixel centers
    /// for consistent line weight (PostScript `setstrokeadjust`).
    pub stroke_adjust: bool,
    /// True when this stroke is a text glyph from a show operator (PaintType 2).
    /// PDF device skips these (uses Text elements instead).
    pub is_text_glyph: bool,
}

/// Parameters for clipping.
#[derive(Clone, Debug)]
pub struct ClipParams {
    pub fill_rule: FillRule,
    pub ctm: Matrix,
}

/// VM-free color space enum for images stored in the display list.
///
/// Unlike `ColorSpace` (which holds EntityId references into VM stores),
/// this enum is self-contained and safe to store outside the interpreter context.
#[derive(Clone, Debug)]
pub enum ImageColorSpace {
    /// 1-component grayscale.
    DeviceGray,
    /// 3-component RGB.
    DeviceRGB,
    /// 4-component CMYK.
    DeviceCMYK,
    /// ICC profile-based color space.
    ICCBased {
        n: u32,
        profile_hash: ProfileHash,
        profile_data: Arc<Vec<u8>>,
    },
    /// Indexed (palette) color space.
    Indexed {
        base: Box<ImageColorSpace>,
        hival: u32,
        lookup: Vec<u8>,
    },
    /// CIE-based ABC (3-component).
    CIEBasedABC { params: Arc<CieAbcParams> },
    /// CIE-based A (1-component).
    CIEBasedA { params: Arc<CieAParams> },
    /// 1-bit imagemask with fill color and polarity.
    Mask { color: DeviceColor, polarity: bool },
    /// Pre-converted RGBA data (4 bytes/pixel). Used for Type 3 masked images
    /// where stencil alpha has been pre-applied at operator time.
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
            ImageColorSpace::Mask { .. } => 1,
            ImageColorSpace::PreconvertedRGBA => 4,
        }
    }
}

/// Parameters for drawing an image.
#[derive(Clone, Debug)]
pub struct ImageParams {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Color space and sample format.
    pub color_space: ImageColorSpace,
    /// Current transformation matrix.
    pub ctm: Matrix,
    /// Image-to-user-space matrix from the PostScript image operator.
    pub image_matrix: Matrix,
    /// Whether to interpolate when scaling.
    pub interpolate: bool,
    /// ImageType 4 color key mask (raw sample values for transparency).
    pub mask_color: Option<Vec<u8>>,
}

/// Color space carried through the display list for native shading output.
///
/// This allows output devices (PDF, future TIFF) to emit shadings in their
/// native color space rather than pre-converting everything to RGB.
/// The raster renderer ignores this and uses `DeviceColor.r/g/b`.
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
    /// Position along the gradient, normalized to 0.0..=1.0.
    pub position: f64,
    /// Color at this position (RGB for raster rendering).
    pub color: DeviceColor,
    /// Raw component values in the native color space (for PDF/TIFF output).
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
}

/// A vertex in a shading triangle mesh.
#[derive(Clone, Debug)]
pub struct ShadingVertex {
    pub x: f64,
    pub y: f64,
    pub color: DeviceColor,
    /// Raw component values in the native color space (for PDF/TIFF output).
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
}

/// A patch in a Coons or tensor-product patch mesh.
#[derive(Clone, Debug)]
pub struct ShadingPatch {
    /// Control points: 12 for Coons (Type 6), 16 for tensor (Type 7).
    pub points: Vec<(f64, f64)>,
    /// Colors at the 4 corners (RGB for raster rendering).
    pub colors: [DeviceColor; 4],
    /// Raw component values for the 4 corners in the native color space.
    pub raw_colors: [Vec<f64>; 4],
}

/// Parameters for Coons/tensor-product patch mesh shading (Types 6 & 7).
#[derive(Clone, Debug)]
pub struct PatchShadingParams {
    pub patches: Vec<ShadingPatch>,
    pub ctm: Matrix,
    pub bbox: Option<[f64; 4]>,
    pub color_space: ShadingColorSpace,
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
}

/// Trait for raster rendering devices.
///
/// Operators never see the concrete implementation — they call trait methods.
/// This enables backend swaps (tiny-skia, cairo, etc.) without changing operator code.
pub trait OutputDevice {
    /// Fill a path with the given color.
    fn fill_path(&mut self, path: &PsPath, params: &FillParams);

    /// Stroke a path with the given parameters.
    fn stroke_path(&mut self, path: &PsPath, params: &StrokeParams);

    /// Intersect the current clip region with the given path.
    fn clip_path(&mut self, path: &PsPath, params: &ClipParams);

    /// Reset clipping to the full page.
    fn init_clip(&mut self);

    /// Erase the page (fill with white).
    fn erase_page(&mut self);

    /// Output the current page (e.g., save PNG) and return Ok/Err.
    fn show_page(&mut self, output_path: &str) -> Result<(), String>;

    /// Draw an image from raw sample data.
    ///
    /// `sample_data` contains raw samples in the format described by `params.color_space`.
    /// The image matrix maps from image space to user space; the CTM then maps
    /// to device space.
    fn draw_image(&mut self, sample_data: &[u8], params: &ImageParams);

    /// Paint an axial (linear) gradient shading.
    fn paint_axial_shading(&mut self, _params: &AxialShadingParams) {}

    /// Paint a radial gradient shading.
    fn paint_radial_shading(&mut self, _params: &RadialShadingParams) {}

    /// Paint a Gouraud-shaded triangle mesh.
    fn paint_mesh_shading(&mut self, _params: &MeshShadingParams) {}

    /// Paint a Coons/tensor-product patch mesh.
    fn paint_patch_shading(&mut self, _params: &PatchShadingParams) {}

    /// Paint a tiled pattern fill.
    fn paint_pattern_fill(&mut self, _params: &PatternFillParams) {}

    /// Set the trim box for the next page (PDF points, lower-left origin).
    /// Only meaningful for PDF output; other devices ignore this.
    fn set_trim_box(&mut self, _llx: f64, _lly: f64, _urx: f64, _ury: f64) {}

    /// Page dimensions in device pixels.
    fn page_size(&self) -> (u32, u32);

    /// Replay a display list and write output in one step.
    ///
    /// Takes the display list **by value** so callers transfer ownership
    /// (zero-cost pointer swap via `std::mem::take`). Backends may spawn
    /// background threads to overlap rendering with interpretation.
    ///
    /// The default implementation replays the list then calls `show_page`.
    /// Backends may override this to implement banded rendering, where replay
    /// and output are interleaved to reduce peak memory usage.
    fn replay_and_show(
        &mut self,
        list: crate::display_list::DisplayList,
        output_path: &str,
    ) -> Result<(), String> {
        use crate::display_list::DisplayElement;
        for element in list.elements() {
            match element {
                DisplayElement::Fill { path, params } => self.fill_path(path, params),
                DisplayElement::Stroke { path, params } => self.stroke_path(path, params),
                DisplayElement::Clip { path, params } => self.clip_path(path, params),
                DisplayElement::InitClip => self.init_clip(),
                DisplayElement::Image {
                    sample_data,
                    params,
                } => self.draw_image(sample_data, params),
                DisplayElement::ErasePage => self.erase_page(),
                DisplayElement::AxialShading { params } => self.paint_axial_shading(params),
                DisplayElement::RadialShading { params } => self.paint_radial_shading(params),
                DisplayElement::MeshShading { params } => self.paint_mesh_shading(params),
                DisplayElement::PatchShading { params } => self.paint_patch_shading(params),
                DisplayElement::PatternFill { params } => self.paint_pattern_fill(params),
                DisplayElement::Text { .. } => {} // PDF-only, ignored by rasterizer
            }
        }
        self.show_page(output_path)
    }

    /// Wait for any pending background render to complete.
    ///
    /// Called after interpretation finishes to ensure the last page's
    /// render completes before the program exits. Default is a no-op.
    fn finish(&mut self) -> Result<(), String> {
        Ok(())
    }

    /// Called after interpretation finishes, with access to the interpreter context.
    ///
    /// PDF device uses this to read font dicts for embedding (charstrings,
    /// encoding, metrics). Default delegates to `finish()`.
    fn finish_with_context(&mut self, _ctx: &crate::context::Context) -> Result<(), String> {
        self.finish()
    }
}

/// A null rendering device that discards all output.
///
/// Used by the `nulldevice` operator for operations that query the graphics
/// state (bounding boxes, string widths, coordinate transforms) without
/// producing any visible output.
pub struct NullDevice {
    width: u32,
    height: u32,
}

impl NullDevice {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

impl OutputDevice for NullDevice {
    fn fill_path(&mut self, _path: &PsPath, _params: &FillParams) {}
    fn stroke_path(&mut self, _path: &PsPath, _params: &StrokeParams) {}
    fn clip_path(&mut self, _path: &PsPath, _params: &ClipParams) {}
    fn init_clip(&mut self) {}
    fn erase_page(&mut self) {}
    fn show_page(&mut self, _output_path: &str) -> Result<(), String> {
        Ok(())
    }
    fn draw_image(&mut self, _sample_data: &[u8], _params: &ImageParams) {}
    fn page_size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

/// Trait for consuming rendered page pixel data.
///
/// Output backends (PNG, TIFF, viewer) implement this trait to receive
/// rendered RGBA rows from the rasterization engine. The engine calls
/// methods in order: begin_page → write_rows (one or more) → end_page.
pub trait PageSink: Send {
    /// Start a new page with the given pixel dimensions.
    fn begin_page(&mut self, width: u32, height: u32) -> Result<(), String>;

    /// Write one or more rows of RGBA pixel data (4 bytes per pixel, row-major).
    fn write_rows(&mut self, rgba_rows: &[u8], num_rows: u32) -> Result<(), String>;

    /// Finish the current page. May block (e.g., viewer waits for user input).
    fn end_page(&mut self) -> Result<(), String>;
}

/// Factory for creating per-page sinks.
///
/// Called once per showpage with the output path for that page.
/// File-based backends use the path; the viewer ignores it.
pub trait PageSinkFactory: Send + Sync {
    /// Create a new sink for a single page.
    fn create_sink(&self, output_path: &str) -> Result<Box<dyn PageSink>, String>;
}
