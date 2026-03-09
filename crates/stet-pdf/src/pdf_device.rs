// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! PDF output device — accumulates pages and writes a PDF file on finish().

use stet_core::context::Context;
use stet_core::device::{ClipParams, FillParams, ImageParams, OutputDevice, StrokeParams};
use stet_core::display_list::DisplayList;
use stet_core::graphics_state::PsPath;

use std::collections::HashMap;

use crate::content_stream::{self, ContentStreamResult, ShadingRef};
use crate::font_embedder;
use crate::font_tracker::FontTracker;
use crate::image_ops::ImageXObject;
use crate::pdf_objects::PdfObj;
use crate::pdf_writer::PdfWriter;
use crate::shading_ops;

/// A single page's data. Display list is stored and content stream generated
/// at finalize time when Context is available for font width extraction.
struct PageData {
    display_list: DisplayList,
    width_pts: f64,
    height_pts: f64,
    page_w: u32,
    page_h: u32,
    dpi: f64,
    trim_box: Option<(f64, f64, f64, f64)>,
}

/// PDF output device. Accumulates display lists per page and generates
/// a single PDF file containing all pages on `finish()`.
pub struct PdfDevice {
    pages: Vec<PageData>,
    page_w: u32,
    page_h: u32,
    dpi: f64,
    output_path: Option<String>,
    pending_trim_box: Option<(f64, f64, f64, f64)>,
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
            pending_trim_box: None,
        }
    }

    /// Set the trim box for the next page (in PDF points, lower-left origin).
    pub fn set_trim_box(&mut self, llx: f64, lly: f64, urx: f64, ury: f64) {
        self.pending_trim_box = Some((llx, lly, urx, ury));
    }

    /// Assemble all accumulated pages into a PDF and write to the output file.
    fn write_pdf(&self, ctx: Option<&Context>) -> Result<(), String> {
        let path = self.output_path.as_deref().ok_or("no output path set")?;

        let mut writer = PdfWriter::new();

        // Pre-allocate catalog and pages objects
        let catalog_ref = writer.alloc_obj();
        let pages_ref = writer.alloc_obj();

        // Document-level font tracker — shared across all pages
        let mut font_tracker = FontTracker::new();

        // First pass: build content streams and register fonts
        let mut page_results: Vec<(ContentStreamResult, &PageData)> = Vec::new();
        for page in &self.pages {
            let result = content_stream::build_content_stream(
                &page.display_list,
                page.page_w,
                page.page_h,
                page.dpi,
                ctx,
                &mut font_tracker,
            );
            page_results.push((result, page));
        }

        // Embed each unique font once at document level
        let font_obj_map: HashMap<String, u32> =
            self.embed_all_fonts(&mut writer, &font_tracker, ctx);

        // Second pass: build page objects referencing shared font objects
        let mut page_refs = Vec::new();
        for (result, page) in &page_results {
            let page_ref = self.build_page(&mut writer, page, pages_ref, result, &font_obj_map)?;
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

        // Info dictionary
        let info_ref = writer.alloc_obj();
        let mut info_entries = vec![
            (b"Producer".to_vec(), PdfObj::LitString(b"stet".to_vec())),
        ];
        // Title from output filename
        if let Some(title) = std::path::Path::new(path)
            .file_stem()
            .and_then(|s| s.to_str())
        {
            info_entries.push((
                b"Title".to_vec(),
                PdfObj::LitString(title.as_bytes().to_vec()),
            ));
        }
        // CreationDate in PDF date format: D:YYYYMMDDHHmmSS
        {
            use std::time::SystemTime;
            let now = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            // Convert to broken-down time via simple arithmetic (UTC)
            let secs_per_day = 86400u64;
            let days = now / secs_per_day;
            let time_of_day = now % secs_per_day;
            let hours = time_of_day / 3600;
            let minutes = (time_of_day % 3600) / 60;
            let seconds = time_of_day % 60;
            // Days since 1970-01-01 to Y/M/D
            let (year, month, day) = days_to_ymd(days);
            let date_str = format!(
                "D:{:04}{:02}{:02}{:02}{:02}{:02}Z",
                year, month, day, hours, minutes, seconds
            );
            info_entries.push((
                b"CreationDate".to_vec(),
                PdfObj::LitString(date_str.into_bytes()),
            ));
        }
        writer.set_object(info_ref, &PdfObj::Dict(info_entries));

        // Write to file
        let file = std::fs::File::create(path).map_err(|e| format!("create {}: {}", path, e))?;
        let mut bw = std::io::BufWriter::new(file);
        writer
            .write_pdf(&mut bw, catalog_ref, Some(info_ref))
            .map_err(|e| format!("write {}: {}", path, e))?;

        eprintln!("PDF written: {} ({} pages)", path, self.pages.len());
        Ok(())
    }

    /// Embed all tracked fonts once at document level.
    /// Returns a map from PDF font name (e.g. "F0") to the PDF object number.
    fn embed_all_fonts(
        &self,
        writer: &mut PdfWriter,
        font_tracker: &FontTracker,
        ctx: Option<&Context>,
    ) -> HashMap<String, u32> {
        let mut map = HashMap::new();
        for usage in font_tracker.fonts() {
            let font_ref = if let Some(c) = ctx {
                font_embedder::build_font_resource(writer, usage, c)
                    .unwrap_or_else(|| self.build_font_reference(writer, usage))
            } else {
                self.build_font_reference(writer, usage)
            };
            map.insert(usage.pdf_name.clone(), font_ref);
        }
        map
    }

    /// Build PDF objects for a single page. Returns the page object number.
    fn build_page(
        &self,
        writer: &mut PdfWriter,
        page: &PageData,
        pages_ref: u32,
        result: &ContentStreamResult,
        font_obj_map: &HashMap<String, u32>,
    ) -> Result<u32, String> {
        let ContentStreamResult {
            content,
            images,
            shading_refs,
            used_font_names,
            ext_gstate_dicts,
        } = result;

        // Build image XObjects
        let mut xobject_entries: Vec<(Vec<u8>, PdfObj)> = Vec::new();
        for (i, img) in images.iter().enumerate() {
            let img_ref = self.build_image_xobject(writer, img);
            xobject_entries.push((format!("Im{}", i).into_bytes(), PdfObj::Ref(img_ref)));
        }

        // Build shading objects
        let mut shading_entries: Vec<(Vec<u8>, PdfObj)> = Vec::new();
        for (i, sh_ref) in shading_refs.iter().enumerate() {
            let sh_obj = match sh_ref {
                ShadingRef::Axial(p) => shading_ops::build_axial_shading(writer, p),
                ShadingRef::Radial(p) => shading_ops::build_radial_shading(writer, p),
                ShadingRef::Mesh(p) => shading_ops::build_mesh_shading(writer, p),
                ShadingRef::Patch(p) => shading_ops::build_patch_shading(writer, p),
            };
            shading_entries.push((format!("Sh{}", i).into_bytes(), PdfObj::Ref(sh_obj)));
        }

        // Build per-page font resource references (pointing to shared document-level objects)
        let mut font_entries: Vec<(Vec<u8>, PdfObj)> = Vec::new();
        for name in used_font_names {
            if let Some(&obj_ref) = font_obj_map.get(name) {
                font_entries.push((name.clone().into_bytes(), PdfObj::Ref(obj_ref)));
            }
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

        // Build ExtGState resources
        if !ext_gstate_dicts.is_empty() {
            let mut gs_entries: Vec<(Vec<u8>, PdfObj)> = Vec::new();
            for (i, gs_dict) in ext_gstate_dicts.iter().enumerate() {
                // Rebuild entries (PdfObj doesn't derive Clone)
                let entries: Vec<(Vec<u8>, PdfObj)> = gs_dict
                    .entries
                    .iter()
                    .map(|(k, v)| {
                        let obj = match v {
                            PdfObj::Bool(b) => PdfObj::Bool(*b),
                            PdfObj::Int(n) => PdfObj::Int(*n),
                            PdfObj::Name(n) => PdfObj::Name(n.clone()),
                            _ => PdfObj::Null,
                        };
                        (k.clone(), obj)
                    })
                    .collect();
                let gs_ref = writer.add_object(&PdfObj::Dict(entries));
                gs_entries.push((format!("GS{}", i).into_bytes(), PdfObj::Ref(gs_ref)));
            }
            resources.push((b"ExtGState".to_vec(), PdfObj::Dict(gs_entries)));
        }

        // Content stream
        let content_ref = writer.add_stream(Vec::new(), &content, true);

        // Page object
        let mut page_entries = vec![
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
        ];
        if let Some((llx, lly, urx, ury)) = page.trim_box {
            page_entries.push((
                b"TrimBox".to_vec(),
                PdfObj::Array(vec![
                    PdfObj::Real(llx),
                    PdfObj::Real(lly),
                    PdfObj::Real(urx),
                    PdfObj::Real(ury),
                ]),
            ));
        }
        let page_ref = writer.add_object(&PdfObj::Dict(page_entries));

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
            (b"BaseFont".to_vec(), PdfObj::Name(usage.font_name.clone())),
        ];

        if !usage.is_standard_14 {
            // For non-standard fonts, add Encoding
            entries.push((b"Encoding".to_vec(), PdfObj::name("WinAnsiEncoding")));
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
                    (b"Interpolate".to_vec(), PdfObj::Bool(false)),
                ],
                smask_data,
                true,
            )
        });

        // Build ICC profile stream if needed
        let icc_ref = img.icc_profile.as_ref().map(|icc| {
            writer.add_stream(
                vec![(b"N".to_vec(), PdfObj::Int(icc.n as i64))],
                &icc.data,
                true,
            )
        });

        // Build PDF ColorSpace value
        let cs_obj = build_pdf_colorspace(&img.pdf_color_space, icc_ref, writer);

        // Main image XObject
        let mut entries = vec![
            (b"Type".to_vec(), PdfObj::name("XObject")),
            (b"Subtype".to_vec(), PdfObj::name("Image")),
            (b"Width".to_vec(), PdfObj::Int(img.width as i64)),
            (b"Height".to_vec(), PdfObj::Int(img.height as i64)),
        ];

        if img.is_imagemask {
            entries.push((b"ImageMask".to_vec(), PdfObj::Bool(true)));
            // Imagemasks don't have ColorSpace or BitsPerComponent in the XObject
            entries.push((
                b"Decode".to_vec(),
                PdfObj::Array(vec![PdfObj::Int(1), PdfObj::Int(0)]),
            ));
        } else {
            entries.push((b"ColorSpace".to_vec(), cs_obj));
            entries.push((
                b"BitsPerComponent".to_vec(),
                PdfObj::Int(img.bits_per_component as i64),
            ));
        }

        entries.push((b"Interpolate".to_vec(), PdfObj::Bool(false)));

        if let Some(smask) = smask_ref {
            entries.push((b"SMask".to_vec(), PdfObj::Ref(smask)));
        }

        // Color key masking (ImageType 4): /Mask array of 2×n integers
        if let Some(ref ckm) = img.color_key_mask {
            let ncomp = img.pdf_color_space.num_components();
            let mask_ints: Vec<PdfObj> = if ckm.len() == ncomp {
                // Exact match: expand each value v to [v, v] range pair
                ckm.iter()
                    .flat_map(|&v| {
                        [PdfObj::Int(v as i64), PdfObj::Int(v as i64)]
                    })
                    .collect()
            } else {
                // Range match: already in [min0, max0, min1, max1, ...] form
                ckm.iter().map(|&v| PdfObj::Int(v as i64)).collect()
            };
            entries.push((b"Mask".to_vec(), PdfObj::Array(mask_ints)));
        }

        writer.add_stream(entries, &img.sample_data, true)
    }
}

/// Build a PDF color space object from our enum.
fn build_pdf_colorspace(
    cs: &crate::image_ops::PdfColorSpace,
    icc_ref: Option<u32>,
    writer: &mut PdfWriter,
) -> PdfObj {
    use crate::image_ops::PdfColorSpace;
    match cs {
        PdfColorSpace::DeviceGray => PdfObj::name("DeviceGray"),
        PdfColorSpace::DeviceRGB => PdfObj::name("DeviceRGB"),
        PdfColorSpace::DeviceCMYK => PdfObj::name("DeviceCMYK"),
        PdfColorSpace::ICCBased { .. } => {
            if let Some(ref_num) = icc_ref {
                PdfObj::Array(vec![PdfObj::name("ICCBased"), PdfObj::Ref(ref_num)])
            } else {
                PdfObj::name("DeviceRGB") // fallback
            }
        }
        PdfColorSpace::Indexed {
            base,
            hival,
            lookup,
        } => {
            let base_obj = build_pdf_colorspace(base, None, writer);
            // Embed lookup table as a hex string stream
            let lookup_ref = writer.add_stream(Vec::new(), lookup, true);
            PdfObj::Array(vec![
                PdfObj::name("Indexed"),
                base_obj,
                PdfObj::Int(*hival as i64),
                PdfObj::Ref(lookup_ref),
            ])
        }
        // Separation/DeviceN: for now, fall back to alt space name.
        // Full Separation/DeviceN PDF emission with sampled function objects is item 1.3.
        PdfColorSpace::Separation { alt, .. } => build_pdf_colorspace(alt, None, writer),
        PdfColorSpace::DeviceN { alt, .. } => build_pdf_colorspace(alt, None, writer),
    }
}

/// Generate a ToUnicode CMap stream.
fn generate_tounicode_cmap(map: &std::collections::HashMap<u16, u16>, font_name: &str) -> Vec<u8> {
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

    fn set_trim_box(&mut self, llx: f64, lly: f64, urx: f64, ury: f64) {
        self.pending_trim_box = Some((llx, lly, urx, ury));
    }

    fn show_page(&mut self, _output_path: &str) -> Result<(), String> {
        Ok(())
    }

    fn draw_image(&mut self, _sample_data: &[u8], _params: &ImageParams) {}

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

        self.pages.push(PageData {
            display_list: list,
            width_pts: self.page_w as f64 * scale,
            height_pts: self.page_h as f64 * scale,
            page_w: self.page_w,
            page_h: self.page_h,
            dpi: self.dpi,
            trim_box: self.pending_trim_box.take(),
        });

        Ok(())
    }

    fn finish(&mut self) -> Result<(), String> {
        if self.pages.is_empty() {
            return Ok(());
        }
        self.write_pdf(None)
    }

    fn finish_with_context(&mut self, ctx: &Context) -> Result<(), String> {
        if self.pages.is_empty() {
            return Ok(());
        }
        self.write_pdf(Some(ctx))
    }
}

/// Convert days since 1970-01-01 to (year, month, day).
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Civil calendar algorithm from Howard Hinnant
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}
