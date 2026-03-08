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

    // Set fill color
    emit_text_color(buf, params);

    // Set font
    write!(buf, "/{} ", pdf_font_name).unwrap();
    fmt_num(buf, font_size);
    buf.extend(b" Tf\n");

    // Set text matrix (positions the text in device space)
    // Tm = [a b c d tx ty]
    //
    // The content stream's base cm maps device (Y-down pixels) to PDF
    // (Y-up points). We work in device space, so the Tm must encode:
    // 1. Non-uniform scaling (narrow/wide text) relative to font_size
    // 2. Rotation from the CTM
    // 3. Y-flip (d < 0) so glyphs render right-side-up
    //
    // font_size = point_size * sqrt(scale_x * scale_y) (geometric mean)
    // Tm encodes the CTM direction normalized by that mean.
    let tx = params.start_x;
    let ty = params.start_y;
    let ctm = params.ctm;

    let scale_x = (ctm[0] * ctm[0] + ctm[1] * ctm[1]).sqrt();
    let scale_y = (ctm[2] * ctm[2] + ctm[3] * ctm[3]).sqrt();
    let effective_scale = (scale_x * scale_y).sqrt();

    let (tm_a, tm_b, tm_c, tm_d) = if effective_scale > 1e-10 {
        // Normalize CTM by the effective scale (geometric mean), so
        // font_size × Tm reproduces the original per-axis scaling ratios.
        //
        // The content stream's base cm already flips Y (device→PDF), so
        // we're working in a Y-down coordinate system. PDF text always
        // renders glyphs upward, so we need d < 0. Some PS programs
        // (e.g., dvips) use a CTM with d > 0 after their own coordinate
        // setup. We preserve the X-axis direction from the CTM but always
        // force Y to flip by using the absolute Y scale with negation.
        let norm_a = ctm[0] / effective_scale;
        let norm_b = ctm[1] / effective_scale;
        // Y axis: use scale ratio but always flip (negate)
        let y_ratio = scale_y / effective_scale;
        (norm_a, norm_b, 0.0, -y_ratio)
    } else {
        (1.0, 0.0, 0.0, -1.0)
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

/// Emit a fill color for text.
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

/// Emit a text string as a PDF hex string <HHHH...>.
fn emit_text_string(buf: &mut Vec<u8>, text: &[u8]) {
    buf.push(b'<');
    for &b in text {
        write!(buf, "{:02X}", b).unwrap();
    }
    buf.push(b'>');
}
