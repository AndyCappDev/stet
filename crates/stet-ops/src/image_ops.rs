// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Image operators: image, imagemask, colorimage.

use std::sync::Arc;

use stet_core::context::Context;
use stet_core::device::{ImageColorSpace, ImageParams};
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
    if width <= 0 || height <= 0 {
        return Err(PsError::RangeCheck);
    }
    if !matches!(bps, 1 | 2 | 4 | 8 | 12) {
        return Err(PsError::RangeCheck);
    }
    let image_matrix = extract_matrix(ctx, mat_obj)?;

    // Per PLRM: the 5-operand form of `image` always renders grayscale
    // (1 component per sample), regardless of the current color space.
    // Multi-component images use the dict form or `colorimage`.
    let ncomp = 1u32;

    // Calculate bytes needed
    let bits_per_row = width as usize * ncomp as usize * bps as usize;
    let bytes_per_row = bits_per_row.div_ceil(8);
    let total_bytes = bytes_per_row * height as usize;

    // Check if data source is a procedure
    if is_procedure(&src_obj) {
        let procedure = src_obj;

        // Pop all 5 operands
        ctx.o_stack.pop()?; // src
        ctx.o_stack.pop()?; // matrix
        ctx.o_stack.pop()?; // bps
        ctx.o_stack.pop()?; // height
        ctx.o_stack.pop()?; // width

        let data = collect_proc_data(ctx, procedure, total_bytes)?;
        let samples = unpack_samples(&data, width as u32, height as u32, bps as u32, ncomp, false);
        draw_image_to_device(
            ctx,
            samples,
            width as u32,
            height as u32,
            ImageColorSpace::DeviceGray,
            &image_matrix,
            None,
        );
        return Ok(());
    }

    // Pop all 5 operands before reading data (matches PostForge behavior)
    ctx.o_stack.pop()?; // src
    ctx.o_stack.pop()?; // matrix
    ctx.o_stack.pop()?; // bps
    ctx.o_stack.pop()?; // height
    ctx.o_stack.pop()?; // width

    // Read data from source
    let data = read_image_data(ctx, src_obj, total_bytes)?;

    // Unpack to 8-bit
    let samples = unpack_samples(&data, width as u32, height as u32, bps as u32, ncomp, false);

    // Draw with DeviceGray (5-operand form is always grayscale)
    draw_image_to_device(
        ctx,
        samples,
        width as u32,
        height as u32,
        ImageColorSpace::DeviceGray,
        &image_matrix,
        None,
    );
    Ok(())
}

/// Dict form of image: dict → —
fn image_dict_form(ctx: &mut Context) -> Result<(), PsError> {
    let dict_obj = ctx.o_stack.peek(0)?;
    let dict_entity = match dict_obj.value {
        PsValue::Dict(e) => e,
        _ => return Err(PsError::TypeCheck),
    };

    // Check ImageType — required per PostForge (TypeCheck if missing or non-integer)
    let image_type_obj = dict_get_obj(ctx, dict_entity, b"ImageType").ok_or(PsError::TypeCheck)?;
    let image_type = image_type_obj.as_i32().ok_or(PsError::TypeCheck)?;
    match image_type {
        1 | 4 => {} // Type 1 and Type 4 share most logic; Type 4 adds MaskColor
        3 => return image_type3_form(ctx, dict_entity),
        _ => return Err(PsError::RangeCheck),
    }

    // Extract required dict keys with strict type validation
    let width_obj = dict_get_obj(ctx, dict_entity, b"Width").ok_or(PsError::TypeCheck)?;
    let width = width_obj.as_i32().ok_or(PsError::TypeCheck)?;
    let height_obj = dict_get_obj(ctx, dict_entity, b"Height").ok_or(PsError::TypeCheck)?;
    let height = height_obj.as_i32().ok_or(PsError::TypeCheck)?;
    if width <= 0 || height <= 0 {
        return Err(PsError::RangeCheck);
    }
    let width = width as u32;
    let height = height as u32;
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
            ColorSpace::Separation { .. } => 1,
            ColorSpace::DeviceN { num_colorants, .. } => num_colorants,
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

    // Unpack samples to 8-bit
    let indexed = matches!(ctx.gstate.color_space, ColorSpace::Indexed { .. });
    let samples = unpack_samples(&data, width, height, bps, ncomp, indexed);

    // ImageType 4: extract mask color (raw BPC values scaled to 8-bit)
    let mask_color_param = if image_type == 4 {
        dict_get_mask_color(ctx, dict_entity, ncomp).map(|mc| {
            mc.iter()
                .map(|&v| scale_mask_value(v, bps) as u8)
                .collect::<Vec<u8>>()
        })
    } else {
        None
    };

    let needs_decode = !indexed && !is_identity_decode(&decode, ncomp);

    // ImageType 4 with non-identity decode: must compare MaskColor against raw
    // (un-decoded) samples per PLRM spec. Pre-apply mask and push as RGBA since
    // the render-time comparison would use decoded samples.
    if mask_color_param.is_some() && needs_decode {
        let mut rgba = samples_to_rgba(ctx, &samples, width, height, ncomp, &decode);
        if let Some(ref mc) = mask_color_param {
            apply_mask_color_raw(&mut rgba, &samples, width, height, ncomp, mc);
        }
        draw_image_to_device(
            ctx,
            rgba,
            width,
            height,
            ImageColorSpace::PreconvertedRGBA,
            &image_matrix,
            None,
        );
    } else if let Some(img_cs) = capture_image_color_space(ctx) {
        // Apply decode to samples if non-identity (for non-indexed spaces)
        let final_samples = if needs_decode {
            apply_decode(&samples, ncomp, &decode)
        } else {
            samples
        };
        draw_image_to_device(
            ctx,
            final_samples,
            width,
            height,
            img_cs,
            &image_matrix,
            mask_color_param,
        );
    } else {
        // CIE DEF/DEFG, Separation, DeviceN: convert to RGB at op time
        let mut rgba = samples_to_rgba(ctx, &samples, width, height, ncomp, &decode);
        if let Some(ref mc) = mask_color_param {
            apply_mask_color_raw(&mut rgba, &samples, width, height, ncomp, mc);
            draw_image_to_device(
                ctx,
                rgba,
                width,
                height,
                ImageColorSpace::PreconvertedRGBA,
                &image_matrix,
                None,
            );
        } else {
            let rgb = rgba_to_rgb(&rgba);
            draw_image_to_device(
                ctx,
                rgb,
                width,
                height,
                ImageColorSpace::DeviceRGB,
                &image_matrix,
                None,
            );
        }
    }
    Ok(())
}

/// Type 3 masked image: outer dict has InterleaveType, DataDict, MaskDict.
fn image_type3_form(
    ctx: &mut Context,
    outer_dict: stet_core::object::EntityId,
) -> Result<(), PsError> {
    // Extract outer dict keys
    let interleave_type =
        dict_get_int(ctx, outer_dict, b"InterleaveType").ok_or(PsError::TypeCheck)?;
    if !matches!(interleave_type, 1 | 2 | 3) {
        return Err(PsError::RangeCheck);
    }

    let data_dict_obj = dict_get_obj(ctx, outer_dict, b"DataDict").ok_or(PsError::TypeCheck)?;
    let mask_dict_obj = dict_get_obj(ctx, outer_dict, b"MaskDict").ok_or(PsError::TypeCheck)?;

    let data_dict = match data_dict_obj.value {
        PsValue::Dict(e) => e,
        _ => return Err(PsError::TypeCheck),
    };
    let mask_dict = match mask_dict_obj.value {
        PsValue::Dict(e) => e,
        _ => return Err(PsError::TypeCheck),
    };

    // Extract DataDict parameters
    let img_w = dict_get_int(ctx, data_dict, b"Width").ok_or(PsError::Undefined)? as u32;
    let img_h = dict_get_int(ctx, data_dict, b"Height").ok_or(PsError::Undefined)? as u32;
    let img_bps =
        dict_get_int(ctx, data_dict, b"BitsPerComponent").ok_or(PsError::Undefined)? as u32;
    let img_matrix = dict_get_matrix(ctx, data_dict, b"ImageMatrix").ok_or(PsError::Undefined)?;
    let img_decode = dict_get_decode(ctx, data_dict).unwrap_or_default();
    let img_data_source = dict_get_obj(ctx, data_dict, b"DataSource");
    let img_multi = dict_get_obj(ctx, data_dict, b"MultipleDataSources")
        .and_then(|o| match o.value {
            PsValue::Bool(b) => Some(b),
            _ => None,
        })
        .unwrap_or(false);

    // Extract MaskDict parameters
    let mask_w = dict_get_int(ctx, mask_dict, b"Width").ok_or(PsError::Undefined)? as u32;
    let mask_h = dict_get_int(ctx, mask_dict, b"Height").ok_or(PsError::Undefined)? as u32;
    let mask_bps = dict_get_int(ctx, mask_dict, b"BitsPerComponent").unwrap_or(1) as u32;
    let mask_decode = dict_get_decode(ctx, mask_dict).unwrap_or_else(|| vec![0.0, 1.0]);
    let mask_data_source = dict_get_obj(ctx, mask_dict, b"DataSource");

    // Mask polarity from Decode (PLRM: decoded value 0 = paint, 1 = mask out):
    // [0 1] → raw 0 paints (polarity=true), [1 0] → raw 1 paints (polarity=false)
    let mask_polarity = mask_decode.first().copied().unwrap_or(0.0) < 0.5;

    // Determine ncomp from image Decode array or color space
    let ncomp = if img_decode.len() >= 2 {
        (img_decode.len() / 2) as u32
    } else {
        match ctx.gstate.color_space {
            ColorSpace::DeviceGray | ColorSpace::Indexed { .. } | ColorSpace::CIEBasedA { .. } => 1,
            ColorSpace::DeviceRGB
            | ColorSpace::CIEBasedABC { .. }
            | ColorSpace::CIEBasedDEF { .. } => 3,
            ColorSpace::DeviceCMYK | ColorSpace::CIEBasedDEFG { .. } => 4,
            ColorSpace::ICCBased { n, .. } => n,
            ColorSpace::Separation { .. } => 1,
            ColorSpace::DeviceN { num_colorants, .. } => num_colorants,
        }
    };

    let img_decode = if img_decode.is_empty() {
        (0..ncomp).flat_map(|_| [0.0, 1.0]).collect()
    } else {
        img_decode
    };

    // Pop the outer dict from operand stack
    ctx.o_stack.pop()?;

    // Read image and mask data based on InterleaveType
    let (img_data, mask_data) = match interleave_type {
        1 => read_type3_interleave1(
            ctx,
            img_data_source.ok_or(PsError::Undefined)?,
            img_multi,
            img_w,
            img_h,
            img_bps,
            ncomp,
        )?,
        2 => read_type3_interleave2(
            ctx,
            img_data_source.ok_or(PsError::Undefined)?,
            img_w,
            img_h,
            img_bps,
            ncomp,
            mask_w,
            mask_h,
        )?,
        3 => read_type3_interleave3(
            ctx,
            img_data_source.ok_or(PsError::Undefined)?,
            img_multi,
            mask_data_source.ok_or(PsError::Undefined)?,
            img_w,
            img_h,
            img_bps,
            ncomp,
            mask_w,
            mask_h,
            mask_bps,
        )?,
        _ => unreachable!(),
    };

    // Type 3 masked images: convert to RGBA at op time (stencil mask
    // needs alpha compositing, and these images are relatively rare)
    let indexed = matches!(ctx.gstate.color_space, ColorSpace::Indexed { .. });
    let samples = unpack_samples(&img_data, img_w, img_h, img_bps, ncomp, indexed);
    let mut rgba = samples_to_rgba(ctx, &samples, img_w, img_h, ncomp, &img_decode);

    // Apply stencil mask to alpha channel
    apply_stencil_mask(
        &mut rgba,
        img_w,
        img_h,
        &mask_data,
        mask_w,
        mask_h,
        mask_bps,
        mask_polarity,
    );

    draw_image_to_device(
        ctx,
        rgba,
        img_w,
        img_h,
        ImageColorSpace::PreconvertedRGBA,
        &img_matrix,
        None,
    );
    Ok(())
}

/// InterleaveType 1: mask and image samples interleaved per pixel.
/// Data layout: [Mask, C1, C2, ..., Mask, C1, C2, ...] per pixel.
/// Mask uses same BPS as image. Single DataSource in DataDict.
fn read_type3_interleave1(
    ctx: &mut Context,
    data_source: PsObject,
    multi: bool,
    img_w: u32,
    img_h: u32,
    img_bps: u32,
    ncomp: u32,
) -> Result<(Vec<u8>, Vec<u8>), PsError> {
    let total_comp = 1 + ncomp; // mask + image components
    let bits_per_row = img_w as usize * total_comp as usize * img_bps as usize;
    let bytes_per_row = bits_per_row.div_ceil(8);
    let total_bytes = bytes_per_row * img_h as usize;

    let raw = if multi {
        read_multi_source_data(ctx, data_source, total_comp, img_w, img_h, img_bps)?
    } else if is_procedure(&data_source) {
        collect_proc_data(ctx, data_source, total_bytes)?
    } else {
        read_image_data(ctx, data_source, total_bytes)?
    };

    // Separate mask and image data
    if img_bps == 8 {
        // Simple byte-level separation
        let pixel_count = (img_w * img_h) as usize;
        let mut img_data = Vec::with_capacity(pixel_count * ncomp as usize);
        let mut mask_data = Vec::with_capacity(pixel_count);

        for row in 0..img_h as usize {
            let row_start = row * bytes_per_row;
            for col in 0..img_w as usize {
                let pixel_start = row_start + col * total_comp as usize;
                // First sample is mask
                mask_data.push(*raw.get(pixel_start).unwrap_or(&0));
                // Remaining samples are image components
                for c in 0..ncomp as usize {
                    img_data.push(*raw.get(pixel_start + 1 + c).unwrap_or(&0));
                }
            }
        }
        Ok((img_data, mask_data))
    } else {
        // Sub-byte or 12-bit: extract at bit level
        let img_bits_per_row = img_w as usize * ncomp as usize * img_bps as usize;
        let img_bytes_per_row = img_bits_per_row.div_ceil(8);
        let mask_bits_per_row = img_w as usize * img_bps as usize;
        let mask_bytes_per_row = mask_bits_per_row.div_ceil(8);

        let mut img_data = vec![0u8; img_bytes_per_row * img_h as usize];
        let mut mask_data = vec![0u8; mask_bytes_per_row * img_h as usize];

        for row in 0..img_h as usize {
            let src_row_start = row * bytes_per_row;
            let mut src_bit = 0usize;
            let mut img_bit = 0usize;
            let mut mask_bit = 0usize;

            for _col in 0..img_w as usize {
                // Extract mask sample
                let mask_val = extract_bits(&raw, src_row_start, src_bit, img_bps as usize);
                set_bits(
                    &mut mask_data,
                    row * mask_bytes_per_row,
                    mask_bit,
                    img_bps as usize,
                    mask_val,
                );
                src_bit += img_bps as usize;
                mask_bit += img_bps as usize;

                // Extract image samples
                for _ in 0..ncomp {
                    let val = extract_bits(&raw, src_row_start, src_bit, img_bps as usize);
                    set_bits(
                        &mut img_data,
                        row * img_bytes_per_row,
                        img_bit,
                        img_bps as usize,
                        val,
                    );
                    src_bit += img_bps as usize;
                    img_bit += img_bps as usize;
                }
            }
        }
        Ok((img_data, mask_data))
    }
}

/// InterleaveType 2: data interleaved by row blocks.
/// Block structure: [mask rows][image rows] repeated.
/// Mask is always 1 BPS.
fn read_type3_interleave2(
    ctx: &mut Context,
    data_source: PsObject,
    img_w: u32,
    img_h: u32,
    img_bps: u32,
    ncomp: u32,
    mask_w: u32,
    mask_h: u32,
) -> Result<(Vec<u8>, Vec<u8>), PsError> {
    let mask_bytes_per_row = (mask_w as usize).div_ceil(8); // 1 bps
    let img_bits_per_row = img_w as usize * ncomp as usize * img_bps as usize;
    let img_bytes_per_row = img_bits_per_row.div_ceil(8);

    // Determine block structure from height ratio
    let (mask_rows_per_block, img_rows_per_block, num_blocks) = if img_h >= mask_h {
        let ratio = if mask_h > 0 { img_h / mask_h } else { 1 };
        (1usize, ratio as usize, mask_h as usize)
    } else {
        let ratio = if img_h > 0 { mask_h / img_h } else { 1 };
        (ratio as usize, 1usize, img_h as usize)
    };

    let bytes_per_block =
        mask_rows_per_block * mask_bytes_per_row + img_rows_per_block * img_bytes_per_row;
    let total_bytes = bytes_per_block * num_blocks;

    let raw = if is_procedure(&data_source) {
        collect_proc_data(ctx, data_source, total_bytes)?
    } else {
        read_image_data(ctx, data_source, total_bytes)?
    };

    // Separate mask and image rows
    let mut mask_data = Vec::with_capacity(mask_bytes_per_row * mask_h as usize);
    let mut img_data = Vec::with_capacity(img_bytes_per_row * img_h as usize);

    let mut offset = 0;
    for _ in 0..num_blocks {
        for _ in 0..mask_rows_per_block {
            let end = (offset + mask_bytes_per_row).min(raw.len());
            if offset < raw.len() {
                mask_data.extend_from_slice(&raw[offset..end]);
            }
            offset += mask_bytes_per_row;
        }
        for _ in 0..img_rows_per_block {
            let end = (offset + img_bytes_per_row).min(raw.len());
            if offset < raw.len() {
                img_data.extend_from_slice(&raw[offset..end]);
            }
            offset += img_bytes_per_row;
        }
    }

    Ok((img_data, mask_data))
}

/// InterleaveType 3: separate data sources for image and mask.
fn read_type3_interleave3(
    ctx: &mut Context,
    img_data_source: PsObject,
    img_multi: bool,
    mask_data_source: PsObject,
    img_w: u32,
    img_h: u32,
    img_bps: u32,
    ncomp: u32,
    mask_w: u32,
    mask_h: u32,
    mask_bps: u32,
) -> Result<(Vec<u8>, Vec<u8>), PsError> {
    // Read image data
    let img_bits_per_row = img_w as usize * ncomp as usize * img_bps as usize;
    let img_bytes_per_row = img_bits_per_row.div_ceil(8);
    let img_total = img_bytes_per_row * img_h as usize;

    let img_data = if img_multi {
        read_multi_source_data(ctx, img_data_source, ncomp, img_w, img_h, img_bps)?
    } else if is_procedure(&img_data_source) {
        collect_proc_data(ctx, img_data_source, img_total)?
    } else {
        read_image_data(ctx, img_data_source, img_total)?
    };

    // Read mask data
    let mask_bits_per_row = mask_w as usize * mask_bps as usize;
    let mask_bytes_per_row = mask_bits_per_row.div_ceil(8);
    let mask_total = mask_bytes_per_row * mask_h as usize;

    let mask_data = if is_procedure(&mask_data_source) {
        collect_proc_data(ctx, mask_data_source, mask_total)?
    } else {
        read_image_data(ctx, mask_data_source, mask_total)?
    };

    Ok((img_data, mask_data))
}

/// Apply a stencil mask to RGBA data, setting alpha=0 where mask says "don't paint".
fn apply_stencil_mask(
    rgba: &mut [u8],
    img_w: u32,
    img_h: u32,
    mask_data: &[u8],
    mask_w: u32,
    mask_h: u32,
    mask_bps: u32,
    polarity: bool, // true: zero=paint (Decode [0,1]), false: nonzero=paint (Decode [1,0])
) {
    let mask_samples_per_row = mask_w as usize;
    let mask_bits_per_row = mask_samples_per_row * mask_bps as usize;
    let mask_bytes_per_row = mask_bits_per_row.div_ceil(8);

    for y in 0..img_h as usize {
        // Map image row to mask row (scale if dimensions differ)
        let mask_y = if mask_h == img_h {
            y
        } else {
            (y * mask_h as usize) / img_h as usize
        };

        for x in 0..img_w as usize {
            let mask_x = if mask_w == img_w {
                x
            } else {
                (x * mask_w as usize) / img_w as usize
            };

            // Extract mask sample value
            let sample = if mask_bps == 1 {
                let byte_idx = mask_y * mask_bytes_per_row + mask_x / 8;
                let bit_offset = 7 - (mask_x % 8);
                let byte_val = mask_data.get(byte_idx).copied().unwrap_or(0);
                (byte_val >> bit_offset) & 1
            } else if mask_bps == 8 {
                let idx = mask_y * mask_w as usize + mask_x;
                mask_data.get(idx).copied().unwrap_or(0)
            } else {
                // General case for other BPS
                extract_bits(
                    mask_data,
                    mask_y * mask_bytes_per_row,
                    mask_x * mask_bps as usize,
                    mask_bps as usize,
                ) as u8
            };

            // Determine if this pixel should paint (PLRM: decoded 0 = paint)
            // polarity=true (Decode [0,1]): raw 0 paints; polarity=false (Decode [1,0]): raw 1 paints
            let paint = if polarity { sample == 0 } else { sample != 0 };

            if !paint {
                let pi = (y * img_w as usize + x) * 4;
                if pi + 3 < rgba.len() {
                    // Zero all channels for valid premultiplied alpha
                    rgba[pi] = 0;
                    rgba[pi + 1] = 0;
                    rgba[pi + 2] = 0;
                    rgba[pi + 3] = 0;
                }
            }
        }
    }
}

/// Extract bits from a byte buffer at a specific bit position.
fn extract_bits(data: &[u8], byte_offset: usize, bit_pos: usize, num_bits: usize) -> u16 {
    let abs_bit = byte_offset * 8 + bit_pos;
    let byte_idx = abs_bit / 8;
    let bit_offset = abs_bit % 8;

    if byte_idx >= data.len() {
        return 0;
    }

    if num_bits <= 8 - bit_offset {
        // Fits within one byte
        let shift = 8 - bit_offset - num_bits;
        let mask = ((1u16 << num_bits) - 1) as u8;
        ((data[byte_idx] >> shift) & mask) as u16
    } else {
        // Spans two bytes
        let hi = data[byte_idx] as u16;
        let lo = data.get(byte_idx + 1).copied().unwrap_or(0) as u16;
        let combined = (hi << 8) | lo;
        let shift = 16 - bit_offset - num_bits;
        let mask = (1u16 << num_bits) - 1;
        (combined >> shift) & mask
    }
}

/// Set bits in a byte buffer at a specific bit position.
fn set_bits(data: &mut [u8], byte_offset: usize, bit_pos: usize, num_bits: usize, value: u16) {
    let abs_bit = byte_offset * 8 + bit_pos;
    let byte_idx = abs_bit / 8;
    let bit_offset = abs_bit % 8;

    if byte_idx >= data.len() {
        return;
    }

    if num_bits <= 8 - bit_offset {
        let shift = 8 - bit_offset - num_bits;
        let mask = ((1u16 << num_bits) - 1) as u8;
        data[byte_idx] &= !(mask << shift);
        data[byte_idx] |= (value as u8 & mask) << shift;
    } else if byte_idx + 1 < data.len() {
        let shift = 16 - bit_offset - num_bits;
        let mask = (1u16 << num_bits) - 1;
        let combined = ((data[byte_idx] as u16) << 8) | data[byte_idx + 1] as u16;
        let cleared = combined & !(mask << shift);
        let result = cleared | ((value & mask) << shift);
        data[byte_idx] = (result >> 8) as u8;
        data[byte_idx + 1] = result as u8;
    }
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
    if width <= 0 || height <= 0 {
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
        let cs = ImageColorSpace::Mask { color, polarity };
        draw_image_to_device(
            ctx,
            data,
            width as u32,
            height as u32,
            cs,
            &image_matrix,
            None,
        );
        return Ok(());
    }

    let data = read_image_data(ctx, src_obj, total_bytes)?;

    ctx.o_stack.pop()?; // src
    ctx.o_stack.pop()?; // matrix
    ctx.o_stack.pop()?; // polarity
    ctx.o_stack.pop()?; // height
    ctx.o_stack.pop()?; // width

    let color = ctx.gstate.color.clone();
    let cs = ImageColorSpace::Mask { color, polarity };
    draw_image_to_device(
        ctx,
        data,
        width as u32,
        height as u32,
        cs,
        &image_matrix,
        None,
    );
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
    let cs = ImageColorSpace::Mask { color, polarity };
    draw_image_to_device(ctx, data, width, height, cs, &image_matrix, None);
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
    if width <= 0 || height <= 0 {
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

            let samples = unpack_samples(
                &data,
                width as u32,
                height as u32,
                bps as u32,
                ncomp as u32,
                false,
            );
            let cs = colorimage_color_space(ncomp);
            draw_image_to_device(
                ctx,
                samples,
                width as u32,
                height as u32,
                cs,
                &image_matrix,
                None,
            );
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
            let samples = unpack_samples(
                &data,
                width as u32,
                height as u32,
                bps as u32,
                ncomp as u32,
                false,
            );
            let cs = colorimage_color_space(ncomp);
            draw_image_to_device(
                ctx,
                samples,
                width as u32,
                height as u32,
                cs,
                &image_matrix,
                None,
            );
            return Ok(());
        }
        read_image_data(ctx, src_obj, total_bytes)?
    };

    // Pop all
    for _ in 0..(w_idx + 1) {
        ctx.o_stack.pop()?;
    }

    let samples = unpack_samples(
        &data,
        width as u32,
        height as u32,
        bps as u32,
        ncomp as u32,
        false,
    );
    let cs = colorimage_color_space(ncomp);
    draw_image_to_device(
        ctx,
        samples,
        width as u32,
        height as u32,
        cs,
        &image_matrix,
        None,
    );
    Ok(())
}

/// Map colorimage ncomp to ImageColorSpace.
fn colorimage_color_space(ncomp: i32) -> ImageColorSpace {
    match ncomp {
        1 => ImageColorSpace::DeviceGray,
        3 => ImageColorSpace::DeviceRGB,
        4 => ImageColorSpace::DeviceCMYK,
        _ => ImageColorSpace::DeviceRGB,
    }
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
            if (len as usize) < bytes_needed {
                return Err(PsError::IOError);
            }
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
fn unpack_samples(
    raw: &[u8],
    width: u32,
    height: u32,
    bps: u32,
    ncomp: u32,
    indexed: bool,
) -> Vec<u8> {
    if bps == 8 {
        return raw.to_vec();
    }

    let total_samples = (width as usize) * (height as usize) * (ncomp as usize);
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
    ctx: &mut Context,
    samples: &[u8],
    width: u32,
    height: u32,
    ncomp: u32,
    decode: &[f64],
) -> Vec<u8> {
    let pixel_count = width as usize * height as usize;
    let mut rgba = vec![255u8; pixel_count * 4]; // Pre-fill alpha = 255

    // Check for Indexed color space
    if let ColorSpace::Indexed {
        ref base,
        hival,
        ref lookup,
        ..
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
                    DeviceColor::from_cmyk_icc(c, m, y, k, &mut ctx.icc_cache)
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

    // ICC-based bulk image conversion
    if let ColorSpace::ICCBased {
        profile_hash: Some(ref hash),
        ..
    } = ctx.gstate.color_space
    {
        let hash = *hash;
        // Apply decode to get 8-bit samples, then bulk-convert
        let decoded_samples = if is_identity_decode(decode, ncomp) {
            // Fast path: decode is identity [0 1 0 1 ...], use raw samples
            None
        } else {
            // Apply decode mapping
            let mut decoded = vec![0u8; pixel_count * ncomp as usize];
            for i in 0..pixel_count {
                for c in 0..ncomp as usize {
                    let raw =
                        samples.get(i * ncomp as usize + c).copied().unwrap_or(0) as f64 / 255.0;
                    let d_min = decode.get(c * 2).copied().unwrap_or(0.0);
                    let d_max = decode.get(c * 2 + 1).copied().unwrap_or(1.0);
                    let val = d_min + raw * (d_max - d_min);
                    decoded[i * ncomp as usize + c] = (val.clamp(0.0, 1.0) * 255.0).round() as u8;
                }
            }
            Some(decoded)
        };
        let src = decoded_samples.as_deref().unwrap_or(samples);
        if let Some(rgb_data) = ctx.icc_cache.convert_image_8bit(&hash, src, pixel_count) {
            // Convert RGB to RGBA
            for i in 0..pixel_count {
                let pi = i * 4;
                let ri = i * 3;
                rgba[pi] = rgb_data[ri];
                rgba[pi + 1] = rgb_data[ri + 1];
                rgba[pi + 2] = rgb_data[ri + 2];
                // alpha stays 255
            }
            return rgba;
        }
    }

    // System CMYK profile bulk conversion for DeviceCMYK images
    if matches!(ctx.gstate.color_space, ColorSpace::DeviceCMYK) && ncomp == 4 {
        if let Some(hash) = ctx.icc_cache.default_cmyk_hash().copied() {
            let decoded_samples = if is_identity_decode(decode, ncomp) {
                None
            } else {
                let mut decoded = vec![0u8; pixel_count * 4];
                for i in 0..pixel_count {
                    for c in 0..4usize {
                        let raw = samples.get(i * 4 + c).copied().unwrap_or(0) as f64 / 255.0;
                        let d_min = decode.get(c * 2).copied().unwrap_or(0.0);
                        let d_max = decode.get(c * 2 + 1).copied().unwrap_or(1.0);
                        let val = d_min + raw * (d_max - d_min);
                        decoded[i * 4 + c] = (val.clamp(0.0, 1.0) * 255.0).round() as u8;
                    }
                }
                Some(decoded)
            };
            let src = decoded_samples.as_deref().unwrap_or(samples);
            if let Some(rgb_data) = ctx.icc_cache.convert_image_8bit(&hash, src, pixel_count) {
                for i in 0..pixel_count {
                    let pi = i * 4;
                    let ri = i * 3;
                    rgba[pi] = rgb_data[ri];
                    rgba[pi + 1] = rgb_data[ri + 1];
                    rgba[pi + 2] = rgb_data[ri + 2];
                }
                return rgba;
            }
        }
    }

    // CIE-based image conversion: run each pixel through CIE pipeline
    let cie_color_space = ctx.gstate.color_space.clone();
    match &cie_color_space {
        ColorSpace::CIEBasedABC { params, .. } => {
            let params = params.clone();
            for i in 0..pixel_count {
                let si = i * 3;
                let pi = i * 4;
                let mut comp = [0.0f64; 3];
                for c in 0..3 {
                    let raw = samples.get(si + c).copied().unwrap_or(0) as f64 / 255.0;
                    let d_min = decode.get(c * 2).copied().unwrap_or(0.0);
                    let d_max = decode.get(c * 2 + 1).copied().unwrap_or(1.0);
                    comp[c] = (d_min + raw * (d_max - d_min)).clamp(0.0, 1.0);
                }
                let color = DeviceColor::from_cie_abc(comp[0], comp[1], comp[2], &params);
                rgba[pi] = (color.r * 255.0).round().clamp(0.0, 255.0) as u8;
                rgba[pi + 1] = (color.g * 255.0).round().clamp(0.0, 255.0) as u8;
                rgba[pi + 2] = (color.b * 255.0).round().clamp(0.0, 255.0) as u8;
            }
            return rgba;
        }
        ColorSpace::CIEBasedA { params, .. } => {
            let params = params.clone();
            for i in 0..pixel_count {
                let pi = i * 4;
                let raw = samples.get(i).copied().unwrap_or(0) as f64 / 255.0;
                let d_min = decode.first().copied().unwrap_or(0.0);
                let d_max = decode.get(1).copied().unwrap_or(1.0);
                let val = (d_min + raw * (d_max - d_min)).clamp(0.0, 1.0);
                let color = DeviceColor::from_cie_a(val, &params);
                rgba[pi] = (color.r * 255.0).round().clamp(0.0, 255.0) as u8;
                rgba[pi + 1] = (color.g * 255.0).round().clamp(0.0, 255.0) as u8;
                rgba[pi + 2] = (color.b * 255.0).round().clamp(0.0, 255.0) as u8;
            }
            return rgba;
        }
        ColorSpace::CIEBasedDEF { params, .. } => {
            let params = params.clone();
            for i in 0..pixel_count {
                let si = i * 3;
                let pi = i * 4;
                let mut comp = [0.0f64; 3];
                for c in 0..3 {
                    let raw = samples.get(si + c).copied().unwrap_or(0) as f64 / 255.0;
                    let d_min = decode.get(c * 2).copied().unwrap_or(0.0);
                    let d_max = decode.get(c * 2 + 1).copied().unwrap_or(1.0);
                    comp[c] = (d_min + raw * (d_max - d_min)).clamp(0.0, 1.0);
                }
                let color = DeviceColor::from_cie_def(comp[0], comp[1], comp[2], &params);
                rgba[pi] = (color.r * 255.0).round().clamp(0.0, 255.0) as u8;
                rgba[pi + 1] = (color.g * 255.0).round().clamp(0.0, 255.0) as u8;
                rgba[pi + 2] = (color.b * 255.0).round().clamp(0.0, 255.0) as u8;
            }
            return rgba;
        }
        ColorSpace::CIEBasedDEFG { params, .. } => {
            let params = params.clone();
            for i in 0..pixel_count {
                let si = i * 4;
                let pi = i * 4;
                let mut comp = [0.0f64; 4];
                for c in 0..4 {
                    let raw = samples.get(si + c).copied().unwrap_or(0) as f64 / 255.0;
                    let d_min = decode.get(c * 2).copied().unwrap_or(0.0);
                    let d_max = decode.get(c * 2 + 1).copied().unwrap_or(1.0);
                    comp[c] = (d_min + raw * (d_max - d_min)).clamp(0.0, 1.0);
                }
                let color = DeviceColor::from_cie_defg(comp[0], comp[1], comp[2], comp[3], &params);
                rgba[pi] = (color.r * 255.0).round().clamp(0.0, 255.0) as u8;
                rgba[pi + 1] = (color.g * 255.0).round().clamp(0.0, 255.0) as u8;
                rgba[pi + 2] = (color.b * 255.0).round().clamp(0.0, 255.0) as u8;
            }
            return rgba;
        }
        _ => {}
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
                let color = DeviceColor::from_cmyk_icc(
                    cmyk[0],
                    cmyk[1],
                    cmyk[2],
                    cmyk[3],
                    &mut ctx.icc_cache,
                );
                rgba[pi] = (color.r * 255.0).round().clamp(0.0, 255.0) as u8;
                rgba[pi + 1] = (color.g * 255.0).round().clamp(0.0, 255.0) as u8;
                rgba[pi + 2] = (color.b * 255.0).round().clamp(0.0, 255.0) as u8;
            }
            _ => {}
        }
    }

    rgba
}

/// Apply ImageType 4 MaskColor against raw (un-decoded) samples, setting
/// alpha=0 in RGBA output for matching pixels.
fn apply_mask_color_raw(
    rgba: &mut [u8],
    raw_samples: &[u8],
    width: u32,
    height: u32,
    ncomp: u32,
    mask_color: &[u8],
) {
    let pixel_count = width as usize * height as usize;
    let nc = ncomp as usize;
    let is_range = mask_color.len() == 2 * nc;

    for i in 0..pixel_count {
        let si = i * nc;
        let matched = if is_range {
            (0..nc).all(|c| {
                let sample = raw_samples.get(si + c).copied().unwrap_or(0);
                let min_val = mask_color.get(c * 2).copied().unwrap_or(0);
                let max_val = mask_color.get(c * 2 + 1).copied().unwrap_or(0);
                sample >= min_val && sample <= max_val
            })
        } else {
            (0..nc).all(|c| {
                let sample = raw_samples.get(si + c).copied().unwrap_or(0);
                let target = mask_color.get(c).copied().unwrap_or(0);
                sample == target
            })
        };
        if matched {
            let pi = i * 4;
            if pi + 3 < rgba.len() {
                rgba[pi] = 0;
                rgba[pi + 1] = 0;
                rgba[pi + 2] = 0;
                rgba[pi + 3] = 0;
            }
        }
    }
}

/// Apply decode array to 8-bit samples, returning decoded 8-bit samples.
fn apply_decode(samples: &[u8], ncomp: u32, decode: &[f64]) -> Vec<u8> {
    let mut result = vec![0u8; samples.len()];
    let nc = ncomp as usize;
    for (i, &raw) in samples.iter().enumerate() {
        let c = i % nc;
        let d_min = decode.get(c * 2).copied().unwrap_or(0.0);
        let d_max = decode.get(c * 2 + 1).copied().unwrap_or(1.0);
        let val = d_min + (raw as f64 / 255.0) * (d_max - d_min);
        result[i] = (val.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    result
}

/// Convert RGBA data to RGB (strip alpha channel).
fn rgba_to_rgb(rgba: &[u8]) -> Vec<u8> {
    let npixels = rgba.len() / 4;
    let mut rgb = Vec::with_capacity(npixels * 3);
    for px in rgba.chunks_exact(4) {
        rgb.push(px[0]);
        rgb.push(px[1]);
        rgb.push(px[2]);
    }
    rgb
}

/// Check if a decode array is the identity mapping [0 1 0 1 ...].
fn is_identity_decode(decode: &[f64], ncomp: u32) -> bool {
    if decode.len() != (ncomp as usize * 2) {
        return decode.is_empty();
    }
    for c in 0..ncomp as usize {
        if (decode[c * 2] - 0.0).abs() > f64::EPSILON
            || (decode[c * 2 + 1] - 1.0).abs() > f64::EPSILON
        {
            return false;
        }
    }
    true
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
        let pixel_count = width as usize * height as usize;
        let mut result = Vec::with_capacity(pixel_count * ncomp as usize);
        for i in 0..pixel_count {
            for comp in comp_data {
                result.push(comp.get(i).copied().unwrap_or(0));
            }
        }
        result
    } else {
        // Sub-byte or 12-bit: unpack each component's samples, interleave, repack
        let pixel_count = width as usize * height as usize;

        // Unpack each component to individual sample values
        let unpacked: Vec<Vec<u16>> = comp_data
            .iter()
            .map(|data| unpack_raw_samples(data, width, height, bps))
            .collect();

        // Interleave: pixel0_comp0, pixel0_comp1, ..., pixel1_comp0, ...
        let interleaved_samples = ncomp as usize * pixel_count;
        let mut interleaved = Vec::with_capacity(interleaved_samples);
        for i in 0..pixel_count {
            for comp in &unpacked {
                interleaved.push(comp.get(i).copied().unwrap_or(0));
            }
        }

        // Repack into bytes
        repack_samples(&interleaved, width, height, bps, ncomp)
    }
}

/// Unpack raw sample values (without scaling to 8-bit) from packed bytes.
fn unpack_raw_samples(raw: &[u8], width: u32, height: u32, bps: u32) -> Vec<u16> {
    let samples_per_row = width as usize;
    let total_samples = samples_per_row * height as usize;
    let mut result = Vec::with_capacity(total_samples);

    if bps == 12 {
        let bits_per_row = samples_per_row * 12;
        let bytes_per_row = bits_per_row.div_ceil(8);
        for row in 0..height as usize {
            let row_start = row * bytes_per_row;
            let mut bit_pos = 0usize;
            for _ in 0..samples_per_row {
                let byte_idx = row_start + bit_pos / 8;
                let bit_offset = bit_pos % 8;
                let sample = if byte_idx + 1 >= raw.len() {
                    0
                } else if bit_offset == 0 {
                    ((raw[byte_idx] as u16) << 4) | ((raw[byte_idx + 1] as u16) >> 4)
                } else {
                    ((raw[byte_idx] as u16 & 0x0F) << 8)
                        | raw.get(byte_idx + 1).copied().unwrap_or(0) as u16
                };
                result.push(sample);
                bit_pos += 12;
            }
        }
    } else {
        let mask = ((1u16 << bps) - 1) as u8;
        let samples_per_byte = 8 / bps as usize;
        let bytes_per_row = (samples_per_row * bps as usize).div_ceil(8);
        for row in 0..height as usize {
            let row_start = row * bytes_per_row;
            for s in 0..samples_per_row {
                let byte_idx = row_start + s / samples_per_byte;
                let bit_offset = (samples_per_byte - 1 - (s % samples_per_byte)) * bps as usize;
                let byte_val = raw.get(byte_idx).copied().unwrap_or(0);
                result.push(((byte_val >> bit_offset) & mask) as u16);
            }
        }
    }
    result
}

/// Repack interleaved sample values into packed bytes.
fn repack_samples(samples: &[u16], width: u32, height: u32, bps: u32, ncomp: u32) -> Vec<u8> {
    let samples_per_row = width as usize * ncomp as usize;

    if bps == 12 {
        let bits_per_row = samples_per_row * 12;
        let bytes_per_row = bits_per_row.div_ceil(8);
        let mut result = vec![0u8; bytes_per_row * height as usize];
        for row in 0..height as usize {
            let row_start = row * bytes_per_row;
            let mut bit_pos = 0usize;
            for s in 0..samples_per_row {
                let sample_idx = row * samples_per_row + s;
                let val = samples.get(sample_idx).copied().unwrap_or(0) & 0xFFF;
                let byte_idx = row_start + bit_pos / 8;
                let bit_offset = bit_pos % 8;
                if byte_idx < result.len() {
                    if bit_offset == 0 {
                        result[byte_idx] = (val >> 4) as u8;
                        if byte_idx + 1 < result.len() {
                            result[byte_idx + 1] = ((val & 0x0F) << 4) as u8;
                        }
                    } else {
                        result[byte_idx] |= (val >> 8) as u8;
                        if byte_idx + 1 < result.len() {
                            result[byte_idx + 1] = (val & 0xFF) as u8;
                        }
                    }
                }
                bit_pos += 12;
            }
        }
        result
    } else {
        let samples_per_byte = 8 / bps as usize;
        let bytes_per_row = (samples_per_row * bps as usize).div_ceil(8);
        let mut result = vec![0u8; bytes_per_row * height as usize];
        let mask = ((1u16 << bps) - 1) as u8;
        for row in 0..height as usize {
            let row_start = row * bytes_per_row;
            for s in 0..samples_per_row {
                let sample_idx = row * samples_per_row + s;
                let val = samples.get(sample_idx).copied().unwrap_or(0) as u8 & mask;
                let byte_idx = row_start + s / samples_per_byte;
                let bit_offset = (samples_per_byte - 1 - (s % samples_per_byte)) * bps as usize;
                if byte_idx < result.len() {
                    result[byte_idx] |= val << bit_offset;
                }
            }
        }
        result
    }
}

/// Record an image draw to the display list with raw sample data.
fn draw_image_to_device(
    ctx: &mut Context,
    sample_data: Vec<u8>,
    width: u32,
    height: u32,
    color_space: ImageColorSpace,
    image_matrix: &Matrix,
    mask_color: Option<Vec<u8>>,
) {
    let params = ImageParams {
        width,
        height,
        color_space,
        ctm: ctx.gstate.ctm,
        image_matrix: *image_matrix,
        interpolate: false,
        mask_color,
        alpha: 1.0,
        blend_mode: 0,
    };
    ctx.display_list.push(DisplayElement::Image {
        sample_data,
        params,
    });
}

/// Capture the current color space as a VM-free `ImageColorSpace`.
///
/// For device spaces (Gray/RGB/CMYK), returns the direct variant.
/// For ICCBased, extracts profile bytes from the VM.
/// For Indexed, copies lookup table and recurses on base.
/// For CIE ABC/A, clones Arc params.
/// For CIE DEF/DEFG and Separation/DeviceN, returns None (caller pre-converts).
fn capture_image_color_space(ctx: &mut Context) -> Option<ImageColorSpace> {
    match ctx.gstate.color_space.clone() {
        ColorSpace::DeviceGray => Some(ImageColorSpace::DeviceGray),
        ColorSpace::DeviceRGB => Some(ImageColorSpace::DeviceRGB),
        ColorSpace::DeviceCMYK => Some(ImageColorSpace::DeviceCMYK),
        ColorSpace::ICCBased {
            dict_entity,
            n,
            profile_hash,
        } => {
            if let Some(hash) = profile_hash {
                // Extract profile bytes from dict DataSource
                let ds_key = ctx.names.find(b"DataSource")?;
                let ds_obj = ctx
                    .dicts
                    .get(dict_entity, &stet_core::dict::DictKey::Name(ds_key))?;
                let bytes = match ds_obj.value {
                    PsValue::String { entity, start, len } => {
                        ctx.strings.get(entity, start, len).to_vec()
                    }
                    _ => return None, // File sources already consumed
                };
                Some(ImageColorSpace::ICCBased {
                    n,
                    profile_hash: hash,
                    profile_data: Arc::new(bytes),
                })
            } else {
                // No profile — fall back to device equivalent
                match n {
                    1 => Some(ImageColorSpace::DeviceGray),
                    3 => Some(ImageColorSpace::DeviceRGB),
                    4 => Some(ImageColorSpace::DeviceCMYK),
                    _ => Some(ImageColorSpace::DeviceRGB),
                }
            }
        }
        ColorSpace::Indexed {
            base,
            hival,
            lookup,
            ..
        } => {
            let base_cs = match base.as_ref() {
                ColorSpace::DeviceGray => ImageColorSpace::DeviceGray,
                ColorSpace::DeviceRGB => ImageColorSpace::DeviceRGB,
                ColorSpace::DeviceCMYK => ImageColorSpace::DeviceCMYK,
                _ => ImageColorSpace::DeviceRGB, // fallback
            };
            Some(ImageColorSpace::Indexed {
                base: Box::new(base_cs),
                hival: hival as u32,
                lookup: lookup.clone(),
            })
        }
        ColorSpace::CIEBasedABC { params, .. } => Some(ImageColorSpace::CIEBasedABC {
            params: params.clone(),
        }),
        ColorSpace::CIEBasedA { params, .. } => Some(ImageColorSpace::CIEBasedA {
            params: params.clone(),
        }),
        ColorSpace::Separation {
            name,
            alt_space,
            tint_transform,
            num_alt_components,
        } => {
            let alt_ics = alt_space_to_image_cs(&alt_space);
            let table = sample_tint_transform(ctx, tint_transform, 1, num_alt_components)?;
            Some(ImageColorSpace::Separation {
                name,
                alt_space: Box::new(alt_ics),
                tint_table: Arc::new(table),
            })
        }
        ColorSpace::DeviceN {
            names,
            num_colorants,
            alt_space,
            tint_transform,
            num_alt_components,
        } => {
            // Cap at 8 colorants for sampling; fall back for more
            if num_colorants > 8 {
                return None;
            }
            let alt_ics = alt_space_to_image_cs(&alt_space);
            let table =
                sample_tint_transform(ctx, tint_transform, num_colorants, num_alt_components)?;
            Some(ImageColorSpace::DeviceN {
                names,
                alt_space: Box::new(alt_ics),
                tint_table: Arc::new(table),
            })
        }
        // CIE DEF/DEFG: pre-convert at op time
        _ => None,
    }
}

/// Map a ColorSpace to the corresponding ImageColorSpace for alt-space usage.
fn alt_space_to_image_cs(cs: &ColorSpace) -> ImageColorSpace {
    match cs {
        ColorSpace::DeviceGray => ImageColorSpace::DeviceGray,
        ColorSpace::DeviceRGB => ImageColorSpace::DeviceRGB,
        ColorSpace::DeviceCMYK => ImageColorSpace::DeviceCMYK,
        _ => ImageColorSpace::DeviceRGB, // fallback
    }
}

/// Sample a tint transform into a lookup table by evaluating it at grid points.
pub(crate) fn sample_tint_transform(
    ctx: &mut Context,
    tint_transform: PsObject,
    num_inputs: u32,
    num_outputs: u32,
) -> Option<stet_core::device::TintLookupTable> {
    let samples_per_dim = if num_inputs == 1 {
        256u32
    } else {
        match num_inputs {
            2 => 33,
            3 => 17,
            4 => 17,
            _ => 9,
        }
    };
    let total_entries = (samples_per_dim as usize).pow(num_inputs);
    let mut data = Vec::with_capacity(total_entries * num_outputs as usize);

    for idx in 0..total_entries {
        // Convert linear index to per-dimension coordinates (row-major: dim 0 varies slowest)
        let mut coords = vec![0usize; num_inputs as usize];
        let mut rem = idx;
        for d in (0..num_inputs as usize).rev() {
            coords[d] = rem % samples_per_dim as usize;
            rem /= samples_per_dim as usize;
        }
        // Push input values in order (dim 0 first = bottom of stack)
        for &coord in &coords {
            let val = coord as f64 / (samples_per_dim - 1) as f64;
            if ctx.o_stack.push(PsObject::real(val)).is_err() {
                return None;
            }
        }

        // Execute the tint transform
        if ctx.exec_sync(tint_transform).is_err() {
            return None;
        }

        // Pop output values
        let mut outputs = vec![0.0f32; num_outputs as usize];
        for i in (0..num_outputs as usize).rev() {
            outputs[i] = ctx.o_stack.pop().ok()?.as_f64().unwrap_or(0.0) as f32;
        }
        data.extend_from_slice(&outputs);
    }

    Some(stet_core::device::TintLookupTable {
        num_inputs,
        num_outputs,
        samples_per_dim,
        data,
    })
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
fn dict_get_obj(ctx: &Context, dict: stet_core::object::EntityId, name: &[u8]) -> Option<PsObject> {
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

/// Look up MaskColor array from a dict. Returns raw integer values.
/// Format: [c1 c2 ... cn] (exact match) or [min1 max1 min2 max2 ...] (range match).
fn dict_get_mask_color(
    ctx: &Context,
    dict: stet_core::object::EntityId,
    _ncomp: u32,
) -> Option<Vec<i32>> {
    let id = ctx.names.find(b"MaskColor")?;
    let val = ctx.dicts.get(dict, &DictKey::Name(id))?;
    match val.value {
        PsValue::Array { entity, start, len } => {
            let mut values = Vec::with_capacity(len as usize);
            for i in 0..len {
                let elem = ctx.arrays.get_element(entity, start + i);
                values.push(elem.as_i32()?);
            }
            Some(values)
        }
        _ => None,
    }
}

/// Scale a MaskColor value from the raw BPC range to 8-bit,
/// matching the scaling done by `unpack_samples`.
fn scale_mask_value(val: i32, bps: u32) -> i32 {
    match bps {
        8 => val,
        12 => val >> 4,
        1 | 2 | 4 => {
            let max_val = ((1i32 << bps) - 1) as f64;
            (val as f64 / max_val * 255.0).round() as i32
        }
        _ => val,
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
    fn test_samples_to_rgba_gray() {
        let mut ctx = stet_core::context::Context::new();
        let samples = vec![0, 128, 255];
        let decode = vec![0.0, 1.0];
        let rgba = samples_to_rgba(&mut ctx, &samples, 3, 1, 1, &decode);
        assert_eq!(rgba[0], 0); // R of pixel 0
        assert_eq!(rgba[4], 128); // R of pixel 1
        assert_eq!(rgba[8], 255); // R of pixel 2
        assert_eq!(rgba[3], 255); // A of pixel 0
    }

    #[test]
    fn test_samples_to_rgba_rgb() {
        let mut ctx = stet_core::context::Context::new();
        let samples = vec![255, 0, 0, 0, 255, 0]; // red, green
        let decode = vec![0.0, 1.0, 0.0, 1.0, 0.0, 1.0];
        let rgba = samples_to_rgba(&mut ctx, &samples, 2, 1, 3, &decode);
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
