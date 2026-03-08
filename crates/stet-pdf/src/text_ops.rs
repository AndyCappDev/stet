// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! PDF text operators — emit BT/ET/Tf/Tj/TJ for Text display elements.

use std::io::Write as IoWrite;

use stet_core::device::TextParams;

use crate::content_stream::fmt_num;
use crate::font_tracker::FontTracker;

/// Emit a text element as PDF BT/ET block.
///
/// Uses the font_tracker to resolve the PDF resource name.
/// Emits: BT → color → Tf → Tm → Tj → ET.
pub fn emit_text_element(
    buf: &mut Vec<u8>,
    params: &TextParams,
    font_tracker: &FontTracker,
) {
    let Some(pdf_font_name) = font_tracker.get_pdf_name(params.font_entity) else {
        return;
    };

    // Compute text matrix from CTM and font matrix
    // The content stream has a base CTM that maps device→PDF, so we need to
    // express text position in device coordinates.
    //
    // PDF text matrix: [font_size 0 0 font_size tx ty] for simple cases
    // For non-uniform scaling, we use the full font_matrix × CTM
    let font_size = params.font_size;
    if font_size.abs() < 0.001 {
        return;
    }

    // Start text block
    buf.extend(b"BT\n");

    // Set color and text rendering mode based on PaintType
    if params.paint_type == 2 {
        // PaintType 2: stroked glyphs — use text rendering mode 1 (stroke)
        // Set stroke color and line width
        emit_stroke_color(buf, params);
        fmt_num(buf, params.stroke_width);
        buf.extend(b" w\n");
        buf.extend(b"1 Tr\n");
    } else {
        // PaintType 0: filled glyphs (default)
        emit_text_color(buf, params);
    }

    // Set font — size is 1 because scaling is in the text matrix
    write!(buf, "/{} 1 Tf\n", pdf_font_name).unwrap();

    // Set text matrix (positions the text in device space)
    // Tm = [a b c d tx ty]
    //
    // Following PostForge's approach: use font_size=1 in Tf and encode
    // the full font_matrix × CTM product in the text matrix. This
    // correctly handles rotation, non-uniform scaling, and all transforms.
    //
    // The content stream's base cm maps device (Y-down) to PDF (Y-up),
    // so the Tm operates in device-space coordinates.
    let tx = params.start_x;
    let ty = params.start_y;
    let ctm = params.ctm;
    let fm = params.font_matrix;

    // Compute font_matrix × CTM (2x2 submatrix)
    // This gives us the complete transform from glyph space to device space.
    let (tm_a, tm_b, tm_c, tm_d) = if fm[0] != 0.0 || fm[1] != 0.0 || fm[2] != 0.0 || fm[3] != 0.0 {
        (
            fm[0] * ctm[0] + fm[1] * ctm[2],
            fm[0] * ctm[1] + fm[1] * ctm[3],
            fm[2] * ctm[0] + fm[3] * ctm[2],
            fm[2] * ctm[1] + fm[3] * ctm[3],
        )
    } else {
        // Fallback: normalize CTM by effective scale
        let scale_x = (ctm[0] * ctm[0] + ctm[1] * ctm[1]).sqrt();
        let scale_y = (ctm[2] * ctm[2] + ctm[3] * ctm[3]).sqrt();
        let eff = (scale_x * scale_y).sqrt();
        if eff > 1e-10 {
            let pt = font_size / eff;
            (pt * ctm[0], pt * ctm[1], pt * ctm[2], pt * ctm[3])
        } else {
            (font_size, 0.0, 0.0, -font_size)
        }
    };

    fmt_num(buf, tm_a);
    buf.push(b' ');
    fmt_num(buf, tm_b);
    buf.push(b' ');
    fmt_num(buf, tm_c);
    buf.push(b' ');
    fmt_num(buf, tm_d);
    buf.push(b' ');
    fmt_num(buf, tx);
    buf.push(b' ');
    fmt_num(buf, ty);
    buf.extend(b" Tm\n");

    // Emit text string
    emit_text_string(buf, &params.text);
    buf.extend(b" Tj\n");

    // End text block
    buf.extend(b"ET\n");
}

/// Emit a fill color for text (lowercase operators: g, rg, k).
fn emit_text_color(buf: &mut Vec<u8>, params: &TextParams) {
    let c = &params.color;
    if let Some((c_val, m, y, k)) = c.native_cmyk {
        fmt_num(buf, c_val);
        buf.push(b' ');
        fmt_num(buf, m);
        buf.push(b' ');
        fmt_num(buf, y);
        buf.push(b' ');
        fmt_num(buf, k);
        buf.extend(b" k\n");
    } else if c.r == c.g && c.g == c.b {
        fmt_num(buf, c.r);
        buf.extend(b" g\n");
    } else {
        fmt_num(buf, c.r);
        buf.push(b' ');
        fmt_num(buf, c.g);
        buf.push(b' ');
        fmt_num(buf, c.b);
        buf.extend(b" rg\n");
    }
}

/// Emit a stroke color for text (uppercase operators: G, RG, K).
fn emit_stroke_color(buf: &mut Vec<u8>, params: &TextParams) {
    let c = &params.color;
    if let Some((c_val, m, y, k)) = c.native_cmyk {
        fmt_num(buf, c_val);
        buf.push(b' ');
        fmt_num(buf, m);
        buf.push(b' ');
        fmt_num(buf, y);
        buf.push(b' ');
        fmt_num(buf, k);
        buf.extend(b" K\n");
    } else if c.r == c.g && c.g == c.b {
        fmt_num(buf, c.r);
        buf.extend(b" G\n");
    } else {
        fmt_num(buf, c.r);
        buf.push(b' ');
        fmt_num(buf, c.g);
        buf.push(b' ');
        fmt_num(buf, c.b);
        buf.extend(b" RG\n");
    }
}

/// Emit a text string as a PDF hex string <HHHH...>.
fn emit_text_string(buf: &mut Vec<u8>, text: &[u8]) {
    buf.push(b'<');
    for &b in text {
        write!(buf, "{:02X}", b).unwrap();
    }
    buf.push(b'>');
}
