// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Raster device trait — abstraction boundary for rendering backends.

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

/// Trait for raster rendering devices.
///
/// Operators never see the concrete implementation — they call trait methods.
/// This enables backend swaps (tiny-skia, cairo, etc.) without changing operator code.
pub trait RasterDevice {
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

impl RasterDevice for NullDevice {
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
