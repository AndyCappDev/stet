// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Convert RGBA display list images to PDF image XObjects.

/// A prepared image XObject ready for inclusion in a PDF.
pub struct ImageXObject {
    /// Raw (uncompressed) RGB data — the writer handles compression.
    pub rgb_data: Vec<u8>,
    /// Raw alpha channel (only if image has transparency).
    pub smask_data: Option<Vec<u8>>,
    pub width: u32,
    pub height: u32,
    /// True if this is an imagemask (1-bit stencil).
    pub is_imagemask: bool,
    /// Fill color for imagemask (extracted from non-transparent pixels).
    pub mask_color: Option<(f64, f64, f64)>,
}

/// Convert RGBA image data to a PDF-ready XObject.
pub fn convert_image(rgba: &[u8], width: u32, height: u32, is_mask: bool) -> ImageXObject {
    let npixels = (width * height) as usize;

    if is_mask {
        return convert_imagemask(rgba, width, height, npixels);
    }

    // Check if alpha channel has any non-opaque pixels
    let has_alpha = rgba.chunks_exact(4).any(|px| px[3] != 255);

    // Extract RGB
    let mut rgb = Vec::with_capacity(npixels * 3);
    for px in rgba.chunks_exact(4) {
        rgb.push(px[0]);
        rgb.push(px[1]);
        rgb.push(px[2]);
    }

    let smask_data = if has_alpha {
        Some(rgba.chunks_exact(4).map(|px| px[3]).collect())
    } else {
        None
    };

    ImageXObject {
        rgb_data: rgb,
        smask_data,
        width,
        height,
        is_imagemask: false,
        mask_color: None,
    }
}

fn convert_imagemask(rgba: &[u8], width: u32, height: u32, npixels: usize) -> ImageXObject {
    // Extract the fill color from the first non-transparent pixel
    let mut mask_color = (0.0_f64, 0.0_f64, 0.0_f64);
    for px in rgba.chunks_exact(4) {
        if px[3] > 0 {
            mask_color = (
                px[0] as f64 / 255.0,
                px[1] as f64 / 255.0,
                px[2] as f64 / 255.0,
            );
            break;
        }
    }

    // For imagemask, create an RGB image with alpha as SMask.
    // This preserves the visual appearance (colored pixels where mask=1).
    let mut rgb = Vec::with_capacity(npixels * 3);
    for px in rgba.chunks_exact(4) {
        rgb.push(px[0]);
        rgb.push(px[1]);
        rgb.push(px[2]);
    }

    let smask: Vec<u8> = rgba.chunks_exact(4).map(|px| px[3]).collect();

    ImageXObject {
        rgb_data: rgb,
        smask_data: Some(smask),
        width,
        height,
        is_imagemask: true,
        mask_color: Some(mask_color),
    }
}
