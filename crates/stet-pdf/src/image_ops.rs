// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Convert display list images to PDF image XObjects.
//!
//! Preserves native color spaces (DeviceGray, DeviceRGB, DeviceCMYK, ICCBased,
//! Indexed) for PDF fidelity. Imagemasks are stored as 1-bit stencils.

use stet_core::device::{ImageColorSpace, ImageParams};

/// A prepared image XObject ready for inclusion in a PDF.
pub struct ImageXObject {
    /// Raw sample data in the native color space (or 1-bit mask data).
    pub sample_data: Vec<u8>,
    /// Raw alpha channel (only if image has transparency from Type 3 masked images).
    pub smask_data: Option<Vec<u8>>,
    pub width: u32,
    pub height: u32,
    /// PDF color space name or array.
    pub pdf_color_space: PdfColorSpace,
    /// Bits per component (8 for most, 1 for imagemask).
    pub bits_per_component: u32,
    /// True if this is an imagemask (1-bit stencil).
    pub is_imagemask: bool,
    /// Fill color for imagemask (RGB, 0.0–1.0).
    pub mask_color: Option<(f64, f64, f64)>,
    /// ImageType 4 color key mask ranges.
    pub color_key_mask: Option<Vec<u8>>,
    /// ICC profile data to embed (if ICCBased color space).
    pub icc_profile: Option<IccProfileData>,
}

/// PDF color space representation.
#[derive(Clone, Debug)]
pub enum PdfColorSpace {
    DeviceGray,
    DeviceRGB,
    DeviceCMYK,
    /// ICCBased — needs profile stream reference (set during XObject build).
    ICCBased {
        n: u32,
    },
    /// Indexed — base space + hival + lookup table.
    Indexed {
        base: Box<PdfColorSpace>,
        hival: u32,
        lookup: Vec<u8>,
    },
}

/// ICC profile data for embedding.
#[derive(Clone)]
pub struct IccProfileData {
    pub data: Vec<u8>,
    pub n: u32,
}

/// Convert raw sample data from a display list image to a PDF-ready XObject.
pub fn convert_image(sample_data: &[u8], params: &ImageParams) -> ImageXObject {
    match &params.color_space {
        ImageColorSpace::Mask { color, polarity } => {
            convert_imagemask(sample_data, params, color, *polarity)
        }
        ImageColorSpace::PreconvertedRGBA => convert_preconverted_rgba(sample_data, params),
        ImageColorSpace::DeviceGray => ImageXObject {
            sample_data: sample_data.to_vec(),
            smask_data: None,
            width: params.width,
            height: params.height,
            pdf_color_space: PdfColorSpace::DeviceGray,
            bits_per_component: 8,
            is_imagemask: false,
            mask_color: None,
            color_key_mask: params.mask_color.clone(),
            icc_profile: None,
        },
        ImageColorSpace::DeviceRGB => ImageXObject {
            sample_data: sample_data.to_vec(),
            smask_data: None,
            width: params.width,
            height: params.height,
            pdf_color_space: PdfColorSpace::DeviceRGB,
            bits_per_component: 8,
            is_imagemask: false,
            mask_color: None,
            color_key_mask: params.mask_color.clone(),
            icc_profile: None,
        },
        ImageColorSpace::DeviceCMYK => ImageXObject {
            sample_data: sample_data.to_vec(),
            smask_data: None,
            width: params.width,
            height: params.height,
            pdf_color_space: PdfColorSpace::DeviceCMYK,
            bits_per_component: 8,
            is_imagemask: false,
            mask_color: None,
            color_key_mask: params.mask_color.clone(),
            icc_profile: None,
        },
        ImageColorSpace::ICCBased {
            n, profile_data, ..
        } => ImageXObject {
            sample_data: sample_data.to_vec(),
            smask_data: None,
            width: params.width,
            height: params.height,
            pdf_color_space: PdfColorSpace::ICCBased { n: *n },
            bits_per_component: 8,
            is_imagemask: false,
            mask_color: None,
            color_key_mask: params.mask_color.clone(),
            icc_profile: Some(IccProfileData {
                data: (**profile_data).clone(),
                n: *n,
            }),
        },
        ImageColorSpace::Indexed {
            base,
            hival,
            lookup,
        } => {
            let pdf_base = match base.as_ref() {
                ImageColorSpace::DeviceGray => PdfColorSpace::DeviceGray,
                ImageColorSpace::DeviceCMYK => PdfColorSpace::DeviceCMYK,
                _ => PdfColorSpace::DeviceRGB,
            };
            ImageXObject {
                sample_data: sample_data.to_vec(),
                smask_data: None,
                width: params.width,
                height: params.height,
                pdf_color_space: PdfColorSpace::Indexed {
                    base: Box::new(pdf_base),
                    hival: *hival,
                    lookup: lookup.clone(),
                },
                bits_per_component: 8,
                is_imagemask: false,
                mask_color: None,
                color_key_mask: params.mask_color.clone(),
                icc_profile: None,
            }
        }
        // CIE-based spaces were pre-converted to DeviceRGB at op time
        ImageColorSpace::CIEBasedABC { .. } | ImageColorSpace::CIEBasedA { .. } => {
            let ncomp = params.color_space.num_components();
            let pdf_cs = if ncomp == 1 {
                PdfColorSpace::DeviceGray
            } else {
                PdfColorSpace::DeviceRGB
            };
            ImageXObject {
                sample_data: sample_data.to_vec(),
                smask_data: None,
                width: params.width,
                height: params.height,
                pdf_color_space: pdf_cs,
                bits_per_component: 8,
                is_imagemask: false,
                mask_color: None,
                color_key_mask: params.mask_color.clone(),
                icc_profile: None,
            }
        }
    }
}

/// Convert an imagemask to PDF XObject (1-bit stencil).
fn convert_imagemask(
    raw_bits: &[u8],
    params: &ImageParams,
    color: &stet_core::graphics_state::DeviceColor,
    polarity: bool,
) -> ImageXObject {
    // PDF imagemask Decode: [1 0] means bit=1 paints (our polarity=true).
    // If polarity=false (bit=0 paints), we need Decode [0 1] which is the PDF default,
    // OR we can invert the bits. We'll pass the mask color and let the content
    // stream set the fill color.
    let mask_data = if polarity {
        // bit=1 paints — this matches PDF [1 0] decode. Use data as-is.
        raw_bits.to_vec()
    } else {
        // bit=0 paints — invert all bits so bit=1 paints in PDF space
        raw_bits.iter().map(|b| !b).collect()
    };

    ImageXObject {
        sample_data: mask_data,
        smask_data: None,
        width: params.width,
        height: params.height,
        pdf_color_space: PdfColorSpace::DeviceGray,
        bits_per_component: 1,
        is_imagemask: true,
        mask_color: Some((color.r, color.g, color.b)),
        color_key_mask: None,
        icc_profile: None,
    }
}

/// Convert pre-converted RGBA data to PDF (extract RGB + optional SMask).
fn convert_preconverted_rgba(rgba: &[u8], params: &ImageParams) -> ImageXObject {
    let npixels = (params.width * params.height) as usize;

    let has_alpha = rgba.chunks_exact(4).any(|px| px[3] != 255);

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
        sample_data: rgb,
        smask_data,
        width: params.width,
        height: params.height,
        pdf_color_space: PdfColorSpace::DeviceRGB,
        bits_per_component: 8,
        is_imagemask: false,
        mask_color: None,
        color_key_mask: None,
        icc_profile: None,
    }
}
