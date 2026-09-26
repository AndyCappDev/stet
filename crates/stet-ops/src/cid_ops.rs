// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! CID-keyed fonts whose glyphs are Type 1 charstrings.
//!
//! Implements `.cid_startdata` — the internal operator that backs the
//! `CIDInit` ProcSet's `StartData` procedure — and `Type1CidFont`, which
//! builds glyphs from the data it reads.

use stet_core::context::Context;
use stet_core::dict::DictKey;
use stet_core::error::PsError;
use stet_core::object::{EntityId, PsObject, PsValue};
use stet_fonts::charstring::{self, CharstringResult};
use stet_fonts::geometry::Matrix;

/// Largest `FDBytes` / `GDBytes` / `SDBytes`, as in Ghostscript
/// (`MAX_FDBytes`, `MAX_GDBytes` in `gxfcid.h`). They are the byte widths of
/// offsets into `GlyphData`, so four already addresses 4 GB.
const MAX_OFFSET_BYTES: usize = 4;

/// `.cid_startdata`: `(Binary)|(Hex) count → string`
///
/// Reads the `count` bytes of CIDFontType 0 glyph data that follow
/// `StartData` in the current file and returns them; `CIDInit`'s
/// `StartData` stores the string as the CIDFont's `GlyphData`, which
/// `Type1CidFont` reads glyphs from.
///
/// The blob sits inline in the PostScript source, immediately after the
/// font's header, and runs to several megabytes (14.7 MB for a CJK font in
/// the corpus); anything left unread would reach the scanner as PostScript.
///
/// The `(Hex)` form counts *decoded* bytes, matching Ghostscript's
/// `gs_cidfn.ps`, which reads through an `ASCIIHexDecode` filter and then
/// closes it — closing consumes the trailing `>`, so this does too. A short
/// read is not an error: in a truncated job the data simply runs to the end
/// of the file, and the glyphs past it are missing.
pub fn op_cid_startdata(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }

    let count = ctx.o_stack.peek(0)?.as_i32().ok_or(PsError::TypeCheck)?;
    if count < 0 {
        return Err(PsError::RangeCheck);
    }
    let format_obj = ctx.o_stack.peek(1)?;
    let is_hex = match format_obj.value {
        PsValue::String { entity, start, len } => ctx.strings.get(entity, start, len) == b"Hex",
        _ => return Err(PsError::TypeCheck),
    };

    ctx.o_stack.pop()?; // count
    ctx.o_stack.pop()?; // format

    let file_entity = current_file(ctx)?;
    ctx.pump_proc_sources(file_entity)?;

    let count = count as usize;
    let data = if is_hex {
        read_hex(ctx, file_entity, count)?
    } else {
        read_binary(ctx, file_entity, count)?
    };
    let len = u32::try_from(data.len()).map_err(|_| PsError::LimitCheck)?;
    let entity = crate::vm_ops::alloc_string(ctx, &data);
    ctx.o_stack.push(PsObject::string(entity, len))?;
    Ok(())
}

/// The topmost file on the execution stack — what `currentfile` returns.
fn current_file(ctx: &Context) -> Result<EntityId, PsError> {
    for i in 0..ctx.e_stack.len() {
        if let Ok(obj) = ctx.e_stack.peek(i)
            && let PsValue::File(e) = obj.value
        {
            return Ok(e);
        }
    }
    Err(PsError::InvalidFont)
}

/// Read up to `count` raw bytes.
///
/// The buffer grows with what is actually read rather than being sized from
/// `count`, which comes from the file and bounds nothing.
fn read_binary(ctx: &mut Context, file: EntityId, count: usize) -> Result<Vec<u8>, PsError> {
    let mut data = Vec::new();
    let mut buf = vec![0u8; 65536];
    while data.len() < count {
        let want = (count - data.len()).min(buf.len());
        let n = ctx
            .files
            .read_into(file, &mut buf[..want])
            .map_err(|_| PsError::IOError)?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
    }
    Ok(data)
}

/// Decode hex digits until `count` bytes have been read, then consume the
/// `>` that ends the data — the byte an `ASCIIHexDecode` filter would eat when
/// it is closed. Whitespace between digits is ignored, as the filter does.
fn read_hex(ctx: &mut Context, file: EntityId, count: usize) -> Result<Vec<u8>, PsError> {
    let mut data = Vec::new();
    let mut high: Option<u8> = None;
    while data.len() < count {
        let b = match ctx.files.read_byte(file).map_err(|_| PsError::IOError)? {
            Some(b) => b,
            None => return Ok(data),
        };
        let nibble = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            // An odd final digit is padded with 0, as ASCIIHexDecode does.
            b'>' => {
                if let Some(h) = high {
                    data.push(h << 4);
                }
                return Ok(data);
            }
            _ => continue, // whitespace and anything else the filter ignores
        };
        match high.take() {
            Some(h) => data.push((h << 4) | nibble),
            None => high = Some(nibble),
        }
    }
    // Consume the EOD marker so the scanner does not meet it.
    loop {
        match ctx.files.read_byte(file).map_err(|_| PsError::IOError)? {
            Some(b) if b.is_ascii_whitespace() => continue,
            Some(b'>') | None => break,
            Some(b) => {
                ctx.files.putback_bytes(file, &[b]);
                break;
            }
        }
    }
    Ok(data)
}

/// One `FDArray` entry of a [`Type1CidFont`]: what its glyphs need to run.
struct FdType1 {
    /// Glyph space to the CIDFont's font space, `None` for the identity.
    /// The CIDFont's own `FontMatrix` applies after it, as for any glyph.
    font_matrix: Option<Matrix>,
    /// `lenIV`, with −1 (unencrypted) as the `usize::MAX` sentinel
    /// [`charstring::decrypt_charstring`] expects.
    len_iv: usize,
    /// Subroutines, sliced out of `GlyphData` through the Private dict's
    /// `SubrMapOffset` / `SDBytes` / `SubrCount`.
    subrs: Vec<Vec<u8>>,
}

/// A CIDFontType 0 font whose glyphs are Type 1 charstrings in `GlyphData`,
/// the form `StartData` builds (Adobe TN 5014).
///
/// `GlyphData` begins, at `CIDMapOffset`, with one entry per CID: an FD
/// index (`FDBytes` wide) and the offset of its charstring (`GDBytes`
/// wide), both big-endian. A glyph runs to the next entry's offset, and an
/// empty one is missing. The FD index picks the `FDArray` font whose
/// `Private` dict supplies `lenIV` and subroutines and whose `FontMatrix`
/// maps the glyph into the CIDFont's font space. Subroutines are optional:
/// Ghostscript reads them only when `SubrCount` is present
/// (`gs_cidfn.ps`), and fonts without any omit all three keys.
pub(crate) struct Type1CidFont {
    glyph_data: (EntityId, u32, u32),
    cid_count: usize,
    cid_map_offset: usize,
    fd_bytes: usize,
    gd_bytes: usize,
    fds: Vec<FdType1>,
}

impl Type1CidFont {
    /// Read the font's parameters, or `None` when `cidfont` has no string
    /// `GlyphData` (a CFF CIDFont, or one this does not support).
    ///
    /// Resolved once per show operation: subroutines are copied out here
    /// rather than per glyph.
    pub(crate) fn of(ctx: &Context, cidfont: EntityId) -> Result<Option<Self>, PsError> {
        let glyph_data = match get(ctx, cidfont, b"GlyphData").map(|o| o.value) {
            Some(PsValue::String { entity, start, len }) => (entity, start, len),
            _ => return Ok(None),
        };
        let fd_bytes = get_usize(ctx, cidfont, b"FDBytes").ok_or(PsError::InvalidFont)?;
        let gd_bytes = get_usize(ctx, cidfont, b"GDBytes").ok_or(PsError::InvalidFont)?;
        if fd_bytes > MAX_OFFSET_BYTES || gd_bytes == 0 || gd_bytes > MAX_OFFSET_BYTES {
            return Err(PsError::InvalidFont);
        }
        let cid_count = get_usize(ctx, cidfont, b"CIDCount").ok_or(PsError::InvalidFont)?;
        let cid_map_offset = get_usize(ctx, cidfont, b"CIDMapOffset").unwrap_or(0);

        let data = ctx.strings.get(glyph_data.0, glyph_data.1, glyph_data.2);
        let fds = match get(ctx, cidfont, b"FDArray").map(|o| o.value) {
            Some(PsValue::Array { entity, start, len }) => (0..len)
                .map(|i| match ctx.arrays.get_element(entity, start + i).value {
                    PsValue::Dict(fd) => fd_type1(ctx, fd, data),
                    _ => Err(PsError::InvalidFont),
                })
                .collect::<Result<Vec<_>, _>>()?,
            _ => return Err(PsError::InvalidFont),
        };
        Ok(Some(Self {
            glyph_data,
            cid_count,
            cid_map_offset,
            fd_bytes,
            gd_bytes,
            fds,
        }))
    }

    /// The outline and advance of `cid` in the CIDFont's font space (its
    /// FD's `FontMatrix` applied), or `None` when the font has no glyph for
    /// it. `Err` means the charstring itself is broken.
    pub(crate) fn glyph(
        &self,
        ctx: &Context,
        cid: i32,
    ) -> Result<Option<CharstringResult>, PsError> {
        let Ok(cid) = usize::try_from(cid) else {
            return Ok(None);
        };
        if cid >= self.cid_count {
            return Ok(None);
        }
        let data = ctx
            .strings
            .get(self.glyph_data.0, self.glyph_data.1, self.glyph_data.2);
        let entry = self.fd_bytes + self.gd_bytes;
        let Some(base) = cid
            .checked_mul(entry)
            .and_then(|o| o.checked_add(self.cid_map_offset))
        else {
            return Ok(None);
        };
        // This entry and the next: the next one's offset ends the glyph.
        let Some(map) = base
            .checked_add(2 * entry)
            .and_then(|end| data.get(base..end))
        else {
            return Ok(None);
        };
        let fd = read_be(&map[..self.fd_bytes]);
        let start = read_be(&map[self.fd_bytes..entry]);
        let end = read_be(&map[entry + self.fd_bytes..]);
        if end <= start {
            return Ok(None); // missing glyph
        }
        let (Some(fd), Some(charstring)) = (self.fds.get(fd), data.get(start..end)) else {
            return Ok(None);
        };
        let mut glyph = charstring::execute_charstring(charstring, &fd.subrs, fd.len_iv, false)
            .map_err(|_| PsError::InvalidFont)?;
        if let Some(m) = &fd.font_matrix {
            glyph.path = glyph.path.transform(m);
            (glyph.width_x, glyph.width_y) = m.transform_delta(glyph.width_x, glyph.width_y);
        }
        Ok(Some(glyph))
    }
}

/// Resolve one `FDArray` font against the glyph data its subroutines live in.
fn fd_type1(ctx: &Context, fd: EntityId, data: &[u8]) -> Result<FdType1, PsError> {
    let font_matrix = match get(ctx, fd, b"FontMatrix").map(|o| o.value) {
        Some(PsValue::Array { entity, start, len }) if len == 6 => {
            let m = ctx.arrays.get(entity, start, len);
            let v = (0..6)
                .map(|i| m[i].as_f64().ok_or(PsError::InvalidFont))
                .collect::<Result<Vec<_>, _>>()?;
            (v != [1.0, 0.0, 0.0, 1.0, 0.0, 0.0])
                .then(|| Matrix::new(v[0], v[1], v[2], v[3], v[4], v[5]))
        }
        _ => None,
    };
    let private = match get(ctx, fd, b"Private").map(|o| o.value) {
        Some(PsValue::Dict(p)) => Some(p),
        _ => None,
    };
    let len_iv = private
        .and_then(|p| get(ctx, p, b"lenIV"))
        .and_then(|o| o.as_i32())
        .unwrap_or(4);
    let len_iv = if len_iv < 0 {
        usize::MAX
    } else {
        len_iv as usize
    };
    let subrs = private
        .map(|p| subrs_from_map(ctx, p, data))
        .transpose()?
        .unwrap_or_default();
    Ok(FdType1 {
        font_matrix,
        len_iv,
        subrs,
    })
}

/// Slice a Private dict's subroutines out of `GlyphData`: `SubrCount + 1`
/// offsets of `SDBytes` each from `SubrMapOffset`, subroutine *i* running
/// from offset *i* to offset *i + 1*. None when `SubrCount` is absent.
fn subrs_from_map(ctx: &Context, private: EntityId, data: &[u8]) -> Result<Vec<Vec<u8>>, PsError> {
    let Some(count) = get_usize(ctx, private, b"SubrCount") else {
        return Ok(Vec::new());
    };
    let sd_bytes = get_usize(ctx, private, b"SDBytes").ok_or(PsError::InvalidFont)?;
    let map_offset = get_usize(ctx, private, b"SubrMapOffset").ok_or(PsError::InvalidFont)?;
    if sd_bytes == 0 || sd_bytes > MAX_OFFSET_BYTES {
        return Err(PsError::InvalidFont);
    }
    // The whole map must be present before anything is sized from `count`,
    // which comes from the file.
    let map = count
        .checked_add(1)
        .and_then(|n| n.checked_mul(sd_bytes))
        .and_then(|n| n.checked_add(map_offset))
        .and_then(|end| data.get(map_offset..end))
        .ok_or(PsError::InvalidFont)?;
    let offsets: Vec<usize> = map.chunks_exact(sd_bytes).map(read_be).collect();
    Ok(offsets
        .windows(2)
        .map(|w| data.get(w[0]..w[1]).unwrap_or_default().to_vec())
        .collect())
}

/// A big-endian unsigned integer of up to [`MAX_OFFSET_BYTES`] bytes.
fn read_be(bytes: &[u8]) -> usize {
    bytes.iter().fold(0, |v, &b| (v << 8) | b as usize)
}

fn get(ctx: &Context, dict: EntityId, key: &[u8]) -> Option<PsObject> {
    let name = ctx.names.find(key)?;
    ctx.dicts.get(dict, &DictKey::Name(name))
}

fn get_usize(ctx: &Context, dict: EntityId, key: &[u8]) -> Option<usize> {
    get(ctx, dict, key)?
        .as_i64()
        .and_then(|v| usize::try_from(v).ok())
}
