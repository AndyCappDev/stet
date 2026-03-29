// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Display list — records drawing operations for deferred replay to a device.

use crate::device::{
    AxialShadingParams, ClipParams, FillParams, ImageParams, MeshShadingParams, PatchShadingParams,
    PatternFillParams, RadialShadingParams, StrokeParams, TextParams,
};
use stet_fonts::geometry::PsPath;

/// Subtype for soft mask extraction.
#[derive(Clone, Debug, PartialEq)]
pub enum SoftMaskSubtype {
    /// Use the alpha channel of the rendered mask directly.
    Alpha,
    /// Convert rendered mask to luminosity (grayscale).
    Luminosity,
}

/// Parameters for a soft mask compositing operation.
#[derive(Clone)]
pub struct SoftMaskParams {
    /// How to extract the mask from the rendered form.
    pub subtype: SoftMaskSubtype,
    /// Device-space bounding box [x_min, y_min, x_max, y_max].
    pub bbox: [f64; 4],
    /// Backdrop color for luminosity masks (RGB, 0.0–1.0). None = black.
    pub backdrop_color: Option<[f64; 3]>,
    /// Whether the mask values should be inverted (from /TR `{1 exch sub}`).
    pub transfer_invert: bool,
    /// Whether the mask form contained nested soft mask scopes (gs-set SMask).
    /// When true, the renderer composites semi-transparent pixels onto the
    /// backdrop before extracting luminosity.
    pub has_nested_mask_scope: bool,
}

/// Parameters for a transparency group compositing operation.
#[derive(Clone)]
pub struct GroupParams {
    /// Device-space bounding box [x_min, y_min, x_max, y_max].
    pub bbox: [f64; 4],
    /// Whether the group is isolated (renders against transparent backdrop).
    pub isolated: bool,
    /// Whether the group uses knockout semantics (elements composite against
    /// the initial backdrop, not against accumulated siblings).
    pub knockout: bool,
    /// Blend mode for compositing the group result onto the parent.
    pub blend_mode: u8,
    /// Opacity for compositing the group result (0.0–1.0).
    pub alpha: f64,
}

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
    /// Transparency group: render children offscreen, composite with blend mode + alpha.
    Group {
        elements: DisplayList,
        params: GroupParams,
    },
    /// Soft-masked content: render mask form to grayscale, multiply with content alpha.
    SoftMasked {
        mask: DisplayList,
        content: DisplayList,
        params: SoftMaskParams,
    },
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

    /// Drain elements from `start..` into a new DisplayList, truncating self.
    pub fn split_off(&mut self, start: usize) -> DisplayList {
        let drained: Vec<DisplayElement> = self.elements.drain(start..).collect();
        DisplayList { elements: drained }
    }

    /// Consume the display list and return the elements.
    pub fn into_elements(self) -> Vec<DisplayElement> {
        self.elements
    }
}

impl Default for DisplayList {
    fn default() -> Self {
        Self::new()
    }
}
