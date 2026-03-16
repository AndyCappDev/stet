// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF object model and serialization.

use std::io::Write;

/// A PDF object value.
#[allow(dead_code)]
pub enum PdfObj {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    Name(Vec<u8>),
    LitString(Vec<u8>),
    HexString(Vec<u8>),
    Array(Vec<PdfObj>),
    Dict(Vec<(Vec<u8>, PdfObj)>),
    Ref(u32),
}

impl PdfObj {
    /// Serialize this object to PDF syntax.
    pub fn write_to(&self, out: &mut Vec<u8>) {
        match self {
            PdfObj::Null => out.extend(b"null"),
            PdfObj::Bool(b) => {
                if *b {
                    out.extend(b"true");
                } else {
                    out.extend(b"false");
                }
            }
            PdfObj::Int(n) => write!(out, "{}", n).unwrap(),
            PdfObj::Real(f) => fmt_real(out, *f),
            PdfObj::Name(n) => {
                out.push(b'/');
                for &b in n {
                    if b > b' '
                        && b < 127
                        && b != b'#'
                        && b != b'('
                        && b != b')'
                        && b != b'<'
                        && b != b'>'
                        && b != b'['
                        && b != b']'
                        && b != b'{'
                        && b != b'}'
                        && b != b'/'
                        && b != b'%'
                    {
                        out.push(b);
                    } else {
                        write!(out, "#{:02X}", b).unwrap();
                    }
                }
            }
            PdfObj::LitString(s) => {
                out.push(b'(');
                for &b in s {
                    match b {
                        b'(' | b')' | b'\\' => {
                            out.push(b'\\');
                            out.push(b);
                        }
                        _ => out.push(b),
                    }
                }
                out.push(b')');
            }
            PdfObj::HexString(s) => {
                out.push(b'<');
                for &b in s {
                    write!(out, "{:02X}", b).unwrap();
                }
                out.push(b'>');
            }
            PdfObj::Array(items) => {
                out.push(b'[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(b' ');
                    }
                    item.write_to(out);
                }
                out.push(b']');
            }
            PdfObj::Dict(entries) => {
                out.extend(b"<<");
                for (key, val) in entries {
                    out.push(b'/');
                    out.extend(key);
                    out.push(b' ');
                    val.write_to(out);
                    out.push(b'\n');
                }
                out.extend(b">>");
            }
            PdfObj::Ref(n) => write!(out, "{} 0 R", n).unwrap(),
        }
    }

    /// Convenience: create a Name from a string.
    pub fn name(s: &str) -> Self {
        PdfObj::Name(s.as_bytes().to_vec())
    }

    /// Convenience: create a Ref.
    #[allow(dead_code)]
    pub fn reference(n: u32) -> Self {
        PdfObj::Ref(n)
    }
}

/// Format a float compactly for PDF output.
fn fmt_real(out: &mut Vec<u8>, v: f64) {
    if v == 0.0 {
        out.push(b'0');
    } else if v == v.round() && v.abs() < 2_147_483_647.0 {
        write!(out, "{}", v as i64).unwrap();
    } else {
        let s = format!("{:.6}", v);
        let s = s.trim_end_matches('0');
        let s = s.trim_end_matches('.');
        out.extend(s.as_bytes());
    }
}
