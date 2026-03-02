// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Output device trait — abstraction boundary for rendering backends.

use crate::graphics_state::{
    DashPattern, DeviceColor, FillRule, LineCap, LineJoin, Matrix, PsPath,
};

/// Parameters for filling a path.
#[derive(Clone, Debug)]
pub struct FillParams {
    pub color: DeviceColor,
    pub fill_rule: FillRule,
    pub ctm: Matrix,
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
}

/// Parameters for clipping.
#[derive(Clone, Debug)]
pub struct ClipParams {
    pub fill_rule: FillRule,
    pub ctm: Matrix,
}

/// Parameters for drawing an image.
#[derive(Clone, Debug)]
pub struct ImageParams {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// True if this is a 1-bit mask (imagemask).
    pub is_mask: bool,
    /// Current transformation matrix.
    pub ctm: Matrix,
    /// Image-to-user-space matrix from the PostScript image operator.
    pub image_matrix: Matrix,
}

/// A single color stop in a gradient.
#[derive(Clone, Debug)]
pub struct ColorStop {
    /// Position along the gradient, normalized to 0.0..=1.0.
    pub position: f64,
    /// Color at this position.
    pub color: DeviceColor,
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
}

/// A vertex in a shading triangle mesh.
#[derive(Clone, Debug)]
pub struct ShadingVertex {
    pub x: f64,
    pub y: f64,
    pub color: DeviceColor,
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
}

/// A patch in a Coons or tensor-product patch mesh.
#[derive(Clone, Debug)]
pub struct ShadingPatch {
    /// Control points: 12 for Coons (Type 6), 16 for tensor (Type 7).
    pub points: Vec<(f64, f64)>,
    /// Colors at the 4 corners.
    pub colors: [DeviceColor; 4],
}

/// Parameters for Coons/tensor-product patch mesh shading (Types 6 & 7).
#[derive(Clone, Debug)]
pub struct PatchShadingParams {
    pub patches: Vec<ShadingPatch>,
    pub ctm: Matrix,
    pub bbox: Option<[f64; 4]>,
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

    /// Draw an RGBA image.
    ///
    /// `rgba_data` is width×height×4 bytes (R, G, B, A per pixel, row-major).
    /// The image matrix maps from image space to user space; the CTM then maps
    /// to device space.
    fn draw_image(&mut self, rgba_data: &[u8], params: &ImageParams);

    /// Paint an axial (linear) gradient shading.
    fn paint_axial_shading(&mut self, _params: &AxialShadingParams) {}

    /// Paint a radial gradient shading.
    fn paint_radial_shading(&mut self, _params: &RadialShadingParams) {}

    /// Paint a Gouraud-shaded triangle mesh.
    fn paint_mesh_shading(&mut self, _params: &MeshShadingParams) {}

    /// Paint a Coons/tensor-product patch mesh.
    fn paint_patch_shading(&mut self, _params: &PatchShadingParams) {}

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
                DisplayElement::Image { rgba_data, params } => self.draw_image(rgba_data, params),
                DisplayElement::ErasePage => self.erase_page(),
                DisplayElement::AxialShading { params } => self.paint_axial_shading(params),
                DisplayElement::RadialShading { params } => self.paint_radial_shading(params),
                DisplayElement::MeshShading { params } => self.paint_mesh_shading(params),
                DisplayElement::PatchShading { params } => self.paint_patch_shading(params),
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
    fn draw_image(&mut self, _rgba_data: &[u8], _params: &ImageParams) {}
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
