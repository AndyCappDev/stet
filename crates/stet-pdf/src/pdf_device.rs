// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF output device — accumulates pages and writes a PDF file on finish().

use stet_core::context::Context;
use stet_core::device::OutputDevice;
use stet_fonts::geometry::PsPath;
use stet_graphics::device::{ClipParams, FillParams, ImageParams, StrokeParams};
use stet_graphics::display_list::DisplayList;

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
    /// ICC output profile bytes, retained for forward compatibility with a
    /// future PDF/X-4 OutputIntent implementation. Currently unused.
    #[allow(dead_code)]
    output_profile: Option<Vec<u8>>,
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
            output_profile: None,
        }
    }

    /// Set the trim box for the next page (in PDF points, lower-left origin).
    pub fn set_trim_box(&mut self, llx: f64, lly: f64, urx: f64, ury: f64) {
        self.pending_trim_box = Some((llx, lly, urx, ury));
    }

    /// Set an ICC output profile.
    ///
    /// Previously embedded as a PDF/X-3 OutputIntent, but the emitted output
    /// contained transparency features (soft masks) that PDF/X-3 prohibits.
    /// The OutputIntent emission path has been removed pending a correct
    /// PDF/X-4 implementation; calling this currently has no effect on the
    /// output. The setter is retained so the API is forward-compatible with
    /// the eventual X-4 work.
    #[deprecated(
        note = "OutputIntent emission is temporarily disabled pending PDF/X-4 support; calling this has no effect"
    )]
    pub fn set_output_profile(&mut self, bytes: Vec<u8>) {
        self.output_profile = Some(bytes);
    }

    /// Build the PDF document into a byte vector.
    ///
    /// Returns the complete PDF file contents. The device must have at least
    /// one page (call after `finish()` or `finish_with_context()`).
    pub fn take_pdf_bytes(&self) -> Option<Vec<u8>> {
        if self.pages.is_empty() {
            return None;
        }
        let (writer, catalog_ref, info_ref) = self.build_pdf(None).ok()?;
        let mut buf = Vec::new();
        writer
            .write_pdf(&mut buf, catalog_ref, Some(info_ref))
            .ok()?;
        Some(buf)
    }

    /// Build the PDF document into a byte vector, using Context for font data.
    pub fn take_pdf_bytes_with_context(&self, ctx: &Context) -> Option<Vec<u8>> {
        if self.pages.is_empty() {
            return None;
        }
        let (writer, catalog_ref, info_ref) = self.build_pdf(Some(ctx)).ok()?;
        let mut buf = Vec::new();
        writer
            .write_pdf(&mut buf, catalog_ref, Some(info_ref))
            .ok()?;
        Some(buf)
    }

    /// Assemble all accumulated pages into a PDF and write to the output file.
    fn write_pdf(&self, ctx: Option<&Context>) -> Result<(), String> {
        let path = self.output_path.as_deref().ok_or("no output path set")?;
        let (writer, catalog_ref, info_ref) = self.build_pdf(ctx)?;

        let file = std::fs::File::create(path).map_err(|e| format!("create {}: {}", path, e))?;
        let mut bw = std::io::BufWriter::new(file);
        writer
            .write_pdf(&mut bw, catalog_ref, Some(info_ref))
            .map_err(|e| format!("write {}: {}", path, e))?;

        eprintln!("PDF written: {} ({} pages)", path, self.pages.len());
        Ok(())
    }

    /// Build the contents of the /Info dict. Starts with device defaults
    /// (Producer + auto-derived Title + UTC CreationDate) and lets any
    /// `/DOCINFO` pdfmark record on `ctx.pdfmark_buffer` override or
    /// extend each key. The pdfmark buffer is *not* drained here; phases
    /// past Phase 1 may want to consult it for separate concerns.
    fn build_info_dict(&self, ctx: Option<&Context>) -> Vec<(Vec<u8>, PdfObj)> {
        let docinfo = ctx.map(|c| collect_docinfo(c)).unwrap_or_default();

        let producer = docinfo
            .producer
            .clone()
            .unwrap_or_else(|| "stet".to_string());
        let mut entries: Vec<(Vec<u8>, PdfObj)> = vec![(
            b"Producer".to_vec(),
            PdfObj::LitString(producer.into_bytes()),
        )];

        // Title — pdfmark wins; otherwise derive from filename.
        let title = docinfo.title.clone().or_else(|| {
            self.output_path
                .as_deref()
                .and_then(|p| std::path::Path::new(p).file_stem())
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
        });
        if let Some(t) = title {
            entries.push((b"Title".to_vec(), PdfObj::LitString(t.into_bytes())));
        }

        for (key, value) in [
            (&b"Author"[..], &docinfo.author),
            (&b"Subject"[..], &docinfo.subject),
            (&b"Keywords"[..], &docinfo.keywords),
            (&b"Creator"[..], &docinfo.creator),
        ] {
            if let Some(v) = value {
                entries.push((key.to_vec(), PdfObj::LitString(v.clone().into_bytes())));
            }
        }

        // CreationDate — pdfmark override or default to "now in UTC".
        let creation_date = docinfo
            .creation_date_string()
            .unwrap_or_else(default_now_pdf_date);
        entries.push((
            b"CreationDate".to_vec(),
            PdfObj::LitString(creation_date.into_bytes()),
        ));

        if let Some(md) = docinfo.mod_date_string() {
            entries.push((b"ModDate".to_vec(), PdfObj::LitString(md.into_bytes())));
        }

        if let Some(t) = docinfo.trapped {
            let name: &[u8] = match t {
                stet_core::pdfmark::TrappedState::True => b"True",
                stet_core::pdfmark::TrappedState::False => b"False",
                stet_core::pdfmark::TrappedState::Unknown => b"Unknown",
            };
            entries.push((b"Trapped".to_vec(), PdfObj::Name(name.to_vec())));
        }

        entries
    }

    /// Build the PDF document, returning the writer and object refs.
    fn build_pdf(&self, ctx: Option<&Context>) -> Result<(PdfWriter, u32, u32), String> {
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

        // Pre-allocate page object numbers so annotations can reference
        // their target pages by indirect ref before the page dict is
        // written, and so /Annots arrays can be assembled at build time.
        let page_refs: Vec<u32> = (0..page_results.len())
            .map(|_| writer.alloc_obj())
            .collect();

        // Build per-page annotation objects up front so each page dict
        // gets its /Annots array. Empty when no /ANN pdfmark records
        // were issued.
        let per_page_annots: Vec<Vec<u32>> = ctx
            .map(|c| {
                let records: Vec<stet_core::pdfmark::AnnotationRecord> = c
                    .pdfmark_buffer
                    .records()
                    .iter()
                    .filter_map(|r| match r {
                        stet_core::pdfmark::PdfMarkRecord::Annotation(rec) => Some(rec.clone()),
                        _ => None,
                    })
                    .collect();
                if records.is_empty() {
                    return vec![Vec::new(); page_refs.len()];
                }
                crate::annotations::collect_per_page(&mut writer, &records, &page_refs)
            })
            .unwrap_or_else(|| vec![Vec::new(); page_refs.len()]);

        // Second pass: build page objects referencing shared font objects
        for (i, (result, page)) in page_results.iter().enumerate() {
            self.build_page(
                &mut writer,
                page,
                pages_ref,
                page_refs[i],
                result,
                &font_obj_map,
                &mut font_tracker,
                &per_page_annots[i],
            )?;
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

        // Outlines — emitted from `/OUT pdfmark` records on the
        // pdfmark buffer. Returns `None` when no /OUT records were
        // issued, in which case /Catalog stays free of /Outlines.
        let outlines_ref = ctx.and_then(|c| {
            let records: Vec<stet_core::pdfmark::OutlineRecord> = c
                .pdfmark_buffer
                .records()
                .iter()
                .filter_map(|r| match r {
                    stet_core::pdfmark::PdfMarkRecord::Outline(rec) => Some(rec.clone()),
                    _ => None,
                })
                .collect();
            if records.is_empty() {
                return None;
            }
            let tree = stet_core::pdfmark::build_outline_tree(&records);
            crate::outline::write_outline_tree(&mut writer, &tree, &page_refs)
        });

        // Catalog
        let mut catalog_entries = vec![
            (b"Type".to_vec(), PdfObj::name("Catalog")),
            (b"Pages".to_vec(), PdfObj::Ref(pages_ref)),
        ];
        if let Some(outline_ref) = outlines_ref {
            catalog_entries.push((b"Outlines".to_vec(), PdfObj::Ref(outline_ref)));
            catalog_entries.push((b"PageMode".to_vec(), PdfObj::name("UseOutlines")));
        }

        writer.set_object(catalog_ref, &PdfObj::Dict(catalog_entries));

        // Info dictionary — start with device defaults, then let any
        // /DOCINFO pdfmark records override or extend.
        let info_ref = writer.alloc_obj();
        let info_entries = self.build_info_dict(ctx);
        writer.set_object(info_ref, &PdfObj::Dict(info_entries));

        Ok((writer, catalog_ref, info_ref))
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
                font_embedder::build_font_resource(writer, usage, c).unwrap_or_else(|| {
                    let tu = font_embedder::build_tounicode_for_fallback(writer, usage, c);
                    self.build_font_reference(writer, usage, tu)
                })
            } else {
                self.build_font_reference(writer, usage, None)
            };
            map.insert(usage.pdf_name.clone(), font_ref);
        }
        map
    }

    /// Build PDF objects for a single page. The page's indirect object
    /// number is pre-allocated by the caller (so annotations can target
    /// the page before its dict is written), and the per-page
    /// annotation refs are passed in for inclusion in the page's
    /// `/Annots` array.
    #[allow(clippy::too_many_arguments)]
    fn build_page(
        &self,
        writer: &mut PdfWriter,
        page: &PageData,
        pages_ref: u32,
        page_ref: u32,
        result: &ContentStreamResult,
        font_obj_map: &HashMap<String, u32>,
        font_tracker: &mut FontTracker,
        annot_refs: &[u32],
    ) -> Result<(), String> {
        let ContentStreamResult {
            content,
            images,
            shading_refs,
            used_font_names,
            ext_gstate_dicts,
            color_spaces,
            pattern_refs,
            pattern_cs_entries,
            transfer_refs,
            halftone_refs,
            bg_ucr_refs,
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
                let mut entries: Vec<(Vec<u8>, PdfObj)> = gs_dict
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

                // Check if this ExtGState has a transfer function reference
                if let Some(tr) = transfer_refs.iter().find(|r| r.ext_gstate_idx == i) {
                    let tr2_value = build_transfer_tr2(writer, &tr.tables, tr.is_color);
                    entries.push((b"TR2".to_vec(), tr2_value));
                }

                // Check if this ExtGState has a halftone reference
                if let Some(hr) = halftone_refs.iter().find(|r| r.ext_gstate_idx == i) {
                    let ht_value = build_halftone_ht(writer, &hr.state);
                    entries.push((b"HT".to_vec(), ht_value));
                }

                // Check if this ExtGState has BG/UCR references
                if let Some(br) = bg_ucr_refs.iter().find(|r| r.ext_gstate_idx == i) {
                    if let Some(ref bg) = br.state.bg {
                        let func_ref = build_type0_function(writer, bg);
                        entries.push((b"BG2".to_vec(), PdfObj::Ref(func_ref)));
                    }
                    if let Some(ref ucr) = br.state.ucr {
                        let func_ref = build_type0_function_signed(writer, ucr);
                        entries.push((b"UCR2".to_vec(), PdfObj::Ref(func_ref)));
                    }
                }

                let gs_ref = writer.add_object(&PdfObj::Dict(entries));
                gs_entries.push((format!("GS{}", i).into_bytes(), PdfObj::Ref(gs_ref)));
            }
            resources.push((b"ExtGState".to_vec(), PdfObj::Dict(gs_entries)));
        }

        // Build ColorSpace resources (for Separation/DeviceN fill/stroke colors)
        let mut cs_entries: Vec<(Vec<u8>, PdfObj)> = Vec::new();
        for (name, spot_cs) in color_spaces {
            let cs_obj = build_spot_colorspace(spot_cs, writer);
            cs_entries.push((name.clone().into_bytes(), cs_obj));
        }
        // Add uncolored pattern color space entries (e.g., [/Pattern /DeviceRGB])
        for (name, cs_obj) in pattern_cs_entries {
            // Reconstruct PdfObj since it doesn't derive Clone
            let obj = match cs_obj {
                PdfObj::Array(items) => {
                    let cloned: Vec<PdfObj> = items
                        .iter()
                        .map(|item| match item {
                            PdfObj::Name(n) => PdfObj::Name(n.clone()),
                            PdfObj::Int(n) => PdfObj::Int(*n),
                            PdfObj::Real(n) => PdfObj::Real(*n),
                            PdfObj::Ref(r) => PdfObj::Ref(*r),
                            _ => PdfObj::Null,
                        })
                        .collect();
                    PdfObj::Array(cloned)
                }
                _ => PdfObj::Null,
            };
            cs_entries.push((name.clone().into_bytes(), obj));
        }
        if !cs_entries.is_empty() {
            resources.push((b"ColorSpace".to_vec(), PdfObj::Dict(cs_entries)));
        }

        // Build Pattern XObject resources
        if !pattern_refs.is_empty() {
            let mut pattern_entries: Vec<(Vec<u8>, PdfObj)> = Vec::new();
            for (i, pat_ref) in pattern_refs.iter().enumerate() {
                let tile_result =
                    content_stream::build_tile_content_stream(&pat_ref.tile, font_tracker);

                // Build tile resources
                let mut tile_resources: Vec<(Vec<u8>, PdfObj)> = Vec::new();

                // Tile images
                if !tile_result.images.is_empty() {
                    let mut tile_xobj: Vec<(Vec<u8>, PdfObj)> = Vec::new();
                    for (j, img) in tile_result.images.iter().enumerate() {
                        let img_ref = self.build_image_xobject(writer, img);
                        tile_xobj.push((format!("Im{}", j).into_bytes(), PdfObj::Ref(img_ref)));
                    }
                    tile_resources.push((b"XObject".to_vec(), PdfObj::Dict(tile_xobj)));
                }

                // Tile shadings
                if !tile_result.shading_refs.is_empty() {
                    let mut tile_sh: Vec<(Vec<u8>, PdfObj)> = Vec::new();
                    for (j, sh_ref) in tile_result.shading_refs.iter().enumerate() {
                        let sh_obj = match sh_ref {
                            ShadingRef::Axial(p) => shading_ops::build_axial_shading(writer, p),
                            ShadingRef::Radial(p) => shading_ops::build_radial_shading(writer, p),
                            ShadingRef::Mesh(p) => shading_ops::build_mesh_shading(writer, p),
                            ShadingRef::Patch(p) => shading_ops::build_patch_shading(writer, p),
                        };
                        tile_sh.push((format!("Sh{}", j).into_bytes(), PdfObj::Ref(sh_obj)));
                    }
                    tile_resources.push((b"Shading".to_vec(), PdfObj::Dict(tile_sh)));
                }

                // Tile fonts
                if !tile_result.used_font_names.is_empty() {
                    let mut tile_fonts: Vec<(Vec<u8>, PdfObj)> = Vec::new();
                    for name in &tile_result.used_font_names {
                        if let Some(&obj_ref) = font_obj_map.get(name) {
                            tile_fonts.push((name.clone().into_bytes(), PdfObj::Ref(obj_ref)));
                        }
                    }
                    if !tile_fonts.is_empty() {
                        tile_resources.push((b"Font".to_vec(), PdfObj::Dict(tile_fonts)));
                    }
                }

                // Tile ExtGState
                if !tile_result.ext_gstate_dicts.is_empty() {
                    let mut tile_gs: Vec<(Vec<u8>, PdfObj)> = Vec::new();
                    for (j, gs_dict) in tile_result.ext_gstate_dicts.iter().enumerate() {
                        let mut entries: Vec<(Vec<u8>, PdfObj)> = gs_dict
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
                        if let Some(tr) = tile_result
                            .transfer_refs
                            .iter()
                            .find(|r| r.ext_gstate_idx == j)
                        {
                            let tr2_value = build_transfer_tr2(writer, &tr.tables, tr.is_color);
                            entries.push((b"TR2".to_vec(), tr2_value));
                        }
                        if let Some(hr) = tile_result
                            .halftone_refs
                            .iter()
                            .find(|r| r.ext_gstate_idx == j)
                        {
                            let ht_value = build_halftone_ht(writer, &hr.state);
                            entries.push((b"HT".to_vec(), ht_value));
                        }
                        if let Some(br) = tile_result
                            .bg_ucr_refs
                            .iter()
                            .find(|r| r.ext_gstate_idx == j)
                        {
                            if let Some(ref bg) = br.state.bg {
                                let func_ref = build_type0_function(writer, bg);
                                entries.push((b"BG2".to_vec(), PdfObj::Ref(func_ref)));
                            }
                            if let Some(ref ucr) = br.state.ucr {
                                let func_ref = build_type0_function_signed(writer, ucr);
                                entries.push((b"UCR2".to_vec(), PdfObj::Ref(func_ref)));
                            }
                        }
                        let gs_ref = writer.add_object(&PdfObj::Dict(entries));
                        tile_gs.push((format!("GS{}", j).into_bytes(), PdfObj::Ref(gs_ref)));
                    }
                    tile_resources.push((b"ExtGState".to_vec(), PdfObj::Dict(tile_gs)));
                }

                // Tile color spaces
                if !tile_result.color_spaces.is_empty() {
                    let mut tile_cs: Vec<(Vec<u8>, PdfObj)> = Vec::new();
                    for (name, spot_cs) in &tile_result.color_spaces {
                        let cs_obj = build_spot_colorspace(spot_cs, writer);
                        tile_cs.push((name.clone().into_bytes(), cs_obj));
                    }
                    tile_resources.push((b"ColorSpace".to_vec(), PdfObj::Dict(tile_cs)));
                }

                // Build Pattern stream object
                let m = &pat_ref.pattern_matrix;
                let pat_dict = vec![
                    (b"Type".to_vec(), PdfObj::name("Pattern")),
                    (b"PatternType".to_vec(), PdfObj::Int(1)),
                    (
                        b"PaintType".to_vec(),
                        PdfObj::Int(pat_ref.paint_type as i64),
                    ),
                    (b"TilingType".to_vec(), PdfObj::Int(1)),
                    (
                        b"BBox".to_vec(),
                        // Expand BBox slightly beyond XStep/YStep so adjacent tiles
                        // overlap, eliminating hairline seam artifacts in PDF viewers.
                        PdfObj::Array(vec![
                            PdfObj::Real(pat_ref.bbox[0] - 0.5),
                            PdfObj::Real(pat_ref.bbox[1] - 0.5),
                            PdfObj::Real(pat_ref.bbox[2] + 0.5),
                            PdfObj::Real(pat_ref.bbox[3] + 0.5),
                        ]),
                    ),
                    (b"XStep".to_vec(), PdfObj::Real(pat_ref.xstep)),
                    (b"YStep".to_vec(), PdfObj::Real(pat_ref.ystep)),
                    (
                        b"Matrix".to_vec(),
                        PdfObj::Array(vec![
                            PdfObj::Real(m.a),
                            PdfObj::Real(m.b),
                            PdfObj::Real(m.c),
                            PdfObj::Real(m.d),
                            PdfObj::Real(m.tx),
                            PdfObj::Real(m.ty),
                        ]),
                    ),
                    (b"Resources".to_vec(), PdfObj::Dict(tile_resources)),
                ];

                let pat_obj = writer.add_stream(pat_dict, &tile_result.content, true);
                pattern_entries.push((format!("P{}", i).into_bytes(), PdfObj::Ref(pat_obj)));
            }

            resources.push((b"Pattern".to_vec(), PdfObj::Dict(pattern_entries)));
        }

        // Content stream
        let content_ref = writer.add_stream(Vec::new(), content, true);

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
        if !annot_refs.is_empty() {
            page_entries.push((
                b"Annots".to_vec(),
                PdfObj::Array(annot_refs.iter().map(|r| PdfObj::Ref(*r)).collect()),
            ));
        }
        writer.set_object(page_ref, &PdfObj::Dict(page_entries));

        Ok(())
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
        tounicode_override: Option<u32>,
    ) -> u32 {
        // Build ToUnicode CMap — use override if provided, otherwise fall back to naive mapping
        let tounicode_ref = tounicode_override.or_else(|| self.build_tounicode_cmap(writer, usage));

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
                    .flat_map(|&v| [PdfObj::Int(v as i64), PdfObj::Int(v as i64)])
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
        PdfColorSpace::Separation {
            name,
            alt,
            tint_table,
        } => {
            let alt_obj = build_pdf_colorspace(alt, None, writer);
            let func_ref = build_tint_function(tint_table, writer);
            PdfObj::Array(vec![
                PdfObj::name("Separation"),
                PdfObj::Name(name.clone()),
                alt_obj,
                PdfObj::Ref(func_ref),
            ])
        }
        PdfColorSpace::DeviceN {
            names,
            alt,
            tint_table,
        } => {
            let alt_obj = build_pdf_colorspace(alt, None, writer);
            let func_ref = build_tint_function(tint_table, writer);
            let names_arr = PdfObj::Array(names.iter().map(|n| PdfObj::Name(n.clone())).collect());
            PdfObj::Array(vec![
                PdfObj::name("DeviceN"),
                names_arr,
                alt_obj,
                PdfObj::Ref(func_ref),
            ])
        }
    }
}

/// Build a PDF Type 0 (sampled) function stream from a TintLookupTable.
/// Returns the object number of the function stream.
fn build_tint_function(
    table: &stet_graphics::device::TintLookupTable,
    writer: &mut PdfWriter,
) -> u32 {
    let ni = table.num_inputs as usize;
    let no = table.num_outputs as usize;

    // Convert f32 data (0.0–1.0) to u8 samples (0–255).
    // Our TintLookupTable stores data in row-major order (last dimension varies fastest),
    // but PDF Type 0 functions require the first dimension to vary fastest.
    // For 1D, the order is the same. For ND, we must transpose.
    let spd = table.samples_per_dim as usize;
    let total_entries = spd.pow(ni as u32);
    let samples: Vec<u8> = if ni <= 1 {
        table
            .data
            .iter()
            .map(|&v| (v.clamp(0.0, 1.0) * 255.0) as u8)
            .collect()
    } else {
        // Reorder: iterate in PDF order (dim0 fastest) and look up in our order (dim0 slowest)
        let mut out = Vec::with_capacity(total_entries * no);
        for pdf_idx in 0..total_entries {
            // Decompose pdf_idx with dim0 varying fastest
            let mut coords = vec![0usize; ni];
            let mut rem = pdf_idx;
            for coord in coords.iter_mut() {
                *coord = rem % spd;
                rem /= spd;
            }
            // Convert to our row-major index (dim0 slowest, last dim fastest)
            let mut our_idx = 0;
            for coord in coords.iter() {
                our_idx = our_idx * spd + coord;
            }
            let base = our_idx * no;
            for c in 0..no {
                out.push((table.data[base + c].clamp(0.0, 1.0) * 255.0) as u8);
            }
        }
        out
    };

    // Domain: [0 1] repeated for each input
    let mut domain = Vec::with_capacity(ni * 2);
    for _ in 0..ni {
        domain.push(PdfObj::Int(0));
        domain.push(PdfObj::Int(1));
    }

    // Range: [0 1] repeated for each output
    let mut range = Vec::with_capacity(no * 2);
    for _ in 0..no {
        range.push(PdfObj::Int(0));
        range.push(PdfObj::Int(1));
    }

    // Size: samples_per_dim repeated for each input dimension
    let size: Vec<PdfObj> = (0..ni)
        .map(|_| PdfObj::Int(table.samples_per_dim as i64))
        .collect();

    let dict_entries = vec![
        (b"FunctionType".to_vec(), PdfObj::Int(0)),
        (b"Domain".to_vec(), PdfObj::Array(domain)),
        (b"Range".to_vec(), PdfObj::Array(range)),
        (b"Size".to_vec(), PdfObj::Array(size)),
        (b"BitsPerSample".to_vec(), PdfObj::Int(8)),
    ];

    writer.add_stream(dict_entries, &samples, true)
}

/// Build a PDF Separation or DeviceN color space array from a SpotColorSpace.
/// Returns a PdfObj (array) suitable for inclusion in the Resources/ColorSpace dict.
fn build_spot_colorspace(
    spot_cs: &stet_graphics::device::SpotColorSpace,
    writer: &mut PdfWriter,
) -> PdfObj {
    use stet_graphics::device::{SimpleColorSpace, SpotColorSpace};
    match spot_cs {
        SpotColorSpace::Separation {
            name,
            alt,
            tint_table,
        } => {
            let alt_obj = match alt {
                SimpleColorSpace::DeviceGray => PdfObj::name("DeviceGray"),
                SimpleColorSpace::DeviceRGB => PdfObj::name("DeviceRGB"),
                SimpleColorSpace::DeviceCMYK => PdfObj::name("DeviceCMYK"),
            };
            let func_ref = build_tint_function(tint_table, writer);
            PdfObj::Array(vec![
                PdfObj::name("Separation"),
                PdfObj::Name(name.clone()),
                alt_obj,
                PdfObj::Ref(func_ref),
            ])
        }
        SpotColorSpace::DeviceN {
            names,
            alt,
            tint_table,
        } => {
            let alt_obj = match alt {
                SimpleColorSpace::DeviceGray => PdfObj::name("DeviceGray"),
                SimpleColorSpace::DeviceRGB => PdfObj::name("DeviceRGB"),
                SimpleColorSpace::DeviceCMYK => PdfObj::name("DeviceCMYK"),
            };
            let func_ref = build_tint_function(tint_table, writer);
            let names_arr = PdfObj::Array(names.iter().map(|n| PdfObj::Name(n.clone())).collect());
            PdfObj::Array(vec![
                PdfObj::name("DeviceN"),
                names_arr,
                alt_obj,
                PdfObj::Ref(func_ref),
            ])
        }
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

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Parse an ICC profile header to extract the number of components and description.
///
/// Returns (N, description) where N is derived from the color space signature
/// at bytes 16–19 and description is extracted from the `desc` or `mluc` tag.
///
/// Currently unused — kept for forward compatibility with the planned
/// PDF/X-4 OutputIntent implementation.
#[allow(dead_code)]
fn parse_icc_header(data: &[u8]) -> (u32, String) {
    let n = if data.len() >= 20 {
        match &data[16..20] {
            b"CMYK" => 4,
            b"RGB " => 3,
            b"GRAY" => 1,
            b"Lab " => 3,
            _ => 4, // assume CMYK for unknown
        }
    } else {
        4
    };
    let desc = extract_icc_description(data).unwrap_or_else(|| "Custom".to_string());
    (n, desc)
}

/// Extract the profile description from an ICC profile's tag table.
///
/// Looks for the `desc` tag (v2, type 'desc') or `mluc` tag (v4, type 'mluc').
#[allow(dead_code)]
fn extract_icc_description(data: &[u8]) -> Option<String> {
    if data.len() < 132 {
        return None;
    }
    let tag_count = u32::from_be_bytes(data[128..132].try_into().ok()?) as usize;
    let tag_table_start = 132;

    for i in 0..tag_count {
        let offset = tag_table_start + i * 12;
        if offset + 12 > data.len() {
            break;
        }
        let tag_sig = &data[offset..offset + 4];
        let tag_offset = u32::from_be_bytes(data[offset + 4..offset + 8].try_into().ok()?) as usize;
        let tag_size = u32::from_be_bytes(data[offset + 8..offset + 12].try_into().ok()?) as usize;

        if tag_sig != b"desc" {
            continue;
        }
        if tag_offset + tag_size > data.len() || tag_size < 12 {
            return None;
        }

        let type_sig = &data[tag_offset..tag_offset + 4];
        if type_sig == b"desc" {
            // ICC v2 'desc' type: u32 count at offset+8, ASCII string at offset+12
            let count =
                u32::from_be_bytes(data[tag_offset + 8..tag_offset + 12].try_into().ok()?) as usize;
            if count == 0 {
                return None;
            }
            let str_end = (tag_offset + 12 + count).min(tag_offset + tag_size);
            let s = &data[tag_offset + 12..str_end];
            // Trim trailing null bytes
            let s = s.split(|&b| b == 0).next().unwrap_or(s);
            return Some(String::from_utf8_lossy(s).to_string());
        } else if type_sig == b"mluc" {
            // ICC v4 'mluc' type: multi-localized Unicode
            if tag_size < 20 {
                return None;
            }
            let record_count =
                u32::from_be_bytes(data[tag_offset + 8..tag_offset + 12].try_into().ok()?) as usize;
            if record_count == 0 {
                return None;
            }
            // First record: language(2) + country(2) + length(4) + offset(4)
            let rec_base = tag_offset + 16;
            if rec_base + 12 > data.len() {
                return None;
            }
            let str_len =
                u32::from_be_bytes(data[rec_base + 4..rec_base + 8].try_into().ok()?) as usize;
            let str_off =
                u32::from_be_bytes(data[rec_base + 8..rec_base + 12].try_into().ok()?) as usize;
            let abs_off = tag_offset + str_off;
            if abs_off + str_len > data.len() || str_len < 2 {
                return None;
            }
            // UTF-16BE → String
            let utf16: Vec<u16> = data[abs_off..abs_off + str_len]
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            return Some(
                String::from_utf16_lossy(&utf16)
                    .trim_end_matches('\0')
                    .to_string(),
            );
        }

        break;
    }
    None
}

/// Build a PDF Type 0 (sampled) function stream from a 256-entry transfer table.
/// Returns the object number of the function stream.
fn build_type0_function(writer: &mut PdfWriter, table: &[f64]) -> u32 {
    let dict_entries = vec![
        (b"FunctionType".to_vec(), PdfObj::Int(0)),
        (
            b"Domain".to_vec(),
            PdfObj::Array(vec![PdfObj::Int(0), PdfObj::Int(1)]),
        ),
        (
            b"Range".to_vec(),
            PdfObj::Array(vec![PdfObj::Int(0), PdfObj::Int(1)]),
        ),
        (
            b"Size".to_vec(),
            PdfObj::Array(vec![PdfObj::Int(table.len() as i64)]),
        ),
        (b"BitsPerSample".to_vec(), PdfObj::Int(8)),
    ];
    let data: Vec<u8> = table
        .iter()
        .map(|&v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
        .collect();
    writer.add_stream(dict_entries, &data, false)
}

/// Build the /TR2 value for an ExtGState dict from transfer function tables.
/// Returns a PdfObj (Ref for single function, Array for 4-component, or Name for identity).
fn build_transfer_tr2(
    writer: &mut PdfWriter,
    tables: &[Option<std::sync::Arc<Vec<f64>>>],
    is_color: bool,
) -> PdfObj {
    if is_color && tables.len() == 4 {
        // 4-component: [R, G, B, Gray], use /Identity for None entries
        let refs: Vec<PdfObj> = tables
            .iter()
            .map(|t| {
                if let Some(table) = t {
                    let func_ref = build_type0_function(writer, table);
                    PdfObj::Ref(func_ref)
                } else {
                    PdfObj::name("Identity")
                }
            })
            .collect();
        PdfObj::Array(refs)
    } else if !is_color && tables.len() == 1 {
        if let Some(ref table) = tables[0] {
            let func_ref = build_type0_function(writer, table);
            PdfObj::Ref(func_ref)
        } else {
            PdfObj::name("Identity")
        }
    } else {
        PdfObj::name("Identity")
    }
}

/// Build a PDF Type 0 (sampled) function stream from a 256-entry table with signed range [-1,1].
/// Used for undercolor removal (UCR) functions.
fn build_type0_function_signed(writer: &mut PdfWriter, table: &[f64]) -> u32 {
    let dict_entries = vec![
        (b"FunctionType".to_vec(), PdfObj::Int(0)),
        (
            b"Domain".to_vec(),
            PdfObj::Array(vec![PdfObj::Int(0), PdfObj::Int(1)]),
        ),
        (
            b"Range".to_vec(),
            PdfObj::Array(vec![PdfObj::Int(-1), PdfObj::Int(1)]),
        ),
        (
            b"Size".to_vec(),
            PdfObj::Array(vec![PdfObj::Int(table.len() as i64)]),
        ),
        (b"BitsPerSample".to_vec(), PdfObj::Int(8)),
    ];
    // Encode [-1,1] → [0,255]: byte = (v + 1) / 2 * 255
    let data: Vec<u8> = table
        .iter()
        .map(|&v| ((v.clamp(-1.0, 1.0) + 1.0) / 2.0 * 255.0).round() as u8)
        .collect();
    writer.add_stream(dict_entries, &data, false)
}

/// Build a PDF Type 4 (PostScript calculator) function from token bytes.
/// Domain is 2D [-1,1]×[-1,1], Range [0,1].
fn build_type4_function(writer: &mut PdfWriter, tokens: &[u8]) -> u32 {
    let dict_entries = vec![
        (b"FunctionType".to_vec(), PdfObj::Int(4)),
        (
            b"Domain".to_vec(),
            PdfObj::Array(vec![
                PdfObj::Int(-1),
                PdfObj::Int(1),
                PdfObj::Int(-1),
                PdfObj::Int(1),
            ]),
        ),
        (
            b"Range".to_vec(),
            PdfObj::Array(vec![PdfObj::Int(0), PdfObj::Int(1)]),
        ),
    ];
    writer.add_stream(dict_entries, tokens, false)
}

/// Build a PDF Type 0 (sampled) 2D function from a 64×64 sample table.
/// Domain is [-1,1]×[-1,1], Range [0,1].
fn build_type0_function_2d(writer: &mut PdfWriter, table: &[f64]) -> u32 {
    let dict_entries = vec![
        (b"FunctionType".to_vec(), PdfObj::Int(0)),
        (
            b"Domain".to_vec(),
            PdfObj::Array(vec![
                PdfObj::Int(-1),
                PdfObj::Int(1),
                PdfObj::Int(-1),
                PdfObj::Int(1),
            ]),
        ),
        (
            b"Range".to_vec(),
            PdfObj::Array(vec![PdfObj::Int(0), PdfObj::Int(1)]),
        ),
        (
            b"Size".to_vec(),
            PdfObj::Array(vec![PdfObj::Int(64), PdfObj::Int(64)]),
        ),
        (b"BitsPerSample".to_vec(), PdfObj::Int(8)),
    ];
    let data: Vec<u8> = table
        .iter()
        .map(|&v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
        .collect();
    writer.add_stream(dict_entries, &data, false)
}

/// Build a PDF halftone screen object (Type 1 halftone dict) from a HalftoneScreen.
/// Returns a PdfObj (either inline Dict or Ref to indirect object).
fn build_halftone_screen(
    writer: &mut PdfWriter,
    screen: &stet_graphics::device::HalftoneScreen,
) -> PdfObj {
    let spot_func = if let Some(ref tokens) = screen.type4_tokens {
        let func_ref = build_type4_function(writer, tokens);
        PdfObj::Ref(func_ref)
    } else if let Some(ref table) = screen.sampled_2d {
        let func_ref = build_type0_function_2d(writer, table);
        PdfObj::Ref(func_ref)
    } else {
        PdfObj::name("Default")
    };

    let entries = vec![
        (b"Type".to_vec(), PdfObj::name("Halftone")),
        (b"HalftoneType".to_vec(), PdfObj::Int(1)),
        (b"Frequency".to_vec(), PdfObj::Real(screen.frequency)),
        (b"Angle".to_vec(), PdfObj::Real(screen.angle)),
        (b"SpotFunction".to_vec(), spot_func),
    ];
    let obj_ref = writer.add_object(&PdfObj::Dict(entries));
    PdfObj::Ref(obj_ref)
}

/// Build the /HT value for an ExtGState dict from a HalftoneState.
fn build_halftone_ht(
    writer: &mut PdfWriter,
    state: &stet_graphics::device::HalftoneState,
) -> PdfObj {
    if let Some(ref color) = state.color {
        // Type 5 composite halftone
        let mut entries = vec![
            (b"Type".to_vec(), PdfObj::name("Halftone")),
            (b"HalftoneType".to_vec(), PdfObj::Int(5)),
        ];
        let component_names: [&[u8]; 4] = [b"Red", b"Green", b"Blue", b"Default"];
        for (i, screen_opt) in color.iter().enumerate() {
            if let Some(screen) = screen_opt {
                let ht_obj = build_halftone_screen(writer, screen);
                entries.push((component_names[i].to_vec(), ht_obj));
            }
        }
        let obj_ref = writer.add_object(&PdfObj::Dict(entries));
        PdfObj::Ref(obj_ref)
    } else if let Some(ref gray) = state.gray {
        build_halftone_screen(writer, gray)
    } else {
        PdfObj::name("Default")
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

/// Format the current wall-clock time as a PDF date string in UTC.
fn default_now_pdf_date() -> String {
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let secs_per_day = 86400u64;
    let days = now / secs_per_day;
    let time_of_day = now % secs_per_day;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;
    let (year, month, day) = days_to_ymd(days);
    format!(
        "D:{:04}{:02}{:02}{:02}{:02}{:02}Z",
        year, month, day, hours, minutes, seconds
    )
}

/// Merge every `/DOCINFO` pdfmark record on the buffer into a single
/// effective record. Later records override earlier ones key-by-key,
/// matching GhostScript pdfwrite's behaviour where multiple
/// `[ /DOCINFO pdfmark` blocks accumulate.
fn collect_docinfo(ctx: &Context) -> stet_core::pdfmark::DocInfoRecord {
    let mut acc = stet_core::pdfmark::DocInfoRecord::default();
    for record in ctx.pdfmark_buffer.records() {
        let stet_core::pdfmark::PdfMarkRecord::DocInfo(rec) = record else {
            continue;
        };
        if let Some(v) = &rec.title {
            acc.title = Some(v.clone());
        }
        if let Some(v) = &rec.author {
            acc.author = Some(v.clone());
        }
        if let Some(v) = &rec.subject {
            acc.subject = Some(v.clone());
        }
        if let Some(v) = &rec.keywords {
            acc.keywords = Some(v.clone());
        }
        if let Some(v) = &rec.creator {
            acc.creator = Some(v.clone());
        }
        if let Some(v) = &rec.producer {
            acc.producer = Some(v.clone());
        }
        if let Some(v) = &rec.creation_date {
            acc.creation_date = Some(v.clone());
        }
        if let Some(v) = &rec.mod_date {
            acc.mod_date = Some(v.clone());
        }
        if let Some(v) = rec.trapped {
            acc.trapped = Some(v);
        }
    }
    acc
}
