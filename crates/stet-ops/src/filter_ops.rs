// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Filter operator: creates decode/encode filter files.

use stet_core::context::Context;
use stet_core::dict::DictKey;
use stet_core::error::PsError;
use stet_core::file_store::FilterKind;
use stet_core::object::{EntityId, ObjFlags, PsObject, PsValue};

/// `filter`: source [params] /filtername filter → file
///
/// Creates a filter file that decodes data from `source` through the named filter.
/// `source` can be a file or string.
///
/// Most filters: `source [dict] /filtername filter`
/// SubFileDecode: `source count (eodstring) /SubFileDecode filter`
pub fn op_filter(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }

    // Top of stack: filter name
    let name_obj = ctx.o_stack.peek(0)?;
    let filter_name_id = match name_obj.value {
        PsValue::Name(id) => id,
        _ => return Err(PsError::TypeCheck),
    };
    let filter_name = ctx.names.get_bytes(filter_name_id).to_vec();

    // SubFileDecode has special stack layout: source count (eodstring) /SubFileDecode
    if filter_name == b"SubFileDecode" {
        return create_subfile_filter(ctx);
    }

    // ReusableStreamDecode: eagerly read all source data into a seekable buffer
    if filter_name == b"ReusableStreamDecode" {
        return create_reusable_stream(ctx);
    }

    // Check for optional dict parameter (second from top, if it's a dict)
    let has_dict = if ctx.o_stack.len() >= 2 {
        matches!(ctx.o_stack.peek(1)?.value, PsValue::Dict(_))
    } else {
        false
    };

    // Determine source position on stack
    let source_idx = if has_dict { 2 } else { 1 };
    if ctx.o_stack.len() <= source_idx {
        return Err(PsError::StackUnderflow);
    }

    // Extract dict parameters if present
    let dict_entity = if has_dict {
        match ctx.o_stack.peek(1)?.value {
            PsValue::Dict(e) => Some(e),
            _ => None,
        }
    } else {
        None
    };

    // Extract filter parameters from dict
    let (predictor, columns, colors, bpc, early_change) = if let Some(de) = dict_entity {
        extract_filter_params(ctx, de)
    } else {
        (1u8, 1u32, 1u32, 8u32, 1i32)
    };

    // Validate DCT parameters BEFORE popping (per validate-before-pop rule)
    if filter_name == b"DCTEncode" {
        let de = dict_entity.ok_or(PsError::RangeCheck)?;
        // This will return error if params are invalid, before any pops
        extract_dct_encode_params(ctx, de)?;
    } else if filter_name == b"DCTDecode" {
        if let Some(de) = dict_entity {
            validate_dct_decode_params(ctx, de)?;
        }
    }

    // Validate filter name is known BEFORE popping operands
    if !is_known_filter_name(&filter_name) {
        return Err(PsError::Undefined);
    }

    // Get the data source
    let source_obj = ctx.o_stack.peek(source_idx)?;

    // Check if source is a procedure (executable array) — per PLRM 3.13.1
    if is_procedure(&source_obj) {
        let procedure = source_obj;

        // Pop all operands
        ctx.o_stack.pop()?; // filter name
        if has_dict {
            ctx.o_stack.pop()?; // dict
        }
        ctx.o_stack.pop()?; // source (procedure)

        // Collect data by calling the procedure synchronously in a loop.
        // Per PLRM, the procedure pushes a string each call; empty string = end of data.
        let is_flate = filter_name == b"FlateDecode";
        let data = collect_procedure_data(ctx, procedure, is_flate)?;
        let source_entity = ctx.files.create_string_source(data);

        let filter_entity = create_filter_by_name(
            ctx,
            &filter_name,
            source_entity,
            predictor,
            columns,
            colors,
            bpc,
            early_change,
            dict_entity,
        )?;

        let file_obj = PsObject {
            value: PsValue::File(filter_entity),
            flags: ObjFlags::literal_composite(),
        };
        ctx.o_stack.push(file_obj)?;
        return Ok(());
    }

    let source_entity = resolve_source(ctx, source_obj)?;

    // Pop all operands
    ctx.o_stack.pop()?; // filter name
    if has_dict {
        ctx.o_stack.pop()?; // dict
    }
    ctx.o_stack.pop()?; // source

    // Create the appropriate filter
    let filter_entity = create_filter_by_name(
        ctx,
        &filter_name,
        source_entity,
        predictor,
        columns,
        colors,
        bpc,
        early_change,
        dict_entity,
    )?;

    // Push the filter file object
    let file_obj = PsObject {
        value: PsValue::File(filter_entity),
        flags: ObjFlags::literal_composite(),
    };
    ctx.o_stack.push(file_obj)?;
    Ok(())
}

/// Handle SubFileDecode's special stack layout:
/// `source count (eodstring) /SubFileDecode filter`
///
/// - `count`: integer — number of EOD string occurrences before EOF (0 = first match)
/// - `eodstring`: string — end-of-data marker
/// - `source`: file or string — data source
fn create_subfile_filter(ctx: &mut Context) -> Result<(), PsError> {
    // Stack: source count (eodstring) /SubFileDecode
    if ctx.o_stack.len() < 4 {
        return Err(PsError::StackUnderflow);
    }

    // peek(0) = /SubFileDecode (already verified)
    // peek(1) = EOD string
    let eod_obj = ctx.o_stack.peek(1)?;
    let eod_string = match eod_obj.value {
        PsValue::String { entity, start, len } => ctx.strings.get(entity, start, len).to_vec(),
        _ => return Err(PsError::TypeCheck),
    };

    // peek(2) = count
    let count_obj = ctx.o_stack.peek(2)?;
    let count = match count_obj.value {
        PsValue::Int(v) => v,
        _ => return Err(PsError::TypeCheck),
    };

    // peek(3) = source
    let source_obj = ctx.o_stack.peek(3)?;
    let source_entity = resolve_source(ctx, source_obj)?;

    // Pop all 4 operands
    ctx.o_stack.pop()?; // /SubFileDecode
    ctx.o_stack.pop()?; // eod string
    ctx.o_stack.pop()?; // count
    ctx.o_stack.pop()?; // source

    // Determine byte limit: if count > 0, use as byte count; otherwise use EOD string
    let bytes_remaining = if count > 0 && eod_string.is_empty() {
        Some(count as i64)
    } else {
        None
    };

    let filter_entity = ctx.files.create_filter(
        source_entity,
        FilterKind::sub_file_decode(eod_string, count, bytes_remaining),
    );

    ctx.o_stack.push(PsObject {
        value: PsValue::File(filter_entity),
        flags: ObjFlags::literal_composite(),
    })?;
    Ok(())
}

/// Handle ReusableStreamDecode: reads all data from source into a seekable buffer.
///
/// Stack: `source [dict] /ReusableStreamDecode filter → file`
///
/// Per PLRM Level 3, this filter eagerly reads all data from the source and
/// creates a seekable, reusable stream that supports `setfileposition`.
fn create_reusable_stream(ctx: &mut Context) -> Result<(), PsError> {
    // Stack: ... source [dict] /ReusableStreamDecode
    let has_dict = if ctx.o_stack.len() >= 2 {
        matches!(ctx.o_stack.peek(1)?.value, PsValue::Dict(_))
    } else {
        false
    };

    let source_idx = if has_dict { 2 } else { 1 };
    if ctx.o_stack.len() <= source_idx {
        return Err(PsError::StackUnderflow);
    }

    let source_obj = ctx.o_stack.peek(source_idx)?;

    // Handle procedure data sources
    if is_procedure(&source_obj) {
        let procedure = source_obj;
        ctx.o_stack.pop()?; // filter name
        if has_dict {
            ctx.o_stack.pop()?; // dict
        }
        ctx.o_stack.pop()?; // source

        let data = collect_procedure_data(ctx, procedure, false)?;
        let entity = ctx.files.create_string_source(data);
        ctx.o_stack.push(PsObject {
            value: PsValue::File(entity),
            flags: ObjFlags::literal_composite(),
        })?;
        return Ok(());
    }

    let source_entity = resolve_source(ctx, source_obj)?;

    ctx.o_stack.pop()?; // filter name
    if has_dict {
        ctx.o_stack.pop()?; // dict
    }
    ctx.o_stack.pop()?; // source

    // Read all data from the source into a buffer
    let mut data = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = ctx
            .files
            .read_into(source_entity, &mut buf)
            .map_err(|_| PsError::IOError)?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
    }

    let entity = ctx.files.create_string_source(data);
    ctx.o_stack.push(PsObject {
        value: PsValue::File(entity),
        flags: ObjFlags::literal_composite(),
    })?;
    Ok(())
}

/// Resolve a data source object to a FileStore EntityId.
fn resolve_source(ctx: &mut Context, obj: PsObject) -> Result<EntityId, PsError> {
    match obj.value {
        PsValue::File(entity) => Ok(entity),
        PsValue::String { entity, start, len } => {
            // Create a string-backed file from the string data
            let data = ctx.strings.get(entity, start, len).to_vec();
            Ok(ctx.files.create_string_source(data))
        }
        _ => Err(PsError::TypeCheck),
    }
}

/// Extract filter parameters from a dict.
fn extract_filter_params(ctx: &Context, dict_entity: EntityId) -> (u8, u32, u32, u32, i32) {
    let mut predictor = 1u8;
    let mut columns = 1u32;
    let mut colors = 1u32;
    let mut bpc = 8u32;
    let mut early_change = 1i32;

    // Helper: look up an integer value by name
    let lookup_int = |ctx: &Context, name: &[u8]| -> Option<i32> {
        let id = ctx.names.find(name)?;
        let val = ctx.dicts.get(dict_entity, &DictKey::Name(id))?;
        val.as_i32()
    };

    if let Some(v) = lookup_int(ctx, b"Predictor") {
        predictor = v as u8;
    }
    if let Some(v) = lookup_int(ctx, b"Columns") {
        columns = v as u32;
    }
    if let Some(v) = lookup_int(ctx, b"Colors") {
        colors = v as u32;
    }
    if let Some(v) = lookup_int(ctx, b"BitsPerComponent") {
        bpc = v as u32;
    }
    if let Some(v) = lookup_int(ctx, b"EarlyChange") {
        early_change = v;
    }

    (predictor, columns, colors, bpc, early_change)
}

/// Parameters for DCTEncode filter creation.
struct DctEncodeParams {
    columns: u32,
    rows: u32,
    colors: u32,
    quality: u8,
    color_transform: bool,
}

/// Extract and validate DCTEncode parameters from a dict.
fn extract_dct_encode_params(ctx: &Context, dict_entity: EntityId) -> Result<DctEncodeParams, PsError> {
    let lookup_obj = |name: &[u8]| -> Option<PsObject> {
        let id = ctx.names.find(name)?;
        ctx.dicts.get(dict_entity, &DictKey::Name(id))
    };

    // Required: Columns (int, > 0)
    let columns = match lookup_obj(b"Columns") {
        Some(obj) => match obj.as_i32() {
            Some(v) if v > 0 => v as u32,
            Some(_) => return Err(PsError::RangeCheck),
            None => return Err(PsError::TypeCheck),
        },
        None => return Err(PsError::RangeCheck),
    };

    // Required: Rows (int, > 0)
    let rows = match lookup_obj(b"Rows") {
        Some(obj) => match obj.as_i32() {
            Some(v) if v > 0 => v as u32,
            Some(_) => return Err(PsError::RangeCheck),
            None => return Err(PsError::TypeCheck),
        },
        None => return Err(PsError::RangeCheck),
    };

    // Required: Colors (int, 1-4)
    let colors = match lookup_obj(b"Colors") {
        Some(obj) => match obj.as_i32() {
            Some(v) if (1..=4).contains(&v) => v as u32,
            Some(_) => return Err(PsError::RangeCheck),
            None => return Err(PsError::TypeCheck),
        },
        None => return Err(PsError::RangeCheck),
    };

    // Optional: QFactor (number, > 0.0, default 1.0)
    let qfactor = match lookup_obj(b"QFactor") {
        Some(obj) => match obj.as_f64() {
            Some(v) if v > 0.0 => v,
            Some(_) => return Err(PsError::RangeCheck),
            None => return Err(PsError::TypeCheck),
        },
        None => 1.0,
    };
    let quality = (50.0 / qfactor).round().max(1.0).min(100.0) as u8;

    // Optional: ColorTransform (int, 0 or 1)
    let color_transform = match lookup_obj(b"ColorTransform") {
        Some(obj) => match obj.as_i32() {
            Some(0) => false,
            Some(1) => true,
            Some(_) => return Err(PsError::RangeCheck),
            None => return Err(PsError::TypeCheck),
        },
        None => colors >= 3, // default: 1 for 3+ components, 0 otherwise
    };

    // Optional: HSamples (array of ints, each 1-4, length == Colors)
    if let Some(obj) = lookup_obj(b"HSamples") {
        validate_sample_array(ctx, obj, colors)?;
    }

    // Optional: VSamples (array of ints, each 1-4, length == Colors)
    if let Some(obj) = lookup_obj(b"VSamples") {
        validate_sample_array(ctx, obj, colors)?;
    }

    Ok(DctEncodeParams {
        columns,
        rows,
        colors,
        quality,
        color_transform,
    })
}

/// Validate an HSamples or VSamples array.
fn validate_sample_array(ctx: &Context, obj: PsObject, colors: u32) -> Result<(), PsError> {
    match obj.value {
        PsValue::Array { entity, start, len } => {
            if len as u32 != colors {
                return Err(PsError::RangeCheck);
            }
            for i in 0..len {
                let elem = ctx.arrays.get_element(entity, start + i);
                match elem.as_i32() {
                    Some(v) if (1..=4).contains(&v) => {}
                    Some(_) => return Err(PsError::RangeCheck),
                    None => return Err(PsError::TypeCheck),
                }
            }
            Ok(())
        }
        _ => Err(PsError::TypeCheck),
    }
}

/// Validate DCTDecode ColorTransform parameter from a dict.
fn validate_dct_decode_params(ctx: &Context, dict_entity: EntityId) -> Result<Option<bool>, PsError> {
    let id = match ctx.names.find(b"ColorTransform") {
        Some(id) => id,
        None => return Ok(None),
    };
    match ctx.dicts.get(dict_entity, &DictKey::Name(id)) {
        Some(obj) => match obj.as_i32() {
            Some(0) => Ok(Some(false)),
            Some(1) => Ok(Some(true)),
            Some(_) => Err(PsError::RangeCheck),
            None => Err(PsError::TypeCheck),
        },
        None => Ok(None),
    }
}

/// Extract CCITTFaxDecode parameters from a dict, with PLRM defaults.
fn extract_ccitt_params(
    ctx: &Context,
    dict_entity: EntityId,
) -> Result<(i32, u32, u32, bool, bool, bool, bool), PsError> {
    let lookup_obj = |name: &[u8]| -> Option<PsObject> {
        let id = ctx.names.find(name)?;
        ctx.dicts.get(dict_entity, &DictKey::Name(id))
    };

    let lookup_int = |name: &[u8], default: i32| -> Result<i32, PsError> {
        match lookup_obj(name) {
            Some(obj) => obj.as_i32().ok_or(PsError::TypeCheck),
            None => Ok(default),
        }
    };

    let lookup_bool = |name: &[u8], default: bool| -> Result<bool, PsError> {
        match lookup_obj(name) {
            Some(obj) => match obj.value {
                PsValue::Bool(v) => Ok(v),
                _ => Err(PsError::TypeCheck),
            },
            None => Ok(default),
        }
    };

    let k = lookup_int(b"K", 0)?;
    let columns = lookup_int(b"Columns", 1728)?;
    if columns < 1 {
        return Err(PsError::RangeCheck);
    }
    let rows = lookup_int(b"Rows", 0)?;
    if rows < 0 {
        return Err(PsError::RangeCheck);
    }
    let end_of_line = lookup_bool(b"EndOfLine", false)?;
    let encoded_byte_align = lookup_bool(b"EncodedByteAlign", false)?;
    let end_of_block = lookup_bool(b"EndOfBlock", true)?;
    let black_is1 = lookup_bool(b"BlackIs1", false)?;

    Ok((
        k,
        columns as u32,
        rows as u32,
        end_of_line,
        encoded_byte_align,
        end_of_block,
        black_is1,
    ))
}

/// Create a filter by name, returning the filter EntityId.
fn create_filter_by_name(
    ctx: &mut Context,
    name: &[u8],
    source: EntityId,
    predictor: u8,
    columns: u32,
    colors: u32,
    bpc: u32,
    early_change: i32,
    dict_entity: Option<EntityId>,
) -> Result<EntityId, PsError> {
    match name {
        b"ASCIIHexDecode" => Ok(ctx
            .files
            .create_filter(source, FilterKind::ascii_hex_decode())),
        b"ASCII85Decode" => Ok(ctx
            .files
            .create_filter(source, FilterKind::ascii85_decode())),
        b"RunLengthDecode" => Ok(ctx
            .files
            .create_filter(source, FilterKind::run_length_decode())),
        b"FlateDecode" => Ok(ctx.files.create_filter(
            source,
            FilterKind::flate_decode(predictor, columns, colors, bpc),
        )),
        b"LZWDecode" => Ok(ctx
            .files
            .create_filter(source, FilterKind::lzw_decode(early_change != 0))),
        b"DCTDecode" => {
            let ct = if let Some(de) = dict_entity {
                validate_dct_decode_params(ctx, de)?
            } else {
                None
            };
            Ok(ctx.files.create_dct_filter(source, ct))
        }
        b"CCITTFaxDecode" => {
            let (k, columns, rows, end_of_line, encoded_byte_align, end_of_block, black_is1) =
                if let Some(de) = dict_entity {
                    extract_ccitt_params(ctx, de)?
                } else {
                    (0, 1728, 0, false, false, true, false)
                };
            Ok(ctx.files.create_filter(
                source,
                FilterKind::ccittfax_decode(
                    k,
                    columns,
                    rows,
                    end_of_line,
                    encoded_byte_align,
                    end_of_block,
                    black_is1,
                ),
            ))
        }
        b"SubFileDecode" => Ok(ctx
            .files
            .create_filter(source, FilterKind::sub_file_decode(Vec::new(), 0, None))),
        // Encode filters
        b"ASCIIHexEncode" => Ok(ctx
            .files
            .create_encode_filter(source, FilterKind::ascii_hex_encode())),
        b"ASCII85Encode" => Ok(ctx
            .files
            .create_encode_filter(source, FilterKind::ascii85_encode())),
        b"RunLengthEncode" => Ok(ctx
            .files
            .create_encode_filter(source, FilterKind::run_length_encode())),
        b"FlateEncode" => Ok(ctx
            .files
            .create_encode_filter(source, FilterKind::flate_encode(predictor, columns, colors, bpc))),
        b"LZWEncode" => Ok(ctx
            .files
            .create_encode_filter(source, FilterKind::lzw_encode(early_change != 0))),
        b"NullEncode" => Ok(ctx
            .files
            .create_encode_filter(source, FilterKind::null_encode())),
        b"DCTEncode" => {
            let de = dict_entity.ok_or(PsError::RangeCheck)?;
            let params = extract_dct_encode_params(ctx, de)?;
            Ok(ctx.files.create_dct_encode_filter(
                source,
                params.columns,
                params.rows,
                params.colors,
                params.quality,
                params.color_transform,
            ))
        }
        _ => Err(PsError::Undefined),
    }
}

/// Check if a filter name is recognized (used for validate-before-pop).
fn is_known_filter_name(name: &[u8]) -> bool {
    matches!(
        name,
        b"ASCIIHexDecode"
            | b"ASCII85Decode"
            | b"RunLengthDecode"
            | b"FlateDecode"
            | b"LZWDecode"
            | b"DCTDecode"
            | b"SubFileDecode"
            | b"ASCIIHexEncode"
            | b"ASCII85Encode"
            | b"RunLengthEncode"
            | b"FlateEncode"
            | b"LZWEncode"
            | b"NullEncode"
            | b"DCTEncode"
            | b"CCITTFaxDecode"
    )
}

/// Check if an object is an executable array (procedure).
fn is_procedure(obj: &PsObject) -> bool {
    matches!(obj.value, PsValue::Array { .. }) && obj.flags.is_executable()
}

/// Maximum bytes to collect from a procedure data source (64 MB).
const MAX_FILTER_PROC_BYTES: usize = 64 * 1024 * 1024;

/// Collect data from a procedure data source by calling it synchronously.
/// Per PLRM 3.13.1, the procedure pushes a string each call; empty string = end of data.
/// For FlateDecode, also stops when the deflate stream is complete (handles cycling procs).
fn collect_procedure_data(
    ctx: &mut Context,
    procedure: PsObject,
    check_flate_end: bool,
) -> Result<Vec<u8>, PsError> {
    let mut data = Vec::new();

    loop {
        ctx.exec_sync(procedure)?;

        // Pop the string result
        if ctx.o_stack.is_empty() {
            break;
        }
        let result = ctx.o_stack.peek(0)?;
        match result.value {
            PsValue::String { entity, start, len } => {
                let bytes = ctx.strings.get(entity, start, len).to_vec();
                ctx.o_stack.pop()?;
                if bytes.is_empty() {
                    break; // End of data per PLRM
                }
                data.extend_from_slice(&bytes);

                // For FlateDecode, stop when the compressed stream is complete
                if check_flate_end && is_flate_stream_complete(&data) {
                    break;
                }
                if data.len() >= MAX_FILTER_PROC_BYTES {
                    break;
                }
            }
            _ => break, // Non-string result, treat as end
        }
    }

    Ok(data)
}

/// Check if collected data contains a complete zlib/deflate stream.
fn is_flate_stream_complete(data: &[u8]) -> bool {
    let mut decomp = flate2::Decompress::new(true);
    let mut out = [0u8; 8192];
    let mut pos = 0;
    loop {
        if pos >= data.len() {
            return false;
        }
        match decomp.decompress(&data[pos..], &mut out, flate2::FlushDecompress::None) {
            Ok(flate2::Status::StreamEnd) => return true,
            Ok(_) => {
                let new_pos = decomp.total_in() as usize;
                if new_pos == pos {
                    return false;
                }
                pos = new_pos;
            }
            Err(_) => return false,
        }
    }
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
    fn test_filter_ascii_hex_from_string() {
        let mut ctx = setup();

        let data = ctx.strings.allocate_from(b"48656C6C6F>");
        ctx.o_stack.push(PsObject::string(data, 11)).unwrap();

        let name_id = ctx.names.intern(b"ASCIIHexDecode");
        ctx.o_stack.push(PsObject::name_lit(name_id)).unwrap();

        op_filter(&mut ctx).unwrap();

        let file_obj = ctx.o_stack.pop().unwrap();
        let entity = match file_obj.value {
            PsValue::File(e) => e,
            _ => panic!("Expected file"),
        };

        let mut result = Vec::new();
        while let Some(b) = ctx.files.read_byte(entity).unwrap() {
            result.push(b);
        }
        assert_eq!(&result, b"Hello");
    }

    #[test]
    fn test_filter_ascii85_from_string() {
        let mut ctx = setup();

        let data = ctx.strings.allocate_from(b"9jqo^~>");
        ctx.o_stack.push(PsObject::string(data, 7)).unwrap();

        let name_id = ctx.names.intern(b"ASCII85Decode");
        ctx.o_stack.push(PsObject::name_lit(name_id)).unwrap();

        op_filter(&mut ctx).unwrap();

        let file_obj = ctx.o_stack.pop().unwrap();
        let entity = match file_obj.value {
            PsValue::File(e) => e,
            _ => panic!("Expected file"),
        };

        let mut result = Vec::new();
        while let Some(b) = ctx.files.read_byte(entity).unwrap() {
            result.push(b);
        }
        assert_eq!(&result, b"Man ");
    }

    #[test]
    fn test_filter_flate_from_string() {
        use flate2::Compression;
        use flate2::write::ZlibEncoder;
        use std::io::Write;

        let mut ctx = setup();

        let original = b"Hello PostScript";
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(original).unwrap();
        let compressed = encoder.finish().unwrap();

        let data = ctx.strings.allocate_from(&compressed);
        ctx.o_stack
            .push(PsObject::string(data, compressed.len() as u32))
            .unwrap();

        let name_id = ctx.names.intern(b"FlateDecode");
        ctx.o_stack.push(PsObject::name_lit(name_id)).unwrap();

        op_filter(&mut ctx).unwrap();

        let file_obj = ctx.o_stack.pop().unwrap();
        let entity = match file_obj.value {
            PsValue::File(e) => e,
            _ => panic!("Expected file"),
        };

        let mut result = Vec::new();
        while let Some(b) = ctx.files.read_byte(entity).unwrap() {
            result.push(b);
        }
        assert_eq!(&result, original);
    }

    #[test]
    fn test_filter_undefined() {
        let mut ctx = setup();

        let data = ctx.strings.allocate_from(b"test");
        ctx.o_stack.push(PsObject::string(data, 4)).unwrap();

        let name_id = ctx.names.intern(b"BogusFilter");
        ctx.o_stack.push(PsObject::name_lit(name_id)).unwrap();

        assert_eq!(op_filter(&mut ctx), Err(PsError::Undefined));
    }

    #[test]
    fn test_filter_typecheck_no_name() {
        let mut ctx = setup();

        let data = ctx.strings.allocate_from(b"test");
        ctx.o_stack.push(PsObject::string(data, 4)).unwrap();
        ctx.o_stack.push(PsObject::int(42)).unwrap();

        assert_eq!(op_filter(&mut ctx), Err(PsError::TypeCheck));
    }

    #[test]
    fn test_filter_chaining() {
        let mut ctx = setup();

        let hex_data = ctx.strings.allocate_from(b"48656C6C6F>");
        ctx.o_stack.push(PsObject::string(hex_data, 11)).unwrap();

        let name_id = ctx.names.intern(b"ASCIIHexDecode");
        ctx.o_stack.push(PsObject::name_lit(name_id)).unwrap();

        op_filter(&mut ctx).unwrap();

        let file_obj = ctx.o_stack.pop().unwrap();
        let entity = match file_obj.value {
            PsValue::File(e) => e,
            _ => panic!("Expected file"),
        };

        let mut result = Vec::new();
        while let Some(b) = ctx.files.read_byte(entity).unwrap() {
            result.push(b);
        }
        assert_eq!(&result, b"Hello");
    }
}
