// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Convert display list images to PDF image XObjects.
//!
//! Preserves native color spaces (DeviceGray, DeviceRGB, DeviceCMYK, ICCBased,
//! Indexed) for PDF fidelity. Imagemasks are stored as 1-bit stencils.

use std::sync::Arc;
use stet_graphics::color::DeviceColor;
use stet_graphics::device::{ImageColorSpace, ImageParams, TintLookupTable};

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
    /// Separation — name + alt space + tint lookup table (full emission in item 1.3).
    Separation {
        name: Vec<u8>,
        alt: Box<PdfColorSpace>,
        tint_table: Arc<TintLookupTable>,
    },
    /// DeviceN — names + alt space + tint lookup table (full emission in item 1.3).
    DeviceN {
        names: Vec<Vec<u8>>,
        alt: Box<PdfColorSpace>,
        tint_table: Arc<TintLookupTable>,
    },
}

impl PdfColorSpace {
    /// Number of color components in this color space.
    pub fn num_components(&self) -> usize {
        match self {
            PdfColorSpace::DeviceGray => 1,
            PdfColorSpace::DeviceRGB => 3,
            PdfColorSpace::DeviceCMYK => 4,
            PdfColorSpace::ICCBased { n } => *n as usize,
            PdfColorSpace::Indexed { .. } => 1,
            PdfColorSpace::Separation { .. } => 1,
            PdfColorSpace::DeviceN { tint_table, .. } => tint_table.num_inputs as usize,
        }
    }
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
        ImageColorSpace::Separation {
            name,
            alt_space,
            tint_table,
        } => {
            let pdf_alt = image_cs_to_pdf_cs(alt_space);
            ImageXObject {
                sample_data: sample_data.to_vec(),
                smask_data: None,
                width: params.width,
                height: params.height,
                pdf_color_space: PdfColorSpace::Separation {
                    name: name.clone(),
                    alt: Box::new(pdf_alt),
                    tint_table: tint_table.clone(),
                },
                bits_per_component: 8,
                is_imagemask: false,
                mask_color: None,
                color_key_mask: params.mask_color.clone(),
                icc_profile: None,
            }
        }
        ImageColorSpace::DeviceN {
            names,
            alt_space,
            tint_table,
        } => {
            let pdf_alt = image_cs_to_pdf_cs(alt_space);
            ImageXObject {
                sample_data: sample_data.to_vec(),
                smask_data: None,
                width: params.width,
                height: params.height,
                pdf_color_space: PdfColorSpace::DeviceN {
                    names: names.clone(),
                    alt: Box::new(pdf_alt),
                    tint_table: tint_table.clone(),
                },
                bits_per_component: 8,
                is_imagemask: false,
                mask_color: None,
                color_key_mask: params.mask_color.clone(),
                icc_profile: None,
            }
        }
        // CIE-based spaces: convert through CIE pipeline to sRGB
        ImageColorSpace::CIEBasedABC { params: cie_params } => {
            let npixels = (params.width * params.height) as usize;
            let mut rgb = Vec::with_capacity(npixels * 3);
            for i in 0..npixels {
                let si = i * 3;
                let a = sample_data.get(si).copied().unwrap_or(0) as f64 / 255.0;
                let b = sample_data.get(si + 1).copied().unwrap_or(0) as f64 / 255.0;
                let c = sample_data.get(si + 2).copied().unwrap_or(0) as f64 / 255.0;
                let color = DeviceColor::from_cie_abc(a, b, c, cie_params);
                rgb.push((color.r * 255.0).round().clamp(0.0, 255.0) as u8);
                rgb.push((color.g * 255.0).round().clamp(0.0, 255.0) as u8);
                rgb.push((color.b * 255.0).round().clamp(0.0, 255.0) as u8);
            }
            ImageXObject {
                sample_data: rgb,
                smask_data: None,
                width: params.width,
                height: params.height,
                pdf_color_space: PdfColorSpace::DeviceRGB,
                bits_per_component: 8,
                is_imagemask: false,
                mask_color: None,
                color_key_mask: params.mask_color.clone(),
                icc_profile: None,
            }
        }
        ImageColorSpace::CIEBasedA { params: cie_params } => {
            let npixels = (params.width * params.height) as usize;
            let mut rgb = Vec::with_capacity(npixels * 3);
            for i in 0..npixels {
                let val = sample_data.get(i).copied().unwrap_or(0) as f64 / 255.0;
                let color = DeviceColor::from_cie_a(val, cie_params);
                rgb.push((color.r * 255.0).round().clamp(0.0, 255.0) as u8);
                rgb.push((color.g * 255.0).round().clamp(0.0, 255.0) as u8);
                rgb.push((color.b * 255.0).round().clamp(0.0, 255.0) as u8);
            }
            ImageXObject {
                sample_data: rgb,
                smask_data: None,
                width: params.width,
                height: params.height,
                pdf_color_space: PdfColorSpace::DeviceRGB,
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
    color: &DeviceColor,
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

/// Map an ImageColorSpace to the corresponding PdfColorSpace (for alt-space usage).
fn image_cs_to_pdf_cs(cs: &ImageColorSpace) -> PdfColorSpace {
    match cs {
        ImageColorSpace::DeviceGray => PdfColorSpace::DeviceGray,
        ImageColorSpace::DeviceRGB => PdfColorSpace::DeviceRGB,
        ImageColorSpace::DeviceCMYK => PdfColorSpace::DeviceCMYK,
        _ => PdfColorSpace::DeviceRGB, // fallback
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
