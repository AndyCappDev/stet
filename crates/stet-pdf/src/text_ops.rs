// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF text operators — emit BT/ET/Tf/Tj/TJ for Text display elements.
//!
//! Batches consecutive same-font text elements into single BT/ET blocks
//! and uses TJ arrays with kern values for text on the same baseline.

use std::io::Write as IoWrite;

use stet_core::object::EntityId;
use stet_graphics::device::TextParams;

use crate::content_stream::fmt_num;
use crate::font_tracker::FontTracker;

/// Emit a batch of same-font Text elements as optimized BT/ET blocks.
///
/// Groups texts into baseline runs and uses TJ arrays with kern values
/// for text on the same line. Separate BT/ET blocks for different baselines.
pub fn emit_text_batch(buf: &mut Vec<u8>, batch: &[&TextParams], font_tracker: &FontTracker) {
    if batch.is_empty() {
        return;
    }

    let first = batch[0];
    let Some(pdf_font_name) = font_tracker.get_pdf_name(EntityId(first.font_entity)) else {
        return;
    };

    if first.font_size.abs() < 0.001 {
        return;
    }

    // Check if widths are available for this font
    let has_widths = !font_tracker
        .fonts()
        .find(|u| u.font_entity == EntityId(first.font_entity))
        .is_none_or(|u| u.widths.is_empty());

    // Single BT/Tf block for the entire batch — Tf is the same for all
    // entries since they share the same font.
    buf.extend(b"BT\n");

    // Set font — size is 1 because scaling is in the text matrix
    writeln!(buf, "/{} 1 Tf", pdf_font_name).unwrap();

    // Track last emitted color to avoid redundant color ops
    let mut last_color: Option<ColorKey> = None;
    let mut last_paint_type: i32 = -1;

    let mut i = 0;
    while i < batch.len() {
        let text_obj = batch[i];
        let (tm_a, tm_b, tm_c, tm_d) = compute_text_matrix(text_obj);

        // Collect consecutive entries on the same baseline for TJ
        let mut run: Vec<usize> = vec![i];

        if has_widths {
            let tm_key = TmKey::new(tm_a, tm_b, tm_c, tm_d);
            let adv_len_sq = tm_a * tm_a + tm_b * tm_b;

            let mut j = i + 1;
            while j < batch.len() {
                let next = batch[j];

                // Must have same color
                if color_key(&next.color, next.paint_type)
                    != color_key(&text_obj.color, text_obj.paint_type)
                {
                    break;
                }

                // Must have same Tm orientation (2×2 submatrix)
                let (ntm_a, ntm_b, ntm_c, ntm_d) = compute_text_matrix(next);
                if TmKey::new(ntm_a, ntm_b, ntm_c, ntm_d) != tm_key {
                    break;
                }

                // Check perpendicular distance from baseline
                let prev_idx = *run.last().unwrap();
                let prev = batch[prev_idx];
                let ndx = next.start_x - prev.start_x;
                let ndy = next.start_y - prev.start_y;

                if adv_len_sq > 1e-10 {
                    let adv_len = adv_len_sq.sqrt();
                    let perp_dist = (-tm_b * ndx + tm_a * ndy).abs() / adv_len;
                    if perp_dist > 0.5 {
                        break;
                    }

                    // Check along-direction gap (≤ 500 font units)
                    let along_dist = (ndx * tm_a + ndy * tm_b) / adv_len_sq;
                    let mut prev_text_width = 0.0;
                    for &byte_val in &prev.text {
                        if let Some(w) = font_tracker
                            .get_glyph_width(EntityId(prev.font_entity), byte_val as u16)
                        {
                            prev_text_width += w as f64;
                        }
                    }
                    let gap = (along_dist * 1000.0).abs() - prev_text_width;
                    if gap > 500.0 {
                        break;
                    }
                } else if ndx.abs() > 0.5 || ndy.abs() > 0.5 {
                    break;
                }

                run.push(j);
                j += 1;
            }
        }

        // Emit color if changed (valid inside BT/ET per PDF spec)
        let cur_color = color_key(&text_obj.color, text_obj.paint_type);
        if last_color.as_ref() != Some(&cur_color) || last_paint_type != text_obj.paint_type {
            if text_obj.paint_type == 2 {
                emit_stroke_color(buf, text_obj);
                fmt_num(buf, text_obj.stroke_width);
                buf.extend(b" w\n");
                buf.extend(b"1 Tr\n");
            } else {
                if last_paint_type == 2 {
                    buf.extend(b"0 Tr\n");
                }
                emit_text_color(buf, text_obj);
            }
            last_color = Some(cur_color);
            last_paint_type = text_obj.paint_type;
        }

        // Emit Tm for first text in run
        emit_tm(
            buf,
            tm_a,
            tm_b,
            tm_c,
            tm_d,
            text_obj.start_x,
            text_obj.start_y,
        );

        if run.len() == 1 {
            // Single entry — simple Tj
            emit_text_string(buf, &text_obj.text);
            buf.extend(b" Tj\n");
        } else {
            // Multiple entries — build TJ array with kern values
            let adv_len_sq = tm_a * tm_a + tm_b * tm_b;
            let mut tj_parts: Vec<TjPart> = Vec::new();

            for (k, &run_idx) in run.iter().enumerate() {
                let tobj = batch[run_idx];

                if k > 0 {
                    let prev_idx = run[k - 1];
                    let prev = batch[prev_idx];
                    let dx = tobj.start_x - prev.start_x;
                    let dy = tobj.start_y - prev.start_y;

                    let mut can_kern = false;
                    if adv_len_sq > 1e-10 {
                        // Advance in text-space x units
                        let text_advance = (dx * tm_a + dy * tm_b) / adv_len_sq;
                        // Sum widths of all glyphs in previous text
                        let mut prev_total_width = 0.0;
                        let mut all_widths_found = true;
                        for &b in &prev.text {
                            if let Some(w) =
                                font_tracker.get_glyph_width(EntityId(prev.font_entity), b as u16)
                            {
                                prev_total_width += w as f64;
                            } else {
                                all_widths_found = false;
                                break;
                            }
                        }
                        if all_widths_found && !prev.text.is_empty() {
                            let kern = prev_total_width - text_advance * 1000.0;
                            let kern_rounded = kern.round() as i64;
                            if kern_rounded != 0 {
                                tj_parts.push(TjPart::Kern(kern_rounded));
                            }
                            can_kern = true;
                        }
                    }

                    if !can_kern {
                        // Can't compute kern — emit what we have and start new Tm
                        if !tj_parts.is_empty() {
                            emit_tj_array(buf, &tj_parts);
                            tj_parts.clear();
                        }
                        emit_tm(buf, tm_a, tm_b, tm_c, tm_d, tobj.start_x, tobj.start_y);
                    }
                }

                tj_parts.push(TjPart::Text(tobj.text.as_slice()));
            }

            if !tj_parts.is_empty() {
                emit_tj_array(buf, &tj_parts);
            }
        }

        i = *run.last().unwrap() + 1;
    }

    buf.extend(b"ET\n");
}

/// Part of a TJ array: either a hex string or a kern value.
enum TjPart<'a> {
    Text(&'a [u8]),
    Kern(i64),
}

/// Emit a TJ array: [<hex1> kern1 <hex2> ...] TJ
fn emit_tj_array(buf: &mut Vec<u8>, parts: &[TjPart]) {
    buf.push(b'[');
    for part in parts {
        match part {
            TjPart::Text(text) => emit_text_string(buf, text),
            TjPart::Kern(k) => write!(buf, "{}", k).unwrap(),
        }
    }
    buf.extend(b"] TJ\n");
}

/// Quantized Tm 2×2 key for comparing text matrix orientation.
#[derive(PartialEq)]
struct TmKey {
    a: i64,
    b: i64,
    c: i64,
    d: i64,
}

impl TmKey {
    fn new(a: f64, b: f64, c: f64, d: f64) -> Self {
        // Quantize to 0.01 resolution (same as PostForge's _fmt comparison)
        Self {
            a: (a * 100.0).round() as i64,
            b: (b * 100.0).round() as i64,
            c: (c * 100.0).round() as i64,
            d: (d * 100.0).round() as i64,
        }
    }
}

/// Quantized color key for comparing text colors.
#[derive(PartialEq)]
struct ColorKey {
    r: u16,
    g: u16,
    b: u16,
    cmyk: Option<(u16, u16, u16, u16)>,
    paint_type: i32,
}

fn quantize_color(v: f64) -> u16 {
    (v.clamp(0.0, 1.0) * 10000.0) as u16
}

fn color_key(c: &stet_graphics::color::DeviceColor, paint_type: i32) -> ColorKey {
    ColorKey {
        r: quantize_color(c.r),
        g: quantize_color(c.g),
        b: quantize_color(c.b),
        cmyk: c.native_cmyk.map(|(c, m, y, k)| {
            (
                quantize_color(c),
                quantize_color(m),
                quantize_color(y),
                quantize_color(k),
            )
        }),
        paint_type,
    }
}

/// Compute the 2×2 text matrix submatrix from a TextParams.
fn compute_text_matrix(params: &TextParams) -> (f64, f64, f64, f64) {
    let ctm = params.ctm;
    let fm = params.font_matrix;

    if fm[0] != 0.0 || fm[1] != 0.0 || fm[2] != 0.0 || fm[3] != 0.0 {
        (
            fm[0] * ctm[0] + fm[1] * ctm[2],
            fm[0] * ctm[1] + fm[1] * ctm[3],
            fm[2] * ctm[0] + fm[3] * ctm[2],
            fm[2] * ctm[1] + fm[3] * ctm[3],
        )
    } else {
        let scale_x = (ctm[0] * ctm[0] + ctm[1] * ctm[1]).sqrt();
        let scale_y = (ctm[2] * ctm[2] + ctm[3] * ctm[3]).sqrt();
        let eff = (scale_x * scale_y).sqrt();
        if eff > 1e-10 {
            let pt = params.font_size / eff;
            (pt * ctm[0], pt * ctm[1], pt * ctm[2], pt * ctm[3])
        } else {
            (params.font_size, 0.0, 0.0, -params.font_size)
        }
    }
}

/// Emit a Tm operator.
fn emit_tm(buf: &mut Vec<u8>, a: f64, b: f64, c: f64, d: f64, tx: f64, ty: f64) {
    fmt_num(buf, a);
    buf.push(b' ');
    fmt_num(buf, b);
    buf.push(b' ');
    fmt_num(buf, c);
    buf.push(b' ');
    fmt_num(buf, d);
    buf.push(b' ');
    fmt_num(buf, tx);
    buf.push(b' ');
    fmt_num(buf, ty);
    buf.extend(b" Tm\n");
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
