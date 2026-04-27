// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF parser, page navigator, and content stream interpreter.
//!
//! `stet-pdf-reader` is a self-contained PDF reader: it opens a PDF, walks
//! its object graph, and interprets each page's content stream into a
//! `stet_graphics::display_list::DisplayList` that any downstream consumer
//! (rasterizer, PDF writer, custom output device) can render.
//!
//! The crate intentionally has **no dependency on `stet-core`** — it uses
//! only `stet-fonts` (font parsing) and `stet-graphics` (display list and
//! ICC types), so it can be used as a standalone PDF parser/renderer
//! without pulling in the PostScript interpreter.
//!
//! # Quick start
//!
//! ```no_run
//! use stet_pdf_reader::PdfDocument;
//!
//! let data = std::fs::read("document.pdf")?;
//! let doc = PdfDocument::from_bytes(&data)?;
//!
//! for page in 0..doc.page_count() {
//!     let display_list = doc.render_page(page, 150.0)?;
//!     // …consume the display list (rasterize, convert, inspect, etc.)
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! With the default `render` feature enabled, [`PdfDocument::render_page_to_rgba`]
//! skips the display-list-handling boilerplate and produces RGBA pixels
//! directly via `stet-render`.
//!
//! # Encrypted PDFs
//!
//! `from_bytes` / `from_bytes_with_icc` try the empty password. If the
//! file uses a non-empty user password they return
//! [`PdfError::PasswordRequired`]; the caller can then prompt the user
//! and retry with [`PdfDocument::from_bytes_with_password`]:
//!
//! ```no_run
//! use stet_pdf_reader::{PdfDocument, PdfError};
//! use stet_graphics::icc::IccCache;
//!
//! let data = std::fs::read("encrypted.pdf")?;
//! let doc = match PdfDocument::from_bytes(&data) {
//!     Ok(doc) => doc,
//!     Err(PdfError::PasswordRequired) => {
//!         let pw = prompt_user_for_password();
//!         PdfDocument::from_bytes_with_password(&data, IccCache::new(), pw.as_bytes())?
//!     }
//!     Err(e) => return Err(e.into()),
//! };
//! # fn prompt_user_for_password() -> String { String::new() }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! RC4 (40/128-bit), AES-128, and AES-256 (R=5/6) are all supported.
//!
//! # Structural API
//!
//! In addition to rendering, [`PdfDocument`] exposes typed, read-only
//! access to a document's structural content — for indexers,
//! accessibility tools, link extractors, format converters, and other
//! consumers that want to *inspect* a PDF rather than display it.
//!
//! Every accessor parses lazily on first call and caches its result;
//! a document the caller only renders pays nothing for the structural
//! API surface.
//!
//! ```no_run
//! use stet_pdf_reader::PdfDocument;
//!
//! let data = std::fs::read("document.pdf")?;
//! let doc = PdfDocument::from_bytes(&data)?;
//!
//! // Document metadata (Info dict + XMP).
//! let m = doc.metadata();
//! println!("Title:    {:?}", m.title);
//! println!("Author:   {:?}", m.author);
//! println!("Producer: {:?}", m.producer);
//!
//! // Outline / bookmarks.
//! for item in doc.outline() {
//!     println!("- {} ({} children)", item.title, item.children.len());
//! }
//!
//! // Annotations on page 1.
//! for annot in doc.page_annotations(0)? {
//!     println!("{:?} at {:?}", annot.kind, annot.rect);
//! }
//!
//! // AcroForm field tree.
//! if let Some(form) = doc.form() {
//!     for field in &form.fields {
//!         println!("{}: {:?}", field.name, field.value);
//!     }
//! }
//!
//! // Embedded file attachments.
//! for (name, file) in doc.embedded_files() {
//!     let bytes = doc.embedded_file_bytes(name)?;
//!     println!("{name} ({} bytes, {:?})", bytes.len(), file.mime_type);
//! }
//!
//! // Recoverable parse problems (cycles, dropped entries, etc.).
//! for w in doc.parse_warnings().iter() {
//!     eprintln!("[{:?}] {:?}: {}", w.severity, w.phase, w.message);
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Full accessor list, each cached after first call:
//!
//! - [`metadata`](PdfDocument::metadata) — Info dict + XMP
//! - [`viewer_preferences`](PdfDocument::viewer_preferences) — display hints
//! - [`outline`](PdfDocument::outline) — bookmark tree
//! - [`destinations`](PdfDocument::destinations) +
//!   [`resolve_named_destination`](PdfDocument::resolve_named_destination) —
//!   named-destination table
//! - [`page_annotations`](PdfDocument::page_annotations) — per-page typed annotations
//! - [`form`](PdfDocument::form) — AcroForm field tree
//! - [`page_boxes`](PdfDocument::page_boxes) — all 5 page boxes + presentation hints
//! - [`embedded_files`](PdfDocument::embedded_files) +
//!   [`embedded_file_bytes`](PdfDocument::embedded_file_bytes) — file attachments
//! - [`layers`](PdfDocument::layers) + [`layer`](PdfDocument::layer) —
//!   Optional Content Group (layer) metadata
//! - [`parse_warnings`](PdfDocument::parse_warnings) — diagnostics
//!
//! Walkers that recurse over potentially-cyclic structures
//! (outline tree, name trees, form-field tree) bound traversal with a
//! visited-set and a depth cap; truncations are surfaced via
//! [`parse_warnings`](PdfDocument::parse_warnings) so a missing branch
//! is never silent.
//!
//! For a longer-form reference with one focused example per accessor,
//! see the [PDF Reader API
//! guide](https://github.com/AndyCappDev/stet/blob/main/docs/PDF-READER-API.md)
//! in the repository.
//!
//! # Acknowledgements
//!
//! JPEG 2000, JBIG2, and CCITT-Fax stream decoding use the
//! [`hayro-jpeg2000`](https://crates.io/crates/hayro-jpeg2000),
//! [`hayro-jbig2`](https://crates.io/crates/hayro-jbig2), and
//! [`hayro-ccitt`](https://crates.io/crates/hayro-ccitt) crates from the
//! [hayro](https://github.com/LaurenzV/hayro) PDF renderer by Laurenz
//! Stampfl. Big thanks to the hayro project for factoring those decoders
//! out as reusable crates — `stet-pdf-reader` would not cover the full
//! PDF stream-filter surface without them.

pub mod annotations;
pub mod content;
pub mod crypto;
pub mod destination;
pub mod diagnostics;
pub mod embedded_files;
pub mod error;
pub mod filters;
pub mod form_fields;
pub mod layers;
pub mod lexer;
pub mod metadata;
pub mod name_tree;
pub mod objects;
pub mod outline;
pub mod page_boxes;
pub mod page_tree;
pub mod resolver;
pub mod resources;
pub mod viewer_prefs;
pub mod xref;

pub use annotations::{
    Annotation, AnnotationColor, AnnotationDate, AnnotationFlags, AnnotationKind,
    AnnotationKindData, Border, CaretAnnotation, FileAttachmentAnnotation, FreeTextAnnotation,
    InkAnnotation, LineAnnotation, LinkAnnotation, MarkupAnnotation, PolygonAnnotation,
    PopupAnnotation, ShapeAnnotation, StampAnnotation, TextAnnotation,
};
pub use destination::{Action, Destination, ViewSpec};
pub use diagnostics::{LocationHint, ParsePhase, ParseWarning, Severity, WarningSink};
pub use embedded_files::{AfRelationship, EmbeddedFile};
pub use error::PdfError;
pub use form_fields::{
    ButtonField, ButtonType, ChoiceField, ChoiceOption, FieldFlags, FieldKind, FieldValue,
    FormCatalog, FormField, SigFlags, SignatureField, TextField,
};
pub use layers::{
    CreatorInfo, ExportUsage, LanguageUsage, Layer, LayerIntent, LayerUsage, PageElementSubtype,
    PrintUsage, UsageState, UserUsage, ViewUsage, ZoomUsage,
};
pub use metadata::{DocumentMetadata, PdfDate, TrappedFlag};
pub use objects::{PdfDict, PdfObj};
pub use outline::{OutlineItem, OutlineStyle};
pub use page_boxes::PageBoxes;
pub use page_tree::PageInfo;
pub use viewer_prefs::{
    Duplex, PageLayout, PageMode, PrintScaling, ReadingDirection, ViewerPreferences,
};

use content::ContentInterpreter;
use resolver::Resolver;
use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use stet_fonts::geometry::Matrix;
use stet_graphics::display_list::DisplayList;
use stet_graphics::icc::IccCache;

/// Font data provider: maps a font file name (e.g. "NimbusSans-Regular") to raw .t1 bytes.
///
/// Used for environments without filesystem access (WASM) where fonts are embedded.
pub type FontProvider = Arc<dyn Fn(&str) -> Option<Vec<u8>> + Send + Sync>;

/// A parsed PDF document.
pub struct PdfDocument<'a> {
    resolver: Resolver<'a>,
    pages: Vec<PageInfo>,
    icc_cache: IccCache,
    font_provider: Option<FontProvider>,
    /// When false (default), PDF overprint flags (OP/op) are suppressed —
    /// skips the expensive CMYK buffer simulation that most viewers omit.
    overprint: bool,
    /// Object numbers of Optional Content Groups that are OFF by default.
    /// Parsed from the catalog's /OCProperties /D /OFF array.
    ocg_off: HashSet<u32>,
    /// Decompressed ICC profile bytes from the first /OutputIntents entry's
    /// /DestOutputProfile stream, if present. Used to match the document's
    /// intended CMYK rendering (ISO Coated v2, SWOP, etc.) at render time.
    output_intent_icc: Option<Vec<u8>>,
    /// Document metadata (Info dict + XMP), parsed lazily on first access.
    metadata_cache: OnceCell<DocumentMetadata>,
    /// Viewer preferences, parsed lazily on first access.
    viewer_prefs_cache: OnceCell<ViewerPreferences>,
    /// Outline tree, parsed lazily on first access.
    outline_cache: OnceCell<Vec<OutlineItem>>,
    /// Named destinations (legacy /Dests + /Names /Dests name tree),
    /// parsed lazily on first access.
    destinations_cache: OnceCell<HashMap<String, Destination>>,
    /// Per-page annotation lists. Each `OnceCell` parses on first
    /// access for that page only — large documents don't pay for
    /// pages a caller never visits.
    page_annotations_cache: Vec<OnceCell<Vec<Annotation>>>,
    /// AcroForm catalog, parsed lazily on first access. Outer
    /// `OnceCell` caches the parse; inner `Option` reflects
    /// presence/absence of `/AcroForm`.
    form_cache: OnceCell<Option<FormCatalog>>,
    /// Embedded files (file attachments), parsed lazily on first
    /// access from the catalog's `/Names /EmbeddedFiles` name tree.
    embedded_files_cache: OnceCell<HashMap<String, EmbeddedFile>>,
    /// Optional Content Groups (layers), parsed lazily on first
    /// access from the catalog's `/OCProperties /OCGs`.
    layers_cache: OnceCell<Vec<Layer>>,
    /// Parse-time warnings accumulated by structural parsers
    /// (outline, annotations, form fields, ...). The sink uses
    /// interior mutability so accessors can record warnings while
    /// holding only `&self`.
    warnings: WarningSink,
}

impl<'a> PdfDocument<'a> {
    /// Parse a PDF from bytes.
    pub fn from_bytes(data: &'a [u8]) -> Result<Self, PdfError> {
        let mut icc_cache = IccCache::new();
        icc_cache.search_system_cmyk_profile();
        Self::from_bytes_inner(data, icc_cache, b"")
    }

    /// Parse a PDF from bytes, using a pre-loaded ICC cache.
    ///
    /// Use this when the caller already has an `IccCache` with the system
    /// CMYK profile loaded (e.g., from the PostScript interpreter context).
    pub fn from_bytes_with_icc(data: &'a [u8], icc_cache: IccCache) -> Result<Self, PdfError> {
        Self::from_bytes_inner(data, icc_cache, b"")
    }

    /// Parse a PDF from bytes using a user-supplied password.
    ///
    /// Returns `PdfError::PasswordRequired` if the password does not
    /// match; callers can retry by calling this again with a different
    /// password.
    pub fn from_bytes_with_password(
        data: &'a [u8],
        icc_cache: IccCache,
        password: &[u8],
    ) -> Result<Self, PdfError> {
        Self::from_bytes_inner(data, icc_cache, password)
    }

    fn from_bytes_inner(
        data: &'a [u8],
        icc_cache: IccCache,
        password: &[u8],
    ) -> Result<Self, PdfError> {
        // Validate header — PDF spec allows up to 1024 bytes before %PDF-
        if !has_pdf_header(data) {
            return Err(PdfError::NotAPdf);
        }

        let xref = xref::parse_xref(data)?;

        // Handle encryption. /Encrypt null means no encryption (some
        // generators emit this).
        let encryption = if let Some(encrypt_ref) = xref.trailer.get(b"Encrypt") {
            if matches!(encrypt_ref, crate::objects::PdfObj::Null) {
                None
            } else {
                // Temporary resolver (without encryption) to dereference
                // the Encrypt dict itself.
                let temp_resolver = Resolver::new(data, &xref);
                let encrypt_obj = temp_resolver.deref(encrypt_ref)?;
                let encrypt_dict = encrypt_obj
                    .as_dict()
                    .ok_or(PdfError::Other("Encrypt is not a dict".into()))?;

                let file_id = xref
                    .trailer
                    .get_array(b"ID")
                    .and_then(|arr| arr.first()?.as_str().map(|s| s.to_vec()))
                    .unwrap_or_default();

                Some(crypto::EncryptionState::try_open_with_password(
                    encrypt_dict,
                    &xref.trailer,
                    &file_id,
                    password,
                )?)
            }
        } else {
            None
        };

        let resolver = Resolver::with_encryption(data, xref, encryption);
        let pages = page_tree::collect_pages(&resolver)?;
        let ocg_off = parse_ocg_off(&resolver);
        let output_intent_icc = parse_output_intent_icc(&resolver);

        let page_annotations_cache = (0..pages.len()).map(|_| OnceCell::new()).collect();
        Ok(Self {
            resolver,
            pages,
            icc_cache,
            font_provider: None,
            overprint: true,
            ocg_off,
            output_intent_icc,
            metadata_cache: OnceCell::new(),
            viewer_prefs_cache: OnceCell::new(),
            outline_cache: OnceCell::new(),
            destinations_cache: OnceCell::new(),
            page_annotations_cache,
            form_cache: OnceCell::new(),
            embedded_files_cache: OnceCell::new(),
            layers_cache: OnceCell::new(),
            warnings: WarningSink::new(),
        })
    }

    /// Enable or disable PDF overprint simulation.
    ///
    /// Enabled by default. When disabled, OP/op flags in graphics state dicts
    /// are ignored, avoiding CMYK buffer tracking.
    pub fn set_overprint(&mut self, enabled: bool) {
        self.overprint = enabled;
    }

    /// Set a font data provider for environments without filesystem access.
    pub fn set_font_provider(&mut self, provider: FontProvider) {
        self.font_provider = Some(provider);
    }

    /// Number of pages in the document.
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Page dimensions in points (width, height), accounting for rotation.
    pub fn page_size(&self, page: usize) -> Result<(f64, f64), PdfError> {
        let info = self
            .pages
            .get(page)
            .ok_or(PdfError::PageOutOfRange(page, self.pages.len()))?;
        let [llx, lly, urx, ury] = info.crop_box;
        let (w, h) = ((urx - llx).abs(), (ury - lly).abs());
        match info.rotate.rem_euclid(360) {
            90 | 270 => Ok((h, w)),
            _ => Ok((w, h)),
        }
    }

    /// Get page info (MediaBox, CropBox, rotation, resources).
    pub fn page_info(&self, page: usize) -> Result<&PageInfo, PdfError> {
        self.pages
            .get(page)
            .ok_or(PdfError::PageOutOfRange(page, self.pages.len()))
    }

    /// Get the decompressed content stream bytes for a page.
    /// If the page has multiple content streams, they are concatenated
    /// with a newline separator.
    pub fn page_contents(&self, page: usize) -> Result<Vec<u8>, PdfError> {
        let info = self
            .pages
            .get(page)
            .ok_or(PdfError::PageOutOfRange(page, self.pages.len()))?;

        if info.contents.is_empty() {
            return Ok(Vec::new());
        }

        let mut result = Vec::new();
        for (i, &(obj_num, gen_num)) in info.contents.iter().enumerate() {
            // Skip content stream refs that fail (e.g., dict without stream body
            // in malformed PDFs). Continue with remaining streams.
            match self.resolver.stream_data(obj_num, gen_num) {
                Ok(data) => {
                    if i > 0 && !result.is_empty() {
                        result.push(b'\n');
                    }
                    result.extend_from_slice(&data);
                }
                Err(_) => continue,
            }
        }

        Ok(result)
    }

    /// Render a page to a DisplayList at the given DPI.
    ///
    /// The display list uses device-space coordinates (paths pre-transformed
    /// through the initial CTM). The initial CTM applies DPI scaling, Y-flip,
    /// and CropBox offset.
    pub fn render_page(&self, page: usize, dpi: f64) -> Result<DisplayList, PdfError> {
        let info = self
            .pages
            .get(page)
            .ok_or(PdfError::PageOutOfRange(page, self.pages.len()))?;

        let [llx, lly, urx, ury] = info.crop_box;
        let (page_w, page_h) = ((urx - llx).abs(), (ury - lly).abs());

        // Build initial CTM: scale by dpi/72, Y-flip (PDF Y-up → device Y-down),
        // and offset by CropBox origin.
        let scale = dpi / 72.0;
        let ctm = match info.rotate.rem_euclid(360) {
            90 => {
                // Rotate 90° CW + Y-flip: (x,y) → (y*s, x*s)
                Matrix::new(0.0, scale, scale, 0.0, 0.0, 0.0).concat(&Matrix::translate(-llx, -lly))
            }
            180 => {
                // Rotate 180° + Y-flip = just X-flip
                Matrix::new(-scale, 0.0, 0.0, scale, page_w * scale, 0.0)
                    .concat(&Matrix::translate(-llx, -lly))
            }
            270 => {
                // Rotate 270° CW + Y-flip: (x,y) → ((page_h-y)*s, (page_w-x)*s)
                Matrix::new(0.0, -scale, -scale, 0.0, page_h * scale, page_w * scale)
                    .concat(&Matrix::translate(-llx, -lly))
            }
            _ => {
                // No rotation: scale + Y-flip + CropBox offset
                // PDF (0,0) at bottom-left → device (0, page_h*scale) at top-left
                Matrix::new(scale, 0.0, 0.0, -scale, -llx * scale, ury * scale)
            }
        };

        // Get page content stream
        let content_data = self.page_contents(page)?;

        // Interpret content stream
        let mut interpreter = ContentInterpreter::new(
            &self.resolver,
            info.resources.clone(),
            ctm,
            &self.icc_cache,
            self.font_provider.clone(),
            self.overprint,
            &self.ocg_off,
        );

        // Check if the page has a DeviceCMYK transparency group — if so,
        // RGB colors need round-tripping through CMYK to match compositing
        // in CMYK space (mutes saturated out-of-gamut RGB colors).
        let page_group_is_cmyk = if let Ok(page_obj) = self.resolver.resolve(info.obj_num, 0)
            && let Some(page_dict) = page_obj.as_dict()
            && let Some(group_obj) = page_dict.get(b"Group")
            && let Ok(group_resolved) = self.resolver.deref(group_obj)
            && let Some(group_dict) = group_resolved.as_dict()
            && group_dict.get_name(b"CS") == Some(b"DeviceCMYK")
        {
            interpreter.set_page_group_cmyk();
            true
        } else {
            false
        };

        // Render page content
        if let Err(e) = interpreter.interpret_stream_public(&content_data) {
            eprintln!("warning: content stream error: {}", e);
        }
        // Unwind any unbalanced q's left by the content stream.
        interpreter.unwind_gstate_stack();

        // Render annotation appearance streams (form field values, stamps, etc.)
        if !info.annots.is_empty() {
            interpreter.reset_clip_for_annotations();
            for &(n, g) in &info.annots {
                let _ = interpreter.render_annotation(n, g);
            }
        }

        let mut dl = interpreter.into_display_list();
        if page_group_is_cmyk {
            dl.set_page_group_color_space(stet_graphics::display_list::GroupColorSpace::DeviceCMYK);
        }
        Ok(dl)
    }

    /// Render a page to RGBA pixel data at the given DPI.
    ///
    /// Returns (pixel_data, width, height). Pixel data is RGBA, 4 bytes per pixel.
    #[cfg(feature = "render")]
    pub fn render_page_to_rgba(
        &self,
        page: usize,
        dpi: f64,
    ) -> Result<(Vec<u8>, u32, u32), PdfError> {
        let (page_w, page_h) = self.page_size(page)?;
        let scale = dpi / 72.0;
        let pixel_w = (page_w * scale).round() as u32;
        let pixel_h = (page_h * scale).round() as u32;

        let display_list = self.render_page(page, dpi)?;

        let rgba = stet_render::render_to_rgba(
            &display_list,
            pixel_w,
            pixel_h,
            dpi,
            Some(&self.icc_cache),
            false,
        );

        Ok((rgba, pixel_w, pixel_h))
    }

    /// Access the ICC color profile cache.
    pub fn icc_cache(&self) -> &IccCache {
        &self.icc_cache
    }

    /// Decompressed ICC profile bytes from the PDF's OutputIntent, if any.
    /// PDF/X files declare their intended CMYK rendering space here (e.g.
    /// ISO Coated v2 300% (ECI)); using it at render time matches the
    /// document author's colour expectations, which system-default profiles
    /// (GS `default_cmyk.icc`, FOGRA39) often approximate only coarsely.
    pub fn output_intent_icc(&self) -> Option<&[u8]> {
        self.output_intent_icc.as_deref()
    }

    /// Register the PDF's OutputIntent ICC profile as the default CMYK profile
    /// in this document's ICC cache, replacing whatever was loaded from
    /// `search_system_cmyk_profile`. Returns `true` when the profile was
    /// present and registered.
    pub fn apply_output_intent_as_default_cmyk(&mut self) -> bool {
        let Some(bytes) = self.output_intent_icc.as_deref() else {
            return false;
        };
        let Some(hash) = self.icc_cache.register_profile(bytes) else {
            return false;
        };
        self.icc_cache.set_system_cmyk(bytes, hash);
        true
    }

    /// Access the resolver for arbitrary object lookups.
    pub fn resolver(&self) -> &Resolver<'a> {
        &self.resolver
    }

    /// Access page info list.
    pub fn pages(&self) -> &[PageInfo] {
        &self.pages
    }

    /// Document metadata: the trailer's `/Info` dict (title, author,
    /// dates, etc.) and the catalog's `/Metadata` XMP stream.
    ///
    /// Parsed lazily on first call and cached. All fields are optional;
    /// a document without an `/Info` dict still returns a value with
    /// every field empty.
    pub fn metadata(&self) -> &DocumentMetadata {
        self.metadata_cache
            .get_or_init(|| metadata::parse_document_metadata(&self.resolver))
    }

    /// Viewer preferences: how the document hints it should be displayed
    /// (page layout, page mode, hide-toolbar, fit-window, print
    /// preferences, etc.).
    ///
    /// Parsed lazily on first call and cached. Fields default per the
    /// PDF spec when the corresponding entries are absent.
    pub fn viewer_preferences(&self) -> &ViewerPreferences {
        self.viewer_prefs_cache
            .get_or_init(|| viewer_prefs::parse_viewer_preferences(&self.resolver))
    }

    /// Document outline (bookmarks) as a tree of [`OutlineItem`]s.
    ///
    /// Returns an empty slice if the document has no outline. Parsed
    /// lazily on first call and cached. Cycles, broken `/First`/`/Next`
    /// chains, and pathological depth are tolerated by hard caps;
    /// each truncation pushes a warning visible through
    /// [`parse_warnings`](Self::parse_warnings).
    pub fn outline(&self) -> &[OutlineItem] {
        self.outline_cache.get_or_init(|| {
            outline::parse_outline_tree(&self.resolver, &self.pages, &self.warnings)
        })
    }

    /// All named destinations in the document, merged from both
    /// `/Catalog /Dests` (legacy) and `/Catalog /Names /Dests` (name
    /// tree). Legacy entries take precedence on key conflict per
    /// ISO 32000-2 §12.3.2.3.
    ///
    /// Parsed lazily on first call and cached. Returns an empty map
    /// when neither source is present.
    pub fn destinations(&self) -> &HashMap<String, Destination> {
        self.destinations_cache
            .get_or_init(|| destination::parse_named_destinations(&self.resolver, &self.pages))
    }

    /// Resolve a named destination by name to its explicit
    /// destination.
    ///
    /// Looks up the document's full name table (legacy + name tree).
    /// If the looked-up entry is itself another named destination
    /// (legal but unusual), the chain is **not** followed — the
    /// caller receives the raw `NamedDest`. This avoids cycles
    /// without bookkeeping.
    pub fn resolve_named_destination(&self, name: &str) -> Option<Destination> {
        self.destinations().get(name).cloned()
    }

    /// Annotations attached to `page` (0-based).
    ///
    /// Returns an empty slice when the page has no annotations.
    /// Parsed lazily on first call **per page** and cached, so a
    /// 1000-page document with annotations only on a handful of
    /// pages doesn't pay to parse the rest.
    ///
    /// Returns `Err(PdfError::PageOutOfRange)` if `page >= page_count()`.
    pub fn page_annotations(&self, page: usize) -> Result<&[Annotation], PdfError> {
        if page >= self.pages.len() {
            return Err(PdfError::PageOutOfRange(page, self.pages.len()));
        }
        let cell = &self.page_annotations_cache[page];
        let annots = cell.get_or_init(|| {
            annotations::parse_page_annotations(&self.resolver, &self.pages, page, &self.warnings)
        });
        Ok(annots.as_slice())
    }

    /// AcroForm — interactive form catalog with field tree, default
    /// appearance, calculation order, and signature flags.
    ///
    /// Returns `None` when the document has no `/AcroForm` (most PDFs
    /// don't). Parsed lazily on first call and cached.
    ///
    /// Each terminal [`FormField`] carries the object numbers of its
    /// widget annotations
    /// ([`FormField::widget_obj_nums`](crate::FormField)); cross-link
    /// with [`page_annotations`](Self::page_annotations) to fetch
    /// renderable widget data.
    pub fn form(&self) -> Option<&FormCatalog> {
        self.form_cache
            .get_or_init(|| form_fields::parse_acroform(&self.resolver, &self.warnings))
            .as_ref()
    }

    /// Parse-time warnings accumulated by the structural accessors.
    ///
    /// Outline cycles, dropped annotations (missing `/Rect`),
    /// form-field tree truncations, and similar recoverable issues
    /// are surfaced here. The list grows as accessors are called for
    /// the first time; cached subsequent calls don't re-emit.
    ///
    /// Returns a borrow of the underlying slice — drop the returned
    /// `Ref` before calling any other accessor that could push more
    /// warnings (e.g. iterating with `for w in doc.parse_warnings().iter()`
    /// is fine; calling `doc.outline()` mid-iteration is not).
    pub fn parse_warnings(&self) -> std::cell::Ref<'_, [ParseWarning]> {
        self.warnings.borrow_slice()
    }

    /// Page geometry for a page (0-based) — all five PDF page boxes
    /// (MediaBox, CropBox, BleedBox, TrimBox, ArtBox) plus rotation,
    /// user unit, and presentation hints.
    ///
    /// Returns `Err(PdfError::PageOutOfRange)` if `page >= page_count()`.
    pub fn page_boxes(&self, page: usize) -> Result<PageBoxes, PdfError> {
        page_boxes::parse_page_boxes(&self.resolver, &self.pages, page)
            .ok_or(PdfError::PageOutOfRange(page, self.pages.len()))
    }

    /// All file attachments declared in the catalog's
    /// `/Names /EmbeddedFiles` name tree, keyed by attachment name.
    ///
    /// Parsed lazily on first call and cached. Returns an empty map
    /// when the document has no embedded files. Use
    /// [`embedded_file_bytes`](Self::embedded_file_bytes) to read the
    /// underlying bytes of an attachment on demand.
    pub fn embedded_files(&self) -> &HashMap<String, EmbeddedFile> {
        self.embedded_files_cache
            .get_or_init(|| embedded_files::parse_embedded_files(&self.resolver))
    }

    /// Read the decompressed bytes of a named embedded file.
    ///
    /// Returns `Err(PdfError::Other(...))` if the name is unknown.
    pub fn embedded_file_bytes(&self, name: &str) -> Result<Vec<u8>, PdfError> {
        let ef = self
            .embedded_files()
            .get(name)
            .ok_or_else(|| PdfError::Other(format!("embedded file not found: {name}")))?;
        embedded_files::decode_embedded_file_stream(
            &self.resolver,
            ef.stream_obj_num,
            ef.stream_gen_num,
        )
    }

    /// All Optional Content Groups (layers) declared by the document.
    ///
    /// Each [`Layer`] carries the OCG's display name, intent, lock
    /// state, full `/Usage` sub-dict, and its initial visibility under
    /// the default configuration. The hierarchy (`/Order`), alternate
    /// configurations, and runtime visibility overrides land in later
    /// phases of the layers API.
    ///
    /// Returns an empty slice when the document has no `/OCProperties`.
    /// Parsed lazily on first call and cached.
    pub fn layers(&self) -> &[Layer] {
        self.layers_cache
            .get_or_init(|| layers::metadata::parse_layers(&self.resolver, &self.warnings))
            .as_slice()
    }

    /// Look up a single layer by its OCG object number.
    ///
    /// Useful when the caller already has an `ocg_id` from a display
    /// list `OcgGroup` element and wants the layer's metadata.
    pub fn layer(&self, ocg_id: u32) -> Option<&Layer> {
        self.layers().iter().find(|l| l.ocg_id == ocg_id)
    }
}

/// Parse the default OFF set from the catalog's OCProperties.
/// Returns a set of object numbers for OCGs that are OFF by default.
/// OCGs not listed in either /ON or /OFF are considered ON (PDF spec default).
fn parse_ocg_off(resolver: &Resolver) -> HashSet<u32> {
    let mut off = HashSet::new();

    // Get catalog — try trailer /Root first, fall back to scanning if it
    // doesn't look like a catalog (corrupt incremental updates can swap
    // /Root and /Info, leaving Root pointing at the Info dict).
    let mut catalog_owned;
    let catalog_dict = if let Some(root_ref) = resolver.trailer().get_ref(b"Root") {
        if let Ok(c) = resolver.resolve(root_ref.0, root_ref.1) {
            catalog_owned = c;
            match catalog_owned.as_dict() {
                Some(d) if d.get(b"OCProperties").is_some() => d,
                _ => match find_catalog(resolver) {
                    Some(c) => {
                        catalog_owned = c;
                        catalog_owned.as_dict().unwrap()
                    }
                    None => return off,
                },
            }
        } else {
            return off;
        }
    } else {
        return off;
    };

    // Get OCProperties -> D (default configuration) -> OFF array
    let oc_props = match catalog_dict.get(b"OCProperties") {
        Some(obj) => match resolver.deref(obj) {
            Ok(o) => o,
            Err(_) => return off,
        },
        None => return off,
    };
    let oc_dict = match oc_props.as_dict() {
        Some(d) => d,
        None => return off,
    };
    let d_obj = match oc_dict.get(b"D") {
        Some(obj) => match resolver.deref(obj) {
            Ok(o) => o,
            Err(_) => return off,
        },
        None => return off,
    };
    let d_dict = match d_obj.as_dict() {
        Some(d) => d,
        None => return off,
    };

    // Collect object numbers from /OFF array (may be an indirect reference)
    if let Some(off_obj) = d_dict.get(b"OFF") {
        let off_resolved = resolver.deref(off_obj).unwrap_or_else(|_| off_obj.clone());
        if let Some(off_arr) = off_resolved.as_array() {
            for obj in off_arr {
                if let Some((num, _gen)) = obj.as_ref() {
                    off.insert(num);
                }
            }
        }
    }

    off
}

/// Extract the decompressed ICC profile bytes from the first PDF/X
/// OutputIntent whose `/DestOutputProfile` is a CMYK ICC stream.
///
/// PDF/X files declare their intended CMYK rendering profile (e.g. "ISO
/// Coated v2 300% (ECI)") via `/Catalog/OutputIntents` with an embedded
/// `/DestOutputProfile` stream. Using that profile at render time matches
/// the author's colour expectations; the system-default profiles used as
/// fallback (GS `default_cmyk.icc`, FOGRA39) only approximate it.
fn parse_output_intent_icc(resolver: &Resolver) -> Option<Vec<u8>> {
    let mut catalog_owned;
    let catalog_dict = if let Some(root_ref) = resolver.trailer().get_ref(b"Root") {
        if let Ok(c) = resolver.resolve(root_ref.0, root_ref.1) {
            catalog_owned = c;
            match catalog_owned.as_dict() {
                Some(d) if d.get(b"OutputIntents").is_some() => d,
                _ => {
                    catalog_owned = find_catalog(resolver)?;
                    catalog_owned.as_dict()?
                }
            }
        } else {
            catalog_owned = find_catalog(resolver)?;
            catalog_owned.as_dict()?
        }
    } else {
        catalog_owned = find_catalog(resolver)?;
        catalog_owned.as_dict()?
    };

    let intents_obj = resolver.deref(catalog_dict.get(b"OutputIntents")?).ok()?;
    let intents_arr = intents_obj.as_array()?;
    for entry in intents_arr {
        let intent = match resolver.deref(entry) {
            Ok(o) => o,
            Err(_) => continue,
        };
        let Some(intent_dict) = intent.as_dict() else {
            continue;
        };
        let Some(profile_obj) = intent_dict.get(b"DestOutputProfile") else {
            continue;
        };
        let Ok(bytes) = resolver.stream_data_from_obj(profile_obj) else {
            continue;
        };
        // ICC header: color space at offset 16, 'acsp' magic at offset 36.
        if bytes.len() >= 40 && &bytes[36..40] == b"acsp" && &bytes[16..20] == b"CMYK" {
            return Some(bytes);
        }
    }
    None
}

/// Scan all objects to find the real Catalog dict (has /Type /Catalog).
/// Used when the trailer's /Root points to the wrong object.
pub(crate) fn find_catalog(resolver: &Resolver) -> Option<PdfObj> {
    let xref_len = resolver.xref_len();
    for obj_num in 0..xref_len as u32 {
        if let Ok(obj) = resolver.resolve(obj_num, 0) {
            if let Some(dict) = obj.as_dict() {
                if dict.get_name(b"Type") == Some(b"Catalog") && dict.get(b"Pages").is_some() {
                    return Some(obj);
                }
            }
        }
    }
    None
}

/// Check for `%PDF-` header within the first 1024 bytes.
/// The PDF spec (§7.5.2) allows data before the header.
fn has_pdf_header(data: &[u8]) -> bool {
    let search_range = data.len().min(1024);
    data[..search_range].windows(5).any(|w| w == b"%PDF-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_a_pdf() {
        let result = PdfDocument::from_bytes(b"not a pdf");
        assert!(matches!(result, Err(PdfError::NotAPdf)));
    }

    #[test]
    fn parse_minimal_pdf() {
        let pdf = build_minimal_pdf();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        assert_eq!(doc.page_count(), 1);

        let (w, h) = doc.page_size(0).unwrap();
        assert_eq!(w, 612.0);
        assert_eq!(h, 792.0);
    }

    #[test]
    fn page_out_of_range() {
        let pdf = build_minimal_pdf();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        assert!(matches!(
            doc.page_size(5),
            Err(PdfError::PageOutOfRange(5, 1))
        ));
    }

    #[test]
    fn page_contents_empty() {
        let pdf = build_minimal_pdf();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        let contents = doc.page_contents(0).unwrap();
        // Our minimal PDF has no content stream
        assert!(contents.is_empty());
    }

    #[test]
    #[ignore]
    fn dump_display_list() {
        use stet_fonts::geometry::PsPath;
        use stet_graphics::display_list::{DisplayElement, DisplayList};

        fn path_bbox(path: &PsPath) -> String {
            use stet_fonts::geometry::PathSegment;
            let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
            for seg in &path.segments {
                let pts: Vec<(f64, f64)> = match seg {
                    PathSegment::MoveTo(x, y) | PathSegment::LineTo(x, y) => vec![(*x, *y)],
                    PathSegment::CurveTo {
                        x1,
                        y1,
                        x2,
                        y2,
                        x3,
                        y3,
                    } => vec![(*x1, *y1), (*x2, *y2), (*x3, *y3)],
                    PathSegment::ClosePath => vec![],
                };
                for (px, py) in pts {
                    x0 = x0.min(px);
                    y0 = y0.min(py);
                    x1 = x1.max(px);
                    y1 = y1.max(py);
                }
            }
            format!("bbox=({:.0},{:.0},{:.0},{:.0})", x0, y0, x1, y1)
        }

        fn dump(list: &DisplayList, depth: usize) {
            let indent = "  ".repeat(depth);
            for (i, elem) in list.elements().iter().enumerate() {
                match elem {
                    DisplayElement::Fill { path, params } => {
                        let c = &params.color;
                        let cmyk_str = if let Some((c2, m, y, k)) = params.color.native_cmyk {
                            format!(" cmyk=({:.2},{:.2},{:.2},{:.2})", c2, m, y, k)
                        } else {
                            String::new()
                        };
                        eprintln!(
                            "{indent}[{i}] Fill rgb=({:.2},{:.2},{:.2}){} op={} opm={} ch=0x{:x} a={:.2} {}",
                            c.r,
                            c.g,
                            c.b,
                            cmyk_str,
                            params.overprint,
                            params.overprint_mode,
                            params.painted_channels,
                            params.alpha,
                            path_bbox(path)
                        );
                    }
                    DisplayElement::Stroke { path, params } => {
                        let c = &params.color;
                        eprintln!(
                            "{indent}[{i}] Stroke rgb=({:.2},{:.2},{:.2}) {}",
                            c.r,
                            c.g,
                            c.b,
                            path_bbox(path)
                        );
                    }
                    DisplayElement::Clip { path, .. } => {
                        eprintln!("{indent}[{i}] Clip {}", path_bbox(path))
                    }
                    DisplayElement::InitClip => eprintln!("{indent}[{i}] InitClip"),
                    DisplayElement::Image { params, .. } => {
                        eprintln!("{indent}[{i}] Image {}x{}", params.width, params.height);
                    }
                    DisplayElement::ErasePage => eprintln!("{indent}[{i}] ErasePage"),
                    DisplayElement::AxialShading { params } => {
                        eprintln!(
                            "{indent}[{i}] AxialShading cs={:?} stops={}",
                            params.color_space,
                            params.color_stops.len()
                        );
                    }
                    DisplayElement::RadialShading { params } => {
                        eprintln!(
                            "{indent}[{i}] RadialShading cs={:?} stops={} ext=({},{}) c0=({:.1},{:.1}) r0={:.1} c1=({:.1},{:.1}) r1={:.1} bbox={:?} op={} ch=0x{:x}",
                            params.color_space,
                            params.color_stops.len(),
                            params.extend_start,
                            params.extend_end,
                            params.x0,
                            params.y0,
                            params.r0,
                            params.x1,
                            params.y1,
                            params.r1,
                            params.bbox,
                            params.overprint,
                            params.painted_channels
                        );
                        // Print first and last stop
                        if let Some(first) = params.color_stops.first() {
                            eprintln!(
                                "{indent}  stop[0]: pos={:.3} rgb=({:.3},{:.3},{:.3}) raw={:?}",
                                first.position,
                                first.color.r,
                                first.color.g,
                                first.color.b,
                                first.raw_components
                            );
                        }
                        if let Some(last) = params.color_stops.last() {
                            eprintln!(
                                "{indent}  stop[{}]: pos={:.3} rgb=({:.3},{:.3},{:.3}) raw={:?}",
                                params.color_stops.len() - 1,
                                last.position,
                                last.color.r,
                                last.color.g,
                                last.color.b,
                                last.raw_components
                            );
                        }
                        // Print mid stop
                        let mid = params.color_stops.len() / 2;
                        if mid > 0 && mid < params.color_stops.len() - 1 {
                            let s = &params.color_stops[mid];
                            eprintln!(
                                "{indent}  stop[{mid}]: pos={:.3} rgb=({:.3},{:.3},{:.3}) raw={:?}",
                                s.position, s.color.r, s.color.g, s.color.b, s.raw_components
                            );
                        }
                    }
                    DisplayElement::MeshShading { .. } => eprintln!("{indent}[{i}] MeshShading"),
                    DisplayElement::PatchShading { .. } => eprintln!("{indent}[{i}] PatchShading"),
                    DisplayElement::PatternFill { .. } => eprintln!("{indent}[{i}] PatternFill"),
                    DisplayElement::Text { .. } => eprintln!("{indent}[{i}] Text"),
                    DisplayElement::Group { elements, params } => {
                        eprintln!(
                            "{indent}[{i}] Group iso={} ko={} blend={} a={:.2} bbox=({:.0},{:.0},{:.0},{:.0}) children={}",
                            params.isolated,
                            params.knockout,
                            params.blend_mode,
                            params.alpha,
                            params.bbox[0],
                            params.bbox[1],
                            params.bbox[2],
                            params.bbox[3],
                            elements.len()
                        );
                        dump(elements, depth + 1);
                    }
                    DisplayElement::SoftMasked {
                        mask,
                        content,
                        params,
                        ..
                    } => {
                        eprintln!(
                            "{indent}[{i}] SoftMasked {:?} mask={} content={}",
                            params.subtype,
                            mask.len(),
                            content.len()
                        );
                        eprintln!("{indent}  MASK:");
                        dump(mask, depth + 2);
                        eprintln!("{indent}  CONTENT:");
                        dump(content, depth + 2);
                    }
                    DisplayElement::OcgGroup {
                        elements,
                        ocg_id,
                        default_visible,
                    } => {
                        eprintln!(
                            "{indent}[{i}] OcgGroup id={} visible={} children={}",
                            ocg_id,
                            default_visible,
                            elements.len()
                        );
                        dump(elements, depth + 1);
                    }
                }
            }
        }

        let data = std::fs::read("../../pdf_samples/PDFX-ready_Output-Test_X4.pdf").unwrap();
        let doc = PdfDocument::from_bytes(&data).unwrap();
        let dl = doc.render_page(0, 72.0).unwrap();
        eprintln!("=== Display list: {} top-level elements ===", dl.len());
        dump(&dl, 0);
    }

    /// Build a minimal PDF with a 3-node outline tree: a parent
    /// "Chapter 1" with two children "Section 1.1" and "Section 1.2".
    /// Used to exercise the outline walker end-to-end.
    fn build_pdf_with_outline() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend(b"%PDF-1.4\n");

        let mut offsets: Vec<usize> = Vec::new();
        let mut push_obj = |buf: &mut Vec<u8>, body: &[u8]| {
            offsets.push(buf.len());
            buf.extend(body);
        };

        // 1: Catalog (with /Outlines ref)
        push_obj(
            &mut pdf,
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>\nendobj\n",
        );
        // 2: Pages
        push_obj(
            &mut pdf,
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        );
        // 3: Page
        push_obj(
            &mut pdf,
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        // 4: Outlines (root): /First and /Last both point to obj 5
        push_obj(
            &mut pdf,
            b"4 0 obj\n<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 3 >>\nendobj\n",
        );
        // 5: Outline "Chapter 1" (open, two children)
        push_obj(
            &mut pdf,
            b"5 0 obj\n<< /Title (Chapter 1) /Parent 4 0 R /First 6 0 R /Last 7 0 R \
              /Count 2 /Dest [3 0 R /Fit] /F 2 >>\nendobj\n",
        );
        // 6: Outline "Section 1.1"
        push_obj(
            &mut pdf,
            b"6 0 obj\n<< /Title (Section 1.1) /Parent 5 0 R /Next 7 0 R \
              /Dest [3 0 R /XYZ 72 700 1.0] /C [0.2 0.3 0.4] >>\nendobj\n",
        );
        // 7: Outline "Section 1.2"
        push_obj(
            &mut pdf,
            b"7 0 obj\n<< /Title (Section 1.2) /Parent 5 0 R /Prev 6 0 R \
              /A << /S /URI /URI (https://example.com) >> /F 1 >>\nendobj\n",
        );

        let xref_offset = pdf.len();
        pdf.extend(b"xref\n0 8\n");
        pdf.extend(b"0000000000 65535 f\r\n");
        for off in &offsets {
            pdf.extend(format!("{:010} 00000 n\r\n", off).as_bytes());
        }
        pdf.extend(b"trailer\n<< /Size 8 /Root 1 0 R >>\n");
        pdf.extend(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

        pdf
    }

    #[test]
    fn outline_basic_tree() {
        let pdf = build_pdf_with_outline();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        let outline = doc.outline();

        assert_eq!(outline.len(), 1, "expected one top-level entry");
        let chapter = &outline[0];
        assert_eq!(chapter.title, "Chapter 1");
        assert!(chapter.open, "Chapter 1 has /Count 2 (positive = open)");
        assert!(chapter.style.bold);
        assert!(!chapter.style.italic);
        assert_eq!(chapter.children.len(), 2);

        let s11 = &chapter.children[0];
        assert_eq!(s11.title, "Section 1.1");
        assert!(s11.action.is_none());
        match &s11.destination {
            Some(crate::Destination::PageView { page, view }) => {
                assert_eq!(*page, Some(0));
                assert!(matches!(view, crate::ViewSpec::Xyz { .. }));
            }
            other => panic!("expected PageView destination, got {other:?}"),
        }
        assert_eq!(s11.color, Some([0.2, 0.3, 0.4]));

        let s12 = &chapter.children[1];
        assert_eq!(s12.title, "Section 1.2");
        assert!(s12.style.italic && !s12.style.bold);
        match &s12.action {
            Some(crate::Action::Uri { uri, is_map }) => {
                assert_eq!(uri, "https://example.com");
                assert!(!is_map);
            }
            other => panic!("expected URI action, got {other:?}"),
        }
    }

    #[test]
    fn outline_caches_across_calls() {
        let pdf = build_pdf_with_outline();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        let a = doc.outline();
        let b = doc.outline();
        assert!(std::ptr::eq(a, b), "outline() must be cached");
    }

    #[test]
    fn outline_empty_when_absent() {
        let pdf = build_minimal_pdf();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        assert!(doc.outline().is_empty());
    }

    /// Build a PDF with named destinations declared via the legacy
    /// `/Catalog /Dests` direct dict.
    fn build_pdf_with_legacy_dests() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend(b"%PDF-1.4\n");

        let mut offsets: Vec<usize> = Vec::new();
        let mut push_obj = |buf: &mut Vec<u8>, body: &[u8]| {
            offsets.push(buf.len());
            buf.extend(body);
        };

        // 1: Catalog with /Dests pointing at obj 4
        push_obj(
            &mut pdf,
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Dests 4 0 R >>\nendobj\n",
        );
        // 2: Pages
        push_obj(
            &mut pdf,
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        );
        // 3: Page
        push_obj(
            &mut pdf,
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        // 4: Legacy /Dests dict
        push_obj(
            &mut pdf,
            b"4 0 obj\n<< /Intro [3 0 R /Fit] /Glossary [3 0 R /XYZ 100 700 1.0] >>\nendobj\n",
        );

        let xref_offset = pdf.len();
        pdf.extend(b"xref\n0 5\n");
        pdf.extend(b"0000000000 65535 f\r\n");
        for off in &offsets {
            pdf.extend(format!("{:010} 00000 n\r\n", off).as_bytes());
        }
        pdf.extend(b"trailer\n<< /Size 5 /Root 1 0 R >>\n");
        pdf.extend(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

        pdf
    }

    /// Build a PDF with named destinations declared via the modern
    /// `/Catalog /Names /Dests` name tree (flat leaf form).
    fn build_pdf_with_name_tree_dests() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend(b"%PDF-1.4\n");

        let mut offsets: Vec<usize> = Vec::new();
        let mut push_obj = |buf: &mut Vec<u8>, body: &[u8]| {
            offsets.push(buf.len());
            buf.extend(body);
        };

        // 1: Catalog with /Names dict
        push_obj(
            &mut pdf,
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Names 4 0 R >>\nendobj\n",
        );
        // 2: Pages
        push_obj(
            &mut pdf,
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        );
        // 3: Page
        push_obj(
            &mut pdf,
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        // 4: /Names dict pointing at /Dests name-tree root
        push_obj(&mut pdf, b"4 0 obj\n<< /Dests 5 0 R >>\nendobj\n");
        // 5: Name-tree leaf with two entries (sorted)
        push_obj(
            &mut pdf,
            b"5 0 obj\n<< /Names [(Alpha) [3 0 R /Fit] (Beta) [3 0 R /XYZ 50 500 0]] >>\nendobj\n",
        );

        let xref_offset = pdf.len();
        pdf.extend(b"xref\n0 6\n");
        pdf.extend(b"0000000000 65535 f\r\n");
        for off in &offsets {
            pdf.extend(format!("{:010} 00000 n\r\n", off).as_bytes());
        }
        pdf.extend(b"trailer\n<< /Size 6 /Root 1 0 R >>\n");
        pdf.extend(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

        pdf
    }

    /// Build a PDF where the *same* destination name appears in both
    /// legacy /Dests and the name tree, with different targets — the
    /// legacy entry must win per spec.
    fn build_pdf_with_dest_conflict() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend(b"%PDF-1.4\n");

        let mut offsets: Vec<usize> = Vec::new();
        let mut push_obj = |buf: &mut Vec<u8>, body: &[u8]| {
            offsets.push(buf.len());
            buf.extend(body);
        };

        // 1: Catalog with both /Dests and /Names
        push_obj(
            &mut pdf,
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Dests 4 0 R /Names 5 0 R >>\nendobj\n",
        );
        push_obj(
            &mut pdf,
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        );
        push_obj(
            &mut pdf,
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        // 4: legacy /Dests — Conflict points at /Fit
        push_obj(&mut pdf, b"4 0 obj\n<< /Conflict [3 0 R /Fit] >>\nendobj\n");
        // 5: /Names with /Dests — Conflict points at /FitB (should be overridden)
        push_obj(&mut pdf, b"5 0 obj\n<< /Dests 6 0 R >>\nendobj\n");
        push_obj(
            &mut pdf,
            b"6 0 obj\n<< /Names [(Conflict) [3 0 R /FitB]] >>\nendobj\n",
        );

        let xref_offset = pdf.len();
        pdf.extend(b"xref\n0 7\n");
        pdf.extend(b"0000000000 65535 f\r\n");
        for off in &offsets {
            pdf.extend(format!("{:010} 00000 n\r\n", off).as_bytes());
        }
        pdf.extend(b"trailer\n<< /Size 7 /Root 1 0 R >>\n");
        pdf.extend(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

        pdf
    }

    #[test]
    fn destinations_legacy_dict() {
        let pdf = build_pdf_with_legacy_dests();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        let dests = doc.destinations();
        assert_eq!(dests.len(), 2);
        match dests.get("Intro") {
            Some(crate::Destination::PageView { page, view }) => {
                assert_eq!(*page, Some(0));
                assert_eq!(*view, crate::ViewSpec::Fit);
            }
            other => panic!("expected PageView for Intro, got {other:?}"),
        }
        assert!(dests.contains_key("Glossary"));
    }

    #[test]
    fn destinations_name_tree() {
        let pdf = build_pdf_with_name_tree_dests();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        let dests = doc.destinations();
        assert_eq!(dests.len(), 2);
        assert!(dests.contains_key("Alpha"));
        assert!(dests.contains_key("Beta"));
    }

    #[test]
    fn destinations_legacy_overrides_name_tree() {
        let pdf = build_pdf_with_dest_conflict();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        let dests = doc.destinations();
        assert_eq!(dests.len(), 1);
        match dests.get("Conflict") {
            Some(crate::Destination::PageView { view, .. }) => {
                assert_eq!(
                    *view,
                    crate::ViewSpec::Fit,
                    "legacy /Dests must override /Names /Dests"
                );
            }
            other => panic!("expected PageView, got {other:?}"),
        }
    }

    #[test]
    fn destinations_caches_across_calls() {
        let pdf = build_pdf_with_legacy_dests();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        let a = doc.destinations();
        let b = doc.destinations();
        assert!(std::ptr::eq(a, b), "destinations() must be cached");
    }

    /// Build a one-page PDF with five annotations exercising the most
    /// commonly used subtypes: Link (URI), Text (sticky note),
    /// Highlight (markup with /QuadPoints), Square (interior color),
    /// FreeText (default appearance + quadding).
    fn build_pdf_with_annotations() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend(b"%PDF-1.4\n");

        let mut offsets: Vec<usize> = Vec::new();
        let mut push_obj = |buf: &mut Vec<u8>, body: &[u8]| {
            offsets.push(buf.len());
            buf.extend(body);
        };

        // 1: Catalog
        push_obj(
            &mut pdf,
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        );
        // 2: Pages
        push_obj(
            &mut pdf,
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        );
        // 3: Page with /Annots referencing 4..8
        push_obj(
            &mut pdf,
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Annots [4 0 R 5 0 R 6 0 R 7 0 R 8 0 R] >>\nendobj\n",
        );
        // 4: Link annotation with URI action
        push_obj(
            &mut pdf,
            b"4 0 obj\n<< /Type /Annot /Subtype /Link /Rect [72 720 540 740] \
              /Border [0 0 1] \
              /A << /S /URI /URI (https://example.com) >> >>\nendobj\n",
        );
        // 5: Text annotation (sticky note)
        push_obj(
            &mut pdf,
            b"5 0 obj\n<< /Type /Annot /Subtype /Text /Rect [100 600 120 620] \
              /Contents (A note) /Open true /Name /Comment /T (Scott) \
              /M (D:20260427120000Z) >>\nendobj\n",
        );
        // 6: Highlight markup
        push_obj(
            &mut pdf,
            b"6 0 obj\n<< /Type /Annot /Subtype /Highlight /Rect [72 500 300 520] \
              /QuadPoints [72 520 300 520 72 500 300 500] \
              /C [1.0 0.95 0.0] >>\nendobj\n",
        );
        // 7: Square shape with interior color
        push_obj(
            &mut pdf,
            b"7 0 obj\n<< /Type /Annot /Subtype /Square /Rect [200 400 300 450] \
              /IC [0.0 0.5 1.0] /C [0.0 0.0 0.0] /F 4 >>\nendobj\n",
        );
        // 8: FreeText
        push_obj(
            &mut pdf,
            b"8 0 obj\n<< /Type /Annot /Subtype /FreeText /Rect [72 300 300 350] \
              /Contents (Visible text) /DA (/Helv 10 Tf 0 g) /Q 1 \
              /IT /FreeTextCallout >>\nendobj\n",
        );

        let xref_offset = pdf.len();
        pdf.extend(b"xref\n0 9\n");
        pdf.extend(b"0000000000 65535 f\r\n");
        for off in &offsets {
            pdf.extend(format!("{:010} 00000 n\r\n", off).as_bytes());
        }
        pdf.extend(b"trailer\n<< /Size 9 /Root 1 0 R >>\n");
        pdf.extend(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

        pdf
    }

    #[test]
    fn page_annotations_basic_subtypes() {
        let pdf = build_pdf_with_annotations();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        let annots = doc.page_annotations(0).unwrap();
        assert_eq!(annots.len(), 5);

        // Link
        let link = &annots[0];
        assert_eq!(link.kind, crate::AnnotationKind::Link);
        assert_eq!(link.rect, [72.0, 720.0, 540.0, 740.0]);
        match &link.kind_data {
            crate::AnnotationKindData::Link(l) => match &l.action {
                Some(crate::Action::Uri { uri, .. }) => {
                    assert_eq!(uri, "https://example.com");
                }
                other => panic!("expected Uri action, got {other:?}"),
            },
            other => panic!("expected Link kind data, got {other:?}"),
        }

        // Text
        let text = &annots[1];
        assert_eq!(text.kind, crate::AnnotationKind::Text);
        assert_eq!(text.contents.as_deref(), Some("A note"));
        assert_eq!(text.title.as_deref(), Some("Scott"));
        match &text.kind_data {
            crate::AnnotationKindData::Text(t) => {
                assert!(t.open);
                assert_eq!(t.icon.as_deref(), Some("Comment"));
            }
            _ => panic!("expected Text kind"),
        }
        // Modified date should parse.
        assert!(matches!(
            text.modified,
            Some(crate::AnnotationDate::Date(_))
        ));

        // Highlight
        let hl = &annots[2];
        assert_eq!(hl.kind, crate::AnnotationKind::Highlight);
        assert_eq!(
            hl.color,
            Some(crate::AnnotationColor::Rgb([1.0, 0.95, 0.0]))
        );
        match &hl.kind_data {
            crate::AnnotationKindData::Markup(m) => {
                assert_eq!(m.quad_points.len(), 1);
            }
            _ => panic!("expected Markup kind"),
        }

        // Square
        let sq = &annots[3];
        assert_eq!(sq.kind, crate::AnnotationKind::Square);
        assert!(sq.flags.print);
        match &sq.kind_data {
            crate::AnnotationKindData::Shape(s) => {
                assert_eq!(
                    s.interior_color,
                    Some(crate::AnnotationColor::Rgb([0.0, 0.5, 1.0]))
                );
            }
            _ => panic!("expected Shape kind"),
        }

        // FreeText
        let ft = &annots[4];
        assert_eq!(ft.kind, crate::AnnotationKind::FreeText);
        match &ft.kind_data {
            crate::AnnotationKindData::FreeText(f) => {
                assert_eq!(f.default_appearance.as_deref(), Some("/Helv 10 Tf 0 g"));
                assert_eq!(f.quadding, 1);
                assert_eq!(f.intent.as_deref(), Some("FreeTextCallout"));
            }
            _ => panic!("expected FreeText kind"),
        }
    }

    #[test]
    fn page_annotations_caches_per_page() {
        let pdf = build_pdf_with_annotations();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        let a = doc.page_annotations(0).unwrap();
        let b = doc.page_annotations(0).unwrap();
        assert!(std::ptr::eq(a, b), "page_annotations(0) must be cached");
    }

    #[test]
    fn page_annotations_out_of_range() {
        let pdf = build_pdf_with_annotations();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        assert!(doc.page_annotations(99).is_err());
    }

    #[test]
    fn page_annotations_empty_when_absent() {
        let pdf = build_minimal_pdf();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        let annots = doc.page_annotations(0).unwrap();
        assert!(annots.is_empty());
    }

    /// Build a one-page PDF with a small AcroForm: a text field, a
    /// checkbox, a 2-button radio group, a combo box, and a
    /// container "shipping" with two terminal text-field children
    /// "shipping.street" and "shipping.zip".
    fn build_pdf_with_form() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend(b"%PDF-1.4\n");

        let mut offsets: Vec<usize> = Vec::new();
        let mut push_obj = |buf: &mut Vec<u8>, body: &[u8]| {
            offsets.push(buf.len());
            buf.extend(body);
        };

        // 1: Catalog with /AcroForm
        push_obj(
            &mut pdf,
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>\nendobj\n",
        );
        // 2: Pages
        push_obj(
            &mut pdf,
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        );
        // 3: Page (with /Annots referencing widgets 5,6,9,10,12)
        push_obj(
            &mut pdf,
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Annots [5 0 R 6 0 R 9 0 R 10 0 R 12 0 R 14 0 R 15 0 R] >>\nendobj\n",
        );
        // 4: AcroForm dict (top-level fields = text, checkbox, radio, combo, shipping container)
        push_obj(
            &mut pdf,
            b"4 0 obj\n<< /Fields [5 0 R 6 0 R 7 0 R 11 0 R 13 0 R] \
              /NeedAppearances true /SigFlags 1 \
              /CO [(name)] /DA (/Helv 12 Tf 0 g) /Q 0 >>\nendobj\n",
        );
        // 5: Text field "name" (also its own widget)
        push_obj(
            &mut pdf,
            b"5 0 obj\n<< /T (name) /TU (Full Name) /FT /Tx /Ff 0 \
              /MaxLen 50 /V (Scott) /DV () \
              /Subtype /Widget /Rect [72 720 300 740] /Type /Annot >>\nendobj\n",
        );
        // 6: Checkbox "agree" (own widget)
        push_obj(
            &mut pdf,
            b"6 0 obj\n<< /T (agree) /FT /Btn /Ff 0 /V /Yes \
              /Subtype /Widget /Rect [72 700 90 718] /Type /Annot >>\nendobj\n",
        );
        // 7: Radio group "color" — non-widget parent with /Kids
        push_obj(
            &mut pdf,
            b"7 0 obj\n<< /T (color) /FT /Btn /Ff 49152 /V /Red \
              /Kids [9 0 R 10 0 R] /Opt [(Red) (Blue)] >>\nendobj\n",
        );
        //   bit 15 (NoToggleToOff) | bit 16 (Radio) | bit 17 cleared = 0xC000 = 49152
        // 9: Radio widget Red (child)
        push_obj(
            &mut pdf,
            b"9 0 obj\n<< /Parent 7 0 R /Subtype /Widget /Type /Annot \
              /Rect [72 680 90 698] /AS /Red >>\nendobj\n",
        );
        // 10: Radio widget Blue (child)
        push_obj(
            &mut pdf,
            b"10 0 obj\n<< /Parent 7 0 R /Subtype /Widget /Type /Annot \
              /Rect [100 680 118 698] /AS /Off >>\nendobj\n",
        );
        // 11: Combo box "country" (own widget)
        push_obj(
            &mut pdf,
            b"11 0 obj\n<< /T (country) /FT /Ch /Ff 131072 /V (US) \
              /Opt [[(US) (United States)] [(GB) (United Kingdom)]] \
              /Subtype /Widget /Rect [72 660 200 678] /Type /Annot >>\nendobj\n",
        );
        //   bit 18 = Combo = 0x20000 = 131072
        // 13: Container "shipping" (no /FT, has /Kids)
        push_obj(
            &mut pdf,
            b"13 0 obj\n<< /T (shipping) /Kids [14 0 R 15 0 R] >>\nendobj\n",
        );
        // 14: Text field "shipping.street" (own widget)
        push_obj(
            &mut pdf,
            b"14 0 obj\n<< /T (street) /Parent 13 0 R /FT /Tx /V (123 Main) \
              /Subtype /Widget /Rect [72 640 300 658] /Type /Annot >>\nendobj\n",
        );
        // 15: Text field "shipping.zip" (own widget)
        push_obj(
            &mut pdf,
            b"15 0 obj\n<< /T (zip) /Parent 13 0 R /FT /Tx /V (12345) \
              /Subtype /Widget /Rect [72 620 200 638] /Type /Annot >>\nendobj\n",
        );

        let xref_offset = pdf.len();
        // We have objects 1..=15 except 8 and 12. Use a simple "all
        // present" xref sized to 16 entries; missing slots get free
        // entries pointing nowhere, which the resolver tolerates.
        let real_offsets: Vec<usize> = offsets;
        // Build a map by inserting each real offset at its declared
        // object number index.
        let mut entries: Vec<Option<usize>> = vec![None; 16];
        // The offsets vector was pushed in declaration order; we
        // declared 1, 2, 3, 4, 5, 6, 7, 9, 10, 11, 13, 14, 15.
        let declared = [1u32, 2, 3, 4, 5, 6, 7, 9, 10, 11, 13, 14, 15];
        for (i, &n) in declared.iter().enumerate() {
            entries[n as usize] = Some(real_offsets[i]);
        }
        pdf.extend(b"xref\n0 16\n");
        pdf.extend(b"0000000000 65535 f\r\n");
        for entry in entries.iter().skip(1) {
            match entry {
                Some(off) => pdf.extend(format!("{:010} 00000 n\r\n", off).as_bytes()),
                None => pdf.extend(b"0000000000 65535 f\r\n"),
            }
        }
        pdf.extend(b"trailer\n<< /Size 16 /Root 1 0 R >>\n");
        pdf.extend(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

        pdf
    }

    #[test]
    fn form_basic_field_tree() {
        let pdf = build_pdf_with_form();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        let form = doc.form().expect("AcroForm should be present");

        assert!(form.need_appearances);
        assert!(form.sig_flags.signatures_exist);
        assert!(!form.sig_flags.append_only);
        assert_eq!(form.calculation_order, vec!["name".to_string()]);
        assert_eq!(form.default_appearance.as_deref(), Some("/Helv 12 Tf 0 g"));

        // Top-level fields: name, agree, color, country, shipping
        assert_eq!(form.fields.len(), 5);

        let name = &form.fields[0];
        assert_eq!(name.name, "name");
        assert_eq!(name.alternate_name.as_deref(), Some("Full Name"));
        match &name.kind {
            crate::FieldKind::Text(t) => {
                assert_eq!(t.max_length, Some(50));
                assert!(!t.multiline && !t.password);
            }
            _ => panic!("name should be a Text field"),
        }
        assert_eq!(name.value, crate::FieldValue::Text("Scott".to_string()));
        // Self-as-widget: name field is its own widget
        assert_eq!(name.widget_obj_nums.len(), 1);

        let agree = &form.fields[1];
        match &agree.kind {
            crate::FieldKind::Button(b) => {
                assert_eq!(b.button_type, crate::ButtonType::Checkbox);
            }
            _ => panic!("agree should be a Button"),
        }
        assert_eq!(agree.value, crate::FieldValue::Name("Yes".to_string()));

        let color = &form.fields[2];
        assert_eq!(color.name, "color");
        match &color.kind {
            crate::FieldKind::Button(b) => {
                assert_eq!(b.button_type, crate::ButtonType::Radio);
                assert!(b.no_toggle_to_off);
                assert_eq!(b.options, vec!["Red".to_string(), "Blue".to_string()]);
            }
            _ => panic!("color should be a Radio group"),
        }
        // Two widget children attached to the radio field
        assert_eq!(color.widget_obj_nums.len(), 2);
        assert!(
            color.children.is_empty(),
            "widget /Kids should not become children"
        );

        let country = &form.fields[3];
        match &country.kind {
            crate::FieldKind::Choice(c) => {
                assert!(c.combo);
                assert_eq!(c.options.len(), 2);
                assert_eq!(c.options[0].export, "US");
                assert_eq!(c.options[0].display, "United States");
            }
            _ => panic!("country should be a Choice"),
        }

        let shipping = &form.fields[4];
        assert_eq!(shipping.name, "shipping");
        assert!(matches!(shipping.kind, crate::FieldKind::Container));
        assert_eq!(shipping.children.len(), 2);
        assert_eq!(shipping.children[0].name, "shipping.street");
        assert_eq!(shipping.children[1].name, "shipping.zip");
        assert_eq!(
            shipping.children[0].value,
            crate::FieldValue::Text("123 Main".to_string())
        );
    }

    #[test]
    fn form_caches_across_calls() {
        let pdf = build_pdf_with_form();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        let a = doc.form().unwrap();
        let b = doc.form().unwrap();
        assert!(std::ptr::eq(a, b), "form() must be cached");
    }

    #[test]
    fn form_absent_returns_none() {
        let pdf = build_minimal_pdf();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        assert!(doc.form().is_none());
    }

    /// Build a one-page PDF declaring all five page boxes plus a
    /// non-default UserUnit and a /Rotate of 90.
    fn build_pdf_with_page_boxes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend(b"%PDF-1.4\n");

        let mut offsets: Vec<usize> = Vec::new();
        let mut push_obj = |buf: &mut Vec<u8>, body: &[u8]| {
            offsets.push(buf.len());
            buf.extend(body);
        };

        push_obj(
            &mut pdf,
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        );
        push_obj(
            &mut pdf,
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        );
        // Page with all 5 boxes distinct, /Rotate 90, /UserUnit 1.5,
        // /Dur 5, /Trans presence, /AA presence.
        push_obj(
            &mut pdf,
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R \
              /MediaBox [0 0 612 792] \
              /CropBox  [10 10 602 782] \
              /BleedBox [5 5 607 787] \
              /TrimBox  [20 20 592 772] \
              /ArtBox   [30 30 582 762] \
              /Rotate 90 /UserUnit 1.5 /Dur 5.0 \
              /Trans << /S /Wipe >> /AA << /O 5 0 R >> >>\nendobj\n",
        );

        let xref_offset = pdf.len();
        pdf.extend(b"xref\n0 4\n");
        pdf.extend(b"0000000000 65535 f\r\n");
        for off in &offsets {
            pdf.extend(format!("{:010} 00000 n\r\n", off).as_bytes());
        }
        pdf.extend(b"trailer\n<< /Size 4 /Root 1 0 R >>\n");
        pdf.extend(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

        pdf
    }

    #[test]
    fn page_boxes_full_set() {
        let pdf = build_pdf_with_page_boxes();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        let pb = doc.page_boxes(0).unwrap();
        assert_eq!(pb.media_box, [0.0, 0.0, 612.0, 792.0]);
        assert_eq!(pb.crop_box, Some([10.0, 10.0, 602.0, 782.0]));
        assert_eq!(pb.bleed_box, Some([5.0, 5.0, 607.0, 787.0]));
        assert_eq!(pb.trim_box, Some([20.0, 20.0, 592.0, 772.0]));
        assert_eq!(pb.art_box, Some([30.0, 30.0, 582.0, 762.0]));
        assert_eq!(pb.rotate, 90);
        assert_eq!(pb.user_unit, 1.5);
        assert_eq!(pb.duration, Some(5.0));
        assert!(pb.has_transition);
        assert!(pb.has_additional_actions);
    }

    #[test]
    fn page_boxes_minimal_defaults() {
        let pdf = build_minimal_pdf();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        let pb = doc.page_boxes(0).unwrap();
        assert_eq!(pb.media_box, [0.0, 0.0, 612.0, 792.0]);
        // No CropBox / BleedBox / TrimBox / ArtBox declared.
        assert!(pb.crop_box.is_none());
        assert!(pb.bleed_box.is_none());
        assert!(pb.trim_box.is_none());
        assert!(pb.art_box.is_none());
        assert_eq!(pb.rotate, 0);
        assert_eq!(pb.user_unit, 1.0);
        assert!(pb.duration.is_none());
        assert!(!pb.has_transition);
        assert!(!pb.has_additional_actions);
    }

    #[test]
    fn page_boxes_out_of_range() {
        let pdf = build_minimal_pdf();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        assert!(doc.page_boxes(99).is_err());
    }

    /// Build a PDF carrying one embedded file via the catalog's
    /// /Names /EmbeddedFiles name tree. The attached "data.csv" is
    /// stored uncompressed so we can round-trip its bytes through
    /// embedded_file_bytes.
    fn build_pdf_with_embedded_file() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend(b"%PDF-1.4\n");

        let mut offsets: Vec<usize> = Vec::new();
        let mut push_obj = |buf: &mut Vec<u8>, body: &[u8]| {
            offsets.push(buf.len());
            buf.extend(body);
        };

        // 1: Catalog → Names dict at obj 4
        push_obj(
            &mut pdf,
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Names 4 0 R >>\nendobj\n",
        );
        // 2: Pages
        push_obj(
            &mut pdf,
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        );
        // 3: Page
        push_obj(
            &mut pdf,
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        // 4: /Names dict pointing at /EmbeddedFiles tree at obj 5
        push_obj(&mut pdf, b"4 0 obj\n<< /EmbeddedFiles 5 0 R >>\nendobj\n");
        // 5: name-tree leaf, single entry "data.csv" → filespec at obj 6
        push_obj(
            &mut pdf,
            b"5 0 obj\n<< /Names [(data.csv) 6 0 R] >>\nendobj\n",
        );
        // 6: filespec dict
        push_obj(
            &mut pdf,
            b"6 0 obj\n<< /Type /Filespec /F (data.csv) /UF (data.csv) \
              /Desc (Sample CSV) /AFRelationship /Data \
              /EF << /F 7 0 R /UF 7 0 R >> >>\nendobj\n",
        );
        // 7: embedded-file stream — uncompressed payload "id,name\n1,a\n"
        // (12 bytes). /Length 12.
        let payload = b"id,name\n1,a\n";
        let stream_header = b"7 0 obj\n<< /Type /EmbeddedFile /Subtype /text#2Fcsv \
            /Length 12 /Params << /Size 12 >> >>\nstream\n";
        offsets.push(pdf.len());
        pdf.extend(stream_header);
        pdf.extend(payload);
        pdf.extend(b"\nendstream\nendobj\n");

        let xref_offset = pdf.len();
        pdf.extend(b"xref\n0 8\n");
        pdf.extend(b"0000000000 65535 f\r\n");
        for off in &offsets {
            pdf.extend(format!("{:010} 00000 n\r\n", off).as_bytes());
        }
        pdf.extend(b"trailer\n<< /Size 8 /Root 1 0 R >>\n");
        pdf.extend(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

        pdf
    }

    #[test]
    fn embedded_files_basic() {
        let pdf = build_pdf_with_embedded_file();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        let map = doc.embedded_files();
        assert_eq!(map.len(), 1);

        let ef = map.get("data.csv").expect("data.csv missing");
        assert_eq!(ef.name, "data.csv");
        assert_eq!(ef.filename.as_deref(), Some("data.csv"));
        assert_eq!(ef.unicode_filename.as_deref(), Some("data.csv"));
        assert_eq!(ef.description.as_deref(), Some("Sample CSV"));
        assert_eq!(ef.relationship, Some(crate::AfRelationship::Data));
        assert_eq!(ef.mime_type.as_deref(), Some("text/csv"));
        assert_eq!(ef.size, Some(12));

        let bytes = doc.embedded_file_bytes("data.csv").unwrap();
        assert_eq!(&bytes[..], b"id,name\n1,a\n");
    }

    #[test]
    fn embedded_files_caches_across_calls() {
        let pdf = build_pdf_with_embedded_file();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        let a = doc.embedded_files();
        let b = doc.embedded_files();
        assert!(std::ptr::eq(a, b), "embedded_files() must be cached");
    }

    /// Build a PDF whose outline tree has a cycle: outline node 5
    /// references itself as its own /Next sibling.
    fn build_pdf_with_cyclic_outline() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend(b"%PDF-1.4\n");

        let mut offsets: Vec<usize> = Vec::new();
        let mut push_obj = |buf: &mut Vec<u8>, body: &[u8]| {
            offsets.push(buf.len());
            buf.extend(body);
        };

        push_obj(
            &mut pdf,
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>\nendobj\n",
        );
        push_obj(
            &mut pdf,
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        );
        push_obj(
            &mut pdf,
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        push_obj(
            &mut pdf,
            b"4 0 obj\n<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>\nendobj\n",
        );
        // 5: cyclic — /Next points back at itself.
        push_obj(
            &mut pdf,
            b"5 0 obj\n<< /Title (Loop) /Parent 4 0 R /Next 5 0 R >>\nendobj\n",
        );

        let xref_offset = pdf.len();
        pdf.extend(b"xref\n0 6\n");
        pdf.extend(b"0000000000 65535 f\r\n");
        for off in &offsets {
            pdf.extend(format!("{:010} 00000 n\r\n", off).as_bytes());
        }
        pdf.extend(b"trailer\n<< /Size 6 /Root 1 0 R >>\n");
        pdf.extend(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

        pdf
    }

    #[test]
    fn warning_emitted_for_outline_cycle() {
        let pdf = build_pdf_with_cyclic_outline();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        // Trigger outline parse.
        let outline = doc.outline();
        // The single Loop entry parses; the cycle stops further siblings.
        assert_eq!(outline.len(), 1);
        let warnings = doc.parse_warnings();
        assert!(
            warnings
                .iter()
                .any(|w| matches!(w.phase, crate::ParsePhase::Outline)
                    && w.severity == crate::Severity::Warning
                    && w.message.contains("cycle")),
            "expected outline cycle warning, got: {:?}",
            warnings.iter().collect::<Vec<_>>()
        );
    }

    /// Build a PDF where the page's /Annots array references an
    /// annotation dict that has /Subtype but no /Rect.
    fn build_pdf_with_rectless_annot() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend(b"%PDF-1.4\n");

        let mut offsets: Vec<usize> = Vec::new();
        let mut push_obj = |buf: &mut Vec<u8>, body: &[u8]| {
            offsets.push(buf.len());
            buf.extend(body);
        };

        push_obj(
            &mut pdf,
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        );
        push_obj(
            &mut pdf,
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        );
        push_obj(
            &mut pdf,
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Annots [4 0 R] >>\nendobj\n",
        );
        // Annotation with /Subtype but missing /Rect.
        push_obj(
            &mut pdf,
            b"4 0 obj\n<< /Type /Annot /Subtype /Text /Contents (no rect) >>\nendobj\n",
        );

        let xref_offset = pdf.len();
        pdf.extend(b"xref\n0 5\n");
        pdf.extend(b"0000000000 65535 f\r\n");
        for off in &offsets {
            pdf.extend(format!("{:010} 00000 n\r\n", off).as_bytes());
        }
        pdf.extend(b"trailer\n<< /Size 5 /Root 1 0 R >>\n");
        pdf.extend(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

        pdf
    }

    #[test]
    fn warning_emitted_for_rectless_annotation() {
        let pdf = build_pdf_with_rectless_annot();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        let annots = doc.page_annotations(0).unwrap();
        // The /Rect-less annotation is skipped.
        assert_eq!(annots.len(), 0);
        let warnings = doc.parse_warnings();
        assert!(
            warnings.iter().any(
                |w| matches!(w.phase, crate::ParsePhase::Annotations { page: 0 })
                    && w.message.contains("/Rect")
            ),
            "expected /Rect warning, got: {:?}",
            warnings.iter().collect::<Vec<_>>()
        );
    }

    #[test]
    fn parse_warnings_empty_for_clean_document() {
        let pdf = build_minimal_pdf();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        // Touch every accessor; nothing should warn for a clean doc.
        let _ = doc.metadata();
        let _ = doc.viewer_preferences();
        let _ = doc.outline();
        let _ = doc.destinations();
        let _ = doc.page_annotations(0).unwrap();
        let _ = doc.form();
        let _ = doc.embedded_files();
        let _ = doc.page_boxes(0).unwrap();
        let warnings = doc.parse_warnings();
        assert_eq!(
            warnings.len(),
            0,
            "got: {:?}",
            warnings.iter().collect::<Vec<_>>()
        );
    }

    #[test]
    fn embedded_files_empty_when_absent() {
        let pdf = build_minimal_pdf();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        assert!(doc.embedded_files().is_empty());
        assert!(doc.embedded_file_bytes("missing").is_err());
    }

    #[test]
    fn form_widgets_appear_in_page_annotations() {
        let pdf = build_pdf_with_form();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        let form = doc.form().unwrap();
        let annots = doc.page_annotations(0).unwrap();

        // Every widget obj_num declared by a terminal field should
        // resolve to a Widget annotation on the page. Use the existing
        // PageInfo.annots ordering: pages are matched by obj_num.
        let widget_annot_subtypes: Vec<_> = annots
            .iter()
            .filter(|a| a.kind == crate::AnnotationKind::Widget)
            .collect();
        assert!(
            !widget_annot_subtypes.is_empty(),
            "expected widget annotations on page"
        );

        // The radio "color" field declares 2 widgets; assert both are
        // in the page's annotation set (we look up by inspecting the
        // page's annot ref obj_nums; PageInfo.annots is ordered, so
        // we just count).
        let color_field = form.fields.iter().find(|f| f.name == "color").unwrap();
        assert_eq!(color_field.widget_obj_nums.len(), 2);
    }

    #[test]
    fn resolve_named_destination_returns_dest() {
        let pdf = build_pdf_with_legacy_dests();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        let d = doc.resolve_named_destination("Intro").unwrap();
        match d {
            crate::Destination::PageView { page, view } => {
                assert_eq!(page, Some(0));
                assert_eq!(view, crate::ViewSpec::Fit);
            }
            _ => panic!("expected PageView"),
        }
        assert!(doc.resolve_named_destination("MissingName").is_none());
    }

    /// Build a PDF with five OCGs that together exercise every Phase 1
    /// metadata path:
    ///
    /// - Object 5: minimal OCG with a PDFDocEncoding `/Name`.
    /// - Object 6: OCG whose name is UTF-16BE with BOM, with an array
    ///   `/Intent` of two values, locked, and full `/Usage` sub-dict
    ///   covering View/Print/Export/Zoom/Language/User/PageElement/CreatorInfo.
    /// - Object 7: OCG with single-name array `/Intent`
    ///   (`[/Design]`) — should collapse to `LayerIntent::Design`.
    /// - Object 8: OCG with `/Intent /Custom` — `LayerIntent::Other`.
    /// - Object 9: OCG default-OFF (listed in `/D /OFF`) and
    ///   `/CreatorInfo` directly on the OCG dict.
    fn build_pdf_with_layers() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend(b"%PDF-1.6\n");

        let mut offsets: Vec<usize> = Vec::new();
        let mut push_obj = |buf: &mut Vec<u8>, body: &[u8]| {
            offsets.push(buf.len());
            buf.extend(body);
        };

        // 1: Catalog with /OCProperties
        push_obj(
            &mut pdf,
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /OCProperties << \
              /OCGs [5 0 R 6 0 R 7 0 R 8 0 R 9 0 R] \
              /D << /Order [5 0 R 6 0 R 7 0 R 8 0 R 9 0 R] \
                    /OFF [9 0 R] /Locked [6 0 R] >> \
              >> >>\nendobj\n",
        );
        // 2: Pages
        push_obj(
            &mut pdf,
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        );
        // 3: Page
        push_obj(
            &mut pdf,
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        // 4: (placeholder so OCG numbering matches the doc-comment)
        push_obj(&mut pdf, b"4 0 obj\nnull\nendobj\n");

        // 5: simplest OCG — only /Type and /Name (PDFDocEncoding ASCII).
        push_obj(
            &mut pdf,
            b"5 0 obj\n<< /Type /OCG /Name (Background) >>\nendobj\n",
        );

        // 6: OCG with UTF-16BE name (FE FF "T" "e" "s" "t") plus full
        // /Usage and array /Intent.
        let mut obj6 = Vec::new();
        obj6.extend(b"6 0 obj\n<< /Type /OCG ");
        obj6.extend(b"/Name <FEFF005400650073007400200394> ");
        // Array /Intent with two distinct names.
        obj6.extend(b"/Intent [/View /Design] ");
        // /Usage — every sub-dict.
        obj6.extend(b"/Usage << ");
        obj6.extend(b"/View << /ViewState /ON >> ");
        obj6.extend(b"/Print << /PrintState /OFF /Subtype /Watermark >> ");
        obj6.extend(b"/Export << /ExportState /ON >> ");
        obj6.extend(b"/Zoom << /min 0.5 /max 4.0 >> ");
        obj6.extend(b"/Language << /Lang (en-US) /Preferred /ON >> ");
        obj6.extend(b"/User << /Type /Ind /Name (alice) >> ");
        obj6.extend(b"/PageElement << /Subtype /HF >> ");
        obj6.extend(b"/CreatorInfo << /Creator (CADtool) /Subtype /Technical >> ");
        obj6.extend(b">> ");
        obj6.extend(b">>\nendobj\n");
        push_obj(&mut pdf, &obj6);

        // 7: single-element array /Intent → should collapse to Design.
        push_obj(
            &mut pdf,
            b"7 0 obj\n<< /Type /OCG /Name (DesignLayer) /Intent [/Design] >>\nendobj\n",
        );

        // 8: unknown intent name → LayerIntent::Other.
        push_obj(
            &mut pdf,
            b"8 0 obj\n<< /Type /OCG /Name (Custom) /Intent /Custom >>\nendobj\n",
        );

        // 9: default-OFF, /CreatorInfo on the OCG itself, /User array.
        push_obj(
            &mut pdf,
            b"9 0 obj\n<< /Type /OCG /Name (HiddenLayer) \
              /CreatorInfo << /Creator (Inkscape) /Subtype /Artwork >> \
              /Usage << /User << /Type /Org /Name [(group-a) (group-b)] >> >> \
              >>\nendobj\n",
        );

        let xref_offset = pdf.len();
        pdf.extend(b"xref\n0 10\n");
        pdf.extend(b"0000000000 65535 f\r\n");
        for off in &offsets {
            pdf.extend(format!("{:010} 00000 n\r\n", off).as_bytes());
        }
        pdf.extend(b"trailer\n<< /Size 10 /Root 1 0 R >>\n");
        pdf.extend(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
        pdf
    }

    #[test]
    fn layers_basic_enumeration() {
        let pdf = build_pdf_with_layers();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        let layers = doc.layers();
        assert_eq!(layers.len(), 5, "expected 5 OCGs, got {}", layers.len());

        // Default-OFF set: only object 9 is in /D /OFF.
        assert!(
            layers
                .iter()
                .find(|l| l.ocg_id == 5)
                .unwrap()
                .default_visible
        );
        assert!(
            layers
                .iter()
                .find(|l| l.ocg_id == 6)
                .unwrap()
                .default_visible
        );
        assert!(
            !layers
                .iter()
                .find(|l| l.ocg_id == 9)
                .unwrap()
                .default_visible
        );

        // Locked set: only object 6 is in /D /Locked.
        assert!(layers.iter().find(|l| l.ocg_id == 6).unwrap().locked);
        assert!(!layers.iter().find(|l| l.ocg_id == 5).unwrap().locked);
        assert!(!layers.iter().find(|l| l.ocg_id == 9).unwrap().locked);
    }

    #[test]
    fn layers_name_decoding() {
        let pdf = build_pdf_with_layers();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();

        // Object 5: PDFDocEncoding ASCII → "Background".
        let bg = doc.layer(5).unwrap();
        assert_eq!(bg.name, "Background");

        // Object 6: UTF-16BE BOM + "Test " + GREEK CAPITAL LETTER DELTA (U+0394).
        let utf16 = doc.layer(6).unwrap();
        assert_eq!(utf16.name, "Test \u{0394}");
    }

    #[test]
    fn layers_intent_variants() {
        let pdf = build_pdf_with_layers();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();

        // No /Intent → default View.
        assert_eq!(doc.layer(5).unwrap().intent, LayerIntent::View);

        // Two-element array /Intent → Multiple.
        match &doc.layer(6).unwrap().intent {
            LayerIntent::Multiple(names) => {
                assert_eq!(names.len(), 2);
                assert_eq!(names[0], "View");
                assert_eq!(names[1], "Design");
            }
            other => panic!("expected Multiple, got {other:?}"),
        }

        // Single-element array /Intent → collapses to Design.
        assert_eq!(doc.layer(7).unwrap().intent, LayerIntent::Design);

        // Unknown name /Intent → Other.
        match &doc.layer(8).unwrap().intent {
            LayerIntent::Other(s) => assert_eq!(s, "Custom"),
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[test]
    fn layers_full_usage_dict() {
        let pdf = build_pdf_with_layers();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        let l = doc.layer(6).unwrap();

        // /View
        let view = l.usage.view.expect("view sub-dict");
        assert_eq!(view.state, UsageState::On);

        // /Print with subtype
        let print = l.usage.print.as_ref().expect("print sub-dict");
        assert_eq!(print.state, UsageState::Off);
        assert_eq!(print.subtype.as_deref(), Some("Watermark"));

        // /Export
        let export = l.usage.export.expect("export sub-dict");
        assert_eq!(export.state, UsageState::On);

        // /Zoom
        let zoom = l.usage.zoom.expect("zoom sub-dict");
        assert_eq!(zoom.min, Some(0.5));
        assert_eq!(zoom.max, Some(4.0));

        // /Language
        let lang = l.usage.language.as_ref().expect("language sub-dict");
        assert_eq!(lang.lang, "en-US");
        assert!(lang.preferred);

        // /User (single string form)
        let user = l.usage.user.as_ref().expect("user sub-dict");
        assert_eq!(user.user_type.as_deref(), Some("Ind"));
        assert_eq!(user.names, vec!["alice".to_string()]);

        // /PageElement
        assert_eq!(l.usage.page_element, Some(PageElementSubtype::HeaderFooter));

        // /CreatorInfo nested under /Usage
        let ci = l.usage.creator_info.as_ref().expect("creator_info");
        assert_eq!(ci.creator, "CADtool");
        assert_eq!(ci.subtype.as_deref(), Some("Technical"));
    }

    #[test]
    fn layers_creator_info_on_ocg() {
        let pdf = build_pdf_with_layers();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        let hidden = doc.layer(9).unwrap();

        // /CreatorInfo on the OCG itself.
        let ci = hidden.creator_info.as_ref().expect("creator_info");
        assert_eq!(ci.creator, "Inkscape");
        assert_eq!(ci.subtype.as_deref(), Some("Artwork"));

        // /User /Name as an array of strings.
        let user = hidden.usage.user.as_ref().expect("user sub-dict");
        assert_eq!(user.user_type.as_deref(), Some("Org"));
        assert_eq!(
            user.names,
            vec!["group-a".to_string(), "group-b".to_string()]
        );
    }

    #[test]
    fn layers_empty_when_no_oc_properties() {
        let pdf = build_minimal_pdf();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        assert!(doc.layers().is_empty());
        assert!(doc.layer(42).is_none());
    }

    #[test]
    fn layers_caches_across_calls() {
        let pdf = build_pdf_with_layers();
        let doc = PdfDocument::from_bytes(&pdf).unwrap();
        let first = doc.layers().as_ptr();
        let second = doc.layers().as_ptr();
        assert_eq!(first, second, "layers() should return a cached slice");
    }

    /// Build a minimal valid PDF for testing.
    fn build_minimal_pdf() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend(b"%PDF-1.4\n");

        // Object 1: Catalog
        let obj1_offset = pdf.len();
        pdf.extend(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        // Object 2: Pages
        let obj2_offset = pdf.len();
        pdf.extend(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        // Object 3: Page
        let obj3_offset = pdf.len();
        pdf.extend(b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n");

        // Xref
        let xref_offset = pdf.len();
        pdf.extend(b"xref\n0 4\n");
        pdf.extend(b"0000000000 65535 f\r\n");
        pdf.extend(format!("{:010} 00000 n\r\n", obj1_offset).as_bytes());
        pdf.extend(format!("{:010} 00000 n\r\n", obj2_offset).as_bytes());
        pdf.extend(format!("{:010} 00000 n\r\n", obj3_offset).as_bytes());
        pdf.extend(b"trailer\n<< /Size 4 /Root 1 0 R >>\n");
        pdf.extend(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

        pdf
    }
}
