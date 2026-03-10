// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF content stream interpreter.
//!
//! Converts PDF page content into a `DisplayList` for rendering through
//! the existing SkiaDevice pipeline.

pub mod color_space;
pub mod font;
pub mod graphics_state;

use crate::error::PdfError;
use crate::lexer::{Lexer, Token};
use crate::objects::{PdfDict, PdfObj};
use crate::resolver::Resolver;

use self::color_space::{
    ResolvedColorSpace, components_to_device_color, resolve_color_space, resolve_color_space_obj,
    to_image_color_space,
};
use self::graphics_state::{ColorSpaceRef, PdfGraphicsState};

use std::sync::Arc;

use self::font::{FontCache, PdfFont};
use stet_core::device::{ClipParams, ImageColorSpace, ImageParams};
use stet_core::display_list::{DisplayElement, DisplayList};
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
}

impl<'a> ContentInterpreter<'a> {
    /// Create a new interpreter.
    pub fn new(resolver: &'a Resolver<'a>, resources: PdfDict, initial_ctm: Matrix) -> Self {
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
            in_text: false,
            depth: 0,
            font_cache: FontCache::new(),
            current_font: None,
        }
    }

    /// Interpret a content stream and return the display list.
    pub fn interpret(mut self, data: &[u8]) -> Result<DisplayList, PdfError> {
        self.interpret_stream(data)?;
        Ok(self.display_list)
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
            self.display_list.push(DisplayElement::Stroke {
                path,
                params: self.gstate.stroke_params(),
            });
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
            self.display_list.push(DisplayElement::Fill {
                path,
                params: self.gstate.fill_params(FillRule::NonZeroWinding),
            });
        }
        self.apply_pending_clip();
        Ok(())
    }

    fn op_f_star(&mut self) -> Result<(), PdfError> {
        // f*: fill (even-odd)
        let path = self.take_path();
        if !path.is_empty() {
            self.display_list.push(DisplayElement::Fill {
                path,
                params: self.gstate.fill_params(FillRule::EvenOdd),
            });
        }
        self.apply_pending_clip();
        Ok(())
    }

    fn op_big_b(&mut self) -> Result<(), PdfError> {
        // B: fill (non-zero) + stroke
        let path = self.take_path();
        if !path.is_empty() {
            self.display_list.push(DisplayElement::Fill {
                path: path.clone(),
                params: self.gstate.fill_params(FillRule::NonZeroWinding),
            });
            self.display_list.push(DisplayElement::Stroke {
                path,
                params: self.gstate.stroke_params(),
            });
        }
        self.apply_pending_clip();
        Ok(())
    }

    fn op_big_b_star(&mut self) -> Result<(), PdfError> {
        // B*: fill (even-odd) + stroke
        let path = self.take_path();
        if !path.is_empty() {
            self.display_list.push(DisplayElement::Fill {
                path: path.clone(),
                params: self.gstate.fill_params(FillRule::EvenOdd),
            });
            self.display_list.push(DisplayElement::Stroke {
                path,
                params: self.gstate.stroke_params(),
            });
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
        Ok(())
    }

    fn op_small_g(&mut self) -> Result<(), PdfError> {
        // g gray: set fill color to gray
        let g = self.pop_number()?;
        self.gstate.fill_color = DeviceColor::from_gray(g);
        self.gstate.fill_color_space = ColorSpaceRef::DeviceGray;
        Ok(())
    }

    fn op_big_rg(&mut self) -> Result<(), PdfError> {
        // RG r g b: set stroke color to RGB
        let n = self.get_numbers(3)?;
        self.gstate.stroke_color = DeviceColor::from_rgb(n[0], n[1], n[2]);
        self.gstate.stroke_color_space = ColorSpaceRef::DeviceRGB;
        Ok(())
    }

    fn op_small_rg(&mut self) -> Result<(), PdfError> {
        // rg r g b: set fill color to RGB
        let n = self.get_numbers(3)?;
        self.gstate.fill_color = DeviceColor::from_rgb(n[0], n[1], n[2]);
        self.gstate.fill_color_space = ColorSpaceRef::DeviceRGB;
        Ok(())
    }

    fn op_big_k(&mut self) -> Result<(), PdfError> {
        // K c m y k: set stroke color to CMYK
        let n = self.get_numbers(4)?;
        self.gstate.stroke_color = DeviceColor::from_cmyk(n[0], n[1], n[2], n[3]);
        self.gstate.stroke_color_space = ColorSpaceRef::DeviceCMYK;
        Ok(())
    }

    fn op_small_k(&mut self) -> Result<(), PdfError> {
        // k c m y k: set fill color to CMYK
        let n = self.get_numbers(4)?;
        self.gstate.fill_color = DeviceColor::from_cmyk(n[0], n[1], n[2], n[3]);
        self.gstate.fill_color_space = ColorSpaceRef::DeviceCMYK;
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
        let n = cs.num_components();
        if n == 0 {
            return Ok(());
        }
        let nums = self.get_numbers(n)?;
        self.gstate.stroke_color = components_to_device_color(&cs, &nums);
        Ok(())
    }

    fn op_sc_fill(&mut self) -> Result<(), PdfError> {
        // sc/scn: set fill color in current color space
        let cs = resolve_color_space(
            &self.gstate.fill_color_space,
            &self.resources,
            self.resolver,
        )?;
        let n = cs.num_components();
        if n == 0 {
            return Ok(());
        }
        let nums = self.get_numbers(n)?;
        self.gstate.fill_color = components_to_device_color(&cs, &nums);
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

        // Look up in resources /Font dict
        let font_ref = self
            .resources
            .get_dict(b"Font")
            .and_then(|fd| fd.get(name).cloned());
        let font_ref = match font_ref {
            Some(r) => r,
            None => {
                self.current_font = None;
                return;
            }
        };

        match font::resolve_font(self.resolver, &font_ref) {
            Ok(font) => {
                let arc = Arc::new(font);
                self.font_cache.insert(name.to_vec(), Arc::clone(&arc));
                self.current_font = Some(arc);
            }
            Err(e) => {
                eprintln!(
                    "warning: font /{}: {}",
                    String::from_utf8_lossy(name),
                    e
                );
                self.current_font = None;
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

        for &byte in text {
            if let Some(glyph_path) = font.glyph_path(byte) {
                // Text rendering matrix (PDF spec §9.4.4):
                // row-vector: font_matrix × [Tfs 0 0 Tfs 0 Trise] × Tm × CTM
                // column-vector: CTM × Tm × [Tfs...] × font_matrix
                let text_state_matrix = Matrix::new(font_size, 0.0, 0.0, font_size, 0.0, text_rise);
                let trm = self
                    .gstate
                    .ctm
                    .concat(&self.gstate.text_matrix)
                    .concat(&text_state_matrix)
                    .concat(&font_matrix);

                // Transform glyph path to device space
                let device_path = glyph_path.transform(&trm);

                if !device_path.is_empty() {
                    let mut params = self.gstate.fill_params(FillRule::NonZeroWinding);
                    params.is_text_glyph = true;
                    self.display_list.push(DisplayElement::Fill {
                        path: device_path,
                        params,
                    });
                }
            }

            // Advance text position
            let w0 = font.glyph_width(byte);
            let mut tx = w0 * font_size + char_spacing;
            if byte == b' ' {
                tx += word_spacing;
            }
            let advance = Matrix::translate(tx, 0.0);
            self.gstate.text_matrix = self.gstate.text_matrix.concat(&advance);
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

        // Look up XObject in resources
        let xobj_dict = self
            .resources
            .get_dict(b"XObject")
            .ok_or(PdfError::Other("no XObject resources".into()))?;
        let xobj_ref = xobj_dict.get(&name).ok_or_else(|| {
            PdfError::Other(format!(
                "XObject /{} not found",
                String::from_utf8_lossy(&name)
            ))
        })?;
        let xobj = self.resolver.deref(xobj_ref)?;
        let dict = xobj
            .as_dict()
            .ok_or(PdfError::Other("XObject is not a stream".into()))?;

        let subtype = dict.get_name(b"Subtype").unwrap_or(b"");
        match subtype {
            b"Image" => self.handle_image_xobject(&xobj, dict)?,
            b"Form" => self.handle_form_xobject(&xobj, dict)?,
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
        let bpc = dict.get_int(b"BitsPerComponent").unwrap_or(8) as u32;

        // Resolve color space
        let color_space = if let Some(cs_obj) = dict.get(b"ColorSpace") {
            let resolved = resolve_color_space_obj(cs_obj, self.resolver)?;
            to_image_color_space(&resolved)
        } else {
            ImageColorSpace::DeviceRGB
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

        // Convert data if BPC != 8
        let sample_data = if bpc == 8 || bpc == 0 {
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
            },
        });
        Ok(())
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

        // Decompress form content stream
        let form_data = self.resolver.stream_data_from_obj(obj)?;

        // Save state
        self.gstate_stack.push(self.gstate.clone());
        let saved_resources = std::mem::replace(&mut self.resources, form_resources);

        // Apply form matrix
        self.gstate.ctm = self.gstate.ctm.concat(&form_matrix);

        // Clip to BBox
        if let Some((x0, y0, x1, y1)) = bbox {
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
                path: clip_path,
                params: ClipParams {
                    fill_rule: FillRule::NonZeroWinding,
                    ctm: Matrix::identity(),
                },
            });
        }

        // Interpret form content
        self.depth += 1;
        self.interpret_stream(&form_data)?;
        self.depth -= 1;

        // Restore state
        self.resources = saved_resources;
        if let Some(saved) = self.gstate_stack.pop() {
            self.gstate = saved;
        }

        Ok(())
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
        let bpc = dict.get_int(b"BitsPerComponent").unwrap_or(8) as u32;

        let color_space = if let Some(cs_obj) = dict.get(b"ColorSpace") {
            match resolve_color_space_obj(cs_obj, self.resolver) {
                Ok(resolved) => resolved,
                Err(_) => ResolvedColorSpace::DeviceGray,
            }
        } else {
            ResolvedColorSpace::DeviceGray
        };
        let n_components = color_space.num_components() as u32;

        // Calculate expected data length
        let row_bits = width * n_components.max(1) * bpc;
        let row_bytes = row_bits.div_ceil(8);
        let expected_len = (row_bytes * height) as usize;

        // Find EI boundary — look for whitespace + "EI" + delimiter/EOF
        let start = pos;
        let mut end = start + expected_len;
        // Search for EI after the expected data
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
        let sample_data = if dict.get(b"Filter").is_some() || dict.get(b"F").is_some() {
            // Try to decompress
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

        // Expand bits if needed
        let sample_data = if bpc != 8 && bpc != 0 {
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
                color_space: to_image_color_space(&color_space),
                ctm: self.gstate.ctm,
                image_matrix,
                interpolate: false,
                mask_color: None,
            },
        });

        Ok(())
    }

    /// Apply ExtGState dictionary entries.
    fn apply_ext_gstate(&mut self, name: &[u8]) -> Result<(), PdfError> {
        let ext_dict = self
            .resources
            .get_dict(b"ExtGState")
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
        if let Some(ca) = gs_dict.get_f64(b"CA") {
            self.gstate.stroke_alpha = ca;
        }
        if let Some(ca) = gs_dict.get_f64(b"ca") {
            self.gstate.fill_alpha = ca;
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

        Ok(())
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
            .resources
            .get_dict(b"Shading")
            .ok_or(PdfError::Other("no Shading resources".into()))?;
        let sh_ref = shading_dict.get(&name).ok_or_else(|| {
            PdfError::Other(format!(
                "Shading /{} not found",
                String::from_utf8_lossy(&name)
            ))
        })?;
        let sh_obj = self.resolver.deref(sh_ref)?;
        let sh_dict = sh_obj
            .as_dict()
            .ok_or(PdfError::Other("Shading is not a dict".into()))?;

        crate::resources::shading::handle_shading(
            sh_dict,
            &self.gstate,
            self.resolver,
            &mut self.display_list,
        )
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
