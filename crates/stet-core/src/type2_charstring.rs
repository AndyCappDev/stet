// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Type 2 charstring interpreter for CFF fonts.
//!
//! Executes Type 2 charstring opcodes (Adobe TN#5177) to produce path segments
//! and glyph width information. Uses the same `CharstringResult` return type as
//! the Type 1 interpreter (`charstring.rs`).
//!
//! Key differences from Type 1:
//! - No encryption (raw bytes)
//! - Width from optional first stack operand (not hsbw/sbw)
//! - Multi-arg path ops (rlineto takes N pairs, etc.)
//! - Built-in flex (hflex/flex/hflex1/flex1)
//! - Implicit subpath close on moveto/endchar
//! - 16.16 fixed-point numbers (byte 255)
//! - Transient array (32 entries) for arithmetic ops

use crate::charstring::CharstringResult;
use crate::graphics_state::{PathSegment, PsPath};

/// Calculate subroutine bias per Type 2 spec.
fn subr_bias(n_subrs: usize) -> i32 {
    if n_subrs < 1240 {
        107
    } else if n_subrs < 33900 {
        1131
    } else {
        32768
    }
}

/// Execute a Type 2 charstring and produce path segments + width.
///
/// If `width_only` is true, path operations are skipped — only width is extracted.
pub fn execute_type2_charstring(
    data: &[u8],
    local_subrs: &[Vec<u8>],
    global_subrs: &[Vec<u8>],
    default_width_x: f64,
    nominal_width_x: f64,
    width_only: bool,
) -> Result<CharstringResult, String> {
    let mut state = T2State {
        stack: Vec::with_capacity(48),
        path: PsPath {
            segments: Vec::new(),
        },
        current_x: 0.0,
        current_y: 0.0,
        advance_width: default_width_x,
        width_parsed: false,
        default_width_x,
        nominal_width_x,
        num_h_hints: 0,
        num_v_hints: 0,
        transient: [0.0; 32],
        width_only,
        has_path: false,
    };

    execute_bytes(&mut state, data, local_subrs, global_subrs, 0)?;

    Ok(CharstringResult {
        path: state.path,
        width_x: state.advance_width,
        width_y: 0.0,
        lsb_x: 0.0,
        lsb_y: 0.0,
    })
}

/// Internal interpreter state.
struct T2State {
    stack: Vec<f64>,
    path: PsPath,
    current_x: f64,
    current_y: f64,
    advance_width: f64,
    width_parsed: bool,
    default_width_x: f64,
    nominal_width_x: f64,
    num_h_hints: usize,
    num_v_hints: usize,
    transient: [f64; 32],
    width_only: bool,
    /// Whether we have emitted any path segments (for implicit close).
    has_path: bool,
}

/// Execute a charstring byte stream (recursive for subroutine calls).
fn execute_bytes(
    state: &mut T2State,
    data: &[u8],
    local_subrs: &[Vec<u8>],
    global_subrs: &[Vec<u8>],
    depth: usize,
) -> Result<(), String> {
    if depth > 10 {
        return Err("Type 2 subroutine call stack overflow".into());
    }

    let mut i = 0;
    let length = data.len();

    while i < length {
        let b0 = data[i];

        // Operators: 0-27 and 29-31 (28 is a number)
        if b0 <= 27 || (29..=31).contains(&b0) {
            if b0 == 12 {
                // Two-byte operator
                i += 1;
                if i >= length {
                    break;
                }
                let b1 = data[i];
                exec_op12(state, b1, local_subrs, global_subrs, depth)?;
                i += 1;
            } else if b0 == 19 || b0 == 20 {
                // hintmask / cntrmask — consume implicit vstems, then skip mask bytes
                handle_hint_mask(state);
                i += 1;
                let n_mask_bytes = (state.num_h_hints + state.num_v_hints).div_ceil(8);
                i += n_mask_bytes;
            } else {
                exec_op(state, b0, local_subrs, global_subrs, depth)?;
                i += 1;
            }
        } else if (32..=246).contains(&b0) {
            state.stack.push((b0 as i32 - 139) as f64);
            i += 1;
        } else if (247..=250).contains(&b0) {
            if i + 1 >= length {
                break;
            }
            let b1 = data[i + 1];
            state
                .stack
                .push(((b0 as i32 - 247) * 256 + b1 as i32 + 108) as f64);
            i += 2;
        } else if (251..=254).contains(&b0) {
            if i + 1 >= length {
                break;
            }
            let b1 = data[i + 1];
            state
                .stack
                .push((-(b0 as i32 - 251) * 256 - b1 as i32 - 108) as f64);
            i += 2;
        } else if b0 == 255 {
            // 16.16 fixed-point number
            if i + 4 >= length {
                break;
            }
            let raw = i32::from_be_bytes([data[i + 1], data[i + 2], data[i + 3], data[i + 4]]);
            state.stack.push(raw as f64 / 65536.0);
            i += 5;
        } else if b0 == 28 {
            // 3-byte signed integer
            if i + 2 >= length {
                break;
            }
            let val = i16::from_be_bytes([data[i + 1], data[i + 2]]);
            state.stack.push(val as f64);
            i += 3;
        } else {
            i += 1;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Width handling
// ---------------------------------------------------------------------------

/// Check for optional width argument before first stack-clearing operator.
fn check_width(state: &mut T2State, expected_args: usize) {
    if state.width_parsed {
        return;
    }
    state.width_parsed = true;

    if state.stack.len() > expected_args {
        let w = state.stack.remove(0);
        state.advance_width = w + state.nominal_width_x;
    } else {
        state.advance_width = state.default_width_x;
    }
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// Close the current subpath (Type 2 implicitly closes on moveto/endchar).
fn close_subpath(state: &mut T2State) {
    if state.width_only {
        return;
    }
    // Add ClosePath if we have path segments and the last wasn't already a close
    if let Some(last) = state.path.segments.last()
        && !matches!(last, PathSegment::ClosePath | PathSegment::MoveTo(_, _))
    {
        state.path.segments.push(PathSegment::ClosePath);
    }
}

fn do_moveto(state: &mut T2State, dx: f64, dy: f64) {
    state.current_x += dx;
    state.current_y += dy;
    if state.width_only {
        return;
    }
    // Implicit close of previous subpath
    if state.has_path {
        close_subpath(state);
    }
    state
        .path
        .segments
        .push(PathSegment::MoveTo(state.current_x, state.current_y));
    state.has_path = true;
}

fn do_lineto(state: &mut T2State, dx: f64, dy: f64) {
    state.current_x += dx;
    state.current_y += dy;
    if state.width_only {
        return;
    }
    state
        .path
        .segments
        .push(PathSegment::LineTo(state.current_x, state.current_y));
}

fn do_curveto(state: &mut T2State, dx1: f64, dy1: f64, dx2: f64, dy2: f64, dx3: f64, dy3: f64) {
    let x1 = state.current_x + dx1;
    let y1 = state.current_y + dy1;
    let x2 = x1 + dx2;
    let y2 = y1 + dy2;
    let x3 = x2 + dx3;
    let y3 = y2 + dy3;
    state.current_x = x3;
    state.current_y = y3;
    if state.width_only {
        return;
    }
    state.path.segments.push(PathSegment::CurveTo {
        x1,
        y1,
        x2,
        y2,
        x3,
        y3,
    });
}

// ---------------------------------------------------------------------------
// Single-byte operator dispatch
// ---------------------------------------------------------------------------

fn exec_op(
    state: &mut T2State,
    op: u8,
    local_subrs: &[Vec<u8>],
    global_subrs: &[Vec<u8>],
    depth: usize,
) -> Result<(), String> {
    match op {
        1 => op_hstem(state),                                         // hstem
        3 => op_vstem(state),                                         // vstem
        4 => op_vmoveto(state),                                       // vmoveto
        5 => op_rlineto(state),                                       // rlineto
        6 => op_hlineto(state),                                       // hlineto
        7 => op_vlineto(state),                                       // vlineto
        8 => op_rrcurveto(state),                                     // rrcurveto
        10 => op_callsubr(state, local_subrs, global_subrs, depth)?,  // callsubr
        11 => {}                    // return (handled by call recursion)
        14 => op_endchar(state),    // endchar
        18 => op_hstemhm(state),    // hstemhm
        21 => op_rmoveto(state),    // rmoveto
        22 => op_hmoveto(state),    // hmoveto
        23 => op_vstemhm(state),    // vstemhm
        24 => op_rcurveline(state), // rcurveline
        25 => op_rlinecurve(state), // rlinecurve
        26 => op_vvcurveto(state),  // vvcurveto
        27 => op_hhcurveto(state),  // hhcurveto
        29 => op_callgsubr(state, local_subrs, global_subrs, depth)?, // callgsubr
        30 => op_vhcurveto(state),  // vhcurveto
        31 => op_hvcurveto(state),  // hvcurveto
        _ => {}                     // unknown — ignore
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Hint operators
// ---------------------------------------------------------------------------

fn op_hstem(state: &mut T2State) {
    let n_pairs = state.stack.len() / 2;
    check_width(state, n_pairs * 2);
    state.num_h_hints += state.stack.len() / 2;
    state.stack.clear();
}

fn op_vstem(state: &mut T2State) {
    let n_pairs = state.stack.len() / 2;
    check_width(state, n_pairs * 2);
    state.num_v_hints += state.stack.len() / 2;
    state.stack.clear();
}

fn op_hstemhm(state: &mut T2State) {
    let n_pairs = state.stack.len() / 2;
    check_width(state, n_pairs * 2);
    state.num_h_hints += state.stack.len() / 2;
    state.stack.clear();
}

fn op_vstemhm(state: &mut T2State) {
    let n_pairs = state.stack.len() / 2;
    check_width(state, n_pairs * 2);
    state.num_v_hints += state.stack.len() / 2;
    state.stack.clear();
}

fn handle_hint_mask(state: &mut T2State) {
    if !state.stack.is_empty() {
        let n_pairs = state.stack.len() / 2;
        check_width(state, n_pairs * 2);
        state.num_v_hints += state.stack.len() / 2;
        state.stack.clear();
    } else if !state.width_parsed {
        check_width(state, 0);
    }
}

// ---------------------------------------------------------------------------
// Path construction operators
// ---------------------------------------------------------------------------

fn op_rmoveto(state: &mut T2State) {
    check_width(state, 2);
    if state.stack.len() < 2 {
        state.stack.clear();
        return;
    }
    let dy = state.stack.pop().unwrap();
    let dx = state.stack.pop().unwrap();
    state.stack.clear();
    do_moveto(state, dx, dy);
}

fn op_hmoveto(state: &mut T2State) {
    check_width(state, 1);
    if state.stack.is_empty() {
        return;
    }
    let dx = state.stack.pop().unwrap();
    state.stack.clear();
    do_moveto(state, dx, 0.0);
}

fn op_vmoveto(state: &mut T2State) {
    check_width(state, 1);
    if state.stack.is_empty() {
        return;
    }
    let dy = state.stack.pop().unwrap();
    state.stack.clear();
    do_moveto(state, 0.0, dy);
}

fn op_rlineto(state: &mut T2State) {
    let args: Vec<f64> = state.stack.drain(..).collect();
    let mut i = 0;
    while i + 1 < args.len() {
        do_lineto(state, args[i], args[i + 1]);
        i += 2;
    }
}

fn op_hlineto(state: &mut T2State) {
    let args: Vec<f64> = state.stack.drain(..).collect();
    let mut horizontal = true;
    for val in args {
        if horizontal {
            do_lineto(state, val, 0.0);
        } else {
            do_lineto(state, 0.0, val);
        }
        horizontal = !horizontal;
    }
}

fn op_vlineto(state: &mut T2State) {
    let args: Vec<f64> = state.stack.drain(..).collect();
    let mut vertical = true;
    for val in args {
        if vertical {
            do_lineto(state, 0.0, val);
        } else {
            do_lineto(state, val, 0.0);
        }
        vertical = !vertical;
    }
}

fn op_rrcurveto(state: &mut T2State) {
    let args: Vec<f64> = state.stack.drain(..).collect();
    let mut i = 0;
    while i + 5 < args.len() {
        do_curveto(
            state,
            args[i],
            args[i + 1],
            args[i + 2],
            args[i + 3],
            args[i + 4],
            args[i + 5],
        );
        i += 6;
    }
}

fn op_hhcurveto(state: &mut T2State) {
    let args: Vec<f64> = state.stack.drain(..).collect();
    let mut i = 0;
    let mut dy1_extra = 0.0;
    if !args.len().is_multiple_of(4) {
        dy1_extra = args[0];
        i = 1;
    }
    while i + 3 < args.len() {
        let dxa = args[i];
        let dxb = args[i + 1];
        let dyb = args[i + 2];
        let dxc = args[i + 3];
        do_curveto(state, dxa, dy1_extra, dxb, dyb, dxc, 0.0);
        dy1_extra = 0.0;
        i += 4;
    }
}

fn op_vvcurveto(state: &mut T2State) {
    let args: Vec<f64> = state.stack.drain(..).collect();
    let mut i = 0;
    let mut dx1_extra = 0.0;
    if !args.len().is_multiple_of(4) {
        dx1_extra = args[0];
        i = 1;
    }
    while i + 3 < args.len() {
        let dya = args[i];
        let dxb = args[i + 1];
        let dyb = args[i + 2];
        let dyc = args[i + 3];
        do_curveto(state, dx1_extra, dya, dxb, dyb, 0.0, dyc);
        dx1_extra = 0.0;
        i += 4;
    }
}

fn op_hvcurveto(state: &mut T2State) {
    let args: Vec<f64> = state.stack.drain(..).collect();
    alternating_curves(state, &args, true);
}

fn op_vhcurveto(state: &mut T2State) {
    let args: Vec<f64> = state.stack.drain(..).collect();
    alternating_curves(state, &args, false);
}

/// Shared logic for hvcurveto / vhcurveto.
fn alternating_curves(state: &mut T2State, args: &[f64], start_horizontal: bool) {
    let mut i = 0;
    let mut phase = start_horizontal;
    let n = args.len();

    while i + 3 < n {
        let remaining = n - i;
        let is_last = remaining < 9;

        if phase {
            // H-start: dx1 dx2 dy2 dy3 [dxf]
            let dx1 = args[i];
            let dx2 = args[i + 1];
            let dy2 = args[i + 2];
            let dy3 = args[i + 3];
            let dxf = if is_last && remaining == 5 {
                args[i + 4]
            } else {
                0.0
            };
            do_curveto(state, dx1, 0.0, dx2, dy2, dxf, dy3);
            i += if is_last && remaining == 5 { 5 } else { 4 };
        } else {
            // V-start: dy1 dx2 dy2 dx3 [dyf]
            let dy1 = args[i];
            let dx2 = args[i + 1];
            let dy2 = args[i + 2];
            let dx3 = args[i + 3];
            let dyf = if is_last && remaining == 5 {
                args[i + 4]
            } else {
                0.0
            };
            do_curveto(state, 0.0, dy1, dx2, dy2, dx3, dyf);
            i += if is_last && remaining == 5 { 5 } else { 4 };
        }
        phase = !phase;
    }
}

fn op_rcurveline(state: &mut T2State) {
    let args: Vec<f64> = state.stack.drain(..).collect();
    if args.len() < 2 {
        return;
    }
    let curve_end = args.len() - 2;
    let mut i = 0;
    while i + 5 <= curve_end {
        do_curveto(
            state,
            args[i],
            args[i + 1],
            args[i + 2],
            args[i + 3],
            args[i + 4],
            args[i + 5],
        );
        i += 6;
    }
    if i + 1 < args.len() {
        do_lineto(state, args[i], args[i + 1]);
    }
}

fn op_rlinecurve(state: &mut T2State) {
    let args: Vec<f64> = state.stack.drain(..).collect();
    if args.len() < 6 {
        return;
    }
    let curve_start = args.len() - 6;
    let mut i = 0;
    while i < curve_start {
        do_lineto(state, args[i], args[i + 1]);
        i += 2;
    }
    do_curveto(
        state,
        args[curve_start],
        args[curve_start + 1],
        args[curve_start + 2],
        args[curve_start + 3],
        args[curve_start + 4],
        args[curve_start + 5],
    );
}

// ---------------------------------------------------------------------------
// endchar
// ---------------------------------------------------------------------------

fn op_endchar(state: &mut T2State) {
    if state.stack.len() >= 4 && !state.width_parsed {
        check_width(state, 4);
    } else if !state.width_parsed {
        check_width(state, 0);
    }
    state.stack.clear();

    if state.width_only {
        return;
    }

    // Implicit close of final subpath
    close_subpath(state);
}

// ---------------------------------------------------------------------------
// Subroutine operators
// ---------------------------------------------------------------------------

fn op_callsubr(
    state: &mut T2State,
    local_subrs: &[Vec<u8>],
    global_subrs: &[Vec<u8>],
    depth: usize,
) -> Result<(), String> {
    if state.stack.is_empty() {
        return Ok(());
    }
    let idx = state.stack.pop().unwrap() as i32;
    let bias = subr_bias(local_subrs.len());
    let biased = (idx + bias) as usize;
    if biased < local_subrs.len() {
        let subr_data = local_subrs[biased].clone();
        execute_bytes(state, &subr_data, local_subrs, global_subrs, depth + 1)?;
    }
    Ok(())
}

fn op_callgsubr(
    state: &mut T2State,
    local_subrs: &[Vec<u8>],
    global_subrs: &[Vec<u8>],
    depth: usize,
) -> Result<(), String> {
    if state.stack.is_empty() {
        return Ok(());
    }
    let idx = state.stack.pop().unwrap() as i32;
    let bias = subr_bias(global_subrs.len());
    let biased = (idx + bias) as usize;
    if biased < global_subrs.len() {
        let subr_data = global_subrs[biased].clone();
        execute_bytes(state, &subr_data, local_subrs, global_subrs, depth + 1)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Two-byte operator dispatch (12, N)
// ---------------------------------------------------------------------------

fn exec_op12(
    state: &mut T2State,
    sub_op: u8,
    local_subrs: &[Vec<u8>],
    global_subrs: &[Vec<u8>],
    depth: usize,
) -> Result<(), String> {
    match sub_op {
        0 => {} // dotsection — deprecated, no-op
        3 => op12_and(state),
        4 => op12_or(state),
        5 => op12_not(state),
        9 => op12_abs(state),
        10 => op12_add(state),
        11 => op12_sub(state),
        12 => op12_div(state),
        14 => op12_neg(state),
        15 => op12_eq(state),
        18 => op12_drop(state),
        20 => op12_put(state),
        21 => op12_get(state),
        22 => op12_ifelse(state),
        23 => op12_random(state),
        24 => op12_mul(state),
        26 => op12_sqrt(state),
        27 => op12_dup(state),
        28 => op12_exch(state),
        29 => op12_index(state),
        30 => op12_roll(state),
        34 => op12_hflex(state),
        35 => op12_flex(state, local_subrs, global_subrs, depth)?,
        36 => op12_hflex1(state),
        37 => op12_flex1(state),
        _ => {} // unknown — ignore
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Flex operators (12, 34-37)
// ---------------------------------------------------------------------------

fn op12_hflex(state: &mut T2State) {
    if state.stack.len() < 7 {
        state.stack.clear();
        return;
    }
    let dx1 = state.stack[0];
    let dx2 = state.stack[1];
    let dy2 = state.stack[2];
    let dx3 = state.stack[3];
    let dx4 = state.stack[4];
    let dx5 = state.stack[5];
    let dx6 = state.stack[6];
    state.stack.clear();
    do_curveto(state, dx1, 0.0, dx2, dy2, dx3, 0.0);
    do_curveto(state, dx4, 0.0, dx5, -dy2, dx6, 0.0);
}

#[allow(unused_variables)]
fn op12_flex(
    state: &mut T2State,
    local_subrs: &[Vec<u8>],
    global_subrs: &[Vec<u8>],
    depth: usize,
) -> Result<(), String> {
    if state.stack.len() < 13 {
        state.stack.clear();
        return Ok(());
    }
    let dx1 = state.stack[0];
    let dy1 = state.stack[1];
    let dx2 = state.stack[2];
    let dy2 = state.stack[3];
    let dx3 = state.stack[4];
    let dy3 = state.stack[5];
    let dx4 = state.stack[6];
    let dy4 = state.stack[7];
    let dx5 = state.stack[8];
    let dy5 = state.stack[9];
    let dx6 = state.stack[10];
    let dy6 = state.stack[11];
    // fd = state.stack[12] — flex depth, not used for rendering
    state.stack.clear();
    do_curveto(state, dx1, dy1, dx2, dy2, dx3, dy3);
    do_curveto(state, dx4, dy4, dx5, dy5, dx6, dy6);
    Ok(())
}

fn op12_hflex1(state: &mut T2State) {
    if state.stack.len() < 9 {
        state.stack.clear();
        return;
    }
    let dx1 = state.stack[0];
    let dy1 = state.stack[1];
    let dx2 = state.stack[2];
    let dy2 = state.stack[3];
    let dx3 = state.stack[4];
    let dx4 = state.stack[5];
    let dx5 = state.stack[6];
    let dy5 = state.stack[7];
    let dx6 = state.stack[8];
    state.stack.clear();
    do_curveto(state, dx1, dy1, dx2, dy2, dx3, 0.0);
    do_curveto(state, dx4, 0.0, dx5, dy5, dx6, -(dy1 + dy2 + dy5));
}

fn op12_flex1(state: &mut T2State) {
    if state.stack.len() < 11 {
        state.stack.clear();
        return;
    }
    let dx1 = state.stack[0];
    let dy1 = state.stack[1];
    let dx2 = state.stack[2];
    let dy2 = state.stack[3];
    let dx3 = state.stack[4];
    let dy3 = state.stack[5];
    let dx4 = state.stack[6];
    let dy4 = state.stack[7];
    let dx5 = state.stack[8];
    let dy5 = state.stack[9];
    let d6 = state.stack[10];
    state.stack.clear();

    let sum_dx = dx1 + dx2 + dx3 + dx4 + dx5;
    let sum_dy = dy1 + dy2 + dy3 + dy4 + dy5;

    let (dx6, dy6) = if sum_dx.abs() > sum_dy.abs() {
        (d6, -sum_dy)
    } else {
        (-sum_dx, d6)
    };

    do_curveto(state, dx1, dy1, dx2, dy2, dx3, dy3);
    do_curveto(state, dx4, dy4, dx5, dy5, dx6, dy6);
}

// ---------------------------------------------------------------------------
// Arithmetic operators (12, N)
// ---------------------------------------------------------------------------

fn op12_abs(state: &mut T2State) {
    if let Some(v) = state.stack.last_mut() {
        *v = v.abs();
    }
}

fn op12_add(state: &mut T2State) {
    if state.stack.len() >= 2 {
        let b = state.stack.pop().unwrap();
        let a = state.stack.pop().unwrap();
        state.stack.push(a + b);
    }
}

fn op12_sub(state: &mut T2State) {
    if state.stack.len() >= 2 {
        let b = state.stack.pop().unwrap();
        let a = state.stack.pop().unwrap();
        state.stack.push(a - b);
    }
}

fn op12_div(state: &mut T2State) {
    if state.stack.len() >= 2 {
        let b = state.stack.pop().unwrap();
        let a = state.stack.pop().unwrap();
        state.stack.push(if b != 0.0 { a / b } else { 0.0 });
    }
}

fn op12_neg(state: &mut T2State) {
    if let Some(v) = state.stack.last_mut() {
        *v = -*v;
    }
}

fn op12_mul(state: &mut T2State) {
    if state.stack.len() >= 2 {
        let b = state.stack.pop().unwrap();
        let a = state.stack.pop().unwrap();
        state.stack.push(a * b);
    }
}

fn op12_sqrt(state: &mut T2State) {
    if let Some(v) = state.stack.last_mut() {
        *v = v.abs().sqrt();
    }
}

fn op12_random(state: &mut T2State) {
    state.stack.push(1.0); // Simplification
}

// ---------------------------------------------------------------------------
// Logic operators (12, N)
// ---------------------------------------------------------------------------

fn op12_and(state: &mut T2State) {
    if state.stack.len() >= 2 {
        let b = state.stack.pop().unwrap();
        let a = state.stack.pop().unwrap();
        state
            .stack
            .push(if a != 0.0 && b != 0.0 { 1.0 } else { 0.0 });
    }
}

fn op12_or(state: &mut T2State) {
    if state.stack.len() >= 2 {
        let b = state.stack.pop().unwrap();
        let a = state.stack.pop().unwrap();
        state
            .stack
            .push(if a != 0.0 || b != 0.0 { 1.0 } else { 0.0 });
    }
}

fn op12_not(state: &mut T2State) {
    if let Some(a) = state.stack.pop() {
        state.stack.push(if a == 0.0 { 1.0 } else { 0.0 });
    }
}

fn op12_eq(state: &mut T2State) {
    if state.stack.len() >= 2 {
        let b = state.stack.pop().unwrap();
        let a = state.stack.pop().unwrap();
        state.stack.push(if a == b { 1.0 } else { 0.0 });
    }
}

fn op12_ifelse(state: &mut T2State) {
    if state.stack.len() >= 4 {
        let v2 = state.stack.pop().unwrap();
        let v1 = state.stack.pop().unwrap();
        let s2 = state.stack.pop().unwrap();
        let s1 = state.stack.pop().unwrap();
        state.stack.push(if v1 <= v2 { s1 } else { s2 });
    }
}

// ---------------------------------------------------------------------------
// Stack manipulation operators (12, N)
// ---------------------------------------------------------------------------

fn op12_drop(state: &mut T2State) {
    state.stack.pop();
}

fn op12_dup(state: &mut T2State) {
    if let Some(&v) = state.stack.last() {
        state.stack.push(v);
    }
}

fn op12_exch(state: &mut T2State) {
    let n = state.stack.len();
    if n >= 2 {
        state.stack.swap(n - 1, n - 2);
    }
}

fn op12_index(state: &mut T2State) {
    if let Some(idx_val) = state.stack.pop() {
        let idx = idx_val as i32;
        let idx = if idx < 0 { 0 } else { idx as usize };
        if idx < state.stack.len() {
            let val = state.stack[state.stack.len() - 1 - idx];
            state.stack.push(val);
        }
    }
}

fn op12_roll(state: &mut T2State) {
    if state.stack.len() >= 2 {
        let j = state.stack.pop().unwrap() as i32;
        let n = state.stack.pop().unwrap() as usize;
        let slen = state.stack.len();
        if n > 0 && n <= slen {
            let start = slen - n;
            let mut subset: Vec<f64> = state.stack[start..].to_vec();
            let j = ((j % n as i32) + n as i32) as usize % n;
            subset.rotate_right(j);
            state.stack[start..].copy_from_slice(&subset);
        }
    }
}

// ---------------------------------------------------------------------------
// Storage operators (12, N) — transient array
// ---------------------------------------------------------------------------

fn op12_put(state: &mut T2State) {
    if state.stack.len() >= 2 {
        let i = state.stack.pop().unwrap() as usize;
        let val = state.stack.pop().unwrap();
        if i < 32 {
            state.transient[i] = val;
        }
    }
}

fn op12_get(state: &mut T2State) {
    if let Some(idx_val) = state.stack.pop() {
        let i = idx_val as usize;
        if i < 32 {
            state.stack.push(state.transient[i]);
        } else {
            state.stack.push(0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subr_bias() {
        assert_eq!(subr_bias(0), 107);
        assert_eq!(subr_bias(1239), 107);
        assert_eq!(subr_bias(1240), 1131);
        assert_eq!(subr_bias(33899), 1131);
        assert_eq!(subr_bias(33900), 32768);
    }

    #[test]
    fn test_simple_charstring() {
        // hmoveto(100), hlineto(200), endchar
        // 100 = 239 (b0-139 = 239-139 = 100)
        // 200 = 247 0 92 (108 + 92 = 200? No: (247-247)*256 + 92 + 108 = 200)
        // Actually: 200 = 247, 92 → (0)*256 + 92 + 108 = 200
        let data = [
            239u8, // 100
            22,    // hmoveto
            247, 92, // 200
            6,  // hlineto
            14, // endchar
        ];
        let result = execute_type2_charstring(&data, &[], &[], 0.0, 0.0, false).unwrap();
        // Width should be default (100 was consumed as width since hmoveto expects 1 arg
        // and we have 1 arg, so no extra width)
        assert_eq!(result.width_x, 0.0); // default_width_x
        assert_eq!(result.path.segments.len(), 3); // moveto, lineto, closepath
    }

    #[test]
    fn test_width_parsing() {
        // width=500, hmoveto(100): two args before hmoveto, first is width
        // 500 = 28, 0x01, 0xF4
        // 100 = 239
        let data = [
            28, 0x01, 0xF4, // 500 (width)
            239,  // 100 (dx)
            22,   // hmoveto
            14,   // endchar
        ];
        let result = execute_type2_charstring(&data, &[], &[], 0.0, 200.0, false).unwrap();
        // Width = 500 + nominal_width_x(200) = 700
        assert_eq!(result.width_x, 700.0);
    }

    #[test]
    fn test_width_only_mode() {
        // Same as above but width_only
        let data = [
            28, 0x01, 0xF4, // 500 (width)
            239,  // 100 (dx)
            22,   // hmoveto
            14,   // endchar
        ];
        let result = execute_type2_charstring(&data, &[], &[], 0.0, 200.0, true).unwrap();
        assert_eq!(result.width_x, 700.0);
        assert!(result.path.segments.is_empty()); // No path in width_only mode
    }
}
