// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Image operators: image, imagemask, colorimage.

use stet_core::context::Context;
use stet_core::device::ImageParams;
use stet_core::dict::DictKey;
use stet_core::display_list::DisplayElement;
use stet_core::error::PsError;
use stet_core::graphics_state::{ColorSpace, DeviceColor, Matrix};
use stet_core::object::{PsObject, PsValue};

// ---------- image operator ----------

/// `image` — 5-operand form: width height bps matrix datasrc image → —
/// Dict form: dict image → —
pub fn op_image(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }

    let top = ctx.o_stack.peek(0)?;
    if matches!(top.value, PsValue::Dict(_)) {
        return image_dict_form(ctx);
    }

    // 5-operand form
    if ctx.o_stack.len() < 5 {
        return Err(PsError::StackUnderflow);
    }
    let src_obj = ctx.o_stack.peek(0)?;
    let mat_obj = ctx.o_stack.peek(1)?;
    let bps_obj = ctx.o_stack.peek(2)?;
    let h_obj = ctx.o_stack.peek(3)?;
    let w_obj = ctx.o_stack.peek(4)?;

    let width = w_obj.as_i32().ok_or(PsError::TypeCheck)?;
    let height = h_obj.as_i32().ok_or(PsError::TypeCheck)?;
    let bps = bps_obj.as_i32().ok_or(PsError::TypeCheck)?;
    if width < 0 || height < 0 {
        return Err(PsError::RangeCheck);
    }
    if !matches!(bps, 1 | 2 | 4 | 8 | 12) {
        return Err(PsError::RangeCheck);
    }
    let image_matrix = extract_matrix(ctx, mat_obj)?;

    // Per PLRM: the 5-operand form of `image` always renders grayscale
    // (1 component per sample), regardless of the current color space.
    // Multi-component images use the dict form or `colorimage`.
    let ncomp = 1;

    // Calculate bytes needed
    let bits_per_row = width as usize * ncomp * bps as usize;
    let bytes_per_row = bits_per_row.div_ceil(8);
    let total_bytes = bytes_per_row * height as usize;

    // Check if data source is a procedure
    if is_procedure(&src_obj) {
        let procedure = src_obj;
        let decode: Vec<f64> = (0..ncomp).flat_map(|_| [0.0, 1.0]).collect();

        // Pop all 5 operands
        ctx.o_stack.pop()?; // src
        ctx.o_stack.pop()?; // matrix
        ctx.o_stack.pop()?; // bps
        ctx.o_stack.pop()?; // height
        ctx.o_stack.pop()?; // width

        let data = collect_proc_data(ctx, procedure, total_bytes)?;
        let samples = unpack_samples(&data, width as u32, height as u32, bps as u32, ncomp as u32, false);
        let rgba = samples_to_rgba(
            ctx,
            &samples,
            width as u32,
            height as u32,
            ncomp as u32,
            &decode,
        );
        draw_image_to_device(ctx, rgba, width as u32, height as u32, false, &image_matrix);
        return Ok(());
    }

    // Read data from source
    let data = read_image_data(ctx, src_obj, total_bytes)?;

    ctx.o_stack.pop()?; // src
    ctx.o_stack.pop()?; // matrix
    ctx.o_stack.pop()?; // bps
    ctx.o_stack.pop()?; // height
    ctx.o_stack.pop()?; // width

    // Default decode: [0 1] per component
    let decode: Vec<f64> = (0..ncomp).flat_map(|_| [0.0, 1.0]).collect();

    // Unpack samples, convert to RGBA
    let samples = unpack_samples(&data, width as u32, height as u32, bps as u32, ncomp as u32, false);
    let rgba = samples_to_rgba(
        ctx,
        &samples,
        width as u32,
        height as u32,
        ncomp as u32,
        &decode,
    );

    // Draw
    draw_image_to_device(ctx, rgba, width as u32, height as u32, false, &image_matrix);
    Ok(())
}

/// Dict form of image: dict → —
fn image_dict_form(ctx: &mut Context) -> Result<(), PsError> {
    let dict_obj = ctx.o_stack.peek(0)?;
    let dict_entity = match dict_obj.value {
        PsValue::Dict(e) => e,
        _ => return Err(PsError::TypeCheck),
    };

    // Extract required dict keys
    let width = dict_get_int(ctx, dict_entity, b"Width").ok_or(PsError::Undefined)? as u32;
    let height = dict_get_int(ctx, dict_entity, b"Height").ok_or(PsError::Undefined)? as u32;
    let bps = dict_get_int(ctx, dict_entity, b"BitsPerComponent").ok_or(PsError::Undefined)? as u32;
    let image_matrix =
        dict_get_matrix(ctx, dict_entity, b"ImageMatrix").ok_or(PsError::Undefined)?;

    if !matches!(bps, 1 | 2 | 4 | 8 | 12) {
        return Err(PsError::RangeCheck);
    }

    // Get DataSource
    let data_source = dict_get_obj(ctx, dict_entity, b"DataSource").ok_or(PsError::Undefined)?;

    // Get Decode array
    let decode = dict_get_decode(ctx, dict_entity).unwrap_or_default();

    // Determine ncomp from decode array or color space
    let ncomp = if decode.len() >= 2 {
        (decode.len() / 2) as u32
    } else {
        match ctx.gstate.color_space {
            ColorSpace::DeviceGray | ColorSpace::Indexed { .. } | ColorSpace::CIEBasedA { .. } => 1,
            ColorSpace::DeviceRGB
            | ColorSpace::CIEBasedABC { .. }
            | ColorSpace::CIEBasedDEF { .. } => 3,
            ColorSpace::DeviceCMYK | ColorSpace::CIEBasedDEFG { .. } => 4,
            ColorSpace::ICCBased { n, .. } => n,
            ColorSpace::Separation {
                num_alt_components, ..
            }
            | ColorSpace::DeviceN {
                num_alt_components, ..
            } => num_alt_components,
        }
    };

    // Fill default decode if empty
    let decode = if decode.is_empty() {
        (0..ncomp).flat_map(|_| [0.0, 1.0]).collect()
    } else {
        decode
    };

    // Check for MultipleDataSources
    let multi = dict_get_obj(ctx, dict_entity, b"MultipleDataSources")
        .and_then(|o| match o.value {
            PsValue::Bool(b) => Some(b),
            _ => None,
        })
        .unwrap_or(false);

    // Calculate bytes needed
    let bits_per_row = width as usize * ncomp as usize * bps as usize;
    let bytes_per_row = bits_per_row.div_ceil(8);
    let total_bytes = bytes_per_row * height as usize;

    ctx.o_stack.pop()?; // dict

    // Read data from source(s)
    let data = if multi {
        // MultipleDataSources: DataSource is an array of ncomp data sources.
        // Read each component separately (round-robin for procedures), then interleave.
        read_multi_source_data(ctx, data_source, ncomp, width, height, bps)?
    } else if is_procedure(&data_source) {
        collect_proc_data(ctx, data_source, total_bytes)?
    } else {
        read_image_data(ctx, data_source, total_bytes)?
    };

    // Unpack and convert
    let indexed = matches!(ctx.gstate.color_space, ColorSpace::Indexed { .. });
    let samples = unpack_samples(&data, width, height, bps, ncomp, indexed);
    let rgba = samples_to_rgba(ctx, &samples, width, height, ncomp, &decode);

    draw_image_to_device(ctx, rgba, width, height, false, &image_matrix);
    Ok(())
}

// ---------- imagemask operator ----------

/// `imagemask` — 5-operand form: width height polarity matrix datasrc imagemask → —
/// Dict form: dict imagemask → —
pub fn op_imagemask(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }

    let top = ctx.o_stack.peek(0)?;
    if matches!(top.value, PsValue::Dict(_)) {
        return imagemask_dict_form(ctx);
    }

    // 5-operand form
    if ctx.o_stack.len() < 5 {
        return Err(PsError::StackUnderflow);
    }
    let src_obj = ctx.o_stack.peek(0)?;
    let mat_obj = ctx.o_stack.peek(1)?;
    let pol_obj = ctx.o_stack.peek(2)?;
    let h_obj = ctx.o_stack.peek(3)?;
    let w_obj = ctx.o_stack.peek(4)?;

    let width = w_obj.as_i32().ok_or(PsError::TypeCheck)?;
    let height = h_obj.as_i32().ok_or(PsError::TypeCheck)?;
    let polarity = match pol_obj.value {
        PsValue::Bool(b) => b,
        _ => return Err(PsError::TypeCheck),
    };
    if width < 0 || height < 0 {
        return Err(PsError::RangeCheck);
    }
    let image_matrix = extract_matrix(ctx, mat_obj)?;

    // Calculate bytes needed: 1 bit per pixel, row-aligned
    let bits_per_row = width as usize;
    let bytes_per_row = bits_per_row.div_ceil(8);
    let total_bytes = bytes_per_row * height as usize;

    // Check if data source is a procedure (executable array)
    if is_procedure(&src_obj) {
        let procedure = src_obj;
        let color = ctx.gstate.color.clone();

        // Pop all 5 operands
        ctx.o_stack.pop()?; // src
        ctx.o_stack.pop()?; // matrix
        ctx.o_stack.pop()?; // polarity
        ctx.o_stack.pop()?; // height
        ctx.o_stack.pop()?; // width

        let data = collect_proc_data(ctx, procedure, total_bytes)?;
        let rgba = mask_to_rgba(&data, width as u32, height as u32, polarity, &color);
        draw_image_to_device(ctx, rgba, width as u32, height as u32, true, &image_matrix);
        return Ok(());
    }

    let data = read_image_data(ctx, src_obj, total_bytes)?;

    ctx.o_stack.pop()?; // src
    ctx.o_stack.pop()?; // matrix
    ctx.o_stack.pop()?; // polarity
    ctx.o_stack.pop()?; // height
    ctx.o_stack.pop()?; // width

    // Convert mask to RGBA using current color
    let color = ctx.gstate.color.clone();
    let rgba = mask_to_rgba(&data, width as u32, height as u32, polarity, &color);

    draw_image_to_device(ctx, rgba, width as u32, height as u32, true, &image_matrix);
    Ok(())
}

/// Dict form of imagemask
fn imagemask_dict_form(ctx: &mut Context) -> Result<(), PsError> {
    let dict_obj = ctx.o_stack.peek(0)?;
    let dict_entity = match dict_obj.value {
        PsValue::Dict(e) => e,
        _ => return Err(PsError::TypeCheck),
    };

    let width = dict_get_int(ctx, dict_entity, b"Width").ok_or(PsError::Undefined)? as u32;
    let height = dict_get_int(ctx, dict_entity, b"Height").ok_or(PsError::Undefined)? as u32;
    let image_matrix =
        dict_get_matrix(ctx, dict_entity, b"ImageMatrix").ok_or(PsError::Undefined)?;
    let data_source = dict_get_obj(ctx, dict_entity, b"DataSource").ok_or(PsError::Undefined)?;

    // Decode determines polarity: [1 0] → true, [0 1] → false
    let decode = dict_get_decode(ctx, dict_entity).unwrap_or_else(|| vec![0.0, 1.0]);
    let polarity = decode.first().copied().unwrap_or(0.0) > 0.5;

    let bits_per_row = width as usize;
    let bytes_per_row = bits_per_row.div_ceil(8);
    let total_bytes = bytes_per_row * height as usize;

    let data = read_image_data(ctx, data_source, total_bytes)?;

    ctx.o_stack.pop()?; // dict

    let color = ctx.gstate.color.clone();
    let rgba = mask_to_rgba(&data, width, height, polarity, &color);

    draw_image_to_device(ctx, rgba, width, height, true, &image_matrix);
    Ok(())
}

// ---------- colorimage operator ----------

/// `colorimage`: w h bps matrix datasrc multi ncomp colorimage → —
pub fn op_colorimage(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }

    let ncomp_obj = ctx.o_stack.peek(0)?;
    let multi_obj = ctx.o_stack.peek(1)?;

    let ncomp = ncomp_obj.as_i32().ok_or(PsError::TypeCheck)?;
    let multi = match multi_obj.value {
        PsValue::Bool(b) => b,
        _ => return Err(PsError::TypeCheck),
    };

    if !matches!(ncomp, 1 | 3 | 4) {
        return Err(PsError::RangeCheck);
    }

    // Determine how many data sources
    let num_sources = if multi { ncomp as usize } else { 1 };
    // Stack: w h bps matrix [datasrc...] multi ncomp
    // Actually: w h bps matrix datasrc multi ncomp  (for single src)
    //   or:     w h bps matrix src1 src2 src3 multi ncomp  (for multi=true)
    // Total stack items: w, h, bps, matrix = 4, plus src(s), plus multi, ncomp = 2
    let total_needed = 4 + num_sources + 2;
    if ctx.o_stack.len() < total_needed {
        return Err(PsError::StackUnderflow);
    }

    // Peek remaining arguments
    let mat_idx = 2 + num_sources; // from top: ncomp, multi, src(s)..., matrix
    let bps_idx = mat_idx + 1;
    let h_idx = bps_idx + 1;
    let w_idx = h_idx + 1;

    let mat_obj = ctx.o_stack.peek(mat_idx)?;
    let bps_obj = ctx.o_stack.peek(bps_idx)?;
    let h_obj = ctx.o_stack.peek(h_idx)?;
    let w_obj = ctx.o_stack.peek(w_idx)?;

    let width = w_obj.as_i32().ok_or(PsError::TypeCheck)?;
    let height = h_obj.as_i32().ok_or(PsError::TypeCheck)?;
    let bps = bps_obj.as_i32().ok_or(PsError::TypeCheck)?;
    if width < 0 || height < 0 {
        return Err(PsError::RangeCheck);
    }
    if !matches!(bps, 1 | 2 | 4 | 8 | 12) {
        return Err(PsError::RangeCheck);
    }
    let image_matrix = extract_matrix(ctx, mat_obj)?;

    // Read data
    let bits_per_row = width as usize * ncomp as usize * bps as usize;
    let bytes_per_row = bits_per_row.div_ceil(8);
    let total_bytes = bytes_per_row * height as usize;

    let data = if multi && num_sources > 1 {
        // Multiple data sources — check if any are procedures
        let first_src = ctx.o_stack.peek(2 + (num_sources - 1))?;
        if is_procedure(&first_src) {
            // Procedure data sources — call each synchronously
            let bits_per_comp_row = width as usize * bps as usize;
            let bytes_per_comp_row = bits_per_comp_row.div_ceil(8);
            let total_comp_bytes = bytes_per_comp_row * height as usize;

            // Collect all procedure objects (from bottom to top on stack)
            let mut procedures = Vec::new();
            for i in 0..num_sources {
                procedures.push(ctx.o_stack.peek(2 + (num_sources - 1 - i))?);
            }

            // Pop all operands
            for _ in 0..(w_idx + 1) {
                ctx.o_stack.pop()?;
            }

            // Read row-by-row: for each scanline, call each procedure once.
            // The data sources typically share a single currentfile, so they
            // must be called in interleaved order (R-row, G-row, B-row, repeat).
            let mut comp_data: Vec<Vec<u8>> = (0..num_sources)
                .map(|_| Vec::with_capacity(total_comp_bytes))
                .collect();
            for _row in 0..height {
                for (c, proc_obj) in procedures.iter().enumerate() {
                    ctx.exec_sync(*proc_obj)?;
                    if let Ok(result) = ctx.o_stack.peek(0) {
                        if let PsValue::String { entity, start, len } = result.value {
                            let bytes = ctx.strings.get(entity, start, len).to_vec();
                            ctx.o_stack.pop()?;
                            comp_data[c].extend_from_slice(&bytes);
                        } else {
                            ctx.o_stack.pop()?;
                        }
                    }
                }
            }
            let data = interleave_components(
                &comp_data,
                width as u32,
                height as u32,
                bps as u32,
                ncomp as u32,
            );

            let decode: Vec<f64> = (0..ncomp).flat_map(|_| [0.0, 1.0]).collect();
            let samples =
                unpack_samples(&data, width as u32, height as u32, bps as u32, ncomp as u32, false);
            let rgba = samples_to_rgba(
                ctx,
                &samples,
                width as u32,
                height as u32,
                ncomp as u32,
                &decode,
            );
            draw_image_to_device(ctx, rgba, width as u32, height as u32, false, &image_matrix);
            return Ok(());
        }

        // Non-procedure multiple data sources — read each, then interleave
        let bits_per_comp_row = width as usize * bps as usize;
        let bytes_per_comp_row = bits_per_comp_row.div_ceil(8);
        let total_comp_bytes = bytes_per_comp_row * height as usize;

        let mut comp_data = Vec::new();
        for i in 0..num_sources {
            let src_obj = ctx.o_stack.peek(2 + (num_sources - 1 - i))?;
            let d = read_image_data(ctx, src_obj, total_comp_bytes)?;
            comp_data.push(d);
        }
        interleave_components(
            &comp_data,
            width as u32,
            height as u32,
            bps as u32,
            ncomp as u32,
        )
    } else {
        // Single source
        let src_obj = ctx.o_stack.peek(2)?;
        if is_procedure(&src_obj) {
            let procedure = src_obj;
            // Pop all operands before calling procedure
            for _ in 0..(w_idx + 1) {
                ctx.o_stack.pop()?;
            }
            let data = collect_proc_data(ctx, procedure, total_bytes)?;
            let decode: Vec<f64> = (0..ncomp).flat_map(|_| [0.0, 1.0]).collect();
            let samples =
                unpack_samples(&data, width as u32, height as u32, bps as u32, ncomp as u32, false);
            let rgba = samples_to_rgba(
                ctx,
                &samples,
                width as u32,
                height as u32,
                ncomp as u32,
                &decode,
            );
            draw_image_to_device(ctx, rgba, width as u32, height as u32, false, &image_matrix);
            return Ok(());
        }
        read_image_data(ctx, src_obj, total_bytes)?
    };

    // Pop all
    for _ in 0..(w_idx + 1) {
        ctx.o_stack.pop()?;
    }

    // Default decode
    let decode: Vec<f64> = (0..ncomp).flat_map(|_| [0.0, 1.0]).collect();

    let samples = unpack_samples(&data, width as u32, height as u32, bps as u32, ncomp as u32, false);
    let rgba = samples_to_rgba(
        ctx,
        &samples,
        width as u32,
        height as u32,
        ncomp as u32,
        &decode,
    );

    draw_image_to_device(ctx, rgba, width as u32, height as u32, false, &image_matrix);
    Ok(())
}

// ---------- Helper functions ----------

/// Extract a 6-element matrix from a PsObject (array).
fn extract_matrix(ctx: &Context, obj: PsObject) -> Result<Matrix, PsError> {
    match obj.value {
        PsValue::Array { entity, start, len } => {
            if len < 6 {
                return Err(PsError::RangeCheck);
            }
            let mut vals = [0.0f64; 6];
            for (i, val) in vals.iter_mut().enumerate() {
                let elem = ctx.arrays.get_element(entity, start + i as u32);
                *val = elem.as_f64().ok_or(PsError::TypeCheck)?;
            }
            Ok(Matrix::new(
                vals[0], vals[1], vals[2], vals[3], vals[4], vals[5],
            ))
        }
        _ => Err(PsError::TypeCheck),
    }
}

/// Read image data from a source object (string, file, or procedure).
fn read_image_data(
    ctx: &mut Context,
    src: PsObject,
    bytes_needed: usize,
) -> Result<Vec<u8>, PsError> {
    match src.value {
        PsValue::String { entity, start, len } => {
            let data = ctx.strings.get(entity, start, len).to_vec();
            Ok(data)
        }
        PsValue::File(entity) => {
            let mut buf = vec![0u8; bytes_needed];
            let n = ctx
                .files
                .read_into(entity, &mut buf)
                .map_err(|_| PsError::IOError)?;
            buf.truncate(n);
            Ok(buf)
        }
        _ => {
            if is_procedure(&src) {
                collect_proc_data(ctx, src, bytes_needed)
            } else {
                Err(PsError::TypeCheck)
            }
        }
    }
}

/// Read data from multiple data sources and interleave pixel-by-pixel.
///
/// When MultipleDataSources is true, DataSource is an array of N separate
/// sources (one per color component). For procedure sources, reads row-by-row
/// in round-robin order (proc0-row, proc1-row, ..., repeat) because procedures
/// typically share currentfile with interleaved row data.
fn read_multi_source_data(
    ctx: &mut Context,
    ds_array: PsObject,
    ncomp: u32,
    width: u32,
    height: u32,
    bps: u32,
) -> Result<Vec<u8>, PsError> {
    let (entity, start, len) = match ds_array.value {
        PsValue::Array { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };

    if len < ncomp {
        return Err(PsError::RangeCheck);
    }

    let bits_per_row = width as usize * bps as usize;
    let bytes_per_row = bits_per_row.div_ceil(8);
    let bytes_per_component = bytes_per_row * height as usize;

    // Collect source objects
    let mut sources: Vec<PsObject> = Vec::with_capacity(ncomp as usize);
    for i in 0..ncomp {
        sources.push(ctx.arrays.get_element(entity, start + i));
    }

    let all_procedures = sources.iter().all(is_procedure);

    let component_data = if all_procedures {
        // Round-robin reading: for each row, call each procedure once.
        // This is required because procedures typically share currentfile,
        // where data is interleaved (C-row, M-row, Y-row, K-row, ...).
        let mut comp_data: Vec<Vec<u8>> = (0..ncomp as usize)
            .map(|_| Vec::with_capacity(bytes_per_component))
            .collect();

        for _row in 0..height {
            for (c, proc_obj) in sources.iter().enumerate() {
                ctx.exec_sync(*proc_obj)?;
                if let Ok(result) = ctx.o_stack.peek(0) {
                    if let PsValue::String { entity, start, len } = result.value {
                        let bytes = ctx.strings.get(entity, start, len).to_vec();
                        ctx.o_stack.pop()?;
                        comp_data[c].extend_from_slice(&bytes);
                    } else {
                        ctx.o_stack.pop()?;
                    }
                }
            }
        }
        comp_data
    } else {
        // Non-procedure sources: read sequentially (each has independent data)
        let mut comp_data: Vec<Vec<u8>> = Vec::with_capacity(ncomp as usize);
        for src in &sources {
            let data = read_image_data(ctx, *src, bytes_per_component)?;
            comp_data.push(data);
        }
        comp_data
    };

    // Interleave components
    Ok(interleave_components(
        &component_data,
        width,
        height,
        bps,
        ncomp,
    ))
}

/// Unpack samples from raw data (1/2/4/8/12 bits) to 8-bit values.
fn unpack_samples(raw: &[u8], width: u32, height: u32, bps: u32, ncomp: u32, indexed: bool) -> Vec<u8> {
    if bps == 8 {
        return raw.to_vec();
    }

    let total_samples = width as usize * height as usize * ncomp as usize;
    let mut result = Vec::with_capacity(total_samples);

    if bps == 12 {
        // 12-bit samples: each sample is 1.5 bytes
        let bits_per_row = width as usize * ncomp as usize * 12;
        let bytes_per_row = bits_per_row.div_ceil(8);

        for row in 0..height as usize {
            let row_start = row * bytes_per_row;
            let mut bit_pos = 0usize;
            for _ in 0..(width as usize * ncomp as usize) {
                let byte_idx = row_start + bit_pos / 8;
                let bit_offset = bit_pos % 8;
                if byte_idx + 1 >= raw.len() {
                    result.push(0);
                    bit_pos += 12;
                    continue;
                }
                let sample = if bit_offset == 0 {
                    ((raw[byte_idx] as u16) << 4) | ((raw[byte_idx + 1] as u16) >> 4)
                } else {
                    // bit_offset == 4
                    ((raw[byte_idx] as u16 & 0x0F) << 8)
                        | raw.get(byte_idx + 1).copied().unwrap_or(0) as u16
                };
                // Scale 12-bit to 8-bit
                result.push((sample >> 4) as u8);
                bit_pos += 12;
            }
        }
    } else {
        // 1, 2, or 4 bits per sample
        let mask = ((1u16 << bps) - 1) as u8;
        let samples_per_byte = 8 / bps as usize;
        let samples_per_row = width as usize * ncomp as usize;
        let bytes_per_row = (samples_per_row * bps as usize).div_ceil(8);
        let max_val = mask as f64;

        for row in 0..height as usize {
            let row_start = row * bytes_per_row;
            for s in 0..samples_per_row {
                let byte_idx = row_start + s / samples_per_byte;
                let bit_offset = (samples_per_byte - 1 - (s % samples_per_byte)) * bps as usize;
                let byte_val = raw.get(byte_idx).copied().unwrap_or(0);
                let sample = (byte_val >> bit_offset) & mask;
                if indexed {
                    // Indexed: raw value is palette index, don't scale
                    result.push(sample);
                } else {
                    // Scale to 8-bit
                    result.push((sample as f64 / max_val * 255.0).round() as u8);
                }
            }
        }
    }

    result
}

/// Convert 8-bit samples to RGBA, applying decode array and color space conversion.
fn samples_to_rgba(
    ctx: &Context,
    samples: &[u8],
    width: u32,
    height: u32,
    ncomp: u32,
    decode: &[f64],
) -> Vec<u8> {
    let pixel_count = (width * height) as usize;
    let mut rgba = vec![255u8; pixel_count * 4]; // Pre-fill alpha = 255

    // Check for Indexed color space
    if let ColorSpace::Indexed {
        ref base,
        hival,
        ref lookup,
    } = ctx.gstate.color_space
    {
        // Each sample is an index into the lookup table
        for i in 0..pixel_count {
            let idx = samples.get(i).copied().unwrap_or(0) as usize;
            let idx = idx.min(hival as usize);
            let base_ncomp = match base.as_ref() {
                ColorSpace::DeviceGray => 1,
                ColorSpace::DeviceRGB => 3,
                ColorSpace::DeviceCMYK => 4,
                _ => 3,
            };
            let offset = idx * base_ncomp;
            let color = match base.as_ref() {
                ColorSpace::DeviceGray => {
                    let g = lookup.get(offset).copied().unwrap_or(0) as f64 / 255.0;
                    DeviceColor::from_gray(g)
                }
                ColorSpace::DeviceRGB => {
                    let r = lookup.get(offset).copied().unwrap_or(0) as f64 / 255.0;
                    let g = lookup.get(offset + 1).copied().unwrap_or(0) as f64 / 255.0;
                    let b = lookup.get(offset + 2).copied().unwrap_or(0) as f64 / 255.0;
                    DeviceColor::from_rgb(r, g, b)
                }
                ColorSpace::DeviceCMYK => {
                    let c = lookup.get(offset).copied().unwrap_or(0) as f64 / 255.0;
                    let m = lookup.get(offset + 1).copied().unwrap_or(0) as f64 / 255.0;
                    let y = lookup.get(offset + 2).copied().unwrap_or(0) as f64 / 255.0;
                    let k = lookup.get(offset + 3).copied().unwrap_or(0) as f64 / 255.0;
                    DeviceColor::from_cmyk(c, m, y, k)
                }
                _ => DeviceColor::from_gray(0.0),
            };
            let pi = i * 4;
            rgba[pi] = (color.r * 255.0).round().clamp(0.0, 255.0) as u8;
            rgba[pi + 1] = (color.g * 255.0).round().clamp(0.0, 255.0) as u8;
            rgba[pi + 2] = (color.b * 255.0).round().clamp(0.0, 255.0) as u8;
        }
        return rgba;
    }

    for i in 0..pixel_count {
        let si = i * ncomp as usize;
        let pi = i * 4;

        match ncomp {
            1 => {
                // Grayscale
                let raw = samples.get(si).copied().unwrap_or(0) as f64 / 255.0;
                let d_min = decode.first().copied().unwrap_or(0.0);
                let d_max = decode.get(1).copied().unwrap_or(1.0);
                let val = d_min + raw * (d_max - d_min);
                let v = (val.clamp(0.0, 1.0) * 255.0).round() as u8;
                rgba[pi] = v;
                rgba[pi + 1] = v;
                rgba[pi + 2] = v;
            }
            3 => {
                // RGB
                for c in 0..3 {
                    let raw = samples.get(si + c).copied().unwrap_or(0) as f64 / 255.0;
                    let d_min = decode.get(c * 2).copied().unwrap_or(0.0);
                    let d_max = decode.get(c * 2 + 1).copied().unwrap_or(1.0);
                    let val = d_min + raw * (d_max - d_min);
                    rgba[pi + c] = (val.clamp(0.0, 1.0) * 255.0).round() as u8;
                }
            }
            4 => {
                // CMYK → RGB
                let mut cmyk = [0.0f64; 4];
                for (c, val) in cmyk.iter_mut().enumerate() {
                    let raw = samples.get(si + c).copied().unwrap_or(0) as f64 / 255.0;
                    let d_min = decode.get(c * 2).copied().unwrap_or(0.0);
                    let d_max = decode.get(c * 2 + 1).copied().unwrap_or(1.0);
                    *val = (d_min + raw * (d_max - d_min)).clamp(0.0, 1.0);
                }
                let color = DeviceColor::from_cmyk(cmyk[0], cmyk[1], cmyk[2], cmyk[3]);
                rgba[pi] = (color.r * 255.0).round().clamp(0.0, 255.0) as u8;
                rgba[pi + 1] = (color.g * 255.0).round().clamp(0.0, 255.0) as u8;
                rgba[pi + 2] = (color.b * 255.0).round().clamp(0.0, 255.0) as u8;
            }
            _ => {}
        }
    }

    rgba
}

/// Convert a 1-bit mask to RGBA using the current color.
fn mask_to_rgba(
    raw: &[u8],
    width: u32,
    height: u32,
    polarity: bool,
    color: &DeviceColor,
) -> Vec<u8> {
    let pixel_count = (width * height) as usize;
    let mut rgba = vec![0u8; pixel_count * 4];
    let bytes_per_row = (width as usize).div_ceil(8);

    let r = (color.r * 255.0).round().clamp(0.0, 255.0) as u8;
    let g = (color.g * 255.0).round().clamp(0.0, 255.0) as u8;
    let b = (color.b * 255.0).round().clamp(0.0, 255.0) as u8;

    for row in 0..height as usize {
        for col in 0..width as usize {
            let byte_idx = row * bytes_per_row + col / 8;
            let bit_offset = 7 - (col % 8);
            let bit = if byte_idx < raw.len() {
                (raw[byte_idx] >> bit_offset) & 1
            } else {
                0
            };

            // polarity=true: bit=1 → paint (alpha=255), bit=0 → transparent
            // polarity=false: bit=0 → paint, bit=1 → transparent
            let paint = if polarity { bit == 1 } else { bit == 0 };
            let pi = (row * width as usize + col) * 4;
            if paint {
                rgba[pi] = r;
                rgba[pi + 1] = g;
                rgba[pi + 2] = b;
                rgba[pi + 3] = 255;
            }
            // else leave as 0,0,0,0 (transparent)
        }
    }

    rgba
}

/// Interleave separate component data into a single buffer.
fn interleave_components(
    comp_data: &[Vec<u8>],
    width: u32,
    height: u32,
    bps: u32,
    ncomp: u32,
) -> Vec<u8> {
    if bps == 8 {
        // Simple byte interleave
        let pixel_count = (width * height) as usize;
        let mut result = Vec::with_capacity(pixel_count * ncomp as usize);
        for i in 0..pixel_count {
            for comp in comp_data {
                result.push(comp.get(i).copied().unwrap_or(0));
            }
        }
        result
    } else {
        // For sub-byte, just concatenate (simplified)
        let mut result = Vec::new();
        for comp in comp_data {
            result.extend_from_slice(comp);
        }
        result
    }
}

/// Record an image draw to the display list.
fn draw_image_to_device(
    ctx: &mut Context,
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    is_mask: bool,
    image_matrix: &Matrix,
) {
    let params = ImageParams {
        width,
        height,
        is_mask,
        ctm: ctx.gstate.ctm,
        image_matrix: *image_matrix,
    };
    ctx.display_list.push(DisplayElement::Image {
        rgba_data: rgba,
        params,
    });
}

// ---------- Procedure data source support ----------

/// Check if a PsObject is an executable procedure (array).
fn is_procedure(obj: &PsObject) -> bool {
    matches!(obj.value, PsValue::Array { .. }) && obj.flags.is_executable()
}

/// Collect data from a procedure data source by calling it synchronously.
/// Per PLRM, the procedure pushes a string each call; empty string = end of data.
fn collect_proc_data(
    ctx: &mut Context,
    procedure: PsObject,
    bytes_needed: usize,
) -> Result<Vec<u8>, PsError> {
    let mut data = Vec::with_capacity(bytes_needed);

    loop {
        ctx.exec_sync(procedure)?;

        if ctx.o_stack.is_empty() {
            break;
        }
        let result = ctx.o_stack.peek(0)?;
        match result.value {
            PsValue::String { entity, start, len } => {
                let bytes = ctx.strings.get(entity, start, len).to_vec();
                ctx.o_stack.pop()?;
                if bytes.is_empty() {
                    break;
                }
                data.extend_from_slice(&bytes);
                if data.len() >= bytes_needed {
                    break;
                }
            }
            _ => break,
        }
    }

    // Pad with zeros if short
    data.resize(bytes_needed, 0);
    Ok(data)
}

// ---------- Dict helper functions ----------

/// Look up an integer value in a dict by name.
fn dict_get_int(ctx: &Context, dict: stet_core::object::EntityId, name: &[u8]) -> Option<i32> {
    let id = ctx.names.find(name)?;
    let val = ctx.dicts.get(dict, &DictKey::Name(id))?;
    val.as_i32().or_else(|| val.as_f64().map(|f| f as i32))
}

/// Look up a matrix from an array in a dict by name.
fn dict_get_matrix(
    ctx: &Context,
    dict: stet_core::object::EntityId,
    name: &[u8],
) -> Option<Matrix> {
    let id = ctx.names.find(name)?;
    let val = ctx.dicts.get(dict, &DictKey::Name(id))?;
    match val.value {
        PsValue::Array { entity, start, len } if len >= 6 => {
            let mut vals = [0.0f64; 6];
            for (i, val) in vals.iter_mut().enumerate() {
                let elem = ctx.arrays.get_element(entity, start + i as u32);
                *val = elem.as_f64()?;
            }
            Some(Matrix::new(
                vals[0], vals[1], vals[2], vals[3], vals[4], vals[5],
            ))
        }
        _ => None,
    }
}

/// Look up any object in a dict by name.
fn dict_get_obj(
    ctx: &Context,
    dict: stet_core::object::EntityId,
    name: &[u8],
) -> Option<PsObject> {
    let id = ctx.names.find(name)?;
    ctx.dicts.get(dict, &DictKey::Name(id))
}

/// Look up a decode array from a dict.
fn dict_get_decode(ctx: &Context, dict: stet_core::object::EntityId) -> Option<Vec<f64>> {
    let id = ctx.names.find(b"Decode")?;
    let val = ctx.dicts.get(dict, &DictKey::Name(id))?;
    match val.value {
        PsValue::Array { entity, start, len } => {
            let mut decode = Vec::with_capacity(len as usize);
            for i in 0..len {
                let elem = ctx.arrays.get_element(entity, start + i);
                decode.push(elem.as_f64()?);
            }
            Some(decode)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unpack_8bit() {
        let data = vec![0, 128, 255];
        let result = unpack_samples(&data, 3, 1, 8, 1, false);
        assert_eq!(result, vec![0, 128, 255]);
    }

    #[test]
    fn test_unpack_1bit() {
        // 0b10110000 = 0xB0 → samples: 1,0,1,1,0,0,0,0
        let data = vec![0xB0];
        let result = unpack_samples(&data, 4, 1, 1, 1, false);
        // 1 bit: 1→255, 0→0
        assert_eq!(result[0], 255); // bit 1
        assert_eq!(result[1], 0); // bit 0
        assert_eq!(result[2], 255); // bit 1
        assert_eq!(result[3], 255); // bit 1
    }

    #[test]
    fn test_unpack_4bit() {
        // 0xAC → high nibble 0xA=10, low nibble 0xC=12
        let data = vec![0xAC];
        let result = unpack_samples(&data, 2, 1, 4, 1, false);
        // 10/15 * 255 = 170, 12/15 * 255 = 204
        assert_eq!(result[0], 170);
        assert_eq!(result[1], 204);
    }

    #[test]
    fn test_mask_to_rgba_polarity_true() {
        // 1 bit mask: 0b10000000 = 0x80
        let data = vec![0x80];
        let color = DeviceColor::from_rgb(1.0, 0.0, 0.0);
        let rgba = mask_to_rgba(&data, 2, 1, true, &color);
        // Pixel 0: bit=1, polarity=true → paint (red, alpha=255)
        assert_eq!(rgba[0], 255); // R
        assert_eq!(rgba[3], 255); // A
        // Pixel 1: bit=0, polarity=true → transparent
        assert_eq!(rgba[4], 0); // R
        assert_eq!(rgba[7], 0); // A
    }

    #[test]
    fn test_mask_to_rgba_polarity_false() {
        let data = vec![0x80];
        let color = DeviceColor::from_rgb(0.0, 1.0, 0.0);
        let rgba = mask_to_rgba(&data, 2, 1, false, &color);
        // Pixel 0: bit=1, polarity=false → transparent
        assert_eq!(rgba[3], 0);
        // Pixel 1: bit=0, polarity=false → paint (green, alpha=255)
        assert_eq!(rgba[5], 255); // G
        assert_eq!(rgba[7], 255); // A
    }

    #[test]
    fn test_samples_to_rgba_gray() {
        let ctx = stet_core::context::Context::new();
        let samples = vec![0, 128, 255];
        let decode = vec![0.0, 1.0];
        let rgba = samples_to_rgba(&ctx, &samples, 3, 1, 1, &decode);
        assert_eq!(rgba[0], 0); // R of pixel 0
        assert_eq!(rgba[4], 128); // R of pixel 1
        assert_eq!(rgba[8], 255); // R of pixel 2
        assert_eq!(rgba[3], 255); // A of pixel 0
    }

    #[test]
    fn test_samples_to_rgba_rgb() {
        let ctx = stet_core::context::Context::new();
        let samples = vec![255, 0, 0, 0, 255, 0]; // red, green
        let decode = vec![0.0, 1.0, 0.0, 1.0, 0.0, 1.0];
        let rgba = samples_to_rgba(&ctx, &samples, 2, 1, 3, &decode);
        assert_eq!(rgba[0], 255); // R
        assert_eq!(rgba[1], 0); // G
        assert_eq!(rgba[2], 0); // B
        assert_eq!(rgba[4], 0); // R
        assert_eq!(rgba[5], 255); // G
        assert_eq!(rgba[6], 0); // B
    }

    #[test]
    fn test_interleave_components() {
        let comp_r = vec![255, 0];
        let comp_g = vec![0, 255];
        let comp_b = vec![0, 0];
        let result = interleave_components(&[comp_r, comp_g, comp_b], 2, 1, 8, 3);
        assert_eq!(result, vec![255, 0, 0, 0, 255, 0]);
    }
}
