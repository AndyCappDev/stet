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

/// `setgray`: num → —
pub fn op_setgray(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let gray = obj.as_f64().ok_or(PsError::TypeCheck)?;
    ctx.o_stack.pop()?;
    ctx.gstate.color = DeviceColor::from_gray(gray.clamp(0.0, 1.0));
    ctx.gstate.color_space = ColorSpace::DeviceGray;
    ctx.gstate.current_pattern = None;
    // UseCIEColor remapping (PLRM 6.2.5)
    if is_use_cie_color(ctx)
        && let Some(cs) = lookup_default_colorspace(ctx, b"DeviceGray")
    {
        ctx.gstate.color_space = cs;
    }
    Ok(())
}

/// `currentgray`: — → num
pub fn op_currentgray(ctx: &mut Context) -> Result<(), PsError> {
    let gray = ctx.gstate.color.to_gray();
    ctx.o_stack.push(PsObject::real(gray))?;
    Ok(())
}

/// `setrgbcolor`: r g b → —
pub fn op_setrgbcolor(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 3 {
        return Err(PsError::StackUnderflow);
    }
    let b_obj = ctx.o_stack.peek(0)?;
    let g_obj = ctx.o_stack.peek(1)?;
    let r_obj = ctx.o_stack.peek(2)?;
    let r = r_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let g = g_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let b = b_obj.as_f64().ok_or(PsError::TypeCheck)?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.gstate.color =
        DeviceColor::from_rgb(r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0));
    ctx.gstate.color_space = ColorSpace::DeviceRGB;
    ctx.gstate.current_pattern = None;
    // UseCIEColor remapping (PLRM 6.2.5)
    if is_use_cie_color(ctx)
        && let Some(cs) = lookup_default_colorspace(ctx, b"DeviceRGB")
    {
        ctx.gstate.color_space = cs;
    }
    Ok(())
}

/// `currentrgbcolor`: — → r g b
pub fn op_currentrgbcolor(ctx: &mut Context) -> Result<(), PsError> {
    ctx.o_stack.push(PsObject::real(ctx.gstate.color.r))?;
    ctx.o_stack.push(PsObject::real(ctx.gstate.color.g))?;
    ctx.o_stack.push(PsObject::real(ctx.gstate.color.b))?;
    Ok(())
}

/// `setcmykcolor`: c m y k → —
pub fn op_setcmykcolor(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 4 {
        return Err(PsError::StackUnderflow);
    }
    let k_obj = ctx.o_stack.peek(0)?;
    let y_obj = ctx.o_stack.peek(1)?;
    let m_obj = ctx.o_stack.peek(2)?;
    let c_obj = ctx.o_stack.peek(3)?;
    let c = c_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let m = m_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let y = y_obj.as_f64().ok_or(PsError::TypeCheck)?;
    let k = k_obj.as_f64().ok_or(PsError::TypeCheck)?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.gstate.color = DeviceColor::from_cmyk_icc(
        c.clamp(0.0, 1.0),
        m.clamp(0.0, 1.0),
        y.clamp(0.0, 1.0),
        k.clamp(0.0, 1.0),
        &mut ctx.icc_cache,
    );
    ctx.gstate.color_space = ColorSpace::DeviceCMYK;
    ctx.gstate.current_pattern = None;
    // UseCIEColor remapping (PLRM 6.2.5)
    if is_use_cie_color(ctx)
        && let Some(cs) = lookup_default_colorspace(ctx, b"DeviceCMYK")
    {
        ctx.gstate.color_space = cs;
    }
    Ok(())
}

/// `currentcmykcolor`: — → c m y k
pub fn op_currentcmykcolor(ctx: &mut Context) -> Result<(), PsError> {
    let (c, m, y, k) = ctx.gstate.color.to_cmyk();
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
    ctx.gstate.color_space = ColorSpace::DeviceRGB;
    ctx.gstate.current_pattern = None;
    Ok(())
}

/// `currenthsbcolor`: — → h s b
pub fn op_currenthsbcolor(ctx: &mut Context) -> Result<(), PsError> {
    let (h, s, b) = ctx.gstate.color.to_hsb();
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
            ctx.gstate.color_space = cs;
            ctx.gstate.current_pattern = None;
            Ok(())
        }
        // Also accept array form [/DeviceGray] or [/Indexed base hival lookup]
        PsValue::Array { entity, start, len } => {
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
                ctx.gstate.color_space = cs;
                ctx.gstate.current_pattern = None;
                cache_tint_table(ctx);
                Ok(())
            } else {
                Err(PsError::TypeCheck)
            }
        }
        _ => Err(PsError::TypeCheck),
    }
}

/// Sample and cache the tint transform for Separation/DeviceN color spaces.
/// Called after setcolorspace installs a new color space.
fn cache_tint_table(ctx: &mut Context) {
    match &ctx.gstate.color_space {
        ColorSpace::Separation {
            tint_transform,
            num_alt_components,
            ..
        } => {
            let tt = *tint_transform;
            let nac = *num_alt_components;
            ctx.gstate.cached_tint_table =
                crate::image_ops::sample_tint_transform(ctx, tt, 1, nac).map(Arc::new);
        }
        ColorSpace::DeviceN {
            num_colorants,
            tint_transform,
            num_alt_components,
            ..
        } => {
            let tt = *tint_transform;
            let nc = *num_colorants;
            let nac = *num_alt_components;
            ctx.gstate.cached_tint_table =
                crate::image_ops::sample_tint_transform(ctx, tt, nc, nac).map(Arc::new);
        }
        _ => {
            ctx.gstate.cached_tint_table = None;
        }
    }
    ctx.gstate.tint_values = None;
}

/// `currentcolorspace`: — → array
pub fn op_currentcolorspace(ctx: &mut Context) -> Result<(), PsError> {
    match &ctx.gstate.color_space {
        ColorSpace::Indexed {
            base,
            hival,
            lookup,
            ..
        } => {
            // Return [/Indexed base hival lookup_string]
            let idx_id = ctx.names.intern(b"Indexed");
            let idx_name = PsObject::name_lit(idx_id);

            let base_name_bytes = match base.as_ref() {
                ColorSpace::DeviceGray => b"DeviceGray" as &[u8],
                ColorSpace::DeviceRGB => b"DeviceRGB",
                ColorSpace::DeviceCMYK => b"DeviceCMYK",
                ColorSpace::Indexed { .. } => b"Indexed",
                ColorSpace::CIEBasedABC { .. } => b"CIEBasedABC",
                ColorSpace::CIEBasedA { .. } => b"CIEBasedA",
                ColorSpace::CIEBasedDEF { .. } => b"CIEBasedDEF",
                ColorSpace::CIEBasedDEFG { .. } => b"CIEBasedDEFG",
                ColorSpace::ICCBased { .. } => b"ICCBased",
                ColorSpace::Separation { .. } => b"Separation",
                ColorSpace::DeviceN { .. } => b"DeviceN",
            };
            let base_id = ctx.names.intern(base_name_bytes);
            let base_obj = PsObject::name_lit(base_id);

            let hival_obj = PsObject::int(*hival as i32);

            let lookup_clone = lookup.clone();
            let str_entity = crate::vm_ops::alloc_string(ctx, &lookup_clone);
            let str_obj =
                crate::vm_ops::make_string_obj(ctx, str_entity, lookup_clone.len() as u32);

            let items = [idx_name, base_obj, hival_obj, str_obj];
            let entity = crate::vm_ops::alloc_array_from(ctx, &items);
            let arr = crate::vm_ops::make_array_obj(ctx, entity, 4);
            ctx.o_stack.push(arr)?;
        }
        ColorSpace::CIEBasedABC { dict_entity, .. } => {
            let dict_entity = *dict_entity;
            let name_id = ctx.names.intern(b"CIEBasedABC");
            let name_obj = PsObject::name_lit(name_id);
            let dict_obj = crate::vm_ops::make_dict_obj(ctx, dict_entity);
            let items = [name_obj, dict_obj];
            let entity = crate::vm_ops::alloc_array_from(ctx, &items);
            let arr = crate::vm_ops::make_array_obj(ctx, entity, 2);
            ctx.o_stack.push(arr)?;
        }
        ColorSpace::CIEBasedA { dict_entity, .. } => {
            let dict_entity = *dict_entity;
            let name_id = ctx.names.intern(b"CIEBasedA");
            let name_obj = PsObject::name_lit(name_id);
            let dict_obj = crate::vm_ops::make_dict_obj(ctx, dict_entity);
            let items = [name_obj, dict_obj];
            let entity = crate::vm_ops::alloc_array_from(ctx, &items);
            let arr = crate::vm_ops::make_array_obj(ctx, entity, 2);
            ctx.o_stack.push(arr)?;
        }
        ColorSpace::CIEBasedDEF { dict_entity, .. } => {
            let dict_entity = *dict_entity;
            let name_id = ctx.names.intern(b"CIEBasedDEF");
            let name_obj = PsObject::name_lit(name_id);
            let dict_obj = crate::vm_ops::make_dict_obj(ctx, dict_entity);
            let items = [name_obj, dict_obj];
            let entity = crate::vm_ops::alloc_array_from(ctx, &items);
            let arr = crate::vm_ops::make_array_obj(ctx, entity, 2);
            ctx.o_stack.push(arr)?;
        }
        ColorSpace::CIEBasedDEFG { dict_entity, .. } => {
            let dict_entity = *dict_entity;
            let name_id = ctx.names.intern(b"CIEBasedDEFG");
            let name_obj = PsObject::name_lit(name_id);
            let dict_obj = crate::vm_ops::make_dict_obj(ctx, dict_entity);
            let items = [name_obj, dict_obj];
            let entity = crate::vm_ops::alloc_array_from(ctx, &items);
            let arr = crate::vm_ops::make_array_obj(ctx, entity, 2);
            ctx.o_stack.push(arr)?;
        }
        ColorSpace::ICCBased { dict_entity, .. } => {
            let dict_entity = *dict_entity;
            let name_id = ctx.names.intern(b"ICCBased");
            let name_obj = PsObject::name_lit(name_id);
            let dict_obj = crate::vm_ops::make_dict_obj(ctx, dict_entity);
            let items = [name_obj, dict_obj];
            let entity = crate::vm_ops::alloc_array_from(ctx, &items);
            let arr = crate::vm_ops::make_array_obj(ctx, entity, 2);
            ctx.o_stack.push(arr)?;
        }
        cs => {
            let name_bytes = match cs {
                ColorSpace::DeviceGray => b"DeviceGray" as &[u8],
                ColorSpace::DeviceRGB => b"DeviceRGB",
                ColorSpace::DeviceCMYK => b"DeviceCMYK",
                ColorSpace::Separation { .. } => b"Separation",
                ColorSpace::DeviceN { .. } => b"DeviceN",
                _ => b"DeviceGray",
            };
            let name_id = ctx.names.intern(name_bytes);
            let name_obj = PsObject::name_lit(name_id);
            let entity = crate::vm_ops::alloc_array_from(ctx, &[name_obj]);
            let arr = crate::vm_ops::make_array_obj(ctx, entity, 1);
            ctx.o_stack.push(arr)?;
        }
    }
    Ok(())
}

/// `setcolor`: comp1 ... compn → — (set color using current color space)
pub fn op_setcolor(ctx: &mut Context) -> Result<(), PsError> {
    // Clear tint values by default; Separation/DeviceN arms re-set them.
    ctx.gstate.tint_values = None;
    match ctx.gstate.color_space.clone() {
        ColorSpace::DeviceGray => op_setgray(ctx),
        ColorSpace::DeviceRGB => op_setrgbcolor(ctx),
        ColorSpace::DeviceCMYK => op_setcmykcolor(ctx),
        ColorSpace::Indexed { .. } => set_indexed_color(ctx),
        ColorSpace::CIEBasedABC { params, .. } => set_cie_abc_color(ctx, &params),
        ColorSpace::CIEBasedA { params, .. } => set_cie_a_color(ctx, &params),
        ColorSpace::CIEBasedDEF { params, .. } => set_cie_def_color(ctx, &params),
        ColorSpace::CIEBasedDEFG { params, .. } => set_cie_defg_color(ctx, &params),
        ColorSpace::ICCBased {
            n, profile_hash, ..
        } => {
            if let Some(hash) = profile_hash {
                set_icc_color(ctx, n, &hash)
            } else {
                // No embedded ICC profile — use PLRM formulas, not system ICC profile
                match n {
                    1 => op_setgray(ctx),
                    3 => op_setrgbcolor(ctx),
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
                        ctx.gstate.current_pattern = None;
                        Ok(())
                    }
                    _ => Err(PsError::RangeCheck),
                }
            }
        }
        ColorSpace::Separation {
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
            ctx.o_stack.push(PsObject::real(tint_clamped))?;
            ctx.exec_sync(tint_transform)?;
            set_color_from_tint_result(ctx, num_alt_components)
        }
        ColorSpace::DeviceN {
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
            for _ in 0..num_colorants {
                ctx.o_stack.pop()?;
            }
            for &t in &tints {
                ctx.o_stack.push(PsObject::real(t))?;
            }
            ctx.exec_sync(tint_transform)?;
            set_color_from_tint_result(ctx, num_alt_components)
        }
    }
}

/// Pop tint transform results from the stack and set the device color.
fn set_color_from_tint_result(ctx: &mut Context, n: u32) -> Result<(), PsError> {
    let n = n as usize;
    let mut components = vec![0.0f64; n];
    for i in (0..n).rev() {
        if !ctx.o_stack.is_empty() {
            components[i] = ctx.o_stack.peek(0)?.as_f64().unwrap_or(0.0).clamp(0.0, 1.0);
            ctx.o_stack.pop()?;
        }
    }

    ctx.gstate.color = match n {
        1 => DeviceColor::from_gray(components[0]),
        3 => DeviceColor::from_rgb(components[0], components[1], components[2]),
        4 => DeviceColor::from_cmyk_icc(
            components[0],
            components[1],
            components[2],
            components[3],
            &mut ctx.icc_cache,
        ),
        _ => DeviceColor::from_gray(0.0),
    };
    ctx.gstate.current_pattern = None;
    Ok(())
}

/// `currentcolor`: — → comp1 ... compn (color in current color space)
///
/// For Indexed color spaces, returns the resolved base-space components.
pub fn op_currentcolor(ctx: &mut Context) -> Result<(), PsError> {
    match &ctx.gstate.color_space {
        ColorSpace::DeviceGray => {
            let gray = ctx.gstate.color.to_gray();
            ctx.o_stack.push(PsObject::real(gray))?;
        }
        ColorSpace::Indexed { base, .. } => {
            // Return resolved components in the base color space
            match base.as_ref() {
                ColorSpace::DeviceGray => {
                    let gray = ctx.gstate.color.to_gray();
                    ctx.o_stack.push(PsObject::real(gray))?;
                }
                ColorSpace::DeviceRGB => {
                    ctx.o_stack.push(PsObject::real(ctx.gstate.color.r))?;
                    ctx.o_stack.push(PsObject::real(ctx.gstate.color.g))?;
                    ctx.o_stack.push(PsObject::real(ctx.gstate.color.b))?;
                }
                ColorSpace::DeviceCMYK => {
                    let (c, m, y, k) = ctx.gstate.color.to_cmyk();
                    ctx.o_stack.push(PsObject::real(c))?;
                    ctx.o_stack.push(PsObject::real(m))?;
                    ctx.o_stack.push(PsObject::real(y))?;
                    ctx.o_stack.push(PsObject::real(k))?;
                }
                _ => {
                    let gray = ctx.gstate.color.to_gray();
                    ctx.o_stack.push(PsObject::real(gray))?;
                }
            }
        }
        ColorSpace::CIEBasedA { .. } => {
            // CIEBasedA has 1 component
            ctx.o_stack.push(PsObject::real(ctx.gstate.color.r))?;
        }
        ColorSpace::CIEBasedABC { .. } | ColorSpace::CIEBasedDEF { .. } => {
            // CIEBasedABC/DEF have 3 components
            ctx.o_stack.push(PsObject::real(ctx.gstate.color.r))?;
            ctx.o_stack.push(PsObject::real(ctx.gstate.color.g))?;
            ctx.o_stack.push(PsObject::real(ctx.gstate.color.b))?;
        }
        ColorSpace::CIEBasedDEFG { .. } => {
            // CIEBasedDEFG has 4 components
            let (c, m, y, k) = ctx.gstate.color.to_cmyk();
            ctx.o_stack.push(PsObject::real(c))?;
            ctx.o_stack.push(PsObject::real(m))?;
            ctx.o_stack.push(PsObject::real(y))?;
            ctx.o_stack.push(PsObject::real(k))?;
        }
        ColorSpace::ICCBased { n, .. } => {
            // ICCBased falls back to device space behavior
            match n {
                1 => {
                    let gray = ctx.gstate.color.to_gray();
                    ctx.o_stack.push(PsObject::real(gray))?;
                }
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
        }
        ColorSpace::DeviceRGB => {
            ctx.o_stack.push(PsObject::real(ctx.gstate.color.r))?;
            ctx.o_stack.push(PsObject::real(ctx.gstate.color.g))?;
            ctx.o_stack.push(PsObject::real(ctx.gstate.color.b))?;
        }
        ColorSpace::DeviceCMYK => {
            let (c, m, y, k) = ctx.gstate.color.to_cmyk();
            ctx.o_stack.push(PsObject::real(c))?;
            ctx.o_stack.push(PsObject::real(m))?;
            ctx.o_stack.push(PsObject::real(y))?;
            ctx.o_stack.push(PsObject::real(k))?;
        }
        ColorSpace::Separation {
            num_alt_components, ..
        }
        | ColorSpace::DeviceN {
            num_alt_components, ..
        } => {
            // Color is already in alternative space
            match num_alt_components {
                1 => {
                    ctx.o_stack
                        .push(PsObject::real(ctx.gstate.color.to_gray()))?;
                }
                4 => {
                    let (c, m, y, k) = ctx.gstate.color.to_cmyk();
                    ctx.o_stack.push(PsObject::real(c))?;
                    ctx.o_stack.push(PsObject::real(m))?;
                    ctx.o_stack.push(PsObject::real(y))?;
                    ctx.o_stack.push(PsObject::real(k))?;
                }
                _ => {
                    // 3-component (RGB) or default
                    ctx.o_stack.push(PsObject::real(ctx.gstate.color.r))?;
                    ctx.o_stack.push(PsObject::real(ctx.gstate.color.g))?;
                    ctx.o_stack.push(PsObject::real(ctx.gstate.color.b))?;
                }
            }
        }
    }
    Ok(())
}

/// Set color in an Indexed color space: index → —
///
/// Looks up the index in the palette and sets the resolved base-space color.
/// Set color from ICCBased color space with an actual ICC profile.
/// Pops N components, converts through ICC profile, stores as DeviceColor.
fn set_icc_color(ctx: &mut Context, n: u32, hash: &[u8; 32]) -> Result<(), PsError> {
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
    // Convert through ICC
    if let Some((r, g, b)) = ctx.icc_cache.convert_color(hash, &comps) {
        ctx.gstate.color = DeviceColor::from_rgb(r, g, b);
    } else {
        // Fallback to device-based conversion
        ctx.gstate.color = match n {
            1 => DeviceColor::from_gray(comps[0].clamp(0.0, 1.0)),
            3 => DeviceColor::from_rgb(
                comps[0].clamp(0.0, 1.0),
                comps[1].clamp(0.0, 1.0),
                comps[2].clamp(0.0, 1.0),
            ),
            4 => DeviceColor::from_cmyk_icc(
                comps[0].clamp(0.0, 1.0),
                comps[1].clamp(0.0, 1.0),
                comps[2].clamp(0.0, 1.0),
                comps[3].clamp(0.0, 1.0),
                &mut ctx.icc_cache,
            ),
            _ => DeviceColor::black(),
        };
    }
    Ok(())
}

fn set_indexed_color(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let idx_obj = ctx.o_stack.peek(0)?;
    let idx = match idx_obj.value {
        PsValue::Int(v) => v as f64,
        PsValue::Real(v) => v,
        _ => return Err(PsError::TypeCheck),
    };

    // Clone the colorspace data we need before mutating ctx
    let (base, hival, lookup) = match &ctx.gstate.color_space {
        ColorSpace::Indexed {
            base,
            hival,
            lookup,
            ..
        } => (base.clone(), *hival, lookup.clone()),
        _ => return Err(PsError::TypeCheck),
    };

    // Round and clamp index
    let index = (idx.round() as i32).clamp(0, hival as i32) as usize;

    // Look up color in palette based on base color space
    let components_per_color = match base.as_ref() {
        ColorSpace::DeviceGray => 1,
        ColorSpace::DeviceRGB => 3,
        ColorSpace::DeviceCMYK => 4,
        _ => 1,
    };

    let offset = index * components_per_color;
    if offset + components_per_color > lookup.len() {
        return Err(PsError::RangeCheck);
    }

    let color = match base.as_ref() {
        ColorSpace::DeviceGray => {
            let g = lookup[offset] as f64 / 255.0;
            DeviceColor::from_gray(g)
        }
        ColorSpace::DeviceRGB => {
            let r = lookup[offset] as f64 / 255.0;
            let g = lookup[offset + 1] as f64 / 255.0;
            let b = lookup[offset + 2] as f64 / 255.0;
            DeviceColor::from_rgb(r, g, b)
        }
        ColorSpace::DeviceCMYK => {
            let c = lookup[offset] as f64 / 255.0;
            let m = lookup[offset + 1] as f64 / 255.0;
            let y = lookup[offset + 2] as f64 / 255.0;
            let k = lookup[offset + 3] as f64 / 255.0;
            DeviceColor::from_cmyk_icc(c, m, y, k, &mut ctx.icc_cache)
        }
        _ => DeviceColor::from_gray(0.0),
    };

    ctx.o_stack.pop()?;
    ctx.gstate.color = color;
    ctx.gstate.current_pattern = None;
    Ok(())
}

/// Return the default (initial) color for a color space per PLRM.
fn default_color_for_space(cs: &ColorSpace, ctx: &mut Context) -> DeviceColor {
    match cs {
        ColorSpace::DeviceGray => DeviceColor::from_gray(0.0),
        ColorSpace::DeviceRGB => DeviceColor::from_rgb(0.0, 0.0, 0.0),
        ColorSpace::DeviceCMYK => {
            DeviceColor::from_cmyk_icc(0.0, 0.0, 0.0, 1.0, &mut ctx.icc_cache)
        }
        ColorSpace::CIEBasedABC { params, .. } => DeviceColor::from_cie_abc(0.0, 0.0, 0.0, params),
        ColorSpace::CIEBasedA { params, .. } => DeviceColor::from_cie_a(0.0, params),
        ColorSpace::CIEBasedDEF { params, .. } => DeviceColor::from_cie_def(0.0, 0.0, 0.0, params),
        ColorSpace::CIEBasedDEFG { params, .. } => {
            DeviceColor::from_cie_defg(0.0, 0.0, 0.0, 0.0, params)
        }
        ColorSpace::ICCBased {
            n, profile_hash, ..
        } => {
            let default_comps: &[f64] = match n {
                1 => &[0.0],
                3 => &[0.0, 0.0, 0.0],
                4 => &[0.0, 0.0, 0.0, 1.0],
                _ => return DeviceColor::black(),
            };
            if let Some(hash) = profile_hash
                && let Some((r, g, b)) = ctx.icc_cache.convert_color(hash, default_comps)
            {
                return DeviceColor::from_rgb(r, g, b);
            }
            match n {
                1 => DeviceColor::from_gray(0.0),
                3 => DeviceColor::from_rgb(0.0, 0.0, 0.0),
                4 => DeviceColor::from_cmyk_icc(0.0, 0.0, 0.0, 1.0, &mut ctx.icc_cache),
                _ => DeviceColor::black(),
            }
        }
        ColorSpace::Indexed { base, lookup, .. } => {
            // Default is index 0 resolved through the palette
            let components_per_color = match base.as_ref() {
                ColorSpace::DeviceGray => 1,
                ColorSpace::DeviceRGB => 3,
                ColorSpace::DeviceCMYK => 4,
                _ => 1,
            };
            if lookup.len() >= components_per_color {
                match base.as_ref() {
                    ColorSpace::DeviceGray => DeviceColor::from_gray(lookup[0] as f64 / 255.0),
                    ColorSpace::DeviceRGB => DeviceColor::from_rgb(
                        lookup[0] as f64 / 255.0,
                        lookup[1] as f64 / 255.0,
                        lookup[2] as f64 / 255.0,
                    ),
                    ColorSpace::DeviceCMYK => DeviceColor::from_cmyk_icc(
                        lookup[0] as f64 / 255.0,
                        lookup[1] as f64 / 255.0,
                        lookup[2] as f64 / 255.0,
                        lookup[3] as f64 / 255.0,
                        &mut ctx.icc_cache,
                    ),
                    _ => DeviceColor::from_gray(0.0),
                }
            } else {
                DeviceColor::from_gray(0.0)
            }
        }
        // Separation/DeviceN: default color requires tint transform execution,
        // which needs the eval loop. Default to black; setcolor will run the
        // tint transform via continuation to set the actual color.
        ColorSpace::Separation { .. } | ColorSpace::DeviceN { .. } => DeviceColor::black(),
    }
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
                _ => Err(PsError::Undefined),
            }
        }
        PsValue::Array { entity, start, len } => {
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

/// Parse `[/Indexed base hival lookup]` from an array.
fn parse_indexed_colorspace(
    ctx: &Context,
    entity: stet_core::object::EntityId,
    start: u32,
) -> Result<ColorSpace, PsError> {
    // Element 1: base color space (must be a device color space name)
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
        _ => return Err(PsError::TypeCheck),
    };

    // Element 2: hival (max index, 0–255)
    let hival_obj = ctx.arrays.get_element(entity, start + 2);
    let hival_i = hival_obj.as_i32().ok_or(PsError::TypeCheck)?;
    if !(0..=255).contains(&hival_i) {
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
        PsValue::ExecArray { .. } | PsValue::Array { .. } => {
            // Procedure lookup — will be pre-evaluated in resolve_indexed_proc_lookup
            (Vec::new(), Some(lookup_obj))
        }
        _ => return Err(PsError::TypeCheck),
    };

    if lookup_proc.is_none() {
        // Validate lookup length: must have (hival+1) * components_per_color bytes
        let components_per_color = match &base {
            ColorSpace::DeviceGray => 1,
            ColorSpace::DeviceRGB => 3,
            ColorSpace::DeviceCMYK => 4,
            _ => 1,
        };
        let required_len = (hival as usize + 1) * components_per_color;
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
    let (entity, start, len) = match resource_obj.value {
        PsValue::Array { entity, start, len } => (entity, start, len),
        _ => return None,
    };
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
    let params = CieDefParams {
        range_def,
        m1: 0,
        m2: 0,
        m3: 0,
        a_table: Vec::new(),
        b_table: Vec::new(),
        c_table: Vec::new(),
        abc_params: CieAbcParams::default(),
    };
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
    let params = CieDefgParams {
        range_defg,
        m1: 0,
        m2: 0,
        m3: 0,
        m4: 0,
        a_table: Vec::new(),
        b_table: Vec::new(),
        c_table: Vec::new(),
        abc_params: CieAbcParams::default(),
    };
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

/// Parse `[/Separation name alternativeSpace tintTransform]` from an array.
fn parse_separation_colorspace(
    ctx: &Context,
    entity: EntityId,
    start: u32,
) -> Result<ColorSpace, PsError> {
    // Element 1: colorant name
    let name_obj = ctx.arrays.get_element(entity, start + 1);
    let name = match name_obj.value {
        PsValue::Name(id) => ctx.names.get_bytes(id).to_vec(),
        _ => Vec::new(),
    };

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
    ctx: &Context,
    entity: EntityId,
    start: u32,
) -> Result<ColorSpace, PsError> {
    // Element 1: array of colorant names
    let names_obj = ctx.arrays.get_element(entity, start + 1);
    let (num_colorants, names) = match names_obj.value {
        PsValue::Array {
            entity: name_ent,
            start: name_start,
            len,
        } => {
            let mut names = Vec::with_capacity(len as usize);
            for i in 0..len {
                let elem = ctx.arrays.get_element(name_ent, name_start + i);
                let name_bytes = match elem.value {
                    PsValue::Name(id) => ctx.names.get_bytes(id).to_vec(),
                    _ => Vec::new(),
                };
                names.push(name_bytes);
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
fn parse_alt_space_name(ctx: &Context, obj: &PsObject) -> Result<(ColorSpace, u32), PsError> {
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
            PsValue::Array { entity, start, len } => {
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
    ctx.gstate.current_pattern = None;
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
    ctx.gstate.current_pattern = None;
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
    ctx.gstate.current_pattern = None;
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
    ctx.gstate.current_pattern = None;
    Ok(())
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
    }
}

/// Pre-evaluate CIE decode procedures and DEF/DEFG lookup tables.
///
/// For CIEBasedABC/A: evaluates DecodeABC, DecodeLMN, DecodeA procedures at 256
/// If the Indexed color space has a procedure-based lookup, pre-evaluate it
/// by calling the procedure for each index 0..hival, collecting the color
/// component bytes into a lookup Vec<u8>.
fn resolve_indexed_proc_lookup(ctx: &mut Context, cs: ColorSpace) -> Result<ColorSpace, PsError> {
    if let ColorSpace::Indexed {
        ref base,
        hival,
        ref lookup_proc,
        ..
    } = cs
        && let Some(proc_obj) = *lookup_proc
    {
        let ncomp = match base.as_ref() {
            ColorSpace::DeviceGray => 1,
            ColorSpace::DeviceRGB => 3,
            ColorSpace::DeviceCMYK => 4,
            _ => 3,
        };
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
            lookup_proc: None,
        });
    }
    Ok(cs)
}

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
    match obj.value {
        PsValue::Array { entity, start, len } => {
            if (len as usize) < expected {
                return None;
            }
            let mut procs = Vec::with_capacity(expected);
            for i in 0..expected as u32 {
                let elem = ctx.arrays.get_element(entity, start + i);
                // Each element should be an executable array (procedure)
                match elem.value {
                    PsValue::Array { .. } if elem.flags.is_executable() => procs.push(elem),
                    _ => return None,
                }
            }
            Some(procs)
        }
        _ => None,
    }
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
    match obj.value {
        PsValue::Array { .. } if obj.flags.is_executable() => Some(obj),
        _ => None,
    }
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
            h.word(v as i64 as u64);
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

/// Pre-convert a CIEBasedDEF 3D lookup table to sRGB.
fn precompute_cie_def_table(
    ctx: &mut Context,
    dict_entity: EntityId,
) -> Result<CieDefParams, PsError> {
    // Extract RangeDEF
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

    // Extract Table: [m1 m2 m3 [string1 ... string_m1]]
    let table_obj = get_dict_obj_by_key(ctx, dict_entity, b"Table");
    let (m1, m2, m3, strings_entity, strings_start) = match table_obj {
        Some(obj) => match obj.value {
            PsValue::Array { entity, start, len } if len >= 4 => {
                let m1 = ctx.arrays.get_element(entity, start).as_i32().unwrap_or(0) as usize;
                let m2 = ctx
                    .arrays
                    .get_element(entity, start + 1)
                    .as_i32()
                    .unwrap_or(0) as usize;
                let m3 = ctx
                    .arrays
                    .get_element(entity, start + 2)
                    .as_i32()
                    .unwrap_or(0) as usize;
                let strings_obj = ctx.arrays.get_element(entity, start + 3);
                match strings_obj.value {
                    PsValue::Array {
                        entity: se,
                        start: ss,
                        ..
                    } => (m1, m2, m3, se, ss),
                    _ => return Err(PsError::TypeCheck),
                }
            }
            _ => return Err(PsError::TypeCheck),
        },
        None => return Err(PsError::Undefined),
    };

    if m1 == 0 || m2 == 0 || m3 == 0 {
        return Err(PsError::RangeCheck);
    }

    // Build CIE ABC params for conversion (with decode tables)
    let abc_params = extract_cie_abc_params(ctx, dict_entity);
    // Pre-evaluate DecodeABC and DecodeLMN over their respective ranges
    let mut abc_params = abc_params;
    if let Some(procs) = get_cie_decode_procs(ctx, dict_entity, b"DecodeABC", 3) {
        let mut tables: [Vec<f64>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for (ch, proc) in procs.iter().enumerate() {
            let lo = abc_params.range_abc[ch * 2];
            let hi = abc_params.range_abc[ch * 2 + 1];
            tables[ch] = eval_decode_table_range(ctx, *proc, 256, lo, hi)?;
        }
        abc_params.decode_abc = Some(tables);
    }
    if let Some(procs) = get_cie_decode_procs(ctx, dict_entity, b"DecodeLMN", 3) {
        let mut tables: [Vec<f64>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for (ch, proc) in procs.iter().enumerate() {
            let lo = abc_params.range_lmn[ch * 2];
            let hi = abc_params.range_lmn[ch * 2 + 1];
            tables[ch] = eval_decode_table_range(ctx, *proc, 256, lo, hi)?;
        }
        abc_params.decode_lmn = Some(tables);
    }

    // Extract RangeABC for byte scaling
    let abc_min = [
        abc_params.range_abc[0],
        abc_params.range_abc[2],
        abc_params.range_abc[4],
    ];
    let abc_scale = [
        (abc_params.range_abc[1] - abc_params.range_abc[0]) / 255.0,
        (abc_params.range_abc[3] - abc_params.range_abc[2]) / 255.0,
        (abc_params.range_abc[5] - abc_params.range_abc[4]) / 255.0,
    ];

    // Extract table ABC values (interpolation done in ABC space, not RGB)
    let total = m1 * m2 * m3;
    let mut a_table = vec![0.0f64; total];
    let mut b_table = vec![0.0f64; total];
    let mut c_table = vec![0.0f64; total];

    let mut idx = 0;
    for di in 0..m1 {
        let string_obj = ctx
            .arrays
            .get_element(strings_entity, strings_start + di as u32);
        let data = match string_obj.value {
            PsValue::String {
                entity: se,
                start: ss,
                len: sl,
            } => ctx.strings.get(se, ss, sl).to_vec(),
            _ => continue,
        };

        for ei in 0..m2 {
            for fi in 0..m3 {
                let offset = (ei * m3 + fi) * 3;
                if offset + 2 < data.len() {
                    a_table[idx] = abc_min[0] + data[offset] as f64 * abc_scale[0];
                    b_table[idx] = abc_min[1] + data[offset + 1] as f64 * abc_scale[1];
                    c_table[idx] = abc_min[2] + data[offset + 2] as f64 * abc_scale[2];
                }
                idx += 1;
            }
        }
    }

    Ok(CieDefParams {
        range_def,
        m1,
        m2,
        m3,
        a_table,
        b_table,
        c_table,
        abc_params,
    })
}

/// Pre-convert a CIEBasedDEFG 4D lookup table to sRGB.
fn precompute_cie_defg_table(
    ctx: &mut Context,
    dict_entity: EntityId,
) -> Result<CieDefgParams, PsError> {
    // Extract RangeDEFG
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

    // Extract Table: [m1 m2 m3 m4 [string1 ... string_m1]]
    let table_obj = get_dict_obj_by_key(ctx, dict_entity, b"Table");
    let (m1, m2, m3, m4, strings_entity, strings_start) = match table_obj {
        Some(obj) => match obj.value {
            PsValue::Array { entity, start, len } if len >= 5 => {
                let m1 = ctx.arrays.get_element(entity, start).as_i32().unwrap_or(0) as usize;
                let m2 = ctx
                    .arrays
                    .get_element(entity, start + 1)
                    .as_i32()
                    .unwrap_or(0) as usize;
                let m3 = ctx
                    .arrays
                    .get_element(entity, start + 2)
                    .as_i32()
                    .unwrap_or(0) as usize;
                let m4 = ctx
                    .arrays
                    .get_element(entity, start + 3)
                    .as_i32()
                    .unwrap_or(0) as usize;
                let strings_obj = ctx.arrays.get_element(entity, start + 4);
                match strings_obj.value {
                    PsValue::Array {
                        entity: se,
                        start: ss,
                        ..
                    } => (m1, m2, m3, m4, se, ss),
                    _ => return Err(PsError::TypeCheck),
                }
            }
            _ => return Err(PsError::TypeCheck),
        },
        None => return Err(PsError::Undefined),
    };

    if m1 == 0 || m2 == 0 || m3 == 0 || m4 == 0 {
        return Err(PsError::RangeCheck);
    }

    // Build CIE ABC params for conversion (with decode tables)
    let mut abc_params = extract_cie_abc_params(ctx, dict_entity);
    if let Some(procs) = get_cie_decode_procs(ctx, dict_entity, b"DecodeABC", 3) {
        let mut tables: [Vec<f64>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for (ch, proc) in procs.iter().enumerate() {
            let lo = abc_params.range_abc[ch * 2];
            let hi = abc_params.range_abc[ch * 2 + 1];
            tables[ch] = eval_decode_table_range(ctx, *proc, 256, lo, hi)?;
        }
        abc_params.decode_abc = Some(tables);
    }
    if let Some(procs) = get_cie_decode_procs(ctx, dict_entity, b"DecodeLMN", 3) {
        let mut tables: [Vec<f64>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for (ch, proc) in procs.iter().enumerate() {
            let lo = abc_params.range_lmn[ch * 2];
            let hi = abc_params.range_lmn[ch * 2 + 1];
            tables[ch] = eval_decode_table_range(ctx, *proc, 256, lo, hi)?;
        }
        abc_params.decode_lmn = Some(tables);
    }

    let abc_min = [
        abc_params.range_abc[0],
        abc_params.range_abc[2],
        abc_params.range_abc[4],
    ];
    let abc_scale = [
        (abc_params.range_abc[1] - abc_params.range_abc[0]) / 255.0,
        (abc_params.range_abc[3] - abc_params.range_abc[2]) / 255.0,
        (abc_params.range_abc[5] - abc_params.range_abc[4]) / 255.0,
    ];

    // Extract table ABC values (conversion done at lookup time, not pre-converted)
    let total = m1 * m2 * m3 * m4;
    let mut a_table = vec![0.0f64; total];
    let mut b_table = vec![0.0f64; total];
    let mut c_table = vec![0.0f64; total];

    let mut idx = 0;
    for di in 0..m1 {
        let string_obj = ctx
            .arrays
            .get_element(strings_entity, strings_start + di as u32);
        let data = match string_obj.value {
            PsValue::String {
                entity: se,
                start: ss,
                len: sl,
            } => ctx.strings.get(se, ss, sl).to_vec(),
            _ => continue,
        };

        for ei in 0..m2 {
            for fi in 0..m3 {
                for gi in 0..m4 {
                    let offset = (ei * m3 * m4 + fi * m4 + gi) * 3;
                    if offset + 2 < data.len() {
                        a_table[idx] = abc_min[0] + data[offset] as f64 * abc_scale[0];
                        b_table[idx] = abc_min[1] + data[offset + 1] as f64 * abc_scale[1];
                        c_table[idx] = abc_min[2] + data[offset + 2] as f64 * abc_scale[2];
                    }
                    idx += 1;
                }
            }
        }
    }

    Ok(CieDefgParams {
        range_defg,
        m1,
        m2,
        m3,
        m4,
        a_table,
        b_table,
        c_table,
        abc_params,
    })
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
        let entity = ctx.arrays.allocate_from(elems);
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
        let inner = ctx.arrays.allocate_from(&[PsObject::int(0)]);
        let mut inner_proc = PsObject::array(inner, 1);
        inner_proc.flags = stet_core::object::ObjFlags::executable_composite();
        let outer = ctx.arrays.allocate_from(&[inner_proc]);
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
        let d = ctx.dicts.allocate(4, b"opaque");
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
}
