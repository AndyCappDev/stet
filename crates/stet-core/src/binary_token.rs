// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Binary token and binary object sequence parser (PLRM §3.14).
//!
//! Handles bytes 128-159 in the PostScript input stream. Tags 128-131 are
//! binary object sequences (compact arrays of typed objects); tags 132-149
//! are individual binary tokens.

use crate::context::Context;
use crate::dict::DictKey;
use crate::error::PsError;
use crate::file_store::FileStore;
use crate::object::{EntityId, ObjFlags, PsObject, PsValue};

/// Result of parsing a binary token.
pub enum BinaryTokenResult {
    /// Individual token (types 132-149) — a single PsObject.
    Single(PsObject),
    /// Binary object sequence (types 128-131) — an executable array.
    Sequence(PsObject),
}

// ─── System name table (PLRM Appendix F, indices 0-480) ────────────────────

/// System name table. Indices 226-255 are reserved (None).
static SYSTEM_NAME_TABLE: [Option<&[u8]>; 481] = [
    // 0-9
    Some(b"abs"),
    Some(b"add"),
    Some(b"aload"),
    Some(b"anchorsearch"),
    Some(b"and"),
    Some(b"arc"),
    Some(b"arcn"),
    Some(b"arct"),
    Some(b"arcto"),
    Some(b"array"),
    // 10-19
    Some(b"ashow"),
    Some(b"astore"),
    Some(b"awidthshow"),
    Some(b"begin"),
    Some(b"bind"),
    Some(b"bitshift"),
    Some(b"ceiling"),
    Some(b"charpath"),
    Some(b"clear"),
    Some(b"cleartomark"),
    // 20-29
    Some(b"clip"),
    Some(b"clippath"),
    Some(b"closepath"),
    Some(b"concat"),
    Some(b"concatmatrix"),
    Some(b"copy"),
    Some(b"count"),
    Some(b"counttomark"),
    Some(b"currentcmykcolor"),
    Some(b"currentdash"),
    // 30-39
    Some(b"currentdict"),
    Some(b"currentfile"),
    Some(b"currentfont"),
    Some(b"currentgray"),
    Some(b"currentgstate"),
    Some(b"currenthsbcolor"),
    Some(b"currentlinecap"),
    Some(b"currentlinejoin"),
    Some(b"currentlinewidth"),
    Some(b"currentmatrix"),
    // 40-49
    Some(b"currentpoint"),
    Some(b"currentrgbcolor"),
    Some(b"currentshared"),
    Some(b"curveto"),
    Some(b"cvi"),
    Some(b"cvlit"),
    Some(b"cvn"),
    Some(b"cvr"),
    Some(b"cvrs"),
    Some(b"cvs"),
    // 50-59
    Some(b"cvx"),
    Some(b"def"),
    Some(b"defineusername"),
    Some(b"dict"),
    Some(b"div"),
    Some(b"dtransform"),
    Some(b"dup"),
    Some(b"end"),
    Some(b"eoclip"),
    Some(b"eofill"),
    // 60-69
    Some(b"eoviewclip"),
    Some(b"eq"),
    Some(b"exch"),
    Some(b"exec"),
    Some(b"exit"),
    Some(b"file"),
    Some(b"fill"),
    Some(b"findfont"),
    Some(b"flattenpath"),
    Some(b"floor"),
    // 70-79
    Some(b"flush"),
    Some(b"flushfile"),
    Some(b"for"),
    Some(b"forall"),
    Some(b"ge"),
    Some(b"get"),
    Some(b"getinterval"),
    Some(b"grestore"),
    Some(b"gsave"),
    Some(b"gstate"),
    // 80-89
    Some(b"gt"),
    Some(b"identmatrix"),
    Some(b"idiv"),
    Some(b"idtransform"),
    Some(b"if"),
    Some(b"ifelse"),
    Some(b"image"),
    Some(b"imagemask"),
    Some(b"index"),
    Some(b"ineofill"),
    // 90-99
    Some(b"infill"),
    Some(b"initviewclip"),
    Some(b"inueofill"),
    Some(b"inufill"),
    Some(b"invertmatrix"),
    Some(b"itransform"),
    Some(b"known"),
    Some(b"le"),
    Some(b"length"),
    Some(b"lineto"),
    // 100-109
    Some(b"load"),
    Some(b"loop"),
    Some(b"lt"),
    Some(b"makefont"),
    Some(b"matrix"),
    Some(b"maxlength"),
    Some(b"mod"),
    Some(b"moveto"),
    Some(b"mul"),
    Some(b"ne"),
    // 110-119
    Some(b"neg"),
    Some(b"newpath"),
    Some(b"not"),
    Some(b"null"),
    Some(b"or"),
    Some(b"pathbbox"),
    Some(b"pathforall"),
    Some(b"pop"),
    Some(b"print"),
    Some(b"printobject"),
    // 120-129
    Some(b"put"),
    Some(b"putinterval"),
    Some(b"rcurveto"),
    Some(b"read"),
    Some(b"readhexstring"),
    Some(b"readline"),
    Some(b"readstring"),
    Some(b"rectclip"),
    Some(b"rectfill"),
    Some(b"rectstroke"),
    // 130-139
    Some(b"rectviewclip"),
    Some(b"repeat"),
    Some(b"restore"),
    Some(b"rlineto"),
    Some(b"rmoveto"),
    Some(b"roll"),
    Some(b"rotate"),
    Some(b"round"),
    Some(b"save"),
    Some(b"scale"),
    // 140-149
    Some(b"scalefont"),
    Some(b"search"),
    Some(b"selectfont"),
    Some(b"setbbox"),
    Some(b"setcachedevice"),
    Some(b"setcachedevice2"),
    Some(b"setcharwidth"),
    Some(b"setcmykcolor"),
    Some(b"setdash"),
    Some(b"setfont"),
    // 150-159
    Some(b"setgray"),
    Some(b"setgstate"),
    Some(b"sethsbcolor"),
    Some(b"setlinecap"),
    Some(b"setlinejoin"),
    Some(b"setlinewidth"),
    Some(b"setmatrix"),
    Some(b"setrgbcolor"),
    Some(b"setshared"),
    Some(b"shareddict"),
    // 160-169
    Some(b"show"),
    Some(b"showpage"),
    Some(b"stop"),
    Some(b"stopped"),
    Some(b"store"),
    Some(b"string"),
    Some(b"stringwidth"),
    Some(b"stroke"),
    Some(b"strokepath"),
    Some(b"sub"),
    // 170-179
    Some(b"systemdict"),
    Some(b"token"),
    Some(b"transform"),
    Some(b"translate"),
    Some(b"truncate"),
    Some(b"type"),
    Some(b"uappend"),
    Some(b"ucache"),
    Some(b"ueofill"),
    Some(b"ufill"),
    // 180-189
    Some(b"undef"),
    Some(b"upath"),
    Some(b"userdict"),
    Some(b"ustroke"),
    Some(b"viewclip"),
    Some(b"viewclippath"),
    Some(b"where"),
    Some(b"widthshow"),
    Some(b"write"),
    Some(b"writehexstring"),
    // 190-199
    Some(b"writeobject"),
    Some(b"writestring"),
    Some(b"wtranslation"),
    Some(b"xor"),
    Some(b"xshow"),
    Some(b"xyshow"),
    Some(b"yshow"),
    Some(b"FontDirectory"),
    Some(b"SharedFontDirectory"),
    Some(b"Courier"),
    // 200-209
    Some(b"Courier-Bold"),
    Some(b"Courier-BoldOblique"),
    Some(b"Courier-Oblique"),
    Some(b"Helvetica"),
    Some(b"Helvetica-Bold"),
    Some(b"Helvetica-BoldOblique"),
    Some(b"Helvetica-Oblique"),
    Some(b"Symbol"),
    Some(b"Times-Bold"),
    Some(b"Times-BoldItalic"),
    // 210-219
    Some(b"Times-Italic"),
    Some(b"Times-Roman"),
    Some(b"execuserobject"),
    Some(b"currentcolor"),
    Some(b"currentcolorspace"),
    Some(b"currentglobal"),
    Some(b"execform"),
    Some(b"filter"),
    Some(b"findresource"),
    Some(b"globaldict"),
    // 220-225
    Some(b"makepattern"),
    Some(b"setcolor"),
    Some(b"setcolorspace"),
    Some(b"setglobal"),
    Some(b"setpagedevice"),
    Some(b"setpattern"),
    // 226-255: reserved (30 entries)
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    // 256-259
    Some(b"="),
    Some(b"=="),
    Some(b"ISOLatin1Encoding"),
    Some(b"StandardEncoding"),
    // 260-269
    Some(b"["),
    Some(b"]"),
    Some(b"atan"),
    Some(b"banddevice"),
    Some(b"bytesavailable"),
    Some(b"cachestatus"),
    Some(b"closefile"),
    Some(b"colorimage"),
    Some(b"condition"),
    Some(b"copypage"),
    // 270-279
    Some(b"cos"),
    Some(b"countdictstack"),
    Some(b"countexecstack"),
    Some(b"cshow"),
    Some(b"currentblackgeneration"),
    Some(b"currentcacheparams"),
    Some(b"currentcolorscreen"),
    Some(b"currentcolortransfer"),
    Some(b"currentcontext"),
    Some(b"currentflat"),
    // 280-289
    Some(b"currenthalftone"),
    Some(b"currenthalftonephase"),
    Some(b"currentmiterlimit"),
    Some(b"currentobjectformat"),
    Some(b"currentpacking"),
    Some(b"currentscreen"),
    Some(b"currentstrokeadjust"),
    Some(b"currenttransfer"),
    Some(b"currentundercolorremoval"),
    Some(b"defaultmatrix"),
    // 290-299
    Some(b"definefont"),
    Some(b"deletefile"),
    Some(b"detach"),
    Some(b"deviceinfo"),
    Some(b"dictstack"),
    Some(b"echo"),
    Some(b"erasepage"),
    Some(b"errordict"),
    Some(b"execstack"),
    Some(b"executeonly"),
    // 300-309
    Some(b"exp"),
    Some(b"false"),
    Some(b"filenameforall"),
    Some(b"fileposition"),
    Some(b"fork"),
    Some(b"framedevice"),
    Some(b"grestoreall"),
    Some(b"handleerror"),
    Some(b"initclip"),
    Some(b"initgraphics"),
    // 310-319
    Some(b"initmatrix"),
    Some(b"instroke"),
    Some(b"inustroke"),
    Some(b"join"),
    Some(b"kshow"),
    Some(b"ln"),
    Some(b"lock"),
    Some(b"log"),
    Some(b"mark"),
    Some(b"monitor"),
    // 320-329
    Some(b"noaccess"),
    Some(b"notify"),
    Some(b"nulldevice"),
    Some(b"packedarray"),
    Some(b"quit"),
    Some(b"rand"),
    Some(b"rcheck"),
    Some(b"readonly"),
    Some(b"realtime"),
    Some(b"renamefile"),
    // 330-339
    Some(b"renderbands"),
    Some(b"resetfile"),
    Some(b"reversepath"),
    Some(b"rootfont"),
    Some(b"rrand"),
    Some(b"run"),
    Some(b"scheck"),
    Some(b"setblackgeneration"),
    Some(b"setcachelimit"),
    Some(b"setcacheparams"),
    // 340-349
    Some(b"setcolorscreen"),
    Some(b"setcolortransfer"),
    Some(b"setfileposition"),
    Some(b"setflat"),
    Some(b"sethalftone"),
    Some(b"sethalftonephase"),
    Some(b"setmiterlimit"),
    Some(b"setobjectformat"),
    Some(b"setpacking"),
    Some(b"setscreen"),
    // 350-359
    Some(b"setstrokeadjust"),
    Some(b"settransfer"),
    Some(b"setucacheparams"),
    Some(b"setundercolorremoval"),
    Some(b"sin"),
    Some(b"sqrt"),
    Some(b"srand"),
    Some(b"stack"),
    Some(b"status"),
    Some(b"statusdict"),
    // 360-369
    Some(b"true"),
    Some(b"ucachestatus"),
    Some(b"undefinefont"),
    Some(b"usertime"),
    Some(b"ustrokepath"),
    Some(b"version"),
    Some(b"vmreclaim"),
    Some(b"vmstatus"),
    Some(b"wait"),
    Some(b"wcheck"),
    // 370-379
    Some(b"xcheck"),
    Some(b"yield"),
    Some(b"defineuserobject"),
    Some(b"undefineuserobject"),
    Some(b"UserObjects"),
    Some(b"cleardictstack"),
    Some(b"A"),
    Some(b"B"),
    Some(b"C"),
    Some(b"D"),
    // 380-389
    Some(b"E"),
    Some(b"F"),
    Some(b"G"),
    Some(b"H"),
    Some(b"I"),
    Some(b"J"),
    Some(b"K"),
    Some(b"L"),
    Some(b"M"),
    Some(b"N"),
    // 390-399
    Some(b"O"),
    Some(b"P"),
    Some(b"Q"),
    Some(b"R"),
    Some(b"S"),
    Some(b"T"),
    Some(b"U"),
    Some(b"V"),
    Some(b"W"),
    Some(b"X"),
    // 400-409
    Some(b"Y"),
    Some(b"Z"),
    Some(b"a"),
    Some(b"b"),
    Some(b"c"),
    Some(b"d"),
    Some(b"e"),
    Some(b"f"),
    Some(b"g"),
    Some(b"h"),
    // 410-419
    Some(b"i"),
    Some(b"j"),
    Some(b"k"),
    Some(b"l"),
    Some(b"m"),
    Some(b"n"),
    Some(b"o"),
    Some(b"p"),
    Some(b"q"),
    Some(b"r"),
    // 420-429
    Some(b"s"),
    Some(b"t"),
    Some(b"u"),
    Some(b"v"),
    Some(b"w"),
    Some(b"x"),
    Some(b"y"),
    Some(b"z"),
    Some(b"setvmthreshold"),
    Some(b"<<"),
    // 430-439
    Some(b">>"),
    Some(b"currentcolorrendering"),
    Some(b"currentdevparams"),
    Some(b"currentoverprint"),
    Some(b"currentpagedevice"),
    Some(b"currentsystemparams"),
    Some(b"currentuserparams"),
    Some(b"defineresource"),
    Some(b"findencoding"),
    Some(b"gcheck"),
    // 440-449
    Some(b"glyphshow"),
    Some(b"languagelevel"),
    Some(b"product"),
    Some(b"pstack"),
    Some(b"resourceforall"),
    Some(b"resourcestatus"),
    Some(b"revision"),
    Some(b"serialnumber"),
    Some(b"setcolorrendering"),
    Some(b"setdevparams"),
    // 450-459
    Some(b"setoverprint"),
    Some(b"setsystemparams"),
    Some(b"setuserparams"),
    Some(b"startjob"),
    Some(b"undefineresource"),
    Some(b"GlobalFontDirectory"),
    Some(b"ASCII85Decode"),
    Some(b"ASCII85Encode"),
    Some(b"ASCIIHexDecode"),
    Some(b"ASCIIHexEncode"),
    // 460-469
    Some(b"CCITTFaxDecode"),
    Some(b"CCITTFaxEncode"),
    Some(b"DCTDecode"),
    Some(b"DCTEncode"),
    Some(b"LZWDecode"),
    Some(b"LZWEncode"),
    Some(b"NullEncode"),
    Some(b"RunLengthDecode"),
    Some(b"RunLengthEncode"),
    Some(b"SubFileDecode"),
    // 470-479
    Some(b"CIEBasedA"),
    Some(b"CIEBasedABC"),
    Some(b"DeviceCMYK"),
    Some(b"DeviceGray"),
    Some(b"DeviceRGB"),
    Some(b"Indexed"),
    Some(b"Pattern"),
    Some(b"Separation"),
    Some(b"CIEBasedDEF"),
    Some(b"CIEBasedDEFG"),
    // 480
    Some(b"DeviceN"),
];

// ─── Slice-based parsing (for StringSource fast path) ───────────────────────

/// Parse a binary token from a byte slice. Returns the result and bytes consumed
/// (not counting the initial tag byte, which was already consumed by the tokenizer).
pub fn parse_from_slice(
    ctx: &mut Context,
    tag: u8,
    data: &[u8],
) -> Result<(BinaryTokenResult, usize), PsError> {
    match tag {
        128..=131 => parse_bos_from_slice(ctx, tag, data),
        132..=136 => parse_int_from_slice(ctx, tag, data),
        137 => parse_fixed_from_slice(ctx, data),
        138..=140 => parse_real_from_slice(tag, data),
        141 => parse_bool_from_slice(data),
        142..=144 => parse_string_from_slice(ctx, tag, data),
        145..=146 => parse_system_name_from_slice(ctx, tag, data),
        149 => parse_number_array_from_slice(ctx, data),
        147 | 148 | 150..=159 => Err(PsError::SyntaxError),
        _ => Err(PsError::SyntaxError),
    }
}

/// Parse a binary token from a file stream. The tag byte has already been read.
pub fn parse_from_stream(
    ctx: &mut Context,
    tag: u8,
    file_entity: EntityId,
) -> Result<BinaryTokenResult, PsError> {
    match tag {
        128..=131 => parse_bos_from_stream(ctx, tag, file_entity),
        132..=136 => parse_int_from_stream(ctx, tag, file_entity),
        137 => parse_fixed_from_stream(ctx, file_entity),
        138..=140 => parse_real_from_stream(tag, &mut ctx.files, file_entity),
        141 => parse_bool_from_stream(&mut ctx.files, file_entity),
        142..=144 => parse_string_from_stream(ctx, tag, file_entity),
        145..=146 => parse_system_name_from_stream(ctx, tag, file_entity),
        149 => parse_number_array_from_stream(ctx, file_entity),
        147 | 148 | 150..=159 => Err(PsError::SyntaxError),
        _ => Err(PsError::SyntaxError),
    }
}

// ─── Helper: require N bytes from slice ─────────────────────────────────────

fn need(data: &[u8], n: usize) -> Result<(), PsError> {
    if data.len() < n {
        Err(PsError::SyntaxError)
    } else {
        Ok(())
    }
}

fn read_n_bytes(files: &mut FileStore, entity: EntityId, n: usize) -> Result<Vec<u8>, PsError> {
    files
        .read_n_bytes(entity, n)
        .map_err(|_| PsError::SyntaxError)
}

// ─── Integer parsers (132-136) ──────────────────────────────────────────────

fn parse_int_from_slice(
    _ctx: &mut Context,
    tag: u8,
    data: &[u8],
) -> Result<(BinaryTokenResult, usize), PsError> {
    match tag {
        132 => {
            need(data, 4)?;
            let v = i32::from_be_bytes([data[0], data[1], data[2], data[3]]);
            Ok((BinaryTokenResult::Single(PsObject::int(v)), 4))
        }
        133 => {
            need(data, 4)?;
            let v = i32::from_le_bytes([data[0], data[1], data[2], data[3]]);
            Ok((BinaryTokenResult::Single(PsObject::int(v)), 4))
        }
        134 => {
            need(data, 2)?;
            let v = i16::from_be_bytes([data[0], data[1]]) as i32;
            Ok((BinaryTokenResult::Single(PsObject::int(v)), 2))
        }
        135 => {
            need(data, 2)?;
            let v = i16::from_le_bytes([data[0], data[1]]) as i32;
            Ok((BinaryTokenResult::Single(PsObject::int(v)), 2))
        }
        136 => {
            need(data, 1)?;
            let v = data[0] as i8 as i32;
            Ok((BinaryTokenResult::Single(PsObject::int(v)), 1))
        }
        _ => unreachable!(),
    }
}

fn parse_int_from_stream(
    ctx: &mut Context,
    tag: u8,
    file_entity: EntityId,
) -> Result<BinaryTokenResult, PsError> {
    match tag {
        132 => {
            let b = read_n_bytes(&mut ctx.files, file_entity, 4)?;
            let v = i32::from_be_bytes([b[0], b[1], b[2], b[3]]);
            Ok(BinaryTokenResult::Single(PsObject::int(v)))
        }
        133 => {
            let b = read_n_bytes(&mut ctx.files, file_entity, 4)?;
            let v = i32::from_le_bytes([b[0], b[1], b[2], b[3]]);
            Ok(BinaryTokenResult::Single(PsObject::int(v)))
        }
        134 => {
            let b = read_n_bytes(&mut ctx.files, file_entity, 2)?;
            let v = i16::from_be_bytes([b[0], b[1]]) as i32;
            Ok(BinaryTokenResult::Single(PsObject::int(v)))
        }
        135 => {
            let b = read_n_bytes(&mut ctx.files, file_entity, 2)?;
            let v = i16::from_le_bytes([b[0], b[1]]) as i32;
            Ok(BinaryTokenResult::Single(PsObject::int(v)))
        }
        136 => {
            let b = read_n_bytes(&mut ctx.files, file_entity, 1)?;
            let v = b[0] as i8 as i32;
            Ok(BinaryTokenResult::Single(PsObject::int(v)))
        }
        _ => unreachable!(),
    }
}

// ─── Fixed-point parser (137) ───────────────────────────────────────────────

fn parse_fixed_from_slice(
    _ctx: &mut Context,
    data: &[u8],
) -> Result<(BinaryTokenResult, usize), PsError> {
    need(data, 1)?;
    let r = data[0];
    let (nbytes, raw, scale) = decode_fixed_repr(r, &data[1..])?;
    let obj = if scale == 0 {
        PsObject::int(raw)
    } else {
        PsObject::real(raw as f64 / (1u64 << scale) as f64)
    };
    Ok((BinaryTokenResult::Single(obj), 1 + nbytes))
}

fn parse_fixed_from_stream(
    ctx: &mut Context,
    file_entity: EntityId,
) -> Result<BinaryTokenResult, PsError> {
    let rb = read_n_bytes(&mut ctx.files, file_entity, 1)?;
    let r = rb[0];
    let (nbytes, _) = fixed_repr_info(r)?;
    let b = read_n_bytes(&mut ctx.files, file_entity, nbytes)?;
    let (_, raw, scale) = decode_fixed_repr(r, &b)?;
    let obj = if scale == 0 {
        PsObject::int(raw)
    } else {
        PsObject::real(raw as f64 / (1u64 << scale) as f64)
    };
    Ok(BinaryTokenResult::Single(obj))
}

/// Returns (nbytes, raw_value, scale) from repr byte + data.
fn decode_fixed_repr(r: u8, data: &[u8]) -> Result<(usize, i32, u8), PsError> {
    let (nbytes, scale) = fixed_repr_info(r)?;
    need(data, nbytes)?;
    let raw = match (nbytes, r < 128) {
        (4, true) => i32::from_be_bytes([data[0], data[1], data[2], data[3]]),
        (4, false) => i32::from_le_bytes([data[0], data[1], data[2], data[3]]),
        (2, true) => i16::from_be_bytes([data[0], data[1]]) as i32,
        (2, false) => i16::from_le_bytes([data[0], data[1]]) as i32,
        _ => return Err(PsError::SyntaxError),
    };
    Ok((nbytes, raw, scale))
}

/// Returns (nbytes, scale) for a fixed-point repr byte.
fn fixed_repr_info(r: u8) -> Result<(usize, u8), PsError> {
    match r {
        0..=31 => Ok((4, r)),
        32..=47 => Ok((2, r - 32)),
        128..=159 => Ok((4, r - 128)),
        160..=175 => Ok((2, r - 160)),
        _ => Err(PsError::SyntaxError),
    }
}

// ─── Real parsers (138-140) ─────────────────────────────────────────────────

fn parse_real_from_slice(tag: u8, data: &[u8]) -> Result<(BinaryTokenResult, usize), PsError> {
    need(data, 4)?;
    let v = match tag {
        138 => f32::from_be_bytes([data[0], data[1], data[2], data[3]]),
        _ => f32::from_le_bytes([data[0], data[1], data[2], data[3]]),
    };
    Ok((BinaryTokenResult::Single(PsObject::real(v as f64)), 4))
}

fn parse_real_from_stream(
    tag: u8,
    files: &mut FileStore,
    file_entity: EntityId,
) -> Result<BinaryTokenResult, PsError> {
    let b = files
        .read_n_bytes(file_entity, 4)
        .map_err(|_| PsError::SyntaxError)?;
    let v = match tag {
        138 => f32::from_be_bytes([b[0], b[1], b[2], b[3]]),
        _ => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
    };
    Ok(BinaryTokenResult::Single(PsObject::real(v as f64)))
}

// ─── Boolean parser (141) ───────────────────────────────────────────────────

fn parse_bool_from_slice(data: &[u8]) -> Result<(BinaryTokenResult, usize), PsError> {
    need(data, 1)?;
    Ok((BinaryTokenResult::Single(PsObject::bool(data[0] != 0)), 1))
}

fn parse_bool_from_stream(
    files: &mut FileStore,
    file_entity: EntityId,
) -> Result<BinaryTokenResult, PsError> {
    let b = files
        .read_n_bytes(file_entity, 1)
        .map_err(|_| PsError::SyntaxError)?;
    Ok(BinaryTokenResult::Single(PsObject::bool(b[0] != 0)))
}

// ─── String parsers (142-144) ───────────────────────────────────────────────

fn parse_string_from_slice(
    ctx: &mut Context,
    tag: u8,
    data: &[u8],
) -> Result<(BinaryTokenResult, usize), PsError> {
    let (str_len, hdr_size) = match tag {
        142 => {
            need(data, 1)?;
            (data[0] as usize, 1)
        }
        143 => {
            need(data, 2)?;
            (u16::from_be_bytes([data[0], data[1]]) as usize, 2)
        }
        144 => {
            need(data, 2)?;
            (u16::from_le_bytes([data[0], data[1]]) as usize, 2)
        }
        _ => unreachable!(),
    };
    need(data, hdr_size + str_len)?;
    let str_data = &data[hdr_size..hdr_size + str_len];
    let obj = alloc_string(ctx, str_data);
    Ok((BinaryTokenResult::Single(obj), hdr_size + str_len))
}

fn parse_string_from_stream(
    ctx: &mut Context,
    tag: u8,
    file_entity: EntityId,
) -> Result<BinaryTokenResult, PsError> {
    let str_len = match tag {
        142 => {
            let b = read_n_bytes(&mut ctx.files, file_entity, 1)?;
            b[0] as usize
        }
        143 => {
            let b = read_n_bytes(&mut ctx.files, file_entity, 2)?;
            u16::from_be_bytes([b[0], b[1]]) as usize
        }
        144 => {
            let b = read_n_bytes(&mut ctx.files, file_entity, 2)?;
            u16::from_le_bytes([b[0], b[1]]) as usize
        }
        _ => unreachable!(),
    };
    let str_data = read_n_bytes(&mut ctx.files, file_entity, str_len)?;
    let obj = alloc_string(ctx, &str_data);
    Ok(BinaryTokenResult::Single(obj))
}

fn alloc_string(ctx: &mut Context, bytes: &[u8]) -> PsObject {
    let save_level = ctx.save_stack.current_level();
    let global = ctx.vm_alloc_mode;
    let created = ctx.save_stack.last_save_id();
    let entity = ctx
        .strings
        .allocate_with(bytes.len(), save_level, global, created);
    ctx.strings
        .get_mut(entity, 0, bytes.len() as u32)
        .copy_from_slice(bytes);
    let mut obj = PsObject::string(entity, bytes.len() as u32);
    if global {
        obj.flags = ObjFlags::new(ObjFlags::ACCESS_UNLIMITED, false, true, true);
    }
    obj
}

// ─── System name parsers (145-146) ──────────────────────────────────────────

fn parse_system_name_from_slice(
    ctx: &mut Context,
    tag: u8,
    data: &[u8],
) -> Result<(BinaryTokenResult, usize), PsError> {
    need(data, 1)?;
    let idx = data[0] as usize;
    let obj = resolve_system_name(ctx, idx, tag == 146)?;
    Ok((BinaryTokenResult::Single(obj), 1))
}

fn parse_system_name_from_stream(
    ctx: &mut Context,
    tag: u8,
    file_entity: EntityId,
) -> Result<BinaryTokenResult, PsError> {
    let b = read_n_bytes(&mut ctx.files, file_entity, 1)?;
    let idx = b[0] as usize;
    let obj = resolve_system_name(ctx, idx, tag == 146)?;
    Ok(BinaryTokenResult::Single(obj))
}

fn resolve_system_name(
    ctx: &mut Context,
    idx: usize,
    executable: bool,
) -> Result<PsObject, PsError> {
    if idx >= SYSTEM_NAME_TABLE.len() {
        return Err(PsError::Undefined);
    }
    let name_bytes = SYSTEM_NAME_TABLE[idx].ok_or(PsError::Undefined)?;
    let name_id = ctx.names.intern(name_bytes);
    if executable {
        Ok(PsObject::name_exec(name_id))
    } else {
        Ok(PsObject::name_lit(name_id))
    }
}

// ─── Homogeneous number array parser (149) ──────────────────────────────────

fn parse_number_array_from_slice(
    ctx: &mut Context,
    data: &[u8],
) -> Result<(BinaryTokenResult, usize), PsError> {
    need(data, 3)?; // repr + 2-byte count
    let r = data[0];
    let (elem_size, is_real, scale, big_endian) = number_array_repr(r)?;
    let count = if big_endian {
        u16::from_be_bytes([data[1], data[2]]) as usize
    } else {
        u16::from_le_bytes([data[1], data[2]]) as usize
    };
    let body_start = 3;
    let body_len = count * elem_size;
    need(data, body_start + body_len)?;
    let elements = decode_number_elements(
        &data[body_start..],
        count,
        elem_size,
        is_real,
        scale,
        big_endian,
    );
    let obj = alloc_array(ctx, &elements);
    Ok((BinaryTokenResult::Single(obj), body_start + body_len))
}

fn parse_number_array_from_stream(
    ctx: &mut Context,
    file_entity: EntityId,
) -> Result<BinaryTokenResult, PsError> {
    let rb = read_n_bytes(&mut ctx.files, file_entity, 1)?;
    let r = rb[0];
    let (elem_size, is_real, scale, big_endian) = number_array_repr(r)?;
    let count_bytes = read_n_bytes(&mut ctx.files, file_entity, 2)?;
    let count = if big_endian {
        u16::from_be_bytes([count_bytes[0], count_bytes[1]]) as usize
    } else {
        u16::from_le_bytes([count_bytes[0], count_bytes[1]]) as usize
    };
    let body = read_n_bytes(&mut ctx.files, file_entity, count * elem_size)?;
    let elements = decode_number_elements(&body, count, elem_size, is_real, scale, big_endian);
    let obj = alloc_array(ctx, &elements);
    Ok(BinaryTokenResult::Single(obj))
}

/// Returns (elem_size, is_real, scale, big_endian) for a number array repr byte.
fn number_array_repr(r: u8) -> Result<(usize, bool, u8, bool), PsError> {
    match r {
        0..=31 => Ok((4, false, r, true)),
        32..=47 => Ok((2, false, r - 32, true)),
        48 => Ok((4, true, 0, true)),
        49 => Ok((4, true, 0, true)), // native real — treat as LE on x86 but count is BE per PostForge
        128..=159 => Ok((4, false, r - 128, false)),
        160..=175 => Ok((2, false, r - 160, false)),
        176 => Ok((4, true, 0, false)),
        177 => Ok((4, true, 0, false)), // native real LE
        _ => Err(PsError::SyntaxError),
    }
}

fn decode_number_elements(
    data: &[u8],
    count: usize,
    elem_size: usize,
    is_real: bool,
    scale: u8,
    big_endian: bool,
) -> Vec<PsObject> {
    let mut elements = Vec::with_capacity(count);
    for i in 0..count {
        let off = i * elem_size;
        if is_real {
            let v = if big_endian {
                f32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
            } else {
                f32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
            };
            elements.push(PsObject::real(v as f64));
        } else if elem_size == 4 {
            let raw = if big_endian {
                i32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
            } else {
                i32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
            };
            if scale == 0 {
                elements.push(PsObject::int(raw));
            } else {
                elements.push(PsObject::real(raw as f64 / (1u64 << scale) as f64));
            }
        } else {
            // 2-byte
            let raw = if big_endian {
                i16::from_be_bytes([data[off], data[off + 1]]) as i32
            } else {
                i16::from_le_bytes([data[off], data[off + 1]]) as i32
            };
            if scale == 0 {
                elements.push(PsObject::int(raw));
            } else {
                elements.push(PsObject::real(raw as f64 / (1u64 << scale) as f64));
            }
        }
    }
    elements
}

fn alloc_array(ctx: &mut Context, elements: &[PsObject]) -> PsObject {
    let save_level = ctx.save_stack.current_level();
    let global = ctx.vm_alloc_mode;
    let created = ctx.save_stack.last_save_id();
    let entity = ctx
        .arrays
        .allocate_with(elements.len(), save_level, global, created);
    let dest = ctx.arrays.get_mut(entity, 0, elements.len() as u32);
    dest.copy_from_slice(elements);
    let mut obj = PsObject {
        value: PsValue::Array {
            entity,
            start: 0,
            len: elements.len() as u32,
        },
        flags: ObjFlags::literal_composite(),
    };
    if global {
        obj.flags = ObjFlags::new(ObjFlags::ACCESS_UNLIMITED, false, true, true);
    }
    obj
}

// ─── Binary object sequence parsers (128-131) ───────────────────────────────

const MAX_BOS_DEPTH: u32 = 100;

fn parse_bos_from_slice(
    ctx: &mut Context,
    tag: u8,
    data: &[u8],
) -> Result<(BinaryTokenResult, usize), PsError> {
    let big_endian = tag == 128 || tag == 130;

    // Read header (first 3 bytes: byte1 + 2-byte count)
    need(data, 3)?;
    let byte1 = data[0];
    let top_level_count = if big_endian {
        u16::from_be_bytes([data[1], data[2]]) as usize
    } else {
        u16::from_le_bytes([data[1], data[2]]) as usize
    };

    let (overall_length, header_size) = if byte1 > 0 {
        (byte1 as usize, 4usize)
    } else {
        need(data, 7)?;
        let ext = if big_endian {
            u32::from_be_bytes([data[3], data[4], data[5], data[6]]) as usize
        } else {
            u32::from_le_bytes([data[3], data[4], data[5], data[6]]) as usize
        };
        (ext, 8usize)
    };

    // Note: overall_length includes the tag byte (already consumed) + rest of header + body.
    // The data slice starts AFTER the tag byte. So body starts at (header_size - 1) from
    // the start of data, and total data consumed = overall_length - 1.
    let data_size = overall_length
        .checked_sub(header_size)
        .ok_or(PsError::SyntaxError)?;
    let body_offset = header_size - 1; // -1 because tag byte not in data
    need(data, body_offset + data_size)?;
    let body = &data[body_offset..body_offset + data_size];

    let obj = parse_bos_body(ctx, body, data_size, top_level_count, big_endian)?;
    Ok((BinaryTokenResult::Sequence(obj), body_offset + data_size))
}

fn parse_bos_from_stream(
    ctx: &mut Context,
    tag: u8,
    file_entity: EntityId,
) -> Result<BinaryTokenResult, PsError> {
    let big_endian = tag == 128 || tag == 130;

    // Read header bytes after the tag
    let hdr = read_n_bytes(&mut ctx.files, file_entity, 3)?;
    let byte1 = hdr[0];
    let top_level_count = if big_endian {
        u16::from_be_bytes([hdr[1], hdr[2]]) as usize
    } else {
        u16::from_le_bytes([hdr[1], hdr[2]]) as usize
    };

    let (overall_length, header_size) = if byte1 > 0 {
        (byte1 as usize, 4usize)
    } else {
        let ext_bytes = read_n_bytes(&mut ctx.files, file_entity, 4)?;
        let ext = if big_endian {
            u32::from_be_bytes([ext_bytes[0], ext_bytes[1], ext_bytes[2], ext_bytes[3]]) as usize
        } else {
            u32::from_le_bytes([ext_bytes[0], ext_bytes[1], ext_bytes[2], ext_bytes[3]]) as usize
        };
        (ext, 8usize)
    };

    let data_size = overall_length
        .checked_sub(header_size)
        .ok_or(PsError::SyntaxError)?;
    let body = read_n_bytes(&mut ctx.files, file_entity, data_size)?;

    let obj = parse_bos_body(ctx, &body, data_size, top_level_count, big_endian)?;
    Ok(BinaryTokenResult::Sequence(obj))
}

/// Parse BOS body (shared by slice and stream paths).
fn parse_bos_body(
    ctx: &mut Context,
    data: &[u8],
    data_size: usize,
    top_level_count: usize,
    big_endian: bool,
) -> Result<PsObject, PsError> {
    if data_size < top_level_count * 8 {
        return Err(PsError::SyntaxError);
    }

    let mut results = Vec::with_capacity(top_level_count);
    for i in 0..top_level_count {
        let obj = build_bos_object(ctx, data, data_size, i * 8, big_endian, 0)?;
        results.push(obj);
    }

    // Wrap in executable array
    let save_level = ctx.save_stack.current_level();
    let global = ctx.vm_alloc_mode;
    let created = ctx.save_stack.last_save_id();
    let entity = ctx
        .arrays
        .allocate_with(results.len(), save_level, global, created);
    let dest = ctx.arrays.get_mut(entity, 0, results.len() as u32);
    dest.copy_from_slice(&results);

    let mut obj = PsObject::procedure(entity, results.len() as u32);
    if global {
        obj.flags = ObjFlags::new(ObjFlags::ACCESS_UNLIMITED, true, true, true);
    }
    Ok(obj)
}

/// Recursively build a PsObject from an 8-byte BOS entry.
fn build_bos_object(
    ctx: &mut Context,
    data: &[u8],
    data_size: usize,
    pos: usize,
    big_endian: bool,
    depth: u32,
) -> Result<PsObject, PsError> {
    if depth > MAX_BOS_DEPTH {
        return Err(PsError::SyntaxError);
    }
    if pos + 8 > data_size {
        return Err(PsError::SyntaxError);
    }

    let type_byte = data[pos];
    let type_code = type_byte & 0x7F;
    let is_exec = type_byte & 0x80 != 0;
    let length_u16 = if big_endian {
        u16::from_be_bytes([data[pos + 2], data[pos + 3]])
    } else {
        u16::from_le_bytes([data[pos + 2], data[pos + 3]])
    };
    let value_u32 = if big_endian {
        u32::from_be_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]])
    } else {
        u32::from_le_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]])
    };

    match type_code {
        // null
        0 => Ok(PsObject::null()),

        // integer
        1 => {
            let v = if big_endian {
                i32::from_be_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]])
            } else {
                i32::from_le_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]])
            };
            Ok(PsObject::int(v))
        }

        // real
        2 => {
            let v = if length_u16 == 0 {
                // IEEE float
                (if big_endian {
                    f32::from_be_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]])
                } else {
                    f32::from_le_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]])
                }) as f64
            } else {
                // Fixed-point: raw / 2^length
                let raw = if big_endian {
                    i32::from_be_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]])
                } else {
                    i32::from_le_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]])
                };
                raw as f64 / (1u64 << length_u16) as f64
            };
            Ok(PsObject::real(v))
        }

        // name (3) / immediately evaluated name (6)
        3 | 6 => {
            let signed_len = if big_endian {
                i16::from_be_bytes([data[pos + 2], data[pos + 3]])
            } else {
                i16::from_le_bytes([data[pos + 2], data[pos + 3]])
            };
            let name_bytes = if signed_len == -1 {
                // System name table lookup
                let idx = value_u32 as usize;
                if idx >= SYSTEM_NAME_TABLE.len() {
                    return Err(PsError::SyntaxError);
                }
                SYSTEM_NAME_TABLE[idx].ok_or(PsError::SyntaxError)?
            } else if signed_len > 0 {
                let offset = value_u32 as usize;
                let len = signed_len as usize;
                if offset + len > data_size {
                    return Err(PsError::SyntaxError);
                }
                &data[offset..offset + len]
            } else {
                return Err(PsError::SyntaxError);
            };

            if type_code == 6 {
                // Immediately evaluated name: look up in dict stack
                let name_id = ctx.names.intern(name_bytes);
                let key = DictKey::Name(name_id);
                ctx.dict_load(&key).ok_or(PsError::Undefined)
            } else {
                let name_id = ctx.names.intern(name_bytes);
                let mut obj = if is_exec {
                    PsObject::name_exec(name_id)
                } else {
                    PsObject::name_lit(name_id)
                };
                if !is_exec {
                    obj.flags.set_literal();
                }
                Ok(obj)
            }
        }

        // boolean
        4 => Ok(PsObject::bool(value_u32 != 0)),

        // string
        5 => {
            let len = length_u16 as usize;
            if len == 0 {
                let obj = alloc_string(ctx, &[]);
                return Ok(obj);
            }
            let offset = value_u32 as usize;
            if offset + len > data_size {
                return Err(PsError::SyntaxError);
            }
            let mut obj = alloc_string(ctx, &data[offset..offset + len]);
            if is_exec {
                obj.flags.set_executable();
            }
            Ok(obj)
        }

        // array
        9 => {
            let count = length_u16 as usize;
            if count == 0 {
                let obj = alloc_array(ctx, &[]);
                return Ok(obj);
            }
            let offset = value_u32 as usize;
            let mut elements = Vec::with_capacity(count);
            for i in 0..count {
                let child =
                    build_bos_object(ctx, data, data_size, offset + i * 8, big_endian, depth + 1)?;
                elements.push(child);
            }
            let mut obj = alloc_array(ctx, &elements);
            if is_exec {
                obj.flags.set_executable();
            }
            Ok(obj)
        }

        // mark
        10 => Ok(PsObject::mark()),

        _ => Err(PsError::SyntaxError),
    }
}
