// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF content stream interpreter.
//!
//! Converts PDF page content into a `DisplayList` for rendering through
//! the existing SkiaDevice pipeline.

pub mod cid_unicode;
pub mod color_space;
pub mod font;
pub mod graphics_state;
mod standard_fonts;

use crate::error::PdfError;
use crate::lexer::{Lexer, Token};
use crate::objects::{PdfDict, PdfObj};
use crate::resolver::Resolver;

use self::color_space::{
    ResolvedColorSpace, components_to_device_color_icc, convert_icc_image_data,
    painted_channels_for_cs, resolve_color_space, resolve_color_space_obj, to_image_color_space,
};
use self::graphics_state::{ColorSpaceRef, PdfGraphicsState};

use std::sync::Arc;

use self::font::{FontCache, PdfFont};
use self::graphics_state::{ShadingPatternDL, TilingPattern};
use crate::FontProvider;
use stet_core::device::{ClipParams, ImageColorSpace, ImageParams, PatternFillParams};
use stet_core::icc::IccCache;
use stet_core::display_list::{
    DisplayElement, DisplayList, GroupParams, SoftMaskParams, SoftMaskSubtype,
};
use stet_core::graphics_state::{
    DashPattern, DeviceColor, FillRule, LineCap, LineJoin, Matrix, PathSegment, PsPath,
};

/// An operand on the content stream operand stack.
#[derive(Clone, Debug)]
pub enum Operand {
    Int(i64),
    Real(f64),
    Name(Vec<u8>),
    Str(Vec<u8>),
    Array(Vec<PdfObj>),
    Dict(PdfDict),
    Bool(bool),
}

impl Operand {
    /// Get numeric value as f64.
    fn as_f64(&self) -> Option<f64> {
        match self {
            Operand::Int(n) => Some(*n as f64),
            Operand::Real(f) => Some(*f),
            _ => None,
        }
    }

    /// Get name bytes.
    fn as_name(&self) -> Option<&[u8]> {
        match self {
            Operand::Name(n) => Some(n),
            _ => None,
        }
    }

    /// Get string bytes.
    #[allow(dead_code)]
    fn as_str(&self) -> Option<&[u8]> {
        match self {
            Operand::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// Tracks the scope of an active soft mask in the display list.
struct SoftMaskScope {
    /// Index in display_list where the mask scope began.
    start_index: usize,
    /// The resolved soft mask (mask display list + params).
    mask: graphics_state::SoftMask,
}

/// PDF content stream interpreter.
pub struct ContentInterpreter<'a> {
    resolver: &'a Resolver<'a>,
    resources: PdfDict,
    gstate_stack: Vec<PdfGraphicsState>,
    gstate: PdfGraphicsState,
    current_path: PsPath,
    current_point: Option<(f64, f64)>,
    subpath_start: Option<(f64, f64)>,
    operand_stack: Vec<Operand>,
    display_list: DisplayList,
    in_text: bool,
    depth: u32,
    font_cache: FontCache,
    current_font: Option<Arc<PdfFont>>,
    /// Initial CTM (before any content stream `cm` operators).
    /// PDF pattern Matrix maps to the default user space, so we need
    /// the initial CTM (not current CTM) for pattern matrix computation.
    initial_ctm: Matrix,
    /// ICC color profile cache for ICCBased color space conversions.
    icc_cache: IccCache,
    /// Active soft mask scope: tracks which display list elements fall under the current SMask.
    soft_mask_scope: Option<SoftMaskScope>,
    /// Optional font data provider for environments without filesystem access.
    font_provider: Option<FontProvider>,
    /// Accumulated text clip path (for text rendering modes 4-7).
    /// Built up during BT..ET, applied as clip at ET.
    text_clip_path: Option<PsPath>,
}

impl<'a> ContentInterpreter<'a> {
    /// Create a new interpreter.
    pub fn new(
        resolver: &'a Resolver<'a>,
        resources: PdfDict,
        initial_ctm: Matrix,
        icc_cache: &IccCache,
        font_provider: Option<FontProvider>,
    ) -> Self {
        Self {
            resolver,
            resources,
            gstate_stack: Vec::new(),
            gstate: PdfGraphicsState::new(initial_ctm),
            current_path: PsPath::new(),
            current_point: None,
            subpath_start: None,
            operand_stack: Vec::new(),
            display_list: DisplayList::new(),
            initial_ctm,
            in_text: false,
            depth: 0,
            font_cache: FontCache::new(),
            current_font: None,
            icc_cache: icc_cache.clone(),
            soft_mask_scope: None,
            font_provider,
            text_clip_path: None,
        }
    }

    /// Look up a sub-dictionary in the resources, resolving indirect references.
    /// e.g., `resolve_resource_subdict(b"Font")` returns the /Font dict.
    fn resolve_resource_subdict(&self, key: &[u8]) -> Option<PdfDict> {
        let obj = self.resources.get(key)?;
        // If it's already a dict, return it
        if let Some(d) = obj.as_dict() {
            return Some(d.clone());
        }
        // Otherwise try to resolve the reference
        let resolved = self.resolver.deref(obj).ok()?;
        resolved.as_dict().cloned()
    }

    /// Interpret a content stream and return the display list.
    pub fn interpret(mut self, data: &[u8]) -> Result<DisplayList, PdfError> {
        if let Err(e) = self.interpret_stream(data) {
            eprintln!("warning: content stream error: {}", e);
        }
        // Flush any active soft mask scope
        self.flush_soft_mask();
        // Return partial display list even on error — handles malformed PDFs
        // where flate decompression produces truncated content streams.
        Ok(self.display_list)
    }

    /// Interpret a content stream, keeping the interpreter alive for further use.
    pub fn interpret_stream_public(&mut self, data: &[u8]) -> Result<(), PdfError> {
        self.interpret_stream(data)
    }

    /// Consume the interpreter and return the display list.
    pub fn into_display_list(mut self) -> DisplayList {
        self.flush_soft_mask();
        self.display_list
    }

    /// Reset clip state before rendering annotations.
    /// This ensures annotations aren't affected by clip regions from the page content.
    pub fn reset_clip_for_annotations(&mut self) {
        self.display_list.push(DisplayElement::InitClip);
        self.gstate.clip_path = None;
        self.gstate.clip_path_version += 1;
    }

    /// Render an annotation's normal appearance stream (/AP /N).
    pub fn render_annotation(&mut self, obj_num: u32, gen_num: u16) -> Result<(), PdfError> {
        let annot_obj = self.resolver.resolve(obj_num, gen_num)?;
        let annot_dict = annot_obj
            .as_dict()
            .ok_or(PdfError::Other("annotation not a dict".into()))?;

        // Get /Rect [llx, lly, urx, ury]
        let rect = annot_dict
            .get_array(b"Rect")
            .and_then(|a| {
                if a.len() >= 4 {
                    Some([
                        a[0].as_f64()?,
                        a[1].as_f64()?,
                        a[2].as_f64()?,
                        a[3].as_f64()?,
                    ])
                } else {
                    None
                }
            })
            .ok_or(PdfError::Other("annotation missing Rect".into()))?;

        // Get /AP dict → /N (normal appearance)
        let ap_obj = annot_dict
            .get(b"AP")
            .ok_or(PdfError::Other("no AP".into()))?;
        let ap_dict = match self.resolver.deref(ap_obj)? {
            PdfObj::Dict(d) => d,
            _ => return Err(PdfError::Other("AP not a dict".into())),
        };

        let n_ref = ap_dict
            .get(b"N")
            .ok_or(PdfError::Other("no AP/N".into()))?;

        // Resolve to get the Form XObject dict + stream
        let n_obj = self.resolver.deref(n_ref)?;
        let form_dict = n_obj
            .as_dict()
            .ok_or(PdfError::Other("AP/N not a stream".into()))?;

        // The appearance stream is a Form XObject. Its BBox defines the
        // coordinate space, and we need to map it to the annotation Rect.
        let bbox = form_dict
            .get_array(b"BBox")
            .and_then(|a| {
                if a.len() >= 4 {
                    Some([
                        a[0].as_f64()?,
                        a[1].as_f64()?,
                        a[2].as_f64()?,
                        a[3].as_f64()?,
                    ])
                } else {
                    None
                }
            })
            .unwrap_or([rect[0], rect[1], rect[2], rect[3]]);

        // Build transform: map BBox → Rect
        let bbox_w = (bbox[2] - bbox[0]).abs().max(0.001);
        let bbox_h = (bbox[3] - bbox[1]).abs().max(0.001);
        let rect_w = (rect[2] - rect[0]).abs();
        let rect_h = (rect[3] - rect[1]).abs();
        let sx = rect_w / bbox_w;
        let sy = rect_h / bbox_h;
        let tx = rect[0] - bbox[0] * sx;
        let ty = rect[1] - bbox[1] * sy;
        let bbox_to_rect = Matrix::new(sx, 0.0, 0.0, sy, tx, ty);

        // Apply form's own matrix if present
        let form_matrix = form_dict
            .get_array(b"Matrix")
            .and_then(|a| {
                let v: Vec<f64> = a.iter().filter_map(|o| o.as_f64()).collect();
                if v.len() == 6 {
                    Some(Matrix::new(v[0], v[1], v[2], v[3], v[4], v[5]))
                } else {
                    None
                }
            })
            .unwrap_or_else(Matrix::identity);

        // Render as a Form XObject with the computed transform
        self.gstate_stack.push(self.gstate.clone());
        let saved_resources = self.resources.clone();
        // Set up resources from the form
        if let Some(res_obj) = form_dict.get(b"Resources") {
            if let Ok(PdfObj::Dict(d)) = self.resolver.deref(res_obj) {
                self.resources = d;
            }
        }

        // Apply CTM: page CTM → bbox_to_rect → form_matrix
        self.gstate.ctm = self.gstate.ctm.concat(&bbox_to_rect).concat(&form_matrix);

        // Note: no BBox clip here — appearance streams do their own internal clipping.

        // Interpret the form content
        let form_data = self.resolver.stream_data_from_obj(n_ref)?;
        self.depth += 1;
        let _ = self.interpret_stream(&form_data);
        self.depth -= 1;

        // Restore state
        self.resources = saved_resources;
        if let Some(gs) = self.gstate_stack.pop() {
            self.gstate = gs;
        }

        // Restore clip in display list (annotation may have modified clip state)
        self.display_list.push(DisplayElement::InitClip);
        if let Some(ref clip) = self.gstate.clip_path {
            self.display_list.push(DisplayElement::Clip {
                path: clip.clone(),
                params: ClipParams {
                    fill_rule: FillRule::NonZeroWinding,
                    ctm: Matrix::identity(),
                },
            });
        }

        Ok(())
    }

    /// Interpret content stream bytes (can be called recursively for Form XObjects).
    fn interpret_stream(&mut self, data: &[u8]) -> Result<(), PdfError> {
        let mut lexer = Lexer::new(data);
        loop {
            let tok = match lexer.next_token() {
                Ok(t) => t,
                Err(_) => {
                    // Skip unrecognized bytes (e.g., stray '*' after whitespace)
                    continue;
                }
            };
            match tok {
                Token::Eof => break,
                Token::Int(n) => self.operand_stack.push(Operand::Int(n)),
                Token::Real(f) => self.operand_stack.push(Operand::Real(f)),
                Token::Name(n) => self.operand_stack.push(Operand::Name(n)),
                Token::LitString(s) | Token::HexString(s) => {
                    self.operand_stack.push(Operand::Str(s));
                }
                Token::Bool(b) => self.operand_stack.push(Operand::Bool(b)),
                Token::ArrayBegin => {
                    let arr = Self::parse_inline_array(&mut lexer)?;
                    self.operand_stack.push(Operand::Array(arr));
                }
                Token::DictBegin => {
                    let dict = crate::lexer::parse_dict_body(&mut lexer)?;
                    self.operand_stack.push(Operand::Dict(dict));
                }
                Token::Keyword(kw) => {
                    // Check for * suffix (f*, B*, b*, W*, T*)
                    let op = if matches!(kw.as_slice(), b"f" | b"B" | b"b" | b"W" | b"T") {
                        let p = lexer.pos();
                        if p < data.len() && data[p] == b'*' {
                            lexer.set_pos(p + 1);
                            let mut combined = kw;
                            combined.push(b'*');
                            combined
                        } else {
                            kw
                        }
                    } else {
                        kw
                    };

                    if op == b"BI" {
                        self.handle_inline_image(&mut lexer)?;
                    } else {
                        self.dispatch_operator(&op)?;
                    }
                    self.operand_stack.clear();
                }
                Token::DictEnd | Token::ArrayEnd => {
                    // Stray delimiters — ignore
                }
            }
        }
        Ok(())
    }

    /// Parse an inline array from the content stream.
    fn parse_inline_array(lexer: &mut Lexer) -> Result<Vec<PdfObj>, PdfError> {
        let mut elems = Vec::new();
        loop {
            let tok = lexer.next_token()?;
            match tok {
                Token::ArrayEnd | Token::Eof => break,
                Token::Int(n) => elems.push(PdfObj::Int(n)),
                Token::Real(f) => elems.push(PdfObj::Real(f)),
                Token::Name(n) => elems.push(PdfObj::Name(n)),
                Token::LitString(s) | Token::HexString(s) => elems.push(PdfObj::Str(s)),
                Token::Bool(b) => elems.push(PdfObj::Bool(b)),
                Token::ArrayBegin => {
                    let sub = Self::parse_inline_array(lexer)?;
                    elems.push(PdfObj::Array(sub));
                }
                _ => {}
            }
        }
        Ok(elems)
    }

    /// Dispatch a PDF content stream operator.
    fn dispatch_operator(&mut self, op: &[u8]) -> Result<(), PdfError> {
        match op {
            // Graphics state
            b"q" => self.op_q(),
            b"Q" => self.op_big_q(),
            b"cm" => self.op_cm(),
            b"w" => self.op_w(),
            b"J" => self.op_big_j(),
            b"j" => self.op_j(),
            b"M" => self.op_big_m(),
            b"d" => self.op_d(),
            b"ri" => self.op_ri(),
            b"i" => self.op_i(),
            b"gs" => self.op_gs(),

            // Path construction
            b"m" => self.op_m(),
            b"l" => self.op_l(),
            b"c" => self.op_c(),
            b"v" => self.op_v(),
            b"y" => self.op_y(),
            b"h" => self.op_h(),
            b"re" => self.op_re(),

            // Path painting
            b"S" => self.op_big_s(),
            b"s" => self.op_small_s(),
            b"f" | b"F" => self.op_f(),
            b"f*" => self.op_f_star(),
            b"B" => self.op_big_b(),
            b"B*" => self.op_big_b_star(),
            b"b" => self.op_small_b(),
            b"b*" => self.op_small_b_star(),
            b"n" => self.op_n(),

            // Clipping
            b"W" => self.op_big_w(),
            b"W*" => self.op_big_w_star(),

            // Color - device
            b"G" => self.op_big_g(),
            b"g" => self.op_small_g(),
            b"RG" => self.op_big_rg(),
            b"rg" => self.op_small_rg(),
            b"K" => self.op_big_k(),
            b"k" => self.op_small_k(),

            // Color - general
            b"CS" => self.op_big_cs(),
            b"cs" => self.op_small_cs(),
            b"SC" | b"SCN" => self.op_sc_stroke(),
            b"sc" | b"scn" => self.op_sc_fill(),

            // Text operators
            b"BT" => {
                self.in_text = true;
                self.gstate.text_matrix = Matrix::identity();
                self.gstate.text_line_matrix = Matrix::identity();
                Ok(())
            }
            b"ET" => {
                self.in_text = false;
                // Apply accumulated text clip path (from rendering modes 4-7)
                if let Some(clip_path) = self.text_clip_path.take() {
                    if !clip_path.is_empty() {
                        self.display_list.push(DisplayElement::Clip {
                            path: clip_path.clone(),
                            params: ClipParams {
                                fill_rule: FillRule::NonZeroWinding,
                                ctm: Matrix::identity(),
                            },
                        });
                        // Track in graphics state so Q/grestore can undo it
                        self.gstate.clip_path = Some(clip_path);
                        self.gstate.clip_path_version += 1;
                    }
                }
                Ok(())
            }
            b"Tf" => self.op_tf(),
            b"Tc" => {
                self.gstate.char_spacing = self.pop_number()?;
                Ok(())
            }
            b"Tw" => {
                self.gstate.word_spacing = self.pop_number()?;
                Ok(())
            }
            b"TL" => {
                self.gstate.text_leading = self.pop_number()?;
                Ok(())
            }
            b"Tr" => {
                self.gstate.text_rendering_mode = self.pop_number()? as i32;
                Ok(())
            }
            b"Ts" => {
                self.gstate.text_rise = self.pop_number()?;
                Ok(())
            }
            b"Td" => self.op_td(),
            b"TD" => self.op_big_td(),
            b"Tm" => self.op_tm(),
            b"T*" => self.op_t_star(),
            b"Tj" => self.op_tj(),
            b"TJ" => self.op_big_tj(),
            b"'" => self.op_quote(),
            b"\"" => self.op_dblquote(),

            // XObject
            b"Do" => self.op_do(),

            // Shading
            b"sh" => self.op_sh(),

            // Marked content (no-op)
            b"BMC" | b"BDC" | b"EMC" | b"MP" | b"DP" => Ok(()),

            // Type 3 glyph operators (width/cache — we use Widths array instead)
            b"d0" | b"d1" => Ok(()),

            // Compatibility (no-op)
            b"BX" | b"EX" => Ok(()),

            _ => {
                // Unknown operator — ignore
                Ok(())
            }
        }
    }

    // === Helper methods ===

    /// Pop one number from the operand stack.
    fn pop_number(&self) -> Result<f64, PdfError> {
        self.operand_stack
            .last()
            .and_then(|o| o.as_f64())
            .ok_or(PdfError::Other("expected number on operand stack".into()))
    }

    /// Get N numbers from the end of the operand stack.
    fn get_numbers(&self, n: usize) -> Result<Vec<f64>, PdfError> {
        let len = self.operand_stack.len();
        if len < n {
            return Err(PdfError::Other(format!("need {n} operands, have {len}")));
        }
        let mut nums = Vec::with_capacity(n);
        for i in (len - n)..len {
            nums.push(
                self.operand_stack[i]
                    .as_f64()
                    .ok_or(PdfError::Other("expected number".into()))?,
            );
        }
        Ok(nums)
    }

    /// Transform a point through the current CTM to device space.
    fn transform(&self, x: f64, y: f64) -> (f64, f64) {
        self.gstate.ctm.transform_point(x, y)
    }

    /// Take the current path and reset it.
    fn take_path(&mut self) -> PsPath {
        let path = std::mem::take(&mut self.current_path);
        self.current_point = None;
        self.subpath_start = None;
        path
    }

    /// Apply pending clip if set, then clear it.
    fn apply_pending_clip(&mut self) {
        if let Some((path, fill_rule)) = self.gstate.pending_clip.take() {
            self.display_list.push(DisplayElement::Clip {
                path: path.clone(),
                params: ClipParams {
                    fill_rule,
                    ctm: Matrix::identity(),
                },
            });
            // Track the clip for restoring on Q
            self.gstate.clip_path = Some(path);
            self.gstate.clip_path_version += 1;
        }
    }

    // === Graphics state operators ===

    fn op_q(&mut self) -> Result<(), PdfError> {
        self.gstate_stack.push(self.gstate.clone());
        Ok(())
    }

    fn op_big_q(&mut self) -> Result<(), PdfError> {
        if let Some(saved) = self.gstate_stack.pop() {
            // Flush soft mask scope before restoring state — the restored
            // state may have a different (or no) soft mask.
            let restored_has_smask = saved.soft_mask.is_some();
            let current_has_smask = self.gstate.soft_mask.is_some();
            if current_has_smask && !restored_has_smask {
                self.flush_soft_mask();
            }

            let old_clip_version = self.gstate.clip_path_version;
            self.gstate = saved;
            // If clip changed during the q/Q block, restore it
            if self.gstate.clip_path_version != old_clip_version {
                self.display_list.push(DisplayElement::InitClip);
                if let Some(ref clip) = self.gstate.clip_path {
                    self.display_list.push(DisplayElement::Clip {
                        path: clip.clone(),
                        params: ClipParams {
                            fill_rule: FillRule::NonZeroWinding,
                            ctm: Matrix::identity(),
                        },
                    });
                }
            }
        }
        Ok(())
    }

    fn op_cm(&mut self) -> Result<(), PdfError> {
        let n = self.get_numbers(6)?;
        let m = Matrix::new(n[0], n[1], n[2], n[3], n[4], n[5]);
        // PDF cm: CTM = CTM × M (pre-multiply, same as PS concat)
        self.gstate.ctm = self.gstate.ctm.concat(&m);
        Ok(())
    }

    fn op_w(&mut self) -> Result<(), PdfError> {
        self.gstate.line_width = self.pop_number()?;
        Ok(())
    }

    fn op_big_j(&mut self) -> Result<(), PdfError> {
        let cap = self.pop_number()? as i32;
        if let Some(lc) = LineCap::from_i32(cap) {
            self.gstate.line_cap = lc;
        }
        Ok(())
    }

    fn op_j(&mut self) -> Result<(), PdfError> {
        let join = self.pop_number()? as i32;
        if let Some(lj) = LineJoin::from_i32(join) {
            self.gstate.line_join = lj;
        }
        Ok(())
    }

    fn op_big_m(&mut self) -> Result<(), PdfError> {
        self.gstate.miter_limit = self.pop_number()?;
        Ok(())
    }

    fn op_d(&mut self) -> Result<(), PdfError> {
        // Operands: array offset
        let len = self.operand_stack.len();
        if len < 2 {
            return Ok(());
        }
        let offset = self.operand_stack[len - 1].as_f64().unwrap_or(0.0);
        let array = match &self.operand_stack[len - 2] {
            Operand::Array(arr) => arr.iter().filter_map(|o| o.as_f64()).collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        self.gstate.dash_pattern = DashPattern { array, offset };
        Ok(())
    }

    fn op_ri(&mut self) -> Result<(), PdfError> {
        // Rendering intent — record but don't enforce
        Ok(())
    }

    fn op_i(&mut self) -> Result<(), PdfError> {
        self.gstate.flatness = self.pop_number()?;
        Ok(())
    }

    fn op_gs(&mut self) -> Result<(), PdfError> {
        let name = self
            .operand_stack
            .last()
            .and_then(|o| o.as_name())
            .ok_or(PdfError::Other("gs: expected name".into()))?
            .to_vec();
        self.apply_ext_gstate(&name)
    }

    // === Path construction operators ===

    fn op_m(&mut self) -> Result<(), PdfError> {
        let n = self.get_numbers(2)?;
        let (dx, dy) = self.transform(n[0], n[1]);
        self.current_path.segments.push(PathSegment::MoveTo(dx, dy));
        self.current_point = Some((dx, dy));
        self.subpath_start = Some((dx, dy));
        Ok(())
    }

    fn op_l(&mut self) -> Result<(), PdfError> {
        let n = self.get_numbers(2)?;
        let (dx, dy) = self.transform(n[0], n[1]);
        self.current_path.segments.push(PathSegment::LineTo(dx, dy));
        self.current_point = Some((dx, dy));
        Ok(())
    }

    fn op_c(&mut self) -> Result<(), PdfError> {
        let n = self.get_numbers(6)?;
        let (x1, y1) = self.transform(n[0], n[1]);
        let (x2, y2) = self.transform(n[2], n[3]);
        let (x3, y3) = self.transform(n[4], n[5]);
        self.current_path.segments.push(PathSegment::CurveTo {
            x1,
            y1,
            x2,
            y2,
            x3,
            y3,
        });
        self.current_point = Some((x3, y3));
        Ok(())
    }

    fn op_v(&mut self) -> Result<(), PdfError> {
        let n = self.get_numbers(4)?;
        let (x1, y1) = self.current_point.unwrap_or((0.0, 0.0));
        let (x2, y2) = self.transform(n[0], n[1]);
        let (x3, y3) = self.transform(n[2], n[3]);
        self.current_path.segments.push(PathSegment::CurveTo {
            x1,
            y1,
            x2,
            y2,
            x3,
            y3,
        });
        self.current_point = Some((x3, y3));
        Ok(())
    }

    fn op_y(&mut self) -> Result<(), PdfError> {
        let n = self.get_numbers(4)?;
        let (x1, y1) = self.transform(n[0], n[1]);
        let (x3, y3) = self.transform(n[2], n[3]);
        self.current_path.segments.push(PathSegment::CurveTo {
            x1,
            y1,
            x2: x3,
            y2: y3,
            x3,
            y3,
        });
        self.current_point = Some((x3, y3));
        Ok(())
    }

    fn op_h(&mut self) -> Result<(), PdfError> {
        self.current_path.segments.push(PathSegment::ClosePath);
        if let Some(start) = self.subpath_start {
            self.current_point = Some(start);
        }
        Ok(())
    }

    fn op_re(&mut self) -> Result<(), PdfError> {
        let n = self.get_numbers(4)?;
        let (x, y, w, h) = (n[0], n[1], n[2], n[3]);
        // re builds: m x y, l x+w y, l x+w y+h, l x y+h, h
        let p0 = self.transform(x, y);
        let p1 = self.transform(x + w, y);
        let p2 = self.transform(x + w, y + h);
        let p3 = self.transform(x, y + h);
        self.current_path
            .segments
            .push(PathSegment::MoveTo(p0.0, p0.1));
        self.current_path
            .segments
            .push(PathSegment::LineTo(p1.0, p1.1));
        self.current_path
            .segments
            .push(PathSegment::LineTo(p2.0, p2.1));
        self.current_path
            .segments
            .push(PathSegment::LineTo(p3.0, p3.1));
        self.current_path.segments.push(PathSegment::ClosePath);
        self.current_point = Some(p0);
        self.subpath_start = Some(p0);
        Ok(())
    }

    // === Path painting operators ===

    fn op_big_s(&mut self) -> Result<(), PdfError> {
        // S: stroke
        let path = self.take_path();
        if !path.is_empty() {
            self.emit_stroke(path);
        }
        self.apply_pending_clip();
        Ok(())
    }

    fn op_small_s(&mut self) -> Result<(), PdfError> {
        // s: close and stroke
        self.op_h()?;
        self.op_big_s()
    }

    fn op_f(&mut self) -> Result<(), PdfError> {
        // f/F: fill (non-zero winding)
        let path = self.take_path();
        if !path.is_empty() {
            self.emit_fill(path, FillRule::NonZeroWinding);
        }
        self.apply_pending_clip();
        Ok(())
    }

    fn op_f_star(&mut self) -> Result<(), PdfError> {
        // f*: fill (even-odd)
        let path = self.take_path();
        if !path.is_empty() {
            self.emit_fill(path, FillRule::EvenOdd);
        }
        self.apply_pending_clip();
        Ok(())
    }

    fn op_big_b(&mut self) -> Result<(), PdfError> {
        // B: fill (non-zero) + stroke
        let path = self.take_path();
        if !path.is_empty() {
            self.emit_fill(path.clone(), FillRule::NonZeroWinding);
            self.emit_stroke(path);
        }
        self.apply_pending_clip();
        Ok(())
    }

    fn op_big_b_star(&mut self) -> Result<(), PdfError> {
        // B*: fill (even-odd) + stroke
        let path = self.take_path();
        if !path.is_empty() {
            self.emit_fill(path.clone(), FillRule::EvenOdd);
            self.emit_stroke(path);
        }
        self.apply_pending_clip();
        Ok(())
    }

    fn op_small_b(&mut self) -> Result<(), PdfError> {
        // b: close, fill (non-zero), stroke
        self.op_h()?;
        self.op_big_b()
    }

    fn op_small_b_star(&mut self) -> Result<(), PdfError> {
        // b*: close, fill (even-odd), stroke
        self.op_h()?;
        self.op_big_b_star()
    }

    /// Emit a fill — either a pattern fill or a regular solid fill.
    fn emit_fill(&mut self, path: PsPath, fill_rule: FillRule) {
        if let Some(shading_box) = self.gstate.fill_shading_pattern.clone() {
            // PatternType 2 (shading pattern): clip to fill path, then emit shading.
            // The surrounding q/Q scope restores the clip when the content block ends.
            self.display_list.push(DisplayElement::Clip {
                path,
                params: ClipParams {
                    fill_rule,
                    ctm: Matrix::identity(),
                },
            });
            // Update clip tracking so Q knows to restore
            self.gstate.clip_path_version += 1;
            for elem in shading_box.0.elements() {
                self.display_list.push(elem.clone());
            }
        } else if let Some(pattern) = self.gstate.fill_pattern.clone() {
            self.display_list.push(DisplayElement::PatternFill {
                params: PatternFillParams {
                    path,
                    fill_rule,
                    tile: pattern.tile,
                    pattern_matrix: pattern.pattern_matrix,
                    bbox: pattern.bbox,
                    xstep: pattern.x_step,
                    ystep: pattern.y_step,
                    paint_type: pattern.paint_type,
                    underlying_color: if pattern.paint_type == 2 {
                        Some(self.gstate.fill_color.clone())
                    } else {
                        None
                    },
                    pattern_id: pattern.pattern_id,
                },
            });
        } else {
            self.display_list.push(DisplayElement::Fill {
                path,
                params: self.gstate.fill_params(fill_rule),
            });
        }
    }

    /// Emit a stroke with proper CTM-aware line width.
    ///
    /// Paths are stored in device space, but strokes need to be applied in user
    /// space for correct anisotropic scaling (non-uniform CTMs make circles into
    /// ellipses, and the stroke width should follow that transformation).
    /// We inverse-transform the path back to user space and pass the CTM to the
    /// renderer so it can apply the stroke correctly.
    fn emit_stroke(&mut self, path: PsPath) {
        let ctm = self.gstate.ctm;
        // Inverse-transform path from device space back to user space
        let user_path = if let Some(inv) = ctm.invert() {
            path.transform(&inv)
        } else {
            // Degenerate CTM — fall back to device-space stroke
            path
        };

        let mut params = self.gstate.stroke_params_with_ctm();
        params.ctm = ctm;
        self.display_list.push(DisplayElement::Stroke {
            path: user_path,
            params,
        });
    }

    fn op_n(&mut self) -> Result<(), PdfError> {
        // n: end path (no paint) — used for clip-only paths
        let _path = self.take_path();
        self.apply_pending_clip();
        Ok(())
    }

    // === Clipping operators ===

    fn op_big_w(&mut self) -> Result<(), PdfError> {
        // W: clip (non-zero winding), deferred to next paint op
        self.gstate.pending_clip = Some((self.current_path.clone(), FillRule::NonZeroWinding));
        Ok(())
    }

    fn op_big_w_star(&mut self) -> Result<(), PdfError> {
        // W*: clip (even-odd), deferred to next paint op
        self.gstate.pending_clip = Some((self.current_path.clone(), FillRule::EvenOdd));
        Ok(())
    }

    // === Device color operators ===

    fn op_big_g(&mut self) -> Result<(), PdfError> {
        // G gray: set stroke color to gray
        let g = self.pop_number()?;
        self.gstate.stroke_color = DeviceColor::from_gray(g);
        self.gstate.stroke_color_space = ColorSpaceRef::DeviceGray;
        self.gstate.stroke_painted_channels = 0;
        Ok(())
    }

    fn op_small_g(&mut self) -> Result<(), PdfError> {
        // g gray: set fill color to gray
        let g = self.pop_number()?;
        self.gstate.fill_color = DeviceColor::from_gray(g);
        self.gstate.fill_color_space = ColorSpaceRef::DeviceGray;
        self.gstate.fill_painted_channels = 0;
        self.gstate.fill_is_device_cmyk = false;
        self.gstate.fill_pattern = None;
        self.gstate.fill_shading_pattern = None;
        Ok(())
    }

    fn op_big_rg(&mut self) -> Result<(), PdfError> {
        // RG r g b: set stroke color to RGB
        let n = self.get_numbers(3)?;
        self.gstate.stroke_color = DeviceColor::from_rgb(n[0], n[1], n[2]);
        self.gstate.stroke_color_space = ColorSpaceRef::DeviceRGB;
        self.gstate.stroke_painted_channels = 0;
        Ok(())
    }

    fn op_small_rg(&mut self) -> Result<(), PdfError> {
        // rg r g b: set fill color to RGB
        let n = self.get_numbers(3)?;
        self.gstate.fill_color = DeviceColor::from_rgb(n[0], n[1], n[2]);
        self.gstate.fill_color_space = ColorSpaceRef::DeviceRGB;
        self.gstate.fill_painted_channels = 0;
        self.gstate.fill_is_device_cmyk = false;
        self.gstate.fill_pattern = None;
        self.gstate.fill_shading_pattern = None;
        Ok(())
    }

    fn op_big_k(&mut self) -> Result<(), PdfError> {
        // K c m y k: set stroke color to CMYK
        let n = self.get_numbers(4)?;
        self.gstate.stroke_color =
            DeviceColor::from_cmyk_icc(n[0], n[1], n[2], n[3], &mut self.icc_cache);
        self.gstate.stroke_color_space = ColorSpaceRef::DeviceCMYK;
        self.gstate.stroke_painted_channels = stet_core::device::CMYK_ALL;
        Ok(())
    }

    fn op_small_k(&mut self) -> Result<(), PdfError> {
        // k c m y k: set fill color to CMYK
        let n = self.get_numbers(4)?;
        self.gstate.fill_color =
            DeviceColor::from_cmyk_icc(n[0], n[1], n[2], n[3], &mut self.icc_cache);
        self.gstate.fill_color_space = ColorSpaceRef::DeviceCMYK;
        self.gstate.fill_painted_channels = stet_core::device::CMYK_ALL;
        self.gstate.fill_is_device_cmyk = true;
        self.gstate.fill_pattern = None;
        self.gstate.fill_shading_pattern = None;
        Ok(())
    }

    // === General color operators ===

    fn op_big_cs(&mut self) -> Result<(), PdfError> {
        // CS name: set stroke color space
        let name = self
            .operand_stack
            .last()
            .and_then(|o| o.as_name())
            .ok_or(PdfError::Other("CS: expected name".into()))?
            .to_vec();
        self.gstate.stroke_color_space = name_to_cs_ref(&name);
        Ok(())
    }

    fn op_small_cs(&mut self) -> Result<(), PdfError> {
        // cs name: set fill color space
        let name = self
            .operand_stack
            .last()
            .and_then(|o| o.as_name())
            .ok_or(PdfError::Other("cs: expected name".into()))?
            .to_vec();
        self.gstate.fill_color_space = name_to_cs_ref(&name);
        Ok(())
    }

    fn op_sc_stroke(&mut self) -> Result<(), PdfError> {
        // SC/SCN: set stroke color in current color space
        let cs = resolve_color_space(
            &self.gstate.stroke_color_space,
            &self.resources,
            self.resolver,
        )?;
        if matches!(cs, ResolvedColorSpace::Pattern) {
            return self.handle_pattern_stroke();
        }
        let n = cs.num_components();
        if n == 0 {
            return Ok(());
        }
        let nums = self.get_numbers(n)?;
        self.gstate.stroke_painted_channels = painted_channels_for_cs(&cs);
        self.gstate.stroke_color =
            components_to_device_color_icc(&cs, &nums, Some(&mut self.icc_cache));
        Ok(())
    }

    fn op_sc_fill(&mut self) -> Result<(), PdfError> {
        // sc/scn: set fill color in current color space
        let cs = resolve_color_space(
            &self.gstate.fill_color_space,
            &self.resources,
            self.resolver,
        )?;
        if matches!(cs, ResolvedColorSpace::Pattern) {
            return self.handle_pattern_fill();
        }
        let n = cs.num_components();
        if n == 0 {
            return Ok(());
        }
        let nums = self.get_numbers(n)?;
        self.gstate.fill_painted_channels = painted_channels_for_cs(&cs);
        self.gstate.fill_is_device_cmyk = matches!(
            cs,
            ResolvedColorSpace::DeviceCMYK | ResolvedColorSpace::ICCBased { n: 4, .. }
        );
        self.gstate.fill_color =
            components_to_device_color_icc(&cs, &nums, Some(&mut self.icc_cache));
        self.gstate.fill_pattern = None;
        self.gstate.fill_shading_pattern = None;
        Ok(())
    }

    // === Text operators (state recording, Phase C will add rendering) ===

    fn op_tf(&mut self) -> Result<(), PdfError> {
        // Tf font size
        let len = self.operand_stack.len();
        if len < 2 {
            return Ok(());
        }
        self.gstate.font_size = self.operand_stack[len - 1].as_f64().unwrap_or(12.0);
        if let Some(name) = self.operand_stack[len - 2].as_name() {
            let name = name.to_vec();
            self.gstate.text_font_name = name.clone();
            self.resolve_current_font(&name);
        }
        Ok(())
    }

    /// Resolve the current font by name from the font cache or resources.
    fn resolve_current_font(&mut self, name: &[u8]) {
        // Check cache first
        if let Some(cached) = self.font_cache.get(name) {
            self.current_font = Some(Arc::clone(cached));
            return;
        }

        // Look up in resources /Font dict (may be an indirect reference)
        let font_ref = self
            .resolve_resource_subdict(b"Font")
            .and_then(|fd| fd.get(name).cloned());
        let font_ref = match font_ref {
            Some(r) => r,
            None => {
                // Font resource missing — try loading a default substitution font
                if let Some(fallback) = font::fallback_font(self.font_provider.as_ref()) {
                    let arc = Arc::new(fallback);
                    self.font_cache.insert(name.to_vec(), Arc::clone(&arc));
                    self.current_font = Some(arc);
                } else {
                    self.current_font = None;
                }
                return;
            }
        };

        match font::resolve_font(self.resolver, &font_ref, self.font_provider.as_ref()) {
            Ok(font) => {
                let arc = Arc::new(font);
                self.font_cache.insert(name.to_vec(), Arc::clone(&arc));
                self.current_font = Some(arc);
            }
            Err(e) => {
                eprintln!("warning: font /{}: {}", String::from_utf8_lossy(name), e);
                // Try fallback font on resolution failure too
                if let Some(fallback) = font::fallback_font(self.font_provider.as_ref()) {
                    let arc = Arc::new(fallback);
                    self.font_cache.insert(name.to_vec(), Arc::clone(&arc));
                    self.current_font = Some(arc);
                } else {
                    self.current_font = None;
                }
            }
        }
    }

    fn op_td(&mut self) -> Result<(), PdfError> {
        let n = self.get_numbers(2)?;
        let m = Matrix::translate(n[0], n[1]);
        self.gstate.text_line_matrix = self.gstate.text_line_matrix.concat(&m);
        self.gstate.text_matrix = self.gstate.text_line_matrix;
        Ok(())
    }

    fn op_big_td(&mut self) -> Result<(), PdfError> {
        let n = self.get_numbers(2)?;
        self.gstate.text_leading = -n[1];
        let m = Matrix::translate(n[0], n[1]);
        self.gstate.text_line_matrix = self.gstate.text_line_matrix.concat(&m);
        self.gstate.text_matrix = self.gstate.text_line_matrix;
        Ok(())
    }

    fn op_tm(&mut self) -> Result<(), PdfError> {
        let n = self.get_numbers(6)?;
        let m = Matrix::new(n[0], n[1], n[2], n[3], n[4], n[5]);
        self.gstate.text_matrix = m;
        self.gstate.text_line_matrix = m;
        Ok(())
    }

    fn op_t_star(&mut self) -> Result<(), PdfError> {
        let leading = self.gstate.text_leading;
        let m = Matrix::translate(0.0, -leading);
        self.gstate.text_line_matrix = self.gstate.text_line_matrix.concat(&m);
        self.gstate.text_matrix = self.gstate.text_line_matrix;
        Ok(())
    }

    // === Text rendering operators ===

    fn op_tj(&mut self) -> Result<(), PdfError> {
        let text = match self.operand_stack.last() {
            Some(Operand::Str(s)) => s.clone(),
            _ => return Ok(()),
        };
        self.show_text(&text);
        Ok(())
    }

    fn op_big_tj(&mut self) -> Result<(), PdfError> {
        let arr = match self.operand_stack.last() {
            Some(Operand::Array(a)) => a.clone(),
            _ => return Ok(()),
        };
        for elem in &arr {
            match elem {
                PdfObj::Str(s) => self.show_text(s),
                PdfObj::Int(n) => {
                    // Negative number → shift right, positive → shift left
                    let shift = -*n as f64 / 1000.0 * self.gstate.font_size;
                    let m = Matrix::translate(shift, 0.0);
                    self.gstate.text_matrix = self.gstate.text_matrix.concat(&m);
                }
                PdfObj::Real(f) => {
                    let shift = -f / 1000.0 * self.gstate.font_size;
                    let m = Matrix::translate(shift, 0.0);
                    self.gstate.text_matrix = self.gstate.text_matrix.concat(&m);
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn op_quote(&mut self) -> Result<(), PdfError> {
        // ': T* then Tj
        self.op_t_star()?;
        self.op_tj()
    }

    fn op_dblquote(&mut self) -> Result<(), PdfError> {
        // ": set word_spacing, char_spacing, then T* + Tj
        let len = self.operand_stack.len();
        if len < 3 {
            return Ok(());
        }
        self.gstate.word_spacing = self.operand_stack[len - 3].as_f64().unwrap_or(0.0);
        self.gstate.char_spacing = self.operand_stack[len - 2].as_f64().unwrap_or(0.0);
        // The string is at len-1, which op_tj reads from last()
        self.op_t_star()?;
        self.op_tj()
    }

    /// Render a text string by emitting glyph paths as Fill display elements.
    fn show_text(&mut self, text: &[u8]) {
        let font = match &self.current_font {
            Some(f) => Arc::clone(f),
            None => return,
        };

        let font_size = self.gstate.font_size;
        let char_spacing = self.gstate.char_spacing;
        let word_spacing = self.gstate.word_spacing;
        let text_rise = self.gstate.text_rise;
        let font_matrix = font.font_matrix();
        let render_mode = self.gstate.text_rendering_mode;

        if font.is_composite() {
            // Composite (CID) font: 2-byte character codes
            let mut i = 0;
            while i + 1 < text.len() {
                let cid = ((text[i] as u16) << 8) | (text[i + 1] as u16);
                i += 2;

                if let Some(glyph_path) = font.glyph_path_cid(cid) {
                    let text_state_matrix =
                        Matrix::new(font_size, 0.0, 0.0, font_size, 0.0, text_rise);
                    let trm = self
                        .gstate
                        .ctm
                        .concat(&self.gstate.text_matrix)
                        .concat(&text_state_matrix)
                        .concat(&font_matrix);
                    let device_path = glyph_path.transform(&trm);
                    if !device_path.is_empty() {
                        self.emit_text_glyph(device_path, render_mode);
                    }
                }

                let w0 = font.glyph_width_cid(cid);
                let tx = w0 * font_size + char_spacing;
                let advance = Matrix::translate(tx, 0.0);
                self.gstate.text_matrix = self.gstate.text_matrix.concat(&advance);
            }
        } else if font.is_type3() {
            // Type 3 font: each glyph is a content stream.
            // Widths are in glyph space — scale by font matrix to get text space.
            let fm = font.font_matrix();
            for &byte in text {
                self.show_type3_glyph(&font, byte);

                let w0_glyph = font.glyph_width(byte);
                let w0 = w0_glyph * fm.a;
                let mut tx = w0 * font_size + char_spacing;
                if byte == b' ' {
                    tx += word_spacing;
                }
                let advance = Matrix::translate(tx, 0.0);
                self.gstate.text_matrix = self.gstate.text_matrix.concat(&advance);
            }
        } else {
            // Simple font: 1-byte character codes
            for &byte in text {
                if let Some(glyph_path) = font.glyph_path(byte) {
                    let text_state_matrix =
                        Matrix::new(font_size, 0.0, 0.0, font_size, 0.0, text_rise);
                    let trm = self
                        .gstate
                        .ctm
                        .concat(&self.gstate.text_matrix)
                        .concat(&text_state_matrix)
                        .concat(&font_matrix);
                    let device_path = glyph_path.transform(&trm);
                    if !device_path.is_empty() {
                        self.emit_text_glyph(device_path, render_mode);
                    }
                }

                let w0 = font.glyph_width(byte);
                let mut tx = w0 * font_size + char_spacing;
                if byte == b' ' {
                    tx += word_spacing;
                }
                let advance = Matrix::translate(tx, 0.0);
                self.gstate.text_matrix = self.gstate.text_matrix.concat(&advance);
            }
        }
    }

    /// Render a single Type 3 glyph by interpreting its CharProc content stream.
    fn show_type3_glyph(&mut self, font: &PdfFont, char_code: u8) {
        let proc_data = match font.type3_char_proc(char_code) {
            Some(data) => data.to_vec(),
            None => return,
        };
        let resources = match font.type3_resources() {
            Some(r) => r.clone(),
            None => return,
        };

        let font_size = self.gstate.font_size;
        let text_rise = self.gstate.text_rise;
        let font_matrix = font.font_matrix();

        // Build the text rendering matrix: CTM × Tm × [fontSize 0 0 fontSize 0 rise] × FontMatrix
        let text_state_matrix =
            Matrix::new(font_size, 0.0, 0.0, font_size, 0.0, text_rise);
        let trm = self
            .gstate
            .ctm
            .concat(&self.gstate.text_matrix)
            .concat(&text_state_matrix)
            .concat(&font_matrix);

        // Interpret the CharProc stream with TRM as the CTM.
        // Save current state and swap in a fresh display list.
        self.gstate_stack.push(self.gstate.clone());
        let saved_resources = std::mem::replace(&mut self.resources, resources);
        let saved_display_list = std::mem::take(&mut self.display_list);
        let saved_path = std::mem::take(&mut self.current_path);
        let saved_point = self.current_point.take();
        let saved_subpath = self.subpath_start.take();

        self.gstate.ctm = trm;

        self.depth += 1;
        let _ = self.interpret_stream(&proc_data);
        self.depth -= 1;

        // Collect glyph display elements and append to main display list
        let glyph_elements = std::mem::replace(&mut self.display_list, saved_display_list);
        self.resources = saved_resources;
        self.current_path = saved_path;
        self.current_point = saved_point;
        self.subpath_start = saved_subpath;
        if let Some(saved) = self.gstate_stack.pop() {
            self.gstate = saved;
        }

        // Append all glyph elements to the main display list
        for elem in glyph_elements.into_elements() {
            self.display_list.push(elem);
        }
    }

    /// Emit a text glyph to the display list based on the text rendering mode.
    ///
    /// Modes: 0=fill, 1=stroke, 2=fill+stroke, 3=invisible,
    ///        4-7=same as 0-3 but add to clipping path (clipping not yet implemented).
    fn emit_text_glyph(&mut self, device_path: PsPath, render_mode: i32) {
        let mode = render_mode & 3; // strip clip bit
        let clip = render_mode & 4 != 0; // bit 2 = add to text clip

        match mode {
            0 => {
                // Fill only
                let mut params = self.gstate.fill_params(FillRule::NonZeroWinding);
                params.is_text_glyph = true;
                self.display_list.push(DisplayElement::Fill {
                    path: device_path.clone(),
                    params,
                });
            }
            1 => {
                // Stroke only
                let mut params = self.gstate.stroke_params();
                params.is_text_glyph = true;
                self.display_list.push(DisplayElement::Stroke {
                    path: device_path.clone(),
                    params,
                });
            }
            2 => {
                // Fill then stroke
                let mut fill_params = self.gstate.fill_params(FillRule::NonZeroWinding);
                fill_params.is_text_glyph = true;
                self.display_list.push(DisplayElement::Fill {
                    path: device_path.clone(),
                    params: fill_params,
                });
                let mut stroke_params = self.gstate.stroke_params();
                stroke_params.is_text_glyph = true;
                self.display_list.push(DisplayElement::Stroke {
                    path: device_path.clone(),
                    params: stroke_params,
                });
            }
            _ => {} // mode 3 = invisible
        }

        // Modes 4-7: accumulate glyph path into text clip
        if clip {
            let tcp = self.text_clip_path.get_or_insert_with(PsPath::new);
            tcp.segments.extend_from_slice(&device_path.segments);
        }
    }

    // === XObject operator ===

    fn op_do(&mut self) -> Result<(), PdfError> {
        let name = self
            .operand_stack
            .last()
            .and_then(|o| o.as_name())
            .ok_or(PdfError::Other("Do: expected name".into()))?
            .to_vec();

        // Look up XObject in resources (may be an indirect reference)
        let xobj_dict = self
            .resolve_resource_subdict(b"XObject")
            .ok_or(PdfError::Other("no XObject resources".into()))?;
        let xobj_ref = xobj_dict.get(&name).ok_or_else(|| {
            PdfError::Other(format!(
                "XObject /{} not found",
                String::from_utf8_lossy(&name)
            ))
        })?;
        // Keep the original ref for stream_data_from_obj (needed for encryption)
        let xobj_ref_clone = xobj_ref.clone();
        let xobj = self.resolver.deref(xobj_ref)?;
        let dict = xobj
            .as_dict()
            .ok_or(PdfError::Other("XObject is not a stream".into()))?;

        let subtype = dict.get_name(b"Subtype").unwrap_or(b"");
        match subtype {
            b"Image" => self.handle_image_xobject(&xobj_ref_clone, dict)?,
            b"Form" => self.handle_form_xobject(&xobj_ref_clone, dict)?,
            _ => {} // Ignore unknown subtypes
        }
        Ok(())
    }

    /// Handle an Image XObject.
    fn handle_image_xobject(&mut self, obj: &PdfObj, dict: &PdfDict) -> Result<(), PdfError> {
        let width = dict
            .get_int(b"Width")
            .ok_or(PdfError::Other("image missing Width".into()))? as u32;
        let height = dict
            .get_int(b"Height")
            .ok_or(PdfError::Other("image missing Height".into()))? as u32;

        // Check for image mask (1-bit stencil painted with current fill color)
        let is_image_mask = dict
            .get(b"ImageMask")
            .and_then(|o| match o {
                PdfObj::Bool(b) => Some(*b),
                _ => None,
            })
            .unwrap_or(false);

        let bpc = if is_image_mask {
            1
        } else {
            dict.get_int(b"BitsPerComponent").unwrap_or(8) as u32
        };

        // Resolve color space
        let resolved_cs = if is_image_mask {
            None
        } else if let Some(cs_obj) = dict.get(b"ColorSpace") {
            Some(resolve_color_space_obj(cs_obj, self.resolver)?)
        } else {
            Some(ResolvedColorSpace::DeviceRGB)
        };

        let color_space = if is_image_mask {
            // Image mask polarity from /Decode array:
            // [1 0] → polarity=true (raw bit 1 paints)
            // [0 1] → polarity=false (raw bit 0 paints) — this is the default
            let polarity = if let Some(arr) = dict.get_array(b"Decode") {
                let vals: Vec<f64> = arr.iter().filter_map(|o| o.as_f64()).collect();
                vals.len() >= 2 && vals[0] > 0.5
            } else {
                false
            };
            ImageColorSpace::Mask {
                color: self.gstate.fill_color.clone(),
                polarity,
            }
        } else {
            to_image_color_space(resolved_cs.as_ref().unwrap())
        };

        // Decode the stream data
        let sample_data = self.resolver.stream_data_from_obj(obj)?;

        // Image matrix: [width 0 0 -height 0 height] maps unit square to image
        let image_matrix =
            Matrix::new(width as f64, 0.0, 0.0, -(height as f64), 0.0, height as f64);

        let interpolate = dict
            .get(b"Interpolate")
            .and_then(|o| match o {
                PdfObj::Bool(b) => Some(*b),
                _ => None,
            })
            .unwrap_or(false);

        // Mask color (for color-key masking)
        let mask_color = dict.get_array(b"Mask").map(|arr| {
            arr.iter()
                .filter_map(|o| o.as_int().map(|n| n as u8))
                .collect()
        });

        // Convert data if BPC != 8 (but NOT for image masks — keep raw 1-bit data)
        let sample_data = if is_image_mask || bpc == 8 || bpc == 0 {
            sample_data
        } else {
            expand_bits_to_bytes(
                &sample_data,
                bpc,
                width,
                height,
                color_space.num_components(),
            )
        };

        // Apply /Decode array if present (maps sample values to color component values).
        // Default for most color spaces is [0 1 0 1 ...] (identity).
        // For Indexed color spaces, default is [0 2^bpc-1] and values are indices.
        // CMYK images may use [1 0 1 0 1 0 1 0] to invert values.
        let is_indexed = matches!(&color_space, ImageColorSpace::Indexed { .. });
        let sample_data = if !is_image_mask {
            if let Some(decode) = dict.get_array(b"Decode") {
                let n_comps = color_space.num_components() as usize;
                let decode_vals: Vec<f64> =
                    decode.iter().filter_map(|o| o.as_f64()).collect();
                if decode_vals.len() >= n_comps * 2 {
                    let max_sample = ((1u32 << bpc) - 1) as f64;
                    // Check if it's the default Decode for this color space.
                    // Indexed: default is [0 max_sample]; others: [0 1 0 1 ...].
                    let is_default = if is_indexed {
                        decode_vals.len() == 2
                            && (decode_vals[0]).abs() < 1e-6
                            && (decode_vals[1] - max_sample).abs() < 1e-6
                    } else {
                        decode_vals.chunks(2).all(|pair| {
                            pair.len() == 2
                                && (pair[0] - 0.0).abs() < 1e-6
                                && (pair[1] - 1.0).abs() < 1e-6
                        })
                    };
                    if !is_default {
                        // After expand_bits_to_bytes, all data is in 0-255 range
                        let max_val = 255.0f64;
                        let mut result = Vec::with_capacity(sample_data.len());
                        if is_indexed {
                            // Indexed: Decode maps sample values to index values (integer range)
                            let d_min = decode_vals[0];
                            let d_max = decode_vals[1];
                            for &sample in sample_data.iter() {
                                let val = d_min + (sample as f64 / max_val) * (d_max - d_min);
                                result.push(val.round().clamp(0.0, 255.0) as u8);
                            }
                        } else {
                            // Non-indexed: Decode maps to normalized [0,1] component values
                            for (i, &sample) in sample_data.iter().enumerate() {
                                let comp = i % n_comps;
                                let d_min = decode_vals[comp * 2];
                                let d_max = decode_vals[comp * 2 + 1];
                                let val = d_min + (sample as f64 / max_val) * (d_max - d_min);
                                result.push((val.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
                            }
                        }
                        result
                    } else {
                        sample_data
                    }
                } else {
                    sample_data
                }
            } else {
                sample_data
            }
        } else {
            sample_data
        };

        // Convert ICCBased image data through ICC profile if available
        let (sample_data, color_space) = if !is_image_mask {
            if let Some(ref rcs) = resolved_cs {
                if let Some((rgb_data, rgb_cs)) =
                    convert_icc_image_data(rcs, &sample_data, width, height, &mut self.icc_cache)
                {
                    (rgb_data, rgb_cs)
                } else {
                    (sample_data, color_space)
                }
            } else {
                (sample_data, color_space)
            }
        } else {
            (sample_data, color_space)
        };

        // Handle SMask (soft mask / alpha channel)
        let (sample_data, color_space) = if !is_image_mask {
            if let Some(smask_data) = self.resolve_smask(dict, width, height)? {
                let rgba = merge_rgb_with_smask(&sample_data, &smask_data, &color_space, width, height);
                (rgba, ImageColorSpace::PreconvertedRGBA)
            } else {
                (sample_data, color_space)
            }
        } else {
            (sample_data, color_space)
        };

        self.display_list.push(DisplayElement::Image {
            sample_data,
            params: ImageParams {
                width,
                height,
                color_space,
                ctm: self.gstate.ctm,
                image_matrix,
                interpolate,
                mask_color,
                alpha: self.gstate.fill_alpha,
                blend_mode: self.gstate.blend_mode,
                overprint: self.gstate.overprint,
                overprint_mode: self.gstate.overprint_mode,
                painted_channels: self.gstate.fill_painted_channels,
            },
        });
        Ok(())
    }

    /// Resolve an SMask (soft mask) from an image dict, returning the alpha data.
    fn resolve_smask(
        &self,
        dict: &PdfDict,
        image_w: u32,
        image_h: u32,
    ) -> Result<Option<Vec<u8>>, PdfError> {
        let smask_ref = match dict.get(b"SMask") {
            Some(obj) => obj.clone(),
            None => return Ok(None),
        };
        let smask_obj = self.resolver.deref(&smask_ref)?;
        let smask_dict = match smask_obj.as_dict() {
            Some(d) => d,
            None => return Ok(None),
        };
        let sw = smask_dict.get_int(b"Width").unwrap_or(0) as u32;
        let sh = smask_dict.get_int(b"Height").unwrap_or(0) as u32;
        if sw == 0 || sh == 0 {
            return Ok(None);
        }
        let mut data = self.resolver.stream_data_from_obj(&smask_ref)?;

        // Apply /Decode array if present (e.g. [1 0] inverts the mask)
        if let Some(decode) = smask_dict.get_array(b"Decode") {
            if decode.len() >= 2 {
                let d0 = decode[0].as_f64().unwrap_or(0.0);
                let d1 = decode[1].as_f64().unwrap_or(1.0);
                if (d0 - 1.0).abs() < 1e-6 && d1.abs() < 1e-6 {
                    // /Decode [1 0] — invert all bytes
                    for b in data.iter_mut() {
                        *b = 255 - *b;
                    }
                } else if (d0).abs() > 1e-6 || (d1 - 1.0).abs() > 1e-6 {
                    // General linear mapping: output = d0 + (d1-d0) * input/255
                    for b in data.iter_mut() {
                        let v = d0 + (d1 - d0) * (*b as f64 / 255.0);
                        *b = (v * 255.0).round().clamp(0.0, 255.0) as u8;
                    }
                }
            }
        }

        // SMask is always DeviceGray, 8bpc — resample if size differs
        if sw == image_w && sh == image_h {
            Ok(Some(data))
        } else {
            // Nearest-neighbor resample to match image dimensions
            let mut resampled = vec![0u8; (image_w * image_h) as usize];
            for y in 0..image_h {
                let sy = (y as u64 * sh as u64 / image_h as u64) as u32;
                for x in 0..image_w {
                    let sx = (x as u64 * sw as u64 / image_w as u64) as u32;
                    resampled[(y * image_w + x) as usize] =
                        data[(sy * sw + sx) as usize];
                }
            }
            Ok(Some(resampled))
        }
    }

    /// Handle a Form XObject (recursive content stream).
    fn handle_form_xobject(&mut self, obj: &PdfObj, dict: &PdfDict) -> Result<(), PdfError> {
        if self.depth >= 20 {
            return Err(PdfError::Other("Form XObject nesting too deep".into()));
        }

        // Get form's own resources (or inherit from page)
        let form_resources = if let Some(res_obj) = dict.get(b"Resources") {
            match self.resolver.deref(res_obj)? {
                PdfObj::Dict(d) => d,
                _ => self.resources.clone(),
            }
        } else {
            self.resources.clone()
        };

        // Form matrix
        let form_matrix = if let Some(arr) = dict.get_array(b"Matrix") {
            let vals: Vec<f64> = arr.iter().filter_map(|o| o.as_f64()).collect();
            if vals.len() == 6 {
                Matrix::new(vals[0], vals[1], vals[2], vals[3], vals[4], vals[5])
            } else {
                Matrix::identity()
            }
        } else {
            Matrix::identity()
        };

        // BBox clipping
        let bbox = if let Some(arr) = dict.get_array(b"BBox") {
            let vals: Vec<f64> = arr.iter().filter_map(|o| o.as_f64()).collect();
            if vals.len() == 4 {
                Some((vals[0], vals[1], vals[2], vals[3]))
            } else {
                None
            }
        } else {
            None
        };

        // Check for transparency group
        let is_transparency_group = self.is_transparency_group(dict);

        // Decompress form content stream
        let form_data = self.resolver.stream_data_from_obj(obj)?;

        // Save state (including font cache — form XObjects may have different
        // font resources with different encodings for the same resource name)
        self.gstate_stack.push(self.gstate.clone());
        let saved_resources = std::mem::replace(&mut self.resources, form_resources);
        let saved_font_cache = std::mem::take(&mut self.font_cache);

        // Apply form matrix
        self.gstate.ctm = self.gstate.ctm.concat(&form_matrix);

        if is_transparency_group {
            // Capture compositing parameters from the current state BEFORE
            // resetting alpha for the group's internal rendering.
            let group_blend_mode = self.gstate.blend_mode;
            let group_alpha = self.gstate.fill_alpha;

            // Reset alpha inside the group: elements render at full opacity.
            // The inherited alpha is applied when compositing the group as a
            // whole, avoiding double-application of alpha.
            self.gstate.fill_alpha = 1.0;
            self.gstate.stroke_alpha = 1.0;

            // Render group contents into a separate sub-DisplayList
            let mut group_list = DisplayList::new();
            std::mem::swap(&mut self.display_list, &mut group_list);

            // Save and clear soft mask scope — it belongs to the parent display list
            let saved_scope = self.soft_mask_scope.take();

            // Clip to BBox inside the group's display list
            if let Some((x0, y0, x1, y1)) = bbox {
                self.push_bbox_clip(x0, y0, x1, y1);
            }

            // Interpret form content into group_list (now in self.display_list)
            self.depth += 1;
            self.interpret_stream(&form_data)?;
            self.depth -= 1;

            // Flush any soft mask scope opened inside the group
            self.flush_soft_mask();

            // Swap back — group_list now contains the group's elements
            std::mem::swap(&mut self.display_list, &mut group_list);

            // Restore parent's soft mask scope
            self.soft_mask_scope = saved_scope;

            // Compute device-space bbox from form BBox + CTM
            let device_bbox = self.compute_device_bbox(bbox);

            // Extract isolated and knockout flags from Group dict
            let isolated = self.get_group_isolated(dict);
            let knockout = self.get_group_knockout(dict);

            // Push Group element to parent display list
            self.display_list.push(DisplayElement::Group {
                elements: group_list,
                params: GroupParams {
                    bbox: device_bbox,
                    isolated,
                    knockout,
                    blend_mode: group_blend_mode,
                    alpha: group_alpha,
                },
            });
        } else {
            // Non-transparency-group Form XObject: render inline (existing behavior)
            if let Some((x0, y0, x1, y1)) = bbox {
                self.push_bbox_clip(x0, y0, x1, y1);
            }

            self.depth += 1;
            self.interpret_stream(&form_data)?;
            self.depth -= 1;
        }

        // Restore state — check if clip needs resetting
        self.resources = saved_resources;
        self.font_cache = saved_font_cache;
        if let Some(saved) = self.gstate_stack.pop() {
            let old_clip_version = self.gstate.clip_path_version;
            self.gstate = saved;
            // For non-group forms, restore clip if it changed
            if !is_transparency_group && self.gstate.clip_path_version != old_clip_version {
                self.display_list.push(DisplayElement::InitClip);
                if let Some(ref clip) = self.gstate.clip_path {
                    self.display_list.push(DisplayElement::Clip {
                        path: clip.clone(),
                        params: ClipParams {
                            fill_rule: FillRule::NonZeroWinding,
                            ctm: Matrix::identity(),
                        },
                    });
                }
            }
        }

        Ok(())
    }

    /// Check if a Form XObject dict has a /Group dict with /S /Transparency.
    fn is_transparency_group(&self, dict: &PdfDict) -> bool {
        let Some(group_obj) = dict.get(b"Group") else {
            return false;
        };
        let group_dict = match self.resolver.deref(group_obj) {
            Ok(PdfObj::Dict(d)) => d,
            _ => return false,
        };
        group_dict.get_name(b"S") == Some(b"Transparency")
    }

    /// Extract the /I (isolated) flag from a Form XObject's /Group dict.
    fn get_group_isolated(&self, dict: &PdfDict) -> bool {
        let Some(group_obj) = dict.get(b"Group") else {
            return false;
        };
        let group_dict = match self.resolver.deref(group_obj) {
            Ok(PdfObj::Dict(d)) => d,
            _ => return false,
        };
        match group_dict.get(b"I") {
            Some(PdfObj::Bool(b)) => *b,
            _ => false,
        }
    }

    /// Extract the /K (knockout) flag from a Form XObject's /Group dict.
    fn get_group_knockout(&self, dict: &PdfDict) -> bool {
        let Some(group_obj) = dict.get(b"Group") else {
            return false;
        };
        let group_dict = match self.resolver.deref(group_obj) {
            Ok(PdfObj::Dict(d)) => d,
            _ => return false,
        };
        match group_dict.get(b"K") {
            Some(PdfObj::Bool(b)) => *b,
            _ => false,
        }
    }

    /// Push a BBox clip path to the current display list.
    fn push_bbox_clip(&mut self, x0: f64, y0: f64, x1: f64, y1: f64) {
        let p0 = self.gstate.ctm.transform_point(x0, y0);
        let p1 = self.gstate.ctm.transform_point(x1, y0);
        let p2 = self.gstate.ctm.transform_point(x1, y1);
        let p3 = self.gstate.ctm.transform_point(x0, y1);
        let mut clip_path = PsPath::new();
        clip_path.segments.push(PathSegment::MoveTo(p0.0, p0.1));
        clip_path.segments.push(PathSegment::LineTo(p1.0, p1.1));
        clip_path.segments.push(PathSegment::LineTo(p2.0, p2.1));
        clip_path.segments.push(PathSegment::LineTo(p3.0, p3.1));
        clip_path.segments.push(PathSegment::ClosePath);
        self.display_list.push(DisplayElement::Clip {
            path: clip_path.clone(),
            params: ClipParams {
                fill_rule: FillRule::NonZeroWinding,
                ctm: Matrix::identity(),
            },
        });
        self.gstate.clip_path = Some(clip_path);
        self.gstate.clip_path_version += 1;
    }

    /// Compute device-space bounding box from form BBox + current CTM.
    fn compute_device_bbox(&self, bbox: Option<(f64, f64, f64, f64)>) -> [f64; 4] {
        let Some((x0, y0, x1, y1)) = bbox else {
            // No BBox — use large sentinel
            return [0.0, 0.0, 1e9, 1e9];
        };
        let corners = [
            self.gstate.ctm.transform_point(x0, y0),
            self.gstate.ctm.transform_point(x1, y0),
            self.gstate.ctm.transform_point(x0, y1),
            self.gstate.ctm.transform_point(x1, y1),
        ];
        let mut min_x = f64::INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut max_y = f64::NEG_INFINITY;
        for (cx, cy) in &corners {
            min_x = min_x.min(*cx);
            min_y = min_y.min(*cy);
            max_x = max_x.max(*cx);
            max_y = max_y.max(*cy);
        }
        [min_x, min_y, max_x, max_y]
    }

    /// Handle inline image (BI ... ID ... EI).
    fn handle_inline_image(&mut self, lexer: &mut Lexer) -> Result<(), PdfError> {
        // Parse image dict (abbreviated keys)
        let mut dict = PdfDict::new();
        loop {
            let tok = lexer.next_token()?;
            match tok {
                Token::Keyword(ref kw) if kw == b"ID" => break,
                Token::Eof => return Ok(()),
                Token::Name(key) => {
                    let expanded_key = expand_inline_key(&key);
                    let val_tok = lexer.next_token()?;
                    let val = match val_tok {
                        Token::Int(n) => PdfObj::Int(n),
                        Token::Real(f) => PdfObj::Real(f),
                        Token::Name(n) => PdfObj::Name(expand_inline_value(&n)),
                        Token::Bool(b) => PdfObj::Bool(b),
                        Token::LitString(s) | Token::HexString(s) => PdfObj::Str(s),
                        Token::ArrayBegin => {
                            let arr = Self::parse_inline_array(lexer)?;
                            PdfObj::Array(arr)
                        }
                        Token::DictBegin => {
                            crate::lexer::parse_dict_body(lexer)
                                .map(PdfObj::Dict)
                                .unwrap_or(PdfObj::Null)
                        }
                        _ => PdfObj::Null,
                    };
                    dict.insert(expanded_key, val);
                }
                _ => {}
            }
        }

        // Skip single whitespace byte after ID
        let data = lexer.data();
        let mut pos = lexer.pos();
        if pos < data.len() && (data[pos] == b' ' || data[pos] == b'\n' || data[pos] == b'\r') {
            pos += 1;
        }

        // Read image data until EI
        let width = dict.get_int(b"Width").unwrap_or(0) as u32;
        let height = dict.get_int(b"Height").unwrap_or(0) as u32;
        let is_image_mask = matches!(dict.get(b"ImageMask"), Some(PdfObj::Bool(true)));
        let bpc = if is_image_mask {
            1
        } else {
            dict.get_int(b"BitsPerComponent").unwrap_or(8) as u32
        };

        let has_filter = dict.get(b"Filter").is_some() || dict.get(b"F").is_some();

        let resolved_cs = if is_image_mask {
            None
        } else if let Some(cs_obj) = dict.get(b"ColorSpace") {
            match resolve_color_space_obj(cs_obj, self.resolver) {
                Ok(resolved) => Some(resolved),
                Err(_) => Some(ResolvedColorSpace::DeviceGray),
            }
        } else {
            Some(ResolvedColorSpace::DeviceGray)
        };
        let n_components = resolved_cs
            .as_ref()
            .map(|cs| cs.num_components() as u32)
            .unwrap_or(1);

        // Calculate expected uncompressed data length (for EI boundary search)
        let row_bits = width * n_components.max(1) * bpc;
        let row_bytes = row_bits.div_ceil(8);
        let expected_len = (row_bytes * height) as usize;

        // Find EI boundary — look for whitespace + "EI" + delimiter/EOF.
        // For compressed data (CCITT, Flate, etc.), the compressed data is smaller
        // than the uncompressed size, so we must search from the start of data.
        let start = pos;
        let search_from = if has_filter { start } else { start + expected_len };
        let mut end = search_from;
        while end + 2 < data.len() {
            if is_whitespace_byte(data[end])
                && data[end + 1] == b'E'
                && data[end + 2] == b'I'
                && (end + 3 >= data.len() || is_delimiter_or_ws(data[end + 3]))
            {
                break;
            }
            end += 1;
        }

        let sample_data = data[start..end.min(data.len())].to_vec();
        lexer.set_pos((end + 3).min(data.len()));

        // Apply filters if present
        let sample_data = if has_filter {
            match crate::filters::parse_filters(&dict) {
                Ok((filters, parms)) if !filters.is_empty() => {
                    crate::filters::decode_stream(&sample_data, &filters, &parms)
                        .unwrap_or(sample_data)
                }
                _ => sample_data,
            }
        } else {
            sample_data
        };

        // Build color space for display list
        let color_space = if is_image_mask {
            let polarity = if let Some(arr) = dict.get_array(b"Decode") {
                let vals: Vec<f64> = arr.iter().filter_map(|o| o.as_f64()).collect();
                vals.len() >= 2 && vals[0] > 0.5
            } else {
                false
            };
            ImageColorSpace::Mask {
                color: self.gstate.fill_color.clone(),
                polarity,
            }
        } else {
            to_image_color_space(resolved_cs.as_ref().unwrap())
        };

        // Expand bits if needed (but NOT for image masks — keep raw 1-bit packed data)
        let sample_data = if !is_image_mask && bpc != 8 && bpc != 0 {
            expand_bits_to_bytes(&sample_data, bpc, width, height, n_components)
        } else {
            sample_data
        };

        let image_matrix =
            Matrix::new(width as f64, 0.0, 0.0, -(height as f64), 0.0, height as f64);

        self.display_list.push(DisplayElement::Image {
            sample_data,
            params: ImageParams {
                width,
                height,
                color_space,
                ctm: self.gstate.ctm,
                image_matrix,
                interpolate: false,
                mask_color: None,
                alpha: self.gstate.fill_alpha,
                blend_mode: self.gstate.blend_mode,
                overprint: self.gstate.overprint,
                overprint_mode: self.gstate.overprint_mode,
                painted_channels: self.gstate.fill_painted_channels,
            },
        });

        Ok(())
    }

    /// Apply ExtGState dictionary entries.
    fn apply_ext_gstate(&mut self, name: &[u8]) -> Result<(), PdfError> {
        let ext_dict = self
            .resolve_resource_subdict(b"ExtGState")
            .ok_or(PdfError::Other("no ExtGState resources".into()))?;
        let gs_ref = ext_dict.get(name).ok_or_else(|| {
            PdfError::Other(format!(
                "ExtGState /{} not found",
                String::from_utf8_lossy(name)
            ))
        })?;
        let gs_obj = self.resolver.deref(gs_ref)?;
        let gs_dict = gs_obj
            .as_dict()
            .ok_or(PdfError::Other("ExtGState is not a dict".into()))?;

        // Apply known keys
        if let Some(lw) = gs_dict.get_f64(b"LW") {
            self.gstate.line_width = lw;
        }
        if let Some(lc) = gs_dict.get_int(b"LC")
            && let Some(cap) = LineCap::from_i32(lc as i32)
        {
            self.gstate.line_cap = cap;
        }
        if let Some(lj) = gs_dict.get_int(b"LJ")
            && let Some(join) = LineJoin::from_i32(lj as i32)
        {
            self.gstate.line_join = join;
        }
        if let Some(ml) = gs_dict.get_f64(b"ML") {
            self.gstate.miter_limit = ml;
        }
        if let Some(fl) = gs_dict.get_f64(b"FL") {
            self.gstate.flatness = fl;
        }
        if let Some(PdfObj::Bool(sa)) = gs_dict.get(b"SA") {
            self.gstate.stroke_adjust = *sa;
        }
        if let Some(PdfObj::Bool(op)) = gs_dict.get(b"OP") {
            self.gstate.overprint = *op;
            // OP also sets stroke overprint
            self.gstate.overprint_stroke = *op;
        }
        if let Some(PdfObj::Bool(op)) = gs_dict.get(b"op") {
            self.gstate.overprint = *op;
        }
        if let Some(opm) = gs_dict.get_int(b"OPM") {
            self.gstate.overprint_mode = opm as i32;
        }
        if let Some(ca) = gs_dict.get_f64(b"CA") {
            self.gstate.stroke_alpha = ca;
        }
        if let Some(ca) = gs_dict.get_f64(b"ca") {
            self.gstate.fill_alpha = ca;
        }

        // Blend mode
        if let Some(bm) = gs_dict.get(b"BM") {
            let bm = self.resolver.deref(bm).unwrap_or_else(|_| bm.clone());
            match &bm {
                PdfObj::Name(name) => {
                    self.gstate.blend_mode = blend_mode_from_name(name);
                }
                PdfObj::Array(arr) => {
                    for obj in arr {
                        if let PdfObj::Name(name) = obj {
                            let mode = blend_mode_from_name(name);
                            if mode != 0 || name.as_slice() == b"Normal" {
                                self.gstate.blend_mode = mode;
                                break;
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        // Dash pattern
        if let Some(d_arr) = gs_dict.get_array(b"D")
            && d_arr.len() == 2
            && let (Some(arr), Some(offset)) = (d_arr[0].as_array(), d_arr[1].as_f64())
        {
            let array: Vec<f64> = arr.iter().filter_map(|o| o.as_f64()).collect();
            self.gstate.dash_pattern = DashPattern { array, offset };
        }

        // Font
        if let Some(font_arr) = gs_dict.get_array(b"Font")
            && font_arr.len() == 2
            && let Some(size) = font_arr[1].as_f64()
        {
            self.gstate.font_size = size;
        }

        // Transfer function: TR2 takes priority over TR
        if let Some(tr_obj) = gs_dict.get(b"TR2").or_else(|| gs_dict.get(b"TR")) {
            self.gstate.transfer = self.parse_transfer_function(tr_obj)?;
        }

        // Soft mask
        if let Some(smask_obj) = gs_dict.get(b"SMask") {
            let smask_obj = self.resolver.deref(smask_obj)?;
            match &smask_obj {
                PdfObj::Name(n) if n.as_slice() == b"None" => {
                    self.flush_soft_mask();
                    self.gstate.soft_mask = None;
                }
                PdfObj::Dict(d) => {
                    self.flush_soft_mask();
                    match self.resolve_soft_mask(d) {
                        Ok(sm) => {
                            let start_index = self.display_list.len();
                            self.gstate.soft_mask = Some(sm.clone());
                            self.soft_mask_scope = Some(SoftMaskScope {
                                start_index,
                                mask: sm,
                            });
                        }
                        Err(e) => {
                            eprintln!("warning: SMask resolve error: {}", e);
                        }
                    }
                }
                _ => {}
            }
        }

        Ok(())
    }

    /// Flush the current soft mask scope: wrap accumulated elements in SoftMasked.
    fn flush_soft_mask(&mut self) {
        if let Some(scope) = self.soft_mask_scope.take() {
            if self.display_list.len() > scope.start_index {
                let content = self.display_list.split_off(scope.start_index);
                self.display_list.push(DisplayElement::SoftMasked {
                    mask: scope.mask.mask_list,
                    content,
                    params: SoftMaskParams {
                        subtype: scope.mask.subtype,
                        bbox: scope.mask.bbox,
                        backdrop_color: scope.mask.backdrop_color,
                    },
                });
            }
        }
    }

    /// Resolve a soft mask dictionary into a SoftMask.
    fn resolve_soft_mask(
        &mut self,
        dict: &PdfDict,
    ) -> Result<graphics_state::SoftMask, PdfError> {
        // Parse /S (subtype): Alpha or Luminosity (default Luminosity)
        let subtype = match dict.get_name(b"S") {
            Some(b"Alpha") => SoftMaskSubtype::Alpha,
            _ => SoftMaskSubtype::Luminosity,
        };

        // Parse /G (Form XObject) — required
        let g_ref = dict
            .get(b"G")
            .ok_or_else(|| PdfError::Other("SMask missing /G".into()))?;
        let g_obj = self.resolver.deref(g_ref)?;
        let g_dict = g_obj
            .as_dict()
            .ok_or_else(|| PdfError::Other("SMask /G is not a dict".into()))?;

        // Get form BBox
        let bbox_tuple = if let Some(arr) = g_dict.get_array(b"BBox") {
            let vals: Vec<f64> = arr.iter().filter_map(|o| o.as_f64()).collect();
            if vals.len() == 4 {
                Some((vals[0], vals[1], vals[2], vals[3]))
            } else {
                None
            }
        } else {
            None
        };

        // Form matrix
        let form_matrix = if let Some(arr) = g_dict.get_array(b"Matrix") {
            let vals: Vec<f64> = arr.iter().filter_map(|o| o.as_f64()).collect();
            if vals.len() == 6 {
                Matrix::new(vals[0], vals[1], vals[2], vals[3], vals[4], vals[5])
            } else {
                Matrix::identity()
            }
        } else {
            Matrix::identity()
        };

        // Get form resources
        let form_resources = if let Some(res_obj) = g_dict.get(b"Resources") {
            match self.resolver.deref(res_obj)? {
                PdfObj::Dict(d) => d,
                _ => self.resources.clone(),
            }
        } else {
            self.resources.clone()
        };

        // Render the form into a display list
        let form_data = self.resolver.stream_data_from_obj(g_ref)?;

        // Save state and render
        self.gstate_stack.push(self.gstate.clone());
        let saved_resources = std::mem::replace(&mut self.resources, form_resources);
        let saved_display_list = std::mem::replace(&mut self.display_list, DisplayList::new());
        let saved_scope = self.soft_mask_scope.take();

        // Apply form matrix to CTM
        self.gstate.ctm = self.gstate.ctm.concat(&form_matrix);

        // Clip to BBox
        if let Some((x0, y0, x1, y1)) = bbox_tuple {
            self.push_bbox_clip(x0, y0, x1, y1);
        }

        self.depth += 1;
        let _ = self.interpret_stream(&form_data);
        self.depth -= 1;

        // Flush any soft mask scope opened inside the mask form
        self.flush_soft_mask();

        let mask_list = std::mem::replace(&mut self.display_list, saved_display_list);
        self.soft_mask_scope = saved_scope;
        self.resources = saved_resources;
        if let Some(saved) = self.gstate_stack.pop() {
            self.gstate = saved;
        }

        // Compute device-space bbox
        let device_bbox = self.compute_device_bbox(bbox_tuple);

        // Parse /BC (backdrop color)
        let backdrop_color = if let Some(bc_arr) = dict.get_array(b"BC") {
            let vals: Vec<f64> = bc_arr.iter().filter_map(|o| o.as_f64()).collect();
            if vals.len() >= 3 {
                Some([vals[0], vals[1], vals[2]])
            } else if vals.len() == 1 {
                // Gray backdrop → replicate to RGB
                Some([vals[0], vals[0], vals[0]])
            } else {
                None
            }
        } else {
            None
        };

        // Log warning for /TR (transfer function) — deferred to Phase E5
        if dict.get(b"TR").is_some() {
            eprintln!("warning: SMask /TR (transfer function) not implemented, ignoring");
        }

        Ok(graphics_state::SoftMask {
            mask_list,
            subtype,
            bbox: device_bbox,
            backdrop_color,
        })
    }

    /// Parse a TR/TR2 value into a TransferState.
    ///
    /// PDF spec: TR/TR2 can be a single function (applied to all channels),
    /// an array of 4 functions [R, G, B, Gray], or /Identity.
    fn parse_transfer_function(
        &self,
        obj: &PdfObj,
    ) -> Result<stet_core::device::TransferState, PdfError> {
        use crate::resources::function::PdfFunction;
        use stet_core::device::TransferState;

        let obj = self.resolver.deref(obj)?;

        // /Identity or /Default → no transfer
        if let Some(name) = obj.as_name()
            && (name == b"Identity" || name == b"Default")
        {
            return Ok(TransferState::default());
        }

        // Array of 4 functions [R, G, B, Gray]
        if let PdfObj::Array(arr) = &obj
            && arr.len() == 4
        {
            let mut tables: [Option<Arc<Vec<f64>>>; 4] = Default::default();
            for (i, fn_obj) in arr.iter().enumerate() {
                let fn_obj = self.resolver.deref(fn_obj)?;
                if let Some(name) = fn_obj.as_name()
                    && (name == b"Identity" || name == b"Default")
                {
                    continue; // None = identity
                }
                if let Ok(func) = PdfFunction::parse(&fn_obj, self.resolver) {
                    tables[i] = Some(Arc::new(sample_transfer_function(&func)));
                }
            }
            return Ok(TransferState {
                gray: None,
                color: Some(tables),
            });
        }

        // Single function → apply to all channels via gray
        if let Ok(func) = PdfFunction::parse(&obj, self.resolver) {
            let table = Arc::new(sample_transfer_function(&func));
            return Ok(TransferState {
                gray: Some(table),
                color: None,
            });
        }

        Ok(TransferState::default())
    }

    // === Shading operator ===

    fn op_sh(&mut self) -> Result<(), PdfError> {
        let name = self
            .operand_stack
            .last()
            .and_then(|o| o.as_name())
            .ok_or(PdfError::Other("sh: expected name".into()))?
            .to_vec();

        let shading_dict = self
            .resolve_resource_subdict(b"Shading")
            .ok_or(PdfError::Other("no Shading resources".into()))?;
        let sh_ref = shading_dict.get(&name).ok_or_else(|| {
            PdfError::Other(format!(
                "Shading /{} not found",
                String::from_utf8_lossy(&name)
            ))
        })?;
        let sh_ref_clone = sh_ref.clone();
        let sh_obj = self.resolver.deref(sh_ref)?;
        let sh_dict = sh_obj
            .as_dict()
            .ok_or(PdfError::Other("Shading is not a dict".into()))?;

        crate::resources::shading::handle_shading(
            &sh_ref_clone,
            sh_dict,
            &self.gstate,
            self.resolver,
            &mut self.display_list,
            &mut self.icc_cache,
        )
    }

    // === Pattern operators ===

    fn handle_pattern_fill(&mut self) -> Result<(), PdfError> {
        let name = self
            .operand_stack
            .last()
            .and_then(|o| o.as_name())
            .ok_or(PdfError::Other("pattern: expected name".into()))?
            .to_vec();

        // Check PatternType before resolving — Type 2 (shading) needs different handling
        let pattern_dict = self
            .resolve_resource_subdict(b"Pattern")
            .ok_or(PdfError::Other("no Pattern resources".into()))?;
        let pat_ref = pattern_dict.get(&name).ok_or_else(|| {
            PdfError::Other(format!("Pattern /{} not found", String::from_utf8_lossy(&name)))
        })?;
        let pat_obj = self.resolver.deref(pat_ref)?;
        let pat_dict = pat_obj
            .as_dict()
            .ok_or(PdfError::Other("Pattern is not a dict".into()))?;
        let pattern_type = pat_dict.get_int(b"PatternType").unwrap_or(1) as i32;

        if pattern_type == 2 {
            let shading_dl = self.resolve_shading_pattern(pat_dict)?;
            self.gstate.fill_pattern = None;
            self.gstate.fill_shading_pattern = Some(Box::new(
                ShadingPatternDL(shading_dl),
            ));
        } else {
            let pattern = self.resolve_pattern(&name)?;
            self.gstate.fill_shading_pattern = None;
            self.gstate.fill_pattern = Some(pattern);
        }
        Ok(())
    }

    fn handle_pattern_stroke(&mut self) -> Result<(), PdfError> {
        let name = self
            .operand_stack
            .last()
            .and_then(|o| o.as_name())
            .ok_or(PdfError::Other("pattern: expected name".into()))?
            .to_vec();
        let pattern = self.resolve_pattern(&name)?;
        self.gstate.stroke_pattern = Some(pattern);
        Ok(())
    }

    fn resolve_pattern(&mut self, name: &[u8]) -> Result<TilingPattern, PdfError> {
        let pattern_dict = self
            .resolve_resource_subdict(b"Pattern")
            .ok_or(PdfError::Other("no Pattern resources".into()))?;
        let pat_ref = pattern_dict.get(name).ok_or_else(|| {
            PdfError::Other(format!(
                "Pattern /{} not found",
                String::from_utf8_lossy(name)
            ))
        })?;
        let pat_ref_clone = pat_ref.clone();
        let pat_obj = self.resolver.deref(pat_ref)?;
        let pat_dict = pat_obj
            .as_dict()
            .ok_or(PdfError::Other("Pattern is not a dict".into()))?;

        let pattern_type = pat_dict.get_int(b"PatternType").unwrap_or(1) as i32;

        match pattern_type {
            1 => self.resolve_tiling_pattern(&pat_ref_clone, pat_dict),
            _ => Err(PdfError::Other(format!(
                "Unsupported PatternType {pattern_type}"
            ))),
        }
    }

    fn resolve_tiling_pattern(
        &mut self,
        pat_obj: &PdfObj,
        pat_dict: &PdfDict,
    ) -> Result<TilingPattern, PdfError> {
        if self.depth >= 20 {
            return Err(PdfError::Other("pattern recursion limit".into()));
        }
        let paint_type = pat_dict.get_int(b"PaintType").unwrap_or(1) as i32;

        let bbox = pat_dict
            .get_array(b"BBox")
            .map(|a| {
                let v: Vec<f64> = a.iter().filter_map(|o| o.as_f64()).collect();
                if v.len() >= 4 {
                    [v[0], v[1], v[2], v[3]]
                } else {
                    [0.0, 0.0, 1.0, 1.0]
                }
            })
            .unwrap_or([0.0, 0.0, 1.0, 1.0]);

        let x_step = pat_dict.get_f64(b"XStep").unwrap_or(bbox[2] - bbox[0]);
        let y_step = pat_dict.get_f64(b"YStep").unwrap_or(bbox[3] - bbox[1]);

        let pattern_matrix = pat_dict
            .get_array(b"Matrix")
            .map(|a| {
                let v: Vec<f64> = a.iter().filter_map(|o| o.as_f64()).collect();
                if v.len() >= 6 {
                    Matrix::new(v[0], v[1], v[2], v[3], v[4], v[5])
                } else {
                    Matrix::identity()
                }
            })
            .unwrap_or_else(Matrix::identity);

        let pattern_resources = if let Some(res_ref) = pat_dict.get(b"Resources") {
            match self.resolver.deref(res_ref)? {
                PdfObj::Dict(d) => d,
                _ => self.resources.clone(),
            }
        } else {
            self.resources.clone()
        };

        let pattern_data = self.resolver.stream_data_from_obj(pat_obj)?;

        // Compute the combined pattern matrix (pattern space → device space).
        // PDF pattern Matrix maps pattern space → default user space (before any
        // `cm` operators), so use initial_ctm, not the current gstate.ctm.
        let combined_matrix = self.initial_ctm.concat(&pattern_matrix);

        // Interpret pattern content stream into a sub-display-list.
        // Use identity CTM so tile paths stay in pattern space (like PostScript's
        // makepattern). The renderer applies pattern_matrix to transform them
        // into device space at render time.
        self.gstate_stack.push(self.gstate.clone());
        let saved_resources = std::mem::replace(&mut self.resources, pattern_resources);
        let saved_display_list = std::mem::take(&mut self.display_list);

        self.gstate.ctm = Matrix::identity();

        self.depth += 1;
        let _ = self.interpret_stream(&pattern_data);
        self.depth -= 1;

        let tile_display_list = std::mem::replace(&mut self.display_list, saved_display_list);
        self.resources = saved_resources;
        if let Some(saved) = self.gstate_stack.pop() {
            self.gstate = saved;
        }

        Ok(TilingPattern {
            tile: tile_display_list,
            bbox,
            x_step,
            y_step,
            pattern_matrix: combined_matrix,
            paint_type,
            pattern_id: 0,
        })
    }

    /// Resolve a PatternType 2 (shading pattern) by rendering the shading into
    /// a display list. The caller stores this and emits it at fill time,
    /// clipped to the fill path.
    fn resolve_shading_pattern(
        &mut self,
        pat_dict: &PdfDict,
    ) -> Result<DisplayList, PdfError> {
        let sh_ref = pat_dict
            .get(b"Shading")
            .ok_or(PdfError::Other("shading pattern missing /Shading".into()))?;
        let sh_ref_clone = sh_ref.clone();
        let sh_obj = self.resolver.deref(sh_ref)?;
        let sh_dict = sh_obj
            .as_dict()
            .ok_or(PdfError::Other("Shading is not a dict".into()))?;

        let pattern_matrix = pat_dict
            .get_array(b"Matrix")
            .map(|a| {
                let v: Vec<f64> = a.iter().filter_map(|o| o.as_f64()).collect();
                if v.len() >= 6 {
                    Matrix::new(v[0], v[1], v[2], v[3], v[4], v[5])
                } else {
                    Matrix::identity()
                }
            })
            .unwrap_or_else(Matrix::identity);

        // Render the shading into a temporary display list with pattern matrix
        // applied to the CTM so coordinates are in device space.
        let combined_matrix = self.initial_ctm.concat(&pattern_matrix);
        let saved_ctm = self.gstate.ctm;
        self.gstate.ctm = combined_matrix;

        let mut shading_dl = DisplayList::new();
        let result = crate::resources::shading::handle_shading(
            &sh_ref_clone,
            sh_dict,
            &self.gstate,
            self.resolver,
            &mut shading_dl,
            &mut self.icc_cache,
        );
        self.gstate.ctm = saved_ctm;
        result?;
        Ok(shading_dl)
    }
}

/// Convert a color space name to a ColorSpaceRef.
fn name_to_cs_ref(name: &[u8]) -> ColorSpaceRef {
    match name {
        b"DeviceGray" | b"G" => ColorSpaceRef::DeviceGray,
        b"DeviceRGB" | b"RGB" => ColorSpaceRef::DeviceRGB,
        b"DeviceCMYK" | b"CMYK" => ColorSpaceRef::DeviceCMYK,
        _ => ColorSpaceRef::Named(name.to_vec()),
    }
}

/// Expand abbreviated inline image key names.
fn expand_inline_key(key: &[u8]) -> Vec<u8> {
    match key {
        b"BPC" => b"BitsPerComponent".to_vec(),
        b"CS" => b"ColorSpace".to_vec(),
        b"D" => b"Decode".to_vec(),
        b"DP" => b"DecodeParms".to_vec(),
        b"F" => b"Filter".to_vec(),
        b"H" => b"Height".to_vec(),
        b"IM" => b"ImageMask".to_vec(),
        b"I" => b"Interpolate".to_vec(),
        b"W" => b"Width".to_vec(),
        _ => key.to_vec(),
    }
}

/// Expand abbreviated inline image value names.
fn expand_inline_value(name: &[u8]) -> Vec<u8> {
    match name {
        b"G" => b"DeviceGray".to_vec(),
        b"RGB" => b"DeviceRGB".to_vec(),
        b"CMYK" => b"DeviceCMYK".to_vec(),
        b"I" => b"Indexed".to_vec(),
        b"AHx" => b"ASCIIHexDecode".to_vec(),
        b"A85" => b"ASCII85Decode".to_vec(),
        b"LZW" => b"LZWDecode".to_vec(),
        b"Fl" => b"FlateDecode".to_vec(),
        b"RL" => b"RunLengthDecode".to_vec(),
        b"CCF" => b"CCITTFaxDecode".to_vec(),
        b"DCT" => b"DCTDecode".to_vec(),
        _ => name.to_vec(),
    }
}

/// Expand image sample data from arbitrary BPC to 8-bit.
/// Merge image sample data with an SMask alpha channel into RGBA.
fn merge_rgb_with_smask(
    image_data: &[u8],
    smask_data: &[u8],
    color_space: &ImageColorSpace,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let n_pixels = (width * height) as usize;
    let mut rgba = vec![255u8; n_pixels * 4];
    let n_comps = color_space.num_components();

    for i in 0..n_pixels {
        let alpha = smask_data.get(i).copied().unwrap_or(255);
        let dst = i * 4;
        match n_comps {
            3 => {
                // RGB
                let src = i * 3;
                rgba[dst] = image_data.get(src).copied().unwrap_or(0);
                rgba[dst + 1] = image_data.get(src + 1).copied().unwrap_or(0);
                rgba[dst + 2] = image_data.get(src + 2).copied().unwrap_or(0);
            }
            1 => {
                // Gray
                let g = image_data.get(i).copied().unwrap_or(0);
                rgba[dst] = g;
                rgba[dst + 1] = g;
                rgba[dst + 2] = g;
            }
            4 => {
                // CMYK → RGB
                let src = i * 4;
                let c = image_data.get(src).copied().unwrap_or(0) as f64 / 255.0;
                let m = image_data.get(src + 1).copied().unwrap_or(0) as f64 / 255.0;
                let y = image_data.get(src + 2).copied().unwrap_or(0) as f64 / 255.0;
                let k = image_data.get(src + 3).copied().unwrap_or(0) as f64 / 255.0;
                rgba[dst] = ((1.0 - c) * (1.0 - k) * 255.0 + 0.5) as u8;
                rgba[dst + 1] = ((1.0 - m) * (1.0 - k) * 255.0 + 0.5) as u8;
                rgba[dst + 2] = ((1.0 - y) * (1.0 - k) * 255.0 + 0.5) as u8;
            }
            _ => {
                // Unknown — treat as black
            }
        }
        // Premultiply alpha (tiny-skia expects premultiplied RGBA)
        if alpha == 255 {
            rgba[dst + 3] = 255;
        } else if alpha == 0 {
            rgba[dst] = 0;
            rgba[dst + 1] = 0;
            rgba[dst + 2] = 0;
            rgba[dst + 3] = 0;
        } else {
            let a = alpha as u16;
            rgba[dst] = ((rgba[dst] as u16 * a + 127) / 255) as u8;
            rgba[dst + 1] = ((rgba[dst + 1] as u16 * a + 127) / 255) as u8;
            rgba[dst + 2] = ((rgba[dst + 2] as u16 * a + 127) / 255) as u8;
            rgba[dst + 3] = alpha;
        }
    }
    rgba
}

fn expand_bits_to_bytes(
    data: &[u8],
    bpc: u32,
    width: u32,
    height: u32,
    components: u32,
) -> Vec<u8> {
    if bpc == 0 || bpc == 8 {
        return data.to_vec();
    }

    let max_val = ((1u32 << bpc) - 1) as f64;
    let samples_per_row = width * components.max(1);
    let mut result = Vec::with_capacity((width * height * components.max(1)) as usize);

    for row in 0..height {
        let row_bit_offset = row as usize * ((samples_per_row * bpc).div_ceil(8) * 8) as usize;
        for col in 0..samples_per_row {
            let bit_offset = row_bit_offset + (col * bpc) as usize;
            let byte_offset = bit_offset / 8;
            let bit_shift = bit_offset % 8;

            if byte_offset >= data.len() {
                result.push(0);
                continue;
            }

            // Extract bpc bits
            let mut val = 0u32;
            let mut bits_remaining = bpc;
            let mut cur_byte = byte_offset;
            let mut cur_bit = bit_shift;

            while bits_remaining > 0 && cur_byte < data.len() {
                let available = 8 - cur_bit as u32;
                let take = bits_remaining.min(available);
                let shift = available - take;
                let mask = ((1u32 << take) - 1) << shift;
                val = (val << take) | ((data[cur_byte] as u32 & mask) >> shift);
                bits_remaining -= take;
                cur_bit = 0;
                cur_byte += 1;
            }

            // Scale to 0-255
            result.push((val as f64 / max_val * 255.0 + 0.5) as u8);
        }
    }

    result
}

/// Convert a PDF blend mode name to a numeric code.
fn blend_mode_from_name(name: &[u8]) -> u8 {
    match name {
        b"Normal" | b"Compatible" => 0,
        b"Multiply" => 1,
        b"Screen" => 2,
        b"Overlay" => 3,
        b"Darken" => 4,
        b"Lighten" => 5,
        b"ColorDodge" => 6,
        b"ColorBurn" => 7,
        b"HardLight" => 8,
        b"SoftLight" => 9,
        b"Difference" => 10,
        b"Exclusion" => 11,
        b"Hue" => 12,
        b"Saturation" => 13,
        b"Color" => 14,
        b"Luminosity" => 15,
        _ => 0,
    }
}

fn is_whitespace_byte(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n' | 0x0C | 0x00)
}

fn is_delimiter_or_ws(b: u8) -> bool {
    is_whitespace_byte(b)
        || matches!(
            b,
            b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
        )
}

/// Evaluate a PDF function at 256 evenly-spaced points in [0,1] to build a transfer table.
fn sample_transfer_function(func: &crate::resources::function::PdfFunction) -> Vec<f64> {
    (0..256)
        .map(|i| {
            let t = i as f64 / 255.0;
            let result = func.evaluate(&[t]);
            result.first().copied().unwrap_or(t).clamp(0.0, 1.0)
        })
        .collect()
}
