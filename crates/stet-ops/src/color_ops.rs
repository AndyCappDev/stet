// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Color operators: setgray, currentgray, setrgbcolor, currentrgbcolor,
//! setcmykcolor, currentcmykcolor, sethsbcolor, currenthsbcolor,
//! setcolorspace, currentcolorspace, setcolor, currentcolor.

use std::sync::Arc;

use stet_core::context::Context;
use stet_core::dict::DictKey;
use stet_core::error::PsError;
use stet_core::graphics_state::ColorSpace;
use stet_core::object::{EntityId, PsObject, PsValue};
use stet_graphics::color::{CieAParams, CieAbcParams, CieDefParams, CieDefgParams, DeviceColor};
use stet_graphics::device::{
    CMYK_C, CMYK_K, CMYK_M, CMYK_Y, cmyk_channel_for_name, devicen_process_cmyk,
    separation_process_cmyk,
};
use stet_graphics::icc::{IccCache, IccRenderingIntent, intent_from_byte};

/// The rendering intent colours are converted with: the graphics state's.
fn gstate_intent(ctx: &Context) -> IccRenderingIntent {
    intent_from_byte(ctx.gstate.rendering_intent)
}

/// Drop the pattern half of the current color.
///
/// Every operator that sets a non-pattern color replaces the whole current
/// color, so the pattern, its dictionary, and its underlying components all go
/// with it (PLRM 4.9.6).
pub fn clear_pattern_color(ctx: &mut Context) {
    ctx.gstate.current_pattern = None;
    ctx.gstate.current_pattern_dict = None;
    ctx.gstate.pattern_components.clear();
}

/// Make `space` current after `setgray`, `setrgbcolor`, `setcmykcolor` or
/// `sethsbcolor` has set `comps` in it, remapped under UseCIEColor (PLRM
/// 6.2.5).
///
/// A remapped colour is painted through the `Default*` space: `comps` go to
/// that space's `setcolor`, as they would after `setcolorspace`, and the
/// device colour already set is replaced by what it converts them to. The
/// remapping is meant to be invisible otherwise, so `currentcolorspace` still
/// reports the device space and `currentcolor` the components given. A
/// default space that cannot take the colour — its `setcolor` fails — leaves
/// the device space and colour in place.
fn install_device_space(ctx: &mut Context, space: ColorSpace, name: &[u8], comps: &[f64]) {
    ctx.gstate.color_space = space;
    ctx.gstate.color_space_object = None;
    if !is_use_cie_color(ctx) {
        return;
    }
    let Some(default_space) = lookup_default_colorspace(ctx, name) else {
        return;
    };
    let device_space = std::mem::replace(&mut ctx.gstate.color_space, default_space);
    if paint_through_current_space(ctx, comps) {
        ctx.gstate.color_space_object = Some(PsObject::name_lit(ctx.names.intern(name)));
    } else {
        ctx.gstate.color_space = device_space;
        ctx.gstate.color_components = None;
    }
}

/// Paint `comps`, a colour in a device space already set as `gstate.color`,
/// through the current colour space: the `Default*` space UseCIEColor put in
/// the device space's place. `currentcolor` reports `comps` either way.
///
/// `false`, leaving the device colour, when that space's `setcolor` fails.
fn paint_through_current_space(ctx: &mut Context, comps: &[f64]) -> bool {
    let device_color = ctx.gstate.color.clone();
    let depth = ctx.o_stack.len();
    let pushed = comps
        .iter()
        .all(|&c| ctx.o_stack.push(PsObject::real(c)).is_ok());
    let painted = pushed && set_color_in_space(ctx).is_ok();
    if !painted {
        ctx.o_stack.truncate(depth);
        ctx.gstate.color = device_color;
    }
    ctx.gstate.color_components = Some(Arc::from(comps));
    painted
}

/// The device space the current colour space stands in for, when UseCIEColor
/// has put a `Default*` space in its place: the device space `setcolorspace`
/// or a colour operator named, while `color_space` holds the default.
///
/// `None` in any other space, including a device space a `Default*` resource
/// left as it was (`[/DeviceCMYK]`, for no remapping).
fn remapped_device_space(ctx: &Context) -> Option<ColorSpace> {
    if matches!(
        ctx.gstate.color_space,
        ColorSpace::DeviceGray | ColorSpace::DeviceRGB | ColorSpace::DeviceCMYK
    ) {
        return None;
    }
    let obj = ctx.gstate.color_space_object?;
    let family = match obj.value {
        PsValue::Name(id) => id,
        _ => match array_span(&obj)? {
            (entity, start, 1) => match ctx.arrays.get_element(entity, start).value {
                PsValue::Name(id) => id,
                _ => return None,
            },
            _ => return None,
        },
    };
    match ctx.names.get_bytes(family) {
        b"DeviceGray" => Some(ColorSpace::DeviceGray),
        b"DeviceRGB" => Some(ColorSpace::DeviceRGB),
        b"DeviceCMYK" => Some(ColorSpace::DeviceCMYK),
        _ => None,
    }
}

/// `setcolor` in a device space UseCIEColor remapped: the device space's
/// components, clipped as it clips them, painted through the `Default*` space.
fn set_remapped_device_color(ctx: &mut Context, device: &ColorSpace) -> Result<(), PsError> {
    let comps: Vec<f64> = match device {
        ColorSpace::DeviceGray => set_gray_components(ctx)?.to_vec(),
        ColorSpace::DeviceRGB => set_rgb_components(ctx)?.to_vec(),
        _ => set_cmyk_components(ctx)?.to_vec(),
    };
    paint_through_current_space(ctx, &comps);
    Ok(())
}

/// The device colour `currentgray`, `currentrgbcolor`, `currentcmykcolor`
/// and `currenthsbcolor` describe.
///
/// That is `gstate.color`, except in a space UseCIEColor remapped, where
/// `gstate.color` is what the `Default*` space converted the colour to. The
/// PLRM keeps the remapping out of "the current color values", so these
/// describe the device colour the program set, as Ghostscript does.
fn reported_color(ctx: &mut Context) -> DeviceColor {
    if let Some(device) = remapped_device_space(ctx)
        && let Some(comps) = ctx.gstate.color_components.clone()
    {
        match (device, &comps[..]) {
            (ColorSpace::DeviceGray, &[g]) => return DeviceColor::from_gray(g),
            (ColorSpace::DeviceRGB, &[r, g, b]) => return DeviceColor::from_rgb(r, g, b),
            (ColorSpace::DeviceCMYK, &[c, m, y, k]) => {
                let intent = gstate_intent(ctx);
                return DeviceColor::from_cmyk_icc_with_intent(
                    c,
                    m,
                    y,
                    k,
                    intent,
                    &mut ctx.icc_cache,
                );
            }
            _ => {}
        }
    }
    ctx.gstate.color.clone()
}

/// Pop a gray level and make it the current colour, leaving the colour
/// space as it is: the part of `setgray` that `setcolor` shares.
fn set_gray_components(ctx: &mut Context) -> Result<[f64; 1], PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let gray = obj.as_f64().ok_or(PsError::TypeCheck)?.clamp(0.0, 1.0);
    ctx.o_stack.pop()?;
    ctx.gstate.color = DeviceColor::from_gray(gray);
    ctx.gstate.icc_components = None;
    ctx.gstate.color_components = None;
    clear_pattern_color(ctx);
    Ok([gray])
}

/// `setgray`: num → —
pub fn op_setgray(ctx: &mut Context) -> Result<(), PsError> {
    let comps = set_gray_components(ctx)?;
    install_device_space(ctx, ColorSpace::DeviceGray, b"DeviceGray", &comps);
    Ok(())
}

/// `currentgray`: — → num
pub fn op_currentgray(ctx: &mut Context) -> Result<(), PsError> {
    let gray = reported_color(ctx).to_gray();
    ctx.o_stack.push(PsObject::real(gray))?;
    Ok(())
}

/// Pop red, green and blue and make them the current colour, leaving the
/// colour space as it is: the part of `setrgbcolor` that `setcolor` shares.
fn set_rgb_components(ctx: &mut Context) -> Result<[f64; 3], PsError> {
    if ctx.o_stack.len() < 3 {
        return Err(PsError::StackUnderflow);
    }
    let b_obj = ctx.o_stack.peek(0)?;
    let g_obj = ctx.o_stack.peek(1)?;
    let r_obj = ctx.o_stack.peek(2)?;
    let r = r_obj.as_f64().ok_or(PsError::TypeCheck)?.clamp(0.0, 1.0);
    let g = g_obj.as_f64().ok_or(PsError::TypeCheck)?.clamp(0.0, 1.0);
    let b = b_obj.as_f64().ok_or(PsError::TypeCheck)?.clamp(0.0, 1.0);
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.gstate.color = DeviceColor::from_rgb(r, g, b);
    ctx.gstate.icc_components = None;
    ctx.gstate.color_components = None;
    clear_pattern_color(ctx);
    Ok([r, g, b])
}

/// `setrgbcolor`: r g b → —
pub fn op_setrgbcolor(ctx: &mut Context) -> Result<(), PsError> {
    let comps = set_rgb_components(ctx)?;
    install_device_space(ctx, ColorSpace::DeviceRGB, b"DeviceRGB", &comps);
    Ok(())
}

/// `currentrgbcolor`: — → r g b
pub fn op_currentrgbcolor(ctx: &mut Context) -> Result<(), PsError> {
    let color = reported_color(ctx);
    ctx.o_stack.push(PsObject::real(color.r))?;
    ctx.o_stack.push(PsObject::real(color.g))?;
    ctx.o_stack.push(PsObject::real(color.b))?;
    Ok(())
}

/// Pop cyan, magenta, yellow and black and make them the current colour,
/// converted through the system CMYK profile, leaving the colour space as it
/// is: the part of `setcmykcolor` that `setcolor` shares.
fn set_cmyk_components(ctx: &mut Context) -> Result<[f64; 4], PsError> {
    if ctx.o_stack.len() < 4 {
        return Err(PsError::StackUnderflow);
    }
    let k_obj = ctx.o_stack.peek(0)?;
    let y_obj = ctx.o_stack.peek(1)?;
    let m_obj = ctx.o_stack.peek(2)?;
    let c_obj = ctx.o_stack.peek(3)?;
    let c = c_obj.as_f64().ok_or(PsError::TypeCheck)?.clamp(0.0, 1.0);
    let m = m_obj.as_f64().ok_or(PsError::TypeCheck)?.clamp(0.0, 1.0);
    let y = y_obj.as_f64().ok_or(PsError::TypeCheck)?.clamp(0.0, 1.0);
    let k = k_obj.as_f64().ok_or(PsError::TypeCheck)?.clamp(0.0, 1.0);
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    let intent = gstate_intent(ctx);
    ctx.gstate.color =
        DeviceColor::from_cmyk_icc_with_intent(c, m, y, k, intent, &mut ctx.icc_cache);
    ctx.gstate.icc_components = None;
    ctx.gstate.color_components = None;
    clear_pattern_color(ctx);
    Ok([c, m, y, k])
}

/// `setcmykcolor`: c m y k → —
pub fn op_setcmykcolor(ctx: &mut Context) -> Result<(), PsError> {
    let comps = set_cmyk_components(ctx)?;
    install_device_space(ctx, ColorSpace::DeviceCMYK, b"DeviceCMYK", &comps);
    Ok(())
}

/// `currentcmykcolor`: — → c m y k
pub fn op_currentcmykcolor(ctx: &mut Context) -> Result<(), PsError> {
    let (c, m, y, k) = reported_color(ctx).to_cmyk();
    ctx.o_stack.push(PsObject::real(c))?;
    ctx.o_stack.push(PsObject::real(m))?;
    ctx.o_stack.push(PsObject::real(y))?;
    ctx.o_stack.push(PsObject::real(k))?;
    Ok(())
}

/// `sethsbcolor`: h s b → —
pub fn op_sethsbcolor(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 3 {
        return Err(PsError::StackUnderflow);
    }
    let b_obj = ctx.o_stack.peek(0)?;
    let s_obj = ctx.o_stack.peek(1)?;
    let h_obj = ctx.o_stack.peek(2)?;
    let h = h_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let s = s_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let b = b_obj.as_f64().ok_or(PsError::TypeCheck)?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.gstate.color =
        DeviceColor::from_hsb(h.clamp(0.0, 1.0), s.clamp(0.0, 1.0), b.clamp(0.0, 1.0));
    ctx.gstate.icc_components = None;
    ctx.gstate.color_components = None;
    clear_pattern_color(ctx);
    // `sethsbcolor` sets an RGB colour, so a remap takes its RGB components.
    let comps = [ctx.gstate.color.r, ctx.gstate.color.g, ctx.gstate.color.b];
    install_device_space(ctx, ColorSpace::DeviceRGB, b"DeviceRGB", &comps);
    Ok(())
}

/// `currenthsbcolor`: — → h s b
pub fn op_currenthsbcolor(ctx: &mut Context) -> Result<(), PsError> {
    let (h, s, b) = reported_color(ctx).to_hsb();
    ctx.o_stack.push(PsObject::real(h))?;
    ctx.o_stack.push(PsObject::real(s))?;
    ctx.o_stack.push(PsObject::real(b))?;
    Ok(())
}

/// `setcolorspace`: name → —
pub fn op_setcolorspace(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    match obj.value {
        PsValue::Name(id) => {
            let name = ctx.names.get_bytes(id);
            let mut cs = match name {
                b"DeviceGray" => ColorSpace::DeviceGray,
                b"DeviceRGB" => ColorSpace::DeviceRGB,
                b"DeviceCMYK" => ColorSpace::DeviceCMYK,
                b"Pattern" => ColorSpace::Pattern { base: None },
                _ => return Err(PsError::Undefined),
            };
            ctx.o_stack.pop()?;
            // UseCIEColor remapping (PLRM 6.2.5)
            if is_use_cie_color(ctx) {
                let device_name = match &cs {
                    ColorSpace::DeviceGray => Some(&b"DeviceGray"[..]),
                    ColorSpace::DeviceRGB => Some(&b"DeviceRGB"[..]),
                    ColorSpace::DeviceCMYK => Some(&b"DeviceCMYK"[..]),
                    _ => None,
                };
                if let Some(dn) = device_name
                    && let Some(cie_cs) = lookup_default_colorspace(ctx, dn)
                {
                    cs = cie_cs;
                }
            }
            ctx.gstate.color = default_color_for_space(&cs, ctx);
            ctx.gstate.color_components = initial_components(ctx, &cs);
            ctx.gstate.color_space = cs;
            ctx.gstate.color_space_object = Some(obj);
            clear_pattern_color(ctx);
            install_remapped_initial_color(ctx);
            Ok(())
        }
        // Also accept array form [/DeviceGray] or [/Indexed base hival lookup],
        // a packed array as well as a plain one.
        PsValue::Array { entity, start, len } | PsValue::PackedArray { entity, start, len } => {
            obj.flags.require_read()?;
            if len == 0 {
                return Err(PsError::RangeCheck);
            }
            let first = ctx.arrays.get_element(entity, start);
            if let PsValue::Name(id) = first.value {
                let name = ctx.names.get_bytes(id).to_vec();
                let cs = match name.as_slice() {
                    b"DeviceGray" => ColorSpace::DeviceGray,
                    b"DeviceRGB" => ColorSpace::DeviceRGB,
                    b"DeviceCMYK" => ColorSpace::DeviceCMYK,
                    b"Indexed" if len >= 4 => parse_indexed_colorspace(ctx, entity, start)?,
                    b"CIEBasedABC" => parse_cie_abc_colorspace(ctx, entity, start, len)?,
                    b"CIEBasedA" => parse_cie_a_colorspace(ctx, entity, start, len)?,
                    b"CIEBasedDEF" => parse_cie_def_colorspace(ctx, entity, start, len)?,
                    b"CIEBasedDEFG" => parse_cie_defg_colorspace(ctx, entity, start, len)?,
                    b"ICCBased" => parse_iccbased_colorspace(ctx, entity, start, len)?,
                    b"Separation" if len >= 4 => parse_separation_colorspace(ctx, entity, start)?,
                    b"DeviceN" if len >= 4 => parse_devicen_colorspace(ctx, entity, start)?,
                    b"Pattern" => parse_pattern_colorspace(ctx, entity, start, len)?,
                    _ => return Err(PsError::Undefined),
                };
                let cs = precompute_cie_decode_tables(ctx, cs)?;
                let cs = resolve_indexed_proc_lookup(ctx, cs)?;
                // UseCIEColor remapping for device spaces in array form (PLRM 6.2.5)
                let cs = if is_use_cie_color(ctx) {
                    match &cs {
                        ColorSpace::DeviceGray => {
                            lookup_default_colorspace(ctx, b"DeviceGray").unwrap_or(cs)
                        }
                        ColorSpace::DeviceRGB => {
                            lookup_default_colorspace(ctx, b"DeviceRGB").unwrap_or(cs)
                        }
                        ColorSpace::DeviceCMYK => {
                            lookup_default_colorspace(ctx, b"DeviceCMYK").unwrap_or(cs)
                        }
                        _ => cs,
                    }
                } else {
                    cs
                };
                ctx.o_stack.pop()?;
                ctx.gstate.color = default_color_for_space(&cs, ctx);
                ctx.gstate.color_components = initial_components(ctx, &cs);
                ctx.gstate.color_space = cs;
                ctx.gstate.color_space_object = Some(obj);
                clear_pattern_color(ctx);
                cache_tint_table(ctx);
                install_initial_tints(ctx);
                install_remapped_initial_color(ctx);
                Ok(())
            } else {
                Err(PsError::TypeCheck)
            }
        }
        _ => Err(PsError::TypeCheck),
    }
}

/// The components of the initial colour `setcolorspace` installs, as
/// `currentcolor` reports them (PLRM, `setcolorspace`): each component of
/// a CIE-based space is 0 clipped to its range, an Indexed space starts at
/// index 0, and a Separation or DeviceN space at tint 1.0 for every colorant.
///
/// `None` for the spaces whose components `gstate.color` holds; see
/// [`GraphicsState::color_components`](stet_core::graphics_state::GraphicsState::color_components).
/// An ICCBased space with a profile reports the components
/// [`default_color_for_space`] converted, so call this after it.
fn initial_components(ctx: &Context, cs: &ColorSpace) -> Option<Arc<[f64]>> {
    let zero_in = |range: &[f64]| clamp_to_ranges(&vec![0.0; range.len() / 2], range);
    match cs {
        ColorSpace::CIEBasedABC { params, .. } => Some(zero_in(&params.range_abc)),
        ColorSpace::CIEBasedA { params, .. } => Some(zero_in(&params.range_a)),
        ColorSpace::CIEBasedDEF { params, .. } => Some(zero_in(&params.range_def)),
        ColorSpace::CIEBasedDEFG { params, .. } => Some(zero_in(&params.range_defg)),
        ColorSpace::ICCBased { .. } => ctx.gstate.icc_components.as_deref().map(Arc::from),
        ColorSpace::Indexed { .. } => Some(Arc::from([0.0])),
        ColorSpace::Separation { .. } => Some(Arc::from([1.0])),
        ColorSpace::DeviceN { num_colorants, .. } => {
            Some(vec![1.0; *num_colorants as usize].into())
        }
        ColorSpace::DeviceGray
        | ColorSpace::DeviceRGB
        | ColorSpace::DeviceCMYK
        | ColorSpace::Pattern { .. } => None,
    }
}

/// Paint the initial colour of a device space UseCIEColor remapped: the
/// device space's own initial colour — black, `0 0 0 1` for DeviceCMYK —
/// through the `Default*` space, rather than that space's initial colour,
/// which for a CIEBasedDEFG `DefaultCMYK` is all zeros, no ink.
fn install_remapped_initial_color(ctx: &mut Context) {
    let Some(device) = remapped_device_space(ctx) else {
        return;
    };
    let comps: &[f64] = match device {
        ColorSpace::DeviceGray => &[0.0],
        ColorSpace::DeviceRGB => &[0.0, 0.0, 0.0],
        _ => &[0.0, 0.0, 0.0, 1.0],
    };
    let intent = gstate_intent(ctx);
    ctx.gstate.color = match device {
        ColorSpace::DeviceGray => DeviceColor::from_gray(0.0),
        ColorSpace::DeviceRGB => DeviceColor::from_rgb(0.0, 0.0, 0.0),
        _ => DeviceColor::from_cmyk_icc_with_intent(0.0, 0.0, 0.0, 1.0, intent, &mut ctx.icc_cache),
    };
    ctx.gstate.icc_components = None;
    paint_through_current_space(ctx, comps);
}

/// Paint the initial colour of a Separation or DeviceN space: every tint 1.0,
/// put through the tint transform as `setcolor` would (PLRM, `setcolorspace`).
///
/// Ghostscript runs the transform when the colour is first painted rather
/// than here, so a transform that fails must not fail `setcolorspace`: the
/// colour is then left black, as it was before the space had an initial
/// colour at all. [`cache_tint_table`] has already run the same transform
/// across its whole range by this point, so this adds no failure it could
/// not have hit.
fn install_initial_tints(ctx: &mut Context) {
    let n = match &ctx.gstate.color_space {
        ColorSpace::Separation { .. } => 1,
        ColorSpace::DeviceN { num_colorants, .. } => *num_colorants as usize,
        ColorSpace::Indexed { .. } => {
            install_initial_index(ctx);
            return;
        }
        _ => return,
    };
    let depth = ctx.o_stack.len();
    let pushed = (0..n).all(|_| ctx.o_stack.push(PsObject::real(1.0)).is_ok());
    if pushed && op_setcolor(ctx).is_ok() {
        return;
    }
    ctx.o_stack.truncate(depth);
    ctx.gstate.color = DeviceColor::black();
    ctx.gstate.tint_values = Some(vec![1.0; n]);
    ctx.gstate.color_components = Some(vec![1.0; n].into());
}

/// Paint the initial colour of an Indexed space: palette entry 0, as
/// `setcolor` would set it (PLRM, `setcolorspace`). A lookup procedure or
/// tint transform that fails leaves the colour black, for the reason
/// [`install_initial_tints`] gives.
fn install_initial_index(ctx: &mut Context) {
    let depth = ctx.o_stack.len();
    if ctx.o_stack.push(PsObject::int(0)).is_ok() && op_setcolor(ctx).is_ok() {
        return;
    }
    ctx.o_stack.truncate(depth);
    ctx.gstate.color = DeviceColor::black();
    ctx.gstate.color_components = Some(Arc::from([0.0]));
}

/// Sample and cache the tint transform for Separation/DeviceN color spaces.
/// Called after setcolorspace installs a new color space.
fn cache_tint_table(ctx: &mut Context) {
    match &ctx.gstate.color_space {
        ColorSpace::Separation {
            alt_space,
            tint_transform,
            num_alt_components,
            ..
        } => {
            let tt = *tint_transform;
            let nac = *num_alt_components;
            let alt = alt_space.clone();
            ctx.gstate.cached_tint_table =
                crate::image_ops::sample_device_tint_transform(ctx, tt, 1, &alt, nac).map(Arc::new);
        }
        ColorSpace::DeviceN {
            alt_space,
            num_colorants,
            tint_transform,
            num_alt_components,
            ..
        } => {
            let tt = *tint_transform;
            let nc = *num_colorants;
            let nac = *num_alt_components;
            let alt = alt_space.clone();
            ctx.gstate.cached_tint_table =
                crate::image_ops::sample_device_tint_transform(ctx, tt, nc, &alt, nac)
                    .map(Arc::new);
        }
        _ => {
            ctx.gstate.cached_tint_table = None;
        }
    }
    ctx.gstate.tint_values = None;
}

/// `currentcolorspace`: — → array
///
/// The array `setcolorspace` was given, itself, as Ghostscript returns it; a
/// space set by name, or by a colour operator such as `setgray`, comes back
/// as a new one-element array.
pub fn op_currentcolorspace(ctx: &mut Context) -> Result<(), PsError> {
    let obj = match ctx.gstate.color_space_object {
        Some(obj) if obj.is_array_type() => obj,
        Some(PsObject {
            value: PsValue::Name(id),
            ..
        }) => {
            let entity = crate::vm_ops::alloc_array_from(ctx, &[PsObject::name_lit(id)]);
            crate::vm_ops::make_array_obj(ctx, entity, 1)
        }
        _ => {
            let cs = ctx.gstate.color_space.clone();
            colorspace_object(ctx, &cs)?
        }
    };
    ctx.o_stack.push(obj)?;
    Ok(())
}

/// The operand that names `cs` inside another space's array — the base of an
/// Indexed space, the alternative space of a Separation or DeviceN one: the
/// family name for a device space, as programs write it, and the whole array
/// for any other.
fn nested_space_object(ctx: &mut Context, cs: &ColorSpace) -> Result<PsObject, PsError> {
    let device = match cs {
        ColorSpace::DeviceGray => b"DeviceGray" as &[u8],
        ColorSpace::DeviceRGB => b"DeviceRGB",
        ColorSpace::DeviceCMYK => b"DeviceCMYK",
        _ => return colorspace_object(ctx, cs),
    };
    Ok(PsObject::name_lit(ctx.names.intern(device)))
}

/// Rebuild the PostScript array that names a color space.
///
/// `currentcolorspace` hands this back when there is no array from
/// `setcolorspace` to return, and a Pattern, Indexed, Separation or DeviceN
/// space embeds the result for the space it is built on, so it has to be able
/// to describe every variant.
fn colorspace_object(ctx: &mut Context, cs: &ColorSpace) -> Result<PsObject, PsError> {
    let obj = match cs {
        ColorSpace::Indexed {
            base,
            hival,
            lookup,
            ..
        } => {
            // Return [/Indexed base hival lookup_string]
            let idx_id = ctx.names.intern(b"Indexed");
            let idx_name = PsObject::name_lit(idx_id);

            let base = base.as_ref().clone();
            let base_obj = nested_space_object(ctx, &base)?;

            let hival_obj = PsObject::int(*hival as i32);

            let lookup_clone = lookup.clone();
            let str_entity = crate::vm_ops::alloc_string(ctx, &lookup_clone);
            let str_obj =
                crate::vm_ops::make_string_obj(ctx, str_entity, lookup_clone.len() as u32);

            let items = [idx_name, base_obj, hival_obj, str_obj];
            let entity = crate::vm_ops::alloc_array_from(ctx, &items);
            crate::vm_ops::make_array_obj(ctx, entity, 4)
        }
        ColorSpace::CIEBasedABC { dict_entity, .. } => {
            cie_space_object(ctx, b"CIEBasedABC", *dict_entity)
        }
        ColorSpace::CIEBasedA { dict_entity, .. } => {
            cie_space_object(ctx, b"CIEBasedA", *dict_entity)
        }
        ColorSpace::CIEBasedDEF { dict_entity, .. } => {
            cie_space_object(ctx, b"CIEBasedDEF", *dict_entity)
        }
        ColorSpace::CIEBasedDEFG { dict_entity, .. } => {
            cie_space_object(ctx, b"CIEBasedDEFG", *dict_entity)
        }
        ColorSpace::ICCBased { dict_entity, .. } => {
            cie_space_object(ctx, b"ICCBased", *dict_entity)
        }
        // `[/Pattern]` for colored patterns only, `[/Pattern base]` when the
        // space also accepts uncolored ones (PLRM 4.9.6).
        ColorSpace::Pattern { base } => {
            let pat_id = ctx.names.intern(b"Pattern");
            let pat_name = PsObject::name_lit(pat_id);
            match base {
                Some(base) => {
                    let base = base.as_ref().clone();
                    let base_obj = colorspace_object(ctx, &base)?;
                    let entity = crate::vm_ops::alloc_array_from(ctx, &[pat_name, base_obj]);
                    crate::vm_ops::make_array_obj(ctx, entity, 2)
                }
                None => {
                    let entity = crate::vm_ops::alloc_array_from(ctx, &[pat_name]);
                    crate::vm_ops::make_array_obj(ctx, entity, 1)
                }
            }
        }
        // `[/Separation name alternativeSpace tintTransform]`.
        ColorSpace::Separation {
            name,
            alt_space,
            tint_transform,
            ..
        } => {
            let family = PsObject::name_lit(ctx.names.intern(b"Separation"));
            let colorant = PsObject::name_lit(ctx.names.intern(name));
            let tint_transform = *tint_transform;
            let alt = alt_space.as_ref().clone();
            let alt_obj = nested_space_object(ctx, &alt)?;
            let items = [family, colorant, alt_obj, tint_transform];
            let entity = crate::vm_ops::alloc_array_from(ctx, &items);
            crate::vm_ops::make_array_obj(ctx, entity, 4)
        }
        // `[/DeviceN names alternativeSpace tintTransform]`.
        ColorSpace::DeviceN {
            names,
            alt_space,
            tint_transform,
            ..
        } => {
            let family = PsObject::name_lit(ctx.names.intern(b"DeviceN"));
            let colorants: Vec<PsObject> = names
                .iter()
                .map(|n| PsObject::name_lit(ctx.names.intern(n)))
                .collect();
            let names_entity = crate::vm_ops::alloc_array_from(ctx, &colorants);
            let names_obj =
                crate::vm_ops::make_array_obj(ctx, names_entity, colorants.len() as u32);
            let tint_transform = *tint_transform;
            let alt = alt_space.as_ref().clone();
            let alt_obj = nested_space_object(ctx, &alt)?;
            let items = [family, names_obj, alt_obj, tint_transform];
            let entity = crate::vm_ops::alloc_array_from(ctx, &items);
            crate::vm_ops::make_array_obj(ctx, entity, 4)
        }
        ColorSpace::DeviceGray | ColorSpace::DeviceRGB | ColorSpace::DeviceCMYK => {
            let name_obj = nested_space_object(ctx, cs)?;
            let entity = crate::vm_ops::alloc_array_from(ctx, &[name_obj]);
            crate::vm_ops::make_array_obj(ctx, entity, 1)
        }
    };
    Ok(obj)
}

/// Build `[/<name> dict]` for the dictionary-parameterised color spaces.
fn cie_space_object(ctx: &mut Context, name: &[u8], dict_entity: EntityId) -> PsObject {
    let name_id = ctx.names.intern(name);
    let name_obj = PsObject::name_lit(name_id);
    let dict_obj = crate::vm_ops::make_dict_obj(ctx, dict_entity);
    let entity = crate::vm_ops::alloc_array_from(ctx, &[name_obj, dict_obj]);
    crate::vm_ops::make_array_obj(ctx, entity, 2)
}

/// `setcolor`: comp1 ... compn → — (set color using current color space)
pub fn op_setcolor(ctx: &mut Context) -> Result<(), PsError> {
    if let Some(device) = remapped_device_space(ctx) {
        return set_remapped_device_color(ctx, &device);
    }
    set_color_in_space(ctx)
}

/// Set the colour in `gstate.color_space` from the operand stack, without
/// regard to UseCIEColor: the remapped device spaces call it to paint
/// through their `Default*` space.
fn set_color_in_space(ctx: &mut Context) -> Result<(), PsError> {
    // Clear tint values and ICC components by default; the Separation,
    // DeviceN and ICCBased arms re-set them. Each arm sets the components
    // `currentcolor` reports once its operands are popped, so a `setcolor`
    // that fails leaves them describing the colour still current. The device
    // arms set the colour alone: the space is already the one wanted, and
    // `setgray` and its kin would replace the array `setcolorspace` kept.
    ctx.gstate.tint_values = None;
    ctx.gstate.icc_components = None;
    match ctx.gstate.color_space.clone() {
        ColorSpace::DeviceGray => set_gray_components(ctx).map(drop),
        ColorSpace::DeviceRGB => set_rgb_components(ctx).map(drop),
        ColorSpace::DeviceCMYK => set_cmyk_components(ctx).map(drop),
        ColorSpace::Indexed { .. } => set_indexed_color(ctx),
        ColorSpace::CIEBasedABC { params, .. } => set_cie_abc_color(ctx, &params),
        ColorSpace::CIEBasedA { params, .. } => set_cie_a_color(ctx, &params),
        ColorSpace::CIEBasedDEF { params, .. } => set_cie_def_color(ctx, &params),
        ColorSpace::CIEBasedDEFG { params, .. } => set_cie_defg_color(ctx, &params),
        ColorSpace::ICCBased {
            n,
            profile_hash,
            dict_entity,
        } => {
            if let Some(hash) = profile_hash {
                set_icc_color(ctx, n, &hash, dict_entity)
            } else {
                // No embedded ICC profile — use PLRM formulas, not system ICC
                // profile. The space stays ICCBased, so `setgray` and
                // `setrgbcolor` themselves would be wrong here.
                match n {
                    1 => set_gray_components(ctx).map(drop),
                    3 => set_rgb_components(ctx).map(drop),
                    4 => {
                        // Use from_cmyk (PLRM formula) directly, not op_setcmykcolor
                        // which would apply the system CMYK ICC profile
                        if ctx.o_stack.len() < 4 {
                            return Err(PsError::StackUnderflow);
                        }
                        let k = ctx.o_stack.peek(0)?.as_f64().ok_or(PsError::TypeCheck)?;
                        let y = ctx.o_stack.peek(1)?.as_f64().ok_or(PsError::TypeCheck)?;
                        let m = ctx.o_stack.peek(2)?.as_f64().ok_or(PsError::TypeCheck)?;
                        let c = ctx.o_stack.peek(3)?.as_f64().ok_or(PsError::TypeCheck)?;
                        ctx.o_stack.pop()?;
                        ctx.o_stack.pop()?;
                        ctx.o_stack.pop()?;
                        ctx.o_stack.pop()?;
                        ctx.gstate.color = DeviceColor::from_cmyk(
                            c.clamp(0.0, 1.0),
                            m.clamp(0.0, 1.0),
                            y.clamp(0.0, 1.0),
                            k.clamp(0.0, 1.0),
                        );
                        ctx.gstate.color_components = None;
                        clear_pattern_color(ctx);
                        Ok(())
                    }
                    _ => Err(PsError::RangeCheck),
                }
            }
        }
        ColorSpace::Separation {
            name,
            alt_space,
            tint_transform,
            num_alt_components,
            ..
        } => {
            if ctx.o_stack.is_empty() {
                return Err(PsError::StackUnderflow);
            }
            let tint = ctx.o_stack.peek(0)?.as_f64().ok_or(PsError::TypeCheck)?;
            ctx.o_stack.pop()?;
            let tint_clamped = tint.clamp(0.0, 1.0);
            ctx.gstate.tint_values = Some(vec![tint_clamped]);
            ctx.gstate.color_components = Some(Arc::from([tint_clamped]));
            let process_cmyk = separation_process_cmyk(&name, tint_clamped);
            // A process colorant is its own ink whatever the alternate space
            // says, so it keeps CMYK numbers even behind a Gray/RGB
            // alternate. `All` stays with the alternate-space colour.
            let single_process = matches!(
                cmyk_channel_for_name(&name),
                CMYK_C | CMYK_M | CMYK_Y | CMYK_K
            );
            let colorants = ProcessColorants {
                process_cmyk,
                native_fallback: if single_process { process_cmyk } else { None },
            };
            ctx.o_stack.push(PsObject::real(tint_clamped))?;
            ctx.exec_sync(tint_transform)?;
            set_color_from_tint_result(ctx, num_alt_components, &alt_space, colorants)
        }
        ColorSpace::DeviceN {
            names,
            alt_space,
            num_colorants,
            tint_transform,
            num_alt_components,
            ..
        } => {
            if ctx.o_stack.len() < num_colorants as usize {
                return Err(PsError::StackUnderflow);
            }
            let mut tints = Vec::new();
            for i in (0..num_colorants as usize).rev() {
                let v = ctx.o_stack.peek(i)?.as_f64().ok_or(PsError::TypeCheck)?;
                tints.push(v.clamp(0.0, 1.0));
            }
            ctx.gstate.tint_values = Some(tints.clone());
            ctx.gstate.color_components = Some(Arc::from(tints.as_slice()));
            let colorants = ProcessColorants {
                process_cmyk: devicen_process_cmyk(&names, &tints),
                native_fallback: None,
            };
            for _ in 0..num_colorants {
                ctx.o_stack.pop()?;
            }
            for &t in &tints {
                ctx.o_stack.push(PsObject::real(t))?;
            }
            ctx.exec_sync(tint_transform)?;
            set_color_from_tint_result(ctx, num_alt_components, &alt_space, colorants)
        }
        // `comp1 … compn pattern setcolor` — same as `setpattern` except that
        // the color space is already Pattern and stays as it is.
        ColorSpace::Pattern { base } => {
            crate::halftone_ops::install_pattern_color(ctx, base.as_deref())
        }
    }
}

/// What a Separation/DeviceN colour puts on the process plates, derived from
/// its colorant names and tints before the tint transform runs.
///
/// The tint transform yields only the alternate-space colour, which folds
/// spot ink into process values; overprint simulation needs the process-only
/// part to leave the process plates under a spot untouched.
struct ProcessColorants {
    /// Stored as [`DeviceColor::process_cmyk`].
    process_cmyk: Option<(f64, f64, f64, f64)>,
    /// [`DeviceColor::native_cmyk`] to use when the alternate space supplies
    /// none (a process colorant behind a Gray/RGB alternate).
    native_fallback: Option<(f64, f64, f64, f64)>,
}

/// The device colour of `comps` in `color_space`, as `setcolor` gives
/// one: clamped to 0-1 in a device space, through the CMYK profile for
/// DeviceCMYK, through its profile for ICCBased, and by the CIE
/// equations for a CIE-based space.
///
/// Shadings use it for their colours, and a Separation or DeviceN colour
/// for its alternate space's components after the tint transform.
pub(crate) fn components_to_device_color(
    comps: &[f64],
    color_space: &ColorSpace,
    intent: IccRenderingIntent,
    icc_cache: &mut stet_graphics::icc::IccCache,
) -> DeviceColor {
    match color_space {
        ColorSpace::DeviceGray => {
            DeviceColor::from_gray(comps.first().copied().unwrap_or(0.0).clamp(0.0, 1.0))
        }
        ColorSpace::DeviceRGB if comps.len() >= 3 => DeviceColor::from_rgb(
            comps[0].clamp(0.0, 1.0),
            comps[1].clamp(0.0, 1.0),
            comps[2].clamp(0.0, 1.0),
        ),
        ColorSpace::DeviceCMYK if comps.len() >= 4 => DeviceColor::from_cmyk_icc_with_intent(
            comps[0].clamp(0.0, 1.0),
            comps[1].clamp(0.0, 1.0),
            comps[2].clamp(0.0, 1.0),
            comps[3].clamp(0.0, 1.0),
            intent,
            icc_cache,
        ),
        ColorSpace::CIEBasedABC { params, .. } if comps.len() >= 3 => {
            DeviceColor::from_cie_abc(comps[0], comps[1], comps[2], params)
        }
        ColorSpace::CIEBasedA { params, .. } => {
            DeviceColor::from_cie_a(comps.first().copied().unwrap_or(0.0), params)
        }
        ColorSpace::CIEBasedDEF { params, .. } if comps.len() >= 3 => {
            DeviceColor::from_cie_def(comps[0], comps[1], comps[2], params)
        }
        ColorSpace::CIEBasedDEFG { params, .. } if comps.len() >= 4 => {
            DeviceColor::from_cie_defg(comps[0], comps[1], comps[2], comps[3], params)
        }
        ColorSpace::ICCBased {
            n, profile_hash, ..
        } => {
            if let Some(hash) = profile_hash
                && let Some((r, g, b)) = icc_cache.convert_color_with_intent(hash, comps, intent)
            {
                return DeviceColor::from_rgb(r, g, b);
            }
            match n {
                1 => DeviceColor::from_gray(comps.first().copied().unwrap_or(0.0).clamp(0.0, 1.0)),
                3 if comps.len() >= 3 => DeviceColor::from_rgb(
                    comps[0].clamp(0.0, 1.0),
                    comps[1].clamp(0.0, 1.0),
                    comps[2].clamp(0.0, 1.0),
                ),
                4 if comps.len() >= 4 => DeviceColor::from_cmyk_icc_with_intent(
                    comps[0].clamp(0.0, 1.0),
                    comps[1].clamp(0.0, 1.0),
                    comps[2].clamp(0.0, 1.0),
                    comps[3].clamp(0.0, 1.0),
                    intent,
                    icc_cache,
                ),
                _ => DeviceColor::from_gray(0.0),
            }
        }
        _ => {
            // Fallback: heuristic based on count
            if comps.len() >= 4 {
                DeviceColor::from_cmyk_icc_with_intent(
                    comps[0].clamp(0.0, 1.0),
                    comps[1].clamp(0.0, 1.0),
                    comps[2].clamp(0.0, 1.0),
                    comps[3].clamp(0.0, 1.0),
                    intent,
                    icc_cache,
                )
            } else if comps.len() >= 3 {
                DeviceColor::from_rgb(
                    comps[0].clamp(0.0, 1.0),
                    comps[1].clamp(0.0, 1.0),
                    comps[2].clamp(0.0, 1.0),
                )
            } else {
                DeviceColor::from_gray(comps.first().copied().unwrap_or(0.0).clamp(0.0, 1.0))
            }
        }
    }
}

/// The alternate space's components of `comps` in a Separation or DeviceN
/// space, by running its tint transform; `comps` themselves in any other.
pub(crate) fn convert_tint_color(
    ctx: &mut Context,
    comps: &[f64],
    color_space: &ColorSpace,
) -> Result<Vec<f64>, PsError> {
    let (tint_transform, num_alt) = match color_space {
        ColorSpace::Separation {
            tint_transform,
            num_alt_components,
            ..
        }
        | ColorSpace::DeviceN {
            tint_transform,
            num_alt_components,
            ..
        } => (*tint_transform, *num_alt_components as usize),
        _ => return Ok(comps.to_vec()),
    };
    for c in comps {
        ctx.o_stack.push(PsObject::real(*c))?;
    }
    ctx.exec_sync(tint_transform)?;
    let mut result = vec![0.0; num_alt];
    for i in (0..num_alt).rev() {
        result[i] = ctx.o_stack.pop()?.as_f64().unwrap_or(0.0);
    }
    Ok(result)
}

/// The base space's components of palette entry `index` of an Indexed
/// space (PLRM 3e §4.8.4): the procedure's results when the lookup is a
/// procedure, or the entry's bytes divided by 255 when it is a string.
/// `index` is rounded and clipped to 0 to `hival`, as `setcolor` takes it.
///
/// A procedure gives components in the base space's own ranges, such as
/// L* 0 to 100 for a Lab base, which a byte table cannot hold; the PLRM
/// gives that as the reason to use one.
fn indexed_entry(
    ctx: &mut Context,
    base: &ColorSpace,
    hival: u32,
    lookup: &[u8],
    lookup_proc: Option<PsObject>,
    index: f64,
) -> Result<Vec<f64>, PsError> {
    let index = index.round().clamp(0.0, hival as f64) as usize;
    let m = color_space_components(base);
    if let Some(proc_obj) = lookup_proc {
        ctx.o_stack.push(PsObject::int(index as i64))?;
        ctx.exec_sync(proc_obj)?;
        if ctx.o_stack.len() < m {
            return Err(PsError::StackUnderflow);
        }
        let mut comps = vec![0.0; m];
        for c in comps.iter_mut().rev() {
            *c = ctx.o_stack.pop()?.as_f64().ok_or(PsError::TypeCheck)?;
        }
        return Ok(comps);
    }
    let start = index * m;
    let bytes = lookup.get(start..start + m).ok_or(PsError::RangeCheck)?;
    Ok(bytes.iter().map(|&b| b as f64 / 255.0).collect())
}

/// The device colour of `comps` in `color_space`, through every space the
/// colour passes on the way: an Indexed colour's palette entry in the base
/// space, a Separation or DeviceN colour's tint transform into the
/// alternate space, and then [`components_to_device_color`]. Also returns
/// the components that last step took, which are in the space a shading's
/// colour space is captured as.
///
/// An Indexed space over anything but a device space came out black, and
/// one over a Separation or DeviceN space never ran its tint transform.
pub(crate) fn resolve_components_color(
    ctx: &mut Context,
    comps: &[f64],
    color_space: &ColorSpace,
) -> Result<(DeviceColor, Vec<f64>), PsError> {
    match color_space {
        ColorSpace::Indexed {
            base,
            hival,
            lookup,
            lookup_proc,
        } => {
            let index = comps.first().copied().unwrap_or(0.0);
            let entry = indexed_entry(ctx, base, *hival, lookup, *lookup_proc, index)?;
            resolve_components_color(ctx, &entry, base)
        }
        ColorSpace::Separation { alt_space, .. } | ColorSpace::DeviceN { alt_space, .. } => {
            let alt = convert_tint_color(ctx, comps, color_space)?;
            resolve_components_color(ctx, &alt, alt_space)
        }
        _ => {
            let intent = gstate_intent(ctx);
            let color = components_to_device_color(comps, color_space, intent, &mut ctx.icc_cache);
            Ok((color, comps.to_vec()))
        }
    }
}

/// Pop tint transform results from the stack and set the device color.
///
/// The components are the alternate space's, and are converted as that
/// space converts them. Taking them by their number alone, as one gray,
/// three RGB or four CMYK values clamped to 0-1, turned a CIE-based
/// alternate's L* a* b* into RGB: a green spot colour whose alternate is
/// Lab came out magenta.
fn set_color_from_tint_result(
    ctx: &mut Context,
    n: u32,
    alt_space: &ColorSpace,
    colorants: ProcessColorants,
) -> Result<(), PsError> {
    let n = n as usize;
    let mut components = vec![0.0f64; n];
    for i in (0..n).rev() {
        if !ctx.o_stack.is_empty() {
            components[i] = ctx.o_stack.peek(0)?.as_f64().unwrap_or(0.0);
            ctx.o_stack.pop()?;
        }
    }

    let intent = gstate_intent(ctx);
    ctx.gstate.color =
        components_to_device_color(&components, alt_space, intent, &mut ctx.icc_cache);
    if ctx.gstate.color.native_cmyk.is_none() {
        ctx.gstate.color.native_cmyk = colorants.native_fallback;
    }
    ctx.gstate.color.process_cmyk = colorants.process_cmyk;
    clear_pattern_color(ctx);
    Ok(())
}

/// `currentcolor`: — → comp1 ... compn (color in current color space)
///
/// The components as `setcolor` took them, in the current space — the index
/// of an Indexed colour, the tints of a Separation or DeviceN one, the inputs
/// of a CIE-based one — not the colour they were converted to. In a device
/// space, and an ICCBased one read without a profile, the colour holds them
/// exactly; the same is true of a device space UseCIEColor remapped, whose
/// default CIE space takes the device components unchanged.
pub fn op_currentcolor(ctx: &mut Context) -> Result<(), PsError> {
    // The pattern is part of the current color, so it comes back with any
    // base-space components an uncolored pattern was given (PLRM 4.9.6).
    // Before the first `setcolor` the color is the null pattern.
    if let ColorSpace::Pattern { .. } = ctx.gstate.color_space {
        let comps = ctx.gstate.pattern_components.clone();
        let dict = ctx.gstate.current_pattern_dict;
        for c in comps {
            ctx.o_stack.push(PsObject::real(c))?;
        }
        match dict {
            Some(entity) => {
                let obj = crate::vm_ops::make_dict_obj(ctx, entity);
                ctx.o_stack.push(obj)?;
            }
            None => ctx.o_stack.push(PsObject::null())?,
        }
        return Ok(());
    }
    if let Some(comps) = ctx.gstate.color_components.clone() {
        // An index is an integer, and Ghostscript returns it as one.
        let indexed = matches!(ctx.gstate.color_space, ColorSpace::Indexed { .. });
        for &c in comps.iter() {
            ctx.o_stack.push(if indexed {
                PsObject::int(c as i64)
            } else {
                PsObject::real(c)
            })?;
        }
        return Ok(());
    }
    match color_space_components(&ctx.gstate.color_space) {
        1 => ctx
            .o_stack
            .push(PsObject::real(ctx.gstate.color.to_gray()))?,
        3 => {
            ctx.o_stack.push(PsObject::real(ctx.gstate.color.r))?;
            ctx.o_stack.push(PsObject::real(ctx.gstate.color.g))?;
            ctx.o_stack.push(PsObject::real(ctx.gstate.color.b))?;
        }
        4 => {
            let (c, m, y, k) = ctx.gstate.color.to_cmyk();
            ctx.o_stack.push(PsObject::real(c))?;
            ctx.o_stack.push(PsObject::real(m))?;
            ctx.o_stack.push(PsObject::real(y))?;
            ctx.o_stack.push(PsObject::real(k))?;
        }
        _ => {}
    }
    Ok(())
}

/// Set color from ICCBased color space with an actual ICC profile.
/// Pops N components, converts through ICC profile, stores as DeviceColor,
/// and keeps the components for [`reconvert_color_for_intent`].
fn set_icc_color(
    ctx: &mut Context,
    n: u32,
    hash: &[u8; 32],
    dict_entity: EntityId,
) -> Result<(), PsError> {
    if (ctx.o_stack.len() as u32) < n {
        return Err(PsError::StackUnderflow);
    }
    // Validate types before popping
    for i in 0..n {
        let obj = ctx.o_stack.peek(i as usize)?;
        match obj.value {
            PsValue::Int(_) | PsValue::Real(_) => {}
            _ => return Err(PsError::TypeCheck),
        }
    }
    // Pop components (top of stack = last component)
    let mut comps = vec![0.0f64; n as usize];
    for i in (0..n as usize).rev() {
        let obj = ctx.o_stack.pop()?;
        comps[i] = match obj.value {
            PsValue::Int(v) => v as f64,
            PsValue::Real(v) => v,
            _ => 0.0,
        };
    }
    // Each component is clipped to the space's /Range, as Ghostscript does,
    // before it is converted and before `currentcolor` reports it.
    let comps = clamp_to_ranges(&comps, &icc_range(ctx, dict_entity, n)).to_vec();
    ctx.gstate.color = icc_device_color(ctx, n, hash, &comps);
    ctx.gstate.color_components = Some(Arc::from(comps.as_slice()));
    ctx.gstate.icc_components = Some(comps);
    Ok(())
}

/// An ICCBased space's `/Range`: a `[min max]` pair for each of its `n`
/// components, `[0 1]` for each when the dictionary gives none or gives one
/// of the wrong length.
pub(crate) fn icc_range(ctx: &Context, dict_entity: EntityId, n: u32) -> Vec<f64> {
    let default: Vec<f64> = (0..n).flat_map(|_| [0.0, 1.0]).collect();
    let range = get_cie_float_array(ctx, dict_entity, b"Range", &default);
    if range.len() == default.len() {
        range
    } else {
        default
    }
}

/// Convert components in an ICCBased space with a profile to the device
/// colour, through the profile, or through the device space with as many
/// components when the profile cannot convert them.
fn icc_device_color(ctx: &mut Context, n: u32, hash: &[u8; 32], comps: &[f64]) -> DeviceColor {
    let intent = gstate_intent(ctx);
    if let Some((r, g, b)) = ctx.icc_cache.convert_color_with_intent(hash, comps, intent) {
        return DeviceColor::from_rgb(r, g, b);
    }
    match n {
        1 => DeviceColor::from_gray(comps[0].clamp(0.0, 1.0)),
        3 => DeviceColor::from_rgb(
            comps[0].clamp(0.0, 1.0),
            comps[1].clamp(0.0, 1.0),
            comps[2].clamp(0.0, 1.0),
        ),
        4 => DeviceColor::from_cmyk_icc_with_intent(
            comps[0].clamp(0.0, 1.0),
            comps[1].clamp(0.0, 1.0),
            comps[2].clamp(0.0, 1.0),
            comps[3].clamp(0.0, 1.0),
            intent,
            &mut ctx.icc_cache,
        ),
        _ => DeviceColor::black(),
    }
}

/// Set color in an Indexed color space: index → —
///
/// Sets the colour the index's palette entry gives in the base space
/// ([`resolve_components_color`]). A lookup procedure runs above the
/// index, which is popped only once it has succeeded.
fn set_indexed_color(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let idx = match ctx.o_stack.peek(0)?.value {
        PsValue::Int(v) => v as f64,
        PsValue::Real(v) => v,
        _ => return Err(PsError::TypeCheck),
    };
    let space = ctx.gstate.color_space.clone();
    let ColorSpace::Indexed { hival, .. } = space else {
        return Err(PsError::TypeCheck);
    };
    let (color, _) = resolve_components_color(ctx, &[idx], &space)?;
    let index = idx.round().clamp(0.0, hival as f64);

    ctx.o_stack.pop()?;
    ctx.gstate.color = color;
    ctx.gstate.color_components = Some(Arc::from([index]));
    clear_pattern_color(ctx);
    Ok(())
}

/// Return the default (initial) color for a color space per PLRM.
///
/// Also records the components of an ICCBased default for
/// [`reconvert_color_for_intent`], and clears them for any other space.
fn default_color_for_space(cs: &ColorSpace, ctx: &mut Context) -> DeviceColor {
    let intent = gstate_intent(ctx);
    ctx.gstate.icc_components = None;
    match cs {
        ColorSpace::DeviceGray => DeviceColor::from_gray(0.0),
        ColorSpace::DeviceRGB => DeviceColor::from_rgb(0.0, 0.0, 0.0),
        ColorSpace::DeviceCMYK => {
            DeviceColor::from_cmyk_icc_with_intent(0.0, 0.0, 0.0, 1.0, intent, &mut ctx.icc_cache)
        }
        ColorSpace::CIEBasedABC { params, .. } => DeviceColor::from_cie_abc(0.0, 0.0, 0.0, params),
        ColorSpace::CIEBasedA { params, .. } => DeviceColor::from_cie_a(0.0, params),
        ColorSpace::CIEBasedDEF { params, .. } => DeviceColor::from_cie_def(0.0, 0.0, 0.0, params),
        ColorSpace::CIEBasedDEFG { params, .. } => {
            DeviceColor::from_cie_defg(0.0, 0.0, 0.0, 0.0, params)
        }
        ColorSpace::ICCBased {
            n,
            profile_hash,
            dict_entity,
        } => {
            let Some(hash) = profile_hash else {
                // Without a profile it can use, the space stands for the
                // device space with as many components, which is what
                // Ghostscript substitutes, and starts at that space's
                // initial colour: black.
                return match n {
                    1 => DeviceColor::from_gray(0.0),
                    3 => DeviceColor::from_rgb(0.0, 0.0, 0.0),
                    4 => DeviceColor::from_cmyk_icc_with_intent(
                        0.0,
                        0.0,
                        0.0,
                        1.0,
                        intent,
                        &mut ctx.icc_cache,
                    ),
                    _ => DeviceColor::black(),
                };
            };
            // Each component starts at 0, clipped to /Range (PLRM,
            // `setcolorspace`) — for a CMYK profile no ink, white, not the
            // `0 0 0 1` DeviceCMYK starts at.
            let comps = clamp_to_ranges(&vec![0.0; *n as usize], &icc_range(ctx, *dict_entity, *n));
            let color = icc_device_color(ctx, *n, hash, &comps);
            ctx.gstate.icc_components = Some(comps.to_vec());
            color
        }
        // Indexed: the initial colour is palette entry 0, which
        // `install_initial_tints` sets once the space is current, since a
        // lookup procedure or a Separation or DeviceN base runs PostScript.
        ColorSpace::Indexed { .. } => DeviceColor::black(),
        // Separation/DeviceN: the initial colour is the tint transform's
        // result at tint 1.0, which `install_initial_tints` sets once the
        // space is current. Black stands in until then, and if it fails.
        ColorSpace::Separation { .. } | ColorSpace::DeviceN { .. } => DeviceColor::black(),
        // The initial color in a Pattern space is the null pattern, which paints
        // nothing; the device color only matters once a pattern is installed.
        ColorSpace::Pattern { .. } => DeviceColor::black(),
    }
}

/// Convert the current colour again under the graphics state's rendering
/// intent, after `setrenderingintent` changes it.
///
/// stet converts a colour when it is set, but the intent that applies is the
/// one in force when it is painted: `setrenderingintent` is the PDF `ri`
/// operator, and Ghostscript changes the colour of a fill that follows `ri`
/// without a new colour. Only conversions through a CMYK
/// profile depend on the intent, so this re-converts the CMYK a colour was
/// set with and leaves every other colour alone. The underlying colour of an
/// uncoloured pattern is brought up to date the same way.
pub fn reconvert_color_for_intent(ctx: &mut Context) {
    let intent = gstate_intent(ctx);
    let gs = &mut ctx.gstate;
    if let Some(color) = color_for_intent(
        &mut ctx.icc_cache,
        &gs.color_space,
        &gs.color,
        gs.icc_components.as_deref(),
        intent,
    ) {
        gs.color = color;
    }
    if let ColorSpace::Pattern { base: Some(base) } = &gs.color_space
        && let Some(under) = &gs.pattern_underlying_color
        && let Some(color) = color_for_intent(
            &mut ctx.icc_cache,
            base,
            under,
            Some(&gs.pattern_components),
            intent,
        )
    {
        gs.pattern_underlying_color = Some(color);
    }
}

/// `color`, set in `space`, converted again with `intent`; `None` when its
/// conversion does not depend on the intent or cannot be repeated.
///
/// Repeats the conversion the colour was set with: CMYK reaching the default
/// CMYK profile (DeviceCMYK, an Indexed palette over it, or the alternate of
/// a Separation or DeviceN space), recorded in `native_cmyk`; or the
/// components of an ICCBased colour through its own profile.
fn color_for_intent(
    icc: &mut IccCache,
    space: &ColorSpace,
    color: &DeviceColor,
    icc_components: Option<&[f64]>,
    intent: IccRenderingIntent,
) -> Option<DeviceColor> {
    let rgb = match space {
        ColorSpace::ICCBased {
            profile_hash: Some(hash),
            ..
        } => match icc_components {
            Some(comps) => icc.convert_color_with_intent(hash, comps, intent)?,
            // An ICCBased space installed by UseCIEColor as DefaultCMYK
            // holds colours `setcmykcolor` converted.
            None => {
                let (c, m, y, k) = color.native_cmyk?;
                icc.convert_cmyk_with_intent(c, m, y, k, intent)?
            }
        },
        ColorSpace::DeviceCMYK => {
            let (c, m, y, k) = color.native_cmyk?;
            icc.convert_cmyk_with_intent(c, m, y, k, intent)?
        }
        ColorSpace::Indexed { base, .. } if matches!(base.as_ref(), ColorSpace::DeviceCMYK) => {
            let (c, m, y, k) = color.native_cmyk?;
            icc.convert_cmyk_with_intent(c, m, y, k, intent)?
        }
        // A Gray or RGB alternate converts without a profile, though a
        // process colorant behind one still records `native_cmyk`.
        ColorSpace::Separation {
            num_alt_components: 4,
            ..
        }
        | ColorSpace::DeviceN {
            num_alt_components: 4,
            ..
        } => {
            let (c, m, y, k) = color.native_cmyk?;
            icc.convert_cmyk_with_intent(c, m, y, k, intent)?
        }
        _ => return None,
    };
    let (r, g, b) = rgb;
    Some(DeviceColor {
        r,
        g,
        b,
        ..color.clone()
    })
}

/// The elements of an array or a packed array: entity, start and length.
///
/// A colour space array, and every array inside one, may be either: a program
/// run with `true setpacking` builds packed arrays throughout, and Ghostscript
/// accepts them anywhere a colour space takes an array.
fn array_span(obj: &PsObject) -> Option<(EntityId, u32, u32)> {
    match obj.value {
        PsValue::Array { entity, start, len } | PsValue::PackedArray { entity, start, len } => {
            Some((entity, start, len))
        }
        _ => None,
    }
}

/// Whether `obj` is a procedure: an executable array or packed array.
fn is_procedure(obj: &PsObject) -> bool {
    obj.is_array_type() && obj.flags.is_executable()
}

/// Resolve a color space from a PsObject (name or array).
/// Returns the ColorSpace and its number of components.
pub fn resolve_color_space_from_obj(
    ctx: &mut Context,
    obj: &PsObject,
) -> Result<(ColorSpace, usize), PsError> {
    match obj.value {
        PsValue::Name(id) => {
            let name = ctx.names.get_bytes(id);
            match name {
                b"DeviceGray" => Ok((ColorSpace::DeviceGray, 1)),
                b"DeviceRGB" => Ok((ColorSpace::DeviceRGB, 3)),
                b"DeviceCMYK" => Ok((ColorSpace::DeviceCMYK, 4)),
                b"Pattern" => Ok((ColorSpace::Pattern { base: None }, 0)),
                _ => Err(PsError::Undefined),
            }
        }
        PsValue::Array { entity, start, len } | PsValue::PackedArray { entity, start, len } => {
            if len == 0 {
                return Err(PsError::RangeCheck);
            }
            let first = ctx.arrays.get_element(entity, start);
            if let PsValue::Name(id) = first.value {
                let name = ctx.names.get_bytes(id).to_vec();
                let cs = match name.as_slice() {
                    b"DeviceGray" => ColorSpace::DeviceGray,
                    b"DeviceRGB" => ColorSpace::DeviceRGB,
                    b"DeviceCMYK" => ColorSpace::DeviceCMYK,
                    b"Indexed" if len >= 4 => parse_indexed_colorspace(ctx, entity, start)?,
                    b"CIEBasedABC" => parse_cie_abc_colorspace(ctx, entity, start, len)?,
                    b"CIEBasedA" => parse_cie_a_colorspace(ctx, entity, start, len)?,
                    b"CIEBasedDEF" => parse_cie_def_colorspace(ctx, entity, start, len)?,
                    b"CIEBasedDEFG" => parse_cie_defg_colorspace(ctx, entity, start, len)?,
                    b"ICCBased" => parse_iccbased_colorspace(ctx, entity, start, len)?,
                    b"Separation" if len >= 4 => parse_separation_colorspace(ctx, entity, start)?,
                    b"DeviceN" if len >= 4 => parse_devicen_colorspace(ctx, entity, start)?,
                    b"Pattern" => parse_pattern_colorspace(ctx, entity, start, len)?,
                    _ => return Err(PsError::Undefined),
                };
                let n_comps = color_space_components(&cs);
                Ok((cs, n_comps))
            } else {
                Err(PsError::TypeCheck)
            }
        }
        _ => Err(PsError::TypeCheck),
    }
}

/// Parse `[/Pattern]` or `[/Pattern base]` from an array (PLRM 4.9.6).
///
/// The optional base is the underlying space that supplies the color of an
/// uncolored (PaintType 2) pattern. It may be any color space except another
/// Pattern space.
fn parse_pattern_colorspace(
    ctx: &mut Context,
    entity: stet_core::object::EntityId,
    start: u32,
    len: u32,
) -> Result<ColorSpace, PsError> {
    if len < 2 {
        return Ok(ColorSpace::Pattern { base: None });
    }
    let base_obj = ctx.arrays.get_element(entity, start + 1);
    let (base, _) = resolve_color_space_from_obj(ctx, &base_obj)?;
    if matches!(base, ColorSpace::Pattern { .. }) {
        return Err(PsError::RangeCheck);
    }
    let base = precompute_cie_decode_tables(ctx, base)?;
    let base = resolve_indexed_proc_lookup(ctx, base)?;
    Ok(ColorSpace::Pattern {
        base: Some(Box::new(base)),
    })
}

/// Parse `[/Indexed base hival lookup]` from an array.
fn parse_indexed_colorspace(
    ctx: &mut Context,
    entity: stet_core::object::EntityId,
    start: u32,
) -> Result<ColorSpace, PsError> {
    // Element 1: the base color space. PLRM 4.9.3 allows any color space here
    // except another Indexed space and Pattern — notably the array forms, which
    // is how `pdftops` writes an indexed image over a CIE base:
    // `[/Indexed [/CIEBasedDEFG <<...>>] 255 <lookup>]`.
    let base_obj = ctx.arrays.get_element(entity, start + 1);
    let base = match base_obj.value {
        PsValue::Name(id) => {
            let name = ctx.names.get_bytes(id);
            match name {
                b"DeviceGray" => ColorSpace::DeviceGray,
                b"DeviceRGB" => ColorSpace::DeviceRGB,
                b"DeviceCMYK" => ColorSpace::DeviceCMYK,
                _ => return Err(PsError::RangeCheck),
            }
        }
        PsValue::Array { .. } | PsValue::PackedArray { .. } => {
            let (cs, _) = resolve_color_space_from_obj(ctx, &base_obj)?;
            // An Indexed space may not be based on another Indexed space.
            if matches!(cs, ColorSpace::Indexed { .. }) {
                return Err(PsError::RangeCheck);
            }
            cs
        }
        _ => return Err(PsError::TypeCheck),
    };

    // Element 2: hival, the largest index: up to 4095, enough for a
    // 12-bit image (PLRM 3e §4.8.4).
    let hival_obj = ctx.arrays.get_element(entity, start + 2);
    let hival_i = hival_obj.as_i32().ok_or(PsError::TypeCheck)?;
    if !(0..=4095).contains(&hival_i) {
        return Err(PsError::RangeCheck);
    }
    let hival = hival_i as u32;

    // Element 3: lookup table (string of bytes) or procedure
    let lookup_obj = ctx.arrays.get_element(entity, start + 3);
    let (lookup, lookup_proc) = match lookup_obj.value {
        PsValue::String {
            entity: se,
            start,
            len,
        } => (ctx.strings.get(se, start, len).to_vec(), None),
        PsValue::ExecArray { .. } | PsValue::Array { .. } | PsValue::PackedArray { .. } => {
            // Procedure lookup — will be pre-evaluated in resolve_indexed_proc_lookup
            (Vec::new(), Some(lookup_obj))
        }
        _ => return Err(PsError::TypeCheck),
    };

    if lookup_proc.is_none() {
        // Validate lookup length: must have (hival+1) * components_per_color bytes
        let required_len = (hival as usize + 1) * color_space_components(&base);
        if lookup.len() < required_len {
            return Err(PsError::RangeCheck);
        }
    }

    Ok(ColorSpace::Indexed {
        base: Box::new(base),
        hival,
        lookup,
        lookup_proc,
    })
}

/// Check if a dict contains a /WhitePoint entry.
fn has_white_point(ctx: &Context, dict_entity: EntityId) -> bool {
    match ctx.names.find(b"WhitePoint") {
        Some(id) => ctx.dicts.known(dict_entity, &DictKey::Name(id)),
        None => false,
    }
}

/// Check if `UseCIEColor` is true in the current page device dictionary.
fn is_use_cie_color(ctx: &Context) -> bool {
    let pd = match ctx.gstate.page_device {
        Some(pd) => pd,
        None => return false,
    };
    let key_id = match ctx.names.find(b"UseCIEColor") {
        Some(id) => id,
        None => return false,
    };
    match ctx.dicts.get(pd, &DictKey::Name(key_id)) {
        Some(obj) => matches!(obj.value, PsValue::Bool(true)),
        None => false,
    }
}

/// Look up a Default* ColorSpace resource and parse it into a `ColorSpace`.
/// Maps "DeviceGray" → "DefaultGray", "DeviceRGB" → "DefaultRGB",
/// "DeviceCMYK" → "DefaultCMYK". Returns None if not found or not parseable.
fn lookup_default_colorspace(ctx: &mut Context, device_space: &[u8]) -> Option<ColorSpace> {
    let default_name = match device_space {
        b"DeviceGray" => b"DefaultGray" as &[u8],
        b"DeviceRGB" => b"DefaultRGB",
        b"DeviceCMYK" => b"DefaultCMYK",
        _ => return None,
    };

    // Look up in ColorSpace resource category (local then global)
    let cat_name_id = ctx.names.find(b"ColorSpace")?;
    let cat_key = DictKey::Name(cat_name_id);

    let res_name_id = ctx.names.intern(default_name);
    let res_key = DictKey::Name(res_name_id);

    // Search local resources first, then global
    let resource_obj = if !ctx.vm_alloc_mode {
        ctx.dicts
            .get(ctx.local_resources, &cat_key)
            .and_then(|cat_obj| {
                if let PsValue::Dict(cat) = cat_obj.value {
                    ctx.dicts.get(cat, &res_key)
                } else {
                    None
                }
            })
    } else {
        None
    }
    .or_else(|| {
        ctx.dicts
            .get(ctx.global_resources, &cat_key)
            .and_then(|cat_obj| {
                if let PsValue::Dict(cat) = cat_obj.value {
                    ctx.dicts.get(cat, &res_key)
                } else {
                    None
                }
            })
    })?;

    // Parse the resource array as a color space
    let (entity, start, len) = array_span(&resource_obj)?;
    if len == 0 {
        return None;
    }
    let first = ctx.arrays.get_element(entity, start);
    let cs = if let PsValue::Name(id) = first.value {
        let name = ctx.names.get_bytes(id).to_vec();
        match name.as_slice() {
            b"DeviceGray" => Some(ColorSpace::DeviceGray),
            b"DeviceRGB" => Some(ColorSpace::DeviceRGB),
            b"DeviceCMYK" => Some(ColorSpace::DeviceCMYK),
            b"CIEBasedABC" => parse_cie_abc_colorspace(ctx, entity, start, len).ok(),
            b"CIEBasedA" => parse_cie_a_colorspace(ctx, entity, start, len).ok(),
            b"CIEBasedDEF" => parse_cie_def_colorspace(ctx, entity, start, len).ok(),
            b"CIEBasedDEFG" => parse_cie_defg_colorspace(ctx, entity, start, len).ok(),
            _ => None,
        }
    } else {
        None
    }?;
    // Precompute CIE decode tables if needed
    precompute_cie_decode_tables(ctx, cs).ok()
}

/// Parse `[/CIEBasedABC dict]` from an array.
fn parse_cie_abc_colorspace(
    ctx: &Context,
    entity: EntityId,
    start: u32,
    len: u32,
) -> Result<ColorSpace, PsError> {
    if len != 2 {
        return Err(PsError::RangeCheck);
    }
    let dict_obj = ctx.arrays.get_element(entity, start + 1);
    let dict_entity = match dict_obj.value {
        PsValue::Dict(e) => e,
        _ => return Err(PsError::TypeCheck),
    };
    // WhitePoint is required
    if !has_white_point(ctx, dict_entity) {
        return Err(PsError::RangeCheck);
    }
    let params = extract_cie_abc_params(ctx, dict_entity);
    Ok(ColorSpace::CIEBasedABC {
        params: Arc::new(params),
        dict_entity,
    })
}

/// Parse `[/CIEBasedA dict]` from an array.
fn parse_cie_a_colorspace(
    ctx: &Context,
    entity: EntityId,
    start: u32,
    len: u32,
) -> Result<ColorSpace, PsError> {
    if len != 2 {
        return Err(PsError::RangeCheck);
    }
    let dict_obj = ctx.arrays.get_element(entity, start + 1);
    let dict_entity = match dict_obj.value {
        PsValue::Dict(e) => e,
        _ => return Err(PsError::TypeCheck),
    };
    // WhitePoint is required
    if !has_white_point(ctx, dict_entity) {
        return Err(PsError::RangeCheck);
    }
    let params = extract_cie_a_params(ctx, dict_entity);
    Ok(ColorSpace::CIEBasedA {
        params: Arc::new(params),
        dict_entity,
    })
}

/// Parse `[/CIEBasedDEF dict]` from an array.
fn parse_cie_def_colorspace(
    ctx: &Context,
    entity: EntityId,
    start: u32,
    len: u32,
) -> Result<ColorSpace, PsError> {
    if len != 2 {
        return Err(PsError::RangeCheck);
    }
    let dict_obj = ctx.arrays.get_element(entity, start + 1);
    let dict_entity = match dict_obj.value {
        PsValue::Dict(e) => e,
        _ => return Err(PsError::TypeCheck),
    };
    if !has_white_point(ctx, dict_entity) {
        return Err(PsError::RangeCheck);
    }
    // Start with empty params; precompute_cie_decode_tables will fill the table
    let range_def_vec = get_cie_float_array(
        ctx,
        dict_entity,
        b"RangeDEF",
        &[0.0, 1.0, 0.0, 1.0, 0.0, 1.0],
    );
    let mut range_def = [0.0, 1.0, 0.0, 1.0, 0.0, 1.0];
    if range_def_vec.len() >= 6 {
        range_def.copy_from_slice(&range_def_vec[..6]);
    }
    let mut params = CieDefParams::default();
    params.range_def = range_def;
    Ok(ColorSpace::CIEBasedDEF {
        params: Arc::new(params),
        dict_entity,
    })
}

/// Parse `[/CIEBasedDEFG dict]` from an array.
fn parse_cie_defg_colorspace(
    ctx: &Context,
    entity: EntityId,
    start: u32,
    len: u32,
) -> Result<ColorSpace, PsError> {
    if len != 2 {
        return Err(PsError::RangeCheck);
    }
    let dict_obj = ctx.arrays.get_element(entity, start + 1);
    let dict_entity = match dict_obj.value {
        PsValue::Dict(e) => e,
        _ => return Err(PsError::TypeCheck),
    };
    if !has_white_point(ctx, dict_entity) {
        return Err(PsError::RangeCheck);
    }
    let range_defg_vec = get_cie_float_array(
        ctx,
        dict_entity,
        b"RangeDEFG",
        &[0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0],
    );
    let mut range_defg = [0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0];
    if range_defg_vec.len() >= 8 {
        range_defg.copy_from_slice(&range_defg_vec[..8]);
    }
    let mut params = CieDefgParams::default();
    params.range_defg = range_defg;
    Ok(ColorSpace::CIEBasedDEFG {
        params: Arc::new(params),
        dict_entity,
    })
}

/// Parse `[/ICCBased dict]` from an array.
/// Extracts and registers the embedded ICC profile if a DataSource is available.
fn parse_iccbased_colorspace(
    ctx: &mut Context,
    entity: EntityId,
    start: u32,
    len: u32,
) -> Result<ColorSpace, PsError> {
    if len != 2 {
        return Err(PsError::RangeCheck);
    }
    let dict_obj = ctx.arrays.get_element(entity, start + 1);
    let dict_entity = match dict_obj.value {
        PsValue::Dict(e) => e,
        _ => return Err(PsError::TypeCheck),
    };
    // /N is required (number of components)
    let n_key = match ctx.names.find(b"N") {
        Some(id) => DictKey::Name(id),
        None => return Err(PsError::RangeCheck),
    };
    let n_obj = ctx
        .dicts
        .get(dict_entity, &n_key)
        .ok_or(PsError::RangeCheck)?;
    let n = n_obj.as_i32().ok_or(PsError::TypeCheck)? as u32;
    if n != 1 && n != 3 && n != 4 {
        return Err(PsError::RangeCheck);
    }

    // Try to extract ICC profile bytes from DataSource or the stream itself
    let profile_hash = extract_icc_profile(ctx, dict_entity);

    Ok(ColorSpace::ICCBased {
        dict_entity,
        n,
        profile_hash,
    })
}

/// Extract ICC profile bytes from a dict's DataSource and register with the ICC cache.
fn extract_icc_profile(
    ctx: &mut Context,
    dict_entity: EntityId,
) -> Option<stet_graphics::icc::ProfileHash> {
    // Look for a string-based DataSource in the dict
    let ds_key = ctx.names.find(b"DataSource")?;
    let ds_obj = ctx.dicts.get(dict_entity, &DictKey::Name(ds_key))?;

    let bytes = match ds_obj.value {
        PsValue::String { entity, start, len } => ctx.strings.get(entity, start, len).to_vec(),
        PsValue::File(file_entity) => {
            // Read all bytes from file/filter
            ctx.pump_proc_sources(file_entity).ok()?;
            let mut buf = Vec::new();
            loop {
                match ctx.files.read_byte(file_entity) {
                    Ok(Some(b)) => buf.push(b),
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
            if buf.is_empty() {
                return None;
            }
            buf
        }
        _ => return None,
    };

    ctx.icc_cache.register_profile(&bytes)
}

/// A Separation or DeviceN colorant name.
///
/// PLRM 3e 4.8.4: "Name and string objects can be used interchangeably as
/// names; a string may be more convenient if the desired name contains
/// spaces". pdftops writes every colorant as a string (`(Black)`,
/// `(PANTONE 273 C)`, `(All)`), and these used to become an empty name, so
/// process colorants were taken for spots and `(All)` for a spot called "".
/// Anything else is a `typecheck`, as in Ghostscript.
fn colorant_name(ctx: &Context, obj: &PsObject) -> Result<Vec<u8>, PsError> {
    match obj.value {
        PsValue::Name(id) => Ok(ctx.names.get_bytes(id).to_vec()),
        PsValue::String { entity, start, len } => Ok(ctx.strings.get(entity, start, len).to_vec()),
        _ => Err(PsError::TypeCheck),
    }
}

/// Parse `[/Separation name alternativeSpace tintTransform]` from an array.
fn parse_separation_colorspace(
    ctx: &mut Context,
    entity: EntityId,
    start: u32,
) -> Result<ColorSpace, PsError> {
    // Element 1: colorant name
    let name_obj = ctx.arrays.get_element(entity, start + 1);
    let name = colorant_name(ctx, &name_obj)?;

    // Element 2: alternative color space (name)
    let alt_obj = ctx.arrays.get_element(entity, start + 2);
    let (alt_space, num_alt) = parse_alt_space_name(ctx, &alt_obj)?;

    // Element 3: tint transform (executable procedure)
    let tint_transform = ctx.arrays.get_element(entity, start + 3);

    Ok(ColorSpace::Separation {
        name,
        alt_space: Box::new(alt_space),
        tint_transform,
        num_alt_components: num_alt,
    })
}

/// Parse `[/DeviceN names alternativeSpace tintTransform]` from an array.
fn parse_devicen_colorspace(
    ctx: &mut Context,
    entity: EntityId,
    start: u32,
) -> Result<ColorSpace, PsError> {
    // Element 1: array of colorant names
    let names_obj = ctx.arrays.get_element(entity, start + 1);
    let (num_colorants, names) = match array_span(&names_obj) {
        Some((name_ent, name_start, len)) => {
            let mut names = Vec::with_capacity(len as usize);
            for i in 0..len {
                let elem = ctx.arrays.get_element(name_ent, name_start + i);
                names.push(colorant_name(ctx, &elem)?);
            }
            (len, names)
        }
        _ => return Err(PsError::TypeCheck),
    };

    // Element 2: alternative color space (name)
    let alt_obj = ctx.arrays.get_element(entity, start + 2);
    let (alt_space, num_alt) = parse_alt_space_name(ctx, &alt_obj)?;

    // Element 3: tint transform (executable procedure)
    let tint_transform = ctx.arrays.get_element(entity, start + 3);

    Ok(ColorSpace::DeviceN {
        names,
        num_colorants,
        alt_space: Box::new(alt_space),
        tint_transform,
        num_alt_components: num_alt,
    })
}

/// Parse an alternative color space name and return the ColorSpace + component count.
fn parse_alt_space_name(ctx: &mut Context, obj: &PsObject) -> Result<(ColorSpace, u32), PsError> {
    match obj.value {
        PsValue::Name(id) => {
            let name = ctx.names.get_bytes(id);
            match name {
                b"DeviceGray" => Ok((ColorSpace::DeviceGray, 1)),
                b"DeviceRGB" => Ok((ColorSpace::DeviceRGB, 3)),
                b"DeviceCMYK" => Ok((ColorSpace::DeviceCMYK, 4)),
                _ => Err(PsError::RangeCheck),
            }
        }
        // PLRM 4.9.4/4.9.5: the alternate space may be any device or CIE-based
        // space, which `pdftops` writes in array form — e.g. a spot colour over
        // `[/CIEBasedABC <<...>>]`. It may not itself be a special space.
        PsValue::Array { .. } | PsValue::PackedArray { .. } => {
            let (cs, n) = resolve_color_space_from_obj(ctx, obj)?;
            if matches!(
                cs,
                ColorSpace::Indexed { .. }
                    | ColorSpace::Separation { .. }
                    | ColorSpace::DeviceN { .. }
            ) {
                return Err(PsError::RangeCheck);
            }
            Ok((cs, n as u32))
        }
        _ => Err(PsError::TypeCheck),
    }
}

/// Extract a float array from a CIE dict entry, using defaults if not present.
fn get_cie_float_array(
    ctx: &Context,
    dict_entity: EntityId,
    key: &[u8],
    default: &[f64],
) -> Vec<f64> {
    let name_id = match ctx.names.find(key) {
        Some(id) => id,
        None => return default.to_vec(),
    };
    let dk = DictKey::Name(name_id);
    match ctx.dicts.get(dict_entity, &dk) {
        Some(obj) => match obj.value {
            PsValue::Array { entity, start, len } | PsValue::PackedArray { entity, start, len } => {
                let mut result = Vec::with_capacity(len as usize);
                for i in 0..len {
                    let elem = ctx.arrays.get_element(entity, start + i);
                    result.push(elem.as_f64().unwrap_or(0.0));
                }
                result
            }
            _ => default.to_vec(),
        },
        None => default.to_vec(),
    }
}

/// Extract CIEBasedABC parameters from a CIE dict.
fn extract_cie_abc_params(ctx: &Context, dict_entity: EntityId) -> CieAbcParams {
    let mut params = CieAbcParams::default();

    let range_abc = get_cie_float_array(ctx, dict_entity, b"RangeABC", &params.range_abc);
    if range_abc.len() >= 6 {
        params.range_abc.copy_from_slice(&range_abc[..6]);
    }

    let mat_abc = get_cie_float_array(ctx, dict_entity, b"MatrixABC", &params.matrix_abc);
    if mat_abc.len() >= 9 {
        params.matrix_abc.copy_from_slice(&mat_abc[..9]);
    }

    let range_lmn = get_cie_float_array(ctx, dict_entity, b"RangeLMN", &params.range_lmn);
    if range_lmn.len() >= 6 {
        params.range_lmn.copy_from_slice(&range_lmn[..6]);
    }

    let mat_lmn = get_cie_float_array(ctx, dict_entity, b"MatrixLMN", &params.matrix_lmn);
    if mat_lmn.len() >= 9 {
        params.matrix_lmn.copy_from_slice(&mat_lmn[..9]);
    }

    let wp = get_cie_float_array(ctx, dict_entity, b"WhitePoint", &params.white_point);
    if wp.len() >= 3 {
        params.white_point.copy_from_slice(&wp[..3]);
    }

    params
}

/// Extract CIEBasedA parameters from a CIE dict.
fn extract_cie_a_params(ctx: &Context, dict_entity: EntityId) -> CieAParams {
    let mut params = CieAParams::default();

    let range_a = get_cie_float_array(ctx, dict_entity, b"RangeA", &params.range_a);
    if range_a.len() >= 2 {
        params.range_a.copy_from_slice(&range_a[..2]);
    }

    let mat_a = get_cie_float_array(ctx, dict_entity, b"MatrixA", &params.matrix_a);
    if mat_a.len() >= 3 {
        params.matrix_a.copy_from_slice(&mat_a[..3]);
    }

    let range_lmn = get_cie_float_array(ctx, dict_entity, b"RangeLMN", &params.range_lmn);
    if range_lmn.len() >= 6 {
        params.range_lmn.copy_from_slice(&range_lmn[..6]);
    }

    let mat_lmn = get_cie_float_array(ctx, dict_entity, b"MatrixLMN", &params.matrix_lmn);
    if mat_lmn.len() >= 9 {
        params.matrix_lmn.copy_from_slice(&mat_lmn[..9]);
    }

    let wp = get_cie_float_array(ctx, dict_entity, b"WhitePoint", &params.white_point);
    if wp.len() >= 3 {
        params.white_point.copy_from_slice(&wp[..3]);
    }

    params
}

/// Set color in CIEBasedABC color space: c1 c2 c3 → —
fn set_cie_abc_color(ctx: &mut Context, params: &Arc<CieAbcParams>) -> Result<(), PsError> {
    if ctx.o_stack.len() < 3 {
        return Err(PsError::StackUnderflow);
    }
    let c_obj = ctx.o_stack.peek(0)?;
    let b_obj = ctx.o_stack.peek(1)?;
    let a_obj = ctx.o_stack.peek(2)?;
    let a = a_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let b = b_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let c = c_obj.as_f64().ok_or(PsError::TypeCheck)?;

    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.gstate.color = DeviceColor::from_cie_abc(a, b, c, params);
    ctx.gstate.color_components = Some(clamp_to_ranges(&[a, b, c], &params.range_abc));
    clear_pattern_color(ctx);
    Ok(())
}

/// Set color in CIEBasedA color space: comp → —
fn set_cie_a_color(ctx: &mut Context, params: &Arc<CieAParams>) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let a_obj = ctx.o_stack.peek(0)?;
    let a = a_obj.as_f64().ok_or(PsError::TypeCheck)?;

    ctx.o_stack.pop()?;
    ctx.gstate.color = DeviceColor::from_cie_a(a, params);
    ctx.gstate.color_components = Some(clamp_to_ranges(&[a], &params.range_a));
    clear_pattern_color(ctx);
    Ok(())
}

/// Set color in CIEBasedDEF color space: d e f → —
fn set_cie_def_color(ctx: &mut Context, params: &Arc<CieDefParams>) -> Result<(), PsError> {
    if ctx.o_stack.len() < 3 {
        return Err(PsError::StackUnderflow);
    }
    let f_obj = ctx.o_stack.peek(0)?;
    let e_obj = ctx.o_stack.peek(1)?;
    let d_obj = ctx.o_stack.peek(2)?;
    let d = d_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let e = e_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let f = f_obj.as_f64().ok_or(PsError::TypeCheck)?;

    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.gstate.color = DeviceColor::from_cie_def(d, e, f, params);
    ctx.gstate.color_components = Some(clamp_to_ranges(&[d, e, f], &params.range_def));
    clear_pattern_color(ctx);
    Ok(())
}

/// Set color in CIEBasedDEFG color space: d e f g → —
fn set_cie_defg_color(ctx: &mut Context, params: &Arc<CieDefgParams>) -> Result<(), PsError> {
    if ctx.o_stack.len() < 4 {
        return Err(PsError::StackUnderflow);
    }
    let g_obj = ctx.o_stack.peek(0)?;
    let f_obj = ctx.o_stack.peek(1)?;
    let e_obj = ctx.o_stack.peek(2)?;
    let d_obj = ctx.o_stack.peek(3)?;
    let d = d_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let e = e_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let f = f_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let g = g_obj.as_f64().ok_or(PsError::TypeCheck)?;

    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.gstate.color = DeviceColor::from_cie_defg(d, e, f, g, params);
    ctx.gstate.color_components = Some(clamp_to_ranges(&[d, e, f, g], &params.range_defg));
    clear_pattern_color(ctx);
    Ok(())
}

/// Clamp each of `values` to its `[min max]` pair in `ranges`, a CIE-based
/// space's RangeA / RangeABC / RangeDEF / RangeDEFG: what `currentcolor`
/// reports for the colour `setcolor` was given.
///
/// The ranges come from the program, so they are not trusted to be ordered;
/// see [`clamp_to_range`](stet_graphics::color::clamp_to_range).
fn clamp_to_ranges(values: &[f64], ranges: &[f64]) -> Arc<[f64]> {
    values
        .iter()
        .zip(ranges.as_chunks::<2>().0)
        .map(|(&v, &[a, b])| stet_graphics::color::clamp_to_range(v, a, b))
        .collect()
}

/// Get the number of color components for a color space.
pub fn color_space_components(cs: &ColorSpace) -> usize {
    match cs {
        ColorSpace::DeviceGray => 1,
        ColorSpace::DeviceRGB => 3,
        ColorSpace::DeviceCMYK => 4,
        ColorSpace::CIEBasedABC { .. } => 3,
        ColorSpace::CIEBasedA { .. } => 1,
        ColorSpace::CIEBasedDEF { .. } => 3,
        ColorSpace::CIEBasedDEFG { .. } => 4,
        ColorSpace::ICCBased { n, .. } => *n as usize,
        ColorSpace::Indexed { .. } => 1,
        ColorSpace::Separation { .. } => 1,
        ColorSpace::DeviceN { num_colorants, .. } => *num_colorants as usize,
        // The pattern object itself is not a numeric component. A Pattern space
        // takes the base space's components (PaintType 2) plus the pattern; with
        // no base it takes the pattern alone.
        ColorSpace::Pattern { base } => base
            .as_ref()
            .map(|b| color_space_components(b))
            .unwrap_or(0),
    }
}

/// Sample an Indexed space's lookup procedure into its byte table, for a
/// consumer that reads the palette as bytes in a device base space: each
/// entry's components clipped to 0-1 and scaled to 255. The procedure is
/// kept, and the colour code calls it ([`indexed_entry`]), since its
/// values are in the base space's ranges, which the bytes cannot hold for
/// a CIE-based base.
fn resolve_indexed_proc_lookup(ctx: &mut Context, cs: ColorSpace) -> Result<ColorSpace, PsError> {
    if let ColorSpace::Indexed {
        ref base,
        hival,
        ref lookup_proc,
        ..
    } = cs
        && let Some(proc_obj) = *lookup_proc
    {
        let ncomp = color_space_components(base);
        let base = base.clone();
        let mut lookup = Vec::with_capacity((hival as usize + 1) * ncomp);
        for idx in 0..=hival {
            ctx.o_stack.push(PsObject::int(idx as i32))?;
            ctx.exec_sync(proc_obj)?;
            // Pop ncomp values (in reverse order)
            let mut components = vec![0u8; ncomp];
            for c in (0..ncomp).rev() {
                let val = if !ctx.o_stack.is_empty() {
                    ctx.o_stack.pop()?.as_f64().unwrap_or(0.0).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                components[c] = (val * 255.0).round() as u8;
            }
            lookup.extend_from_slice(&components);
        }
        return Ok(ColorSpace::Indexed {
            base,
            hival,
            lookup,
            lookup_proc: Some(proc_obj),
        });
    }
    Ok(cs)
}

/// Pre-evaluate CIE decode procedures and DEF/DEFG lookup tables.
///
/// For CIEBasedABC/A: evaluates DecodeABC, DecodeLMN, DecodeA procedures at 256
/// sample points via exec_sync, storing the results as lookup tables.
/// For CIEBasedDEF/DEFG: pre-converts the entire 3D/4D table through the CIE
/// pipeline to sRGB for fast interpolation at render time.
pub fn precompute_cie_decode_tables(
    ctx: &mut Context,
    cs: ColorSpace,
) -> Result<ColorSpace, PsError> {
    match cs {
        ColorSpace::CIEBasedABC {
            params,
            dict_entity,
        } => {
            let mut p = (*params).clone();

            // Pre-evaluate DecodeABC over the RangeABC range
            if let Some(decode_abc_procs) = get_cie_decode_procs(ctx, dict_entity, b"DecodeABC", 3)
            {
                let mut tables: [Vec<f64>; 3] = [Vec::new(), Vec::new(), Vec::new()];
                for (ch, proc) in decode_abc_procs.iter().enumerate() {
                    let lo = p.range_abc[ch * 2];
                    let hi = p.range_abc[ch * 2 + 1];
                    tables[ch] = eval_decode_table_range(ctx, *proc, 256, lo, hi)?;
                }
                p.decode_abc = Some(tables);
            }

            // Pre-evaluate DecodeLMN over the RangeLMN range
            if let Some(decode_lmn_procs) = get_cie_decode_procs(ctx, dict_entity, b"DecodeLMN", 3)
            {
                let mut tables: [Vec<f64>; 3] = [Vec::new(), Vec::new(), Vec::new()];
                for (ch, proc) in decode_lmn_procs.iter().enumerate() {
                    let lo = p.range_lmn[ch * 2];
                    let hi = p.range_lmn[ch * 2 + 1];
                    tables[ch] = eval_decode_table_range(ctx, *proc, 256, lo, hi)?;
                }
                p.decode_lmn = Some(tables);
            }

            Ok(ColorSpace::CIEBasedABC {
                params: Arc::new(p),
                dict_entity,
            })
        }
        ColorSpace::CIEBasedA {
            params,
            dict_entity,
        } => {
            let mut p = (*params).clone();

            // Pre-evaluate DecodeA over RangeA
            if let Some(decode_a_proc) = get_cie_decode_proc_single(ctx, dict_entity, b"DecodeA") {
                p.decode_a = Some(eval_decode_table_range(
                    ctx,
                    decode_a_proc,
                    256,
                    p.range_a[0],
                    p.range_a[1],
                )?);
            }

            // Pre-evaluate DecodeLMN over RangeLMN
            if let Some(decode_lmn_procs) = get_cie_decode_procs(ctx, dict_entity, b"DecodeLMN", 3)
            {
                let mut tables: [Vec<f64>; 3] = [Vec::new(), Vec::new(), Vec::new()];
                for (ch, proc) in decode_lmn_procs.iter().enumerate() {
                    let lo = p.range_lmn[ch * 2];
                    let hi = p.range_lmn[ch * 2 + 1];
                    tables[ch] = eval_decode_table_range(ctx, *proc, 256, lo, hi)?;
                }
                p.decode_lmn = Some(tables);
            }

            Ok(ColorSpace::CIEBasedA {
                params: Arc::new(p),
                dict_entity,
            })
        }
        ColorSpace::CIEBasedDEF { dict_entity, .. } => {
            let params = precompute_cie_def_table(ctx, dict_entity)?;
            Ok(ColorSpace::CIEBasedDEF {
                params: Arc::new(params),
                dict_entity,
            })
        }
        ColorSpace::CIEBasedDEFG { dict_entity, .. } => {
            let params = precompute_cie_defg_table(ctx, dict_entity)?;
            Ok(ColorSpace::CIEBasedDEFG {
                params: Arc::new(params),
                dict_entity,
            })
        }
        // An Indexed space is not itself CIE-based, but its base may be, and
        // the base is what the lookup table's components are interpreted in.
        ColorSpace::Indexed {
            base,
            hival,
            lookup,
            lookup_proc,
        } => Ok(ColorSpace::Indexed {
            base: Box::new(precompute_cie_decode_tables(ctx, *base)?),
            hival,
            lookup,
            lookup_proc,
        }),
        // Likewise the alternate space a tint transform produces colour in.
        ColorSpace::Separation {
            name,
            alt_space,
            tint_transform,
            num_alt_components,
        } => Ok(ColorSpace::Separation {
            name,
            alt_space: Box::new(precompute_cie_decode_tables(ctx, *alt_space)?),
            tint_transform,
            num_alt_components,
        }),
        ColorSpace::DeviceN {
            names,
            num_colorants,
            alt_space,
            tint_transform,
            num_alt_components,
        } => Ok(ColorSpace::DeviceN {
            names,
            num_colorants,
            alt_space: Box::new(precompute_cie_decode_tables(ctx, *alt_space)?),
            tint_transform,
            num_alt_components,
        }),
        // Non-CIE color spaces pass through unchanged
        other => Ok(other),
    }
}

/// Extract an array of decode procedures from a CIE dict entry.
/// Returns None if the key is not present or not an array of procedures.
fn get_cie_decode_procs(
    ctx: &Context,
    dict_entity: EntityId,
    key: &[u8],
    expected: usize,
) -> Option<Vec<PsObject>> {
    let name_id = ctx.names.find(key)?;
    let dk = DictKey::Name(name_id);
    let obj = ctx.dicts.get(dict_entity, &dk)?;
    let (entity, start, len) = array_span(&obj)?;
    if (len as usize) < expected {
        return None;
    }
    let mut procs = Vec::with_capacity(expected);
    for i in 0..expected as u32 {
        let elem = ctx.arrays.get_element(entity, start + i);
        // Each element should be a procedure
        if !is_procedure(&elem) {
            return None;
        }
        procs.push(elem);
    }
    Some(procs)
}

/// Extract a single decode procedure from a CIE dict entry (for DecodeA).
fn get_cie_decode_proc_single(
    ctx: &Context,
    dict_entity: EntityId,
    key: &[u8],
) -> Option<PsObject> {
    let name_id = ctx.names.find(key)?;
    let dk = DictKey::Name(name_id);
    let obj = ctx.dicts.get(dict_entity, &dk)?;
    is_procedure(&obj).then_some(obj)
}

/// Upper bound on memoised decode tables. Each entry is one `Vec<f64>` of
/// `n` samples (2 KB at the usual n=256), so the ceiling caps the cache at a
/// few megabytes even on a file with pathologically many distinct CIE spaces.
/// Past the ceiling, evaluation still succeeds — it just stops being cached,
/// so this can never turn into its own unbounded-growth path.
const CIE_DECODE_CACHE_MAX: usize = 4096;

/// Depth ceiling for the fingerprint walk. Beyond it the procedure is treated
/// as un-fingerprintable and evaluated uncached, which is always correct.
const FINGERPRINT_MAX_DEPTH: usize = 64;

/// Ceiling on objects examined while fingerprinting one procedure.
///
/// Fingerprinting has to be cheaper than the sampling it avoids. Walking a
/// decode procedure is normally trivial (a few thousand objects even with the
/// inline lookup tables `pdftops` emits), but a procedure that merely *refers*
/// to a huge bound table would drag that table into every walk and could cost
/// more than the 256 evaluations it saves. Exceeding the budget abandons the
/// fingerprint, so such a procedure is evaluated uncached exactly as before.
const FINGERPRINT_MAX_OBJECTS: usize = 64 * 1024;

/// Writes the same bytes into two differently-salted hashers, yielding a
/// 128-bit fingerprint. A collision would silently reuse another procedure's
/// colour table, so 64 bits is not enough margin to rely on here.
struct DualHasher {
    a: std::collections::hash_map::DefaultHasher,
    b: std::collections::hash_map::DefaultHasher,
}

impl DualHasher {
    fn new() -> Self {
        use std::hash::Hasher;
        let mut a = std::collections::hash_map::DefaultHasher::new();
        let mut b = std::collections::hash_map::DefaultHasher::new();
        a.write_u64(0x9E37_79B9_7F4A_7C15);
        b.write_u64(0xC2B2_AE3D_27D4_EB4F);
        Self { a, b }
    }

    fn tag(&mut self, v: u8) {
        use std::hash::Hasher;
        self.a.write_u8(v);
        self.b.write_u8(v);
    }

    fn word(&mut self, v: u64) {
        use std::hash::Hasher;
        self.a.write_u64(v);
        self.b.write_u64(v);
    }

    fn bytes(&mut self, v: &[u8]) {
        use std::hash::Hasher;
        self.a.write(v);
        self.b.write(v);
    }

    fn finish(&self) -> (u64, u64) {
        use std::hash::Hasher;
        (self.a.finish(), self.b.finish())
    }
}

/// Normalise a float so equal values hash equally: `-0.0` and `0.0` compare
/// equal but have different bit patterns, and every NaN is folded to one.
fn canonical_f64_bits(v: f64) -> u64 {
    if v == 0.0 {
        0.0f64.to_bits()
    } else if v.is_nan() {
        f64::NAN.to_bits()
    } else {
        v.to_bits()
    }
}

/// Fold `obj` into the fingerprint, following nested procedures and resolving
/// executable names through the dict stack.
///
/// Sets `ok` to false — meaning "don't cache this" — on anything whose
/// behaviour can't be pinned down from its contents (files, save objects,
/// gstates, dicts) or on exceeding the depth ceiling. Bailing out is always
/// safe: the caller falls back to evaluating uncached.
fn fingerprint_obj(
    ctx: &mut Context,
    obj: PsObject,
    h: &mut DualHasher,
    visited: &mut std::collections::HashSet<(EntityId, u32, u32)>,
    depth: usize,
    budget: &mut usize,
    ok: &mut bool,
) {
    if !*ok {
        return;
    }
    if depth > FINGERPRINT_MAX_DEPTH || *budget == 0 {
        *ok = false;
        return;
    }
    *budget -= 1;
    let executable = obj.flags.is_executable();
    h.tag(executable as u8);

    match obj.value {
        PsValue::Null => h.tag(1),
        PsValue::Mark => h.tag(2),
        PsValue::DictMark => h.tag(3),
        PsValue::Bool(v) => {
            h.tag(4);
            h.tag(v as u8);
        }
        PsValue::Int(v) => {
            h.tag(5);
            h.word(v as u64);
        }
        PsValue::Real(v) => {
            h.tag(6);
            h.word(canonical_f64_bits(v));
        }
        PsValue::Operator(op) => {
            h.tag(7);
            h.word(op.0 as u64);
        }
        PsValue::String { entity, start, len } => {
            h.tag(8);
            h.word(len as u64);
            let bytes = ctx.strings.get(entity, start, len).to_vec();
            h.bytes(&bytes);
        }
        PsValue::Name(id) => {
            h.tag(9);
            let bytes = ctx.names.get_bytes(id).to_vec();
            h.bytes(&bytes);
            // A literal name is just data. An executable one is a call whose
            // meaning comes from the dict stack, so what it resolves to right
            // now is part of the fingerprint: rebind the name and the
            // fingerprint changes, which misses the cache and re-evaluates.
            if executable {
                match ctx.dict_load(&DictKey::Name(id)) {
                    Some(bound) => {
                        h.tag(1);
                        fingerprint_obj(ctx, bound, h, visited, depth + 1, budget, ok);
                    }
                    None => h.tag(0),
                }
            }
        }
        PsValue::Array { entity, start, len } | PsValue::PackedArray { entity, start, len } => {
            h.tag(10);
            h.word(len as u64);
            // Procedure graphs may legitimately be cyclic — the back-patched
            // stub idiom Ghostscript's opdfread.ps prolog uses (see op_bind).
            // Hash a back-edge marker on revisit so the walk terminates.
            if !visited.insert((entity, start, len)) {
                h.tag(0xFF);
                return;
            }
            for i in 0..len {
                let elem = ctx.arrays.get_element(entity, start + i);
                fingerprint_obj(ctx, elem, h, visited, depth + 1, budget, ok);
                if !*ok {
                    return;
                }
            }
        }
        _ => *ok = false,
    }
}

/// Build the cache key for sampling `proc` over `[lo, hi]`, or `None` when the
/// procedure can't be fingerprinted and must be evaluated uncached.
///
/// The key covers the procedure's full program text, every nested procedure,
/// and — for each executable name — whatever the dict stack binds that name to
/// at this moment, resolved recursively. Two procedures therefore share a key
/// only when they would execute the same instructions against the same
/// bindings.
///
/// What the key deliberately does *not* cover is the dict stack itself, so a
/// procedure that reaches its data through a run-time lookup the static walk
/// can't follow (`currentdict … get`, `load` of a computed name) could in
/// principle share a key with one that behaves differently. That is out of
/// scope by construction: the PLRM requires a CIE decode procedure to be a
/// pure function of its single numeric input, which is the same assumption
/// this code has always made by sampling it at `n` points and interpolating
/// between them. Including the dict stack was measured and rejected — the
/// per-object dicts `pdftops` pushes differ on every call, which drops the hit
/// rate to zero and restores the unbounded growth this cache exists to stop.
fn decode_table_cache_key(
    ctx: &mut Context,
    proc: PsObject,
    n: usize,
    lo: f64,
    hi: f64,
) -> Option<stet_core::context::CieDecodeKey> {
    let mut h = DualHasher::new();
    let mut visited = std::collections::HashSet::new();
    let mut budget = FINGERPRINT_MAX_OBJECTS;
    let mut ok = true;
    fingerprint_obj(ctx, proc, &mut h, &mut visited, 0, &mut budget, &mut ok);
    if !ok {
        return None;
    }
    let (fa, fb) = h.finish();
    Some((
        fa,
        fb,
        n as u32,
        canonical_f64_bits(lo),
        canonical_f64_bits(hi),
    ))
}

/// Evaluate a PostScript decode procedure at `n` evenly spaced points spanning
/// `[lo, hi]`, memoising the result.
///
/// The memo is what keeps a file that re-installs the same CIE space on every
/// page from re-running the procedure thousands of times — and, because each
/// evaluation can allocate inline array literals into the non-reclaiming array
/// arena, from growing memory without bound. See [`Context::cie_decode_cache`].
fn eval_decode_table_range(
    ctx: &mut Context,
    proc: PsObject,
    n: usize,
    lo: f64,
    hi: f64,
) -> Result<Vec<f64>, PsError> {
    let key = decode_table_cache_key(ctx, proc, n, lo, hi);
    if let Some(k) = key
        && let Some(hit) = ctx.cie_decode_cache.get(&k)
    {
        return Ok(hit.clone());
    }

    let table = eval_decode_table_range_uncached(ctx, proc, n, lo, hi)?;

    if let Some(k) = key
        && ctx.cie_decode_cache.len() < CIE_DECODE_CACHE_MAX
    {
        ctx.cie_decode_cache.insert(k, table.clone());
    }
    Ok(table)
}

/// Sample `proc` at `n` evenly spaced points spanning `[lo, hi]`.
fn eval_decode_table_range_uncached(
    ctx: &mut Context,
    proc: PsObject,
    n: usize,
    lo: f64,
    hi: f64,
) -> Result<Vec<f64>, PsError> {
    let mut table = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f64 / (n - 1) as f64;
        let input = lo + t * (hi - lo);
        ctx.o_stack.push(PsObject::real(input))?;
        ctx.exec_sync(proc)?;
        let result = if !ctx.o_stack.is_empty() {
            ctx.o_stack.pop()?.as_f64().unwrap_or(input)
        } else {
            input
        };
        table.push(result);
    }
    Ok(table)
}

/// Most entries a CIEBasedDEF or CIEBasedDEFG table may have.
///
/// The dimensions are the program's to choose, and arrays may alias — one
/// inner array repeated down the outer one, one string down each inner one —
/// so a few kilobytes of program can claim a table of any size; the strings
/// alone do not bound it. Real tables are far smaller: `pdftops` writes 17⁴
/// (83,521) entries for an ICC profile, and 33 points an axis, 33⁴ (1.19M),
/// is as fine as colour-management tables go. 2²² (4.19M) entries is 100 MB
/// of tables; past it, `limitcheck`, the PLRM's error for an implementation
/// limit.
const MAX_CIE_TABLE_ENTRIES: usize = 1 << 22;

/// Read the `/Table` of a CIEBasedDEF (`n` = 3) or CIEBasedDEFG (`n` = 4)
/// space: `[m1 … mn table]` (PLRM, "CIEBasedDEF and CIEBasedDEFG Color
/// Spaces"). For DEF, `table` is m1 strings of 3·m2·m3 bytes; for DEFG it is
/// m1 arrays, each of m2 strings of 3·m3·m4 bytes. Each entry's three bytes
/// are A, B and C, scaled into `range_abc`. Returns the dimensions and the
/// A, B and C tables, the last axis varying fastest.
///
/// Every dimension must be an integer greater than 1 and every array and
/// string at least as long as the dimensions make it — `rangecheck`
/// otherwise, `typecheck` for an element of the wrong type, as in
/// Ghostscript. All of it is checked before the tables are allocated.
fn read_cie_table(
    ctx: &Context,
    dict_entity: EntityId,
    n: usize,
    range_abc: &[f64; 6],
) -> Result<(Vec<usize>, [Vec<f64>; 3]), PsError> {
    let table_obj = get_dict_obj_by_key(ctx, dict_entity, b"Table").ok_or(PsError::Undefined)?;
    let (entity, start, len) = array_span(&table_obj).ok_or(PsError::TypeCheck)?;
    if (len as usize) < n + 1 {
        return Err(PsError::RangeCheck);
    }
    let mut dims = Vec::with_capacity(n);
    for i in 0..n {
        // An integer, as the PLRM has it; a real with a whole value is taken
        // for one, and any other real is a `rangecheck`, as in Ghostscript.
        let m = match ctx.arrays.get_element(entity, start + i as u32).value {
            PsValue::Int(m) => m as f64,
            PsValue::Real(m) if m.fract() == 0.0 => m,
            PsValue::Real(_) => return Err(PsError::RangeCheck),
            _ => return Err(PsError::TypeCheck),
        };
        if !(2.0..=MAX_CIE_TABLE_ENTRIES as f64).contains(&m) {
            return Err(if m < 2.0 {
                PsError::RangeCheck
            } else {
                PsError::LimitCheck
            });
        }
        dims.push(m as usize);
    }
    let total = dims
        .iter()
        .try_fold(1usize, |acc, &m| acc.checked_mul(m))
        .filter(|&t| t <= MAX_CIE_TABLE_ENTRIES)
        .ok_or(PsError::LimitCheck)?;
    // Each string holds the entries of the last two axes, three bytes each.
    let per_string = dims[n - 2] * dims[n - 1];

    let string_at = |obj: PsObject| -> Result<(EntityId, u32), PsError> {
        match obj.value {
            PsValue::String { entity, start, len } if (len as usize) >= per_string * 3 => {
                Ok((entity, start))
            }
            PsValue::String { .. } => Err(PsError::RangeCheck),
            _ => Err(PsError::TypeCheck),
        }
    };
    // The strings in table order, the leading axes slowest.
    let outer = ctx.arrays.get_element(entity, start + n as u32);
    let (outer_entity, outer_start, outer_len) = array_span(&outer).ok_or(PsError::TypeCheck)?;
    if (outer_len as usize) < dims[0] {
        return Err(PsError::RangeCheck);
    }
    let mut strings = Vec::with_capacity(total / per_string);
    for h in 0..dims[0] {
        let elem = ctx.arrays.get_element(outer_entity, outer_start + h as u32);
        if n == 3 {
            strings.push(string_at(elem)?);
            continue;
        }
        let (inner_entity, inner_start, inner_len) = array_span(&elem).ok_or(PsError::TypeCheck)?;
        if (inner_len as usize) < dims[1] {
            return Err(PsError::RangeCheck);
        }
        for i in 0..dims[1] {
            strings.push(string_at(
                ctx.arrays.get_element(inner_entity, inner_start + i as u32),
            )?);
        }
    }

    let scale = |ch: usize, byte: u8| {
        range_abc[2 * ch] + byte as f64 * (range_abc[2 * ch + 1] - range_abc[2 * ch]) / 255.0
    };
    let mut tables = [
        Vec::with_capacity(total),
        Vec::with_capacity(total),
        Vec::with_capacity(total),
    ];
    for (str_entity, str_start) in strings {
        let bytes = ctx
            .strings
            .get(str_entity, str_start, (per_string * 3) as u32);
        for entry in bytes.as_chunks::<3>().0 {
            for (ch, table) in tables.iter_mut().enumerate() {
                table.push(scale(ch, entry[ch]));
            }
        }
    }
    Ok((dims, tables))
}

/// A CIE range array from the dictionary — `[min max]` per component, `N`
/// numbers — or `[0 1]` for each component when it is absent or short.
fn cie_range<const N: usize>(ctx: &Context, dict_entity: EntityId, key: &[u8]) -> [f64; N] {
    let mut range = [0.0; N];
    for pair in range.as_chunks_mut::<2>().0 {
        pair[1] = 1.0;
    }
    let found = get_cie_float_array(ctx, dict_entity, key, &range);
    if found.len() >= N {
        range.copy_from_slice(&found[..N]);
    }
    range
}

/// Sample the `N` decode procedures under `key` over their input ranges —
/// `range` gives a `[min max]` pair for each — as the ABC and LMN decode
/// tables are sampled. `None` when the dictionary has none.
fn sample_decode_procs<const N: usize>(
    ctx: &mut Context,
    dict_entity: EntityId,
    key: &[u8],
    range: &[f64],
) -> Result<Option<[Vec<f64>; N]>, PsError> {
    let Some(procs) = get_cie_decode_procs(ctx, dict_entity, key, N) else {
        return Ok(None);
    };
    let mut tables: [Vec<f64>; N] = std::array::from_fn(|_| Vec::new());
    for (ch, proc) in procs.iter().enumerate() {
        tables[ch] = eval_decode_table_range(ctx, *proc, 256, range[2 * ch], range[2 * ch + 1])?;
    }
    Ok(Some(tables))
}

/// The ABC stage of a CIEBasedDEF or CIEBasedDEFG space, which its table
/// feeds, with its DecodeABC and DecodeLMN procedures sampled.
fn table_space_abc_params(
    ctx: &mut Context,
    dict_entity: EntityId,
) -> Result<CieAbcParams, PsError> {
    let mut abc_params = extract_cie_abc_params(ctx, dict_entity);
    let range_abc = abc_params.range_abc;
    abc_params.decode_abc = sample_decode_procs::<3>(ctx, dict_entity, b"DecodeABC", &range_abc)?;
    let range_lmn = abc_params.range_lmn;
    abc_params.decode_lmn = sample_decode_procs::<3>(ctx, dict_entity, b"DecodeLMN", &range_lmn)?;
    Ok(abc_params)
}

/// Read a CIEBasedDEF space's table and decode stages: DEF → DecodeDEF →
/// HIJ (RangeHIJ) → the 3D table → ABC.
fn precompute_cie_def_table(
    ctx: &mut Context,
    dict_entity: EntityId,
) -> Result<CieDefParams, PsError> {
    let range_def = cie_range::<6>(ctx, dict_entity, b"RangeDEF");
    let range_hij = cie_range::<6>(ctx, dict_entity, b"RangeHIJ");
    let abc_params = table_space_abc_params(ctx, dict_entity)?;
    let (dims, [a_table, b_table, c_table]) =
        read_cie_table(ctx, dict_entity, 3, &abc_params.range_abc)?;
    let decode_def = sample_decode_procs::<3>(ctx, dict_entity, b"DecodeDEF", &range_def)?;
    let mut params = CieDefParams::default();
    params.range_def = range_def;
    params.range_hij = range_hij;
    params.decode_def = decode_def;
    [params.m1, params.m2, params.m3] = [dims[0], dims[1], dims[2]];
    params.a_table = a_table;
    params.b_table = b_table;
    params.c_table = c_table;
    params.abc_params = abc_params;
    Ok(params)
}

/// Read a CIEBasedDEFG space's table and decode stages: DEFG → DecodeDEFG →
/// HIJK (RangeHIJK) → the 4D table → ABC.
fn precompute_cie_defg_table(
    ctx: &mut Context,
    dict_entity: EntityId,
) -> Result<CieDefgParams, PsError> {
    let range_defg = cie_range::<8>(ctx, dict_entity, b"RangeDEFG");
    let range_hijk = cie_range::<8>(ctx, dict_entity, b"RangeHIJK");
    let abc_params = table_space_abc_params(ctx, dict_entity)?;
    let (dims, [a_table, b_table, c_table]) =
        read_cie_table(ctx, dict_entity, 4, &abc_params.range_abc)?;
    let decode_defg = sample_decode_procs::<4>(ctx, dict_entity, b"DecodeDEFG", &range_defg)?;
    let mut params = CieDefgParams::default();
    params.range_defg = range_defg;
    params.range_hijk = range_hijk;
    params.decode_defg = decode_defg;
    [params.m1, params.m2, params.m3, params.m4] = [dims[0], dims[1], dims[2], dims[3]];
    params.a_table = a_table;
    params.b_table = b_table;
    params.c_table = c_table;
    params.abc_params = abc_params;
    Ok(params)
}

/// Get a dict entry as a PsObject.
fn get_dict_obj_by_key(ctx: &Context, dict_entity: EntityId, key: &[u8]) -> Option<PsObject> {
    let name_id = ctx.names.find(key)?;
    let dk = DictKey::Name(name_id);
    ctx.dicts.get(dict_entity, &dk)
}

#[cfg(test)]
mod tests {
    use super::*;
    use stet_core::context::Context;

    fn setup() -> Context {
        let mut ctx = Context::new();
        crate::build_system_dict(&mut ctx);
        ctx
    }

    /// Build an executable procedure array from `elems`.
    fn make_proc(ctx: &mut Context, elems: &[PsObject]) -> PsObject {
        let entity = ctx.arrays.allocate_from_at_level_zero(elems);
        let mut proc = PsObject::array(entity, elems.len() as u32);
        proc.flags = stet_core::object::ObjFlags::executable_composite();
        proc
    }

    /// An executable name object, i.e. a call rather than a literal.
    fn exec_name(ctx: &mut Context, name: &[u8]) -> PsObject {
        let id = ctx.names.intern(name);
        PsObject::name_exec(id)
    }

    fn key_of(ctx: &mut Context, proc: PsObject) -> Option<stet_core::context::CieDecodeKey> {
        decode_table_cache_key(ctx, proc, 256, 0.0, 1.0)
    }

    /// Two separately allocated procedures with identical contents must share
    /// a key — this is what lets a file that re-installs the same CIE space on
    /// every page reuse one sampled table instead of resampling per page.
    #[test]
    fn test_decode_fingerprint_matches_across_identical_procs() {
        let mut ctx = setup();
        let a = make_proc(&mut ctx, &[PsObject::real(0.5), PsObject::int(3)]);
        let b = make_proc(&mut ctx, &[PsObject::real(0.5), PsObject::int(3)]);
        let (ka, kb) = (key_of(&mut ctx, a), key_of(&mut ctx, b));
        assert!(ka.is_some());
        assert_eq!(ka, kb);
    }

    /// Differing contents must not collide.
    #[test]
    fn test_decode_fingerprint_differs_on_content() {
        let mut ctx = setup();
        let a = make_proc(&mut ctx, &[PsObject::real(0.5)]);
        let b = make_proc(&mut ctx, &[PsObject::real(0.25)]);
        assert_ne!(key_of(&mut ctx, a), key_of(&mut ctx, b));
    }

    /// The sampled range is part of the key: the same procedure sampled over a
    /// different interval is a different table.
    #[test]
    fn test_decode_fingerprint_includes_range() {
        let mut ctx = setup();
        let p = make_proc(&mut ctx, &[PsObject::real(0.5)]);
        let a = decode_table_cache_key(&mut ctx, p, 256, 0.0, 1.0);
        let b = decode_table_cache_key(&mut ctx, p, 256, 0.0, 2.0);
        let c = decode_table_cache_key(&mut ctx, p, 128, 0.0, 1.0);
        assert!(a.is_some());
        assert_ne!(a, b);
        assert_ne!(a, c);
    }

    /// An executable name is a call, so what the dict stack binds it to is part
    /// of the fingerprint: rebinding must produce a different key, otherwise a
    /// stale table would be reused after the procedure's meaning changed.
    #[test]
    fn test_decode_fingerprint_tracks_rebinding() {
        let mut ctx = setup();
        let called = exec_name(&mut ctx, b"my_cie_helper");
        let proc = make_proc(&mut ctx, &[called]);

        let unbound = key_of(&mut ctx, proc);
        assert!(unbound.is_some(), "an unresolved name is still hashable");

        let helper_a = make_proc(&mut ctx, &[PsObject::real(1.0)]);
        let name_id = ctx.names.intern(b"my_cie_helper");
        let userdict = ctx.userdict;
        ctx.dicts.put(userdict, DictKey::Name(name_id), helper_a);
        ctx.dict_version += 1;
        let bound_a = key_of(&mut ctx, proc);
        assert_ne!(unbound, bound_a, "binding the name changes the key");

        let helper_b = make_proc(&mut ctx, &[PsObject::real(2.0)]);
        ctx.dicts.put(userdict, DictKey::Name(name_id), helper_b);
        ctx.dict_version += 1;
        let bound_b = key_of(&mut ctx, proc);
        assert_ne!(bound_a, bound_b, "rebinding the name changes the key again");
    }

    /// A cyclic procedure graph must not hang the fingerprint walk.
    ///
    /// The back-patched stub idiom that produces such cycles is standard under
    /// `//` immediate evaluation and appears in Ghostscript's `opdfread.ps`
    /// prolog; naive recursion here would overflow the stack exactly as it did
    /// in `op_bind`.
    #[test]
    fn test_decode_fingerprint_terminates_on_cycle() {
        let mut ctx = setup();
        let inner = ctx.arrays.allocate_from_at_level_zero(&[PsObject::int(0)]);
        let mut inner_proc = PsObject::array(inner, 1);
        inner_proc.flags = stet_core::object::ObjFlags::executable_composite();
        let outer = ctx.arrays.allocate_from_at_level_zero(&[inner_proc]);
        let mut outer_proc = PsObject::array(outer, 1);
        outer_proc.flags = stet_core::object::ObjFlags::executable_composite();
        // Close the loop: inner[0] := outer.
        ctx.arrays.set_element(inner, 0, outer_proc);

        assert!(key_of(&mut ctx, outer_proc).is_some());
    }

    /// Objects whose behaviour can't be pinned down from their contents make
    /// the procedure un-cacheable, so it falls back to uncached evaluation.
    #[test]
    fn test_decode_fingerprint_bails_on_opaque_value() {
        let mut ctx = setup();
        let d = ctx.dicts.allocate_at_level_zero(4, b"opaque");
        let proc = make_proc(&mut ctx, &[PsObject::dict(d)]);
        assert_eq!(key_of(&mut ctx, proc), None);
    }

    #[test]
    fn test_setgray_currentgray() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(0.5)).unwrap();
        op_setgray(&mut ctx).unwrap();
        op_currentgray(&mut ctx).unwrap();
        let v = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        assert!((v - 0.5).abs() < 1e-10);
    }

    /// A procedure too large to fingerprint cheaply is left uncached rather
    /// than made slower than the sampling the cache exists to avoid.
    #[test]
    fn test_decode_fingerprint_bails_over_object_budget() {
        let mut ctx = setup();
        let huge: Vec<PsObject> = (0..FINGERPRINT_MAX_OBJECTS + 16)
            .map(|i| PsObject::int(i as i32))
            .collect();
        let proc = make_proc(&mut ctx, &huge);
        assert_eq!(key_of(&mut ctx, proc), None);

        // A procedure comfortably inside the budget still fingerprints, so the
        // guard doesn't disable caching for the tables this is meant to fix.
        let ordinary: Vec<PsObject> = (0..1024).map(PsObject::int).collect();
        let ok_proc = make_proc(&mut ctx, &ordinary);
        assert!(key_of(&mut ctx, ok_proc).is_some());
    }

    #[test]
    fn test_setrgbcolor_currentrgbcolor() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(0.1)).unwrap();
        ctx.o_stack.push(PsObject::real(0.2)).unwrap();
        ctx.o_stack.push(PsObject::real(0.3)).unwrap();
        op_setrgbcolor(&mut ctx).unwrap();
        op_currentrgbcolor(&mut ctx).unwrap();
        let b = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        let g = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        let r = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        assert!((r - 0.1).abs() < 1e-10);
        assert!((g - 0.2).abs() < 1e-10);
        assert!((b - 0.3).abs() < 1e-10);
    }

    #[test]
    fn test_setcmykcolor_currentcmykcolor() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(1.0)).unwrap(); // c
        ctx.o_stack.push(PsObject::real(0.0)).unwrap(); // m
        ctx.o_stack.push(PsObject::real(0.0)).unwrap(); // y
        ctx.o_stack.push(PsObject::real(0.0)).unwrap(); // k
        op_setcmykcolor(&mut ctx).unwrap();
        // Should be r=0, g=1, b=1 (cyan)
        assert!((ctx.gstate.color.r - 0.0).abs() < 1e-10);
        assert!((ctx.gstate.color.g - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_sethsbcolor_currenthsbcolor() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(0.0)).unwrap(); // h=red
        ctx.o_stack.push(PsObject::real(1.0)).unwrap(); // s
        ctx.o_stack.push(PsObject::real(1.0)).unwrap(); // b
        op_sethsbcolor(&mut ctx).unwrap();
        assert!((ctx.gstate.color.r - 1.0).abs() < 1e-10);
        assert!((ctx.gstate.color.g - 0.0).abs() < 1e-10);
    }

    #[test]
    fn test_setgray_clamps() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(2.0)).unwrap();
        op_setgray(&mut ctx).unwrap();
        assert!((ctx.gstate.color.r - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_setcolorspace_name() {
        let mut ctx = setup();
        let name_id = ctx.names.intern(b"DeviceRGB");
        ctx.o_stack.push(PsObject::name_lit(name_id)).unwrap();
        op_setcolorspace(&mut ctx).unwrap();
        assert_eq!(ctx.gstate.color_space, ColorSpace::DeviceRGB);
    }

    #[test]
    fn test_currentcolorspace() {
        let mut ctx = setup();
        ctx.gstate.color_space = ColorSpace::DeviceCMYK;
        op_currentcolorspace(&mut ctx).unwrap();
        let arr = ctx.o_stack.pop().unwrap();
        match arr.value {
            PsValue::Array { entity, start, len } => {
                assert_eq!(len, 1);
                let first = ctx.arrays.get_element(entity, start);
                if let PsValue::Name(id) = first.value {
                    assert_eq!(ctx.names.get_bytes(id), b"DeviceCMYK");
                } else {
                    panic!("Expected name");
                }
            }
            _ => panic!("Expected array"),
        }
    }

    #[test]
    fn test_setcolor_gray() {
        let mut ctx = setup();
        ctx.gstate.color_space = ColorSpace::DeviceGray;
        ctx.o_stack.push(PsObject::real(0.75)).unwrap();
        op_setcolor(&mut ctx).unwrap();
        let gray = ctx.gstate.color.to_gray();
        assert!((gray - 0.75).abs() < 1e-10);
    }

    #[test]
    fn test_setgray_typecheck() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::bool(true)).unwrap();
        assert_eq!(op_setgray(&mut ctx), Err(PsError::TypeCheck));
    }

    /// `[/Indexed /DeviceRGB 2 <ff0000 00ff00 0000ff>]`.
    fn indexed_rgb_space(ctx: &mut Context) -> PsObject {
        let lookup: &[u8] = &[255, 0, 0, 0, 255, 0, 0, 0, 255];
        let str_entity = crate::vm_ops::alloc_string(ctx, lookup);
        let lookup = crate::vm_ops::make_string_obj(ctx, str_entity, 9);
        let items = [
            PsObject::name_lit(ctx.names.intern(b"Indexed")),
            PsObject::name_lit(ctx.names.intern(b"DeviceRGB")),
            PsObject::int(2),
            lookup,
        ];
        let entity = crate::vm_ops::alloc_array_from(ctx, &items);
        crate::vm_ops::make_array_obj(ctx, entity, 4)
    }

    fn current_color(ctx: &mut Context) -> Vec<PsValue> {
        let depth = ctx.o_stack.len();
        op_currentcolor(ctx).unwrap();
        let mut out: Vec<PsValue> = (depth..ctx.o_stack.len())
            .map(|_| ctx.o_stack.pop().unwrap().value)
            .collect();
        out.reverse();
        out
    }

    fn same_array(a: &PsObject, b: &PsObject) -> bool {
        matches!(
            (&a.value, &b.value),
            (
                PsValue::Array { entity: ea, start: sa, len: la },
                PsValue::Array { entity: eb, start: sb, len: lb },
            ) if ea == eb && sa == sb && la == lb
        )
    }

    #[test]
    fn test_indexed_currentcolor_is_the_index() {
        let mut ctx = setup();
        let space = indexed_rgb_space(&mut ctx);
        ctx.o_stack.push(space).unwrap();
        op_setcolorspace(&mut ctx).unwrap();
        assert!(matches!(current_color(&mut ctx)[..], [PsValue::Int(0)]));

        // Rounded and clipped to hival, and an integer however it was given.
        ctx.o_stack.push(PsObject::real(1.6)).unwrap();
        op_setcolor(&mut ctx).unwrap();
        assert!(matches!(current_color(&mut ctx)[..], [PsValue::Int(2)]));
        // The colour is still the one the index looks up.
        assert!((ctx.gstate.color.b - 1.0).abs() < 1e-9);

        // A setcolor that fails leaves the index that was current.
        ctx.o_stack.push(PsObject::bool(true)).unwrap();
        assert_eq!(op_setcolor(&mut ctx), Err(PsError::TypeCheck));
        ctx.o_stack.pop().unwrap();
        assert!(matches!(current_color(&mut ctx)[..], [PsValue::Int(2)]));
    }

    #[test]
    fn test_currentcolorspace_returns_the_array_given() {
        let mut ctx = setup();
        let space = indexed_rgb_space(&mut ctx);
        ctx.o_stack.push(space).unwrap();
        op_setcolorspace(&mut ctx).unwrap();
        ctx.o_stack.push(PsObject::int(1)).unwrap();
        op_setcolor(&mut ctx).unwrap();
        op_currentcolorspace(&mut ctx).unwrap();
        assert!(same_array(&ctx.o_stack.pop().unwrap(), &space));

        // A colour operator sets a space of its own.
        ctx.o_stack.push(PsObject::real(0.5)).unwrap();
        op_setgray(&mut ctx).unwrap();
        assert!(ctx.gstate.color_space_object.is_none());
        op_currentcolorspace(&mut ctx).unwrap();
        assert!(!same_array(&ctx.o_stack.pop().unwrap(), &space));
    }

    #[test]
    fn test_icc_range_defaults_to_unit_ranges() {
        let mut ctx = setup();
        let dict = crate::vm_ops::alloc_dict(&mut ctx, 4, b"icc");
        assert_eq!(icc_range(&ctx, dict, 3), [0.0, 1.0, 0.0, 1.0, 0.0, 1.0]);

        // A /Range of the wrong length is ignored rather than misread.
        let short = [PsObject::real(0.2), PsObject::real(1.0)];
        let entity = crate::vm_ops::alloc_array_from(&mut ctx, &short);
        let short = crate::vm_ops::make_array_obj(&ctx, entity, 2);
        let key = DictKey::Name(ctx.names.intern(b"Range"));
        ctx.dicts.put(dict, key, short);
        assert_eq!(icc_range(&ctx, dict, 3), [0.0, 1.0, 0.0, 1.0, 0.0, 1.0]);
        assert_eq!(icc_range(&ctx, dict, 1), [0.2, 1.0]);
    }

    #[test]
    fn test_clamp_to_ranges_takes_ranges_in_either_order() {
        assert_eq!(
            &*clamp_to_ranges(&[5.0, 5.0], &[0.0, 1.0, 1.0, 0.0]),
            &[1.0, 1.0]
        );
        // A NaN bound must not panic, as `f64::clamp` would: `min` and `max`
        // pass over it, so the other bound stands for both.
        assert_eq!(&*clamp_to_ranges(&[0.5], &[f64::NAN, 1.0]), &[1.0]);
    }
}
