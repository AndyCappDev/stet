// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `pdfmark` authoring records and accumulator.
//!
//! `pdfmark` is the PostScript-to-PDF authoring bridge. PostScript code
//! issues `pdfmark` calls during interpretation; the interpreter parks the
//! resulting [`PdfMarkRecord`]s on a [`PdfMarkBuffer`] hanging off
//! [`crate::context::Context`]. The PDF output device drains that buffer at
//! end-of-job (`finish_with_context`) and writes the records into the PDF
//! catalog, info dictionary, outline tree, page annotation arrays, and so
//! on. Non-PDF output devices simply ignore the buffer, so `pdfmark` is a
//! no-op for PNG / viewer output.
//!
//! See `docs/PLAN-PDFMARK-AUTHORING.md` for the staged plan and
//! `docs/PDFMARK-REFERENCE.md` (TBD) for the public reference once the
//! plan reaches its rollup.

/// One accumulated `pdfmark` record. Each variant corresponds to a
/// type-tag the interpreter recognises (`/DOCINFO`, `/OUT`, `/ANN`, …).
/// Later phases add variants without disturbing this enum's external API
/// beyond the new variant itself.
///
/// Marked `#[non_exhaustive]`: cross-crate `match` sites need a
/// wildcard arm so future type-tags (Tagged PDF, etc.) land additively.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum PdfMarkRecord {
    /// `/DOCINFO` — entries to merge into the PDF Info dictionary.
    DocInfo(DocInfoRecord),
    /// `/OUT` — one bookmark entry, contributing to the document's
    /// outline tree.
    Outline(OutlineRecord),
    /// `/ANN` — one page annotation (link, sticky note, free-text, …).
    Annotation(AnnotationRecord),
    /// `/DEST` — one named destination contributing to /Names /Dests.
    Dest(DestRecord),
    /// `/PAGE` (single-page override) or `/PAGES` (document-wide
    /// default for keys that aren't already overridden on a specific
    /// page).
    PageOverride(PageOverrideRecord),
    /// `/VIEWERPREFERENCES` — catalog-level viewer preferences plus
    /// the `/PageLayout` and `/PageMode` overrides that live directly
    /// on `/Catalog` rather than nested under `/ViewerPreferences`.
    ViewerPrefs(ViewerPrefsRecord),
    /// `/Metadata` — XMP metadata stream attached to `/Catalog`.
    Metadata(MetadataRecord),
    /// `/FORM` — document-level AcroForm dict. Multiple records merge
    /// last-wins key-by-key; the `/Fields` array is implicit (built from
    /// `/Widget` annotations at write time).
    Form(FormRecord),
    /// `/EMBED` — one embedded file attachment. Multiple records
    /// accumulate; the writer assembles a `/Names /EmbeddedFiles`
    /// name tree and references it from `/Catalog`.
    Embed(EmbedRecord),
}

/// Buffered `pdfmark` records. Lives on `Context` for the entire job;
/// drained once by the PDF output device at end-of-job. The buffer is
/// document-global (not VM-level), so `save` / `restore` do **not** roll
/// it back — pdfmark records issued before a `restore` survive.
#[derive(Default, Clone, Debug)]
pub struct PdfMarkBuffer {
    records: Vec<PdfMarkRecord>,
    /// Count of completed `showpage` calls so far. The interpreter's
    /// `showpage` continuation increments this. Page-scoped records
    /// (annotations, page boxes) that omit an explicit `/Page` key
    /// default to `current_page + 1` — i.e. the page currently being
    /// assembled. So after N showpages, `current_page == N` and the
    /// page-being-assembled is `N + 1`.
    pub current_page: u32,
}

impl PdfMarkBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a record; ordering is preserved.
    pub fn push(&mut self, record: PdfMarkRecord) {
        self.records.push(record);
    }

    /// Read-only view of accumulated records.
    pub fn records(&self) -> &[PdfMarkRecord] {
        &self.records
    }

    /// Take ownership of the records, leaving the buffer empty. Used by
    /// the PDF output device once at end of job.
    pub fn drain(&mut self) -> Vec<PdfMarkRecord> {
        std::mem::take(&mut self.records)
    }

    /// True when no records have been pushed.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

/// `/DOCINFO` payload — `Option<String>` for every key so absent entries
/// don't overwrite values from another producer (or the device's
/// auto-generated defaults). `creation_date` and `mod_date` accept either
/// a parsed [`PdfDate`] or a passthrough string the writer emits verbatim.
#[derive(Clone, Debug, Default)]
pub struct DocInfoRecord {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
    pub creator: Option<String>,
    pub producer: Option<String>,
    pub creation_date: Option<DocDate>,
    pub mod_date: Option<DocDate>,
    /// Trapped: PDF spec requires /True, /False, or /Unknown.
    pub trapped: Option<TrappedState>,
}

/// `/Trapped` value as written to the Info dict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TrappedState {
    True,
    False,
    Unknown,
}

impl DocInfoRecord {
    /// Return the `/CreationDate` value formatted as a PDF date string,
    /// or `None` when no creation date is set.
    pub fn creation_date_string(&self) -> Option<String> {
        self.creation_date.as_ref().map(DocDate::to_pdf_string)
    }

    /// Return the `/ModDate` value formatted as a PDF date string, or
    /// `None` when no mod date is set.
    pub fn mod_date_string(&self) -> Option<String> {
        self.mod_date.as_ref().map(DocDate::to_pdf_string)
    }
}

impl DocDate {
    /// Render the date as a PDF date string. `Raw` round-trips the
    /// producer's bytes verbatim; `Parsed` reformats from the
    /// structural form.
    pub fn to_pdf_string(&self) -> String {
        match self {
            DocDate::Raw(s) => s.clone(),
            DocDate::Parsed(d) => {
                let mut out = format!(
                    "D:{:04}{:02}{:02}{:02}{:02}{:02}",
                    d.year, d.month, d.day, d.hour, d.minute, d.second
                );
                match d.tz_sign {
                    TzSign::Utc => out.push('Z'),
                    TzSign::East => out.push_str(&format!("+{:02}'{:02}'", d.tz_hour, d.tz_minute)),
                    TzSign::West => out.push_str(&format!("-{:02}'{:02}'", d.tz_hour, d.tz_minute)),
                    TzSign::Unknown => {}
                }
                out
            }
        }
    }
}

/// A document date entry. The writer can either round-trip a raw string
/// (already in PDF date syntax) or format a parsed [`PdfDate`].
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum DocDate {
    /// Raw string the producer issued — passed through verbatim. Used
    /// when the input is already in PDF date format and round-tripping
    /// the bytes preserves precision and timezone offset exactly.
    Raw(String),
    /// Parsed structural form. Reserved for future phases that
    /// normalise dates; Phase 1 stores everything as `Raw`.
    Parsed(PdfDate),
}

/// Parsed PDF date string of the form `D:YYYYMMDDHHmmSSOHH'mm'`, where
/// `O` is one of `+`, `-`, or `Z` for the offset sign. All fields after
/// the year are optional in the PDF spec; missing components default to
/// the values shown in [`PdfDate::default`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PdfDate {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub tz_sign: TzSign,
    pub tz_hour: u8,
    pub tz_minute: u8,
}

/// Sign of a PDF date timezone offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TzSign {
    /// `+` — east of UTC.
    East,
    /// `-` — west of UTC.
    West,
    /// `Z` — UTC.
    Utc,
    /// Offset omitted entirely — treat as local time per PDF spec.
    Unknown,
}

impl Default for PdfDate {
    fn default() -> Self {
        Self {
            year: 0,
            month: 1,
            day: 1,
            hour: 0,
            minute: 0,
            second: 0,
            tz_sign: TzSign::Unknown,
            tz_hour: 0,
            tz_minute: 0,
        }
    }
}

impl PdfDate {
    /// Parse a PDF date string. Accepts the canonical `D:YYYY[MMDDHHmmSS[O[HH'[mm']]]]`
    /// shape. The `D:` prefix is required; everything after the year is
    /// optional and missing fields use [`PdfDate::default`] values.
    /// Returns `None` on malformed input.
    pub fn parse(s: &str) -> Option<Self> {
        let body = s.strip_prefix("D:")?;
        let bytes = body.as_bytes();
        if bytes.len() < 4 || !bytes[..4].iter().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let year: u16 = std::str::from_utf8(&bytes[..4]).ok()?.parse().ok()?;
        let mut date = PdfDate {
            year,
            ..PdfDate::default()
        };
        let mut i = 4;

        let take_pair = |idx: &mut usize, max: u8| -> Option<u8> {
            if *idx + 2 > bytes.len() {
                return None;
            }
            let pair = std::str::from_utf8(&bytes[*idx..*idx + 2]).ok()?;
            if !pair.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let v: u8 = pair.parse().ok()?;
            if v > max {
                return None;
            }
            *idx += 2;
            Some(v)
        };

        if i + 2 <= bytes.len() && bytes[i].is_ascii_digit() {
            date.month = take_pair(&mut i, 12)?;
        }
        if i + 2 <= bytes.len() && bytes[i].is_ascii_digit() {
            date.day = take_pair(&mut i, 31)?;
        }
        if i + 2 <= bytes.len() && bytes[i].is_ascii_digit() {
            date.hour = take_pair(&mut i, 23)?;
        }
        if i + 2 <= bytes.len() && bytes[i].is_ascii_digit() {
            date.minute = take_pair(&mut i, 59)?;
        }
        if i + 2 <= bytes.len() && bytes[i].is_ascii_digit() {
            date.second = take_pair(&mut i, 59)?;
        }

        if i < bytes.len() {
            match bytes[i] {
                b'Z' => {
                    date.tz_sign = TzSign::Utc;
                }
                b'+' => {
                    date.tz_sign = TzSign::East;
                    i += 1;
                    date.tz_hour = take_pair(&mut i, 23)?;
                    if i < bytes.len() && bytes[i] == b'\'' {
                        i += 1;
                        if i + 2 <= bytes.len() && bytes[i].is_ascii_digit() {
                            date.tz_minute = take_pair(&mut i, 59)?;
                        }
                    }
                }
                b'-' => {
                    date.tz_sign = TzSign::West;
                    i += 1;
                    date.tz_hour = take_pair(&mut i, 23)?;
                    if i < bytes.len() && bytes[i] == b'\'' {
                        i += 1;
                        if i + 2 <= bytes.len() && bytes[i].is_ascii_digit() {
                            date.tz_minute = take_pair(&mut i, 59)?;
                        }
                    }
                }
                _ => return None,
            }
        }

        Some(date)
    }
}

// ----- Outlines (Phase 2) ---------------------------------------------------

/// One `/OUT pdfmark` entry. Each record contributes a bookmark node to
/// the document outline tree the PDF writer assembles at end-of-job.
#[derive(Clone, Debug)]
pub struct OutlineRecord {
    /// `/Title` — required user-visible label.
    pub title: String,
    /// `/Page`, `/Dest`, or `/Action` — what clicking the bookmark
    /// resolves to. `None` is allowed (bookmark is a non-navigable
    /// label).
    pub destination: Option<OutlineDestination>,
    /// `/Count` — Adobe nesting hint. Positive: this bookmark is
    /// expanded with `count` direct children that immediately follow.
    /// Negative: collapsed with `|count|` children. Zero / absent:
    /// leaf. `None` = absent.
    pub count: Option<i32>,
    /// `/OutlineLevel` — stet extension. Explicit nesting level
    /// (1-based; 1 is top-level). When at least one record uses this,
    /// the tree builder switches to level-based parenting and ignores
    /// `count`.
    pub outline_level: Option<u32>,
    /// `/Color` — RGB triple in `[0, 1]`, optional.
    pub color: Option<[f64; 3]>,
    /// `/F` — style flags: bit 0 = italic, bit 1 = bold (matches the
    /// PDF 1.4 outline `/F` field).
    pub flags: Option<u32>,
}

/// What a bookmark entry navigates to when clicked.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum OutlineDestination {
    /// `/Page N /View [...]` — explicit page + view spec.
    PageView { page: u32, view: ViewSpec },
    /// `/Dest /Name` — reference to a named destination registered
    /// elsewhere (in Phase 4 via `/DEST pdfmark`).
    NamedDest(String),
    /// `/Action <<...>>` — passthrough action dict. Phase 1 captures
    /// the URI subset; richer action types (GoTo, JavaScript, …) land
    /// as later phases need them.
    Action(OutlineAction),
}

/// Outline view spec, mirroring PDF's `/Dest` array shape.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum ViewSpec {
    /// `[/XYZ left top zoom]` — null components keep the current value.
    Xyz {
        left: Option<f64>,
        top: Option<f64>,
        zoom: Option<f64>,
    },
    /// `[/Fit]`.
    Fit,
    /// `[/FitH top]`.
    FitH(Option<f64>),
    /// `[/FitV left]`.
    FitV(Option<f64>),
    /// `[/FitR left bottom right top]`.
    FitR {
        left: f64,
        bottom: f64,
        right: f64,
        top: f64,
    },
    /// `[/FitB]`.
    FitB,
    /// `[/FitBH top]`.
    FitBH(Option<f64>),
    /// `[/FitBV left]`.
    FitBV(Option<f64>),
}

impl Default for ViewSpec {
    fn default() -> Self {
        ViewSpec::Xyz {
            left: None,
            top: None,
            zoom: None,
        }
    }
}

/// Outline-action passthrough. Despite the name, this enum is shared
/// across every place an "action dict" appears — outline `/Action`,
/// link annotation `/A`, page `/AA` open / close — because the on-the-
/// wire shape is identical.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum OutlineAction {
    /// `<< /S /URI /URI (string) >>`.
    Uri(String),
    /// `<< /S /GoTo /D <name-or-array> >>` — the destination is either
    /// a named destination (`Named`) or an explicit page+view
    /// (`Explicit`).
    GoTo(GoToTarget),
    /// `<< /S /JavaScript /JS (string) >>` — pass-through. stet does
    /// **not** execute JavaScript; the bytes are round-tripped verbatim
    /// so a downstream viewer (Acrobat, Foxit) that does run JS can
    /// pick them up.
    JavaScript(String),
    /// `<< /S /Named /N /<name> >>` — a built-in viewer command
    /// (e.g. `/NextPage`, `/PrevPage`, `/FirstPage`, `/LastPage`,
    /// `/Print`, `/Find`, …). The producer-supplied name is round-
    /// tripped verbatim; viewers that don't recognise it ignore the
    /// action.
    Named(String),
}

/// `/GoTo` action target.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum GoToTarget {
    /// `/D /SomeName` — resolved against the document's name tree.
    Named(String),
    /// `/D [N /Fit]` — explicit 1-based page + view spec.
    Explicit { page: u32, view: ViewSpec },
}

/// One node in the assembled outline tree.
#[derive(Clone, Debug)]
pub struct OutlineNode {
    pub record: OutlineRecord,
    pub children: Vec<OutlineNode>,
}

/// Build an outline tree from a flat sequence of [`OutlineRecord`]s.
///
/// Two authoring conventions are supported and detected automatically:
///
/// 1. **Level-based** (stet extension): when *any* record carries
///    `outline_level`, the builder uses those levels exclusively and
///    ignores `count`. Each level-1 record opens a new top-level
///    branch; deeper records become descendants of the most recent
///    record at the level immediately above them.
/// 2. **Count-based** (Adobe convention): the default. Each record
///    declares how many direct children follow it via `count`
///    (positive = expanded, negative = collapsed; sign affects display
///    but not topology). Records with `count.is_none()` or `count == 0`
///    are leaves.
///
/// Mixed input — some records use `outline_level`, others use
/// `count` — falls into the level-based path; `count` on
/// level-tagged records is preserved on each node so the writer can
/// still emit Adobe-style `/Count` initial-display hints.
pub fn build_outline_tree(records: &[OutlineRecord]) -> Vec<OutlineNode> {
    if records.is_empty() {
        return Vec::new();
    }
    let any_level = records.iter().any(|r| r.outline_level.is_some());
    if any_level {
        build_level_based(records)
    } else {
        build_count_based(records)
    }
}

fn build_count_based(records: &[OutlineRecord]) -> Vec<OutlineNode> {
    let mut idx = 0;
    let mut roots = Vec::new();
    while idx < records.len() {
        roots.push(consume_count_node(records, &mut idx));
    }
    roots
}

fn consume_count_node(records: &[OutlineRecord], idx: &mut usize) -> OutlineNode {
    let record = records[*idx].clone();
    let child_count = record.count.unwrap_or(0).unsigned_abs() as usize;
    *idx += 1;
    let mut children = Vec::with_capacity(child_count);
    for _ in 0..child_count {
        if *idx >= records.len() {
            break;
        }
        children.push(consume_count_node(records, idx));
    }
    OutlineNode { record, children }
}

fn build_level_based(records: &[OutlineRecord]) -> Vec<OutlineNode> {
    // `stack[i]` is the in-progress sibling list at depth `i + 1`. When
    // a record at depth d arrives we close out everything deeper than
    // d-1 (folding child lists into their parents) before pushing.
    let mut roots: Vec<OutlineNode> = Vec::new();
    let mut stack: Vec<Vec<OutlineNode>> = Vec::new();
    let mut depths: Vec<u32> = Vec::new();

    for record in records {
        let mut depth = record.outline_level.unwrap_or(1).max(1);
        // Disallow gaps: clamp the requested depth to one deeper than
        // the deepest currently open node, falling back to 1 when the
        // stack is empty.
        let max_allowed = depths.last().copied().unwrap_or(0) + 1;
        if depth > max_allowed {
            depth = max_allowed;
        }
        // Fold every open level deeper than (or equal to) `depth` back
        // into its parent so the new record can sit at `depth`.
        while let Some(&top_depth) = depths.last() {
            if top_depth < depth {
                break;
            }
            let folded = stack.pop().unwrap_or_default();
            depths.pop();
            attach_children(&mut roots, &mut stack, folded);
        }
        let node = OutlineNode {
            record: record.clone(),
            children: Vec::new(),
        };
        if depth == 1 {
            roots.push(node);
            stack.push(Vec::new());
            depths.push(1);
        } else {
            stack.last_mut().unwrap().push(node);
            stack.push(Vec::new());
            depths.push(depth);
        }
    }
    while let Some(level_children) = stack.pop() {
        depths.pop();
        attach_children(&mut roots, &mut stack, level_children);
    }
    roots
}

fn attach_children(
    roots: &mut [OutlineNode],
    stack: &mut [Vec<OutlineNode>],
    children: Vec<OutlineNode>,
) {
    if children.is_empty() {
        return;
    }
    let parent = match stack.last_mut() {
        Some(siblings) => siblings.last_mut(),
        None => roots.last_mut(),
    };
    if let Some(p) = parent {
        p.children = children;
    }
}

// ----- Annotations (Phase 3) ------------------------------------------------

/// One `/ANN pdfmark` entry. Each record contributes a single
/// annotation (`/Annot`) to one page's `/Annots` array. `page` is
/// 1-based; `0` is reserved for "no explicit page" (the writer
/// substitutes the page being assembled at the time the pdfmark fired).
#[derive(Clone, Debug)]
pub struct AnnotationRecord {
    /// Page the annotation lives on (1-based). Set by the operator: an
    /// explicit `/Page` (or `/SrcPg` alias) wins; otherwise the writer
    /// falls back to `current_page + 1` from
    /// [`PdfMarkBuffer::current_page`].
    pub page: u32,
    /// `/Rect [llx lly urx ury]` — default user-space bounds. Required
    /// per PDF spec; defaulted to the empty rect on malformed input so
    /// the annotation at least has *somewhere* to land.
    pub rect: [f64; 4],
    /// Optional `/Color` triple in `[0, 1]` (PDF /C entry).
    pub color: Option<[f64; 3]>,
    /// Optional border specification. Translates to /Border on output.
    pub border: Option<Border>,
    /// Optional `/Title` (annotator name) — meaningful for `/Text` and
    /// `/FreeText`.
    pub title: Option<String>,
    /// Optional `/Contents` — meaningful for `/Text` and `/FreeText`;
    /// also accepted as a tooltip on `/Link`.
    pub contents: Option<String>,
    /// Subtype-specific payload.
    pub subtype: AnnotationSubtype,
}

/// Per-subtype annotation payload. Each variant carries the keys
/// specific to that subtype; shared keys (rect, color, border, title,
/// contents, page) live on the parent [`AnnotationRecord`].
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum AnnotationSubtype {
    /// `/Subtype /Link` — clickable region. Target is an action,
    /// explicit page+view, or a named destination; exactly one of the
    /// three is expected.
    Link {
        target: Option<AnnotationTarget>,
        /// `/H` highlight mode: `/N` (none), `/I` (invert), `/O`
        /// (outline), `/P` (push). Optional.
        highlight: Option<LinkHighlight>,
    },
    /// `/Subtype /Text` — sticky-note annotation.
    Text {
        /// `/Open` (boolean, default false).
        open: bool,
        /// `/Name` icon — /Comment, /Note (default), /Key, /Help,
        /// /NewParagraph, /Paragraph, /Insert.
        icon: TextAnnotationIcon,
    },
    /// `/Subtype /FreeText` — free-floating text annotation rendered
    /// directly on the page.
    FreeText {
        /// `/DA` default appearance string. Optional but most viewers
        /// require it to render anything; if absent, stet emits a sane
        /// default (`0 0 0 rg /Helv 10 Tf`).
        default_appearance: Option<String>,
        /// `/Q` quadding: 0=left, 1=center, 2=right. Optional.
        quadding: Option<u32>,
    },
    /// `/Subtype /Widget` — interactive form field. Author-only: stet
    /// doesn't render or run forms interactively, but it emits the
    /// PDF AcroForm structure so downstream viewers (Acrobat, Okular,
    /// pdf.js) can. The widget annotation and its leaf field dict are
    /// merged into a single PDF object — common when a field has
    /// exactly one widget — and the field-tree builder in
    /// `crates/stet-pdf/src/form_fields.rs` handles the multi-widget
    /// (radio group) and dotted-name parent cases.
    Widget(WidgetAnnotation),
}

/// `/Widget` annotation payload — also acts as the field dict when the
/// widget is a single-leaf field (the common case). Multiple widgets
/// sharing the same dotted [`field_name`](Self::field_name) become
/// `/Kids` of an implicit parent field at write time (radio groups).
#[derive(Clone, Debug, Default)]
pub struct WidgetAnnotation {
    /// `/T` — fully qualified field name. Dot-separated segments imply
    /// nesting (`order.shipping.street` → parents `order` →
    /// `order.shipping` and a leaf `street`). The PDF emitter renders
    /// only the last segment as `/T`; PDF resolves the full name by
    /// walking the `/Parent` chain.
    pub field_name: String,
    /// `/FT` field type. Optional — when absent the field inherits its
    /// type from the parent. Required on root fields.
    pub field_type: Option<FieldType>,
    /// `/V` field value — variant shape depends on `/FT`. Optional.
    pub value: Option<FieldValue>,
    /// `/DV` default value — same shape rules as `value`.
    pub default_value: Option<FieldValue>,
    /// `/Ff` field flags (PDF 1.7 spec § 12.7.3.1). Bit semantics
    /// vary by `/FT`; passed through verbatim.
    pub flags: Option<i32>,
    /// `/MaxLen` — text-field-only character limit.
    pub max_len: Option<i32>,
    /// `/Opt` — choice-field options. Each entry is either a single
    /// display string (export = display) or `[export display]` pair.
    pub options: Option<Vec<ChoiceOption>>,
    /// `/Q` quadding: 0=left, 1=center, 2=right.
    pub quadding: Option<i32>,
    /// `/DA` default appearance string. Falls back to the form-level
    /// `/DA` (or `0 0 0 rg /Helv 10 Tf` when neither is set) at write
    /// time.
    pub default_appearance: Option<String>,
}

/// Field type per PDF 1.7 spec § 12.7.4. The variant maps directly to
/// the `/FT` name in the output PDF.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FieldType {
    /// `/Btn` — pushbuttons, checkboxes, radio buttons.
    Btn,
    /// `/Tx` — text fields.
    Tx,
    /// `/Ch` — choice fields (combo boxes, list boxes).
    Ch,
    /// `/Sig` — signature fields.
    Sig,
}

/// Field value — variant shape depends on the field's `/FT`. The
/// emitter writes the corresponding PDF object kind for each variant.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum FieldValue {
    /// Text string — used for `/Tx` and single-select `/Ch` fields.
    Text(String),
    /// Name — used for `/Btn` checkboxes (`/Yes` / `/Off`) and radio
    /// groups (the chosen kid's appearance state).
    Name(String),
    /// Array of text strings — used for multi-select `/Ch` fields.
    TextArray(Vec<String>),
}

/// One `/Opt` entry on a choice field. PDF allows two shapes: a single
/// string (export = display) or `[export display]` for distinct values.
#[derive(Clone, Debug)]
pub struct ChoiceOption {
    /// Internal value persisted in the PDF when this option is selected.
    pub export: String,
    /// Human-readable label shown to the user. Equal to `export` when
    /// the producer used the single-string form.
    pub display: String,
}

/// `/Link` highlight mode — controls the visual feedback when the
/// user activates the link region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LinkHighlight {
    None,
    Invert,
    Outline,
    Push,
}

/// Standard `/Text` annotation icon names. Anything outside this set
/// falls back to `/Note`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TextAnnotationIcon {
    Comment,
    Note,
    Key,
    Help,
    NewParagraph,
    Paragraph,
    Insert,
}

impl Default for TextAnnotationIcon {
    fn default() -> Self {
        TextAnnotationIcon::Note
    }
}

/// `/Border` array `[Hradius Vradius Width]`. PDF spec also allows a
/// dash pattern fourth entry; we capture it but only emit when present.
#[derive(Clone, Debug, Default)]
pub struct Border {
    pub h_radius: f64,
    pub v_radius: f64,
    pub width: f64,
    pub dash: Option<Vec<f64>>,
}

/// What an annotation activates. Mirrors [`OutlineDestination`] but
/// kept distinct because annotations can carry richer action data
/// (e.g. JavaScript) and have their own resolution rules.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum AnnotationTarget {
    /// Explicit `/Page N /View [...]`. `page` is 1-based.
    PageView { page: u32, view: ViewSpec },
    /// `/Dest /Name` — named destination resolved against the
    /// document's name tree.
    NamedDest(String),
    /// `/Action <<...>>` passthrough.
    Action(OutlineAction),
}

// ----- Named destinations (Phase 4) ----------------------------------------

/// One `/DEST pdfmark` entry — registers a named destination in the
/// document's `/Names /Dests` name tree. PDF outline entries and link
/// annotations resolve the matching `name` against this tree.
#[derive(Clone, Debug)]
pub struct DestRecord {
    /// `/Dest` — the destination name (interned bytes; UTF-8 lossy).
    pub name: String,
    /// `/Page` — 1-based target page.
    pub page: u32,
    /// `/View` — view spec; default `[/XYZ null null null]`.
    pub view: ViewSpec,
}

// ----- Page boxes & page overrides (Phase 4) -------------------------------

/// One `/PAGE` (single-page override) or `/PAGES` (document-wide
/// default) pdfmark entry. The writer applies the keys to the
/// per-page dict at build time; `/PAGE` wins over `/PAGES` for
/// any key that's set on both, and an explicit `/PAGE` for page N
/// wins over the implicit "current page" target.
#[derive(Clone, Debug)]
pub struct PageOverrideRecord {
    /// Scope of the override.
    pub scope: PageOverrideScope,
    /// `/CropBox`, `/BleedBox`, `/TrimBox`, `/ArtBox` rectangles in
    /// default user space — `[llx, lly, urx, ury]`.
    pub boxes: PageBoxes,
    /// `/Rotate` — 0, 90, 180, or 270. Other values land here as-is
    /// and are dropped at write time.
    pub rotate: Option<i32>,
    /// `/AA` — additional-actions dict. Page-open (`/O`) fires when
    /// the page becomes visible; page-close (`/C`) fires when the
    /// user navigates away.
    pub additional_actions: Option<PageAdditionalActions>,
}

/// Page-level `/AA` (additional actions) — open and close hooks the
/// PDF viewer fires when a page becomes / leaves visible. Either
/// hook is optional; both are passed through verbatim from the
/// producer's action dict.
#[derive(Clone, Debug, Default)]
pub struct PageAdditionalActions {
    /// `/O` — fired when the page becomes visible.
    pub on_open: Option<OutlineAction>,
    /// `/C` — fired when the page leaves visibility.
    pub on_close: Option<OutlineAction>,
}

impl PageAdditionalActions {
    pub fn is_empty(&self) -> bool {
        self.on_open.is_none() && self.on_close.is_none()
    }

    /// Merge `other` under `self` — `self`'s `Some` actions win.
    pub fn merge_over(&self, other: &PageAdditionalActions) -> PageAdditionalActions {
        PageAdditionalActions {
            on_open: self.on_open.clone().or_else(|| other.on_open.clone()),
            on_close: self.on_close.clone().or_else(|| other.on_close.clone()),
        }
    }
}

/// One `/EMBED pdfmark` entry — a single attached file. The writer
/// emits one `/Filespec` dict + one `/EmbeddedFile` stream per record
/// and assembles them into a `/Names /EmbeddedFiles` name tree.
#[derive(Clone, Debug)]
pub struct EmbedRecord {
    /// `/FS` — file specification string (typically the original
    /// filename). Required.
    pub filename: String,
    /// `/DataSource` — raw file contents. Required. PostScript
    /// strings can hold arbitrary bytes, so binary attachments
    /// (PNGs, ZIPs, …) round-trip without re-encoding.
    pub data: Vec<u8>,
    /// `/UF` — unicode filename. PDF spec recommends both `/F` and
    /// `/UF`; when absent, the writer reuses `filename`.
    pub unicode_filename: Option<String>,
    /// `/Desc` — human-readable description.
    pub description: Option<String>,
    /// `/AFRelationship` — relationship of this attachment to the
    /// document content. Allow-list: `Source`, `Data`, `Alternative`,
    /// `Supplement`, `EncryptedPayload`, `Unspecified`.
    pub af_relationship: Option<String>,
    /// `/MIMEType` (PDF 1.7) — MIME type of the attached file.
    /// Optional; viewers that respect it use it to pick the right
    /// "open with" handler.
    pub mime_type: Option<String>,
}

/// Whether a [`PageOverrideRecord`] targets one specific page or the
/// whole document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PageOverrideScope {
    /// `/PAGE` — single-page override. `1`-based page index.
    Single(u32),
    /// `/PAGES` — document-wide defaults applied to every page that
    /// doesn't have an explicit `/PAGE` value for that same key.
    All,
}

/// Per-page box rectangles. Each entry is `Option<[llx, lly, urx, ury]>`;
/// `None` means "leave the device default in place".
#[derive(Clone, Copy, Debug, Default)]
pub struct PageBoxes {
    pub crop_box: Option<[f64; 4]>,
    pub bleed_box: Option<[f64; 4]>,
    pub trim_box: Option<[f64; 4]>,
    pub art_box: Option<[f64; 4]>,
}

// ----- Viewer prefs + metadata (Phase 5) -----------------------------------

/// One `/VIEWERPREFERENCES pdfmark` payload. All keys are optional;
/// later records override earlier ones key-by-key. The "page layout"
/// and "page mode" entries technically live on `/Catalog` directly
/// (not under `/ViewerPreferences`) but Adobe pdfmark groups them with
/// the rest of the viewer-control bag, so stet does too.
#[derive(Clone, Debug, Default)]
pub struct ViewerPrefsRecord {
    pub hide_toolbar: Option<bool>,
    pub hide_menubar: Option<bool>,
    pub hide_window_ui: Option<bool>,
    pub fit_window: Option<bool>,
    pub center_window: Option<bool>,
    pub display_doc_title: Option<bool>,
    /// `/NonFullScreenPageMode` — one of `UseNone`, `UseOutlines`,
    /// `UseThumbs`, `UseOC`. Stored as the raw bytes for forward-
    /// compatibility with values stet doesn't recognise.
    pub non_full_screen_page_mode: Option<String>,
    /// `/Direction` — `L2R` or `R2L`.
    pub direction: Option<String>,
    /// Catalog-level `/PageLayout`: `SinglePage`, `OneColumn`,
    /// `TwoColumnLeft`, `TwoColumnRight`, `TwoPageLeft`, `TwoPageRight`.
    pub page_layout: Option<String>,
    /// Catalog-level `/PageMode`: `UseNone`, `UseOutlines`,
    /// `UseThumbs`, `FullScreen`, `UseOC`, `UseAttachments`. Wins over
    /// the `UseOutlines` default the writer applies when `/OUT`
    /// records exist.
    pub page_mode: Option<String>,
}

impl ViewerPrefsRecord {
    /// Merge `other` into `self` — `self`'s `Some` values win when both
    /// records set the same key. Used to layer multiple
    /// `/VIEWERPREFERENCES pdfmark` blocks into one effective record.
    pub fn merge_over(&self, other: &ViewerPrefsRecord) -> ViewerPrefsRecord {
        ViewerPrefsRecord {
            hide_toolbar: self.hide_toolbar.or(other.hide_toolbar),
            hide_menubar: self.hide_menubar.or(other.hide_menubar),
            hide_window_ui: self.hide_window_ui.or(other.hide_window_ui),
            fit_window: self.fit_window.or(other.fit_window),
            center_window: self.center_window.or(other.center_window),
            display_doc_title: self.display_doc_title.or(other.display_doc_title),
            non_full_screen_page_mode: self
                .non_full_screen_page_mode
                .clone()
                .or_else(|| other.non_full_screen_page_mode.clone()),
            direction: self.direction.clone().or_else(|| other.direction.clone()),
            page_layout: self
                .page_layout
                .clone()
                .or_else(|| other.page_layout.clone()),
            page_mode: self.page_mode.clone().or_else(|| other.page_mode.clone()),
        }
    }

    /// True when no field has a value — the writer skips the catalog
    /// entry entirely in this case.
    pub fn nested_is_empty(&self) -> bool {
        self.hide_toolbar.is_none()
            && self.hide_menubar.is_none()
            && self.hide_window_ui.is_none()
            && self.fit_window.is_none()
            && self.center_window.is_none()
            && self.display_doc_title.is_none()
            && self.non_full_screen_page_mode.is_none()
            && self.direction.is_none()
    }
}

/// One `/Metadata pdfmark` entry — an XMP stream attached to the
/// document's `/Catalog`. The writer wraps the bytes in a
/// `/Type /Metadata /Subtype /XML` stream object.
#[derive(Clone, Debug)]
pub struct MetadataRecord {
    /// Raw XMP XML bytes — round-tripped verbatim.
    pub xmp_bytes: Vec<u8>,
}

// ----- AcroForm (Phase 6) --------------------------------------------------

/// `/FORM` payload — document-level AcroForm dict. All fields are
/// optional; `/Fields` is implicit (built from `/Widget` annotations at
/// write time). Multiple `/FORM` records merge last-wins via
/// [`FormRecord::merge_over`].
#[derive(Clone, Debug, Default)]
pub struct FormRecord {
    /// `/NeedAppearances` — when true, the viewer regenerates appearance
    /// streams on open. stet defaults to `true` at write time when the
    /// producer doesn't set this; that lets viewers (Acrobat, Okular,
    /// pdf.js) draw form fields without us authoring appearance streams.
    pub need_appearances: Option<bool>,
    /// `/SigFlags` — signature flags. Bit 0: SignaturesExist. Bit 1:
    /// AppendOnly. Pass-through; stet doesn't synthesise signatures.
    pub sig_flags: Option<i32>,
    /// `/CO` — calculate-order array of fully-qualified field names.
    /// Used when calc-script-driven fields depend on each other.
    pub calc_order: Option<Vec<String>>,
    /// `/DA` — document-level default appearance string for fields that
    /// don't set their own.
    pub default_appearance: Option<String>,
    /// `/Q` — document-level quadding default.
    pub quadding: Option<i32>,
}

impl FormRecord {
    /// Merge `self` over `other` — `self`'s `Some` fields win.
    pub fn merge_over(&self, other: &FormRecord) -> FormRecord {
        FormRecord {
            need_appearances: self.need_appearances.or(other.need_appearances),
            sig_flags: self.sig_flags.or(other.sig_flags),
            calc_order: self.calc_order.clone().or_else(|| other.calc_order.clone()),
            default_appearance: self
                .default_appearance
                .clone()
                .or_else(|| other.default_appearance.clone()),
            quadding: self.quadding.or(other.quadding),
        }
    }
}

impl PageBoxes {
    /// Merge `other` into `self` — `self`'s entries win when both are
    /// `Some`. Used by the writer to layer per-page `/PAGE` over
    /// document-wide `/PAGES` defaults.
    pub fn merge_over(&self, other: &PageBoxes) -> PageBoxes {
        PageBoxes {
            crop_box: self.crop_box.or(other.crop_box),
            bleed_box: self.bleed_box.or(other.bleed_box),
            trim_box: self.trim_box.or(other.trim_box),
            art_box: self.art_box.or(other.art_box),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.crop_box.is_none()
            && self.bleed_box.is_none()
            && self.trim_box.is_none()
            && self.art_box.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_parse_full() {
        let d = PdfDate::parse("D:20261231120000-05'00'").unwrap();
        assert_eq!(d.year, 2026);
        assert_eq!(d.month, 12);
        assert_eq!(d.day, 31);
        assert_eq!(d.hour, 12);
        assert_eq!(d.tz_sign, TzSign::West);
        assert_eq!(d.tz_hour, 5);
    }

    #[test]
    fn date_parse_utc() {
        let d = PdfDate::parse("D:20260101000000Z").unwrap();
        assert_eq!(d.tz_sign, TzSign::Utc);
        assert_eq!(d.year, 2026);
    }

    #[test]
    fn date_parse_year_only() {
        let d = PdfDate::parse("D:2026").unwrap();
        assert_eq!(d.year, 2026);
        assert_eq!(d.month, 1);
        assert_eq!(d.day, 1);
    }

    #[test]
    fn date_parse_no_prefix() {
        assert!(PdfDate::parse("20260101").is_none());
    }

    #[test]
    fn date_parse_garbage() {
        assert!(PdfDate::parse("D:abcd").is_none());
    }

    #[test]
    fn buffer_round_trip() {
        let mut buf = PdfMarkBuffer::new();
        assert!(buf.is_empty());
        buf.push(PdfMarkRecord::DocInfo(DocInfoRecord {
            title: Some("Hello".into()),
            ..DocInfoRecord::default()
        }));
        assert_eq!(buf.records().len(), 1);
        let drained = buf.drain();
        assert_eq!(drained.len(), 1);
        assert!(buf.is_empty());
    }

    fn outline(title: &str, count: Option<i32>, level: Option<u32>) -> OutlineRecord {
        OutlineRecord {
            title: title.into(),
            destination: None,
            count,
            outline_level: level,
            color: None,
            flags: None,
        }
    }

    #[test]
    fn outline_empty_input() {
        let tree = build_outline_tree(&[]);
        assert!(tree.is_empty());
    }

    #[test]
    fn outline_count_based_three_with_two_kids_each() {
        // Adobe convention: each parent declares a /Count of 2, then
        // its two children follow immediately.
        let records = vec![
            outline("A", Some(2), None),
            outline("A.1", None, None),
            outline("A.2", None, None),
            outline("B", Some(2), None),
            outline("B.1", None, None),
            outline("B.2", None, None),
            outline("C", Some(2), None),
            outline("C.1", None, None),
            outline("C.2", None, None),
        ];
        let tree = build_outline_tree(&records);
        assert_eq!(tree.len(), 3);
        for (i, root) in tree.iter().enumerate() {
            assert_eq!(root.children.len(), 2, "root {i} should have 2 kids");
        }
        assert_eq!(tree[0].record.title, "A");
        assert_eq!(tree[0].children[0].record.title, "A.1");
        assert_eq!(tree[2].children[1].record.title, "C.2");
    }

    #[test]
    fn outline_count_based_collapsed_negative() {
        // Negative count = collapsed but topology is the same as
        // positive: still 2 direct children.
        let records = vec![
            outline("A", Some(-2), None),
            outline("A.1", None, None),
            outline("A.2", None, None),
        ];
        let tree = build_outline_tree(&records);
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].children.len(), 2);
    }

    #[test]
    fn outline_count_based_nested_grandchildren() {
        // A has 1 child A.1 which itself declares 2 grandchildren.
        let records = vec![
            outline("A", Some(1), None),
            outline("A.1", Some(2), None),
            outline("A.1.1", None, None),
            outline("A.1.2", None, None),
        ];
        let tree = build_outline_tree(&records);
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].children.len(), 1);
        assert_eq!(tree[0].children[0].children.len(), 2);
    }

    #[test]
    fn outline_level_based_1_2_2_1_2_3_3_1() {
        // Sequence levels 1,2,2,1,2,3,3,1 → three top-level items, the
        // first with 2 kids, second with 1 kid (which itself has 2),
        // third a leaf.
        let records = vec![
            outline("A", None, Some(1)),
            outline("A.1", None, Some(2)),
            outline("A.2", None, Some(2)),
            outline("B", None, Some(1)),
            outline("B.1", None, Some(2)),
            outline("B.1.1", None, Some(3)),
            outline("B.1.2", None, Some(3)),
            outline("C", None, Some(1)),
        ];
        let tree = build_outline_tree(&records);
        assert_eq!(tree.len(), 3);
        assert_eq!(tree[0].record.title, "A");
        assert_eq!(tree[0].children.len(), 2);
        assert_eq!(tree[1].record.title, "B");
        assert_eq!(tree[1].children.len(), 1);
        assert_eq!(tree[1].children[0].children.len(), 2);
        assert_eq!(tree[1].children[0].children[1].record.title, "B.1.2");
        assert_eq!(tree[2].record.title, "C");
        assert!(tree[2].children.is_empty());
    }

    #[test]
    fn outline_level_skip_clamps_to_next_depth() {
        // A jump from level 1 directly to level 5 is clamped to
        // level 2 (one deeper than the open root). This stops malformed
        // input from producing dangling phantom nodes.
        let records = vec![
            outline("Root", None, Some(1)),
            outline("Child", None, Some(5)),
        ];
        let tree = build_outline_tree(&records);
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].children.len(), 1);
        assert_eq!(tree[0].children[0].record.title, "Child");
    }

    #[test]
    fn outline_mixed_input_uses_level_path() {
        // Any /OutlineLevel entry switches the whole batch to the
        // level-based builder. The leading count-only record without
        // a level falls into the default depth = 1.
        let records = vec![
            outline("Bare", Some(2), None),
            outline("Tagged-1", None, Some(1)),
            outline("Tagged-2", None, Some(2)),
        ];
        let tree = build_outline_tree(&records);
        assert_eq!(tree.len(), 2);
        assert!(tree[0].children.is_empty());
        assert_eq!(tree[1].children.len(), 1);
    }
}
