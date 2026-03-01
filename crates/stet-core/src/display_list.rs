// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Display list — records drawing operations for deferred replay to a device.

use crate::device::{ClipParams, FillParams, ImageParams, RasterDevice, StrokeParams};
use crate::graphics_state::PsPath;

/// A single recorded drawing operation.
pub enum DisplayElement {
    /// Fill a path.
    Fill { path: PsPath, params: FillParams },
    /// Stroke a path.
    Stroke { path: PsPath, params: StrokeParams },
    /// Intersect the clip region with a path.
    Clip { path: PsPath, params: ClipParams },
    /// Reset clipping to the full page.
    InitClip,
    /// Draw an RGBA image.
    Image {
        rgba_data: Vec<u8>,
        params: ImageParams,
    },
    /// Erase the page (fill with white).
    ErasePage,
}

/// An ordered list of drawing operations for a single page.
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
}

impl Default for DisplayList {
    fn default() -> Self {
        Self::new()
    }
}

/// Replay a display list to any raster device.
pub fn replay_to_device(list: &DisplayList, device: &mut dyn RasterDevice) {
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
            DisplayElement::Image { rgba_data, params } => {
                device.draw_image(rgba_data, params);
            }
            DisplayElement::ErasePage => {
                device.erase_page();
            }
        }
    }
}
