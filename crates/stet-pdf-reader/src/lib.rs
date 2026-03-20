// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF parser, page navigator, and content stream interpreter.
//!
//! Phase A: structural parsing — open a PDF, navigate its object graph,
//! find pages, and extract raw content streams.
//!
//! Phase B: content stream interpretation — convert PDF page content into
//! DisplayList elements for rendering through the SkiaDevice pipeline.

pub mod content;
pub mod crypto;
pub mod error;
pub mod filters;
pub mod lexer;
pub mod objects;
pub mod page_tree;
pub mod resolver;
pub mod resources;
pub mod xref;

pub use error::PdfError;
pub use objects::{PdfDict, PdfObj};
pub use page_tree::PageInfo;

use content::ContentInterpreter;
use resolver::Resolver;
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
}

impl<'a> PdfDocument<'a> {
    /// Parse a PDF from bytes.
    pub fn from_bytes(data: &'a [u8]) -> Result<Self, PdfError> {
        // Validate header
        if !data.starts_with(b"%PDF-") {
            return Err(PdfError::NotAPdf);
        }

        // Check for encryption (we'll detect it after parsing the trailer)
        let xref = xref::parse_xref(data)?;

        // Handle encryption: try to open with empty password.
        // /Encrypt null means no encryption (some generators emit this).
        let encryption = if let Some(encrypt_ref) = xref.trailer.get(b"Encrypt") {
            if matches!(encrypt_ref, crate::objects::PdfObj::Null) {
                None
            } else {
                // Use a temporary resolver (without encryption) to dereference the Encrypt dict
                let temp_resolver = Resolver::new(data, &xref);
                let encrypt_obj = temp_resolver.deref(encrypt_ref)?;
                let encrypt_dict = encrypt_obj
                    .as_dict()
                    .ok_or(PdfError::Other("Encrypt is not a dict".into()))?;

                // Get file ID from trailer
                let file_id = xref
                    .trailer
                    .get_array(b"ID")
                    .and_then(|arr| arr.first()?.as_str().map(|s| s.to_vec()))
                    .unwrap_or_default();

                Some(crypto::EncryptionState::try_open(
                    encrypt_dict,
                    &xref.trailer,
                    &file_id,
                )?)
            }
        } else {
            None
        };

        let resolver = Resolver::with_encryption(data, xref, encryption);
        let pages = page_tree::collect_pages(&resolver)?;

        let mut icc_cache = IccCache::new();
        icc_cache.search_system_cmyk_profile();

        Ok(Self {
            resolver,
            pages,
            icc_cache,
            font_provider: None,
            overprint: false,
        })
    }

    /// Parse a PDF from bytes, using a pre-loaded ICC cache.
    ///
    /// Use this when the caller already has an `IccCache` with the system
    /// CMYK profile loaded (e.g., from the PostScript interpreter context).
    pub fn from_bytes_with_icc(data: &'a [u8], icc_cache: IccCache) -> Result<Self, PdfError> {
        if !data.starts_with(b"%PDF-") {
            return Err(PdfError::NotAPdf);
        }

        let xref = xref::parse_xref(data)?;

        let encryption = if let Some(encrypt_ref) = xref.trailer.get(b"Encrypt") {
            if matches!(encrypt_ref, crate::objects::PdfObj::Null) {
                None
            } else {
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

                Some(crypto::EncryptionState::try_open(
                    encrypt_dict,
                    &xref.trailer,
                    &file_id,
                )?)
            }
        } else {
            None
        };

        let resolver = Resolver::with_encryption(data, xref, encryption);
        let pages = page_tree::collect_pages(&resolver)?;

        Ok(Self {
            resolver,
            pages,
            icc_cache,
            font_provider: None,
            overprint: false,
        })
    }

    /// Enable or disable PDF overprint simulation.
    ///
    /// When false (default), OP/op flags in graphics state dicts are ignored,
    /// avoiding expensive CMYK buffer tracking. Set to true for prepress accuracy.
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
        );

        // Check if the page has a DeviceCMYK transparency group — if so,
        // RGB colors need round-tripping through CMYK to match compositing
        // in CMYK space (mutes saturated out-of-gamut RGB colors).
        if let Ok(page_obj) = self.resolver.resolve(info.obj_num, 0)
            && let Some(page_dict) = page_obj.as_dict()
            && let Some(group_obj) = page_dict.get(b"Group")
            && let Ok(group_resolved) = self.resolver.deref(group_obj)
            && let Some(group_dict) = group_resolved.as_dict()
            && group_dict.get_name(b"CS") == Some(b"DeviceCMYK")
        {
            interpreter.set_page_group_cmyk();
        }

        // Render page content
        if let Err(e) = interpreter.interpret_stream_public(&content_data) {
            eprintln!("warning: content stream error: {}", e);
        }

        // Render annotation appearance streams (form field values, stamps, etc.)
        if !info.annots.is_empty() {
            interpreter.reset_clip_for_annotations();
            for &(n, g) in &info.annots {
                let _ = interpreter.render_annotation(n, g);
            }
        }

        Ok(interpreter.into_display_list())
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

    /// Access the resolver for arbitrary object lookups.
    pub fn resolver(&self) -> &Resolver<'a> {
        &self.resolver
    }

    /// Access page info list.
    pub fn pages(&self) -> &[PageInfo] {
        &self.pages
    }
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
                }
            }
        }

        let data = std::fs::read("../../pdf_samples/PDFX-ready_Output-Test_X4.pdf").unwrap();
        let doc = PdfDocument::from_bytes(&data).unwrap();
        let dl = doc.render_page(0, 72.0).unwrap();
        eprintln!("=== Display list: {} top-level elements ===", dl.len());
        dump(&dl, 0);
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
