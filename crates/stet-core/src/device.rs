// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Output device trait — abstraction boundary for rendering backends.

use crate::display_list::{DisplayElement, DisplayList};
use crate::graphics_state::PsPath;

// ── Re-exports from stet-graphics ───────────────────────────────────────────
// These types used to be defined here. Re-export for backward compatibility.
pub use stet_graphics::device::{
    AxialShadingParams, BgUcrState, CMYK_ALL, CMYK_C, CMYK_K, CMYK_M, CMYK_Y, ClipParams,
    ColorStop, FillParams, HalftoneScreen, HalftoneState, ImageColorSpace, ImageParams,
    MeshShadingParams, PatchShadingParams, PatternFillParams, RadialShadingParams,
    ShadingColorSpace, ShadingPatch, ShadingTriangle, ShadingVertex, SimpleColorSpace, SpotColor,
    SpotColorSpace, StrokeParams, TextParams, TintLookupTable, TransferState, TransferTable,
    cmyk_channel_for_name,
};
pub use stet_graphics::device::{PageSink, PageSinkFactory};

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
    fn replay_and_show(&mut self, list: DisplayList, output_path: &str) -> Result<(), String> {
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
                DisplayElement::Group { .. } | DisplayElement::SoftMasked { .. } => {
                    // Groups/SoftMasked are handled by the banded renderer (SkiaDevice).
                }
                DisplayElement::OcgGroup {
                    elements,
                    visibility,
                } => {
                    // Clip ops always apply so subsequent top-level elements
                    // inherit the right clip region, even when the layer is
                    // hidden; paint ops are gated on visibility.
                    let visible = visibility.default_visible();
                    for elem in elements.elements() {
                        match elem {
                            DisplayElement::Clip { path, params } => self.clip_path(path, params),
                            DisplayElement::InitClip => self.init_clip(),
                            _ if !visible => {}
                            DisplayElement::Fill { path, params } => self.fill_path(path, params),
                            DisplayElement::Stroke { path, params } => {
                                self.stroke_path(path, params)
                            }
                            DisplayElement::Image {
                                sample_data,
                                params,
                            } => self.draw_image(sample_data, params),
                            DisplayElement::ErasePage => self.erase_page(),
                            DisplayElement::AxialShading { params } => {
                                self.paint_axial_shading(params)
                            }
                            DisplayElement::RadialShading { params } => {
                                self.paint_radial_shading(params)
                            }
                            DisplayElement::MeshShading { params } => {
                                self.paint_mesh_shading(params)
                            }
                            DisplayElement::PatchShading { params } => {
                                self.paint_patch_shading(params)
                            }
                            DisplayElement::PatternFill { params } => {
                                self.paint_pattern_fill(params)
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        self.show_page(output_path)
    }

    /// Wait for any pending background render to complete.
    fn finish(&mut self) -> Result<(), String> {
        Ok(())
    }

    /// Called after interpretation finishes, with access to the interpreter context.
    fn finish_with_context(&mut self, _ctx: &crate::context::Context) -> Result<(), String> {
        self.finish()
    }

    /// Downcast to a concrete type. Override in implementations that need
    /// to be accessed after rendering (e.g., PdfDevice for in-memory output).
    fn as_any(&self) -> &dyn std::any::Any {
        // Default: return a unit reference (downcasts will fail gracefully)
        &()
    }
}

/// A null rendering device that discards all output.
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

/// Replay a display list to any raster device.
pub fn replay_to_device(list: &DisplayList, device: &mut dyn OutputDevice) {
    for element in list.elements() {
        match element {
            DisplayElement::Fill { path, params } => {
                device.fill_path(path, params);
            }
            DisplayElement::Stroke { path, params } => {
                device.stroke_path(path, params);
            }
            DisplayElement::Clip { path, params } => {
                device.clip_path(path, params);
            }
            DisplayElement::InitClip => {
                device.init_clip();
            }
            DisplayElement::Image {
                sample_data,
                params,
            } => {
                device.draw_image(sample_data, params);
            }
            DisplayElement::ErasePage => {
                device.erase_page();
            }
            DisplayElement::AxialShading { params } => {
                device.paint_axial_shading(params);
            }
            DisplayElement::RadialShading { params } => {
                device.paint_radial_shading(params);
            }
            DisplayElement::MeshShading { params } => {
                device.paint_mesh_shading(params);
            }
            DisplayElement::PatchShading { params } => {
                device.paint_patch_shading(params);
            }
            DisplayElement::PatternFill { params } => {
                device.paint_pattern_fill(params);
            }
            DisplayElement::Text { .. } => {}
            DisplayElement::Group { elements, .. } => {
                replay_to_device(elements, device);
            }
            DisplayElement::SoftMasked { content, .. } => {
                replay_to_device(content, device);
            }
            DisplayElement::OcgGroup {
                elements,
                visibility,
            } => {
                if visibility.default_visible() {
                    replay_to_device(elements, device);
                } else {
                    // Still replay Clip/InitClip so they affect subsequent
                    // top-level elements (see OcgGroup render path for the
                    // rationale).
                    for elem in elements.elements() {
                        match elem {
                            DisplayElement::Clip { path, params } => {
                                device.clip_path(path, params);
                            }
                            DisplayElement::InitClip => {
                                device.init_clip();
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }
}
