// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Output device parameter types — pure data structures for rendering operations.

use crate::color::{DashPattern, DeviceColor, FillRule, LineCap, LineJoin};
use crate::display_list::DisplayList;
use crate::icc::ProfileHash;
use std::sync::Arc;
use stet_fonts::geometry::{Matrix, PsPath};

/// Pre-sampled transfer function (256 samples, domain `[0,1]` → range `[0,1]`).
/// Arc for cheap clone across display list elements.
pub type TransferTable = Arc<Vec<f64>>;

/// Transfer function state captured at paint time.
///
/// A transfer function adjusts device colour components just before
/// output (PLRM 7.3; ISO 32000-1 §10.5). Renderers apply it to the final
/// RGB of a paint, one function per channel ([`rgb_tables`](Self::rgb_tables)),
/// and only where the paint is fully opaque (ISO 32000-1 §11.7.5.2): alpha
/// 1, Normal blend mode, no soft mask, and the same for every enclosing
/// group. The PDF writer emits it as `/TR`.
#[derive(Clone, Debug, Default)]
pub struct TransferState {
    /// Single-component transfer (from settransfer). None = identity.
    pub gray: Option<TransferTable>,
    /// Per-component color transfer \[R, G, B, Gray\] (from setcolortransfer).
    /// When set, overrides `gray`.
    pub color: Option<[Option<TransferTable>; 4]>,
}

impl TransferState {
    /// Returns true if any non-identity transfer function is set.
    pub fn has_functions(&self) -> bool {
        if self.gray.is_some() {
            return true;
        }
        if let Some(ref color) = self.color {
            return color.iter().any(|t| t.is_some());
        }
        false
    }

    /// The functions for the red, green and blue components of an RGB
    /// device: the per-component functions when set (`setcolortransfer`, a
    /// four-element `/TR` array), otherwise the single function for all
    /// three. `None` is identity.
    pub fn rgb_tables(&self) -> [Option<&[f64]>; 3] {
        if let Some(ref color) = self.color {
            [0, 1, 2].map(|i| color[i].as_deref().map(|t| &t[..]))
        } else {
            let gray = self.gray.as_deref().map(|t| &t[..]);
            [gray; 3]
        }
    }

    /// Apply the functions to an RGB colour with components in `[0, 1]`.
    pub fn apply_rgb(&self, rgb: [f64; 3]) -> [f64; 3] {
        let tables = self.rgb_tables();
        [0, 1, 2].map(|i| transfer_lookup(rgb[i], tables[i]))
    }

    /// 256-entry lookup tables for 8-bit red, green and blue, or `None`
    /// when every function is identity.
    pub fn rgb_luts(&self) -> Option<[[u8; 256]; 3]> {
        if !self.has_functions() {
            return None;
        }
        Some(self.rgb_tables().map(|table| {
            let mut lut = [0u8; 256];
            for (i, v) in lut.iter_mut().enumerate() {
                *v = match table {
                    Some(t) if t.len() == 256 => (t[i].clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
                    _ => i as u8,
                };
            }
            lut
        }))
    }

    /// Apply the functions to 8-bit RGBA pixels in place. With
    /// `premultiplied`, the functions apply to each pixel's colour rather
    /// than to its product with alpha, and a clear pixel stays clear.
    pub fn apply_to_rgba(&self, data: &mut [u8], premultiplied: bool) {
        let Some(luts) = self.rgb_luts() else {
            return;
        };
        for pixel in data.as_chunks_mut::<4>().0 {
            let alpha = if premultiplied {
                u16::from(pixel[3])
            } else {
                255
            };
            if alpha == 0 {
                continue;
            }
            for (v, lut) in pixel.iter_mut().zip(&luts) {
                if alpha == 255 {
                    *v = lut[*v as usize];
                } else {
                    let colour = ((u16::from(*v) * 255 + alpha / 2) / alpha).min(255);
                    *v = ((u16::from(lut[colour as usize]) * alpha + 127) / 255) as u8;
                }
            }
        }
    }
}

/// Look up `value` in `[0, 1]` through a 256-sample transfer table,
/// interpolating linearly between samples. `None`, or a table of another
/// length, is identity.
pub fn transfer_lookup(value: f64, table: Option<&[f64]>) -> f64 {
    match table {
        Some(t) if t.len() == 256 => {
            let idx = (value * 255.0).clamp(0.0, 255.0);
            let lo = idx.floor() as usize;
            let hi = (lo + 1).min(255);
            let frac = idx - lo as f64;
            (t[lo] + frac * (t[hi] - t[lo])).clamp(0.0, 1.0)
        }
        _ => value,
    }
}

/// A pre-computed halftone screen for PDF output.
#[derive(Clone, Debug)]
pub struct HalftoneScreen {
    pub frequency: f64,
    pub angle: f64,
    /// Spot function as PDF Type 4 calculator bytes (e.g., b"{ dup mul exch dup mul add 1 exch sub }").
    /// None if conversion failed (falls back to sampled_2d).
    pub type4_tokens: Option<Arc<Vec<u8>>>,
    /// Spot function sampled on a 64×64 grid (4096 f64 values, domain `[-1,1]²`, range `[0,1]`).
    /// Used when Type 4 decompilation fails.
    pub sampled_2d: Option<Arc<Vec<f64>>>,
}

/// Pre-sampled black generation / undercolor removal state for PDF output.
#[derive(Clone, Debug, Default)]
pub struct BgUcrState {
    /// Black generation function (256 samples, domain `[0,1]` → range `[0,1]`).
    pub bg: Option<Arc<Vec<f64>>>,
    /// Undercolor removal function (256 samples, domain `[0,1]` → range `[-1,1]`).
    pub ucr: Option<Arc<Vec<f64>>>,
}

/// Pre-computed halftone state captured at paint time.
#[derive(Clone, Debug, Default)]
pub struct HalftoneState {
    /// Single-component halftone (from setscreen). None = default (suppress).
    pub gray: Option<Arc<HalftoneScreen>>,
    /// Per-component \[R, G, B, Gray\] (from setcolorscreen). Emits Type 5 composite.
    pub color: Option<[Option<Arc<HalftoneScreen>>; 4]>,
}

/// Native ICCBased fill/stroke color info for PDF output.
///
/// Preserves the raw component values from the source `sc`/`scn`
/// operator plus the ICC profile bytes, so a PDF reader can capture the
/// exact ICCBased paint at parse time and the PDF writer can emit a
/// faithful `/CSn cs` + `c1 c2 c3 scn` round-trip — independent of the
/// `DeviceColor` ICC-converted RGB that's used for rasterizing.
#[derive(Clone, Debug)]
pub struct IccColor {
    /// Raw component values from `sc`/`scn` (length matches `color_space.n`).
    pub components: Vec<f64>,
    /// The ICC color space definition.
    pub color_space: IccColorSpace,
}

/// Pre-resolved ICCBased color space ready for PDF emission.
#[derive(Clone, Debug)]
pub struct IccColorSpace {
    /// Number of components (1, 3, or 4).
    pub n: u32,
    /// Raw ICC profile bytes (Arc-shared so multiple paints can dedup
    /// to a single PDF stream).
    pub profile_data: Arc<Vec<u8>>,
    /// Profile hash, used both as a writer-side dedup key and to keep
    /// the IccCache lookups in sync with the rasterizer.
    pub profile_hash: ProfileHash,
}

/// Native Separation/DeviceN color info for PDF output.
#[derive(Clone, Debug)]
pub struct SpotColor {
    /// Tint values from the most recent setcolor (1 for Separation, N for DeviceN).
    pub tint_values: Vec<f64>,
    /// Color space definition for this spot color.
    pub color_space: SpotColorSpace,
}

/// Separation or DeviceN color space with pre-sampled tint function.
///
/// Marked `#[non_exhaustive]`; cross-crate `match` expressions need a
/// wildcard arm.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum SpotColorSpace {
    Separation {
        name: Vec<u8>,
        alt: SimpleColorSpace,
        tint_table: Arc<TintLookupTable>,
    },
    DeviceN {
        names: Vec<Vec<u8>>,
        alt: SimpleColorSpace,
        tint_table: Arc<TintLookupTable>,
    },
}

/// Simple device color space for alt-space references.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SimpleColorSpace {
    DeviceGray,
    DeviceRGB,
    DeviceCMYK,
}

/// Bitmask of CMYK channels painted by an overprint operation.
/// Bits: 0=Cyan, 1=Magenta, 2=Yellow, 3=Black.
pub const CMYK_C: u8 = 1 << 0;
pub const CMYK_M: u8 = 1 << 1;
pub const CMYK_Y: u8 = 1 << 2;
pub const CMYK_K: u8 = 1 << 3;
pub const CMYK_ALL: u8 = CMYK_C | CMYK_M | CMYK_Y | CMYK_K;

/// Map a CMYK process color name to its channel bit.
pub fn cmyk_channel_for_name(name: &[u8]) -> u8 {
    match name {
        b"Cyan" => CMYK_C,
        b"Magenta" => CMYK_M,
        b"Yellow" => CMYK_Y,
        b"Black" => CMYK_K,
        b"All" => CMYK_ALL,
        b"None" => 0,
        _ => 0,
    }
}

/// Process channels painted by a Separation or DeviceN paint.
///
/// The union of [`cmyk_channel_for_name`] over the colorant names: a
/// process colorant paints its own channel, `All` paints every channel, and
/// spot colorants and `None` paint none. `0` therefore means "spot-only";
/// renderers must not widen it to [`CMYK_ALL`] for a paint that carries a
/// [`DeviceColor::process_cmyk`], or a spot overprint knocks out the
/// process plates beneath it.
pub fn painted_channels_for_colorants<N: AsRef<[u8]>>(names: &[N]) -> u8 {
    names
        .iter()
        .fold(0u8, |acc, n| acc | cmyk_channel_for_name(n.as_ref()))
}

/// Process-only CMYK contribution of a Separation paint.
///
/// A process colorant (`Cyan`, `Magenta`, `Yellow`, `Black`) puts `tint` on
/// its own channel. A spot colorant or `None` contributes `(0, 0, 0, 0)`, so
/// the overprint tracker writes nothing to the process plates rather than
/// the spot's alternate-space CMYK. `All` returns `None`: every plate
/// receives the tint, which the alternate-space colour already expresses.
///
/// This is the value callers store in [`DeviceColor::process_cmyk`].
pub fn separation_process_cmyk(name: &[u8], tint: f64) -> Option<(f64, f64, f64, f64)> {
    let tint = tint.clamp(0.0, 1.0);
    match cmyk_channel_for_name(name) {
        CMYK_C => Some((tint, 0.0, 0.0, 0.0)),
        CMYK_M => Some((0.0, tint, 0.0, 0.0)),
        CMYK_Y => Some((0.0, 0.0, tint, 0.0)),
        CMYK_K => Some((0.0, 0.0, 0.0, tint)),
        0 => Some((0.0, 0.0, 0.0, 0.0)),
        _ => None,
    }
}

/// Process-only CMYK contribution of a DeviceN paint.
///
/// Each colorant that names a process channel (`Cyan`, `Magenta`, `Yellow`,
/// `Black`, or `All` for all four) adds its tint to that plate by
/// subtractive stacking, `1 - Π(1 - tᵢ)`; spot colorants contribute
/// nothing. Returns `None` when the name and tint counts differ.
///
/// This is the value callers store in [`DeviceColor::process_cmyk`].
pub fn devicen_process_cmyk<N: AsRef<[u8]>>(
    names: &[N],
    tints: &[f64],
) -> Option<(f64, f64, f64, f64)> {
    if names.len() != tints.len() {
        return None;
    }
    let mut compl = [1.0f64; 4];
    for (name, &tint) in names.iter().zip(tints) {
        let keep = 1.0 - tint.clamp(0.0, 1.0);
        let channels = cmyk_channel_for_name(name.as_ref());
        for (i, c) in compl.iter_mut().enumerate() {
            if channels & (1 << i) != 0 {
                *c *= keep;
            }
        }
    }
    Some((
        1.0 - compl[0],
        1.0 - compl[1],
        1.0 - compl[2],
        1.0 - compl[3],
    ))
}

/// Parameters for filling a path.
///
/// Constructed by interpreter/parser code (stet-ops, stet-pdf-reader)
/// and read by renderers. New fields may be added without notice; pattern-
/// matching consumers should use `..` to ignore unmatched fields.
#[derive(Clone, Debug)]
pub struct FillParams {
    pub color: DeviceColor,
    pub fill_rule: FillRule,
    pub ctm: Matrix,
    /// True when this fill is a text glyph from a show operator.
    /// PDF device skips these (uses Text elements instead).
    pub is_text_glyph: bool,
    /// Overprint flag from graphics state (used by PDF output).
    pub overprint: bool,
    /// Overprint mode (0 or 1). With OPM 1 + DeviceCMYK, only non-zero channels are painted.
    pub overprint_mode: i32,
    /// True when /OPM was set together with /op or /OP in the same ExtGState
    /// dict that configured this fill. Enables strict OPM-1 "preserve zero
    /// components" behavior; when false, an all-zero CMYK source still
    /// performs a full knockout (legacy Adobe compatibility).
    pub opm_paired: bool,
    /// Which CMYK channels this fill paints (bitmask of CMYK_C/M/Y/K).
    pub painted_channels: u8,
    /// True when color space is DeviceCMYK or ICCBased(4).
    pub is_device_cmyk: bool,
    /// Separation/DeviceN color for PDF output. None for device color spaces.
    pub spot_color: Option<SpotColor>,
    /// ICCBased color for PDF output. None for device color spaces and
    /// for Separation/DeviceN paints (those round-trip through `spot_color`).
    pub icc_color: Option<IccColor>,
    /// Rendering intent (0=RelativeColorimetric, 1=Absolute, 2=Perceptual, 3=Saturation);
    /// see [`crate::rendering_intent`].
    pub rendering_intent: u8,
    /// Transfer function in force when painted: applied by the renderer
    /// to the final colour, and carried for PDF output (see
    /// [`TransferState`]).
    pub transfer: TransferState,
    /// Pre-computed halftone screen state for PDF output.
    pub halftone: HalftoneState,
    /// Pre-sampled black generation / undercolor removal for PDF output.
    pub bg_ucr: BgUcrState,
    /// Fill opacity (0.0–1.0, default 1.0). Used by PDF transparency.
    pub alpha: f64,
    /// Blend mode (0=Normal, 1=Multiply, ..., 11=Exclusion). Default 0.
    pub blend_mode: u8,
    /// PDF `AIS` (alpha-is-shape). When true, the source is interpreted as
    /// shape rather than opacity. Default false.
    pub alpha_is_shape: bool,
}

/// Parameters for a text element emitted by show operators.
///
/// The PDF device uses these for BT/ET/Tf/Tj text operators.
/// The raster device ignores them (uses Fill elements for glyph paths).
///
/// New fields may be added without notice; pattern-matching consumers
/// should use `..` to ignore unmatched fields.
#[derive(Clone, Debug)]
pub struct TextParams {
    /// Character bytes (or 2-byte CID values for Type 0).
    pub text: Vec<u8>,
    /// Device-space X position at start of string.
    pub start_x: f64,
    /// Device-space Y position at start of string.
    pub start_y: f64,
    /// Font dict entity ID (raw u32 for VM independence).
    pub font_entity: u32,
    /// FontName bytes (e.g., b"Times-Roman").
    pub font_name: Vec<u8>,
    /// FontType (0, 1, 2, 3, 42).
    pub font_type: i32,
    /// Effective device-space font size.
    pub font_size: f64,
    /// Fill color at render time.
    pub color: DeviceColor,
    /// CTM at render time.
    pub ctm: [f64; 6],
    /// User-space font matrix (scaled to point units).
    pub font_matrix: [f64; 6],
    /// PaintType: 0 = fill (default), 2 = stroke (outlined glyphs).
    pub paint_type: i32,
    /// Device-space stroke width for PaintType 2 fonts.
    pub stroke_width: f64,
    /// Separation/DeviceN color for PDF output. None for device color spaces.
    pub spot_color: Option<SpotColor>,
    /// ICCBased color for PDF output. None for device color spaces and
    /// for Separation/DeviceN paints (those round-trip through `spot_color`).
    pub icc_color: Option<IccColor>,
    /// Rendering intent (0=RelativeColorimetric, 1=Absolute, 2=Perceptual, 3=Saturation);
    /// see [`crate::rendering_intent`].
    pub rendering_intent: u8,
    /// Transfer function in force when painted: applied by the renderer
    /// to the final colour, and carried for PDF output (see
    /// [`TransferState`]).
    pub transfer: TransferState,
    /// Pre-computed halftone screen state for PDF output.
    pub halftone: HalftoneState,
    /// Pre-sampled black generation / undercolor removal for PDF output.
    pub bg_ucr: BgUcrState,
    /// Fill opacity (0.0–1.0, default 1.0). Used by PDF transparency.
    pub fill_opacity: f64,
    /// Stroke opacity (0.0–1.0, default 1.0). Applies to PaintType-2 fonts.
    pub stroke_opacity: f64,
    /// Blend mode (0=Normal, 1=Multiply, …, 15=Luminosity). Default 0.
    pub blend_mode: u8,
    /// Alpha-is-shape (PDF `AIS`). Default false.
    pub alpha_is_shape: bool,
    /// Text knockout (PDF `TK`). Default true.
    pub text_knockout: bool,
}

/// Parameters for a [`TextRun`](crate::display_list::DisplayElement::TextRun)
/// element: a stretch of shown text, in Unicode, with the position of every
/// glyph — for text extraction, search and selection.
///
/// A run holds consecutive glyphs of one font at one size and orientation
/// that carry on along one baseline, whether one show operation displayed
/// them or several did: a producer that shows each glyph separately still
/// gives one run per stretch of text. A glyph that leaves the baseline or
/// jumps well back starts a new run — the rule is
/// [`step_to`](Self::step_to), in [`crate::text`].
///
/// Recorded only when a producer is asked to extract text, at a
/// [`TextExtraction`] level (the PostScript interpreter's
/// `text_extraction`, the PDF reader's `set_text_extraction`). It paints
/// nothing: renderers and the PDF writer
/// skip it, and the glyphs themselves are drawn by the elements that
/// accompany it.
///
/// All positions are in display-list (device) space, like every other
/// element, so they map directly onto a raster of the same list.
///
/// New fields may be added without notice; pattern-matching consumers
/// should use `..` to ignore unmatched fields.
#[derive(Clone, Debug, Default)]
pub struct TextRunParams {
    /// The run's text: every glyph's text, concatenated in the order shown.
    pub text: String,
    /// One entry per glyph shown, in content order.
    pub glyphs: Vec<ShownGlyph>,
    /// Glyph space → device space for the run's first glyph: the font
    /// matrix, font size, horizontal scaling, text rise, text matrix and
    /// CTM combined. Its translation places that glyph's glyph-space
    /// origin — [`start`](Self::start), unless the font matrix is offset;
    /// its linear part is the same for every glyph in the run.
    ///
    /// A glyph's box is the parallelogram spanned by its
    /// [`advance`](ShownGlyph::advance) and by this matrix's linear part
    /// applied to `(0, descent)` and `(0, ascent)` — to `(descent, 0)` and
    /// `(ascent, 0)` for [`vertical`](Self::vertical) runs — placed at its
    /// [`origin`](ShownGlyph::origin). Rotated and skewed text gives a
    /// rotated or skewed box.
    pub glyph_to_device: Matrix,
    /// Font ascent in glyph space (positive; around 800 for a font with
    /// 1000 units per em). From the font descriptor when present, else the
    /// font bounding box — for a PostScript Type 3 font with an empty one,
    /// the union of its glyphs' `setcachedevice` boxes — else 0.8 em. For a vertical run, the glyph's
    /// extent to the right of its origin instead: half the em.
    pub ascent: f64,
    /// Font descent in glyph space (negative; around -200 for a font with
    /// 1000 units per em). Same sources as `ascent`, else -0.2 em. For a
    /// vertical run, the extent to the left of the origin: minus half the
    /// em.
    pub descent: f64,
    /// The font's name (PDF `/BaseFont`, PostScript `/FontName`), or empty
    /// when it has none.
    pub font_name: String,
    /// True when the text was shown but not painted: PDF text render
    /// modes 3 and 7, used by OCR layers over scanned pages. Included
    /// because it is text the document contains; a consumer that wants only
    /// visible text drops these runs.
    pub invisible: bool,
    /// Vertical writing (a CID font with writing mode 1). Each glyph's
    /// origin is its vertical origin, at the top centre of the glyph, and
    /// its advance points down the column; `ascent` and `descent` bound
    /// the glyph across the column rather than above and below a baseline.
    pub vertical: bool,
    /// Device-space origin of the run's first glyph.
    pub start: (f64, f64),
    /// Device-space point where the run's last glyph ends: its origin plus
    /// its advance. From `start` to here, the run spans its baseline.
    pub end: (f64, f64),
    /// Byte offsets in `text`, ascending, where a word's gap
    /// ([`WORD_GAP`](crate::text::WORD_GAP)) separated two glyphs with no
    /// space shown between them: word boundaries drawn as distance rather
    /// than as characters, the way TeX spaces words. `text` itself is left
    /// as shown; insert a space at each offset to read it as words.
    pub word_breaks: Vec<u32>,
}

/// One glyph of a [`TextRunParams`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShownGlyph {
    /// Byte range of this glyph's text within [`TextRunParams::text`].
    ///
    /// Empty when no Unicode could be found for the glyph: no fallback
    /// encoding is assumed (see [`UnicodeSource`] for what is tried).
    /// Several characters for a ligature (`fi`) or a
    /// supplementary-plane character. When a PDF `/ActualText` span
    /// replaces its glyphs' text, the span's first glyph carries the whole
    /// replacement and the rest carry empty ranges.
    pub text_range: std::ops::Range<u32>,
    /// Device-space position of the glyph's origin.
    pub origin: (f64, f64),
    /// Device-space vector from this glyph's origin to where the next
    /// glyph would start without spacing adjustments: its width, scaled
    /// and rotated like the text. Vertical for vertical writing.
    pub advance: (f64, f64),
    /// The character code shown, for consumers that want their own mapping
    /// (one byte for simple fonts, one to four for composite fonts). 0 for
    /// a glyph PostScript's `glyphshow` showed, which selects a glyph by
    /// name rather than by code.
    pub code: u32,
    /// Where the glyph's text came from; lets a consumer judge confidence.
    pub source: UnicodeSource,
}

/// How the text of a [`ShownGlyph`] was found.
///
/// Marked `#[non_exhaustive]`: new sources may be added, so `match` on it
/// with a wildcard arm.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum UnicodeSource {
    /// The PDF font's `/ToUnicode` CMap: the producer's own statement.
    ToUnicode,
    /// The glyph's name, through the Adobe Glyph List (`Aacute`,
    /// `uni00C1`, `f_i`) — or, for a name outside it that spells a
    /// character code the way dvips names bitmap-font glyphs (`a80`), that
    /// code read as Latin-1, as Poppler does.
    GlyphName,
    /// The CID, through an Adobe CJK collection's CID → Unicode table
    /// (Japan1, CNS1, GB1, Korea1).
    CidOrdering,
    /// An enclosing PDF `/ActualText` marked-content span, which replaces
    /// the text of every glyph inside it.
    ActualText,
    /// Nothing gave the glyph any text; its range is empty.
    #[default]
    Unmapped,
}

/// How much text a producer records as
/// [`TextRun`](crate::display_list::DisplayElement::TextRun)s.
///
/// Marked `#[non_exhaustive]`: new levels may be added, so `match` on it
/// with a wildcard arm.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TextExtraction {
    /// Record no text: display lists carry no `TextRun`s. The default.
    #[default]
    Off,
    /// Record runs without their glyphs: each run's text, font, extent
    /// ([`start`](TextRunParams::start), [`end`](TextRunParams::end),
    /// [`glyph_to_device`](TextRunParams::glyph_to_device), `ascent`,
    /// `descent`) and word breaks, with
    /// [`glyphs`](TextRunParams::glyphs) empty. The runs are those of
    /// [`Glyphs`](Self::Glyphs), cut the same way, in about half the
    /// memory — enough to search, index or copy a page's text and to find
    /// its lines, but not to place a glyph or a word within a run.
    Runs,
    /// Record runs with every glyph: its position, advance, character code
    /// and where its text came from.
    Glyphs,
}

/// Parameters for stroking a path.
///
/// New fields may be added without notice; pattern-matching consumers
/// should use `..` to ignore unmatched fields.
#[derive(Clone, Debug)]
pub struct StrokeParams {
    pub color: DeviceColor,
    pub line_width: f64,
    pub line_cap: LineCap,
    pub line_join: LineJoin,
    pub miter_limit: f64,
    pub dash_pattern: DashPattern,
    pub ctm: Matrix,
    /// When true, snap thin stroke coordinates to device pixel centers.
    pub stroke_adjust: bool,
    /// True when this stroke is a text glyph from a show operator (PaintType 2).
    pub is_text_glyph: bool,
    /// Overprint flag from graphics state (used by PDF output).
    pub overprint: bool,
    /// Overprint mode (0 or 1).
    pub overprint_mode: i32,
    /// See FillParams::opm_paired. Strict OPM-1 preserve requires both
    /// /OPM and /op|/OP set in the same ExtGState dict.
    pub opm_paired: bool,
    /// Which CMYK channels this stroke paints (bitmask of CMYK_C/M/Y/K).
    pub painted_channels: u8,
    /// True when stroke color space is DeviceCMYK or ICCBased(4) — OPM 1 only applies to these.
    pub is_device_cmyk: bool,
    /// Separation/DeviceN color for PDF output. None for device color spaces.
    pub spot_color: Option<SpotColor>,
    /// ICCBased color for PDF output. None for device color spaces and
    /// for Separation/DeviceN paints (those round-trip through `spot_color`).
    pub icc_color: Option<IccColor>,
    /// Rendering intent (0=RelativeColorimetric, 1=Absolute, 2=Perceptual, 3=Saturation);
    /// see [`crate::rendering_intent`].
    pub rendering_intent: u8,
    /// Transfer function in force when painted: applied by the renderer
    /// to the final colour, and carried for PDF output (see
    /// [`TransferState`]).
    pub transfer: TransferState,
    /// Pre-computed halftone screen state for PDF output.
    pub halftone: HalftoneState,
    /// Pre-sampled black generation / undercolor removal for PDF output.
    pub bg_ucr: BgUcrState,
    /// Stroke opacity (0.0–1.0, default 1.0). Used by PDF transparency.
    pub alpha: f64,
    /// Blend mode (0=Normal, 1=Multiply, ..., 11=Exclusion). Default 0.
    pub blend_mode: u8,
    /// PDF `AIS` (alpha-is-shape). When true, the source is interpreted as
    /// shape rather than opacity. Default false.
    pub alpha_is_shape: bool,
}

/// Parameters for clipping.
///
/// New fields may be added without notice; pattern-matching consumers
/// should use `..` to ignore unmatched fields.
#[derive(Clone, Debug, Default)]
pub struct ClipParams {
    pub fill_rule: FillRule,
    pub ctm: Matrix,
    /// For stroke-based clips: stroke parameters to expand the clip path
    /// from a centerline to a stroke outline before rasterizing.
    pub stroke_params: Option<StrokeParams>,
}

/// Pre-sampled tint transform: maps input tint values to alt-space components.
#[derive(Clone, Debug)]
pub struct TintLookupTable {
    /// Number of input components (1 for Separation, N for DeviceN).
    pub num_inputs: u32,
    /// Number of output components (matches alternative space: 1/3/4).
    pub num_outputs: u32,
    /// Number of samples per dimension.
    pub samples_per_dim: u32,
    /// Flattened f32 data, row-major order. Length = samples_per_dim^num_inputs × num_outputs.
    pub data: Vec<f32>,
}

impl TintLookupTable {
    /// Linear interpolation lookup for 1D (Separation) tint transforms.
    #[inline]
    pub fn lookup_1d(&self, tint: f32, out: &mut [f32]) {
        let n = self.samples_per_dim as usize;
        let no = self.num_outputs as usize;
        let idx = tint * (n - 1) as f32;
        let i0 = (idx as usize).min(n - 2);
        let frac = idx - i0 as f32;
        let base0 = i0 * no;
        let base1 = (i0 + 1) * no;
        for (c, out_val) in out[..no].iter_mut().enumerate() {
            *out_val = self.data[base0 + c] * (1.0 - frac) + self.data[base1 + c] * frac;
        }
    }

    /// Multilinear interpolation lookup for N-D (DeviceN) tint transforms.
    pub fn lookup_nd(&self, inputs: &[f32], out: &mut [f32]) {
        let ni = self.num_inputs as usize;
        let no = self.num_outputs as usize;
        let n = self.samples_per_dim as usize;

        let mut idx = [0usize; 8];
        let mut frac = [0.0f32; 8];
        for d in 0..ni {
            let fi = inputs[d] * (n - 1) as f32;
            idx[d] = (fi as usize).min(n - 2);
            frac[d] = fi - idx[d] as f32;
        }

        let corners = 1usize << ni;
        for out_val in out[..no].iter_mut() {
            *out_val = 0.0;
        }
        for corner in 0..corners {
            let mut weight = 1.0f32;
            let mut linear_idx = 0usize;
            for d in 0..ni {
                let bit = (corner >> d) & 1;
                let dim_idx = idx[d] + bit;
                weight *= if bit == 1 { frac[d] } else { 1.0 - frac[d] };
                let stride = n.pow((ni - 1 - d) as u32);
                linear_idx += dim_idx * stride;
            }
            let base = linear_idx * no;
            for (c, out_val) in out[..no].iter_mut().enumerate() {
                *out_val += weight * self.data.get(base + c).copied().unwrap_or(0.0);
            }
        }
    }
}

/// VM-free color space enum for images stored in the display list.
///
/// Marked `#[non_exhaustive]`; cross-crate `match` expressions need a
/// wildcard arm to remain forward-compatible.
#[derive(Clone, Debug)]
#[non_exhaustive]
#[derive(Default)]
pub enum ImageColorSpace {
    #[default]
    DeviceGray,
    DeviceRGB,
    DeviceCMYK,
    ICCBased {
        n: u32,
        profile_hash: ProfileHash,
        profile_data: Arc<Vec<u8>>,
    },
    Indexed {
        base: Box<ImageColorSpace>,
        hival: u32,
        lookup: Vec<u8>,
    },
    CIEBasedABC {
        params: Arc<crate::color::CieAbcParams>,
    },
    CIEBasedA {
        params: Arc<crate::color::CieAParams>,
    },
    /// CIE L*a*b* color space (PDF /Lab or ICCBased Lab alternate).
    ///
    /// Sample byte layout: 3 components (L, a, b), 8-bit each. Decode
    /// scales bytes: L = byte/255 × 100; a = byte/255 × (`range[1]`-`range[0]`) + `range[0]`;
    /// b = byte/255 × (`range[3]`-`range[2]`) + `range[2]`.
    Lab {
        white_point: [f64; 3],
        range: [f64; 4],
    },
    Separation {
        name: Vec<u8>,
        alt_space: Box<ImageColorSpace>,
        tint_table: Arc<TintLookupTable>,
    },
    DeviceN {
        names: Vec<Vec<u8>>,
        alt_space: Box<ImageColorSpace>,
        tint_table: Arc<TintLookupTable>,
    },
    Mask {
        color: DeviceColor,
        polarity: bool,
        /// Optional Separation/DeviceN spot color carried alongside `color` so
        /// the PDF writer can round-trip the imagemask's fill as `/CSn cs +
        /// tint scn` instead of collapsing to a process-color paint.
        /// `None` when the imagemask fill came from a Device/ICC space.
        spot_color: Option<SpotColor>,
    },
    PreconvertedRGBA,
}

impl ImageColorSpace {
    /// Number of components per sample.
    pub fn num_components(&self) -> u32 {
        match self {
            ImageColorSpace::DeviceGray => 1,
            ImageColorSpace::DeviceRGB => 3,
            ImageColorSpace::DeviceCMYK => 4,
            ImageColorSpace::ICCBased { n, .. } => *n,
            ImageColorSpace::Indexed { .. } => 1,
            ImageColorSpace::CIEBasedABC { .. } => 3,
            ImageColorSpace::CIEBasedA { .. } => 1,
            ImageColorSpace::Lab { .. } => 3,
            ImageColorSpace::Separation { .. } => 1,
            ImageColorSpace::DeviceN { tint_table, .. } => tint_table.num_inputs,
            ImageColorSpace::Mask { .. } => 1,
            ImageColorSpace::PreconvertedRGBA => 4,
        }
    }
}

/// Parameters for drawing an image.
///
/// New fields may be added without notice; pattern-matching consumers
/// should use `..` to ignore unmatched fields.
#[derive(Clone, Debug)]
pub struct ImageParams {
    pub width: u32,
    pub height: u32,
    pub color_space: ImageColorSpace,
    pub bits_per_component: u8,
    pub ctm: Matrix,
    pub image_matrix: Matrix,
    pub interpolate: bool,
    /// A colour-key mask over `sample_data` as it stands: a minimum and a
    /// maximum for each component (or one exact value each), and a pixel
    /// whose every sample lies in range is not painted. The samples here
    /// are already reduced to 8 bits and decoded, so a key the file states
    /// over deeper or `/Decode`d samples cannot be put here exactly; the
    /// front ends apply such a key themselves
    /// ([`ColorKey`](crate::image_samples::ColorKey)).
    pub mask_color: Option<Vec<u8>>,
    pub alpha: f64,
    pub blend_mode: u8,
    pub overprint: bool,
    pub overprint_mode: i32,
    /// See FillParams::opm_paired.
    pub opm_paired: bool,
    pub painted_channels: u8,
    /// PDF `AIS` (alpha-is-shape). Default false.
    pub alpha_is_shape: bool,
    /// Rendering intent that selects which `A2B*`/`B2A*` table the source
    /// profile and the output-intent profile use when this image flows
    /// through the proofing chain. Same encoding as every other param
    /// struct: 0=RelativeColorimetric, 1=Absolute, 2=Perceptual,
    /// 3=Saturation; see [`crate::rendering_intent`].
    /// Per ISO 32000 §11.3.4 a per-image `/Intent` overrides the gstate
    /// `/RI`; PDF readers populate this from `/Intent` when present and
    /// fall back to `gstate.rendering_intent` otherwise.
    pub rendering_intent: u8,
    /// Transfer function in force when painted: applied by the renderer
    /// to the image's colour after conversion, and carried for PDF output
    /// (see [`TransferState`]). Front ends leave it at the default for an
    /// image with a soft mask, which is never fully opaque.
    pub transfer: TransferState,
}

/// A spot/DeviceN tint transform reduced to a uniform sampled grid.
///
/// Carries enough information for the PDF writer to round-trip the source's
/// `/ColorSpace [/Separation … <tintFunc>]` or `/ColorSpace [/DeviceN … <tintFunc>]`
/// dictionary as a SampledFunction without losing the spot identity.
#[derive(Clone, Debug)]
pub struct SpotTintFunction {
    /// Number of input channels (1 for Separation, N for DeviceN).
    pub input_dim: usize,
    /// Samples per input dimension. For Separation this is the length of
    /// `samples / 4`. For DeviceN this is the per-axis count of a grid of
    /// total size `samples_per_dim.pow(input_dim)`.
    pub samples_per_dim: usize,
    /// Flat row-major grid of CMYK output samples, length
    /// `samples_per_dim^input_dim * 4`. The reader builds this by evaluating
    /// the source PDF's tint function at uniform input points; the writer
    /// emits it back as a FunctionType-0 `/Function`.
    pub cmyk_samples: Arc<Vec<f64>>,
}

/// Color space carried through the display list for native shading output.
///
/// Marked `#[non_exhaustive]`; cross-crate `match` expressions need a
/// wildcard arm.
#[derive(Clone, Debug)]
#[non_exhaustive]
#[derive(Default)]
pub enum ShadingColorSpace {
    DeviceGray,
    #[default]
    DeviceRGB,
    DeviceCMYK,
    ICCBased {
        n: u32,
        profile_hash: ProfileHash,
        profile_data: Arc<Vec<u8>>,
    },
    CalRGB {
        white_point: [f64; 3],
        matrix: Option<[f64; 9]>,
        gamma: Option<[f64; 3]>,
    },
    CalGray {
        white_point: [f64; 3],
        gamma: Option<f64>,
    },
    /// Separation (single spot ink) with a CMYK alternate.
    ///
    /// Round-tripping this variant preserves the spot identity in the output
    /// PDF; without it, the writer emits `/DeviceCMYK` and downstream readers
    /// can't reconstruct the spot-tint-blend compositing behavior.
    Separation {
        /// Spot color name (PDF Name bytes, e.g. `"GWG Green"`).
        name: Vec<u8>,
        /// Alternate process color space. Typically `DeviceCMYK`.
        alternate: SimpleColorSpace,
        /// Sampled tint transform mapping spot tint `[0,1]` to alternate-space
        /// components.
        tint_function: SpotTintFunction,
    },
    /// DeviceN (multiple spot inks) with a CMYK alternate.
    DeviceN {
        /// Colorant names, in input-channel order.
        names: Vec<Vec<u8>>,
        /// Alternate process color space. Typically `DeviceCMYK`.
        alternate: SimpleColorSpace,
        /// Sampled tint transform mapping N spot tints to alternate-space
        /// components.
        tint_function: SpotTintFunction,
    },
}

impl ShadingColorSpace {
    /// Number of color components in this color space.
    pub fn num_components(&self) -> usize {
        match self {
            ShadingColorSpace::DeviceGray | ShadingColorSpace::CalGray { .. } => 1,
            ShadingColorSpace::DeviceRGB | ShadingColorSpace::CalRGB { .. } => 3,
            ShadingColorSpace::DeviceCMYK => 4,
            ShadingColorSpace::ICCBased { n, .. } => *n as usize,
            ShadingColorSpace::Separation { .. } => 1,
            ShadingColorSpace::DeviceN { names, .. } => names.len(),
        }
    }
}

/// A single color stop in a gradient.
///
/// New fields may be added without notice; pattern-matching consumers
/// should use `..` to ignore unmatched fields.
#[derive(Clone, Debug)]
pub struct ColorStop {
    pub position: f64,
    pub color: DeviceColor,
    pub raw_components: Vec<f64>,
    /// Pre-tint-transform component values, in the shading's *source*
    /// color space. For a Separation/DeviceN shading this holds the spot
    /// tint(s) the source `/Function` evaluated to at this stop; the writer
    /// emits these as the shading function's output so the round-trip PDF
    /// preserves the spot input dimension. Empty when not applicable.
    pub source_components: Vec<f64>,
}

/// Parameters for axial (linear) gradient shading (Type 2).
///
/// New fields may be added without notice; pattern-matching consumers
/// should use `..` to ignore unmatched fields.
#[derive(Clone, Debug)]
pub struct AxialShadingParams {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
    pub color_stops: Vec<ColorStop>,
    pub extend_start: bool,
    pub extend_end: bool,
    pub ctm: Matrix,
    pub bbox: Option<[f64; 4]>,
    pub color_space: ShadingColorSpace,
    pub overprint: bool,
    /// PDF `OPM` (overprint mode). 0 = standard (KO when a component is set);
    /// 1 = Illustrator-style (a component explicitly set to 0 is preserved).
    /// Must round-trip through PDF→DL→PDF or shadings painted onto an
    /// underlay lose their preserved-component behavior.
    pub overprint_mode: i32,
    pub painted_channels: u8,
    /// Fill alpha from graphics state (0.0–1.0).
    pub alpha: f64,
    /// Blend mode (0=Normal, …, 15=Luminosity). Default 0.
    pub blend_mode: u8,
    /// PDF `AIS` (alpha-is-shape). Default false.
    pub alpha_is_shape: bool,
    /// True when this shading uses a Separation/DeviceN color space with a
    /// CMYK alternate AND at least one non-process spot colorant.  The
    /// renderer composites the per-pixel CMYK from the gradient stops with
    /// the tracked CMYK buffer multiplicatively, preserving underlying CMYK
    /// paints under the gradient (e.g. green checkmarks under a green→cyan
    /// DeviceN strip survive).
    pub spot_tint_blend: bool,
    /// Rendering intent the shading's colours were converted with (see
    /// [`crate::rendering_intent`]; default relative colorimetric). The
    /// renderer uses it wherever it converts the shading's CMYK again at
    /// render time — overprint, spot-tint blending, patch subdivision.
    pub rendering_intent: u8,
    /// Transfer function in force when painted: applied by the renderer
    /// to each colour the shading evaluates, and carried for PDF output
    /// (see [`TransferState`]).
    pub transfer: TransferState,
}

/// Parameters for radial gradient shading (Type 3).
///
/// New fields may be added without notice; pattern-matching consumers
/// should use `..` to ignore unmatched fields.
#[derive(Clone, Debug)]
pub struct RadialShadingParams {
    pub x0: f64,
    pub y0: f64,
    pub r0: f64,
    pub x1: f64,
    pub y1: f64,
    pub r1: f64,
    pub color_stops: Vec<ColorStop>,
    pub extend_start: bool,
    pub extend_end: bool,
    pub ctm: Matrix,
    pub bbox: Option<[f64; 4]>,
    pub color_space: ShadingColorSpace,
    pub overprint: bool,
    /// See [`AxialShadingParams::overprint_mode`].
    pub overprint_mode: i32,
    pub painted_channels: u8,
    /// Fill alpha from graphics state (0.0–1.0).
    pub alpha: f64,
    /// Blend mode (0=Normal, …, 15=Luminosity). Default 0.
    pub blend_mode: u8,
    /// PDF `AIS` (alpha-is-shape). Default false.
    pub alpha_is_shape: bool,
    /// See [`AxialShadingParams::spot_tint_blend`].
    pub spot_tint_blend: bool,
    /// See [`AxialShadingParams::rendering_intent`].
    pub rendering_intent: u8,
    /// Transfer function in force when painted: applied by the renderer
    /// to each colour the shading evaluates, and carried for PDF output
    /// (see [`TransferState`]).
    pub transfer: TransferState,
}

/// A vertex in a shading triangle mesh.
#[derive(Clone, Debug)]
pub struct ShadingVertex {
    pub x: f64,
    pub y: f64,
    pub color: DeviceColor,
    pub raw_components: Vec<f64>,
}

/// A triangle in a shading mesh.
#[derive(Clone, Debug)]
pub struct ShadingTriangle {
    pub v0: ShadingVertex,
    pub v1: ShadingVertex,
    pub v2: ShadingVertex,
}

/// Parameters for Gouraud-shaded triangle mesh shading (Types 4 & 5).
///
/// New fields may be added without notice; pattern-matching consumers
/// should use `..` to ignore unmatched fields.
#[derive(Clone, Debug)]
pub struct MeshShadingParams {
    pub triangles: Vec<ShadingTriangle>,
    pub ctm: Matrix,
    pub bbox: Option<[f64; 4]>,
    pub color_space: ShadingColorSpace,
    pub overprint: bool,
    /// See [`AxialShadingParams::overprint_mode`].
    pub overprint_mode: i32,
    pub painted_channels: u8,
    /// Pre-sampled color LUT for function-based mesh shadings.
    /// When present, vertex `raw_components[0]` holds a normalized `[0,1]`
    /// function input. The renderer interpolates this per-pixel, then
    /// indexes the LUT instead of Gouraud-interpolating DeviceColor.
    pub color_lut: Option<Arc<Vec<DeviceColor>>>,
    /// Fill alpha from graphics state (0.0–1.0). Default 1.0.
    pub alpha: f64,
    /// Blend mode (0=Normal, …, 15=Luminosity). Default 0.
    pub blend_mode: u8,
    /// PDF `AIS` (alpha-is-shape). Default false.
    pub alpha_is_shape: bool,
    /// See [`AxialShadingParams::rendering_intent`].
    pub rendering_intent: u8,
    /// Transfer function in force when painted: applied by the renderer
    /// to each colour the shading evaluates, and carried for PDF output
    /// (see [`TransferState`]).
    pub transfer: TransferState,
}

/// A patch in a Coons or tensor-product patch mesh.
#[derive(Clone, Debug)]
pub struct ShadingPatch {
    pub points: Vec<(f64, f64)>,
    pub colors: [DeviceColor; 4],
    pub raw_colors: [Vec<f64>; 4],
}

/// Parameters for Coons/tensor-product patch mesh shading (Types 6 & 7).
///
/// New fields may be added without notice; pattern-matching consumers
/// should use `..` to ignore unmatched fields.
#[derive(Clone, Debug)]
pub struct PatchShadingParams {
    pub patches: Vec<ShadingPatch>,
    pub ctm: Matrix,
    pub bbox: Option<[f64; 4]>,
    pub color_space: ShadingColorSpace,
    pub overprint: bool,
    /// See [`AxialShadingParams::overprint_mode`].
    pub overprint_mode: i32,
    pub painted_channels: u8,
    /// When present, vertex `raw_colors[i][0]` holds a normalized `[0,1]`
    /// function input. The renderer interpolates this per-pixel, then
    /// indexes the LUT for per-pixel non-linear function evaluation.
    pub color_lut: Option<Arc<Vec<DeviceColor>>>,
    /// Fill alpha from graphics state (0.0–1.0). Default 1.0.
    pub alpha: f64,
    /// Blend mode (0=Normal, …, 15=Luminosity). Default 0.
    pub blend_mode: u8,
    /// PDF `AIS` (alpha-is-shape). Default false.
    pub alpha_is_shape: bool,
    /// See [`AxialShadingParams::rendering_intent`].
    pub rendering_intent: u8,
    /// Transfer function in force when painted: applied by the renderer
    /// to each colour the shading evaluates, and carried for PDF output
    /// (see [`TransferState`]).
    pub transfer: TransferState,
}

/// Parameters for a tiled pattern fill.
#[derive(Clone, Debug)]
pub struct PatternFillParams {
    /// The path to fill with the pattern.
    pub path: PsPath,
    /// Fill rule for the path.
    pub fill_rule: FillRule,
    /// Pre-rendered display list for a single tile.
    pub tile: DisplayList,
    /// Pattern matrix (pattern space → device space).
    pub pattern_matrix: Matrix,
    /// Bounding box of one tile in pattern space.
    pub bbox: [f64; 4],
    /// Horizontal step between tile origins.
    pub xstep: f64,
    /// Vertical step between tile origins.
    pub ystep: f64,
    /// Paint type: 1 = colored, 2 = uncolored.
    pub paint_type: i32,
    /// For uncolored patterns, the fill color.
    pub underlying_color: Option<DeviceColor>,
    /// Unique pattern ID from pattern_store (for dedup in PDF output).
    pub pattern_id: u32,
    /// When true, tile display list elements have CTMs in device space
    /// (the pattern matrix is already baked into element transforms).
    /// When false, elements are in pattern space and the renderer applies
    /// the pattern_matrix during rendering.
    pub device_space_tile: bool,
    /// When true, the tile content was designed for a Y-flipped coordinate
    /// system (pattern matrix had negative d). The pre-rendered tile must
    /// be vertically flipped before stamping.
    pub flip_tile_y: bool,
    /// For pattern strokes: stroke parameters to expand the centerline path
    /// into a fill outline for masking. When Some, `path` is a user-space
    /// stroke centerline rather than a fill path.
    pub stroke_params: Option<StrokeParams>,
    /// PDF overprint mode (0 or 1). When 1, CMYK(0,0,0,0) pixels in tile
    /// images are transparent (no ink = don't paint).
    pub overprint_mode: i32,
}

// ---------------------------------------------------------------------------
// Default impls
//
// The `Default` impls below pair with `#[non_exhaustive]` on each type:
// downstream consumers (and other workspace crates) construct values via
// `FillParams { color, ..Default::default() }`-style functional update so
// new fields can be added without breaking call sites. The defaults are
// chosen for ergonomics (alpha = 1.0, blend mode = Normal, identity CTM,
// solid black colour, no transfer/halftone/spot state) — not as
// semantically meaningful "blank records".
// ---------------------------------------------------------------------------

impl Default for FillParams {
    fn default() -> Self {
        Self {
            color: DeviceColor::default(),
            fill_rule: FillRule::default(),
            ctm: Matrix::default(),
            is_text_glyph: false,
            overprint: false,
            overprint_mode: 0,
            opm_paired: false,
            painted_channels: 0,
            is_device_cmyk: false,
            spot_color: None,
            icc_color: None,
            rendering_intent: crate::rendering_intent::RELATIVE_COLORIMETRIC,
            transfer: TransferState::default(),
            halftone: HalftoneState::default(),
            bg_ucr: BgUcrState::default(),
            alpha: 1.0,
            blend_mode: 0,
            alpha_is_shape: false,
        }
    }
}

impl Default for StrokeParams {
    fn default() -> Self {
        Self {
            color: DeviceColor::default(),
            line_width: 1.0,
            line_cap: LineCap::default(),
            line_join: LineJoin::default(),
            miter_limit: 10.0,
            dash_pattern: DashPattern::default(),
            ctm: Matrix::default(),
            stroke_adjust: false,
            is_text_glyph: false,
            overprint: false,
            overprint_mode: 0,
            opm_paired: false,
            painted_channels: 0,
            is_device_cmyk: false,
            spot_color: None,
            icc_color: None,
            rendering_intent: crate::rendering_intent::RELATIVE_COLORIMETRIC,
            transfer: TransferState::default(),
            halftone: HalftoneState::default(),
            bg_ucr: BgUcrState::default(),
            alpha: 1.0,
            blend_mode: 0,
            alpha_is_shape: false,
        }
    }
}

impl Default for TextParams {
    fn default() -> Self {
        Self {
            text: Vec::new(),
            start_x: 0.0,
            start_y: 0.0,
            font_entity: 0,
            font_name: Vec::new(),
            font_type: 1,
            font_size: 0.0,
            color: DeviceColor::default(),
            ctm: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            font_matrix: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            paint_type: 0,
            stroke_width: 0.0,
            spot_color: None,
            icc_color: None,
            rendering_intent: crate::rendering_intent::RELATIVE_COLORIMETRIC,
            transfer: TransferState::default(),
            halftone: HalftoneState::default(),
            bg_ucr: BgUcrState::default(),
            fill_opacity: 1.0,
            stroke_opacity: 1.0,
            blend_mode: 0,
            alpha_is_shape: false,
            text_knockout: true,
        }
    }
}

impl Default for ImageParams {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            color_space: ImageColorSpace::default(),
            bits_per_component: 8,
            ctm: Matrix::default(),
            image_matrix: Matrix::default(),
            interpolate: false,
            mask_color: None,
            alpha: 1.0,
            blend_mode: 0,
            overprint: false,
            overprint_mode: 0,
            opm_paired: false,
            painted_channels: 0,
            alpha_is_shape: false,
            rendering_intent: crate::rendering_intent::RELATIVE_COLORIMETRIC,
            transfer: TransferState::default(),
        }
    }
}

impl Default for ColorStop {
    fn default() -> Self {
        Self {
            position: 0.0,
            color: DeviceColor::default(),
            raw_components: Vec::new(),
            source_components: Vec::new(),
        }
    }
}

impl Default for AxialShadingParams {
    fn default() -> Self {
        Self {
            x0: 0.0,
            y0: 0.0,
            x1: 0.0,
            y1: 0.0,
            color_stops: Vec::new(),
            extend_start: false,
            extend_end: false,
            ctm: Matrix::default(),
            bbox: None,
            color_space: ShadingColorSpace::default(),
            overprint: false,
            overprint_mode: 0,
            painted_channels: 0,
            alpha: 1.0,
            blend_mode: 0,
            alpha_is_shape: false,
            spot_tint_blend: false,
            rendering_intent: crate::rendering_intent::RELATIVE_COLORIMETRIC,
            transfer: TransferState::default(),
        }
    }
}

impl Default for RadialShadingParams {
    fn default() -> Self {
        Self {
            x0: 0.0,
            y0: 0.0,
            r0: 0.0,
            x1: 0.0,
            y1: 0.0,
            r1: 0.0,
            color_stops: Vec::new(),
            extend_start: false,
            extend_end: false,
            ctm: Matrix::default(),
            bbox: None,
            color_space: ShadingColorSpace::default(),
            overprint: false,
            overprint_mode: 0,
            painted_channels: 0,
            alpha: 1.0,
            blend_mode: 0,
            alpha_is_shape: false,
            spot_tint_blend: false,
            rendering_intent: crate::rendering_intent::RELATIVE_COLORIMETRIC,
            transfer: TransferState::default(),
        }
    }
}

impl Default for MeshShadingParams {
    fn default() -> Self {
        Self {
            triangles: Vec::new(),
            ctm: Matrix::default(),
            bbox: None,
            color_space: ShadingColorSpace::default(),
            overprint: false,
            overprint_mode: 0,
            painted_channels: 0,
            color_lut: None,
            alpha: 1.0,
            blend_mode: 0,
            alpha_is_shape: false,
            rendering_intent: crate::rendering_intent::RELATIVE_COLORIMETRIC,
            transfer: TransferState::default(),
        }
    }
}

impl Default for PatchShadingParams {
    fn default() -> Self {
        Self {
            patches: Vec::new(),
            ctm: Matrix::default(),
            bbox: None,
            color_space: ShadingColorSpace::default(),
            overprint: false,
            overprint_mode: 0,
            painted_channels: 0,
            color_lut: None,
            alpha: 1.0,
            blend_mode: 0,
            alpha_is_shape: false,
            rendering_intent: crate::rendering_intent::RELATIVE_COLORIMETRIC,
            transfer: TransferState::default(),
        }
    }
}

/// Trait for consuming rendered page pixel data.
pub trait PageSink: Send {
    /// Start a new page with the given pixel dimensions.
    fn begin_page(&mut self, width: u32, height: u32) -> Result<(), String>;

    /// Write one or more rows of RGBA pixel data (4 bytes per pixel, row-major).
    fn write_rows(&mut self, rgba_rows: &[u8], num_rows: u32) -> Result<(), String>;

    /// Finish the current page. May block (e.g., viewer waits for user input).
    fn end_page(&mut self) -> Result<(), String>;
}

/// Factory for creating per-page sinks.
pub trait PageSinkFactory: Send + Sync {
    /// Create a new sink for a single page.
    fn create_sink(&self, output_path: &str) -> Result<Box<dyn PageSink>, String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn painted_channels_union_the_process_colorants() {
        assert_eq!(painted_channels_for_colorants(&[b"Cyan"]), CMYK_C);
        assert_eq!(
            painted_channels_for_colorants(&[b"Magenta".as_slice(), b"PANTONE 273 C", b"Black"]),
            CMYK_M | CMYK_K
        );
        assert_eq!(painted_channels_for_colorants(&[b"All"]), CMYK_ALL);
        assert_eq!(painted_channels_for_colorants(&[b"Spot"]), 0);
        assert_eq!(painted_channels_for_colorants::<&[u8]>(&[]), 0);
    }

    #[test]
    fn separation_process_part_is_zero_for_a_spot() {
        assert_eq!(
            separation_process_cmyk(b"Yellow", 0.4),
            Some((0.0, 0.0, 0.4, 0.0))
        );
        assert_eq!(
            separation_process_cmyk(b"PANTONE 273 C", 1.0),
            Some((0.0, 0.0, 0.0, 0.0))
        );
        assert_eq!(
            separation_process_cmyk(b"None", 1.0),
            Some((0.0, 0.0, 0.0, 0.0))
        );
        assert_eq!(separation_process_cmyk(b"All", 1.0), None);
        assert_eq!(
            separation_process_cmyk(b"Cyan", 1.5),
            Some((1.0, 0.0, 0.0, 0.0))
        );
    }

    #[test]
    fn devicen_process_part_stacks_subtractively() {
        let names = [b"Cyan".as_slice(), b"Spot", b"All"];
        let (c, m, y, k) = devicen_process_cmyk(&names, &[0.5, 1.0, 0.5]).unwrap();
        assert!((c - 0.75).abs() < 1e-12);
        assert!((m - 0.5).abs() < 1e-12);
        assert!((y - 0.5).abs() < 1e-12);
        assert!((k - 0.5).abs() < 1e-12);
        assert_eq!(devicen_process_cmyk(&names, &[1.0]), None);
    }
}
