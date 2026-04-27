// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Typed PDF annotations (links, sticky notes, highlights, stamps,
//! callouts, attachments, etc.).
//!
//! `stet-pdf-reader` already knows about the `/Annots` array on each
//! page and *renders* annotation appearance streams during page
//! rasterization (`content/mod.rs::render_annotation`). This module
//! exposes the same annotations as **structured data** for callers
//! that want to inspect, index, or convert them — link extractors,
//! review aggregators, accessibility tools.
//!
//! The full PDF annotation set is large (ISO 32000-2 §12.5). This
//! module parses the common subtypes — Link, Text, FreeText, the four
//! markup annotations (Highlight / Underline / Squiggly / StrikeOut),
//! Line, Square, Circle, Polygon, PolyLine, Ink, Stamp, Caret,
//! FileAttachment, Popup — into typed structs. Less-common subtypes
//! (Screen, PrinterMark, TrapNet, Watermark, Sound, Movie, Widget,
//! and any unknown) are exposed via [`AnnotationKindData::Minimal`]
//! with the common fields populated; consumers can still inspect
//! `subtype` and `kind_data`'s `Other` variant for the raw name.
//!
//! Widget annotations get a placeholder here; Phase 5 of the reader
//! plan adds the dedicated `FormField` type that cross-links them to
//! the document's AcroForm.

use crate::destination::{Action, Destination, parse_action, parse_destination};
use crate::diagnostics::{LocationHint, ParsePhase, Severity, WarningSink};
use crate::metadata::{PdfDate, pdf_string_to_rust_pub};
use crate::objects::{PdfDict, PdfObj};
use crate::page_tree::PageInfo;
use crate::resolver::Resolver;

/// One PDF annotation.
///
/// Common fields (rect, contents, flags, color, border, appearance)
/// live directly on this struct. Subtype-specific fields live in
/// [`AnnotationKindData`].
#[derive(Debug, Clone)]
pub struct Annotation {
    /// `/Subtype` value, parsed into a typed enum.
    pub kind: AnnotationKind,
    /// `/Rect` — annotation rectangle in default user space
    /// `[llx, lly, urx, ury]`.
    pub rect: [f64; 4],
    /// `/Contents` — text contents, alternate description, or the
    /// markup text for markup annotations.
    pub contents: Option<String>,
    /// `/NM` — unique annotation identifier within the document.
    pub name: Option<String>,
    /// `/M` — modification timestamp.
    pub modified: Option<AnnotationDate>,
    /// `/T` — title (annotator name) for markup annotations.
    pub title: Option<String>,
    /// `/Subj` — subject (markup annotations).
    pub subject: Option<String>,
    /// `/F` — annotation behavior flags.
    pub flags: AnnotationFlags,
    /// `/C` — color used for the annotation's icon, border, or fill.
    pub color: Option<AnnotationColor>,
    /// `/Border` — `[hradius vradius width [dash array]]`.
    pub border: Option<Border>,
    /// `/AP` — appearance stream presence (we don't expose the stream
    /// itself; the renderer consumes it directly). `true` if the
    /// annotation has any appearance entry.
    pub has_appearance: bool,
    /// Subtype-specific fields.
    pub kind_data: AnnotationKindData,
}

/// `/M` modification entry — usually a PDF date string, occasionally
/// an arbitrary string.
#[derive(Debug, Clone, PartialEq)]
pub enum AnnotationDate {
    /// Successfully parsed as a PDF date.
    Date(PdfDate),
    /// Free-form string (`/M` is sometimes used loosely).
    Raw(String),
}

/// Annotation subtype.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnnotationKind {
    Text,
    Link,
    FreeText,
    Line,
    Square,
    Circle,
    Polygon,
    PolyLine,
    Highlight,
    Underline,
    Squiggly,
    StrikeOut,
    Stamp,
    Caret,
    Ink,
    Popup,
    FileAttachment,
    Widget,
    Screen,
    PrinterMark,
    TrapNet,
    Watermark,
    /// Deprecated: PDF 1.2 sound annotation. Marker only.
    Sound,
    /// Deprecated: PDF 1.2 movie annotation. Marker only.
    Movie,
    /// `/3D` annotation. Not parsed beyond marker.
    ThreeD,
    /// `/RichMedia` annotation. Not parsed.
    RichMedia,
    /// Unknown or unparsed `/Subtype`. Raw name preserved.
    Other(String),
}

impl AnnotationKind {
    fn from_name(name: &[u8]) -> Self {
        match name {
            b"Text" => AnnotationKind::Text,
            b"Link" => AnnotationKind::Link,
            b"FreeText" => AnnotationKind::FreeText,
            b"Line" => AnnotationKind::Line,
            b"Square" => AnnotationKind::Square,
            b"Circle" => AnnotationKind::Circle,
            b"Polygon" => AnnotationKind::Polygon,
            b"PolyLine" => AnnotationKind::PolyLine,
            b"Highlight" => AnnotationKind::Highlight,
            b"Underline" => AnnotationKind::Underline,
            b"Squiggly" => AnnotationKind::Squiggly,
            b"StrikeOut" => AnnotationKind::StrikeOut,
            b"Stamp" => AnnotationKind::Stamp,
            b"Caret" => AnnotationKind::Caret,
            b"Ink" => AnnotationKind::Ink,
            b"Popup" => AnnotationKind::Popup,
            b"FileAttachment" => AnnotationKind::FileAttachment,
            b"Widget" => AnnotationKind::Widget,
            b"Screen" => AnnotationKind::Screen,
            b"PrinterMark" => AnnotationKind::PrinterMark,
            b"TrapNet" => AnnotationKind::TrapNet,
            b"Watermark" => AnnotationKind::Watermark,
            b"Sound" => AnnotationKind::Sound,
            b"Movie" => AnnotationKind::Movie,
            b"3D" => AnnotationKind::ThreeD,
            b"RichMedia" => AnnotationKind::RichMedia,
            other => AnnotationKind::Other(String::from_utf8_lossy(other).into_owned()),
        }
    }
}

/// `/F` annotation flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AnnotationFlags {
    pub invisible: bool,
    pub hidden: bool,
    pub print: bool,
    pub no_zoom: bool,
    pub no_rotate: bool,
    pub no_view: bool,
    pub read_only: bool,
    pub locked: bool,
    pub toggle_no_view: bool,
    pub locked_contents: bool,
}

impl AnnotationFlags {
    fn from_bits(bits: i64) -> Self {
        Self {
            invisible: bits & 0x0001 != 0,
            hidden: bits & 0x0002 != 0,
            print: bits & 0x0004 != 0,
            no_zoom: bits & 0x0008 != 0,
            no_rotate: bits & 0x0010 != 0,
            no_view: bits & 0x0020 != 0,
            read_only: bits & 0x0040 != 0,
            locked: bits & 0x0080 != 0,
            toggle_no_view: bits & 0x0100 != 0,
            locked_contents: bits & 0x0200 != 0,
        }
    }
}

/// `/C` color array — interpreted by length per spec.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AnnotationColor {
    /// Empty array — transparent / no color.
    Transparent,
    /// One component: DeviceGray.
    Gray(f32),
    /// Three components: DeviceRGB.
    Rgb([f32; 3]),
    /// Four components: DeviceCMYK.
    Cmyk([f32; 4]),
}

impl AnnotationColor {
    fn from_array(arr: &[PdfObj]) -> Option<Self> {
        let n = |i: usize| arr.get(i).and_then(|o| o.as_f64()).map(|v| v as f32);
        match arr.len() {
            0 => Some(AnnotationColor::Transparent),
            1 => Some(AnnotationColor::Gray(n(0)?)),
            3 => Some(AnnotationColor::Rgb([n(0)?, n(1)?, n(2)?])),
            4 => Some(AnnotationColor::Cmyk([n(0)?, n(1)?, n(2)?, n(3)?])),
            _ => None,
        }
    }
}

/// `/Border` entry: `[hradius vradius width [dash]]`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Border {
    pub h_radius: f64,
    pub v_radius: f64,
    pub width: f64,
    pub dash: Vec<f64>,
}

/// Subtype-specific annotation data.
#[derive(Debug, Clone)]
pub enum AnnotationKindData {
    Link(LinkAnnotation),
    Text(TextAnnotation),
    FreeText(FreeTextAnnotation),
    /// Highlight, Underline, Squiggly, StrikeOut.
    Markup(MarkupAnnotation),
    Line(LineAnnotation),
    /// Square / Circle.
    Shape(ShapeAnnotation),
    /// Polygon / PolyLine.
    Polygon(PolygonAnnotation),
    Ink(InkAnnotation),
    Stamp(StampAnnotation),
    Caret(CaretAnnotation),
    FileAttachment(FileAttachmentAnnotation),
    Popup(PopupAnnotation),
    /// For unhandled/rare subtypes (Screen, PrinterMark, TrapNet,
    /// Watermark, Sound, Movie, Widget, 3D, RichMedia, Other).
    Minimal,
}

/// `/Subtype /Link`.
#[derive(Debug, Clone, Default)]
pub struct LinkAnnotation {
    /// `/A` action (preferred over `/Dest` when both present).
    pub action: Option<Action>,
    /// `/Dest` destination.
    pub destination: Option<Destination>,
    /// `/H` highlight mode (`/N` = none, `/I` = invert, `/O` = outline,
    /// `/P` = push). Stored as raw name.
    pub highlight_mode: Option<String>,
    /// `/QuadPoints` — for links over multi-line text. Each quad is
    /// 4 (x, y) corner points (8 floats).
    pub quad_points: Vec<[f64; 8]>,
}

/// `/Subtype /Text` — sticky note.
#[derive(Debug, Clone, Default)]
pub struct TextAnnotation {
    /// `/Open` — whether the popup is initially open.
    pub open: bool,
    /// `/Name` — icon name (`/Comment`, `/Note`, `/Key`, `/Help`,
    /// `/NewParagraph`, `/Paragraph`, `/Insert`).
    pub icon: Option<String>,
    /// `/State` — review state for state-model annotations.
    pub state: Option<String>,
    /// `/StateModel` — state model identifier.
    pub state_model: Option<String>,
}

/// `/Subtype /FreeText` — visible text on the page.
#[derive(Debug, Clone, Default)]
pub struct FreeTextAnnotation {
    /// `/DA` — default appearance string (graphics state ops + font).
    pub default_appearance: Option<String>,
    /// `/Q` — quadding (text alignment): 0 = left, 1 = center, 2 = right.
    pub quadding: u8,
    /// `/RC` — rich content as XHTML/XFA fragment.
    pub rich_content: Option<String>,
    /// `/DS` — default style string.
    pub default_style: Option<String>,
    /// `/CL` — callout line points (4 or 6 numbers).
    pub callout_line: Option<Vec<f64>>,
    /// `/IT` — intent (`/FreeTextCallout`, `/FreeTextTypeWriter`).
    pub intent: Option<String>,
    /// `/RD` — rect difference (border padding inside Rect).
    pub rect_diff: Option<[f64; 4]>,
    /// `/LE` — line ending style (callout's start).
    pub line_ending: Option<String>,
}

/// Markup annotations (Highlight / Underline / Squiggly / StrikeOut).
#[derive(Debug, Clone, Default)]
pub struct MarkupAnnotation {
    /// `/QuadPoints` — 8 numbers per quad describing the marked region.
    /// Each quad is `[x1 y1 x2 y2 x3 y3 x4 y4]` (corners, possibly in
    /// non-canonical order).
    pub quad_points: Vec<[f64; 8]>,
}

/// `/Subtype /Line`.
#[derive(Debug, Clone, Default)]
pub struct LineAnnotation {
    /// `/L` — `[x1 y1 x2 y2]`.
    pub endpoints: [f64; 4],
    /// `/LE` — line endings: `[start_style end_style]`.
    pub line_ending: Option<[String; 2]>,
    /// `/IC` — interior color (for filled endings).
    pub interior_color: Option<AnnotationColor>,
    /// `/LL` — leader line length.
    pub leader_length: Option<f64>,
    /// `/LLE` — leader line extension.
    pub leader_extension: Option<f64>,
    /// `/LLO` — leader line offset.
    pub leader_offset: Option<f64>,
    /// `/Cap` — show caption.
    pub cap: Option<bool>,
    /// `/CP` — caption position (`/Inline` or `/Top`).
    pub cap_position: Option<String>,
    /// `/IT` — intent (`/LineArrow`, `/LineDimension`).
    pub intent: Option<String>,
}

/// `/Subtype /Square` or `/Circle`.
#[derive(Debug, Clone, Default)]
pub struct ShapeAnnotation {
    /// `/IC` — interior fill color.
    pub interior_color: Option<AnnotationColor>,
    /// `/RD` — rect difference (border padding).
    pub rect_diff: Option<[f64; 4]>,
}

/// `/Subtype /Polygon` or `/PolyLine`.
#[derive(Debug, Clone, Default)]
pub struct PolygonAnnotation {
    /// `/Vertices` — flat list of alternating x, y coordinates.
    pub vertices: Vec<f64>,
    /// `/LE` — line endings (PolyLine only).
    pub line_ending: Option<[String; 2]>,
    /// `/IC` — interior fill (Polygon only).
    pub interior_color: Option<AnnotationColor>,
    /// `/IT` — intent (`/PolygonCloud`, `/PolyLineDimension`, etc.).
    pub intent: Option<String>,
}

/// `/Subtype /Ink`.
#[derive(Debug, Clone, Default)]
pub struct InkAnnotation {
    /// `/InkList` — array of strokes, each a flat list of alternating
    /// x, y coordinates.
    pub strokes: Vec<Vec<f64>>,
}

/// `/Subtype /Stamp`.
#[derive(Debug, Clone, Default)]
pub struct StampAnnotation {
    /// `/Name` — stamp icon name (`/Approved`, `/Confidential`,
    /// `/Draft`, etc., or a custom name).
    pub icon: Option<String>,
    /// `/IT` — intent.
    pub intent: Option<String>,
}

/// `/Subtype /Caret`.
#[derive(Debug, Clone, Default)]
pub struct CaretAnnotation {
    /// `/RD` — rect difference inside Rect for the caret glyph.
    pub rect_diff: Option<[f64; 4]>,
    /// `/Sy` — caret symbol (`/None` or `/P` paragraph).
    pub symbol: Option<String>,
}

/// `/Subtype /FileAttachment`.
#[derive(Debug, Clone, Default)]
pub struct FileAttachmentAnnotation {
    /// `/FS` — filename/path. Just the name; consumers wanting the
    /// embedded bytes go through the Phase 6 `embedded_files()` API.
    pub filename: Option<String>,
    /// `/Name` — icon (`/Graph`, `/Paperclip`, `/PushPin`, `/Tag`).
    pub icon: Option<String>,
}

/// `/Subtype /Popup`.
#[derive(Debug, Clone, Default)]
pub struct PopupAnnotation {
    /// `/Open` — whether the popup is shown on first display.
    pub open: bool,
    /// `/Parent` — object number of the parent annotation that owns
    /// this popup. `None` if the popup is freestanding (rare).
    pub parent_obj_num: Option<u32>,
}

/// Parse all annotations on a page from its [`PageInfo::annots`] list.
///
/// Skips annotations missing the required `/Rect` field; the skipped
/// entry produces a [`ParseWarning`] in `sink`. Always returns a
/// `Vec` (possibly empty).
///
/// [`ParseWarning`]: crate::ParseWarning
pub fn parse_page_annotations(
    resolver: &Resolver,
    pages: &[PageInfo],
    page_index: usize,
    sink: &WarningSink,
) -> Vec<Annotation> {
    let Some(page) = pages.get(page_index) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(page.annots.len());
    for &(num, gen_num) in &page.annots {
        let Ok(obj) = resolver.resolve(num, gen_num) else {
            sink.record(
                ParsePhase::Annotations { page: page_index },
                Some(LocationHint::Object {
                    obj_num: num,
                    gen_num,
                }),
                Severity::Warning,
                "annotation object could not be resolved; skipped",
            );
            continue;
        };
        let Some(dict) = obj.as_dict() else {
            sink.record(
                ParsePhase::Annotations { page: page_index },
                Some(LocationHint::Object {
                    obj_num: num,
                    gen_num,
                }),
                Severity::Warning,
                "annotation object is not a dict; skipped",
            );
            continue;
        };
        match parse_annotation(resolver, pages, dict) {
            Some(annot) => out.push(annot),
            None => sink.record(
                ParsePhase::Annotations { page: page_index },
                Some(LocationHint::Object {
                    obj_num: num,
                    gen_num,
                }),
                Severity::Warning,
                "annotation missing or malformed /Rect; skipped",
            ),
        }
    }
    out
}

/// Parse a single annotation dict into a typed [`Annotation`].
///
/// Returns `None` if `/Rect` is missing or malformed (the only
/// strictly-required field).
pub fn parse_annotation(
    resolver: &Resolver,
    pages: &[PageInfo],
    dict: &PdfDict,
) -> Option<Annotation> {
    let rect = dict.get_array(b"Rect").and_then(parse_rect)?;
    let subtype_bytes = dict.get_name(b"Subtype").unwrap_or(b"");
    let kind = AnnotationKind::from_name(subtype_bytes);

    let contents = dict.get(b"Contents").and_then(pdf_string_to_rust_pub);
    let name = dict.get(b"NM").and_then(pdf_string_to_rust_pub);
    let modified = dict.get(b"M").and_then(parse_annotation_date);
    let title = dict.get(b"T").and_then(pdf_string_to_rust_pub);
    let subject = dict.get(b"Subj").and_then(pdf_string_to_rust_pub);
    let flags = AnnotationFlags::from_bits(dict.get_int(b"F").unwrap_or(0));
    let color = dict.get_array(b"C").and_then(AnnotationColor::from_array);
    let border = parse_border(dict);
    let has_appearance = dict.get(b"AP").is_some();

    let kind_data = parse_kind_data(resolver, pages, &kind, dict);

    Some(Annotation {
        kind,
        rect,
        contents,
        name,
        modified,
        title,
        subject,
        flags,
        color,
        border,
        has_appearance,
        kind_data,
    })
}

fn parse_rect(arr: &[PdfObj]) -> Option<[f64; 4]> {
    if arr.len() < 4 {
        return None;
    }
    Some([
        arr[0].as_f64()?,
        arr[1].as_f64()?,
        arr[2].as_f64()?,
        arr[3].as_f64()?,
    ])
}

fn parse_rect_diff(arr: &[PdfObj]) -> Option<[f64; 4]> {
    if arr.len() < 4 {
        return None;
    }
    Some([
        arr[0].as_f64()?,
        arr[1].as_f64()?,
        arr[2].as_f64()?,
        arr[3].as_f64()?,
    ])
}

fn parse_border(dict: &PdfDict) -> Option<Border> {
    let arr = dict.get_array(b"Border")?;
    if arr.len() < 3 {
        return None;
    }
    let mut border = Border {
        h_radius: arr[0].as_f64().unwrap_or(0.0),
        v_radius: arr[1].as_f64().unwrap_or(0.0),
        width: arr[2].as_f64().unwrap_or(1.0),
        dash: Vec::new(),
    };
    if let Some(dash_arr) = arr.get(3).and_then(|o| o.as_array()) {
        border.dash = dash_arr.iter().filter_map(|o| o.as_f64()).collect();
    }
    Some(border)
}

fn parse_annotation_date(obj: &PdfObj) -> Option<AnnotationDate> {
    let s = obj.as_str()?;
    if let Some(date) = PdfDate::parse(s) {
        Some(AnnotationDate::Date(date))
    } else {
        Some(AnnotationDate::Raw(String::from_utf8_lossy(s).into_owned()))
    }
}

fn parse_quad_points(arr: &[PdfObj]) -> Vec<[f64; 8]> {
    let mut out = Vec::new();
    let coords: Vec<f64> = arr.iter().filter_map(|o| o.as_f64()).collect();
    let mut i = 0;
    while i + 8 <= coords.len() {
        out.push([
            coords[i],
            coords[i + 1],
            coords[i + 2],
            coords[i + 3],
            coords[i + 4],
            coords[i + 5],
            coords[i + 6],
            coords[i + 7],
        ]);
        i += 8;
    }
    out
}

fn parse_line_ending_pair(arr: &[PdfObj]) -> Option<[String; 2]> {
    if arr.len() < 2 {
        return None;
    }
    let a = arr[0]
        .as_name()
        .map(|n| String::from_utf8_lossy(n).into_owned())?;
    let b = arr[1]
        .as_name()
        .map(|n| String::from_utf8_lossy(n).into_owned())?;
    Some([a, b])
}

fn parse_kind_data(
    resolver: &Resolver,
    pages: &[PageInfo],
    kind: &AnnotationKind,
    dict: &PdfDict,
) -> AnnotationKindData {
    match kind {
        AnnotationKind::Link => {
            let action = dict
                .get(b"A")
                .and_then(|o| parse_action(resolver, pages, o));
            let destination = dict
                .get(b"Dest")
                .and_then(|o| parse_destination(resolver, pages, o));
            let highlight_mode = dict
                .get_name(b"H")
                .map(|n| String::from_utf8_lossy(n).into_owned());
            let quad_points = dict
                .get_array(b"QuadPoints")
                .map(parse_quad_points)
                .unwrap_or_default();
            AnnotationKindData::Link(LinkAnnotation {
                action,
                destination,
                highlight_mode,
                quad_points,
            })
        }
        AnnotationKind::Text => AnnotationKindData::Text(TextAnnotation {
            open: dict.get(b"Open").and_then(as_bool).unwrap_or(false),
            icon: dict
                .get_name(b"Name")
                .map(|n| String::from_utf8_lossy(n).into_owned()),
            state: dict.get(b"State").and_then(pdf_string_to_rust_pub),
            state_model: dict.get(b"StateModel").and_then(pdf_string_to_rust_pub),
        }),
        AnnotationKind::FreeText => AnnotationKindData::FreeText(FreeTextAnnotation {
            default_appearance: dict.get(b"DA").and_then(pdf_string_to_rust_pub),
            quadding: dict.get_int(b"Q").unwrap_or(0).clamp(0, 2) as u8,
            rich_content: dict.get(b"RC").and_then(pdf_string_to_rust_pub),
            default_style: dict.get(b"DS").and_then(pdf_string_to_rust_pub),
            callout_line: dict
                .get_array(b"CL")
                .map(|a| a.iter().filter_map(|o| o.as_f64()).collect()),
            intent: dict
                .get_name(b"IT")
                .map(|n| String::from_utf8_lossy(n).into_owned()),
            rect_diff: dict.get_array(b"RD").and_then(parse_rect_diff),
            line_ending: dict
                .get_name(b"LE")
                .map(|n| String::from_utf8_lossy(n).into_owned()),
        }),
        AnnotationKind::Highlight
        | AnnotationKind::Underline
        | AnnotationKind::Squiggly
        | AnnotationKind::StrikeOut => AnnotationKindData::Markup(MarkupAnnotation {
            quad_points: dict
                .get_array(b"QuadPoints")
                .map(parse_quad_points)
                .unwrap_or_default(),
        }),
        AnnotationKind::Line => {
            let endpoints = dict
                .get_array(b"L")
                .and_then(parse_rect)
                .unwrap_or_default();
            AnnotationKindData::Line(LineAnnotation {
                endpoints,
                line_ending: dict.get_array(b"LE").and_then(parse_line_ending_pair),
                interior_color: dict.get_array(b"IC").and_then(AnnotationColor::from_array),
                leader_length: dict.get_f64(b"LL"),
                leader_extension: dict.get_f64(b"LLE"),
                leader_offset: dict.get_f64(b"LLO"),
                cap: dict.get(b"Cap").and_then(as_bool),
                cap_position: dict
                    .get_name(b"CP")
                    .map(|n| String::from_utf8_lossy(n).into_owned()),
                intent: dict
                    .get_name(b"IT")
                    .map(|n| String::from_utf8_lossy(n).into_owned()),
            })
        }
        AnnotationKind::Square | AnnotationKind::Circle => {
            AnnotationKindData::Shape(ShapeAnnotation {
                interior_color: dict.get_array(b"IC").and_then(AnnotationColor::from_array),
                rect_diff: dict.get_array(b"RD").and_then(parse_rect_diff),
            })
        }
        AnnotationKind::Polygon | AnnotationKind::PolyLine => {
            AnnotationKindData::Polygon(PolygonAnnotation {
                vertices: dict
                    .get_array(b"Vertices")
                    .map(|a| a.iter().filter_map(|o| o.as_f64()).collect())
                    .unwrap_or_default(),
                line_ending: dict.get_array(b"LE").and_then(parse_line_ending_pair),
                interior_color: dict.get_array(b"IC").and_then(AnnotationColor::from_array),
                intent: dict
                    .get_name(b"IT")
                    .map(|n| String::from_utf8_lossy(n).into_owned()),
            })
        }
        AnnotationKind::Ink => {
            let strokes = dict
                .get_array(b"InkList")
                .map(|outer| {
                    outer
                        .iter()
                        .filter_map(|stroke| {
                            stroke
                                .as_array()
                                .map(|a| a.iter().filter_map(|o| o.as_f64()).collect::<Vec<_>>())
                        })
                        .collect()
                })
                .unwrap_or_default();
            AnnotationKindData::Ink(InkAnnotation { strokes })
        }
        AnnotationKind::Stamp => AnnotationKindData::Stamp(StampAnnotation {
            icon: dict
                .get_name(b"Name")
                .map(|n| String::from_utf8_lossy(n).into_owned()),
            intent: dict
                .get_name(b"IT")
                .map(|n| String::from_utf8_lossy(n).into_owned()),
        }),
        AnnotationKind::Caret => AnnotationKindData::Caret(CaretAnnotation {
            rect_diff: dict.get_array(b"RD").and_then(parse_rect_diff),
            symbol: dict
                .get_name(b"Sy")
                .map(|n| String::from_utf8_lossy(n).into_owned()),
        }),
        AnnotationKind::FileAttachment => {
            let filename = parse_file_spec_value(resolver, dict.get(b"FS"));
            AnnotationKindData::FileAttachment(FileAttachmentAnnotation {
                filename,
                icon: dict
                    .get_name(b"Name")
                    .map(|n| String::from_utf8_lossy(n).into_owned()),
            })
        }
        AnnotationKind::Popup => AnnotationKindData::Popup(PopupAnnotation {
            open: dict.get(b"Open").and_then(as_bool).unwrap_or(false),
            parent_obj_num: dict.get_ref(b"Parent").map(|(n, _)| n),
        }),
        // Phase-5 territory plus the rare/deprecated/unknown set.
        _ => AnnotationKindData::Minimal,
    }
}

fn as_bool(obj: &PdfObj) -> Option<bool> {
    match obj {
        PdfObj::Bool(b) => Some(*b),
        _ => None,
    }
}

fn parse_file_spec_value(resolver: &Resolver, obj: Option<&PdfObj>) -> Option<String> {
    let obj = obj?;
    let resolved = resolver.deref(obj).ok()?;
    if let Some(s) = resolved.as_str() {
        return Some(crate::metadata::decode_pdf_text_string_pub(s));
    }
    if let Some(d) = resolved.as_dict() {
        if let Some(uf) = d.get(b"UF").and_then(pdf_string_to_rust_pub) {
            return Some(uf);
        }
        if let Some(f) = d.get(b"F").and_then(pdf_string_to_rust_pub) {
            return Some(f);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_decode() {
        let f = AnnotationFlags::from_bits(0x0085); // Print + Locked + Invisible
        assert!(f.invisible);
        assert!(f.print);
        assert!(f.locked);
        assert!(!f.hidden);
        assert!(!f.read_only);
    }

    #[test]
    fn color_array_lengths() {
        assert_eq!(
            AnnotationColor::from_array(&[]),
            Some(AnnotationColor::Transparent)
        );
        assert_eq!(
            AnnotationColor::from_array(&[PdfObj::Real(0.5)]),
            Some(AnnotationColor::Gray(0.5))
        );
        assert_eq!(
            AnnotationColor::from_array(
                &[PdfObj::Real(1.0), PdfObj::Real(0.0), PdfObj::Real(0.0),]
            ),
            Some(AnnotationColor::Rgb([1.0, 0.0, 0.0]))
        );
        assert_eq!(
            AnnotationColor::from_array(&[
                PdfObj::Real(0.0),
                PdfObj::Real(0.0),
                PdfObj::Real(0.0),
                PdfObj::Real(1.0),
            ]),
            Some(AnnotationColor::Cmyk([0.0, 0.0, 0.0, 1.0]))
        );
        // Length 2 is invalid.
        assert!(AnnotationColor::from_array(&[PdfObj::Real(0.5), PdfObj::Real(0.5)]).is_none());
    }

    #[test]
    fn quad_points_split_into_quads() {
        let coords: Vec<PdfObj> = (0..16).map(|i| PdfObj::Real(i as f64)).collect();
        let quads = parse_quad_points(&coords);
        assert_eq!(quads.len(), 2);
        assert_eq!(quads[0], [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]);
        assert_eq!(quads[1], [8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0]);
    }

    #[test]
    fn quad_points_drops_partial_trailing() {
        let coords: Vec<PdfObj> = (0..12).map(|i| PdfObj::Real(i as f64)).collect();
        let quads = parse_quad_points(&coords);
        assert_eq!(quads.len(), 1, "trailing 4 coords are dropped");
    }

    #[test]
    fn subtype_known_and_unknown() {
        assert_eq!(AnnotationKind::from_name(b"Link"), AnnotationKind::Link);
        assert_eq!(
            AnnotationKind::from_name(b"Highlight"),
            AnnotationKind::Highlight
        );
        assert_eq!(
            AnnotationKind::from_name(b"InventedSubtype"),
            AnnotationKind::Other("InventedSubtype".to_string())
        );
    }

    #[test]
    fn border_default_when_short() {
        // /Border with only 2 entries is invalid → None.
        let mut dict = PdfDict::new();
        dict.insert(
            b"Border".to_vec(),
            PdfObj::Array(vec![PdfObj::Int(0), PdfObj::Int(0)]),
        );
        assert!(parse_border(&dict).is_none());
    }

    #[test]
    fn border_with_dash_array() {
        let mut dict = PdfDict::new();
        dict.insert(
            b"Border".to_vec(),
            PdfObj::Array(vec![
                PdfObj::Int(0),
                PdfObj::Int(0),
                PdfObj::Real(2.0),
                PdfObj::Array(vec![PdfObj::Real(3.0), PdfObj::Real(2.0)]),
            ]),
        );
        let b = parse_border(&dict).unwrap();
        assert_eq!(b.width, 2.0);
        assert_eq!(b.dash, vec![3.0, 2.0]);
    }
}
