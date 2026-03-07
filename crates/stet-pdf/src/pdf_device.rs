// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! PDF output device — accumulates pages and writes a PDF file on finish().

use stet_core::device::{ClipParams, FillParams, ImageParams, OutputDevice, StrokeParams};
use stet_core::display_list::DisplayList;
use stet_core::graphics_state::PsPath;

use crate::content_stream::{self, ContentStreamResult, ShadingRef};
use crate::font_tracker::FontTracker;
use crate::image_ops::ImageXObject;
use crate::pdf_objects::PdfObj;
use crate::pdf_writer::PdfWriter;
use crate::shading_ops;

/// A single page's data, ready for PDF assembly.
struct PageData {
    content: Vec<u8>,
    width_pts: f64,
    height_pts: f64,
    images: Vec<ImageXObject>,
    shading_refs: Vec<ShadingRef>,
    font_tracker: FontTracker,
}

/// PDF output device. Accumulates display lists per page and generates
/// a single PDF file containing all pages on `finish()`.
pub struct PdfDevice {
    pages: Vec<PageData>,
    page_w: u32,
    page_h: u32,
    dpi: f64,
    output_path: Option<String>,
}

impl PdfDevice {
    /// Create a new PDF device with the given page dimensions and DPI.
    pub fn new(width: u32, height: u32, dpi: f64) -> Self {
        Self {
            pages: Vec::new(),
            page_w: width,
            page_h: height,
            dpi,
            output_path: None,
        }
    }

    /// Assemble all accumulated pages into a PDF and write to the output file.
    fn write_pdf(&self) -> Result<(), String> {
        let path = self.output_path.as_deref().ok_or("no output path set")?;

        let mut writer = PdfWriter::new();

        // Pre-allocate catalog and pages objects
        let catalog_ref = writer.alloc_obj();
        let pages_ref = writer.alloc_obj();

        let mut page_refs = Vec::new();

        for page in &self.pages {
            let page_ref = self.build_page(&mut writer, page, pages_ref)?;
            page_refs.push(page_ref);
        }

        // Pages object
        writer.set_object(
            pages_ref,
            &PdfObj::Dict(vec![
                (b"Type".to_vec(), PdfObj::name("Pages")),
                (
                    b"Kids".to_vec(),
                    PdfObj::Array(page_refs.iter().map(|&r| PdfObj::Ref(r)).collect()),
                ),
                (b"Count".to_vec(), PdfObj::Int(page_refs.len() as i64)),
            ]),
        );

        // Catalog
        writer.set_object(
            catalog_ref,
            &PdfObj::Dict(vec![
                (b"Type".to_vec(), PdfObj::name("Catalog")),
                (b"Pages".to_vec(), PdfObj::Ref(pages_ref)),
            ]),
        );

        // Write to file
        let file = std::fs::File::create(path).map_err(|e| format!("create {}: {}", path, e))?;
        let mut bw = std::io::BufWriter::new(file);
        writer
            .write_pdf(&mut bw, catalog_ref)
            .map_err(|e| format!("write {}: {}", path, e))?;

        eprintln!("PDF written: {} ({} pages)", path, self.pages.len());
        Ok(())
    }

    /// Build PDF objects for a single page. Returns the page object number.
    fn build_page(
        &self,
        writer: &mut PdfWriter,
        page: &PageData,
        pages_ref: u32,
    ) -> Result<u32, String> {
        // Build image XObjects
        let mut xobject_entries: Vec<(Vec<u8>, PdfObj)> = Vec::new();
        for (i, img) in page.images.iter().enumerate() {
            let img_ref = self.build_image_xobject(writer, img);
            xobject_entries.push((format!("Im{}", i).into_bytes(), PdfObj::Ref(img_ref)));
        }

        // Build shading objects
        let mut shading_entries: Vec<(Vec<u8>, PdfObj)> = Vec::new();
        for (i, sh_ref) in page.shading_refs.iter().enumerate() {
            let sh_obj = match sh_ref {
                ShadingRef::Axial(p) => shading_ops::build_axial_shading(writer, p),
                ShadingRef::Radial(p) => shading_ops::build_radial_shading(writer, p),
                ShadingRef::Mesh(p) => shading_ops::build_mesh_shading(writer, p),
                ShadingRef::Patch(p) => shading_ops::build_patch_shading(writer, p),
            };
            shading_entries.push((format!("Sh{}", i).into_bytes(), PdfObj::Ref(sh_obj)));
        }

        // Build font resources
        let mut font_entries: Vec<(Vec<u8>, PdfObj)> = Vec::new();
        for usage in page.font_tracker.fonts() {
            let font_ref = self.build_font_reference(writer, usage);
            font_entries.push((usage.pdf_name.clone().into_bytes(), PdfObj::Ref(font_ref)));
        }

        // Resources dict
        let mut resources: Vec<(Vec<u8>, PdfObj)> = Vec::new();
        if !font_entries.is_empty() {
            resources.push((b"Font".to_vec(), PdfObj::Dict(font_entries)));
        }
        if !xobject_entries.is_empty() {
            resources.push((b"XObject".to_vec(), PdfObj::Dict(xobject_entries)));
        }
        if !shading_entries.is_empty() {
            resources.push((b"Shading".to_vec(), PdfObj::Dict(shading_entries)));
        }

        // Content stream
        let content_ref = writer.add_stream(Vec::new(), &page.content, true);

        // Page object
        let page_ref = writer.add_object(&PdfObj::Dict(vec![
            (b"Type".to_vec(), PdfObj::name("Page")),
            (b"Parent".to_vec(), PdfObj::Ref(pages_ref)),
            (
                b"MediaBox".to_vec(),
                PdfObj::Array(vec![
                    PdfObj::Int(0),
                    PdfObj::Int(0),
                    PdfObj::Real(page.width_pts),
                    PdfObj::Real(page.height_pts),
                ]),
            ),
            (b"Contents".to_vec(), PdfObj::Ref(content_ref)),
            (b"Resources".to_vec(), PdfObj::Dict(resources)),
        ]));

        Ok(page_ref)
    }

    /// Build a PDF font reference for a tracked font.
    ///
    /// For Standard 14 fonts, creates a simple Type1 font dict.
    /// For other fonts, also creates a simple Type1 dict (no embedding yet).
    /// Both include a ToUnicode CMap for searchability.
    fn build_font_reference(
        &self,
        writer: &mut PdfWriter,
        usage: &crate::font_tracker::FontUsage,
    ) -> u32 {
        // Build ToUnicode CMap
        let tounicode_ref = self.build_tounicode_cmap(writer, usage);

        let mut entries: Vec<(Vec<u8>, PdfObj)> = vec![
            (b"Type".to_vec(), PdfObj::name("Font")),
            (b"Subtype".to_vec(), PdfObj::name("Type1")),
            (
                b"BaseFont".to_vec(),
                PdfObj::Name(usage.font_name.clone()),
            ),
        ];

        if !usage.is_standard_14 {
            // For non-standard fonts, add Encoding
            entries.push((
                b"Encoding".to_vec(),
                PdfObj::name("WinAnsiEncoding"),
            ));
        }

        if let Some(tu_ref) = tounicode_ref {
            entries.push((b"ToUnicode".to_vec(), PdfObj::Ref(tu_ref)));
        }

        writer.add_object(&PdfObj::Dict(entries))
    }

    /// Build a ToUnicode CMap for a font.
    ///
    /// Maps character codes to Unicode based on common Adobe glyph naming.
    /// For printable ASCII codes, maps code→Unicode directly (works for most
    /// Latin text fonts). The full glyph-name-based mapping requires encoding
    /// array access (deferred to font embedding phase).
    fn build_tounicode_cmap(
        &self,
        writer: &mut PdfWriter,
        usage: &crate::font_tracker::FontUsage,
    ) -> Option<u32> {
        use std::collections::HashMap;

        let mut map: HashMap<u16, u16> = HashMap::new();

        for &code in &usage.used_codes {
            if code <= 255 {
                // For ASCII range, assume code = Unicode (works for standard encodings)
                if (0x20..=0x7E).contains(&code) {
                    map.insert(code, code);
                }
            }
        }

        if map.is_empty() {
            return None;
        }

        let font_name = String::from_utf8_lossy(&usage.font_name);
        let cmap_data = generate_tounicode_cmap(&map, &font_name);
        Some(writer.add_stream(Vec::new(), &cmap_data, true))
    }

    /// Build a PDF image XObject from prepared image data. Returns the object number.
    fn build_image_xobject(&self, writer: &mut PdfWriter, img: &ImageXObject) -> u32 {
        // Build SMask if present
        let smask_ref = img.smask_data.as_ref().map(|smask_data| {
            writer.add_stream(
                vec![
                    (b"Type".to_vec(), PdfObj::name("XObject")),
                    (b"Subtype".to_vec(), PdfObj::name("Image")),
                    (b"Width".to_vec(), PdfObj::Int(img.width as i64)),
                    (b"Height".to_vec(), PdfObj::Int(img.height as i64)),
                    (b"ColorSpace".to_vec(), PdfObj::name("DeviceGray")),
                    (b"BitsPerComponent".to_vec(), PdfObj::Int(8)),
                ],
                smask_data,
                true,
            )
        });

        // Main image XObject
        let mut entries = vec![
            (b"Type".to_vec(), PdfObj::name("XObject")),
            (b"Subtype".to_vec(), PdfObj::name("Image")),
            (b"Width".to_vec(), PdfObj::Int(img.width as i64)),
            (b"Height".to_vec(), PdfObj::Int(img.height as i64)),
            (b"ColorSpace".to_vec(), PdfObj::name("DeviceRGB")),
            (b"BitsPerComponent".to_vec(), PdfObj::Int(8)),
        ];

        if let Some(smask) = smask_ref {
            entries.push((b"SMask".to_vec(), PdfObj::Ref(smask)));
        }

        writer.add_stream(entries, &img.rgb_data, true)
    }
}

/// Generate a ToUnicode CMap stream.
fn generate_tounicode_cmap(
    map: &std::collections::HashMap<u16, u16>,
    font_name: &str,
) -> Vec<u8> {
    use std::io::Write;
    let mut buf = Vec::new();

    writeln!(buf, "/CIDInit /ProcSet findresource begin").unwrap();
    writeln!(buf, "12 dict begin").unwrap();
    writeln!(buf, "begincmap").unwrap();
    writeln!(buf, "/CIDSystemInfo <<").unwrap();
    writeln!(buf, "  /Registry (Adobe)").unwrap();
    writeln!(buf, "  /Ordering (UCS)").unwrap();
    writeln!(buf, "  /Supplement 0").unwrap();
    writeln!(buf, ">> def").unwrap();
    writeln!(buf, "/CMapName /{}-UCS def", font_name).unwrap();
    writeln!(buf, "/CMapType 2 def").unwrap();
    writeln!(buf, "1 begincodespacerange").unwrap();
    writeln!(buf, "<00> <FF>").unwrap();
    writeln!(buf, "endcodespacerange").unwrap();

    let mut sorted: Vec<_> = map.iter().collect();
    sorted.sort_by_key(|&(&code, _)| code);

    for chunk in sorted.chunks(100) {
        writeln!(buf, "{} beginbfchar", chunk.len()).unwrap();
        for &(&code, &unicode) in chunk {
            writeln!(buf, "<{:02X}> <{:04X}>", code, unicode).unwrap();
        }
        writeln!(buf, "endbfchar").unwrap();
    }

    writeln!(buf, "endcmap").unwrap();
    writeln!(buf, "CMapName currentdict /CMap defineresource pop").unwrap();
    writeln!(buf, "end").unwrap();
    writeln!(buf, "end").unwrap();

    buf
}

impl OutputDevice for PdfDevice {
    fn fill_path(&mut self, _path: &PsPath, _params: &FillParams) {}
    fn stroke_path(&mut self, _path: &PsPath, _params: &StrokeParams) {}
    fn clip_path(&mut self, _path: &PsPath, _params: &ClipParams) {}
    fn init_clip(&mut self) {}
    fn erase_page(&mut self) {}

    fn show_page(&mut self, _output_path: &str) -> Result<(), String> {
        Ok(())
    }

    fn draw_image(&mut self, _rgba_data: &[u8], _params: &ImageParams) {}

    fn page_size(&self) -> (u32, u32) {
        (self.page_w, self.page_h)
    }

    fn replay_and_show(&mut self, list: DisplayList, output_path: &str) -> Result<(), String> {
        // Capture output path from first page
        if self.output_path.is_none() {
            // Strip extension (.png or .pdf)
            let base = if let Some(pos) = output_path.rfind('.') {
                &output_path[..pos]
            } else {
                output_path
            };
            // Remove -NNNN page number suffix (e.g., "arc-0001" → "arc")
            let base = if base.len() >= 5 && base.as_bytes()[base.len() - 5] == b'-' {
                let suffix = &base[base.len() - 4..];
                if suffix.bytes().all(|b| b.is_ascii_digit()) {
                    &base[..base.len() - 5]
                } else {
                    base
                }
            } else {
                base
            };
            self.output_path = Some(format!("{}.pdf", base));
        }

        let scale = 72.0 / self.dpi;
        let ContentStreamResult {
            content,
            images,
            shading_refs,
            font_tracker,
        } = content_stream::build_content_stream(&list, self.page_w, self.page_h, self.dpi);

        self.pages.push(PageData {
            content,
            width_pts: self.page_w as f64 * scale,
            height_pts: self.page_h as f64 * scale,
            images,
            shading_refs,
            font_tracker,
        });

        Ok(())
    }

    fn finish(&mut self) -> Result<(), String> {
        if self.pages.is_empty() {
            return Ok(());
        }
        self.write_pdf()
    }
}
