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
    // The content stream's base cm transform already maps device coords
    // (Y-down pixels) to PDF coords (Y-up points). So we work in device
    // space here — just position the text with identity scaling (font_size
    // is already in Tf).
    let tx = params.start_x;
    let ty = params.start_y;

    // The base cm maps device (Y-down) to PDF (Y-up). But PDF text
    // rendering draws glyphs upward from baseline. In our Y-down device
    // coordinate system, we need d=-1 to flip glyphs right-side-up.
    buf.extend(b"1 0 0 -1 ");
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
