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
pub mod error;
pub mod filters;
pub mod lexer;
pub mod metadata;
pub mod name_tree;
pub mod objects;
pub mod outline;
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
pub use error::PdfError;
pub use metadata::{DocumentMetadata, PdfDate, TrappedFlag};
pub use objects::{PdfDict, PdfObj};
pub use outline::{OutlineItem, OutlineStyle};
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
    /// chains, and pathological depth are tolerated by hard caps.
    pub fn outline(&self) -> &[OutlineItem] {
        self.outline_cache
            .get_or_init(|| outline::parse_outline_tree(&self.resolver, &self.pages))
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
        let annots = cell
            .get_or_init(|| annotations::parse_page_annotations(&self.resolver, &self.pages, page));
        Ok(annots.as_slice())
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
