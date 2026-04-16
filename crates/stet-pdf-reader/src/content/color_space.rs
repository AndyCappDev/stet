// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Color space resolution from PDF page resources.

use std::sync::Arc;

use crate::error::PdfError;
use crate::objects::{PdfDict, PdfObj};
use crate::resolver::Resolver;
use crate::resources::function::PdfFunction;

use super::graphics_state::ColorSpaceRef;
use stet_graphics::color::{CieAParams, CieAbcParams, DeviceColor};
use stet_graphics::device::{ImageColorSpace, TintLookupTable};
use stet_graphics::icc::{IccCache, ProfileHash};

/// Resolved color space with enough info to convert color values.
#[derive(Clone, Debug)]
pub enum ResolvedColorSpace {
    DeviceGray,
    DeviceRGB,
    DeviceCMYK,
    ICCBased {
        n: u32,
        /// Raw ICC profile bytes (None if extraction failed).
        profile_data: Option<Arc<Vec<u8>>>,
        /// Alternate color space from stream dict (used when ICC transform fails).
        alternate: Option<Box<ResolvedColorSpace>>,
        /// Pre-computed ICC profile hash (avoids re-hashing per color conversion).
        profile_hash: Option<stet_graphics::icc::ProfileHash>,
    },
    Indexed {
        base: Box<ResolvedColorSpace>,
        hival: u32,
        lookup: Vec<u8>,
    },
    Separation {
        name: Vec<u8>,
        alt: Box<ResolvedColorSpace>,
        tint_fn: Option<PdfFunction>,
    },
    DeviceN {
        names: Vec<Vec<u8>>,
        alt: Box<ResolvedColorSpace>,
        tint_fn: Option<PdfFunction>,
    },
    CalGray {
        params: CieAParams,
    },
    CalRGB {
        params: CieAbcParams,
    },
    Lab {
        white_point: [f64; 3],
        range: [f64; 4], // [a_min, a_max, b_min, b_max]
    },
    Pattern,
}

impl ResolvedColorSpace {
    /// True if this is a Separation color space with the special "None" colorant.
    /// Per PDF spec 4.5.5, "None" produces no visible marks on the page.
    pub fn is_none_colorant(&self) -> bool {
        matches!(self, Self::Separation { name, .. } if name == b"None")
    }

    /// Number of color components.
    pub fn num_components(&self) -> usize {
        match self {
            Self::DeviceGray => 1,
            Self::DeviceRGB => 3,
            Self::DeviceCMYK => 4,
            Self::ICCBased { n, .. } => *n as usize,
            Self::Indexed { .. } => 1,
            Self::Separation { .. } => 1,
            Self::DeviceN { names, .. } => names.len(),
            Self::CalGray { .. } => 1,
            Self::CalRGB { .. } => 3,
            Self::Lab { .. } => 3,
            Self::Pattern => 0,
        }
    }
}

/// Compute the CMYK painted_channels bitmask for overprint simulation.
///
/// Returns which CMYK process color channels are affected by painting in this color space:
/// - DeviceCMYK: all 4 channels (OPM filtering happens at render time)
/// - Separation: the single named channel (Cyan/Magenta/Yellow/Black/All/None)
/// - DeviceN: union of named channels
/// - ICCBased with 4 components: treated as DeviceCMYK
/// - Other (Gray/RGB/CalGray/CalRGB/Lab/Pattern): 0 (no CMYK overprint)
pub fn painted_channels_for_cs(cs: &ResolvedColorSpace) -> u8 {
    use stet_graphics::device::{CMYK_ALL, cmyk_channel_for_name};
    match cs {
        ResolvedColorSpace::DeviceCMYK => CMYK_ALL,
        ResolvedColorSpace::ICCBased { n: 4, .. } => CMYK_ALL,
        ResolvedColorSpace::Separation { name, .. } => cmyk_channel_for_name(name),
        ResolvedColorSpace::DeviceN { names, .. } => names
            .iter()
            .fold(0u8, |acc, n| acc | cmyk_channel_for_name(n)),
        ResolvedColorSpace::Indexed { base, .. } => painted_channels_for_cs(base),
        _ => 0,
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

    // Look up in resources ColorSpace dict (may be an indirect reference)
    let cs_dict = resources.get(b"ColorSpace").and_then(|obj| match obj {
        PdfObj::Dict(_) => Some(obj.as_dict().unwrap().clone()),
        PdfObj::Ref(n, g) => resolver.resolve(*n, *g).ok()?.as_dict().cloned(),
        _ => None,
    });
    let cs_obj = cs_dict.as_ref().and_then(|d| d.get(name)).ok_or_else(|| {
        PdfError::Other(format!(
            "color space /{} not found in resources",
            String::from_utf8_lossy(name)
        ))
    })?;

    resolve_color_space_obj(cs_obj, resolver)
}

/// Maximum recursion depth for color space resolution. Guards against PDFs
/// with self-referential color space definitions (e.g., an Indexed space
/// whose base points back to itself via an indirect reference — seen in
/// Acrobat Distiller 4.0 output).
const MAX_CS_DEPTH: u32 = 16;

/// Resolve a color space from a PdfObj (name or array).
pub fn resolve_color_space_obj(
    obj: &PdfObj,
    resolver: &Resolver,
) -> Result<ResolvedColorSpace, PdfError> {
    resolve_color_space_obj_depth(obj, resolver, 0)
}

fn resolve_color_space_obj_depth(
    obj: &PdfObj,
    resolver: &Resolver,
    depth: u32,
) -> Result<ResolvedColorSpace, PdfError> {
    if depth >= MAX_CS_DEPTH {
        return Err(PdfError::Other(
            "color space recursion limit exceeded".into(),
        ));
    }
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
            // Deref array[0] in case it's an indirect reference to a name
            // (e.g., [7 0 R] where obj 7 = /Pattern).
            let first = resolver.deref(&arr[0])?;
            let cs_name = first
                .as_name()
                .ok_or(PdfError::Other("color space array[0] is not a name".into()))?;
            match cs_name {
                b"DeviceGray" => Ok(ResolvedColorSpace::DeviceGray),
                b"DeviceRGB" => Ok(ResolvedColorSpace::DeviceRGB),
                b"DeviceCMYK" => Ok(ResolvedColorSpace::DeviceCMYK),
                b"ICCBased" => resolve_icc_based(&arr[1..], resolver, depth + 1),
                b"Indexed" | b"I" => resolve_indexed(&arr[1..], resolver, depth + 1),
                b"Separation" => resolve_separation(&arr[1..], resolver, depth + 1),
                b"DeviceN" => resolve_devicen(&arr[1..], resolver, depth + 1),
                b"CalGray" => resolve_cal_gray(&arr[1..], resolver),
                b"CalRGB" => resolve_cal_rgb(&arr[1..], resolver),
                b"Lab" => resolve_lab(&arr[1..], resolver),
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

fn resolve_icc_based(
    args: &[PdfObj],
    resolver: &Resolver,
    depth: u32,
) -> Result<ResolvedColorSpace, PdfError> {
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

    // Extract ICC profile bytes from the stream (use original ref for encryption)
    let profile_data = resolver
        .stream_data_from_obj(&args[0])
        .ok()
        .filter(|d| !d.is_empty())
        .map(Arc::new);

    // Parse /Alternate color space (used as fallback when ICC transform fails)
    let alternate = dict
        .get(b"Alternate")
        .and_then(|obj| resolve_color_space_obj_depth(obj, resolver, depth).ok())
        .or_else(|| icc_alternate_from_header(profile_data.as_deref(), n))
        .map(Box::new);

    // Pre-compute profile hash to avoid re-hashing per color conversion
    let profile_hash = profile_data
        .as_deref()
        .map(|data| IccCache::hash_profile(data));

    Ok(ResolvedColorSpace::ICCBased {
        n,
        profile_data,
        alternate,
        profile_hash,
    })
}

/// Infer an alternate color space from the ICC profile header when /Alternate is absent.
/// Reads the data color space signature at offset 16-19 in the ICC header.
fn icc_alternate_from_header(data: Option<&Vec<u8>>, _n: u32) -> Option<ResolvedColorSpace> {
    let data = data?;
    if data.len() < 20 {
        return None;
    }
    match &data[16..20] {
        b"Lab " => Some(ResolvedColorSpace::Lab {
            // Default D50 white point, full a*/b* range
            white_point: [0.9505, 1.0, 1.089],
            range: [-128.0, 127.0, -128.0, 127.0],
        }),
        _ => None, // RGB/CMYK/Gray already handled correctly by n-based fallback
    }
}

fn resolve_indexed(
    args: &[PdfObj],
    resolver: &Resolver,
    depth: u32,
) -> Result<ResolvedColorSpace, PdfError> {
    if args.len() < 3 {
        return Err(PdfError::Other("Indexed color space needs 3 args".into()));
    }
    let base = resolve_color_space_obj_depth(&args[0], resolver, depth)?;
    let hival = args[1]
        .as_int()
        .ok_or(PdfError::Other("Indexed hival not int".into()))? as u32;

    let lookup_obj = resolver.deref(&args[2])?;
    let lookup = match &lookup_obj {
        PdfObj::Str(s) => s.clone(),
        PdfObj::Stream { .. } => resolver.stream_data_from_obj(&args[2])?,
        PdfObj::Dict(_) => {
            // Dict without Stream variant — try reading stream data directly
            resolver.stream_data_from_obj(&args[2])?
        }
        PdfObj::Null => {
            // Malformed PDF: null lookup table. Use empty data (all indices → black).
            Vec::new()
        }
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
    depth: u32,
) -> Result<ResolvedColorSpace, PdfError> {
    if args.len() < 2 {
        return Err(PdfError::Other("Separation needs at least 2 args".into()));
    }
    let name = args[0]
        .as_name()
        .ok_or(PdfError::Other("Separation name not a name".into()))?
        .to_vec();
    let alt = resolve_color_space_obj_depth(&args[1], resolver, depth)
        .or_else(|_| fallback_alternate(args, resolver))?;
    let tint_fn = if args.len() >= 3 {
        PdfFunction::parse(&args[2], resolver).ok()
    } else {
        None
    };
    Ok(ResolvedColorSpace::Separation {
        name,
        alt: Box::new(alt),
        tint_fn,
    })
}

fn resolve_devicen(
    args: &[PdfObj],
    resolver: &Resolver,
    depth: u32,
) -> Result<ResolvedColorSpace, PdfError> {
    // DeviceN array: [names alternateSpace tintTransform]
    if args.len() < 2 {
        return Err(PdfError::Other("DeviceN needs at least 2 args".into()));
    }
    let names_obj = resolver.deref(&args[0])?;
    let names = match &names_obj {
        PdfObj::Array(arr) => arr
            .iter()
            .filter_map(|o| o.as_name().map(|n| n.to_vec()))
            .collect(),
        _ => return Err(PdfError::Other("DeviceN names not an array".into())),
    };
    let alt = resolve_color_space_obj_depth(&args[1], resolver, depth)
        .or_else(|_| fallback_alternate(args, resolver))?;
    let tint_fn = if args.len() >= 3 {
        PdfFunction::parse(&args[2], resolver).ok()
    } else {
        None
    };
    Ok(ResolvedColorSpace::DeviceN {
        names,
        alt: Box::new(alt),
        tint_fn,
    })
}

/// Fallback alternate color space for broken Separation/DeviceN definitions
/// where the alternate space name is invalid (e.g., same as the colorant name).
///
/// Uses DeviceGray as a safe default — the tint function won't be used since
/// it targets a different number of components, so the Separation evaluator
/// falls back to a simple gray: tint 0 = white, tint 1 = black.
fn fallback_alternate(_args: &[PdfObj], _resolver: &Resolver) -> Result<ResolvedColorSpace, PdfError> {
    Ok(ResolvedColorSpace::DeviceGray)
}

fn resolve_cal_gray(args: &[PdfObj], resolver: &Resolver) -> Result<ResolvedColorSpace, PdfError> {
    let dict = if !args.is_empty() {
        let obj = resolver.deref(&args[0])?;
        obj.as_dict().cloned()
    } else {
        None
    };
    let dict = dict.as_ref();

    let white_point = parse_triple(dict, b"WhitePoint").unwrap_or([0.9505, 1.0, 1.089]);
    let gamma = dict.and_then(|d| d.get_f64(b"Gamma")).unwrap_or(1.0);

    // CalGray maps to CIEBasedA:
    // DecodeA = x^gamma, MatrixA = WhitePoint (so gray=1 → white point XYZ)
    let decode_a = if (gamma - 1.0).abs() > 1e-6 {
        Some((0..256).map(|i| (i as f64 / 255.0).powf(gamma)).collect())
    } else {
        None
    };

    let params = CieAParams {
        white_point,
        matrix_a: white_point, // full intensity = white point
        decode_a,
        // MatrixA produces LMN values that can exceed 1.0 (e.g. D65 Z=1.089).
        // Set RangeLMN upper bounds to the white point so values aren't clamped.
        range_lmn: [
            0.0,
            white_point[0].max(1.0),
            0.0,
            white_point[1].max(1.0),
            0.0,
            white_point[2].max(1.0),
        ],
        ..Default::default()
    };

    Ok(ResolvedColorSpace::CalGray { params })
}

fn resolve_cal_rgb(args: &[PdfObj], resolver: &Resolver) -> Result<ResolvedColorSpace, PdfError> {
    let dict = if !args.is_empty() {
        let obj = resolver.deref(&args[0])?;
        obj.as_dict().cloned()
    } else {
        None
    };
    let dict = dict.as_ref();

    let white_point = parse_triple(dict, b"WhitePoint").unwrap_or([0.9505, 1.0, 1.089]);
    let gamma = parse_triple(dict, b"Gamma").unwrap_or([1.0, 1.0, 1.0]);

    // Matrix is a 9-element array [Xa Ya Za Xb Yb Zb Xc Yc Zc]
    // PDF spec: column i is [Xi Yi Zi] — same as CIEBasedABC column-major convention
    let matrix = dict
        .and_then(|d| d.get_array(b"Matrix"))
        .map(|arr| {
            let v: Vec<f64> = arr.iter().filter_map(|o| o.as_f64()).collect();
            if v.len() >= 9 {
                [v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7], v[8]]
            } else {
                [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]
            }
        })
        .unwrap_or([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);

    let decode_abc = if gamma.iter().any(|&g| (g - 1.0).abs() > 1e-6) {
        Some([
            (0..256)
                .map(|i| (i as f64 / 255.0).powf(gamma[0]))
                .collect(),
            (0..256)
                .map(|i| (i as f64 / 255.0).powf(gamma[1]))
                .collect(),
            (0..256)
                .map(|i| (i as f64 / 255.0).powf(gamma[2]))
                .collect(),
        ])
    } else {
        None
    };

    let params = CieAbcParams {
        white_point,
        matrix_abc: matrix,
        decode_abc,
        // MatrixABC produces XYZ values that can exceed 1.0 (e.g. D65 Z=1.089).
        // Set RangeLMN upper bounds to the white point so values aren't clamped.
        range_lmn: [
            0.0,
            white_point[0].max(1.0),
            0.0,
            white_point[1].max(1.0),
            0.0,
            white_point[2].max(1.0),
        ],
        ..Default::default()
    };

    Ok(ResolvedColorSpace::CalRGB { params })
}

fn resolve_lab(args: &[PdfObj], resolver: &Resolver) -> Result<ResolvedColorSpace, PdfError> {
    let dict = if !args.is_empty() {
        let obj = resolver.deref(&args[0])?;
        obj.as_dict().cloned()
    } else {
        None
    };
    let dict = dict.as_ref();

    let white_point = parse_triple(dict, b"WhitePoint").unwrap_or([0.9505, 1.0, 1.089]);
    let range = dict
        .and_then(|d| d.get_array(b"Range"))
        .map(|arr| {
            let v: Vec<f64> = arr.iter().filter_map(|o| o.as_f64()).collect();
            if v.len() >= 4 {
                [v[0], v[1], v[2], v[3]]
            } else {
                [-100.0, 100.0, -100.0, 100.0]
            }
        })
        .unwrap_or([-100.0, 100.0, -100.0, 100.0]);

    Ok(ResolvedColorSpace::Lab { white_point, range })
}

fn parse_triple(dict: Option<&PdfDict>, key: &[u8]) -> Option<[f64; 3]> {
    dict?.get_array(key).and_then(|arr| {
        let v: Vec<f64> = arr.iter().filter_map(|o| o.as_f64()).collect();
        if v.len() >= 3 {
            Some([v[0], v[1], v[2]])
        } else {
            None
        }
    })
}

/// Convert color components to DeviceColor based on resolved color space.
pub fn components_to_device_color(cs: &ResolvedColorSpace, components: &[f64]) -> DeviceColor {
    components_to_device_color_icc(cs, components, None)
}

/// Process-only CMYK contribution for a Separation colorant. Returns
/// `Some((c, m, y, k))` with the tint placed on the named process channel
/// (or `(0, 0, 0, 0)` for a pure spot colorant so the overprint tracker
/// writes zero to its plates instead of the spot's alt-CMYK tint).
fn separation_process_cmyk(name: &[u8], tint: f64) -> Option<(f64, f64, f64, f64)> {
    use stet_graphics::device::{CMYK_C, CMYK_K, CMYK_M, CMYK_Y, cmyk_channel_for_name};
    let tint = tint.clamp(0.0, 1.0);
    match cmyk_channel_for_name(name) {
        CMYK_C => Some((tint, 0.0, 0.0, 0.0)),
        CMYK_M => Some((0.0, tint, 0.0, 0.0)),
        CMYK_Y => Some((0.0, 0.0, tint, 0.0)),
        CMYK_K => Some((0.0, 0.0, 0.0, tint)),
        0 => Some((0.0, 0.0, 0.0, 0.0)),
        _ => None,
    }
}

/// Process-only CMYK contribution for a DeviceN paint. Each component whose
/// colorant name maps to a process channel (C/M/Y/K/All) adds its tint to
/// that plate via subtractive (multiplicative-complement) stacking; spot
/// colorants contribute nothing to the process buffer.
#[allow(non_snake_case)]
fn deviceN_process_cmyk(names: &[Vec<u8>], tints: &[f64]) -> Option<(f64, f64, f64, f64)> {
    if names.len() != tints.len() {
        return None;
    }
    let mut c_compl = 1.0f64;
    let mut m_compl = 1.0f64;
    let mut y_compl = 1.0f64;
    let mut k_compl = 1.0f64;
    for (name, &tint) in names.iter().zip(tints.iter()) {
        let t = tint.clamp(0.0, 1.0);
        match name.as_slice() {
            b"Cyan" => c_compl *= 1.0 - t,
            b"Magenta" => m_compl *= 1.0 - t,
            b"Yellow" => y_compl *= 1.0 - t,
            b"Black" => k_compl *= 1.0 - t,
            b"All" => {
                let mult = 1.0 - t;
                c_compl *= mult;
                m_compl *= mult;
                y_compl *= mult;
                k_compl *= mult;
            }
            _ => {} // spot colorant — no process contribution
        }
    }
    Some((
        1.0 - c_compl,
        1.0 - m_compl,
        1.0 - y_compl,
        1.0 - k_compl,
    ))
}

/// Convert color components to DeviceColor, with optional ICC profile support.
pub fn components_to_device_color_icc(
    cs: &ResolvedColorSpace,
    components: &[f64],
    mut icc_cache: Option<&mut IccCache>,
) -> DeviceColor {
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
            if let Some(cache) = icc_cache {
                DeviceColor::from_cmyk_icc(c, m, y, k, cache)
            } else {
                DeviceColor::from_cmyk(c, m, y, k)
            }
        }
        ResolvedColorSpace::ICCBased {
            n,
            profile_data,
            alternate,
            profile_hash,
        } => {
            // Try ICC profile conversion first, using pre-computed hash to avoid
            // re-hashing the profile data on every color conversion.
            let hash = profile_hash.or_else(|| {
                icc_cache
                    .as_deref_mut()
                    .and_then(|cache| profile_data.as_deref().and_then(|d| cache.register_profile_with_n(d, Some(*n))))
            });
            if let Some(cache) = icc_cache.as_deref_mut()
                && let Some(hash) = hash
            {
                // Ensure the profile is registered (first time only)
                if !cache.has_profile(&hash) {
                    if let Some(data) = profile_data {
                        cache.register_profile_with_n(data, Some(*n));
                    }
                }
                if let Some((r, g, b)) = cache.convert_color(&hash, components) {
                    // For 4-component (CMYK) ICC profiles, preserve the
                    // source CMYK values in native_cmyk for overprint simulation.
                    if *n == 4 {
                        let c = components.first().copied().unwrap_or(0.0);
                        let m = components.get(1).copied().unwrap_or(0.0);
                        let y = components.get(2).copied().unwrap_or(0.0);
                        let k = components.get(3).copied().unwrap_or(0.0);
                        return DeviceColor {
                            r,
                            g,
                            b,
                            native_cmyk: Some((c, m, y, k)),
                            process_cmyk: None,
                        };
                    }
                    return DeviceColor::from_rgb(r, g, b);
                }
            }
            // Fall back to alternate color space if available (handles Lab, XYZ, etc.)
            if let Some(alt) = alternate {
                return components_to_device_color_icc(alt, components, icc_cache);
            }
            // Last resort: device space based on component count
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
            let raw = components.first().copied().unwrap_or(0.0).round();
            let idx = (raw as i64).clamp(0, *hival as i64) as usize;
            let n = base.num_components();
            let offset = idx * n;
            let mut base_components = Vec::with_capacity(n);
            for i in 0..n {
                let byte = lookup.get(offset + i).copied().unwrap_or(0);
                base_components.push(byte as f64 / 255.0);
            }
            components_to_device_color_icc(base, &base_components, icc_cache)
        }
        ResolvedColorSpace::Separation {
            alt, tint_fn, name, ..
        } => {
            // The special colorant name "None" produces no visible marks (PDF spec 4.5.5).
            // Return white here; callers should also set alpha=0 for true transparency.
            if name == b"None" {
                return DeviceColor::from_gray(1.0);
            }
            let tint = components.first().copied().unwrap_or(0.0);
            let mut color = if let Some(func) = tint_fn {
                let alt_components = func.evaluate(&[tint]);
                components_to_device_color_icc(alt, &alt_components, icc_cache)
            } else {
                // Fallback without tint function
                match alt.as_ref() {
                    ResolvedColorSpace::DeviceGray => DeviceColor::from_gray(1.0 - tint),
                    ResolvedColorSpace::DeviceCMYK => DeviceColor::from_cmyk(0.0, 0.0, 0.0, tint),
                    _ => DeviceColor::from_gray(1.0 - tint),
                }
            };
            // For CMYK process colorants (Cyan/Magenta/Yellow/Black), ensure
            // native_cmyk is set even when the alternate space is non-CMYK
            // (e.g. ICCBased RGB). Without this, overprint simulation uses
            // the (1-r, 1-g, 1-b, 0) fallback which puts Black ink into C+M+Y
            // instead of K, making Separation /Black strokes invisible when
            // painted_channels = CMYK_K.
            if color.native_cmyk.is_none() {
                use stet_graphics::device::{CMYK_C, CMYK_M, CMYK_Y, CMYK_K, cmyk_channel_for_name};
                let ch = cmyk_channel_for_name(name);
                if ch != 0 {
                    let (c, m, y, k) = match ch {
                        CMYK_C => (tint, 0.0, 0.0, 0.0),
                        CMYK_M => (0.0, tint, 0.0, 0.0),
                        CMYK_Y => (0.0, 0.0, tint, 0.0),
                        CMYK_K => (0.0, 0.0, 0.0, tint),
                        _ => (0.0, 0.0, 0.0, 0.0),
                    };
                    color.native_cmyk = Some((c, m, y, k));
                }
            }
            // Process-only contribution: for a process colorant the tint maps
            // directly to that process channel; for a pure spot the process
            // contribution is zero (the alt-CMYK is spot-derived and must not
            // leak into the process-CMYK tracker).
            color.process_cmyk = separation_process_cmyk(name, tint);
            color
        }
        ResolvedColorSpace::DeviceN { names, alt, tint_fn, .. } => {
            let mut color = if let Some(func) = tint_fn {
                let alt_components = func.evaluate(components);
                components_to_device_color_icc(alt, &alt_components, icc_cache)
            } else {
                // Fallback: use first component as gray
                let v = components.first().copied().unwrap_or(0.0);
                DeviceColor::from_gray(1.0 - v)
            };
            color.process_cmyk = deviceN_process_cmyk(names, components);
            color
        }
        ResolvedColorSpace::CalGray { params } => {
            let a = components.first().copied().unwrap_or(0.0);
            DeviceColor::from_cie_a(a, params)
        }
        ResolvedColorSpace::CalRGB { params } => {
            let a = components.first().copied().unwrap_or(0.0);
            let b = components.get(1).copied().unwrap_or(0.0);
            let c = components.get(2).copied().unwrap_or(0.0);
            DeviceColor::from_cie_abc(a, b, c, params)
        }
        ResolvedColorSpace::Lab { white_point, range } => {
            lab_to_device_color(components, white_point, range)
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
        ResolvedColorSpace::ICCBased {
            n,
            profile_data,
            profile_hash,
            alternate,
        } => {
            // When an ICC profile is available, the rasterizer transforms
            // samples through it directly — keep ImageColorSpace::ICCBased so
            // that path runs.  Only fall back to the declared alternate (Lab /
            // CalRGB / CalGray / device) when no profile bytes are present.
            if let (Some(data), Some(hash)) = (profile_data, profile_hash) {
                return ImageColorSpace::ICCBased {
                    n: *n,
                    profile_hash: *hash,
                    profile_data: data.clone(),
                };
            }
            if let Some(alt) = alternate {
                match alt.as_ref() {
                    ResolvedColorSpace::Lab { .. }
                    | ResolvedColorSpace::CalRGB { .. }
                    | ResolvedColorSpace::CalGray { .. } => return to_image_color_space(alt),
                    _ => {}
                }
            }
            match n {
                1 => ImageColorSpace::DeviceGray,
                3 => ImageColorSpace::DeviceRGB,
                4 => ImageColorSpace::DeviceCMYK,
                _ => ImageColorSpace::DeviceRGB,
            }
        }
        ResolvedColorSpace::Indexed {
            base,
            hival,
            lookup,
        } => ImageColorSpace::Indexed {
            base: Box::new(to_image_color_space(base)),
            hival: *hival,
            lookup: lookup.clone(),
        },
        ResolvedColorSpace::Separation { alt, tint_fn, .. } => {
            if let Some(func) = tint_fn {
                build_1d_tint_image_cs(func, alt)
            } else {
                ImageColorSpace::DeviceGray
            }
        }
        ResolvedColorSpace::DeviceN {
            names,
            alt,
            tint_fn,
        } => {
            if let Some(func) = tint_fn {
                build_nd_tint_image_cs(func, alt, names.len())
            } else {
                ImageColorSpace::DeviceGray
            }
        }
        ResolvedColorSpace::CalGray { params } => ImageColorSpace::CIEBasedA {
            params: std::sync::Arc::new(params.clone()),
        },
        ResolvedColorSpace::CalRGB { params } => ImageColorSpace::CIEBasedABC {
            params: std::sync::Arc::new(params.clone()),
        },
        ResolvedColorSpace::Lab { white_point, range } => ImageColorSpace::Lab {
            white_point: *white_point,
            range: *range,
        },
        ResolvedColorSpace::Pattern => ImageColorSpace::DeviceRGB,
    }
}

/// Register an ICC profile and return its hash (for use in color conversions).
pub fn register_icc_profile(
    cs: &ResolvedColorSpace,
    icc_cache: &mut IccCache,
) -> Option<ProfileHash> {
    match cs {
        ResolvedColorSpace::ICCBased { n, profile_data, .. } => {
            let data = profile_data.as_ref()?;
            icc_cache.register_profile_with_n(data, Some(*n))
        }
        _ => None,
    }
}

fn lab_to_device_color(
    components: &[f64],
    _white_point: &[f64; 3],
    range: &[f64; 4],
) -> DeviceColor {
    let l_star = components.first().copied().unwrap_or(0.0);
    let a_star = components.get(1).copied().unwrap_or(0.0);
    let b_star = components.get(2).copied().unwrap_or(0.0);
    DeviceColor::from_lab(l_star, a_star, b_star, range)
}

/// Check if a color space requires CIE→RGB conversion (Lab, CalRGB, CalGray).
/// The tint table stores pre-converted RGB values for these spaces.
fn is_cie_space(cs: &ResolvedColorSpace) -> bool {
    match cs {
        ResolvedColorSpace::Lab { .. }
        | ResolvedColorSpace::CalRGB { .. }
        | ResolvedColorSpace::CalGray { .. } => true,
        // ICCBased profiles that wrap a CIE alternate (e.g. Lab) need CIE conversion too
        ResolvedColorSpace::ICCBased {
            alternate: Some(alt),
            ..
        } => is_cie_space(alt),
        _ => false,
    }
}

/// Convert tint function output through a CIE alternate space to RGB,
/// pushing 3 f32 values (r, g, b) into `data`.
fn push_cie_converted(alt: &ResolvedColorSpace, out: &[f64], data: &mut Vec<f32>) {
    let color = components_to_device_color_icc(alt, out, None);
    data.push(color.r as f32);
    data.push(color.g as f32);
    data.push(color.b as f32);
}

/// Collapse the tint table's alt color space to a device-only ImageColorSpace.
/// The tint table stores the raw tint-function output components; at rasterize
/// time `alt_comps_to_rgb` interprets them.  That function only knows Device*
/// variants, so any ICCBased alt must be flattened to the matching device
/// space (which matches the old parse-time behaviour).
fn tint_alt_device_cs(alt: &ResolvedColorSpace) -> ImageColorSpace {
    match alt {
        ResolvedColorSpace::ICCBased { n, .. } => match n {
            1 => ImageColorSpace::DeviceGray,
            4 => ImageColorSpace::DeviceCMYK,
            _ => ImageColorSpace::DeviceRGB,
        },
        _ => to_image_color_space(alt),
    }
}

/// Build a 1D TintLookupTable for Separation image color space.
fn build_1d_tint_image_cs(func: &PdfFunction, alt: &ResolvedColorSpace) -> ImageColorSpace {
    let cie = is_cie_space(alt);
    let (alt_cs, n_out) = if cie {
        (ImageColorSpace::DeviceRGB, 3)
    } else {
        (tint_alt_device_cs(alt), alt.num_components())
    };
    let samples = 256u32;
    let mut data = Vec::with_capacity(samples as usize * n_out);
    for i in 0..samples {
        let t = i as f64 / 255.0;
        let out = func.evaluate(&[t]);
        if cie {
            push_cie_converted(alt, &out, &mut data);
        } else {
            for j in 0..n_out {
                data.push(out.get(j).copied().unwrap_or(0.0) as f32);
            }
        }
    }
    let table = TintLookupTable {
        num_inputs: 1,
        num_outputs: n_out as u32,
        samples_per_dim: samples,
        data,
    };
    ImageColorSpace::Separation {
        name: Vec::new(),
        alt_space: Box::new(alt_cs),
        tint_table: Arc::new(table),
    }
}

/// Build an N-D TintLookupTable for DeviceN image color space.
fn build_nd_tint_image_cs(
    func: &PdfFunction,
    alt: &ResolvedColorSpace,
    n_inputs: usize,
) -> ImageColorSpace {
    let cie = is_cie_space(alt);
    let (alt_cs, n_out) = if cie {
        (ImageColorSpace::DeviceRGB, 3)
    } else {
        (tint_alt_device_cs(alt), alt.num_components())
    };
    // Use fewer samples per dimension for higher-dimensional spaces.
    // Total table entries = spd^n_inputs × n_out, so balance quality vs memory.
    // These tables are used for fills/strokes; images with ≥2 inputs bypass
    // the table via direct per-pixel function evaluation (see mod.rs).
    let spd = match n_inputs {
        1 => 256u32,
        2 => 64,
        3 => 17,
        _ => 9,
    };
    let total: usize = (spd as usize).pow(n_inputs as u32);
    let mut data = Vec::with_capacity(total * n_out);
    let mut inputs = vec![0.0f64; n_inputs];
    for idx in 0..total {
        // Convert linear index to multi-dimensional coordinates
        let mut rem = idx;
        for d in (0..n_inputs).rev() {
            inputs[d] = (rem % spd as usize) as f64 / (spd - 1) as f64;
            rem /= spd as usize;
        }
        let out = func.evaluate(&inputs);
        if cie {
            push_cie_converted(alt, &out, &mut data);
        } else {
            for j in 0..n_out {
                data.push(out.get(j).copied().unwrap_or(0.0) as f32);
            }
        }
    }
    let table = TintLookupTable {
        num_inputs: n_inputs as u32,
        num_outputs: n_out as u32,
        samples_per_dim: spd,
        data,
    };
    ImageColorSpace::DeviceN {
        names: Vec::new(),
        alt_space: Box::new(alt_cs),
        tint_table: Arc::new(table),
    }
}

/// Convert tinting function output components (f64, 0..1) to (R, G, B) bytes
/// through the given alternative color space.
pub fn alt_comps_to_rgb_f64(comps: &[f64], alt: &ResolvedColorSpace) -> (u8, u8, u8) {
    match alt {
        ResolvedColorSpace::DeviceGray => {
            let g = (comps.first().copied().unwrap_or(0.0).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            (g, g, g)
        }
        ResolvedColorSpace::DeviceRGB => {
            let r = (comps.first().copied().unwrap_or(0.0).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            let g = (comps.get(1).copied().unwrap_or(0.0).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            let b = (comps.get(2).copied().unwrap_or(0.0).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            (r, g, b)
        }
        ResolvedColorSpace::DeviceCMYK => {
            let c = comps.first().copied().unwrap_or(0.0).clamp(0.0, 1.0);
            let m = comps.get(1).copied().unwrap_or(0.0).clamp(0.0, 1.0);
            let y = comps.get(2).copied().unwrap_or(0.0).clamp(0.0, 1.0);
            let k = comps.get(3).copied().unwrap_or(0.0).clamp(0.0, 1.0);
            let r = ((1.0 - (c + k).min(1.0)) * 255.0 + 0.5) as u8;
            let g = ((1.0 - (m + k).min(1.0)) * 255.0 + 0.5) as u8;
            let b = ((1.0 - (y + k).min(1.0)) * 255.0 + 0.5) as u8;
            (r, g, b)
        }
        _ => (0, 0, 0),
    }
}
