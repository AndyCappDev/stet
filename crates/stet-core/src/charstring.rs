// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Type 1 charstring interpreter.
//!
//! Decrypts and executes Type 1 charstring opcodes to produce path segments
//! and glyph width information.

use crate::encoding::STANDARD_ENCODING;
use crate::graphics_state::{PathSegment, PsPath};

/// Result of executing a charstring: the glyph path and advance width.
pub struct CharstringResult {
    pub path: PsPath,
    pub width_x: f64,
    pub width_y: f64,
    pub lsb_x: f64,
    pub lsb_y: f64,
    /// Deprecated seac (Standard Encoding Accented Character) from endchar with 4 args.
    /// Contains (adx, ady, bchar, achar) — Standard Encoding codes for base and accent.
    pub seac: Option<(f64, f64, u8, u8)>,
}

/// Decrypt a charstring using the Type 1 charstring cipher (R=4330).
/// Skips the first `len_iv` random bytes.
pub fn decrypt_charstring(data: &[u8], len_iv: usize) -> Vec<u8> {
    let c1: u32 = 52845;
    let c2: u32 = 22719;
    let mut r: u32 = 4330;
    let mut result = Vec::with_capacity(data.len().saturating_sub(len_iv));
    for (i, &cipher) in data.iter().enumerate() {
        let plain = (cipher as u32 ^ (r >> 8)) as u8;
        if i >= len_iv {
            result.push(plain);
        }
        r = ((cipher as u32 + r) * c1 + c2) & 0xFFFF;
    }
    result
}

/// Charstring lookup function for seac composite character support.
/// Maps glyph name (bytes) to encrypted charstring bytes.
pub type CharstringLookup<'a> = dyn Fn(&str) -> Option<Vec<u8>> + 'a;

/// Execute a Type 1 charstring and produce path segments + width.
///
/// If `width_only` is true, path operations are skipped — only width is extracted.
/// If `cs_lookup` is provided, seac (composite characters) can look up component charstrings.
pub fn execute_charstring(
    charstring: &[u8],
    subrs: &[Vec<u8>],
    len_iv: usize,
    width_only: bool,
) -> Result<CharstringResult, String> {
    execute_charstring_ex(charstring, subrs, len_iv, width_only, None)
}

/// Execute a Type 1 charstring with optional charstring lookup for seac support.
pub fn execute_charstring_ex(
    charstring: &[u8],
    subrs: &[Vec<u8>],
    len_iv: usize,
    width_only: bool,
    cs_lookup: Option<&CharstringLookup<'_>>,
) -> Result<CharstringResult, String> {
    let decrypted = decrypt_charstring(charstring, len_iv);
    let mut interp = CharstringInterp::new(subrs, len_iv, width_only, cs_lookup);
    interp.execute(&decrypted)?;
    Ok(CharstringResult {
        path: interp.path,
        width_x: interp.width_x,
        width_y: interp.width_y,
        lsb_x: interp.lsb_x,
        lsb_y: interp.lsb_y,
        seac: None,
    })
}

/// Execute a charstring for seac (accent composition), applying an offset.
pub fn execute_charstring_with_offset(
    charstring: &[u8],
    subrs: &[Vec<u8>],
    len_iv: usize,
    offset_x: f64,
    offset_y: f64,
) -> Result<CharstringResult, String> {
    let decrypted = decrypt_charstring(charstring, len_iv);
    let mut interp = CharstringInterp::new(subrs, len_iv, false, None);
    interp.x = offset_x;
    interp.y = offset_y;
    interp.execute(&decrypted)?;
    Ok(CharstringResult {
        path: interp.path,
        width_x: interp.width_x,
        width_y: interp.width_y,
        lsb_x: interp.lsb_x,
        lsb_y: interp.lsb_y,
        seac: None,
    })
}

/// Internal charstring interpreter state.
struct CharstringInterp<'a> {
    stack: Vec<f64>,
    path: PsPath,
    x: f64,
    y: f64,
    width_x: f64,
    width_y: f64,
    lsb_x: f64,
    lsb_y: f64,
    subrs: &'a [Vec<u8>],
    len_iv: usize,
    width_only: bool,
    done: bool,
    // Flex support (OtherSubrs 0-3)
    flex_active: bool,
    flex_points: Vec<(f64, f64)>,
    // OtherSubrs return stack (for pop operator)
    ps_stack: Vec<f64>,
    // Charstring lookup for seac composite character support
    cs_lookup: Option<&'a CharstringLookup<'a>>,
}

impl<'a> CharstringInterp<'a> {
    fn new(
        subrs: &'a [Vec<u8>],
        len_iv: usize,
        width_only: bool,
        cs_lookup: Option<&'a CharstringLookup<'a>>,
    ) -> Self {
        Self {
            stack: Vec::with_capacity(48),
            path: PsPath::new(),
            x: 0.0,
            y: 0.0,
            width_x: 0.0,
            width_y: 0.0,
            lsb_x: 0.0,
            lsb_y: 0.0,
            subrs,
            len_iv,
            width_only,
            done: false,
            flex_active: false,
            flex_points: Vec::new(),
            ps_stack: Vec::new(),
            cs_lookup,
        }
    }

    fn execute(&mut self, data: &[u8]) -> Result<(), String> {
        self.execute_inner(data, 0)
    }

    fn execute_inner(&mut self, data: &[u8], depth: usize) -> Result<(), String> {
        if depth > 10 {
            return Err("Charstring subroutine depth exceeded".to_string());
        }

        let mut pos = 0;
        while pos < data.len() && !self.done {
            let b = data[pos];
            pos += 1;

            match b {
                // Commands (0–31)
                0 => {} // reserved, ignore
                1 => {
                    // hstem: y dy — ignore (hint), pop 2 args
                    if self.stack.len() >= 2 {
                        self.stack.pop();
                        self.stack.pop();
                    }
                }
                2 => {} // reserved
                3 => {
                    // vstem: x dx — ignore (hint), pop 2 args
                    if self.stack.len() >= 2 {
                        self.stack.pop();
                        self.stack.pop();
                    }
                }
                4 => {
                    // vmoveto: dy
                    if self.stack.is_empty() {
                        return Err("vmoveto: stack underflow".to_string());
                    }
                    let dy = self.stack.pop().unwrap();
                    self.y += dy;
                    if !self.width_only && !self.flex_active {
                        self.path.segments.push(PathSegment::MoveTo(self.x, self.y));
                    }
                    // During flex, moveto just updates current point — OtherSubrs 2 handles flex_points
                }
                5 => {
                    // rlineto: dx dy
                    if self.stack.len() < 2 {
                        return Err("rlineto: stack underflow".to_string());
                    }
                    let dy = self.stack.pop().unwrap();
                    let dx = self.stack.pop().unwrap();
                    self.x += dx;
                    self.y += dy;
                    if !self.width_only {
                        self.path.segments.push(PathSegment::LineTo(self.x, self.y));
                    }
                }
                6 => {
                    // hlineto: dx
                    if self.stack.is_empty() {
                        return Err("hlineto: stack underflow".to_string());
                    }
                    let dx = self.stack.pop().unwrap();
                    self.x += dx;
                    if !self.width_only {
                        self.path.segments.push(PathSegment::LineTo(self.x, self.y));
                    }
                }
                7 => {
                    // vlineto: dy
                    if self.stack.is_empty() {
                        return Err("vlineto: stack underflow".to_string());
                    }
                    let dy = self.stack.pop().unwrap();
                    self.y += dy;
                    if !self.width_only {
                        self.path.segments.push(PathSegment::LineTo(self.x, self.y));
                    }
                }
                8 => {
                    // rrcurveto: dx1 dy1 dx2 dy2 dx3 dy3
                    if self.stack.len() < 6 {
                        return Err("rrcurveto: stack underflow".to_string());
                    }
                    let dy3 = self.stack.pop().unwrap();
                    let dx3 = self.stack.pop().unwrap();
                    let dy2 = self.stack.pop().unwrap();
                    let dx2 = self.stack.pop().unwrap();
                    let dy1 = self.stack.pop().unwrap();
                    let dx1 = self.stack.pop().unwrap();
                    let x1 = self.x + dx1;
                    let y1 = self.y + dy1;
                    let x2 = x1 + dx2;
                    let y2 = y1 + dy2;
                    let x3 = x2 + dx3;
                    let y3 = y2 + dy3;
                    if !self.width_only {
                        self.path.segments.push(PathSegment::CurveTo {
                            x1,
                            y1,
                            x2,
                            y2,
                            x3,
                            y3,
                        });
                    }
                    self.x = x3;
                    self.y = y3;
                }
                9 => {
                    // closepath
                    if !self.width_only {
                        self.path.segments.push(PathSegment::ClosePath);
                    }
                }
                10 => {
                    // callsubr: index
                    if self.stack.is_empty() {
                        return Err("callsubr: stack underflow".to_string());
                    }
                    let idx = self.stack.pop().unwrap() as usize;
                    if idx >= self.subrs.len() {
                        return Err(format!("callsubr: index {} out of range", idx));
                    }
                    let subr_data = decrypt_charstring(&self.subrs[idx], self.len_iv);
                    self.execute_inner(&subr_data, depth + 1)?;
                }
                11 => {
                    // return — return from subroutine
                    return Ok(());
                }
                12 => {
                    // Two-byte escape
                    if pos >= data.len() {
                        break;
                    }
                    let b2 = data[pos];
                    pos += 1;
                    self.execute_escape(b2)?;
                }
                13 => {
                    // hsbw: sbx wx
                    // Sets sidebearing and width. Does NOT emit a MoveTo —
                    // the first real moveto in the glyph body will do that.
                    if self.stack.len() < 2 {
                        return Err("hsbw: stack underflow".to_string());
                    }
                    let wx = self.stack.pop().unwrap();
                    let sbx = self.stack.pop().unwrap();
                    self.lsb_x = sbx;
                    self.lsb_y = 0.0;
                    self.width_x = wx;
                    self.width_y = 0.0;
                    self.x = sbx;
                    self.y = 0.0;
                }
                14 => {
                    // endchar — signal completion
                    if !self.width_only && !self.path.is_empty() {
                        // Implicit closepath if path is open
                    }
                    self.done = true;
                    return Ok(());
                }
                15..=20 => {} // reserved
                21 => {
                    // rmoveto: dx dy
                    if self.stack.len() < 2 {
                        return Err("rmoveto: stack underflow".to_string());
                    }
                    let dy = self.stack.pop().unwrap();
                    let dx = self.stack.pop().unwrap();
                    self.x += dx;
                    self.y += dy;
                    if !self.width_only && !self.flex_active {
                        self.path.segments.push(PathSegment::MoveTo(self.x, self.y));
                    }
                    // During flex, moveto just updates current point — OtherSubrs 2 handles flex_points
                }
                22 => {
                    // hmoveto: dx
                    if self.stack.is_empty() {
                        return Err("hmoveto: stack underflow".to_string());
                    }
                    let dx = self.stack.pop().unwrap();
                    self.x += dx;
                    if !self.width_only && !self.flex_active {
                        self.path.segments.push(PathSegment::MoveTo(self.x, self.y));
                    }
                    // During flex, moveto just updates current point — OtherSubrs 2 handles flex_points
                }
                23..=29 => {} // reserved
                30 => {
                    // vhcurveto: dy1 dx2 dy2 dx3
                    if self.stack.len() < 4 {
                        return Err("vhcurveto: stack underflow".to_string());
                    }
                    let dx3 = self.stack.pop().unwrap();
                    let dy2 = self.stack.pop().unwrap();
                    let dx2 = self.stack.pop().unwrap();
                    let dy1 = self.stack.pop().unwrap();
                    let x1 = self.x;
                    let y1 = self.y + dy1;
                    let x2 = x1 + dx2;
                    let y2 = y1 + dy2;
                    let x3 = x2 + dx3;
                    let y3 = y2;
                    if !self.width_only {
                        self.path.segments.push(PathSegment::CurveTo {
                            x1,
                            y1,
                            x2,
                            y2,
                            x3,
                            y3,
                        });
                    }
                    self.x = x3;
                    self.y = y3;
                }
                31 => {
                    // hvcurveto: dx1 dx2 dy2 dy3
                    if self.stack.len() < 4 {
                        return Err("hvcurveto: stack underflow".to_string());
                    }
                    let dy3 = self.stack.pop().unwrap();
                    let dy2 = self.stack.pop().unwrap();
                    let dx2 = self.stack.pop().unwrap();
                    let dx1 = self.stack.pop().unwrap();
                    let x1 = self.x + dx1;
                    let y1 = self.y;
                    let x2 = x1 + dx2;
                    let y2 = y1 + dy2;
                    let x3 = x2;
                    let y3 = y2 + dy3;
                    if !self.width_only {
                        self.path.segments.push(PathSegment::CurveTo {
                            x1,
                            y1,
                            x2,
                            y2,
                            x3,
                            y3,
                        });
                    }
                    self.x = x3;
                    self.y = y3;
                }
                // Number encoding
                32..=246 => {
                    // Single-byte integer: value = b - 139
                    self.stack.push(b as f64 - 139.0);
                }
                247..=250 => {
                    // Two-byte positive: ((b - 247) * 256 + next) + 108
                    if pos >= data.len() {
                        break;
                    }
                    let b2 = data[pos];
                    pos += 1;
                    let val = ((b as i32 - 247) * 256 + b2 as i32) + 108;
                    self.stack.push(val as f64);
                }
                251..=254 => {
                    // Two-byte negative: -((b - 251) * 256 + next) - 108
                    if pos >= data.len() {
                        break;
                    }
                    let b2 = data[pos];
                    pos += 1;
                    let val = -((b as i32 - 251) * 256 + b2 as i32) - 108;
                    self.stack.push(val as f64);
                }
                255 => {
                    // Five-byte signed 32-bit integer
                    if pos + 4 > data.len() {
                        break;
                    }
                    let val = i32::from_be_bytes([
                        data[pos],
                        data[pos + 1],
                        data[pos + 2],
                        data[pos + 3],
                    ]);
                    pos += 4;
                    self.stack.push(val as f64);
                }
            }
        }
        Ok(())
    }

    fn execute_escape(&mut self, b2: u8) -> Result<(), String> {
        match b2 {
            0 => {
                // dotsection — ignore (hint), no args
            }
            1 => {
                // vstem3: x0 dx0 x1 dx1 x2 dx2 — ignore (hint), pop 6 args
                for _ in 0..6.min(self.stack.len()) {
                    self.stack.pop();
                }
            }
            2 => {
                // hstem3: y0 dy0 y1 dy1 y2 dy2 — ignore (hint), pop 6 args
                for _ in 0..6.min(self.stack.len()) {
                    self.stack.pop();
                }
            }
            6 => {
                // seac: asb adx ady bchar achar
                // Builds a composite glyph from base + accent characters
                if self.stack.len() < 5 {
                    return Err("seac: stack underflow".to_string());
                }
                let achar = self.stack.pop().unwrap() as u8;
                let bchar = self.stack.pop().unwrap() as u8;
                let ady = self.stack.pop().unwrap();
                let adx = self.stack.pop().unwrap();
                let asb = self.stack.pop().unwrap();

                // Look up base and accent glyph names in StandardEncoding
                let bname = STANDARD_ENCODING[bchar as usize];
                let aname = STANDARD_ENCODING[achar as usize];

                // Extract charstring data from lookup before executing (borrow checker)
                let bchar_data = self.cs_lookup.as_ref().and_then(|f| f(bname));
                let achar_data = self.cs_lookup.as_ref().and_then(|f| f(aname));

                if let Some(bchar_data) = bchar_data {
                    let saved_width_x = self.width_x;
                    let saved_width_y = self.width_y;
                    let saved_x = self.x;
                    let saved_y = self.y;

                    // Execute base character charstring
                    let decrypted = decrypt_charstring(&bchar_data, self.len_iv);
                    self.x = 0.0;
                    self.y = 0.0;
                    self.done = false;
                    self.execute(&decrypted)?;
                    let base_lsb = self.lsb_x;
                    self.done = false;

                    // Execute accent character charstring with offset
                    if let Some(achar_data) = achar_data {
                        let accent_x = adx - asb + base_lsb;
                        let accent_y = ady;
                        let decrypted = decrypt_charstring(&achar_data, self.len_iv);
                        self.x = accent_x;
                        self.y = accent_y;
                        self.execute(&decrypted)?;
                    }

                    // Restore original width (from the composite's hsbw/sbw)
                    self.width_x = saved_width_x;
                    self.width_y = saved_width_y;
                    self.x = saved_x;
                    self.y = saved_y;
                }
                // If no lookup available, seac produces no path (graceful degradation)
            }
            7 => {
                // sbw: sbx sby wx wy
                // Sets sidebearing and width. Does NOT emit a MoveTo —
                // the first real moveto in the glyph body will do that.
                if self.stack.len() < 4 {
                    return Err("sbw: stack underflow".to_string());
                }
                let wy = self.stack.pop().unwrap();
                let wx = self.stack.pop().unwrap();
                let sby = self.stack.pop().unwrap();
                let sbx = self.stack.pop().unwrap();
                self.lsb_x = sbx;
                self.lsb_y = sby;
                self.width_x = wx;
                self.width_y = wy;
                self.x = sbx;
                self.y = sby;
            }
            12 => {
                // div: num1 num2 → num1/num2
                if self.stack.len() < 2 {
                    return Err("div: stack underflow".to_string());
                }
                let b = self.stack.pop().unwrap();
                let a = self.stack.pop().unwrap();
                if b == 0.0 {
                    self.stack.push(0.0);
                } else {
                    self.stack.push(a / b);
                }
            }
            16 => {
                // callothersubr: args... n subr#
                if self.stack.len() < 2 {
                    return Err("callothersubr: stack underflow".to_string());
                }
                let subr_num = self.stack.pop().unwrap() as i32;
                let n_args = self.stack.pop().unwrap() as usize;

                if self.stack.len() < n_args {
                    return Err("callothersubr: not enough args".to_string());
                }

                // Pop arguments from charstring stack
                let mut args: Vec<f64> = Vec::with_capacity(n_args);
                for _ in 0..n_args {
                    args.push(self.stack.pop().unwrap());
                }
                args.reverse(); // Args were popped in reverse order

                match subr_num {
                    0 => {
                        // EndFlex: construct two bezier curves from flex points
                        // args[0] = flex_depth (unused — we always draw curves)
                        if self.flex_points.len() >= 7 {
                            let _p0 = self.flex_points[0]; // reference point
                            let p1 = self.flex_points[1];
                            let p2 = self.flex_points[2];
                            let p3 = self.flex_points[3];
                            let p4 = self.flex_points[4];
                            let p5 = self.flex_points[5];
                            let p6 = self.flex_points[6];

                            if !self.width_only {
                                // First curve: from current (should be p0) to p3
                                self.path.segments.push(PathSegment::CurveTo {
                                    x1: p1.0,
                                    y1: p1.1,
                                    x2: p2.0,
                                    y2: p2.1,
                                    x3: p3.0,
                                    y3: p3.1,
                                });
                                // Second curve: from p3 to p6
                                self.path.segments.push(PathSegment::CurveTo {
                                    x1: p4.0,
                                    y1: p4.1,
                                    x2: p5.0,
                                    y2: p5.1,
                                    x3: p6.0,
                                    y3: p6.1,
                                });
                            }
                            self.x = p6.0;
                            self.y = p6.1;
                        }

                        self.flex_active = false;
                        self.flex_points.clear();

                        // Push y then x onto ps_stack so pop+pop+setcurrentpoint
                        // gets the correct order (x on top, popped first into
                        // charstring stack, then y — matching PostForge)
                        self.ps_stack.push(self.y);
                        self.ps_stack.push(self.x);
                    }
                    1 => {
                        // StartFlex: begin accumulating flex points
                        // Do NOT pre-push current point — OtherSubrs 2 (AddFlex)
                        // handles all point accumulation (matching PostForge)
                        self.flex_active = true;
                        self.flex_points.clear();
                    }
                    2 => {
                        // AddFlex: add current point to flex list
                        self.flex_points.push((self.x, self.y));
                        // Push y then x onto ps_stack for the subsequent pop+pop
                        // in the standard flex subroutine (matching PostForge)
                        self.ps_stack.push(self.y);
                        self.ps_stack.push(self.x);
                    }
                    3 => {
                        // Hint replacement — push 3 onto ps_stack for pop
                        self.ps_stack.push(3.0);
                    }
                    _ => {
                        // Unknown OtherSubr — push args onto ps_stack
                        for &a in &args {
                            self.ps_stack.push(a);
                        }
                    }
                }
            }
            17 => {
                // pop: move value from OtherSubrs stack to charstring stack
                if let Some(val) = self.ps_stack.pop() {
                    self.stack.push(val);
                } else {
                    self.stack.push(0.0);
                }
            }
            33 => {
                // setcurrentpoint: x y
                if self.stack.len() < 2 {
                    return Err("setcurrentpoint: stack underflow".to_string());
                }
                let y = self.stack.pop().unwrap();
                let x = self.stack.pop().unwrap();
                self.x = x;
                self.y = y;
            }
            _ => {
                // Unknown escape — ignore
            }
        }
        Ok(())
    }
}

// Fix: p0 is used in the flex code above but the compiler may not see it.
// The flex code references p0 via flex_points[0] directly.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decrypt_charstring_basic() {
        // Encrypt some data with R=4330, then decrypt and verify
        let plain = b"\x8b\x0e"; // push 0 (0x8b = 139-139=0), endchar (0x0e = 14)
        let c1: u32 = 52845;
        let c2: u32 = 22719;
        let mut r: u32 = 4330;

        // Prepend 4 random bytes (zeros)
        let mut to_encrypt = vec![0u8; 4];
        to_encrypt.extend_from_slice(plain);

        let mut encrypted = Vec::new();
        for &p in &to_encrypt {
            let c = (p as u32 ^ (r >> 8)) as u8;
            encrypted.push(c);
            r = ((c as u32 + r) * c1 + c2) & 0xFFFF;
        }

        let decrypted = decrypt_charstring(&encrypted, 4);
        assert_eq!(decrypted, plain);
    }

    #[test]
    fn test_number_encoding_single_byte() {
        // Test that single-byte numbers are decoded correctly
        // byte 139 = 0, byte 140 = 1, byte 246 = 107, byte 32 = -107
        // Use hsbw to consume 2 values, then endchar
        let code = vec![
            139, // push 0 (sbx)
            140, // push 1 (wx)
            13,  // hsbw
            14,  // endchar
        ];
        let mut interp = CharstringInterp::new(&[], 4, true, None);
        interp.execute_inner(&code, 0).unwrap();
        assert!((interp.width_x - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_hsbw_sets_width() {
        // hsbw: sbx=0 wx=600
        // For 600: value = ((b-247)*256 + b2) + 108
        // 600 - 108 = 492; 492 / 256 = 1 rem 236 → b=248, b2=236
        let data = vec![
            139, // push 0 (sbx)
            248, 236, // push 600 (wx)
            13,  // hsbw
            14,  // endchar
        ];
        let mut interp = CharstringInterp::new(&[], 4, true, None);
        interp.execute_inner(&data, 0).unwrap();
        assert!((interp.width_x - 600.0).abs() < 0.01);
        assert!((interp.lsb_x - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_rmoveto_rlineto() {
        let data = vec![
            139, // push 0 (sbx)
            248,
            236, // push 600 (wx)
            13,  // hsbw
            // rmoveto: dx=100, dy=200
            139 + 100, // push 100
            139 + 107, // push 107 (max single byte)
            21,        // rmoveto
            // rlineto: dx=50, dy=50
            139 + 50, // push 50
            139 + 50, // push 50
            5,        // rlineto
            9,        // closepath
            14,       // endchar
        ];
        let mut interp = CharstringInterp::new(&[], 4, false, None);
        interp.execute_inner(&data, 0).unwrap();

        // hsbw(0, 600): sets x=0, y=0 but does NOT emit MoveTo
        // rmoveto(100, 107): x=100, y=107, emits MoveTo(100,107)
        // rlineto(50, 50): x=150, y=157, emits LineTo(150,157)
        // closepath
        assert_eq!(interp.path.segments.len(), 3); // moveto(rmoveto), lineto, closepath
        match &interp.path.segments[1] {
            PathSegment::LineTo(x, y) => {
                assert!((x - 150.0).abs() < 0.01);
                assert!((y - 157.0).abs() < 0.01);
            }
            _ => panic!("Expected LineTo"),
        }
    }

    #[test]
    fn test_execute_real_charstring() {
        // Load a real font and execute the 'space' charstring
        let font_path = std::path::Path::new(
            "/home/scott/Projects/postforge/postforge/resources/Font/NimbusSans-Regular.t1",
        );
        if !font_path.exists() {
            eprintln!("Skipping test — font file not found");
            return;
        }

        let data = std::fs::read(font_path).unwrap();
        let font = crate::type1_parser::parse_type1(&data).unwrap();

        // Execute 'space' charstring — should have a width but no path
        let space_cs = font.charstrings.get("space").expect("'space' charstring");
        let result = execute_charstring(space_cs, &font.subrs, font.len_iv, false).unwrap();
        assert!(result.width_x > 0.0, "space should have positive width");

        // Execute 'A' charstring — should have paths
        let a_cs = font.charstrings.get("A").expect("'A' charstring");
        let result = execute_charstring(a_cs, &font.subrs, font.len_iv, false).unwrap();
        assert!(result.width_x > 0.0, "A should have positive width");
        assert!(!result.path.is_empty(), "A should have path segments");

        // Width-only mode should produce same width but empty path
        let result_wo = execute_charstring(a_cs, &font.subrs, font.len_iv, true).unwrap();
        assert!((result_wo.width_x - result.width_x).abs() < 0.01);
        assert!(result_wo.path.is_empty());
    }

    #[test]
    fn test_execute_multiple_glyphs() {
        let font_path = std::path::Path::new(
            "/home/scott/Projects/postforge/postforge/resources/Font/NimbusSans-Regular.t1",
        );
        if !font_path.exists() {
            eprintln!("Skipping test — font file not found");
            return;
        }

        let data = std::fs::read(font_path).unwrap();
        let font = crate::type1_parser::parse_type1(&data).unwrap();

        // Execute several common glyphs
        for glyph_name in &["A", "B", "a", "b", "zero", "one", "period", "comma"] {
            if let Some(cs) = font.charstrings.get(*glyph_name) {
                let result = execute_charstring(cs, &font.subrs, font.len_iv, false).unwrap();
                assert!(
                    result.width_x > 0.0,
                    "'{}' should have positive width",
                    glyph_name
                );
                assert!(
                    !result.path.is_empty(),
                    "'{}' should have path segments",
                    glyph_name
                );
            }
        }
    }
}
