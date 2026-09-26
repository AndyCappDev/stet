// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! CID-keyed CFF writer (Adobe TN#5176).
//!
//! Serialises a [`CidFont`] — Type 2 charstrings grouped into Font DICTs —
//! as a one-font CFF FontSet, the form PDF embeds as a `/CIDFontType0C`
//! `FontFile3` stream.
//!
//! The writer does not convert or subset charstrings; the caller supplies
//! finished Type 2 charstrings whose widths are already encoded against
//! their Font DICT's `defaultWidthX` / `nominalWidthX`.
//!
//! Layout, in file order:
//!
//! ```text
//! Header | Name INDEX | Top DICT INDEX | String INDEX | Global Subr INDEX
//! charset | FDSelect | CharStrings INDEX | FDArray INDEX
//! (Private DICT | Local Subr INDEX) per Font DICT
//! ```
//!
//! Every DICT operand that is an offset is written in the fixed 5-byte
//! integer form, so each DICT's length is known before the offsets it
//! carries are, and one layout pass suffices.

use crate::cff_parser::{
    DictEntry, DictOp, STANDARD_STRINGS, build_cid_to_gid, dict_get, dict_usize, get_sid_string,
    parse_dict_data, parse_fd_select, parse_index,
};

/// Largest object count a CFF INDEX can hold (its `count` is a Card16).
const MAX_INDEX_COUNT: usize = u16::MAX as usize;

/// Default Top DICT `FontMatrix` (TN#5176 Table 9).
const DEFAULT_FONT_MATRIX: [f64; 6] = [0.001, 0.0, 0.0, 0.001, 0.0, 0.0];

/// A CID-keyed font to serialise with [`write_cid_font`].
///
/// Build one with [`CidFont::new`] and set the remaining fields.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct CidFont {
    /// CIDFontName, written to the Name INDEX.
    pub name: String,
    /// `CIDSystemInfo` Registry.
    pub registry: String,
    /// `CIDSystemInfo` Ordering.
    pub ordering: String,
    /// `CIDSystemInfo` Supplement.
    pub supplement: i32,
    /// Top DICT `FontMatrix`. Glyph space maps through a Font DICT's own
    /// matrix, when it has one, and then through this one.
    pub font_matrix: [f64; 6],
    /// Top DICT `FontBBox`.
    pub font_bbox: [f64; 4],
    /// `CIDCount`: one more than the highest CID the font can address.
    pub cid_count: u32,
    /// Global subroutines, shared by every charstring.
    pub global_subrs: Vec<Vec<u8>>,
    /// The Font DICTs (FDArray). At least one, at most 256.
    pub font_dicts: Vec<FontDict>,
    /// Glyphs in GID order. GID 0 must be CID 0 (`.notdef`), and CIDs must
    /// increase strictly.
    pub glyphs: Vec<Glyph>,
}

impl CidFont {
    /// A font with the given name and `CIDSystemInfo`, the default
    /// `FontMatrix`, and no Font DICTs or glyphs.
    pub fn new(name: &str, registry: &str, ordering: &str, supplement: i32) -> Self {
        Self {
            name: name.to_owned(),
            registry: registry.to_owned(),
            ordering: ordering.to_owned(),
            supplement,
            font_matrix: DEFAULT_FONT_MATRIX,
            font_bbox: [0.0; 4],
            cid_count: 0,
            global_subrs: Vec::new(),
            font_dicts: Vec::new(),
            glyphs: Vec::new(),
        }
    }

    /// The font with only the glyphs whose CIDs `keep` accepts, and CID 0,
    /// which a CIDFont always has. Font DICTs no remaining glyph uses are
    /// dropped and the others renumbered; subroutines are kept whole.
    pub fn subset(&self, keep: impl Fn(u16) -> bool) -> CidFont {
        let glyphs: Vec<&Glyph> = self
            .glyphs
            .iter()
            .filter(|g| g.cid == 0 || keep(g.cid))
            .collect();
        let mut fd_map = vec![None; self.font_dicts.len()];
        let mut font_dicts = Vec::new();
        for g in &glyphs {
            if let Some(slot) = fd_map.get_mut(usize::from(g.fd))
                && slot.is_none()
            {
                *slot = Some(font_dicts.len() as u8);
                font_dicts.push(self.font_dicts[usize::from(g.fd)].clone());
            }
        }
        CidFont {
            glyphs: glyphs
                .into_iter()
                .map(|g| {
                    let fd = fd_map.get(usize::from(g.fd)).copied().flatten();
                    Glyph::new(g.cid, fd.unwrap_or(g.fd), g.charstring.clone())
                })
                .collect(),
            font_dicts,
            name: self.name.clone(),
            registry: self.registry.clone(),
            ordering: self.ordering.clone(),
            supplement: self.supplement,
            font_matrix: self.font_matrix,
            font_bbox: self.font_bbox,
            cid_count: self.cid_count,
            global_subrs: self.global_subrs.clone(),
        }
    }
}

/// One Font DICT of the FDArray, with its Private DICT and local subrs.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FontDict {
    /// `FontName` (12 38), if any.
    pub name: Option<String>,
    /// This Font DICT's `FontMatrix`, if it has its own.
    pub font_matrix: Option<[f64; 6]>,
    /// The Private DICT.
    pub private: PrivateDict,
    /// Local subroutines. The Private DICT's `Subrs` offset is written for
    /// them; it is not a field of [`PrivateDict`].
    pub local_subrs: Vec<Vec<u8>>,
}

/// Private DICT values (TN#5176 Table 23), other than `Subrs`.
///
/// `None` and empty arrays are omitted from the output, so the reader
/// applies the specification's default.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PrivateDict {
    /// `BlueValues`, as absolute values.
    pub blue_values: Vec<f64>,
    /// `OtherBlues`, as absolute values.
    pub other_blues: Vec<f64>,
    /// `FamilyBlues`, as absolute values.
    pub family_blues: Vec<f64>,
    /// `FamilyOtherBlues`, as absolute values.
    pub family_other_blues: Vec<f64>,
    /// `BlueScale`.
    pub blue_scale: Option<f64>,
    /// `BlueShift`.
    pub blue_shift: Option<f64>,
    /// `BlueFuzz`.
    pub blue_fuzz: Option<f64>,
    /// `StdHW`.
    pub std_hw: Option<f64>,
    /// `StdVW`.
    pub std_vw: Option<f64>,
    /// `StemSnapH`, as absolute values.
    pub stem_snap_h: Vec<f64>,
    /// `StemSnapV`, as absolute values.
    pub stem_snap_v: Vec<f64>,
    /// `ForceBold`.
    pub force_bold: bool,
    /// `LanguageGroup`.
    pub language_group: Option<i32>,
    /// `ExpansionFactor`.
    pub expansion_factor: Option<f64>,
    /// `initialRandomSeed`.
    pub initial_random_seed: Option<f64>,
    /// `defaultWidthX`: the advance of a glyph whose charstring has no width.
    pub default_width_x: f64,
    /// `nominalWidthX`: the base a charstring's width operand is added to.
    pub nominal_width_x: f64,
}

/// One glyph of a [`CidFont`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct Glyph {
    /// The glyph's CID.
    pub cid: u16,
    /// Index of its Font DICT in [`CidFont::font_dicts`].
    pub fd: u8,
    /// Type 2 charstring.
    pub charstring: Vec<u8>,
}

impl Glyph {
    /// A glyph with the given CID, Font DICT index and Type 2 charstring.
    pub fn new(cid: u16, fd: u8, charstring: Vec<u8>) -> Self {
        Self {
            cid,
            fd,
            charstring,
        }
    }
}

// ---------------------------------------------------------------------------
// Operators
// ---------------------------------------------------------------------------

/// Two-byte DICT operator `12 b`.
const fn esc(b: u8) -> u16 {
    0x0C00 | b as u16
}

const OP_FONT_BBOX: u16 = 5;
const OP_BLUE_VALUES: u16 = 6;
const OP_OTHER_BLUES: u16 = 7;
const OP_FAMILY_BLUES: u16 = 8;
const OP_FAMILY_OTHER_BLUES: u16 = 9;
const OP_STD_HW: u16 = 10;
const OP_STD_VW: u16 = 11;
const OP_CHARSET: u16 = 15;
const OP_CHAR_STRINGS: u16 = 17;
const OP_PRIVATE: u16 = 18;
const OP_SUBRS: u16 = 19;
const OP_DEFAULT_WIDTH_X: u16 = 20;
const OP_NOMINAL_WIDTH_X: u16 = 21;
const OP_FONT_MATRIX: u16 = esc(7);
const OP_BLUE_SCALE: u16 = esc(9);
const OP_BLUE_SHIFT: u16 = esc(10);
const OP_BLUE_FUZZ: u16 = esc(11);
const OP_STEM_SNAP_H: u16 = esc(12);
const OP_STEM_SNAP_V: u16 = esc(13);
const OP_FORCE_BOLD: u16 = esc(14);
const OP_LANGUAGE_GROUP: u16 = esc(17);
const OP_EXPANSION_FACTOR: u16 = esc(18);
const OP_INITIAL_RANDOM_SEED: u16 = esc(19);
const OP_ROS: u16 = esc(30);
const OP_CID_COUNT: u16 = esc(34);
const OP_FD_ARRAY: u16 = esc(36);
const OP_FD_SELECT: u16 = esc(37);
const OP_FONT_NAME: u16 = esc(38);

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

/// Serialise `font` as CFF.
///
/// Fails when the font breaks a CFF structural limit or one of the
/// invariants documented on [`CidFont`]: no Font DICTs or more than 256, no
/// glyphs, a first glyph that is not CID 0, CIDs out of order or not below
/// `cid_count`, a glyph naming a missing Font DICT, or an INDEX of more than
/// 65535 objects.
pub fn write_cid_font(font: &CidFont) -> Result<Vec<u8>, String> {
    validate(font)?;

    let mut strings = StringTable::default();
    let registry = strings.sid(&font.registry)?;
    let ordering = strings.sid(&font.ordering)?;
    let fd_names = font
        .font_dicts
        .iter()
        .map(|fd| fd.name.as_deref().map(|n| strings.sid(n)).transpose())
        .collect::<Result<Vec<_>, _>>()?;

    // Everything whose content does not depend on where it lands.
    let name_index = index(&[font.name.as_bytes()])?;
    let string_index = index(&strings.custom)?;
    let global_subr_index = index(&font.global_subrs)?;
    let charset = charset(&font.glyphs);
    let fd_select = fd_select(&font.glyphs);
    let char_strings = font.glyphs.iter().map(|g| &g.charstring);
    let char_strings_index = index(&char_strings.collect::<Vec<_>>())?;
    let privates = font
        .font_dicts
        .iter()
        .map(private_and_subrs)
        .collect::<Result<Vec<_>, _>>()?;

    // Offsets are fixed-width, so a DICT built with placeholders has its
    // final length.
    let top_len = top_dict(font, registry, ordering, &TopOffsets::default()).len();
    let fd_dicts_len: Vec<usize> = font
        .font_dicts
        .iter()
        .zip(&fd_names)
        .map(|(fd, &name)| font_dict(fd, name, 0, 0).len())
        .collect();

    let top_index_len = index_len(&[top_len]);
    let fd_array_len = index_len(&fd_dicts_len);

    let mut at = 4 + name_index.len() + top_index_len + string_index.len();
    at += global_subr_index.len();
    let offsets = TopOffsets {
        charset: at,
        fd_select: at + charset.len(),
        char_strings: at + charset.len() + fd_select.len(),
        fd_array: at + charset.len() + fd_select.len() + char_strings_index.len(),
    };
    at = offsets.fd_array + fd_array_len;

    let mut fd_dicts = Vec::with_capacity(font.font_dicts.len());
    for ((fd, &name), (private, subrs)) in font.font_dicts.iter().zip(&fd_names).zip(&privates) {
        fd_dicts.push(font_dict(fd, name, private.len(), at));
        at += private.len() + subrs.len();
    }
    let total = at;

    let top = top_dict(font, registry, ordering, &offsets);
    debug_assert_eq!(top.len(), top_len);
    let top_index = index(&[&top])?;
    let fd_array_index = index(&fd_dicts)?;
    debug_assert_eq!(fd_array_index.len(), fd_array_len);

    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&[1, 0, 4, offset_size(total)]);
    for part in [
        &name_index,
        &top_index,
        &string_index,
        &global_subr_index,
        &charset,
        &fd_select,
        &char_strings_index,
        &fd_array_index,
    ] {
        out.extend_from_slice(part);
    }
    for (private, subrs) in &privates {
        out.extend_from_slice(private);
        out.extend_from_slice(subrs);
    }
    debug_assert_eq!(out.len(), total);
    Ok(out)
}

/// Check the invariants [`write_cid_font`] documents.
fn validate(font: &CidFont) -> Result<(), String> {
    if font.font_dicts.is_empty() || font.font_dicts.len() > 256 {
        return Err(format!(
            "CFF: {} Font DICTs (1 to 256 allowed)",
            font.font_dicts.len()
        ));
    }
    if font.glyphs.len() > MAX_INDEX_COUNT {
        return Err(format!("CFF: {} glyphs (at most 65535)", font.glyphs.len()));
    }
    match font.glyphs.first() {
        Some(g) if g.cid == 0 => {}
        Some(g) => return Err(format!("CFF: GID 0 is CID {}, not CID 0", g.cid)),
        None => return Err("CFF: no glyphs (GID 0 must be CID 0)".into()),
    }
    for pair in font.glyphs.windows(2) {
        if pair[1].cid <= pair[0].cid {
            return Err(format!(
                "CFF: CID {} follows CID {}; CIDs must increase",
                pair[1].cid, pair[0].cid
            ));
        }
    }
    let last = font.glyphs.last().map_or(0, |g| u32::from(g.cid));
    if last >= font.cid_count {
        return Err(format!(
            "CFF: CID {last} is not below CIDCount {}",
            font.cid_count
        ));
    }
    if let Some(g) = font
        .glyphs
        .iter()
        .find(|g| usize::from(g.fd) >= font.font_dicts.len())
    {
        return Err(format!(
            "CFF: CID {} names Font DICT {} of {}",
            g.cid,
            g.fd,
            font.font_dicts.len()
        ));
    }
    Ok(())
}

/// Absolute offsets the Top DICT carries.
#[derive(Default)]
struct TopOffsets {
    charset: usize,
    fd_select: usize,
    char_strings: usize,
    fd_array: usize,
}

/// The Top DICT. ROS comes first: that is how a reader tells a CIDFont
/// from a name-keyed font without parsing the whole DICT (TN#5176 §9).
fn top_dict(font: &CidFont, registry: u16, ordering: u16, at: &TopOffsets) -> Vec<u8> {
    let mut d = DictWriter::default();
    d.int(i32::from(registry));
    d.int(i32::from(ordering));
    d.int(font.supplement);
    d.op(OP_ROS);
    if font.font_matrix != DEFAULT_FONT_MATRIX {
        d.numbers(&font.font_matrix);
        d.op(OP_FONT_MATRIX);
    }
    d.numbers(&font.font_bbox);
    d.op(OP_FONT_BBOX);
    d.number(f64::from(font.cid_count));
    d.op(OP_CID_COUNT);
    d.offset(at.charset);
    d.op(OP_CHARSET);
    d.offset(at.fd_select);
    d.op(OP_FD_SELECT);
    d.offset(at.char_strings);
    d.op(OP_CHAR_STRINGS);
    d.offset(at.fd_array);
    d.op(OP_FD_ARRAY);
    d.0
}

/// One Font DICT of the FDArray.
fn font_dict(fd: &FontDict, name: Option<u16>, private_len: usize, private_at: usize) -> Vec<u8> {
    let mut d = DictWriter::default();
    if let Some(sid) = name {
        d.int(i32::from(sid));
        d.op(OP_FONT_NAME);
    }
    if let Some(m) = &fd.font_matrix {
        d.numbers(m);
        d.op(OP_FONT_MATRIX);
    }
    d.offset(private_len);
    d.offset(private_at);
    d.op(OP_PRIVATE);
    d.0
}

/// A Font DICT's Private DICT and the local Subr INDEX that follows it.
fn private_and_subrs(fd: &FontDict) -> Result<(Vec<u8>, Vec<u8>), String> {
    let p = &fd.private;
    let mut d = DictWriter::default();
    // OtherBlues and FamilyOtherBlues must follow BlueValues and
    // FamilyBlues (TN#5176 §15).
    for (vals, op) in [
        (&p.blue_values, OP_BLUE_VALUES),
        (&p.other_blues, OP_OTHER_BLUES),
        (&p.family_blues, OP_FAMILY_BLUES),
        (&p.family_other_blues, OP_FAMILY_OTHER_BLUES),
    ] {
        d.delta(vals, op);
    }
    for (val, op) in [
        (p.blue_scale, OP_BLUE_SCALE),
        (p.blue_shift, OP_BLUE_SHIFT),
        (p.blue_fuzz, OP_BLUE_FUZZ),
        (p.std_hw, OP_STD_HW),
        (p.std_vw, OP_STD_VW),
    ] {
        if let Some(v) = val {
            d.number(v);
            d.op(op);
        }
    }
    d.delta(&p.stem_snap_h, OP_STEM_SNAP_H);
    d.delta(&p.stem_snap_v, OP_STEM_SNAP_V);
    if p.force_bold {
        d.int(1);
        d.op(OP_FORCE_BOLD);
    }
    if let Some(v) = p.language_group {
        d.int(v);
        d.op(OP_LANGUAGE_GROUP);
    }
    for (val, op) in [
        (p.expansion_factor, OP_EXPANSION_FACTOR),
        (p.initial_random_seed, OP_INITIAL_RANDOM_SEED),
    ] {
        if let Some(v) = val {
            d.number(v);
            d.op(op);
        }
    }
    for (val, op) in [
        (p.default_width_x, OP_DEFAULT_WIDTH_X),
        (p.nominal_width_x, OP_NOMINAL_WIDTH_X),
    ] {
        if val != 0.0 {
            d.number(val);
            d.op(op);
        }
    }
    if fd.local_subrs.is_empty() {
        return Ok((d.0, Vec::new()));
    }
    // The Subrs offset is relative to the Private DICT, and the INDEX
    // follows it directly, so the offset is the DICT's own final length.
    let len = d.0.len() + 5 + 1;
    d.offset(len);
    d.op(OP_SUBRS);
    Ok((d.0, index(&fd.local_subrs)?))
}

/// The charset: GID → CID for every glyph after `.notdef`, in whichever of
/// the three formats (TN#5176 §13) is smallest.
fn charset(glyphs: &[Glyph]) -> Vec<u8> {
    let cids: Vec<u16> = glyphs[1..].iter().map(|g| g.cid).collect();
    let runs = runs(&cids);

    let format0 = 2 * cids.len();
    let format1: usize = runs.iter().map(|&(_, n)| 3 * n.div_ceil(256)).sum();
    let format2 = 4 * runs.len();

    let mut out = Vec::with_capacity(1 + format0.min(format1).min(format2));
    if format0 <= format1 && format0 <= format2 {
        out.push(0);
        for cid in cids {
            out.extend_from_slice(&cid.to_be_bytes());
        }
    } else if format1 <= format2 {
        out.push(1);
        for &(first, n) in &runs {
            for chunk in (0..n).step_by(256) {
                let len = (n - chunk).min(256);
                out.extend_from_slice(&(first + chunk as u16).to_be_bytes());
                out.push((len - 1) as u8);
            }
        }
    } else {
        out.push(2);
        for &(first, n) in &runs {
            out.extend_from_slice(&first.to_be_bytes());
            out.extend_from_slice(&((n - 1) as u16).to_be_bytes());
        }
    }
    out
}

/// Runs of consecutive values in strictly increasing `cids`, as
/// `(first, length)`.
fn runs(cids: &[u16]) -> Vec<(u16, usize)> {
    let mut runs: Vec<(u16, usize)> = Vec::new();
    for &cid in cids {
        match runs.last_mut() {
            Some((first, n)) if usize::from(*first) + *n == usize::from(cid) => *n += 1,
            _ => runs.push((cid, 1)),
        }
    }
    runs
}

/// FDSelect: GID → Font DICT, as format 0 or format 3 (TN#5176 §19),
/// whichever is smaller.
fn fd_select(glyphs: &[Glyph]) -> Vec<u8> {
    let mut ranges: Vec<(u16, u8)> = Vec::new();
    for (gid, g) in glyphs.iter().enumerate() {
        if ranges.last().is_none_or(|&(_, fd)| fd != g.fd) {
            ranges.push((gid as u16, g.fd));
        }
    }
    let format0 = glyphs.len();
    let format3 = 2 + 3 * ranges.len() + 2;
    if format0 <= format3 {
        let mut out = Vec::with_capacity(1 + format0);
        out.push(0);
        out.extend(glyphs.iter().map(|g| g.fd));
        return out;
    }
    let mut out = Vec::with_capacity(1 + format3);
    out.push(3);
    out.extend_from_slice(&(ranges.len() as u16).to_be_bytes());
    for (first, fd) in ranges {
        out.extend_from_slice(&first.to_be_bytes());
        out.push(fd);
    }
    // The sentinel is the glyph count, one past the last GID.
    out.extend_from_slice(&(glyphs.len() as u16).to_be_bytes());
    out
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// Read the CID-keyed font named `name` from CFF `data` (a FontSet may hold
/// several fonts) as a [`CidFont`], with everything [`write_cid_font`]
/// writes: Private DICT hint values, Font DICT names and matrices, and
/// subroutines.
///
/// Fails when the font is missing, not CID-keyed, or malformed.
pub fn read_cid_font(data: &[u8], name: &str) -> Result<CidFont, String> {
    let header_size = usize::from(*data.get(2).ok_or("CFF: truncated header")?);
    let (names, at) = parse_index(data, header_size)?;
    let (top_dicts, at) = parse_index(data, at)?;
    let (strings, at) = parse_index(data, at)?;
    let (global_subrs, _) = parse_index(data, at)?;

    let index = names
        .iter()
        .position(|n| n == name.as_bytes())
        .ok_or_else(|| format!("CFF: no font named {name}"))?;
    let top = parse_dict_data(top_dicts.get(index).ok_or("CFF: no Top DICT")?);
    let ros = dict_get(&top, esc_op(30)).ok_or("CFF: not a CIDFont (no ROS)")?;
    let [registry, ordering, supplement] = ros[..] else {
        return Err("CFF: malformed ROS".into());
    };
    let sid = |v: f64| get_sid_string(v as u16, &strings);

    let mut font = CidFont::new(name, &sid(registry), &sid(ordering), supplement as i32);
    if let Some(m) = matrix(&top) {
        font.font_matrix = m;
    }
    if let Some(&[a, b, c, d]) = dict_get(&top, DictOp::OneByte(5)) {
        font.font_bbox = [a, b, c, d];
    }
    font.global_subrs = global_subrs;

    let char_strings = offset(&top, DictOp::OneByte(17))
        .map(|at| parse_index(data, at))
        .transpose()?
        .ok_or("CFF: no CharStrings")?
        .0;
    let n_glyphs = char_strings.len();
    let charset = offset(&top, DictOp::OneByte(15)).ok_or("CFF: no charset")?;
    let mut gid_to_cid = vec![0u16; n_glyphs];
    for (cid, &gid) in build_cid_to_gid(data, charset, n_glyphs)?
        .iter()
        .enumerate()
    {
        if let Some(slot) = gid_to_cid.get_mut(usize::from(gid)) {
            *slot = cid as u16;
        }
    }
    let fd_select = match offset(&top, esc_op(37)) {
        Some(at) => parse_fd_select(data, at, n_glyphs)?,
        None => vec![0; n_glyphs],
    };
    font.cid_count = match dict_get(&top, esc_op(34)).and_then(|v| v.first()) {
        Some(&n) => dict_usize(n).ok_or("CFF: bad CIDCount")? as u32,
        None => 8720,
    };

    let fd_array = offset(&top, esc_op(36)).ok_or("CFF: no FDArray")?;
    for fd_data in parse_index(data, fd_array)?.0 {
        let fd = parse_dict_data(&fd_data);
        let mut font_dict = FontDict {
            name: dict_get(&fd, esc_op(38))
                .and_then(|v| v.first())
                .map(|&v| sid(v)),
            font_matrix: matrix(&fd),
            ..FontDict::default()
        };
        if let Some(&[size, at]) = dict_get(&fd, DictOp::OneByte(18)) {
            let (size, at) = (
                dict_usize(size).ok_or("CFF: bad Private size")?,
                dict_usize(at).ok_or("CFF: bad Private offset")?,
            );
            let private = at
                .checked_add(size)
                .and_then(|end| data.get(at..end))
                .ok_or("CFF: Private DICT outside the data")?;
            let private = parse_dict_data(private);
            font_dict.private = private_dict(&private);
            if let Some(rel) = offset(&private, DictOp::OneByte(19)) {
                let subrs = at.checked_add(rel).ok_or("CFF: bad Subrs offset")?;
                font_dict.local_subrs = parse_index(data, subrs)?.0;
            }
        }
        font.font_dicts.push(font_dict);
    }
    if font.font_dicts.len() > 256 {
        return Err("CFF: more than 256 Font DICTs".into());
    }

    for (gid, charstring) in char_strings.into_iter().enumerate() {
        let fd = *fd_select.get(gid).unwrap_or(&0);
        font.glyphs
            .push(Glyph::new(gid_to_cid[gid], fd, charstring));
    }
    // Readers look glyphs up by CID; the writer wants them in CID order.
    font.glyphs.sort_by_key(|g| g.cid);
    font.glyphs.dedup_by_key(|g| g.cid);
    Ok(font)
}

/// The DICT operator `12 b`, in the parser's terms.
fn esc_op(b: u8) -> DictOp {
    DictOp::TwoByte(12, b)
}

/// An offset operand.
fn offset(dict: &[DictEntry], op: DictOp) -> Option<usize> {
    dict_get(dict, op)?.first().copied().and_then(dict_usize)
}

/// A `FontMatrix` (12 7) with its six operands.
fn matrix(dict: &[DictEntry]) -> Option<[f64; 6]> {
    match dict_get(dict, esc_op(7))? {
        &[a, b, c, d, e, f] => Some([a, b, c, d, e, f]),
        _ => None,
    }
}

/// The values of a Private DICT (TN#5176 Table 23), undoing the delta
/// encoding of its arrays.
fn private_dict(dict: &[DictEntry]) -> PrivateDict {
    let delta = |op: DictOp| -> Vec<f64> {
        let mut sum = 0.0;
        dict_get(dict, op)
            .unwrap_or_default()
            .iter()
            .map(|&d| {
                sum += d;
                sum
            })
            .collect()
    };
    let number = |op: DictOp| dict_get(dict, op).and_then(|v| v.first().copied());
    PrivateDict {
        blue_values: delta(DictOp::OneByte(6)),
        other_blues: delta(DictOp::OneByte(7)),
        family_blues: delta(DictOp::OneByte(8)),
        family_other_blues: delta(DictOp::OneByte(9)),
        blue_scale: number(esc_op(9)),
        blue_shift: number(esc_op(10)),
        blue_fuzz: number(esc_op(11)),
        std_hw: number(DictOp::OneByte(10)),
        std_vw: number(DictOp::OneByte(11)),
        stem_snap_h: delta(esc_op(12)),
        stem_snap_v: delta(esc_op(13)),
        force_bold: number(esc_op(14)).is_some_and(|v| v != 0.0),
        language_group: number(esc_op(17)).map(|v| v as i32),
        expansion_factor: number(esc_op(18)),
        initial_random_seed: number(esc_op(19)),
        default_width_x: number(DictOp::OneByte(20)).unwrap_or(0.0),
        nominal_width_x: number(DictOp::OneByte(21)).unwrap_or(0.0),
    }
}

// ---------------------------------------------------------------------------
// Strings
// ---------------------------------------------------------------------------

/// String INDEX under construction: SIDs past the standard strings.
#[derive(Default)]
struct StringTable {
    custom: Vec<Vec<u8>>,
}

impl StringTable {
    /// The SID for `s`: a standard string's own SID, or a String INDEX
    /// entry, added once.
    fn sid(&mut self, s: &str) -> Result<u16, String> {
        let idx = match STANDARD_STRINGS.iter().position(|&std| std == s) {
            Some(sid) => sid,
            None => {
                let custom = match self.custom.iter().position(|c| c == s.as_bytes()) {
                    Some(i) => i,
                    None => {
                        self.custom.push(s.as_bytes().to_vec());
                        self.custom.len() - 1
                    }
                };
                STANDARD_STRINGS.len() + custom
            }
        };
        // SIDs 65000 and above are reserved for implementations (§10).
        if idx >= 65000 {
            return Err("CFF: more than 65000 strings".into());
        }
        Ok(idx as u16)
    }
}

// ---------------------------------------------------------------------------
// INDEX
// ---------------------------------------------------------------------------

/// Serialise `items` as an INDEX (TN#5176 §5).
fn index<T: AsRef<[u8]>>(items: &[T]) -> Result<Vec<u8>, String> {
    if items.len() > MAX_INDEX_COUNT {
        return Err(format!(
            "CFF: INDEX of {} objects (at most 65535)",
            items.len()
        ));
    }
    let data_len: usize = items.iter().map(|i| i.as_ref().len()).sum();
    // Offsets count from the byte before the data, so the last is one
    // past its length.
    let last = data_len + 1;
    if last > u32::MAX as usize {
        return Err("CFF: INDEX data exceeds 4 GiB".into());
    }
    let mut out = Vec::with_capacity(index_len(
        &items.iter().map(|i| i.as_ref().len()).collect::<Vec<_>>(),
    ));
    out.extend_from_slice(&(items.len() as u16).to_be_bytes());
    // An empty INDEX is its count alone.
    if items.is_empty() {
        return Ok(out);
    }
    let size = offset_size(last);
    out.push(size);
    let mut offset = 1;
    push_offset(&mut out, offset, size);
    for item in items {
        offset += item.as_ref().len();
        push_offset(&mut out, offset, size);
    }
    for item in items {
        out.extend_from_slice(item.as_ref());
    }
    Ok(out)
}

/// Serialised length of an INDEX whose objects have lengths `lens`.
fn index_len(lens: &[usize]) -> usize {
    if lens.is_empty() {
        return 2;
    }
    let data: usize = lens.iter().sum();
    2 + 1 + (lens.len() + 1) * usize::from(offset_size(data + 1)) + data
}

/// Bytes needed for an offset of `max`.
fn offset_size(max: usize) -> u8 {
    match max {
        0..=0xFF => 1,
        0x100..=0xFFFF => 2,
        0x1_0000..=0xFF_FFFF => 3,
        _ => 4,
    }
}

/// Append `value` big-endian in `size` bytes.
fn push_offset(out: &mut Vec<u8>, value: usize, size: u8) {
    let bytes = (value as u32).to_be_bytes();
    out.extend_from_slice(&bytes[4 - usize::from(size)..]);
}

// ---------------------------------------------------------------------------
// DICT encoding
// ---------------------------------------------------------------------------

/// DICT data under construction (TN#5176 §4).
#[derive(Default)]
struct DictWriter(Vec<u8>);

impl DictWriter {
    /// An operator: one byte, or the escape byte 12 and a second byte.
    fn op(&mut self, op: u16) {
        if op > 0xFF {
            self.0.push(12);
        }
        self.0.push(op as u8);
    }

    /// An integer in its shortest encoding (Table 3).
    fn int(&mut self, v: i32) {
        match v {
            -107..=107 => self.0.push((v + 139) as u8),
            108..=1131 => {
                let v = v - 108;
                self.0
                    .extend_from_slice(&[(v / 256 + 247) as u8, (v % 256) as u8]);
            }
            -1131..=-108 => {
                let v = -v - 108;
                self.0
                    .extend_from_slice(&[(v / 256 + 251) as u8, (v % 256) as u8]);
            }
            -32768..=32767 => {
                self.0.push(28);
                self.0.extend_from_slice(&(v as i16).to_be_bytes());
            }
            _ => {
                self.0.push(29);
                self.0.extend_from_slice(&v.to_be_bytes());
            }
        }
    }

    /// An offset or length, always in the 5-byte form so that its length
    /// does not depend on its value.
    fn offset(&mut self, v: usize) {
        self.0.push(29);
        self.0.extend_from_slice(&(v as u32).to_be_bytes());
    }

    /// A number: an integer when it is integral and in range, otherwise a
    /// real.
    fn number(&mut self, v: f64) {
        if v.fract() == 0.0 && v >= f64::from(i32::MIN) && v <= f64::from(i32::MAX) {
            self.int(v as i32);
        } else {
            self.real(v);
        }
    }

    /// Several numbers.
    fn numbers(&mut self, vals: &[f64]) {
        for &v in vals {
            self.number(v);
        }
    }

    /// A delta-encoded array and its operator, or nothing if it is empty.
    fn delta(&mut self, vals: &[f64], op: u16) {
        if vals.is_empty() {
            return;
        }
        let mut prev = 0.0;
        for &v in vals {
            self.number(v - prev);
            prev = v;
        }
        self.op(op);
    }

    /// A real number as BCD nibbles (Table 5), from the shorter of Rust's
    /// plain and exponent forms. Both are the shortest decimal that
    /// round-trips, so a parsed real is written back as it was read.
    fn real(&mut self, v: f64) {
        let v = if v.is_finite() { v } else { 0.0 };
        let plain = format!("{v}");
        let exp = format!("{v:e}");
        let text = if exp.len() < plain.len() { exp } else { plain };

        let mut nibbles = Vec::with_capacity(text.len() + 2);
        let mut chars = text.bytes().peekable();
        while let Some(c) = chars.next() {
            match c {
                b'0'..=b'9' => nibbles.push(c - b'0'),
                b'.' => nibbles.push(0xA),
                b'-' => nibbles.push(0xE),
                b'e' | b'E' if chars.peek() == Some(&b'-') => {
                    chars.next();
                    nibbles.push(0xC);
                }
                b'e' | b'E' => nibbles.push(0xB),
                _ => {}
            }
        }
        // End of number, padded to a whole byte.
        nibbles.push(0xF);
        if nibbles.len() % 2 == 1 {
            nibbles.push(0xF);
        }
        self.0.push(30);
        for pair in nibbles.chunks(2) {
            self.0.push((pair[0] << 4) | pair[1]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cff_parser::parse_cff;
    use crate::type2_charstring::execute_type2_charstring;
    use crate::{PathSegment, PsPath};

    /// A Type 2 charstring: operands in the 1-byte range, then operators.
    fn cs(parts: &[i32]) -> Vec<u8> {
        // Operands are -107..=107; operators are passed as 1000 + op.
        parts
            .iter()
            .map(|&p| {
                if p >= 1000 {
                    (p - 1000) as u8
                } else {
                    (p + 139) as u8
                }
            })
            .collect()
    }

    const RMOVETO: i32 = 1021;
    const RLINETO: i32 = 1005;
    const ENDCHAR: i32 = 1014;
    const CALLSUBR: i32 = 1010;
    const CALLGSUBR: i32 = 1029;
    const RETURN: i32 = 1011;

    fn dict_bytes(f: impl FnOnce(&mut DictWriter)) -> Vec<u8> {
        let mut d = DictWriter::default();
        f(&mut d);
        d.0
    }

    #[test]
    fn integers_use_the_table_4_encodings() {
        for (v, bytes) in [
            (0, &[0x8b][..]),
            (100, &[0xef]),
            (-100, &[0x27]),
            (1000, &[0xfa, 0x7c]),
            (-1000, &[0xfe, 0x7c]),
            (10000, &[0x1c, 0x27, 0x10]),
            (-10000, &[0x1c, 0xd8, 0xf0]),
            (100000, &[0x1d, 0x00, 0x01, 0x86, 0xa0]),
            (-100000, &[0x1d, 0xff, 0xfe, 0x79, 0x60]),
        ] {
            assert_eq!(dict_bytes(|d| d.int(v)), bytes, "{v}");
        }
    }

    #[test]
    fn reals_use_the_nibble_encoding() {
        // The two examples in TN#5176 §4.
        assert_eq!(dict_bytes(|d| d.real(-2.25)), [0x1e, 0xe2, 0xa2, 0x5f]);
        assert_eq!(
            dict_bytes(|d| d.real(0.140541e-3)),
            [0x1e, 0x1a, 0x40, 0x54, 0x1c, 0x4f]
        );
        assert_eq!(dict_bytes(|d| d.real(0.039625)), {
            let mut v = vec![0x1e, 0x0a, 0x03, 0x96, 0x25];
            v.push(0xff);
            v
        });
    }

    #[test]
    fn index_offsets_count_from_the_byte_before_the_data() {
        assert_eq!(index::<&[u8]>(&[]).unwrap(), [0, 0]);
        assert_eq!(
            index(&[&b"ab"[..], &b"c"[..]]).unwrap(),
            [0, 2, 1, 1, 3, 4, b'a', b'b', b'c']
        );
        let big = vec![0u8; 300];
        let idx = index(&[&big]).unwrap();
        assert_eq!(&idx[..7], [0, 1, 2, 0, 1, 0x01, 0x2d]);
        assert_eq!(idx.len(), index_len(&[300]));
    }

    fn square(size: i32) -> Vec<i32> {
        vec![0, 0, RMOVETO, size, 0, 0, size, -size, 0, RLINETO, ENDCHAR]
    }

    fn outline(font: &crate::cff_parser::CffFont, gid: usize) -> (f64, PsPath) {
        let fd = &font.fd_array[font.fd_select[gid] as usize];
        let r = execute_type2_charstring(
            &font.char_strings[gid],
            &fd.local_subrs,
            &font.global_subrs,
            fd.default_width_x,
            fd.nominal_width_x,
            false,
        )
        .unwrap();
        (r.width_x, r.path)
    }

    fn two_fd_font() -> CidFont {
        let mut font = CidFont::new("Test-CID", "Adobe", "Identity", 0);
        font.font_matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        font.font_bbox = [-50.0, -200.0, 1000.0, 880.0];
        font.cid_count = 20;
        font.global_subrs = vec![cs(&[50, 0, RLINETO, RETURN])];

        let mut fd0 = FontDict {
            name: Some("Test-CID-Alpha".into()),
            font_matrix: Some([0.001, 0.0, 0.0, 0.001, 0.0, 0.0]),
            ..FontDict::default()
        };
        fd0.private.default_width_x = 500.0;
        fd0.private.nominal_width_x = 600.0;
        fd0.private.blue_values = vec![-12.0, 0.0, 700.0, 712.0];
        fd0.private.blue_scale = Some(0.039625);
        fd0.private.std_vw = Some(80.0);
        fd0.private.stem_snap_v = vec![80.0, 92.0];
        fd0.private.language_group = Some(1);
        fd0.local_subrs = vec![cs(&[0, 50, RLINETO, RETURN])];

        let mut fd1 = FontDict {
            font_matrix: Some([0.0005, 0.0, 0.0, 0.0005, 0.0, 0.0]),
            ..FontDict::default()
        };
        fd1.private.force_bold = true;

        font.font_dicts = vec![fd0, fd1];

        // CID 0: default width; CID 3: width 600 + 40; CIDs 4..=6 in FD 1;
        // CID 10 calls a local and a global subr (bias 107 for both).
        let mut subrs = vec![40];
        subrs.extend(square(100));
        font.glyphs = vec![
            Glyph::new(0, 0, cs(&square(10))),
            Glyph::new(3, 0, cs(&subrs)),
            Glyph::new(4, 1, cs(&square(20))),
            Glyph::new(5, 1, cs(&square(30))),
            Glyph::new(6, 1, cs(&square(40))),
            Glyph::new(
                10,
                0,
                cs(&[0, 0, RMOVETO, -107, CALLSUBR, -107, CALLGSUBR, ENDCHAR]),
            ),
        ];
        font
    }

    #[test]
    fn a_written_font_parses_back() {
        let font = two_fd_font();
        let data = write_cid_font(&font).unwrap();
        let parsed = parse_cff(&data).unwrap();
        assert_eq!(parsed.len(), 1);
        let p = &parsed[0];

        assert_eq!(p.name, "Test-CID");
        assert!(p.is_cid);
        assert_eq!(p.ros, Some(("Adobe".into(), "Identity".into(), 0)));
        assert_eq!(p.font_matrix, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        assert_eq!(p.font_bbox, [-50.0, -200.0, 1000.0, 880.0]);
        assert_eq!(p.char_strings.len(), 6);
        assert_eq!(p.global_subrs, font.global_subrs);
        assert_eq!(p.fd_select, [0, 0, 1, 1, 1, 0]);
        for (cid, gid) in [(0, 0), (3, 1), (4, 2), (5, 3), (6, 4), (10, 5)] {
            assert_eq!(p.cid_to_gid[cid], gid, "CID {cid}");
        }
        assert_eq!(p.cid_to_gid[1], 0xFFFF);

        assert_eq!(p.fd_array.len(), 2);
        assert_eq!(p.fd_array[0].default_width_x, 500.0);
        assert_eq!(p.fd_array[0].nominal_width_x, 600.0);
        assert_eq!(p.fd_array[0].local_subrs, font.font_dicts[0].local_subrs);
        assert_eq!(
            p.fd_array[0].font_matrix,
            Some([0.001, 0.0, 0.0, 0.001, 0.0, 0.0])
        );
        assert_eq!(
            p.fd_array[1].font_matrix,
            Some([0.0005, 0.0, 0.0, 0.0005, 0.0, 0.0])
        );
        assert!(p.fd_array[1].local_subrs.is_empty());

        assert_eq!(outline(p, 0).0, 500.0);
        assert_eq!(outline(p, 1).0, 640.0);
        let (_, path) = outline(p, 5);
        let ends: Vec<(f64, f64)> = path
            .segments
            .iter()
            .filter_map(|s| match *s {
                PathSegment::LineTo(x, y) => Some((x, y)),
                _ => None,
            })
            .collect();
        assert_eq!(ends, [(0.0, 50.0), (50.0, 50.0)]);
    }

    #[test]
    fn a_written_font_reads_back_whole() {
        let font = two_fd_font();
        let read = read_cid_font(&write_cid_font(&font).unwrap(), "Test-CID").unwrap();
        assert_eq!(read, font);
        assert!(read_cid_font(&write_cid_font(&font).unwrap(), "Other").is_err());
    }

    #[test]
    fn subset_keeps_cid_0_and_renumbers_font_dicts() {
        let font = two_fd_font();
        // CIDs 4 and 5 are in FD 1 only.
        let sub = font.subset(|cid| cid == 4 || cid == 5);
        let cids: Vec<(u16, u8)> = sub.glyphs.iter().map(|g| (g.cid, g.fd)).collect();
        assert_eq!(cids, [(0, 0), (4, 1), (5, 1)]);
        assert_eq!(sub.font_dicts.len(), 2);

        let only_notdef = font.subset(|_| false);
        assert_eq!(only_notdef.glyphs.len(), 1);
        assert_eq!(only_notdef.font_dicts, font.font_dicts[..1]);

        // FD 1 alone: renumbered to 0.
        let mut font = font;
        font.glyphs[0].fd = 1;
        let sub = font.subset(|cid| cid == 6);
        assert_eq!(sub.font_dicts, font.font_dicts[1..]);
        assert!(sub.glyphs.iter().all(|g| g.fd == 0));
        let parsed = parse_cff(&write_cid_font(&sub).unwrap()).unwrap();
        assert_eq!(parsed[0].char_strings.len(), 2);
    }

    #[test]
    fn private_dict_values_are_written_as_given() {
        let font = two_fd_font();
        let (private, subrs) = private_and_subrs(&font.font_dicts[0]).unwrap();
        let mut expected = DictWriter::default();
        for v in [-12, 12, 700, 12] {
            expected.int(v);
        }
        expected.op(OP_BLUE_VALUES);
        expected.real(0.039625);
        expected.op(OP_BLUE_SCALE);
        expected.int(80);
        expected.op(OP_STD_VW);
        expected.int(80);
        expected.int(12);
        expected.op(OP_STEM_SNAP_V);
        expected.int(1);
        expected.op(OP_LANGUAGE_GROUP);
        expected.int(500);
        expected.op(OP_DEFAULT_WIDTH_X);
        expected.int(600);
        expected.op(OP_NOMINAL_WIDTH_X);
        let len = expected.0.len() + 6;
        expected.offset(len);
        expected.op(OP_SUBRS);
        assert_eq!(private, expected.0);
        assert_eq!(subrs, index(&font.font_dicts[0].local_subrs).unwrap());

        let (private, subrs) = private_and_subrs(&font.font_dicts[1]).unwrap();
        assert_eq!(private, [0x8c, 12, 14]);
        assert!(subrs.is_empty());
    }

    #[test]
    fn standard_strings_are_not_duplicated() {
        let mut t = StringTable::default();
        assert_eq!(t.sid(".notdef").unwrap(), 0);
        assert_eq!(t.sid("Regular").unwrap(), 388);
        assert_eq!(t.sid("Adobe").unwrap(), 391);
        assert_eq!(t.sid("Japan1").unwrap(), 392);
        assert_eq!(t.sid("Adobe").unwrap(), 391);
        assert_eq!(t.custom.len(), 2);
    }

    #[test]
    fn charset_picks_the_smallest_format() {
        let glyphs = |cids: &[u16]| -> Vec<Glyph> {
            cids.iter().map(|&c| Glyph::new(c, 0, vec![14])).collect()
        };
        // Scattered CIDs: format 0.
        assert_eq!(charset(&glyphs(&[0, 5, 9])), [0, 0, 5, 0, 9]);
        // A run of up to 256: format 1.
        let run: Vec<u16> = (0..=100).collect();
        assert_eq!(charset(&glyphs(&run)), [1, 0, 1, 99]);
        // Runs of 256 or fewer, where format 1 is smaller than format 2.
        let mut two: Vec<u16> = (0..=256).collect();
        two.extend(300..=400);
        assert_eq!(charset(&glyphs(&two)), [1, 0, 1, 255, 1, 44, 100]);
        // A run longer than 256: format 2.
        let run: Vec<u16> = (0..=2000).collect();
        assert_eq!(charset(&glyphs(&run)), [2, 0, 1, 0x07, 0xcf]);
    }

    #[test]
    fn fd_select_picks_the_smallest_format() {
        let glyphs = |fds: &[u8]| -> Vec<Glyph> {
            fds.iter()
                .enumerate()
                .map(|(i, &fd)| Glyph::new(i as u16, fd, vec![14]))
                .collect()
        };
        assert_eq!(fd_select(&glyphs(&[0, 1, 0])), [0, 0, 1, 0]);
        let fds = [[0u8; 10], [1; 10]].concat();
        assert_eq!(
            fd_select(&glyphs(&fds)),
            [3, 0, 2, 0, 0, 0, 0, 10, 1, 0, 20]
        );
    }

    #[test]
    fn invalid_fonts_are_rejected() {
        let ok = two_fd_font();
        assert!(write_cid_font(&ok).is_ok());

        let mut f = ok.clone();
        f.font_dicts.clear();
        assert!(write_cid_font(&f).is_err());

        let mut f = ok.clone();
        f.glyphs.remove(0);
        assert!(write_cid_font(&f).unwrap_err().contains("GID 0"));

        let mut f = ok.clone();
        f.glyphs.swap(1, 2);
        assert!(write_cid_font(&f).unwrap_err().contains("increase"));

        let mut f = ok.clone();
        f.cid_count = 10;
        assert!(write_cid_font(&f).unwrap_err().contains("CIDCount"));

        let mut f = ok;
        f.glyphs[1].fd = 2;
        assert!(write_cid_font(&f).unwrap_err().contains("Font DICT 2"));
    }
}
