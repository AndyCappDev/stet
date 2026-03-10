// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Color space resolution from PDF page resources.

use crate::error::PdfError;
use crate::objects::{PdfDict, PdfObj};
use crate::resolver::Resolver;

use super::graphics_state::ColorSpaceRef;
use stet_core::device::ImageColorSpace;
use stet_core::graphics_state::DeviceColor;

/// Resolved color space with enough info to convert color values.
#[derive(Clone, Debug)]
pub enum ResolvedColorSpace {
    DeviceGray,
    DeviceRGB,
    DeviceCMYK,
    ICCBased {
        n: u32,
    },
    Indexed {
        base: Box<ResolvedColorSpace>,
        hival: u32,
        lookup: Vec<u8>,
    },
    Separation {
        name: Vec<u8>,
        alt: Box<ResolvedColorSpace>,
    },
    Pattern,
}

impl ResolvedColorSpace {
    /// Number of color components.
    pub fn num_components(&self) -> usize {
        match self {
            Self::DeviceGray => 1,
            Self::DeviceRGB => 3,
            Self::DeviceCMYK => 4,
            Self::ICCBased { n } => *n as usize,
            Self::Indexed { .. } => 1,
            Self::Separation { .. } => 1,
            Self::Pattern => 0,
        }
    }
}

/// Resolve a color space name or array from resources.
pub fn resolve_color_space(
    cs_ref: &ColorSpaceRef,
    resources: &PdfDict,
    resolver: &Resolver,
) -> Result<ResolvedColorSpace, PdfError> {
    match cs_ref {
        ColorSpaceRef::DeviceGray => Ok(ResolvedColorSpace::DeviceGray),
        ColorSpaceRef::DeviceRGB => Ok(ResolvedColorSpace::DeviceRGB),
        ColorSpaceRef::DeviceCMYK => Ok(ResolvedColorSpace::DeviceCMYK),
        ColorSpaceRef::Named(name) => resolve_named_color_space(name, resources, resolver),
    }
}

/// Resolve a named color space from the ColorSpace resource dict.
fn resolve_named_color_space(
    name: &[u8],
    resources: &PdfDict,
    resolver: &Resolver,
) -> Result<ResolvedColorSpace, PdfError> {
    // Check simple device names first
    match name {
        b"DeviceGray" | b"G" => return Ok(ResolvedColorSpace::DeviceGray),
        b"DeviceRGB" | b"RGB" => return Ok(ResolvedColorSpace::DeviceRGB),
        b"DeviceCMYK" | b"CMYK" => return Ok(ResolvedColorSpace::DeviceCMYK),
        b"Pattern" => return Ok(ResolvedColorSpace::Pattern),
        _ => {}
    }

    // Look up in resources ColorSpace dict
    let cs_dict = resources.get_dict(b"ColorSpace");
    let cs_obj = cs_dict.and_then(|d| d.get(name)).ok_or_else(|| {
        PdfError::Other(format!(
            "color space /{} not found in resources",
            String::from_utf8_lossy(name)
        ))
    })?;

    resolve_color_space_obj(cs_obj, resolver)
}

/// Resolve a color space from a PdfObj (name or array).
pub fn resolve_color_space_obj(
    obj: &PdfObj,
    resolver: &Resolver,
) -> Result<ResolvedColorSpace, PdfError> {
    let obj = resolver.deref(obj)?;
    match &obj {
        PdfObj::Name(name) => match name.as_slice() {
            b"DeviceGray" | b"G" => Ok(ResolvedColorSpace::DeviceGray),
            b"DeviceRGB" | b"RGB" => Ok(ResolvedColorSpace::DeviceRGB),
            b"DeviceCMYK" | b"CMYK" => Ok(ResolvedColorSpace::DeviceCMYK),
            b"Pattern" => Ok(ResolvedColorSpace::Pattern),
            _ => Err(PdfError::Other(format!(
                "unknown color space name: /{}",
                String::from_utf8_lossy(name)
            ))),
        },
        PdfObj::Array(arr) if !arr.is_empty() => {
            let cs_name = arr[0]
                .as_name()
                .ok_or(PdfError::Other("color space array[0] is not a name".into()))?;
            match cs_name {
                b"DeviceGray" => Ok(ResolvedColorSpace::DeviceGray),
                b"DeviceRGB" => Ok(ResolvedColorSpace::DeviceRGB),
                b"DeviceCMYK" => Ok(ResolvedColorSpace::DeviceCMYK),
                b"ICCBased" => resolve_icc_based(&arr[1..], resolver),
                b"Indexed" | b"I" => resolve_indexed(&arr[1..], resolver),
                b"Separation" => resolve_separation(&arr[1..], resolver),
                b"CalGray" => Ok(ResolvedColorSpace::DeviceGray),
                b"CalRGB" => Ok(ResolvedColorSpace::DeviceRGB),
                b"Lab" => Ok(ResolvedColorSpace::DeviceRGB),
                b"Pattern" => Ok(ResolvedColorSpace::Pattern),
                _ => Err(PdfError::Other(format!(
                    "unsupported color space: /{}",
                    String::from_utf8_lossy(cs_name)
                ))),
            }
        }
        _ => Err(PdfError::Other(format!(
            "cannot resolve color space from: {obj:?}"
        ))),
    }
}

fn resolve_icc_based(args: &[PdfObj], resolver: &Resolver) -> Result<ResolvedColorSpace, PdfError> {
    if args.is_empty() {
        return Err(PdfError::Other("ICCBased missing stream ref".into()));
    }
    let stream_obj = resolver.deref(&args[0])?;
    let dict = stream_obj.as_dict().ok_or(PdfError::Other(
        "ICCBased stream is not a dict/stream".into(),
    ))?;
    let n = dict
        .get_int(b"N")
        .ok_or(PdfError::Other("ICCBased missing /N".into()))? as u32;
    Ok(ResolvedColorSpace::ICCBased { n })
}

fn resolve_indexed(args: &[PdfObj], resolver: &Resolver) -> Result<ResolvedColorSpace, PdfError> {
    if args.len() < 3 {
        return Err(PdfError::Other("Indexed color space needs 3 args".into()));
    }
    let base = resolve_color_space_obj(&args[0], resolver)?;
    let hival = args[1]
        .as_int()
        .ok_or(PdfError::Other("Indexed hival not int".into()))? as u32;

    let lookup_obj = resolver.deref(&args[2])?;
    let lookup = match &lookup_obj {
        PdfObj::Str(s) => s.clone(),
        PdfObj::Stream { .. } => resolver.stream_data_from_obj(&lookup_obj)?,
        _ => {
            return Err(PdfError::Other(
                "Indexed lookup not string or stream".into(),
            ));
        }
    };

    Ok(ResolvedColorSpace::Indexed {
        base: Box::new(base),
        hival,
        lookup,
    })
}

fn resolve_separation(
    args: &[PdfObj],
    resolver: &Resolver,
) -> Result<ResolvedColorSpace, PdfError> {
    if args.len() < 2 {
        return Err(PdfError::Other("Separation needs at least 2 args".into()));
    }
    let name = args[0]
        .as_name()
        .ok_or(PdfError::Other("Separation name not a name".into()))?
        .to_vec();
    let alt = resolve_color_space_obj(&args[1], resolver)?;
    Ok(ResolvedColorSpace::Separation {
        name,
        alt: Box::new(alt),
    })
}

/// Convert color components to DeviceColor based on resolved color space.
pub fn components_to_device_color(cs: &ResolvedColorSpace, components: &[f64]) -> DeviceColor {
    match cs {
        ResolvedColorSpace::DeviceGray => {
            let g = components.first().copied().unwrap_or(0.0);
            DeviceColor::from_gray(g)
        }
        ResolvedColorSpace::DeviceRGB => {
            let r = components.first().copied().unwrap_or(0.0);
            let g = components.get(1).copied().unwrap_or(0.0);
            let b = components.get(2).copied().unwrap_or(0.0);
            DeviceColor::from_rgb(r, g, b)
        }
        ResolvedColorSpace::DeviceCMYK => {
            let c = components.first().copied().unwrap_or(0.0);
            let m = components.get(1).copied().unwrap_or(0.0);
            let y = components.get(2).copied().unwrap_or(0.0);
            let k = components.get(3).copied().unwrap_or(0.0);
            DeviceColor::from_cmyk(c, m, y, k)
        }
        ResolvedColorSpace::ICCBased { n } => {
            // Fall back to device space based on component count
            match n {
                1 => {
                    let g = components.first().copied().unwrap_or(0.0);
                    DeviceColor::from_gray(g)
                }
                3 => {
                    let r = components.first().copied().unwrap_or(0.0);
                    let g = components.get(1).copied().unwrap_or(0.0);
                    let b = components.get(2).copied().unwrap_or(0.0);
                    DeviceColor::from_rgb(r, g, b)
                }
                4 => {
                    let c = components.first().copied().unwrap_or(0.0);
                    let m = components.get(1).copied().unwrap_or(0.0);
                    let y = components.get(2).copied().unwrap_or(0.0);
                    let k = components.get(3).copied().unwrap_or(0.0);
                    DeviceColor::from_cmyk(c, m, y, k)
                }
                _ => DeviceColor::black(),
            }
        }
        ResolvedColorSpace::Indexed {
            base,
            hival,
            lookup,
        } => {
            let idx = components.first().copied().unwrap_or(0.0) as usize;
            let idx = idx.min(*hival as usize);
            let n = base.num_components();
            let offset = idx * n;
            let mut base_components = Vec::with_capacity(n);
            for i in 0..n {
                let byte = lookup.get(offset + i).copied().unwrap_or(0);
                base_components.push(byte as f64 / 255.0);
            }
            components_to_device_color(base, &base_components)
        }
        ResolvedColorSpace::Separation { alt, .. } => {
            // Without the tint transform function, map tint to alternate space
            // Tint 0.0 = no ink, 1.0 = full ink. Simple fallback: use gray.
            let tint = components.first().copied().unwrap_or(0.0);
            match alt.as_ref() {
                ResolvedColorSpace::DeviceGray => DeviceColor::from_gray(1.0 - tint),
                ResolvedColorSpace::DeviceCMYK => DeviceColor::from_cmyk(0.0, 0.0, 0.0, tint),
                _ => DeviceColor::from_gray(1.0 - tint),
            }
        }
        ResolvedColorSpace::Pattern => DeviceColor::black(),
    }
}

/// Convert a ResolvedColorSpace to ImageColorSpace for image rendering.
pub fn to_image_color_space(cs: &ResolvedColorSpace) -> ImageColorSpace {
    match cs {
        ResolvedColorSpace::DeviceGray => ImageColorSpace::DeviceGray,
        ResolvedColorSpace::DeviceRGB => ImageColorSpace::DeviceRGB,
        ResolvedColorSpace::DeviceCMYK => ImageColorSpace::DeviceCMYK,
        ResolvedColorSpace::ICCBased { n } => match n {
            1 => ImageColorSpace::DeviceGray,
            3 => ImageColorSpace::DeviceRGB,
            4 => ImageColorSpace::DeviceCMYK,
            _ => ImageColorSpace::DeviceRGB,
        },
        ResolvedColorSpace::Indexed {
            base,
            hival,
            lookup,
        } => ImageColorSpace::Indexed {
            base: Box::new(to_image_color_space(base)),
            hival: *hival,
            lookup: lookup.clone(),
        },
        ResolvedColorSpace::Separation { .. } => ImageColorSpace::DeviceGray,
        ResolvedColorSpace::Pattern => ImageColorSpace::DeviceRGB,
    }
}
