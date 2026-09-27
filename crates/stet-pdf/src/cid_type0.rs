// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PostScript CIDFontType 0 fonts as PDF `CIDFontType0` fonts.
//!
//! PDF has no Type 1 CIDFont: a `CIDFontType0` descendant embeds CFF
//! (`FontFile3` with `/Subtype /CIDFontType0C`). Both PostScript forms end
//! up there, subset to the CIDs the document uses:
//!
//! - **Type 1 charstrings in `GlyphData`** (what `CIDInit`'s `StartData`
//!   builds, and how pdftops writes CID fonts): each charstring is converted
//!   to Type 2 by [`stet_fonts::type1_to_type2`] and the Private dict hints
//!   are carried into the CFF Private DICTs.
//! - **CFF loaded through `FontSetInit`** (`_CFFData`): the font is read
//!   back out of its FontSet and written again with only the used glyphs.
//!
//! **Matrices.** A glyph reaches the CIDFont's font space through its FD's
//! `FontMatrix` and then the CIDFont's. PDF readers agree on the CFF Top
//! DICT `FontMatrix` but not on how a Font DICT's own matrix combines with
//! it, so the GlyphData path writes one combined matrix at the top and
//! none per Font DICT. Glyphs whose FD combines to a different matrix than
//! the one chosen — no corpus font has one — get the difference folded into
//! their outlines. The CFF path keeps the source font's matrices as its
//! producer wrote them.
//!
//! **Widths** are the charstring advances through that matrix, in the
//! thousandths of text space `/W` counts in. They are also the widths the
//! content stream positions text with, so the two agree by construction.

use std::collections::{BTreeMap, HashMap, HashSet};

use stet_core::context::Context;
use stet_core::dict::DictKey;
use stet_core::object::{EntityId, PsObject, PsValue};
use stet_fonts::cff_writer::{self, CidFont, FontDict, Glyph, PrivateDict};
use stet_fonts::cid_type0::{self, CidMap};
use stet_fonts::cid_unicode;
use stet_fonts::geometry::Matrix;
use stet_fonts::type1_to_type2::{self, Type2Glyph};
use stet_fonts::type2_charstring;

use crate::font_tracker::FontUsage;
use crate::pdf_objects::PdfObj;
use crate::pdf_writer::PdfWriter;

/// Whether `cidfont` is a CIDFontType 0 font this module embeds.
pub(crate) fn handles(ctx: &Context, cidfont: EntityId) -> bool {
    glyph_data(ctx, cidfont).is_some() || cff_data(ctx, cidfont).is_some()
}

/// Widths of the used CIDs, in thousandths of text space, for positioning
/// text in the content stream.
pub(crate) fn widths(usage: &FontUsage, ctx: &Context, cidfont: EntityId) -> HashMap<u16, i32> {
    match prepare(ctx, cidfont, &usage.used_codes) {
        Ok(font) => usage
            .used_codes
            .iter()
            .map(|&cid| (cid, font.width_of(cid)))
            .collect(),
        Err(_) => HashMap::new(),
    }
}

/// Embed `cidfont` for `usage` as a Type 0 font over a `CIDFontType0`
/// descendant, returning the Type 0 font's object number.
pub(crate) fn build(
    writer: &mut PdfWriter,
    usage: &FontUsage,
    ctx: &Context,
    cidfont: EntityId,
) -> Option<u32> {
    let font = match prepare(ctx, cidfont, &usage.used_codes) {
        Ok(font) => font,
        Err(e) => {
            eprintln!(
                "[font_embedder] CIDFontType 0 {}: {e}",
                String::from_utf8_lossy(&usage.font_name)
            );
            return None;
        }
    };
    let cff = match cff_writer::write_cid_font(&font.cff) {
        Ok(cff) => cff,
        Err(e) => {
            eprintln!(
                "[font_embedder] CIDFontType 0 {}: {e}",
                String::from_utf8_lossy(&usage.font_name)
            );
            return None;
        }
    };

    let font_file = writer.add_stream(
        vec![(b"Subtype".to_vec(), PdfObj::name("CIDFontType0C"))],
        &cff,
        true,
    );
    let bbox = font.text_bbox();
    let descriptor = writer.add_object(&PdfObj::Dict(vec![
        (b"Type".to_vec(), PdfObj::name("FontDescriptor")),
        (b"FontName".to_vec(), PdfObj::Name(usage.font_name.clone())),
        (b"Flags".to_vec(), PdfObj::Int(0x0004)), // Symbolic
        (
            b"FontBBox".to_vec(),
            PdfObj::Array(bbox.iter().map(|&v| PdfObj::Int(v)).collect()),
        ),
        (b"ItalicAngle".to_vec(), PdfObj::Int(0)),
        (b"Ascent".to_vec(), PdfObj::Int(bbox[3])),
        (b"Descent".to_vec(), PdfObj::Int(bbox[1])),
        (b"CapHeight".to_vec(), PdfObj::Int(bbox[3])),
        (b"StemV".to_vec(), PdfObj::Int(80)),
        (b"FontFile3".to_vec(), PdfObj::Ref(font_file)),
    ]));

    let mut cid_font = vec![
        (b"Type".to_vec(), PdfObj::name("Font")),
        (b"Subtype".to_vec(), PdfObj::name("CIDFontType0")),
        (b"BaseFont".to_vec(), PdfObj::Name(usage.font_name.clone())),
        (
            b"CIDSystemInfo".to_vec(),
            PdfObj::Dict(vec![
                (
                    b"Registry".to_vec(),
                    PdfObj::LitString(font.cff.registry.as_bytes().to_vec()),
                ),
                (
                    b"Ordering".to_vec(),
                    PdfObj::LitString(font.cff.ordering.as_bytes().to_vec()),
                ),
                (
                    b"Supplement".to_vec(),
                    PdfObj::Int(i64::from(font.cff.supplement)),
                ),
            ]),
        ),
        (b"FontDescriptor".to_vec(), PdfObj::Ref(descriptor)),
        (b"DW".to_vec(), PdfObj::Int(i64::from(font.default_width))),
    ];
    let w = w_array(&font, &usage.used_codes);
    if !w.is_empty() {
        cid_font.push((b"W".to_vec(), PdfObj::Array(w)));
    }
    if usage.wmode == 1 {
        let w2 = crate::font_embedder::w2_array(
            ctx,
            cidfont,
            &usage.used_codes,
            &glyph_to_text(ctx, cidfont),
        );
        if !w2.is_empty() {
            cid_font.push((b"W2".to_vec(), PdfObj::Array(w2)));
        }
    }
    let cid_font = writer.add_object(&PdfObj::Dict(cid_font));

    let mut type0 = vec![
        (b"Type".to_vec(), PdfObj::name("Font")),
        (b"Subtype".to_vec(), PdfObj::name("Type0")),
        (b"BaseFont".to_vec(), PdfObj::Name(usage.font_name.clone())),
        (
            b"Encoding".to_vec(),
            crate::font_embedder::identity_cmap(usage),
        ),
        (
            b"DescendantFonts".to_vec(),
            PdfObj::Array(vec![PdfObj::Ref(cid_font)]),
        ),
    ];
    if let Some(cmap) = to_unicode(&font.cff, &usage.used_codes, &usage.font_name) {
        type0.push((
            b"ToUnicode".to_vec(),
            PdfObj::Ref(writer.add_stream(Vec::new(), &cmap, true)),
        ));
    }
    Some(writer.add_object(&PdfObj::Dict(type0)))
}

// ---------------------------------------------------------------------------
// Preparation
// ---------------------------------------------------------------------------

/// A subset font ready to write, with what the PDF font dicts need.
struct Prepared {
    cff: CidFont,
    /// The matrix from each output Font DICT's glyph space to font space.
    fd_matrices: Vec<Matrix>,
    /// Width of each glyph in the subset, in thousandths of text space.
    widths: BTreeMap<u16, i32>,
    /// Width of a CID the font has no glyph for: CID 0's.
    default_width: i32,
}

impl Prepared {
    /// The width text is set with for `cid`: its own, or CID 0's when the
    /// font lacks it, since CID 0 is what is shown (TN 5014 §2.5).
    fn width_of(&self, cid: u16) -> i32 {
        self.widths.get(&cid).copied().unwrap_or(self.default_width)
    }

    /// The bounding box of the subset's outlines in thousandths of text
    /// space, for the FontDescriptor.
    fn text_bbox(&self) -> [i64; 4] {
        let mut b = [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ];
        for g in &self.cff.glyphs {
            let fd = &self.cff.font_dicts[usize::from(g.fd)];
            let Ok(r) = type2_charstring::execute_type2_charstring(
                &g.charstring,
                &fd.local_subrs,
                &self.cff.global_subrs,
                fd.private.default_width_x,
                fd.private.nominal_width_x,
                false,
            ) else {
                continue;
            };
            let m = self.fd_matrices[usize::from(g.fd)];
            for (x, y) in points(&r.path) {
                let (x, y) = m.transform_point(x, y);
                b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
            }
        }
        if b[0] > b[2] {
            return [0; 4];
        }
        [
            (b[0] * 1000.0).floor() as i64,
            (b[1] * 1000.0).floor() as i64,
            (b[2] * 1000.0).ceil() as i64,
            (b[3] * 1000.0).ceil() as i64,
        ]
    }
}

/// Every point of a path, control points included.
fn points(path: &stet_fonts::PsPath) -> Vec<(f64, f64)> {
    use stet_fonts::PathSegment;
    let mut out = Vec::new();
    for seg in &path.segments {
        match *seg {
            PathSegment::MoveTo(x, y) | PathSegment::LineTo(x, y) => out.push((x, y)),
            PathSegment::CurveTo {
                x1,
                y1,
                x2,
                y2,
                x3,
                y3,
            } => out.extend([(x1, y1), (x2, y2), (x3, y3)]),
            PathSegment::ClosePath => {}
        }
    }
    out
}

fn prepare(ctx: &Context, cidfont: EntityId, used: &HashSet<u16>) -> Result<Prepared, String> {
    let mut font = if let Some(data) = glyph_data(ctx, cidfont) {
        prepare_glyph_data(ctx, cidfont, data, used)
    } else if let Some((data, name)) = cff_data(ctx, cidfont) {
        prepare_cff(data, &name, used)
    } else {
        Err("neither GlyphData nor CFF data".into())
    }?;
    // A `CDevProc` may have shown a glyph with another width.
    let fm = glyph_to_text(ctx, cidfont);
    for &cid in used {
        if let Some(w) = crate::font_embedder::shown_width(ctx, cidfont, cid, &fm) {
            font.widths.insert(cid, w);
        }
    }
    Ok(font)
}

/// The CIDFont's glyph space to text space, its `FontMatrix`: the space
/// the interpreter's glyph metrics are in.
fn glyph_to_text(ctx: &Context, cidfont: EntityId) -> Matrix {
    matrix(ctx, cidfont).unwrap_or(Matrix::scale(0.001, 0.001))
}

/// One `FDArray` font of a GlyphData CIDFont.
struct SourceFd {
    matrix: Matrix,
    len_iv: usize,
    subrs: Vec<Vec<u8>>,
    private: PrivateDict,
}

/// A GlyphData (Type 1 charstring) CIDFont.
fn prepare_glyph_data(
    ctx: &Context,
    cidfont: EntityId,
    data: &[u8],
    used: &HashSet<u16>,
) -> Result<Prepared, String> {
    let int = |key: &[u8]| {
        get(ctx, cidfont, key)
            .and_then(|o| o.as_i64())
            .and_then(|v| usize::try_from(v).ok())
    };
    let map = CidMap::new(
        int(b"CIDCount").ok_or("no CIDCount")?,
        int(b"CIDMapOffset").unwrap_or(0),
        int(b"FDBytes").ok_or("no FDBytes")?,
        int(b"GDBytes").ok_or("no GDBytes")?,
    )?;
    let fds = match get(ctx, cidfont, b"FDArray").map(|o| o.value) {
        Some(PsValue::Array { entity, start, len }) => (0..len)
            .map(|i| match ctx.arrays.get_element(entity, start + i).value {
                PsValue::Dict(fd) => source_fd(ctx, fd, data),
                _ => Err("FDArray entry is not a dict".to_string()),
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err("no FDArray".into()),
    };
    let font_matrix = matrix(ctx, cidfont).unwrap_or(Matrix::new(0.001, 0.0, 0.0, 0.001, 0.0, 0.0));
    let combined: Vec<Matrix> = fds
        .iter()
        .map(|fd| font_matrix.multiply(&fd.matrix))
        .collect();

    // The glyphs to embed: CID 0, which a CIDFont always has, and each used
    // CID the font has a glyph for.
    let mut cids: Vec<u16> = used.iter().copied().chain([0]).collect();
    cids.sort_unstable();
    cids.dedup();
    let found: Vec<(u16, usize, &[u8])> = cids
        .iter()
        .filter_map(|&cid| {
            let (fd, cs) = map.glyph(data, usize::from(cid))?;
            (fd < fds.len()).then_some((cid, fd, cs))
        })
        .collect();

    // The top matrix: the combined matrix of the FD most glyphs use.
    let mut uses = vec![0usize; fds.len()];
    for &(_, fd, _) in &found {
        uses[fd] += 1;
    }
    let top_fd = (0..fds.len()).max_by_key(|&fd| uses[fd]).unwrap_or(0);
    let top = combined.get(top_fd).copied().unwrap_or(font_matrix);
    let top_inverse = top.invert().ok_or("singular FontMatrix")?;

    // Convert, folding any FD whose matrix differs into its outlines.
    let mut fd_out: Vec<Option<usize>> = vec![None; fds.len()];
    let mut out_fds: Vec<usize> = Vec::new();
    let mut converted: Vec<(u16, usize, Type2Glyph)> = Vec::new();
    for (cid, fd, cs) in found {
        let fold = if same(&combined[fd], &top) {
            Matrix::identity()
        } else {
            top_inverse.multiply(&combined[fd])
        };
        let source = &fds[fd];
        let Ok(glyph) =
            type1_to_type2::convert_charstring_transformed(cs, &source.subrs, source.len_iv, &fold)
        else {
            // A broken charstring: the glyph is missing, so CID 0 shows.
            continue;
        };
        let out = *fd_out[fd].get_or_insert_with(|| {
            out_fds.push(fd);
            out_fds.len() - 1
        });
        converted.push((cid, out, glyph));
    }

    let mut cff = new_font(ctx, cidfont)?;
    cff.font_matrix = matrix_array(&top);
    cff.cid_count = cff
        .cid_count
        .max(map.cid_count() as u32)
        .max(converted.last().map_or(1, |&(cid, ..)| u32::from(cid) + 1));
    for &fd in &out_fds {
        let mut font_dict = FontDict::default();
        font_dict.private = fds[fd].private.clone();
        let widths: Vec<f64> = converted
            .iter()
            .filter(|&&(_, out, _)| out_fds[out] == fd)
            .map(|(_, _, g)| g.width)
            .collect();
        let (default_width_x, nominal_width_x) = type1_to_type2::choose_widths(&widths);
        font_dict.private.default_width_x = default_width_x;
        font_dict.private.nominal_width_x = nominal_width_x;
        cff.font_dicts.push(font_dict);
    }
    if cff.font_dicts.is_empty() {
        cff.font_dicts.push(FontDict::default());
    }
    if converted.first().is_none_or(|&(cid, ..)| cid != 0) {
        // CID 0 must exist; an empty .notdef stands in for a broken one.
        cff.glyphs.push(Glyph::new(0, 0, vec![14]));
    }
    let mut widths = BTreeMap::new();
    for (cid, out, glyph) in &converted {
        let private = &cff.font_dicts[*out].private;
        let charstring = glyph.charstring(private.default_width_x, private.nominal_width_x)?;
        cff.glyphs.push(Glyph::new(*cid, *out as u8, charstring));
        widths.insert(*cid, text_width(glyph.width, &top));
    }

    // Without a CID 0 either, the interpreter advances by `DW` (1000 when
    // absent) through the CIDFont's matrix.
    let dw = get(ctx, cidfont, b"DW")
        .and_then(|o| o.as_f64())
        .unwrap_or(1000.0);
    let default_width = widths
        .get(&0)
        .copied()
        .unwrap_or_else(|| text_width(dw, &font_matrix));
    let fd_matrices = vec![top; cff.font_dicts.len()];
    Ok(Prepared {
        cff,
        fd_matrices,
        widths,
        default_width,
    })
}

/// A CIDFont loaded from CFF through `FontSetInit`.
fn prepare_cff(data: &[u8], name: &str, used: &HashSet<u16>) -> Result<Prepared, String> {
    let full = cff_writer::read_cid_font(data, name)?;
    let mut cff = full.subset(|cid| used.contains(&cid));
    cff.cid_count = cff
        .cid_count
        .max(cff.glyphs.last().map_or(1, |g| u32::from(g.cid) + 1));

    let top = matrix_of(&cff.font_matrix);
    let fd_matrices: Vec<Matrix> = cff
        .font_dicts
        .iter()
        .map(|fd| match &fd.font_matrix {
            Some(m) => top.multiply(&matrix_of(m)),
            None => top,
        })
        .collect();

    let mut widths = BTreeMap::new();
    for g in &cff.glyphs {
        let fd = &cff.font_dicts[usize::from(g.fd)];
        let r = type2_charstring::execute_type2_charstring(
            &g.charstring,
            &fd.local_subrs,
            &cff.global_subrs,
            fd.private.default_width_x,
            fd.private.nominal_width_x,
            true,
        )?;
        widths.insert(
            g.cid,
            text_width(r.width_x, &fd_matrices[usize::from(g.fd)]),
        );
    }
    let default_width = widths.get(&0).copied().unwrap_or(0);
    Ok(Prepared {
        cff,
        fd_matrices,
        widths,
        default_width,
    })
}

/// A glyph advance in thousandths of text space.
fn text_width(width: f64, m: &Matrix) -> i32 {
    (m.transform_delta(width, 0.0).0 * 1000.0).round() as i32
}

// ---------------------------------------------------------------------------
// PostScript dictionaries
// ---------------------------------------------------------------------------

fn get(ctx: &Context, dict: EntityId, key: &[u8]) -> Option<PsObject> {
    let name = ctx.names.find(key)?;
    ctx.dicts.get(dict, &DictKey::Name(name))
}

/// The CIDFont's `GlyphData` string.
fn glyph_data(ctx: &Context, cidfont: EntityId) -> Option<&[u8]> {
    match get(ctx, cidfont, b"GlyphData")?.value {
        PsValue::String { entity, start, len } => Some(ctx.strings.get(entity, start, len)),
        _ => None,
    }
}

/// The FontSet a CFF CIDFont was loaded from, and its name there.
fn cff_data(ctx: &Context, cidfont: EntityId) -> Option<(&[u8], String)> {
    let data = match get(ctx, cidfont, b"_CFFData")?.value {
        PsValue::String { entity, start, len } => ctx.strings.get(entity, start, len),
        _ => return None,
    };
    let name = get(ctx, cidfont, b"FontName")
        .or_else(|| get(ctx, cidfont, b"CIDFontName"))
        .and_then(|o| bytes(ctx, &o))?;
    Some((data, String::from_utf8_lossy(&name).into_owned()))
}

/// The bytes of a name or string.
fn bytes(ctx: &Context, obj: &PsObject) -> Option<Vec<u8>> {
    match obj.value {
        PsValue::Name(id) => Some(ctx.names.get_bytes(id).to_vec()),
        PsValue::String { entity, start, len } => {
            Some(ctx.strings.get(entity, start, len).to_vec())
        }
        _ => None,
    }
}

/// The numbers of an array.
fn numbers(ctx: &Context, obj: &PsObject) -> Option<Vec<f64>> {
    match obj.value {
        PsValue::Array { entity, start, len } | PsValue::PackedArray { entity, start, len } => (0
            ..len)
            .map(|i| ctx.arrays.get_element(entity, start + i).as_f64())
            .collect(),
        _ => None,
    }
}

/// A dict's `FontMatrix`.
fn matrix(ctx: &Context, dict: EntityId) -> Option<Matrix> {
    match numbers(ctx, &get(ctx, dict, b"FontMatrix")?)?[..] {
        [a, b, c, d, tx, ty] => Some(Matrix::new(a, b, c, d, tx, ty)),
        _ => None,
    }
}

/// A CFF font named after the CIDFont, with its `CIDSystemInfo`,
/// `CIDCount` and `FontBBox`.
fn new_font(ctx: &Context, cidfont: EntityId) -> Result<CidFont, String> {
    let name = get(ctx, cidfont, b"CIDFontName")
        .and_then(|o| bytes(ctx, &o))
        .unwrap_or_else(|| b"CIDFont".to_vec());
    let info = match get(ctx, cidfont, b"CIDSystemInfo").map(|o| o.value) {
        Some(PsValue::Dict(info)) => Some(info),
        _ => None,
    };
    let entry = |key: &[u8]| {
        info.and_then(|i| get(ctx, i, key))
            .and_then(|o| bytes(ctx, &o))
            .map(|b| String::from_utf8_lossy(&b).into_owned())
    };
    let supplement = info
        .and_then(|i| get(ctx, i, b"Supplement"))
        .and_then(|o| o.as_i32())
        .unwrap_or(0);
    let mut font = CidFont::new(
        &cff_name(&name),
        &entry(b"Registry").unwrap_or_else(|| "Adobe".into()),
        &entry(b"Ordering").unwrap_or_else(|| "Identity".into()),
        supplement,
    );
    if let Some(&[a, b, c, d]) = get(ctx, cidfont, b"FontBBox")
        .and_then(|o| numbers(ctx, &o))
        .as_deref()
    {
        font.font_bbox = [a, b, c, d];
    }
    Ok(font)
}

/// A name the CFF Name INDEX accepts: printable ASCII without the
/// delimiters TN#5176 §7 excludes.
fn cff_name(name: &[u8]) -> String {
    let name: String = name
        .iter()
        .map(|&b| match b {
            33..=126 if !b"[](){}<>/%".contains(&b) => b as char,
            _ => '-',
        })
        .take(127)
        .collect();
    if name.is_empty() {
        "CIDFont".into()
    } else {
        name
    }
}

/// Resolve one `FDArray` font against the glyph data its subroutines live
/// in.
fn source_fd(ctx: &Context, fd: EntityId, data: &[u8]) -> Result<SourceFd, String> {
    let matrix = matrix(ctx, fd).unwrap_or(Matrix::identity());
    let Some(PsValue::Dict(private)) = get(ctx, fd, b"Private").map(|o| o.value) else {
        return Ok(SourceFd {
            matrix,
            len_iv: 4,
            subrs: Vec::new(),
            private: PrivateDict::default(),
        });
    };
    let int = |key: &[u8]| get(ctx, private, key).and_then(|o| o.as_i64());
    let len_iv = match int(b"lenIV") {
        Some(v) if v < 0 => usize::MAX,
        Some(v) => usize::try_from(v).map_err(|_| "bad lenIV")?,
        None => 4,
    };
    // Subroutines only when SubrCount is present, as in Ghostscript.
    let subrs = match int(b"SubrCount") {
        Some(count) => {
            let offset = |key: &[u8]| {
                int(key)
                    .and_then(|v| usize::try_from(v).ok())
                    .ok_or_else(|| format!("bad {}", String::from_utf8_lossy(key)))
            };
            cid_type0::subrs(
                data,
                offset(b"SubrMapOffset")?,
                offset(b"SDBytes")?,
                usize::try_from(count).map_err(|_| "bad SubrCount")?,
            )?
        }
        None => Vec::new(),
    };
    Ok(SourceFd {
        matrix,
        len_iv,
        subrs,
        private: private_dict(ctx, private),
    })
}

/// The hint values of a Type 1 Private dict, in CFF terms.
fn private_dict(ctx: &Context, private: EntityId) -> PrivateDict {
    let array = |key: &[u8]| {
        get(ctx, private, key)
            .and_then(|o| numbers(ctx, &o))
            .unwrap_or_default()
    };
    let number = |key: &[u8]| get(ctx, private, key).and_then(|o| o.as_f64());
    // StdHW and StdVW are one-element arrays in Type 1, numbers in CFF.
    let first = |key: &[u8]| array(key).first().copied().or_else(|| number(key));
    let mut p = PrivateDict::default();
    p.blue_values = array(b"BlueValues");
    p.other_blues = array(b"OtherBlues");
    p.family_blues = array(b"FamilyBlues");
    p.family_other_blues = array(b"FamilyOtherBlues");
    p.blue_scale = number(b"BlueScale");
    p.blue_shift = number(b"BlueShift");
    p.blue_fuzz = number(b"BlueFuzz");
    p.std_hw = first(b"StdHW");
    p.std_vw = first(b"StdVW");
    p.stem_snap_h = array(b"StemSnapH");
    p.stem_snap_v = array(b"StemSnapV");
    p.force_bold = matches!(
        get(ctx, private, b"ForceBold").map(|o| o.value),
        Some(PsValue::Bool(true))
    );
    p.language_group = get(ctx, private, b"LanguageGroup").and_then(|o| o.as_i32());
    p.expansion_factor = number(b"ExpansionFactor");
    p
}

// ---------------------------------------------------------------------------
// PDF pieces
// ---------------------------------------------------------------------------

/// `/W`: the widths of used CIDs that differ from `/DW`, in runs of
/// consecutive CIDs.
fn w_array(font: &Prepared, used: &HashSet<u16>) -> Vec<PdfObj> {
    let mut cids: Vec<u16> = used
        .iter()
        .copied()
        .filter(|cid| font.width_of(*cid) != font.default_width)
        .collect();
    cids.sort_unstable();
    let mut out = Vec::new();
    let mut i = 0;
    while i < cids.len() {
        let mut run = vec![PdfObj::Int(i64::from(font.width_of(cids[i])))];
        let mut j = i + 1;
        while j < cids.len() && cids[j] == cids[j - 1] + 1 {
            run.push(PdfObj::Int(i64::from(font.width_of(cids[j]))));
            j += 1;
        }
        out.push(PdfObj::Int(i64::from(cids[i])));
        out.push(PdfObj::Array(run));
        i = j;
    }
    out
}

/// A ToUnicode CMap for the used CIDs of a font in one of the Adobe
/// character collections stet has tables for, or `None` for any other
/// (including `Identity`, whose CIDs carry no meaning of their own).
fn to_unicode(font: &CidFont, used: &HashSet<u16>, name: &[u8]) -> Option<Vec<u8>> {
    if font.registry != "Adobe" {
        return None;
    }
    let ordering = font.ordering.as_bytes();
    let mut cids: Vec<u16> = used.iter().copied().collect();
    cids.sort_unstable();
    let entries: Vec<(u16, &str)> = cids
        .into_iter()
        .filter_map(|cid| Some((cid, cid_unicode::cid_to_text(ordering, cid)?)))
        .collect();
    if entries.is_empty() {
        return None;
    }

    let name = String::from_utf8_lossy(name);
    let mut cmap = format!(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /{name}-UCS def\n/CMapType 2 def\n\
         1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n"
    );
    for chunk in entries.chunks(100) {
        cmap.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for (cid, text) in chunk {
            let utf16: String = text.encode_utf16().map(|u| format!("{u:04X}")).collect();
            cmap.push_str(&format!("<{cid:04X}> <{utf16}>\n"));
        }
        cmap.push_str("endbfchar\n");
    }
    cmap.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    Some(cmap.into_bytes())
}

// ---------------------------------------------------------------------------
// Matrices
// ---------------------------------------------------------------------------

fn same(a: &Matrix, b: &Matrix) -> bool {
    matrix_array(a) == matrix_array(b)
}

fn matrix_array(m: &Matrix) -> [f64; 6] {
    [m.a, m.b, m.c, m.d, m.tx, m.ty]
}

fn matrix_of(m: &[f64; 6]) -> Matrix {
    Matrix::new(m[0], m[1], m[2], m[3], m[4], m[5])
}
