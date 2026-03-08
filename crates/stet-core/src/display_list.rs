// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Display list — records drawing operations for deferred replay to a device.

use crate::device::{
    AxialShadingParams, ClipParams, FillParams, ImageParams, MeshShadingParams, OutputDevice,
    PatchShadingParams, PatternFillParams, RadialShadingParams, StrokeParams, TextParams,
};
use crate::graphics_state::PsPath;

/// A single recorded drawing operation.
#[derive(Clone)]
pub enum DisplayElement {
    /// Fill a path.
    Fill { path: PsPath, params: FillParams },
    /// Stroke a path.
    Stroke { path: PsPath, params: StrokeParams },
    /// Intersect the clip region with a path.
    Clip { path: PsPath, params: ClipParams },
    /// Reset clipping to the full page.
    InitClip,
    /// Draw an image (raw sample data in native color space).
    Image {
        sample_data: Vec<u8>,
        params: ImageParams,
    },
    /// Erase the page (fill with white).
    ErasePage,
    /// Axial (linear) gradient shading.
    AxialShading { params: AxialShadingParams },
    /// Radial gradient shading.
    RadialShading { params: RadialShadingParams },
    /// Gouraud-shaded triangle mesh.
    MeshShading { params: MeshShadingParams },
    /// Coons/tensor-product patch mesh.
    PatchShading { params: PatchShadingParams },
    /// Tiled pattern fill.
    PatternFill { params: PatternFillParams },
    /// Text element from show operators (used by PDF device, ignored by rasterizer).
    Text { params: TextParams },
}

/// An ordered list of drawing operations for a single page.
#[derive(Clone)]
pub struct DisplayList {
    elements: Vec<DisplayElement>,
}

impl DisplayList {
    /// Create an empty display list.
    pub fn new() -> Self {
        Self {
            elements: Vec::new(),
        }
    }

    /// Append a drawing operation.
    pub fn push(&mut self, element: DisplayElement) {
        self.elements.push(element);
    }

    /// Access recorded elements.
    pub fn elements(&self) -> &[DisplayElement] {
        &self.elements
    }

    /// Discard all recorded operations.
    pub fn clear(&mut self) {
        self.elements.clear();
    }

    /// Returns true if the display list has no elements.
    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    /// Returns the number of elements.
    pub fn len(&self) -> usize {
        self.elements.len()
    }

    /// Returns a slice of elements starting from the given index.
    pub fn elements_from(&self, start: usize) -> &[DisplayElement] {
        &self.elements[start..]
    }
}

impl Default for DisplayList {
    fn default() -> Self {
        Self::new()
    }
}

/// Replay a display list to any raster device.
pub fn replay_to_device(list: &DisplayList, device: &mut dyn OutputDevice) {
    for element in &list.elements {
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
            DisplayElement::Text { .. } => {
                // Text elements are only used by the PDF device.
                // The rasterizer ignores them (glyph paths are in Fill elements).
            }
        }
    }
}
