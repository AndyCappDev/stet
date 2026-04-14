// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Halftone, transfer, pattern, and device operators.
//!
//! These operators store their parameters in the graphics state so that
//! the corresponding `current*` operators return what was set. Since stet
//! renders to raster devices directly, the halftone/transfer values are
//! not used during rendering, but they must be preserved for PS programs
//! that query them.

use std::sync::Arc;

use stet_core::context::Context;
use stet_core::dict::DictKey;
use stet_core::error::PsError;
use stet_core::graphics_state::PatternData;
use stet_core::object::{PsObject, PsValue};
use stet_fonts::geometry::Matrix;
use stet_graphics::device::HalftoneScreen;
use stet_graphics::display_list::DisplayList;

// ---------- Halftone pre-computation for PDF output ----------

/// PDF Type 4 calculator function allowed operators.
const TYPE4_ALLOWED: &[&[u8]] = &[
    b"abs",
    b"add",
    b"atan",
    b"ceiling",
    b"cos",
    b"cvi",
    b"cvr",
    b"div",
    b"exp",
    b"floor",
    b"idiv",
    b"ln",
    b"log",
    b"mod",
    b"mul",
    b"neg",
    b"round",
    b"sin",
    b"sqrt",
    b"sub",
    b"truncate",
    b"eq",
    b"ne",
    b"gt",
    b"ge",
    b"lt",
    b"le",
    b"and",
    b"or",
    b"xor",
    b"not",
    b"bitshift",
    b"if",
    b"ifelse",
    b"copy",
    b"dup",
    b"exch",
    b"index",
    b"pop",
    b"roll",
    b"true",
    b"false",
];

/// Try to decompile a PS spot function procedure to PDF Type 4 calculator bytes.
fn decompile_spot_to_type4(ctx: &Context, proc: PsObject) -> Option<Vec<u8>> {
    let (entity, start, len) = match proc.value {
        PsValue::Array { entity, start, len } | PsValue::PackedArray { entity, start, len } => {
            (entity, start, len)
        }
        _ => return None,
    };
    if len == 0 {
        return None;
    }
    let mut result = Vec::new();
    result.extend(b"{ ");
    if !decompile_elements(ctx, entity, start, len, &mut result) {
        return None;
    }
    result.extend(b"}");
    Some(result)
}

/// Recursively decompile array elements to Type 4 tokens. Returns false on failure.
fn decompile_elements(
    ctx: &Context,
    entity: stet_core::object::EntityId,
    start: u32,
    len: u32,
    result: &mut Vec<u8>,
) -> bool {
    for i in 0..len {
        let obj = ctx.arrays.get_element(entity, start + i);
        match obj.value {
            PsValue::Int(n) => {
                use std::io::Write;
                write!(result, "{} ", n).unwrap();
            }
            PsValue::Real(f) => {
                use std::io::Write;
                write!(result, "{} ", f).unwrap();
            }
            PsValue::Bool(b) => {
                if b {
                    result.extend(b"true ");
                } else {
                    result.extend(b"false ");
                }
            }
            PsValue::Operator(opcode) => {
                let name_id = ctx.operators[opcode.0 as usize].name;
                let name_bytes = ctx.names.get_bytes(name_id);
                if !TYPE4_ALLOWED.contains(&name_bytes) {
                    return false;
                }
                result.extend(name_bytes);
                result.push(b' ');
            }
            PsValue::Name(name_id) if obj.flags.is_executable() => {
                let name_bytes = ctx.names.get_bytes(name_id);
                if !TYPE4_ALLOWED.contains(&name_bytes) {
                    return false;
                }
                result.extend(name_bytes);
                result.push(b' ');
            }
            PsValue::Array {
                entity: e,
                start: s,
                len: l,
            }
            | PsValue::PackedArray {
                entity: e,
                start: s,
                len: l,
            } if obj.flags.is_executable() => {
                result.extend(b"{ ");
                if !decompile_elements(ctx, e, s, l, result) {
                    return false;
                }
                result.extend(b"} ");
            }
            _ => return false,
        }
    }
    true
}

/// Sample a spot function on a 64×64 grid (domain [-1,1]², range [0,1]).
fn sample_spot_function_2d(
    ctx: &mut Context,
    proc: PsObject,
) -> Result<Option<Arc<Vec<f64>>>, PsError> {
    // Empty procedure = default
    if let PsValue::Array { len, .. } | PsValue::PackedArray { len, .. } = proc.value
        && len == 0
    {
        return Ok(None);
    }
    let mut table = Vec::with_capacity(64 * 64);
    for iy in 0..64 {
        let y = -1.0 + iy as f64 * 2.0 / 63.0;
        for ix in 0..64 {
            let x = -1.0 + ix as f64 * 2.0 / 63.0;
            ctx.o_stack.push(PsObject::real(x))?;
            ctx.o_stack.push(PsObject::real(y))?;
            ctx.exec_sync(proc)?;
            let result = if !ctx.o_stack.is_empty() {
                ctx.o_stack.pop()?.as_f64().unwrap_or(0.5).clamp(0.0, 1.0)
            } else {
                0.5
            };
            table.push(result);
        }
    }
    Ok(Some(Arc::new(table)))
}

/// Pre-compute a HalftoneScreen from frequency, angle, and spot function proc.
fn precompute_halftone_screen(
    ctx: &mut Context,
    freq: f64,
    angle: f64,
    proc: PsObject,
) -> Result<HalftoneScreen, PsError> {
    // Empty proc → no precomputation needed
    if let PsValue::Array { len, .. } | PsValue::PackedArray { len, .. } = proc.value
        && len == 0
    {
        return Ok(HalftoneScreen {
            frequency: freq,
            angle,
            type4_tokens: None,
            sampled_2d: None,
        });
    }

    // Try Type 4 decompilation first
    let type4_tokens = decompile_spot_to_type4(ctx, proc).map(Arc::new);

    // Fall back to 2D sampling if decompilation failed
    let sampled_2d = if type4_tokens.is_none() {
        sample_spot_function_2d(ctx, proc)?
    } else {
        None
    };

    Ok(HalftoneScreen {
        frequency: freq,
        angle,
        type4_tokens,
        sampled_2d,
    })
}

// ---------- Halftone screen operators ----------

/// `setscreen`: freq angle proc → —
pub fn op_setscreen(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 3 {
        return Err(PsError::StackUnderflow);
    }
    // Validate types: freq=num, angle=num, proc=proc/dict
    let proc_obj = ctx.o_stack.peek(0)?;
    match proc_obj.value {
        PsValue::Array { .. } | PsValue::PackedArray { .. } | PsValue::Dict(_) => {}
        _ => return Err(PsError::TypeCheck),
    }
    let angle = ctx.o_stack.peek(1)?.as_f64().ok_or(PsError::TypeCheck)?;
    let freq = ctx.o_stack.peek(2)?.as_f64().ok_or(PsError::TypeCheck)?;
    let proc_obj = ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.o_stack.pop()?;
    ctx.gstate.screen_freq = freq;
    ctx.gstate.screen_angle = angle;
    ctx.gstate.screen_proc = Some(proc_obj);
    // setscreen supersedes any halftone dictionary
    ctx.gstate.halftone = None;
    // Pre-compute halftone for PDF output
    let screen = precompute_halftone_screen(ctx, freq, angle, proc_obj)?;
    ctx.gstate.precomputed_halftone = Some(Arc::new(screen));
    ctx.gstate.precomputed_color_halftone = None;
    Ok(())
}

/// `currentscreen`: — → freq angle proc
pub fn op_currentscreen(ctx: &mut Context) -> Result<(), PsError> {
    // If a halftone dict was set, return its Frequency/Angle/dict
    if let Some(ht_obj) = ctx.gstate.halftone
        && let PsValue::Dict(entity) = ht_obj.value
    {
        let freq_key = DictKey::Name(ctx.names.intern(b"Frequency"));
        let angle_key = DictKey::Name(ctx.names.intern(b"Angle"));
        let freq = ctx
            .dicts
            .get(entity, &freq_key)
            .and_then(|o| o.as_f64())
            .unwrap_or(ctx.gstate.screen_freq);
        let angle = ctx
            .dicts
            .get(entity, &angle_key)
            .and_then(|o| o.as_f64())
            .unwrap_or(ctx.gstate.screen_angle);
        ctx.o_stack.push(PsObject::real(freq))?;
        ctx.o_stack.push(PsObject::real(angle))?;
        ctx.o_stack.push(ht_obj)?;
        return Ok(());
    }
    ctx.o_stack.push(PsObject::real(ctx.gstate.screen_freq))?;
    ctx.o_stack.push(PsObject::real(ctx.gstate.screen_angle))?;
    match ctx.gstate.screen_proc {
        Some(proc_obj) => ctx.o_stack.push(proc_obj)?,
        None => {
            let entity = ctx.arrays.allocate_from(&[]);
            ctx.o_stack.push(PsObject::procedure(entity, 0))?;
        }
    }
    Ok(())
}

/// `setcolorscreen`: freq1 angle1 proc1 ... freq4 angle4 proc4 → —
///
/// Order: red(bottom) green blue gray(top)
pub fn op_setcolorscreen(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 12 {
        return Err(PsError::StackUnderflow);
    }
    // Validate all 12 operands before popping: 4 × (freq, angle, proc)
    // Stack top-to-bottom: gray_proc gray_angle gray_freq ... red_proc red_angle red_freq
    for i in 0..4 {
        let base = i * 3;
        match ctx.o_stack.peek(base)?.value {
            PsValue::Array { .. } | PsValue::PackedArray { .. } | PsValue::Dict(_) => {}
            _ => return Err(PsError::TypeCheck),
        }
        ctx.o_stack
            .peek(base + 1)?
            .as_f64()
            .ok_or(PsError::TypeCheck)?;
        ctx.o_stack
            .peek(base + 2)?
            .as_f64()
            .ok_or(PsError::TypeCheck)?;
    }
    // Now pop: gray(top), blue, green, red(bottom)
    let mut components: [(f64, f64, PsObject); 4] = [(0.0, 0.0, PsObject::null()); 4];
    for i in (0..4).rev() {
        let proc_obj = ctx.o_stack.pop()?;
        let angle = ctx.o_stack.pop()?.as_f64().unwrap();
        let freq = ctx.o_stack.pop()?.as_f64().unwrap();
        components[i] = (freq, angle, proc_obj);
    }
    // Gray component also becomes the screen
    ctx.gstate.screen_freq = components[3].0;
    ctx.gstate.screen_angle = components[3].1;
    ctx.gstate.screen_proc = Some(components[3].2);
    ctx.gstate.color_screen = Some(components);
    // setcolorscreen supersedes any halftone dictionary
    ctx.gstate.halftone = None;
    // Pre-compute per-component halftones for PDF output
    let red = Arc::new(precompute_halftone_screen(
        ctx,
        components[0].0,
        components[0].1,
        components[0].2,
    )?);
    let green = Arc::new(precompute_halftone_screen(
        ctx,
        components[1].0,
        components[1].1,
        components[1].2,
    )?);
    let blue = Arc::new(precompute_halftone_screen(
        ctx,
        components[2].0,
        components[2].1,
        components[2].2,
    )?);
    let gray = Arc::new(precompute_halftone_screen(
        ctx,
        components[3].0,
        components[3].1,
        components[3].2,
    )?);
    ctx.gstate.precomputed_halftone = Some(gray.clone());
    ctx.gstate.precomputed_color_halftone = Some([Some(red), Some(green), Some(blue), Some(gray)]);
    Ok(())
}

/// `currentcolorscreen`: — → freq1 angle1 proc1 ... freq4 angle4 proc4
///
/// Returns: red(bottom) green blue gray(top)
pub fn op_currentcolorscreen(ctx: &mut Context) -> Result<(), PsError> {
    if let Some(cs) = ctx.gstate.color_screen {
        for (freq, angle, proc_obj) in &cs {
            ctx.o_stack.push(PsObject::real(*freq))?;
            ctx.o_stack.push(PsObject::real(*angle))?;
            ctx.o_stack.push(*proc_obj)?;
        }
    } else {
        // Default: all 4 components use current screen params
        let proc_obj = match ctx.gstate.screen_proc {
            Some(p) => p,
            None => {
                let entity = ctx.arrays.allocate_from(&[]);
                PsObject::procedure(entity, 0)
            }
        };
        for _ in 0..4 {
            ctx.o_stack.push(PsObject::real(ctx.gstate.screen_freq))?;
            ctx.o_stack.push(PsObject::real(ctx.gstate.screen_angle))?;
            ctx.o_stack.push(proc_obj)?;
        }
    }
    Ok(())
}

// ---------- Transfer function operators ----------

/// Sample a PS transfer procedure at 256 points via exec_sync.
/// Returns None if the procedure is identity (or empty).
fn sample_transfer(ctx: &mut Context, proc: PsObject) -> Result<Option<Arc<Vec<f64>>>, PsError> {
    // Empty procedure = identity
    if let PsValue::Array { len, .. } | PsValue::PackedArray { len, .. } = proc.value
        && len == 0
    {
        return Ok(None);
    }
    let mut table = Vec::with_capacity(256);
    for i in 0..256 {
        let input = i as f64 / 255.0;
        ctx.o_stack.push(PsObject::real(input))?;
        ctx.exec_sync(proc)?;
        let result = if !ctx.o_stack.is_empty() {
            ctx.o_stack.pop()?.as_f64().unwrap_or(input).clamp(0.0, 1.0)
        } else {
            input
        };
        table.push(result);
    }
    // Identity check
    let is_identity = table
        .iter()
        .enumerate()
        .all(|(i, &v)| (v - i as f64 / 255.0).abs() < 1e-6);
    if is_identity {
        Ok(None)
    } else {
        Ok(Some(Arc::new(table)))
    }
}

/// `settransfer`: proc → —
pub fn op_settransfer(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    if !obj.is_array_type() || !obj.flags.is_executable() {
        return Err(PsError::TypeCheck);
    }
    let proc_obj = ctx.o_stack.pop()?;
    ctx.gstate.transfer_function = Some(proc_obj);
    ctx.gstate.sampled_transfer = sample_transfer(ctx, proc_obj)?;
    ctx.gstate.sampled_color_transfer = None; // settransfer clears color transfer
    Ok(())
}

/// `currenttransfer`: — → proc
pub fn op_currenttransfer(ctx: &mut Context) -> Result<(), PsError> {
    match ctx.gstate.transfer_function {
        Some(proc_obj) => ctx.o_stack.push(proc_obj)?,
        None => {
            let entity = ctx.arrays.allocate_from(&[]);
            ctx.o_stack.push(PsObject::procedure(entity, 0))?;
        }
    }
    Ok(())
}

/// `setcolortransfer`: redproc greenproc blueproc grayproc → —
pub fn op_setcolortransfer(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 4 {
        return Err(PsError::StackUnderflow);
    }
    // Validate all 4 are procedures before popping
    for i in 0..4 {
        match ctx.o_stack.peek(i)?.value {
            PsValue::Array { .. } | PsValue::PackedArray { .. } => {}
            _ => return Err(PsError::TypeCheck),
        }
    }
    let gray = ctx.o_stack.pop()?;
    let blue = ctx.o_stack.pop()?;
    let green = ctx.o_stack.pop()?;
    let red = ctx.o_stack.pop()?;
    ctx.gstate.color_transfer = Some([red, green, blue, gray]);
    // Gray component also becomes the transfer function
    ctx.gstate.transfer_function = Some(gray);
    // Sample all 4 transfer functions
    ctx.gstate.sampled_color_transfer = Some([
        sample_transfer(ctx, red)?,
        sample_transfer(ctx, green)?,
        sample_transfer(ctx, blue)?,
        sample_transfer(ctx, gray)?,
    ]);
    ctx.gstate.sampled_transfer = sample_transfer(ctx, gray)?;
    Ok(())
}

/// `currentcolortransfer`: — → redproc greenproc blueproc grayproc
pub fn op_currentcolortransfer(ctx: &mut Context) -> Result<(), PsError> {
    if let Some(ct) = ctx.gstate.color_transfer {
        for proc_obj in &ct {
            ctx.o_stack.push(*proc_obj)?;
        }
    } else {
        // Default: all 4 components use current transfer function
        let proc_obj = match ctx.gstate.transfer_function {
            Some(p) => p,
            None => {
                let entity = ctx.arrays.allocate_from(&[]);
                PsObject::procedure(entity, 0)
            }
        };
        for _ in 0..4 {
            ctx.o_stack.push(proc_obj)?;
        }
    }
    Ok(())
}

// ---------- Black generation / undercolor removal ----------

/// Sample a PS UCR procedure at 256 points via exec_sync.
/// Like sample_transfer() but range is [-1,1] instead of [0,1].
fn sample_ucr(ctx: &mut Context, proc: PsObject) -> Result<Option<Arc<Vec<f64>>>, PsError> {
    if let PsValue::Array { len, .. } | PsValue::PackedArray { len, .. } = proc.value
        && len == 0
    {
        return Ok(None);
    }
    let mut table = Vec::with_capacity(256);
    for i in 0..256 {
        let input = i as f64 / 255.0;
        ctx.o_stack.push(PsObject::real(input))?;
        ctx.exec_sync(proc)?;
        let result = if !ctx.o_stack.is_empty() {
            ctx.o_stack.pop()?.as_f64().unwrap_or(0.0).clamp(-1.0, 1.0)
        } else {
            0.0
        };
        table.push(result);
    }
    // Identity check (UCR identity = all zeros)
    let is_identity = table.iter().all(|&v| v.abs() < 1e-6);
    if is_identity {
        Ok(None)
    } else {
        Ok(Some(Arc::new(table)))
    }
}

/// `setblackgeneration`: proc → —
pub fn op_setblackgeneration(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    if !obj.is_array_type() || !obj.flags.is_executable() {
        return Err(PsError::TypeCheck);
    }
    let proc_obj = ctx.o_stack.pop()?;
    ctx.gstate.black_generation = Some(proc_obj);
    ctx.gstate.sampled_black_generation = sample_transfer(ctx, proc_obj)?;
    Ok(())
}

/// `currentblackgeneration`: — → proc
pub fn op_currentblackgeneration(ctx: &mut Context) -> Result<(), PsError> {
    match ctx.gstate.black_generation {
        Some(proc_obj) => ctx.o_stack.push(proc_obj)?,
        None => {
            let entity = ctx.arrays.allocate_from(&[]);
            ctx.o_stack.push(PsObject::procedure(entity, 0))?;
        }
    }
    Ok(())
}

/// `setundercolorremoval`: proc → —
pub fn op_setundercolorremoval(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    if !obj.is_array_type() || !obj.flags.is_executable() {
        return Err(PsError::TypeCheck);
    }
    let proc_obj = ctx.o_stack.pop()?;
    ctx.gstate.undercolor_removal = Some(proc_obj);
    ctx.gstate.sampled_ucr = sample_ucr(ctx, proc_obj)?;
    Ok(())
}

/// `currentundercolorremoval`: — → proc
pub fn op_currentundercolorremoval(ctx: &mut Context) -> Result<(), PsError> {
    match ctx.gstate.undercolor_removal {
        Some(proc_obj) => ctx.o_stack.push(proc_obj)?,
        None => {
            let entity = ctx.arrays.allocate_from(&[]);
            ctx.o_stack.push(PsObject::procedure(entity, 0))?;
        }
    }
    Ok(())
}

// ---------- Halftone dictionary operators ----------

/// `sethalftone`: dict → —
pub fn op_sethalftone(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    match obj.value {
        PsValue::Dict(entity) => {
            // Extract Frequency/Angle from dict to update screen params
            let freq_key = DictKey::Name(ctx.names.intern(b"Frequency"));
            let angle_key = DictKey::Name(ctx.names.intern(b"Angle"));
            if let Some(freq_obj) = ctx.dicts.get(entity, &freq_key)
                && let Some(f) = freq_obj.as_f64()
            {
                ctx.gstate.screen_freq = f;
            }
            if let Some(angle_obj) = ctx.dicts.get(entity, &angle_key)
                && let Some(a) = angle_obj.as_f64()
            {
                ctx.gstate.screen_angle = a;
            }
            // Pre-compute halftone for Type 1 dicts with SpotFunction
            let ht_type_key = DictKey::Name(ctx.names.intern(b"HalftoneType"));
            let spot_key = DictKey::Name(ctx.names.intern(b"SpotFunction"));
            let ht_type = ctx.dicts.get(entity, &ht_type_key).and_then(|o| o.as_i32());
            if ht_type == Some(1) {
                if let (Some(freq), Some(angle), Some(spot_proc)) = (
                    ctx.dicts.get(entity, &freq_key).and_then(|o| o.as_f64()),
                    ctx.dicts.get(entity, &angle_key).and_then(|o| o.as_f64()),
                    ctx.dicts.get(entity, &spot_key),
                ) {
                    let screen = precompute_halftone_screen(ctx, freq, angle, spot_proc)?;
                    ctx.gstate.precomputed_halftone = Some(Arc::new(screen));
                    ctx.gstate.precomputed_color_halftone = None;
                }
            } else {
                // Non-Type 1 halftones: suppress (leave as None)
                ctx.gstate.precomputed_halftone = None;
                ctx.gstate.precomputed_color_halftone = None;
            }
            let obj = ctx.o_stack.pop()?;
            ctx.gstate.halftone = Some(obj);
            Ok(())
        }
        _ => Err(PsError::TypeCheck),
    }
}

/// `currenthalftone`: — → dict
///
/// Returns the halftone dictionary set by `sethalftone`, or a default
/// Type 1 halftone matching `currentscreen`.
pub fn op_currenthalftone(ctx: &mut Context) -> Result<(), PsError> {
    if let Some(ht_obj) = ctx.gstate.halftone {
        ctx.o_stack.push(ht_obj)?;
    } else {
        let entity = crate::vm_ops::alloc_dict(ctx, 5, b"halftone");
        ctx.dicts.put(
            entity,
            DictKey::Name(ctx.names.intern(b"HalftoneType")),
            PsObject::int(1),
        );
        ctx.dicts.put(
            entity,
            DictKey::Name(ctx.names.intern(b"Frequency")),
            PsObject::real(ctx.gstate.screen_freq),
        );
        ctx.dicts.put(
            entity,
            DictKey::Name(ctx.names.intern(b"Angle")),
            PsObject::real(ctx.gstate.screen_angle),
        );
        let proc_obj = match ctx.gstate.screen_proc {
            Some(p) => p,
            None => {
                let proc_entity = ctx.arrays.allocate_from(&[]);
                PsObject::procedure(proc_entity, 0)
            }
        };
        ctx.dicts.put(
            entity,
            DictKey::Name(ctx.names.intern(b"SpotFunction")),
            proc_obj,
        );
        ctx.o_stack.push(PsObject::dict(entity))?;
    }
    Ok(())
}

// ---------- Color rendering ----------

/// `setcolorrendering`: dict → —
pub fn op_setcolorrendering(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    match obj.value {
        PsValue::Dict(_) => {
            let obj = ctx.o_stack.pop()?;
            ctx.gstate.color_rendering = Some(obj);
            Ok(())
        }
        _ => Err(PsError::TypeCheck),
    }
}

/// `currentcolorrendering`: — → dict
pub fn op_currentcolorrendering(ctx: &mut Context) -> Result<(), PsError> {
    match ctx.gstate.color_rendering {
        Some(obj) => ctx.o_stack.push(obj)?,
        None => {
            let entity = crate::vm_ops::alloc_dict(ctx, 1, b"colorrendering");
            ctx.o_stack.push(PsObject::dict(entity))?;
        }
    }
    Ok(())
}

// ---------- Rendering Intent ----------

/// Intent name → u8 encoding.
fn intent_from_name(name: &[u8]) -> Option<u8> {
    match name {
        b"RelativeColorimetric" => Some(0),
        b"AbsoluteColorimetric" => Some(1),
        b"Perceptual" => Some(2),
        b"Saturation" => Some(3),
        _ => None,
    }
}

/// u8 encoding → intent name bytes.
fn intent_name(intent: u8) -> &'static [u8] {
    match intent {
        0 => b"RelativeColorimetric",
        1 => b"AbsoluteColorimetric",
        2 => b"Perceptual",
        3 => b"Saturation",
        _ => b"RelativeColorimetric",
    }
}

/// `setrenderingintent`: name → —
pub fn op_setrenderingintent(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let name_bytes = match ctx.o_stack.peek(0)?.value {
        PsValue::Name(id) => ctx.names.get_bytes(id).to_vec(),
        _ => return Err(PsError::TypeCheck),
    };
    let intent = intent_from_name(&name_bytes).ok_or(PsError::RangeCheck)?;
    ctx.o_stack.pop()?;
    ctx.gstate.rendering_intent = intent;
    Ok(())
}

/// `currentrenderingintent`: — → name
pub fn op_currentrenderingintent(ctx: &mut Context) -> Result<(), PsError> {
    let name_id = ctx.names.intern(intent_name(ctx.gstate.rendering_intent));
    ctx.o_stack.push(PsObject::name_lit(name_id))?;
    Ok(())
}

// ---------- Smoothness ----------

/// `setsmoothness`: num → —
pub fn op_setsmoothness(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let val = ctx.o_stack.peek(0)?.as_f64().ok_or(PsError::TypeCheck)?;
    ctx.o_stack.pop()?;
    // Clamp to [0.0, 1.0]
    ctx.gstate.smoothness = val.clamp(0.0, 1.0);
    Ok(())
}

/// `currentsmoothness`: — → num
pub fn op_currentsmoothness(ctx: &mut Context) -> Result<(), PsError> {
    ctx.o_stack.push(PsObject::real(ctx.gstate.smoothness))?;
    Ok(())
}

// ---------- Trapping stubs (Level 3) ----------

/// `settrapparams`: dict → —
pub fn op_settrapparams(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    match ctx.o_stack.peek(0)?.value {
        PsValue::Dict(_) => {
            ctx.o_stack.pop()?;
            Ok(())
        }
        _ => Err(PsError::TypeCheck),
    }
}

/// `currenttrapparams`: — → dict
pub fn op_currenttrapparams(ctx: &mut Context) -> Result<(), PsError> {
    let entity = crate::vm_ops::alloc_dict(ctx, 1, b"trapparams");
    ctx.o_stack.push(PsObject::dict(entity))?;
    Ok(())
}

/// `settrapzone`: — → —
pub fn op_settrapzone(ctx: &mut Context) -> Result<(), PsError> {
    let _ = ctx;
    Ok(())
}

// ---------- Pattern stubs ----------

/// Helper: extract 6-element f64 array from a PS array object.
fn extract_matrix(ctx: &Context, obj: &PsObject) -> Result<Matrix, PsError> {
    match obj.value {
        PsValue::Array { entity, start, len } | PsValue::PackedArray { entity, start, len } => {
            if len != 6 {
                return Err(PsError::TypeCheck);
            }
            let mut vals = [0.0f64; 6];
            for (i, val) in vals.iter_mut().enumerate() {
                *val = ctx
                    .arrays
                    .get_element(entity, start + i as u32)
                    .as_f64()
                    .ok_or(PsError::TypeCheck)?;
            }
            Ok(Matrix::new(
                vals[0], vals[1], vals[2], vals[3], vals[4], vals[5],
            ))
        }
        _ => Err(PsError::TypeCheck),
    }
}

/// Helper: extract a 4-element f64 array from a PS array (BBox).
fn extract_bbox(ctx: &Context, obj: &PsObject) -> Result<[f64; 4], PsError> {
    match obj.value {
        PsValue::Array { entity, start, len } | PsValue::PackedArray { entity, start, len } => {
            if len != 4 {
                return Err(PsError::RangeCheck);
            }
            let mut vals = [0.0f64; 4];
            for (i, val) in vals.iter_mut().enumerate() {
                *val = ctx
                    .arrays
                    .get_element(entity, start + i as u32)
                    .as_f64()
                    .ok_or(PsError::TypeCheck)?;
            }
            Ok(vals)
        }
        _ => Err(PsError::TypeCheck),
    }
}

/// `makepattern`: dict matrix → dict
///
/// Instantiates a pattern: copies the dict, computes the pattern matrix,
/// executes PaintProc to capture a display list, and stores the result.
pub fn op_makepattern(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }

    // Validate matrix (top of stack)
    let matrix_obj = ctx.o_stack.peek(0)?;
    let matrix = extract_matrix(ctx, &matrix_obj)?;

    // Validate dict (second on stack)
    let dict_obj = ctx.o_stack.peek(1)?;
    let dict_entity = match dict_obj.value {
        PsValue::Dict(entity) => entity,
        _ => return Err(PsError::TypeCheck),
    };

    // Validate PatternType
    let pt_key = DictKey::Name(ctx.names.intern(b"PatternType"));
    let pattern_type = ctx
        .dicts
        .get(dict_entity, &pt_key)
        .and_then(|o| o.as_i32())
        .ok_or(PsError::Undefined)?;
    if pattern_type != 1 && pattern_type != 2 {
        return Err(PsError::RangeCheck);
    }

    // For Type 1, validate required keys
    let mut paint_type = 1;
    let mut tiling_type = 1;
    let mut bbox = [0.0f64; 4];
    let mut xstep = 0.0f64;
    let mut ystep = 0.0f64;
    let mut paint_proc = None;

    if pattern_type == 1 {
        let paint_type_key = DictKey::Name(ctx.names.intern(b"PaintType"));
        paint_type = ctx
            .dicts
            .get(dict_entity, &paint_type_key)
            .and_then(|o| o.as_i32())
            .ok_or(PsError::Undefined)?;
        if paint_type != 1 && paint_type != 2 {
            return Err(PsError::RangeCheck);
        }

        let tt_key = DictKey::Name(ctx.names.intern(b"TilingType"));
        tiling_type = ctx
            .dicts
            .get(dict_entity, &tt_key)
            .and_then(|o| o.as_i32())
            .ok_or(PsError::Undefined)?;
        if !(1..=3).contains(&tiling_type) {
            return Err(PsError::RangeCheck);
        }

        let bbox_key = DictKey::Name(ctx.names.intern(b"BBox"));
        let bbox_obj = ctx
            .dicts
            .get(dict_entity, &bbox_key)
            .ok_or(PsError::Undefined)?;
        bbox = extract_bbox(ctx, &bbox_obj)?;

        let xs_key = DictKey::Name(ctx.names.intern(b"XStep"));
        xstep = ctx
            .dicts
            .get(dict_entity, &xs_key)
            .and_then(|o| o.as_f64())
            .ok_or(PsError::Undefined)?;
        if xstep == 0.0 {
            return Err(PsError::RangeCheck);
        }

        let ys_key = DictKey::Name(ctx.names.intern(b"YStep"));
        ystep = ctx
            .dicts
            .get(dict_entity, &ys_key)
            .and_then(|o| o.as_f64())
            .ok_or(PsError::Undefined)?;
        if ystep == 0.0 {
            return Err(PsError::RangeCheck);
        }

        let pp_key = DictKey::Name(ctx.names.intern(b"PaintProc"));
        paint_proc = ctx.dicts.get(dict_entity, &pp_key);
        if paint_proc.is_none() {
            return Err(PsError::Undefined);
        }
    }

    // Pop operands
    ctx.o_stack.pop()?; // matrix
    ctx.o_stack.pop()?; // dict

    // Copy dict to new entity (local VM)
    let new_dict = crate::vm_ops::alloc_dict(ctx, 20, b"pattern");
    // Copy all entries from original dict
    let entries: Vec<(DictKey, PsObject)> = ctx
        .dicts
        .entry(dict_entity)
        .entries
        .iter()
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    for (k, v) in entries {
        ctx.dicts.put(new_dict, k, v);
    }

    // Compute pattern_matrix = matrix_arg × CTM (row-vector convention)
    let pattern_matrix = ctx.gstate.ctm.concat(&matrix);

    // Execute PaintProc to capture display list (Type 1 only)
    let cached_display_list = if pattern_type == 1 {
        let pp = paint_proc.unwrap();

        // Save display list and CTM, set CTM to identity so PaintProc
        // captures paths in pattern space (not device space)
        let saved_dl = std::mem::take(&mut ctx.display_list);
        let saved_ctm = ctx.gstate.ctm;
        ctx.gstate.ctm = Matrix::identity();

        // Clear path for PaintProc
        ctx.gstate.path.clear();
        ctx.gstate.current_point = None;

        // Push pattern dict on o_stack for PaintProc to consume
        ctx.o_stack.push(PsObject::dict(new_dict))?;

        // Execute PaintProc
        let result = ctx.exec_sync(pp);

        // Restore CTM and display list
        ctx.gstate.ctm = saved_ctm;
        let captured = std::mem::replace(&mut ctx.display_list, saved_dl);

        result?;
        captured
    } else {
        DisplayList::new()
    };

    // Build PatternData and store
    let pattern_id = ctx.pattern_store.len() as u32;
    ctx.pattern_store.push(PatternData {
        pattern_type,
        paint_type,
        tiling_type,
        bbox,
        xstep,
        ystep,
        pattern_matrix,
        cached_display_list,
    });

    // Store Implementation in the copied dict
    let impl_key = DictKey::Name(ctx.names.intern(b"Implementation"));
    ctx.dicts
        .put(new_dict, impl_key, PsObject::int(pattern_id as i32));

    // Push result dict
    ctx.o_stack.push(PsObject::dict(new_dict))?;
    Ok(())
}

/// `setpattern`: pattern → — (colored) or comp... pattern → — (uncolored)
///
/// Sets the current pattern for subsequent fill/stroke operations.
pub fn op_setpattern(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }

    let dict_obj = ctx.o_stack.peek(0)?;
    let dict_entity = match dict_obj.value {
        PsValue::Dict(entity) => entity,
        _ => return Err(PsError::TypeCheck),
    };

    // Must have /Implementation (proof of makepattern)
    let impl_key = DictKey::Name(ctx.names.intern(b"Implementation"));
    let impl_obj = ctx
        .dicts
        .get(dict_entity, &impl_key)
        .ok_or(PsError::TypeCheck)?;
    let pattern_id = impl_obj.as_i32().ok_or(PsError::TypeCheck)? as u32;

    // Get paint type
    let pt_key = DictKey::Name(ctx.names.intern(b"PaintType"));
    let paint_type = ctx
        .dicts
        .get(dict_entity, &pt_key)
        .and_then(|o| o.as_i32())
        .unwrap_or(1);

    ctx.o_stack.pop()?; // dict

    if paint_type == 2 {
        // Uncolored pattern: pop underlying color components
        // For now, pop one component (gray) as a simple case
        if ctx.o_stack.is_empty() {
            return Err(PsError::StackUnderflow);
        }
        let color_val = ctx.o_stack.peek(0)?.as_f64().ok_or(PsError::TypeCheck)?;
        ctx.o_stack.pop()?;
        ctx.gstate.pattern_underlying_color =
            Some(stet_graphics::color::DeviceColor::from_gray(color_val));
    }

    ctx.gstate.current_pattern = Some(pattern_id);
    Ok(())
}

/// Transform cached form-space display list elements through a CTM and append
/// to the target display list. Cached elements have identity-CTM coordinates
/// (form space); path points are transformed directly, while elements with
/// their own CTM (Stroke, Image) get their CTM composed with the real CTM.
fn replay_form_elements(
    cached: &stet_graphics::display_list::DisplayList,
    ctm: &Matrix,
    target: &mut stet_graphics::display_list::DisplayList,
) {
    use stet_fonts::geometry::PathSegment;
    use stet_graphics::display_list::DisplayElement;

    let transform_path = |path: &stet_fonts::geometry::PsPath| -> stet_fonts::geometry::PsPath {
        let mut result = stet_fonts::geometry::PsPath::new();
        for seg in &path.segments {
            match seg {
                PathSegment::MoveTo(x, y) => {
                    let (nx, ny) = ctm.transform_point(*x, *y);
                    result.segments.push(PathSegment::MoveTo(nx, ny));
                }
                PathSegment::LineTo(x, y) => {
                    let (nx, ny) = ctm.transform_point(*x, *y);
                    result.segments.push(PathSegment::LineTo(nx, ny));
                }
                PathSegment::CurveTo {
                    x1,
                    y1,
                    x2,
                    y2,
                    x3,
                    y3,
                } => {
                    let (nx1, ny1) = ctm.transform_point(*x1, *y1);
                    let (nx2, ny2) = ctm.transform_point(*x2, *y2);
                    let (nx3, ny3) = ctm.transform_point(*x3, *y3);
                    result.segments.push(PathSegment::CurveTo {
                        x1: nx1,
                        y1: ny1,
                        x2: nx2,
                        y2: ny2,
                        x3: nx3,
                        y3: ny3,
                    });
                }
                PathSegment::ClosePath => {
                    result.segments.push(PathSegment::ClosePath);
                }
            }
        }
        result
    };

    for elem in cached.elements() {
        match elem {
            DisplayElement::Fill { path, params } => {
                let new_path = transform_path(path);
                // Path points already transformed to device space; use identity CTM
                let new_params = stet_graphics::device::FillParams {
                    color: params.color.clone(),
                    fill_rule: params.fill_rule,
                    ctm: Matrix::identity(),
                    is_text_glyph: params.is_text_glyph,
                    overprint: params.overprint,
                    overprint_mode: params.overprint_mode,
                    painted_channels: params.painted_channels,
                    is_device_cmyk: params.is_device_cmyk,
                    spot_color: params.spot_color.clone(),
                    rendering_intent: params.rendering_intent,
                    transfer: params.transfer.clone(),
                    halftone: params.halftone.clone(),
                    bg_ucr: params.bg_ucr.clone(),
                    alpha: params.alpha,
                    blend_mode: params.blend_mode,
                };
                target.push(DisplayElement::Fill {
                    path: new_path,
                    params: new_params,
                });
            }
            DisplayElement::Stroke { path, params } => {
                let new_path = transform_path(path);
                // Scale line width by CTM scale factor (form space → device space)
                let scale = (ctm.a * ctm.a + ctm.b * ctm.b).sqrt();
                let new_params = stet_graphics::device::StrokeParams {
                    color: params.color.clone(),
                    line_width: params.line_width * scale,
                    line_cap: params.line_cap,
                    line_join: params.line_join,
                    miter_limit: params.miter_limit,
                    dash_pattern: params.dash_pattern.clone(),
                    ctm: Matrix::identity(),
                    stroke_adjust: params.stroke_adjust,
                    is_text_glyph: params.is_text_glyph,
                    overprint: params.overprint,
                    overprint_mode: params.overprint_mode,
                    painted_channels: params.painted_channels,
                    is_device_cmyk: params.is_device_cmyk,
                    spot_color: params.spot_color.clone(),
                    rendering_intent: params.rendering_intent,
                    transfer: params.transfer.clone(),
                    halftone: params.halftone.clone(),
                    bg_ucr: params.bg_ucr.clone(),
                    alpha: params.alpha,
                    blend_mode: params.blend_mode,
                };
                target.push(DisplayElement::Stroke {
                    path: new_path,
                    params: new_params,
                });
            }
            DisplayElement::Clip { path, params } => {
                let new_path = transform_path(path);
                let new_params = stet_graphics::device::ClipParams {
                    fill_rule: params.fill_rule,
                    ctm: Matrix::identity(),
                    stroke_params: None,
                };
                target.push(DisplayElement::Clip {
                    path: new_path,
                    params: new_params,
                });
            }
            DisplayElement::Image {
                sample_data,
                params,
            } => {
                let mut new_params = params.clone();
                new_params.ctm = ctm.concat(&params.ctm);
                target.push(DisplayElement::Image {
                    sample_data: sample_data.clone(),
                    params: new_params,
                });
            }
            // Skip ErasePage/InitClip — shouldn't appear in form PaintProc
            DisplayElement::ErasePage | DisplayElement::InitClip => {}
            // For shading elements, compose CTM
            DisplayElement::AxialShading { params } => {
                let mut new_params = params.clone();
                new_params.ctm = ctm.concat(&params.ctm);
                target.push(DisplayElement::AxialShading { params: new_params });
            }
            DisplayElement::RadialShading { params } => {
                let mut new_params = params.clone();
                new_params.ctm = ctm.concat(&params.ctm);
                target.push(DisplayElement::RadialShading { params: new_params });
            }
            DisplayElement::MeshShading { params } => {
                let mut new_params = params.clone();
                new_params.ctm = ctm.concat(&params.ctm);
                target.push(DisplayElement::MeshShading { params: new_params });
            }
            DisplayElement::PatchShading { params } => {
                let mut new_params = params.clone();
                new_params.ctm = ctm.concat(&params.ctm);
                target.push(DisplayElement::PatchShading { params: new_params });
            }
            DisplayElement::PatternFill { params } => {
                let new_path = transform_path(&params.path);
                let mut new_params = params.clone();
                new_params.path = new_path;
                target.push(DisplayElement::PatternFill { params: new_params });
            }
            DisplayElement::Text { params } => {
                // Transform text from form space to device space:
                // - Position: transform through real CTM
                // - CTM: compose form-space CTM with real CTM
                let mut new_params = params.clone();
                let (dev_x, dev_y) = ctm.transform_point(params.start_x, params.start_y);
                new_params.start_x = dev_x;
                new_params.start_y = dev_y;
                // Compose the stored CTM (identity during form capture) with real CTM
                let form_ctm = Matrix {
                    a: params.ctm[0],
                    b: params.ctm[1],
                    c: params.ctm[2],
                    d: params.ctm[3],
                    tx: params.ctm[4],
                    ty: params.ctm[5],
                };
                let composed = ctm.concat(&form_ctm);
                new_params.ctm = [
                    composed.a,
                    composed.b,
                    composed.c,
                    composed.d,
                    composed.tx,
                    composed.ty,
                ];
                target.push(DisplayElement::Text { params: new_params });
            }
            DisplayElement::Group { .. }
            | DisplayElement::SoftMasked { .. }
            | DisplayElement::OcgGroup { .. } => {
                // Groups/SoftMasked/OcgGroup are PDF-only; PS display lists don't contain them
            }
        }
    }
}

/// `execform`: dict → —
///
/// Execute a Form XObject. Per PLRM: gsave, concat form Matrix, clip to BBox,
/// execute PaintProc (caching in form space on first call), replay cached
/// elements transformed through current CTM, grestore.
pub fn op_execform(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }

    let dict_obj = ctx.o_stack.peek(0)?;
    let dict_entity = match dict_obj.value {
        PsValue::Dict(entity) => entity,
        _ => return Err(PsError::TypeCheck),
    };

    // Check if already has Implementation (cached)
    let impl_key = DictKey::Name(ctx.names.intern(b"Implementation"));
    let first_invocation = ctx.dicts.get(dict_entity, &impl_key).is_none();

    if first_invocation {
        // Validate FormType
        let ft_key = DictKey::Name(ctx.names.intern(b"FormType"));
        let form_type = ctx
            .dicts
            .get(dict_entity, &ft_key)
            .and_then(|o| o.as_i32())
            .ok_or(PsError::TypeCheck)?;
        if form_type != 1 {
            return Err(PsError::RangeCheck);
        }

        // Validate BBox, Matrix, PaintProc exist
        let bbox_key = DictKey::Name(ctx.names.intern(b"BBox"));
        if ctx.dicts.get(dict_entity, &bbox_key).is_none() {
            return Err(PsError::TypeCheck);
        }
        let matrix_key = DictKey::Name(ctx.names.intern(b"Matrix"));
        if ctx.dicts.get(dict_entity, &matrix_key).is_none() {
            return Err(PsError::TypeCheck);
        }
        let pp_key = DictKey::Name(ctx.names.intern(b"PaintProc"));
        if ctx.dicts.get(dict_entity, &pp_key).is_none() {
            return Err(PsError::TypeCheck);
        }
    }

    ctx.o_stack.pop()?; // dict

    // 1. gsave
    crate::graphics_state_ops::op_gsave(ctx)?;

    // 2. Concat form's Matrix with CTM
    let matrix_key = DictKey::Name(ctx.names.intern(b"Matrix"));
    let matrix_obj = ctx
        .dicts
        .get(dict_entity, &matrix_key)
        .ok_or(PsError::TypeCheck)?;
    ctx.o_stack.push(matrix_obj)?;
    crate::matrix_ops::op_concat(ctx)?;

    // 3. Clip to BBox via rectclip
    let bbox_key = DictKey::Name(ctx.names.intern(b"BBox"));
    let bbox_obj = ctx
        .dicts
        .get(dict_entity, &bbox_key)
        .ok_or(PsError::TypeCheck)?;
    let bbox = extract_bbox(ctx, &bbox_obj)?;
    ctx.o_stack.push(PsObject::real(bbox[0]))?;
    ctx.o_stack.push(PsObject::real(bbox[1]))?;
    ctx.o_stack.push(PsObject::real(bbox[2] - bbox[0]))?;
    ctx.o_stack.push(PsObject::real(bbox[3] - bbox[1]))?;
    crate::clip_ops::op_rectclip(ctx)?;

    // Save the real CTM (after concat + rectclip) for replay
    let real_ctm = ctx.gstate.ctm;

    // 4. Cache PaintProc output if first invocation
    if first_invocation {
        // Set CTM to identity so display list is in form space
        ctx.gstate.ctm = Matrix::identity();

        // Save display list, execute PaintProc
        let saved_dl = std::mem::take(&mut ctx.display_list);

        // Push form dict for PaintProc to consume
        ctx.o_stack.push(PsObject::dict(dict_entity))?;

        let pp_key = DictKey::Name(ctx.names.intern(b"PaintProc"));
        let paint_proc = ctx
            .dicts
            .get(dict_entity, &pp_key)
            .ok_or(PsError::TypeCheck)?;

        let result = ctx.exec_sync(paint_proc);

        let captured = std::mem::replace(&mut ctx.display_list, saved_dl);
        result?;

        ctx.form_cache.insert(dict_entity, captured);

        // Restore real CTM for replay
        ctx.gstate.ctm = real_ctm;

        // Mark as cached
        ctx.cow_check_dict(dict_entity);
        let impl_key = DictKey::Name(ctx.names.intern(b"Implementation"));
        ctx.dicts.put(dict_entity, impl_key, PsObject::bool(true));
    }

    // 5. Replay cached elements transformed through real CTM
    if let Some(cached) = ctx.form_cache.get(&dict_entity) {
        // Clone to avoid borrow conflict (cached borrows ctx.form_cache)
        let cached_clone = cached.clone();
        replay_form_elements(&cached_clone, &real_ctm, &mut ctx.display_list);
    }

    // 6. grestore
    crate::graphics_state_ops::op_grestore(ctx)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use stet_core::context::Context;
    use stet_core::object::PsObject;

    fn setup() -> Context {
        let mut ctx = Context::new();
        crate::build_system_dict(&mut ctx);
        ctx
    }

    #[test]
    fn test_setscreen_stores_values() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(120.0)).unwrap();
        ctx.o_stack.push(PsObject::real(30.0)).unwrap();
        let e = ctx.arrays.allocate_from(&[]);
        ctx.o_stack.push(PsObject::procedure(e, 0)).unwrap();
        op_setscreen(&mut ctx).unwrap();
        assert!(ctx.o_stack.is_empty());
        assert!((ctx.gstate.screen_freq - 120.0).abs() < 1e-10);
        assert!((ctx.gstate.screen_angle - 30.0).abs() < 1e-10);
    }

    #[test]
    fn test_currentscreen_returns_stored() {
        let mut ctx = setup();
        ctx.gstate.screen_freq = 90.0;
        ctx.gstate.screen_angle = 15.0;
        op_currentscreen(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.len(), 3);
        let _proc = ctx.o_stack.pop().unwrap();
        let angle = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        let freq = ctx.o_stack.pop().unwrap().as_f64().unwrap();
        assert!((freq - 90.0).abs() < 1e-10);
        assert!((angle - 15.0).abs() < 1e-10);
    }

    #[test]
    fn test_settransfer_stores_proc() {
        let mut ctx = setup();
        let e = ctx.arrays.allocate_from(&[]);
        ctx.o_stack.push(PsObject::procedure(e, 0)).unwrap();
        op_settransfer(&mut ctx).unwrap();
        assert!(ctx.o_stack.is_empty());
        assert!(ctx.gstate.transfer_function.is_some());
    }

    #[test]
    fn test_currenttransfer_returns_stored() {
        let mut ctx = setup();
        let e = ctx.arrays.allocate_from(&[]);
        let proc_obj = PsObject::procedure(e, 0);
        ctx.gstate.transfer_function = Some(proc_obj);
        op_currenttransfer(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.len(), 1);
    }

    #[test]
    fn test_setsmoothness_clamp() {
        let mut ctx = setup();
        ctx.o_stack.push(PsObject::real(2.0)).unwrap();
        op_setsmoothness(&mut ctx).unwrap();
        assert!((ctx.gstate.smoothness - 1.0).abs() < 1e-10);
        ctx.o_stack.push(PsObject::real(-0.5)).unwrap();
        op_setsmoothness(&mut ctx).unwrap();
        assert!((ctx.gstate.smoothness - 0.0).abs() < 1e-10);
    }
}
