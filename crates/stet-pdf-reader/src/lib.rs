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
use stet_core::display_list::DisplayList;
use stet_core::graphics_state::Matrix;
use stet_core::icc::IccCache;

/// A parsed PDF document.
pub struct PdfDocument<'a> {
    resolver: Resolver<'a>,
    pages: Vec<PageInfo>,
    icc_cache: IccCache,
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

        // Handle encryption: try to open with empty password
        let encryption = if let Some(encrypt_ref) = xref.trailer.get(b"Encrypt") {
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
        } else {
            None
        };

        let resolver = Resolver::with_encryption(data, xref, encryption);
        let pages = page_tree::collect_pages(&resolver)?;

        Ok(Self {
            resolver,
            pages,
            icc_cache,
        })
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
            if i > 0 {
                result.push(b'\n');
            }
            let data = self.resolver.stream_data(obj_num, gen_num)?;
            result.extend_from_slice(&data);
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
                // Rotate 90° CW: (x,y) → (y, page_w - x)
                Matrix::new(0.0, -scale, -scale, 0.0, page_h * scale, page_w * scale)
                    .concat(&Matrix::translate(-llx, -lly))
            }
            180 => {
                // Rotate 180° + Y-flip = just X-flip
                Matrix::new(-scale, 0.0, 0.0, scale, page_w * scale, 0.0)
                    .concat(&Matrix::translate(-llx, -lly))
            }
            270 => {
                // Rotate 270° CW + Y-flip
                Matrix::new(0.0, scale, scale, 0.0, 0.0, 0.0).concat(&Matrix::translate(-llx, -lly))
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
        let interpreter =
            ContentInterpreter::new(&self.resolver, info.resources.clone(), ctm, &self.icc_cache);
        interpreter.interpret(&content_data)
    }

    /// Render a page to RGBA pixel data at the given DPI.
    ///
    /// Returns (pixel_data, width, height). Pixel data is RGBA, 4 bytes per pixel.
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
        );

        Ok((rgba, pixel_w, pixel_h))
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
