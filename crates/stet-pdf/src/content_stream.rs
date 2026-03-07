// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Convert a DisplayList into PDF content stream bytes.

use std::io::Write as IoWrite;

use stet_core::device::{
    AxialShadingParams, MeshShadingParams, PatchShadingParams, PatternFillParams,
    RadialShadingParams, StrokeParams,
};
use stet_core::display_list::{DisplayElement, DisplayList};
use stet_core::graphics_state::{DeviceColor, FillRule, LineCap, LineJoin, Matrix, PsPath};

use crate::image_ops::{self, ImageXObject};

/// Result of generating a content stream from a display list.
pub struct ContentStreamResult {
    /// Raw content stream bytes (before compression).
    pub content: Vec<u8>,
    /// Image XObjects referenced by the content stream.
    pub images: Vec<ImageXObject>,
    /// Indices into the display list for shading elements, paired with
    /// the shading resource name index used in the content stream.
    pub shading_refs: Vec<ShadingRef>,
}

/// Reference to a shading element that needs a PDF shading resource.
pub enum ShadingRef {
    Axial(AxialShadingParams),
    Radial(RadialShadingParams),
    Mesh(MeshShadingParams),
    Patch(PatchShadingParams),
}

/// Cached graphics state for suppressing redundant PDF operators.
struct GState {
    fill_color: Option<PdfColor>,
    stroke_color: Option<PdfColor>,
    line_width: f64,
    line_cap: i32,
    line_join: i32,
    miter_limit: f64,
    dash_array: Vec<f64>,
    dash_offset: f64,
}

impl GState {
    fn new() -> Self {
        Self {
            fill_color: None,
            stroke_color: None,
            line_width: -1.0, // force first emission
            line_cap: -1,
            line_join: -1,
            miter_limit: -1.0,
            dash_array: Vec::new(),
            dash_offset: -1.0,
        }
    }

    fn reset(&mut self) {
        *self = Self::new();
    }
}

#[derive(Clone, Debug, PartialEq)]
enum PdfColor {
    Gray(u16),          // quantized to 0..10000
    Rgb(u16, u16, u16), // quantized
    Cmyk(u16, u16, u16, u16),
}

fn quantize(v: f64) -> u16 {
    (v.clamp(0.0, 1.0) * 10000.0) as u16
}

fn color_to_pdf(c: &DeviceColor) -> PdfColor {
    if let Some((c_val, m, y, k)) = c.native_cmyk {
        PdfColor::Cmyk(quantize(c_val), quantize(m), quantize(y), quantize(k))
    } else if c.r == c.g && c.g == c.b {
        PdfColor::Gray(quantize(c.r))
    } else {
        PdfColor::Rgb(quantize(c.r), quantize(c.g), quantize(c.b))
    }
}

/// Generate PDF content stream bytes from a display list.
pub fn build_content_stream(
    list: &DisplayList,
    page_w: u32,
    page_h: u32,
    dpi: f64,
) -> ContentStreamResult {
    let scale = 72.0 / dpi;
    let page_h_pts = page_h as f64 * scale;

    let mut buf = Vec::with_capacity(4096);
    let mut images: Vec<ImageXObject> = Vec::new();
    let mut shading_refs: Vec<ShadingRef> = Vec::new();
    let mut clip_depth: u32 = 0;
    let mut gs = GState::new();

    // Initial CTM: device space (Y-down, pixels) → PDF space (Y-up, points)
    fmt_num(&mut buf, scale);
    buf.extend(b" 0 0 ");
    fmt_num(&mut buf, -scale);
    buf.extend(b" 0 ");
    fmt_num(&mut buf, page_h_pts);
    buf.extend(b" cm\n");

    // Clip to page bounds (device coordinates). The rasterizer implicitly
    // clips to the pixmap, but PDF has no implicit page clip.
    buf.extend(b"0 0 ");
    fmt_num(&mut buf, page_w as f64);
    buf.push(b' ');
    fmt_num(&mut buf, page_h as f64);
    buf.extend(b" re W n\n");

    for element in list.elements() {
        match element {
            DisplayElement::ErasePage => {
                // Fill entire page with white (device coordinates)
                buf.extend(b"1 g 0 0 ");
                fmt_num(&mut buf, page_w as f64);
                buf.push(b' ');
                fmt_num(&mut buf, page_h as f64);
                buf.extend(b" re f\n");
                gs.fill_color = Some(PdfColor::Gray(10000));
            }
            DisplayElement::Fill { path, params } => {
                emit_fill_color(&mut buf, &params.color, &mut gs);
                emit_path(&mut buf, path);
                if params.fill_rule == FillRule::EvenOdd {
                    buf.extend(b"f*\n");
                } else {
                    buf.extend(b"f\n");
                }
            }
            DisplayElement::Stroke { path, params } => {
                let has_ctm = !is_identity(&params.ctm);
                if has_ctm {
                    // Anisotropic stroke: path is in user space, CTM transforms
                    // user→device. Wrap in q/Q and apply CTM via cm operator.
                    buf.extend(b"q\n");
                    emit_cm(&mut buf, &params.ctm);
                }
                emit_stroke_color(&mut buf, &params.color, &mut gs);
                emit_line_state(&mut buf, params, &mut gs);
                emit_path(&mut buf, path);
                buf.extend(b"S\n");
                if has_ctm {
                    buf.extend(b"Q\n");
                    // Reset cached state since Q restores previous graphics state
                    gs = GState::new();
                }
            }
            DisplayElement::Clip { path, params } => {
                buf.extend(b"q\n");
                emit_path(&mut buf, path);
                if params.fill_rule == FillRule::EvenOdd {
                    buf.extend(b"W* n\n");
                } else {
                    buf.extend(b"W n\n");
                }
                clip_depth += 1;
                // Don't reset gs — colors/line state carry into clip scope
            }
            DisplayElement::InitClip => {
                for _ in 0..clip_depth {
                    buf.extend(b"Q\n");
                }
                clip_depth = 0;
                gs.reset();
            }
            DisplayElement::Image { rgba_data, params } => {
                let img_idx = images.len();
                let xobj = image_ops::convert_image(
                    rgba_data,
                    params.width,
                    params.height,
                    params.is_mask,
                );

                // Compute placement matrix: unit square → device space
                let m = compute_image_matrix(params);

                buf.extend(b"q ");
                emit_matrix(&mut buf, &m);
                buf.extend(b" cm ");
                if xobj.is_imagemask {
                    // Set fill color for imagemask
                    if let Some((r, g, b)) = xobj.mask_color {
                        emit_fill_color_rgb(&mut buf, r, g, b);
                    }
                }
                writeln!(buf, "/Im{} Do Q", img_idx).unwrap();

                images.push(xobj);
            }
            DisplayElement::AxialShading { params } => {
                let sh_idx = shading_refs.len();
                buf.extend(b"q ");
                // Apply shading CTM if not identity
                if !is_identity(&params.ctm) {
                    emit_matrix(&mut buf, &params.ctm);
                    buf.extend(b" cm ");
                }
                writeln!(buf, "/Sh{} sh Q", sh_idx).unwrap();
                shading_refs.push(ShadingRef::Axial(params.clone()));
            }
            DisplayElement::RadialShading { params } => {
                let sh_idx = shading_refs.len();
                buf.extend(b"q ");
                if !is_identity(&params.ctm) {
                    emit_matrix(&mut buf, &params.ctm);
                    buf.extend(b" cm ");
                }
                writeln!(buf, "/Sh{} sh Q", sh_idx).unwrap();
                shading_refs.push(ShadingRef::Radial(params.clone()));
            }
            DisplayElement::MeshShading { params } => {
                let sh_idx = shading_refs.len();
                buf.extend(b"q ");
                if !is_identity(&params.ctm) {
                    emit_matrix(&mut buf, &params.ctm);
                    buf.extend(b" cm ");
                }
                writeln!(buf, "/Sh{} sh Q", sh_idx).unwrap();
                shading_refs.push(ShadingRef::Mesh(params.clone()));
            }
            DisplayElement::PatchShading { params } => {
                let sh_idx = shading_refs.len();
                buf.extend(b"q ");
                if !is_identity(&params.ctm) {
                    emit_matrix(&mut buf, &params.ctm);
                    buf.extend(b" cm ");
                }
                writeln!(buf, "/Sh{} sh Q", sh_idx).unwrap();
                shading_refs.push(ShadingRef::Patch(params.clone()));
            }
            DisplayElement::PatternFill { params } => {
                emit_pattern_fill_placeholder(&mut buf, params, &mut gs);
            }
        }
    }

    // Close remaining clips
    for _ in 0..clip_depth {
        buf.extend(b"Q\n");
    }

    ContentStreamResult {
        content: buf,
        images,
        shading_refs,
    }
}

/// Emit a non-stroking (fill) color command.
fn emit_fill_color(buf: &mut Vec<u8>, color: &DeviceColor, gs: &mut GState) {
    let pc = color_to_pdf(color);
    if gs.fill_color.as_ref() == Some(&pc) {
        return;
    }
    match &pc {
        PdfColor::Cmyk(c, m, y, k) => {
            fmt_num(buf, *c as f64 / 10000.0);
            buf.push(b' ');
            fmt_num(buf, *m as f64 / 10000.0);
            buf.push(b' ');
            fmt_num(buf, *y as f64 / 10000.0);
            buf.push(b' ');
            fmt_num(buf, *k as f64 / 10000.0);
            buf.extend(b" k\n");
        }
        PdfColor::Gray(g) => {
            fmt_num(buf, *g as f64 / 10000.0);
            buf.extend(b" g\n");
        }
        PdfColor::Rgb(r, g, b) => {
            fmt_num(buf, *r as f64 / 10000.0);
            buf.push(b' ');
            fmt_num(buf, *g as f64 / 10000.0);
            buf.push(b' ');
            fmt_num(buf, *b as f64 / 10000.0);
            buf.extend(b" rg\n");
        }
    }
    gs.fill_color = Some(pc);
}

/// Emit a non-stroking RGB color (for imagemask fill color).
fn emit_fill_color_rgb(buf: &mut Vec<u8>, r: f64, g: f64, b: f64) {
    fmt_num(buf, r);
    buf.push(b' ');
    fmt_num(buf, g);
    buf.push(b' ');
    fmt_num(buf, b);
    buf.extend(b" rg ");
}

/// Emit a stroking color command.
fn emit_stroke_color(buf: &mut Vec<u8>, color: &DeviceColor, gs: &mut GState) {
    let pc = color_to_pdf(color);
    if gs.stroke_color.as_ref() == Some(&pc) {
        return;
    }
    match &pc {
        PdfColor::Cmyk(c, m, y, k) => {
            fmt_num(buf, *c as f64 / 10000.0);
            buf.push(b' ');
            fmt_num(buf, *m as f64 / 10000.0);
            buf.push(b' ');
            fmt_num(buf, *y as f64 / 10000.0);
            buf.push(b' ');
            fmt_num(buf, *k as f64 / 10000.0);
            buf.extend(b" K\n");
        }
        PdfColor::Gray(g) => {
            fmt_num(buf, *g as f64 / 10000.0);
            buf.extend(b" G\n");
        }
        PdfColor::Rgb(r, g, b) => {
            fmt_num(buf, *r as f64 / 10000.0);
            buf.push(b' ');
            fmt_num(buf, *g as f64 / 10000.0);
            buf.push(b' ');
            fmt_num(buf, *b as f64 / 10000.0);
            buf.extend(b" RG\n");
        }
    }
    gs.stroke_color = Some(pc);
}

/// Emit line state commands (width, cap, join, miter limit, dash).
fn emit_line_state(buf: &mut Vec<u8>, params: &StrokeParams, gs: &mut GState) {
    if gs.line_width != params.line_width {
        fmt_num(buf, params.line_width);
        buf.extend(b" w\n");
        gs.line_width = params.line_width;
    }

    let lc = match params.line_cap {
        LineCap::Butt => 0,
        LineCap::Round => 1,
        LineCap::Square => 2,
    };
    if gs.line_cap != lc {
        writeln!(buf, "{} J", lc).unwrap();
        gs.line_cap = lc;
    }

    let lj = match params.line_join {
        LineJoin::Miter => 0,
        LineJoin::Round => 1,
        LineJoin::Bevel => 2,
    };
    if gs.line_join != lj {
        writeln!(buf, "{} j", lj).unwrap();
        gs.line_join = lj;
    }

    if gs.miter_limit != params.miter_limit {
        fmt_num(buf, params.miter_limit);
        buf.extend(b" M\n");
        gs.miter_limit = params.miter_limit;
    }

    let dash = &params.dash_pattern;
    if gs.dash_array != dash.array || gs.dash_offset != dash.offset {
        buf.push(b'[');
        for (i, &d) in dash.array.iter().enumerate() {
            if i > 0 {
                buf.push(b' ');
            }
            fmt_num(buf, d);
        }
        buf.extend(b"] ");
        fmt_num(buf, dash.offset);
        buf.extend(b" d\n");
        gs.dash_array.clone_from(&dash.array);
        gs.dash_offset = dash.offset;
    }
}

/// Emit path segments as PDF path operators.
fn emit_path(buf: &mut Vec<u8>, path: &PsPath) {
    use stet_core::graphics_state::PathSegment;
    for seg in &path.segments {
        match seg {
            PathSegment::MoveTo(x, y) => {
                fmt_num(buf, *x);
                buf.push(b' ');
                fmt_num(buf, *y);
                buf.extend(b" m\n");
            }
            PathSegment::LineTo(x, y) => {
                fmt_num(buf, *x);
                buf.push(b' ');
                fmt_num(buf, *y);
                buf.extend(b" l\n");
            }
            PathSegment::CurveTo {
                x1,
                y1,
                x2,
                y2,
                x3,
                y3,
            } => {
                fmt_num(buf, *x1);
                buf.push(b' ');
                fmt_num(buf, *y1);
                buf.push(b' ');
                fmt_num(buf, *x2);
                buf.push(b' ');
                fmt_num(buf, *y2);
                buf.push(b' ');
                fmt_num(buf, *x3);
                buf.push(b' ');
                fmt_num(buf, *y3);
                buf.extend(b" c\n");
            }
            PathSegment::ClosePath => {
                buf.extend(b"h\n");
            }
        }
    }
}

/// Compute the PDF cm matrix for image placement.
///
/// Maps the unit square (0,0)-(1,0)-(0,1) to the image rectangle in device space.
/// The content stream's base CTM then maps device space to PDF space.
fn compute_image_matrix(params: &stet_core::device::ImageParams) -> Matrix {
    // image_matrix maps user space → image pixel space (PostScript convention)
    // Its inverse maps image pixels → user space
    // CTM maps user space → device space
    // M_scale maps unit square → image pixel grid
    let m_scale = Matrix::new(
        params.width as f64,
        0.0,
        0.0,
        params.height as f64,
        0.0,
        0.0,
    );
    let inv_im = params.image_matrix.invert().unwrap_or(Matrix::identity());

    // PDF image coordinate (0,0) is bottom-left, but data is stored top-to-bottom.
    // The content stream CTM flips Y (device→PDF), which would flip the image.
    // Compensate by flipping the v-axis of the unit square: (u,v) → (u, 1-v).
    let flip_v = Matrix::new(1.0, 0.0, 0.0, -1.0, 0.0, 1.0);

    // Row-vector composition: flip_v × M_scale × inv_im × CTM
    // Using concat: ctm.concat(&inv_im).concat(&m_scale).concat(&flip_v)
    params.ctm.concat(&inv_im).concat(&m_scale).concat(&flip_v)
}

/// Emit a 6-element matrix as PDF `a b c d e f`.
fn emit_matrix(buf: &mut Vec<u8>, m: &Matrix) {
    fmt_num(buf, m.a);
    buf.push(b' ');
    fmt_num(buf, m.b);
    buf.push(b' ');
    fmt_num(buf, m.c);
    buf.push(b' ');
    fmt_num(buf, m.d);
    buf.push(b' ');
    fmt_num(buf, m.tx);
    buf.push(b' ');
    fmt_num(buf, m.ty);
}

/// Emit a `cm` (concat matrix) operator.
fn emit_cm(buf: &mut Vec<u8>, m: &Matrix) {
    emit_matrix(buf, m);
    buf.extend(b" cm\n");
}

/// Check if a matrix is (approximately) identity.
fn is_identity(m: &Matrix) -> bool {
    (m.a - 1.0).abs() < 1e-10
        && m.b.abs() < 1e-10
        && m.c.abs() < 1e-10
        && (m.d - 1.0).abs() < 1e-10
        && m.tx.abs() < 1e-10
        && m.ty.abs() < 1e-10
}

/// Placeholder for pattern fills — fills the path with a solid mid-gray.
fn emit_pattern_fill_placeholder(buf: &mut Vec<u8>, params: &PatternFillParams, gs: &mut GState) {
    let gray = DeviceColor::from_gray(0.7);
    emit_fill_color(buf, &gray, gs);
    emit_path(buf, &params.path);
    if params.fill_rule == FillRule::EvenOdd {
        buf.extend(b"f*\n");
    } else {
        buf.extend(b"f\n");
    }
}

/// Format a number compactly for PDF content streams.
fn fmt_num(buf: &mut Vec<u8>, v: f64) {
    if v == 0.0 {
        buf.push(b'0');
    } else if v == v.round() && v.abs() < 2_147_483_647.0 {
        write!(buf, "{}", v as i64).unwrap();
    } else {
        // 4 decimal places — enough precision for sub-pixel coordinates
        let s = format!("{:.4}", v);
        let s = s.trim_end_matches('0');
        let s = s.trim_end_matches('.');
        buf.extend(s.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fmt_num() {
        let mut buf = Vec::new();
        fmt_num(&mut buf, 0.0);
        assert_eq!(&buf, b"0");

        buf.clear();
        fmt_num(&mut buf, 1.0);
        assert_eq!(&buf, b"1");

        buf.clear();
        fmt_num(&mut buf, -42.0);
        assert_eq!(&buf, b"-42");

        buf.clear();
        fmt_num(&mut buf, 1.5);
        assert_eq!(&buf, b"1.5");

        buf.clear();
        fmt_num(&mut buf, 0.001);
        assert_eq!(&buf, b"0.001");
    }

    #[test]
    fn test_color_to_pdf() {
        let gray = DeviceColor::from_gray(0.5);
        assert_eq!(color_to_pdf(&gray), PdfColor::Gray(5000));

        let rgb = DeviceColor::from_rgb(1.0, 0.0, 0.5);
        assert_eq!(color_to_pdf(&rgb), PdfColor::Rgb(10000, 0, 5000));
    }
}
