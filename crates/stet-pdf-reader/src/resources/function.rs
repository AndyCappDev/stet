// stet-pdf-reader
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF Function evaluator (Types 0, 2, 3, 4).

use crate::error::PdfError;
use crate::objects::{PdfDict, PdfObj};
use crate::resolver::Resolver;

/// A parsed PDF function.
#[derive(Clone, Debug)]
pub enum PdfFunction {
    /// Type 0: Sampled function.
    Sampled {
        domain: Vec<[f64; 2]>,
        range: Vec<[f64; 2]>,
        size: Vec<u32>,
        bps: u32,
        encode: Vec<[f64; 2]>,
        decode: Vec<[f64; 2]>,
        samples: Vec<f64>,
        n_outputs: usize,
    },
    /// Type 2: Exponential interpolation.
    Exponential {
        domain: Vec<[f64; 2]>,
        range: Vec<[f64; 2]>,
        c0: Vec<f64>,
        c1: Vec<f64>,
        n: f64,
    },
    /// Type 3: Stitching function.
    Stitching {
        domain: Vec<[f64; 2]>,
        range: Vec<[f64; 2]>,
        functions: Vec<PdfFunction>,
        bounds: Vec<f64>,
        encode: Vec<[f64; 2]>,
    },
    /// Type 4: PostScript calculator.
    Calculator {
        domain: Vec<[f64; 2]>,
        range: Vec<[f64; 2]>,
        tokens: Vec<CalcToken>,
    },
}

/// Token for Type 4 calculator functions.
#[derive(Clone, Debug)]
pub enum CalcToken {
    Number(f64),
    Bool(bool),
    // Arithmetic
    Add,
    Sub,
    Mul,
    Div,
    Idiv,
    Mod,
    Neg,
    Abs,
    Ceiling,
    Floor,
    Round,
    Truncate,
    Sqrt,
    Exp,
    Ln,
    Log,
    Sin,
    Cos,
    Atan,
    // Relational/boolean
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    And,
    Or,
    Xor,
    Not,
    Bitshift,
    // Stack
    Dup,
    Exch,
    Pop,
    Copy,
    Index,
    Roll,
    // Conditional
    If(Vec<CalcToken>),
    IfElse(Vec<CalcToken>, Vec<CalcToken>),
    // Conversion
    Cvi,
    Cvr,
    True,
    False,
}

impl PdfFunction {
    /// Parse a PDF function from a dict/stream object.
    pub fn parse(obj: &PdfObj, resolver: &Resolver) -> Result<Self, PdfError> {
        let resolved = resolver.deref(obj)?;
        let dict = resolved
            .as_dict()
            .ok_or(PdfError::Other("function is not a dict/stream".into()))?;

        let fn_type =
            dict.get_int(b"FunctionType")
                .ok_or(PdfError::Other("function missing FunctionType".into()))? as i32;

        let domain = parse_domain_range(dict, b"Domain")?;
        let range = parse_domain_range(dict, b"Range").unwrap_or_default();

        match fn_type {
            0 => Self::parse_sampled(dict, obj, domain, range, resolver),
            2 => Self::parse_exponential(dict, domain, range),
            3 => Self::parse_stitching(dict, domain, range, resolver),
            4 => Self::parse_calculator(obj, domain, range, resolver),
            _ => Err(PdfError::Other(format!(
                "unsupported function type {fn_type}"
            ))),
        }
    }

    /// Evaluate the function for given inputs.
    pub fn evaluate(&self, inputs: &[f64]) -> Vec<f64> {
        match self {
            Self::Sampled {
                domain,
                range,
                size,
                encode,
                decode,
                samples,
                n_outputs,
                ..
            } => evaluate_sampled(
                inputs, domain, range, size, encode, decode, samples, *n_outputs,
            ),
            Self::Exponential {
                domain,
                range,
                c0,
                c1,
                n,
            } => evaluate_exponential(inputs, domain, range, c0, c1, *n),
            Self::Stitching {
                domain,
                range,
                functions,
                bounds,
                encode,
            } => evaluate_stitching(inputs, domain, range, functions, bounds, encode),
            Self::Calculator {
                domain,
                range,
                tokens,
            } => evaluate_calculator(inputs, domain, range, tokens),
        }
    }

    /// Number of output values.
    pub fn n_outputs(&self) -> usize {
        match self {
            Self::Sampled { n_outputs, .. } => *n_outputs,
            Self::Exponential { c0, .. } => c0.len(),
            Self::Stitching {
                range, functions, ..
            } => {
                if !range.is_empty() {
                    range.len()
                } else if let Some(f) = functions.first() {
                    f.n_outputs()
                } else {
                    1
                }
            }
            Self::Calculator { range, .. } => range.len(),
        }
    }

    fn parse_sampled(
        dict: &PdfDict,
        obj: &PdfObj,
        domain: Vec<[f64; 2]>,
        range: Vec<[f64; 2]>,
        resolver: &Resolver,
    ) -> Result<Self, PdfError> {
        let size: Vec<u32> = dict
            .get_array(b"Size")
            .ok_or(PdfError::Other("sampled function missing Size".into()))?
            .iter()
            .filter_map(|o| o.as_int().map(|n| n as u32))
            .collect();

        let bps = dict
            .get_int(b"BitsPerSample")
            .ok_or(PdfError::Other("missing BitsPerSample".into()))? as u32;

        let n_outputs = range.len();

        let encode = if let Ok(enc) = parse_domain_range(dict, b"Encode") {
            enc
        } else {
            size.iter().map(|s| [0.0, (*s as f64) - 1.0]).collect()
        };

        let decode = if let Ok(dec) = parse_domain_range(dict, b"Decode") {
            dec
        } else {
            range.clone()
        };

        // Read sample data
        let data = resolver.stream_data_from_obj(obj)?;
        let max_val = ((1u64 << bps) - 1) as f64;
        let total_samples: usize = size.iter().map(|s| *s as usize).product::<usize>() * n_outputs;
        let mut samples = Vec::with_capacity(total_samples);

        let mut bit_offset = 0usize;
        for _ in 0..total_samples {
            let byte_idx = bit_offset / 8;
            let bit_idx = bit_offset % 8;
            let mut val = 0u64;
            let mut bits_left = bps;
            let mut cur_byte = byte_idx;
            let mut cur_bit = bit_idx;

            while bits_left > 0 && cur_byte < data.len() {
                let avail = 8 - cur_bit as u32;
                let take = bits_left.min(avail);
                let shift = avail - take;
                let mask = ((1u64 << take) - 1) << shift;
                val = (val << take) | ((data[cur_byte] as u64 & mask) >> shift);
                bits_left -= take;
                cur_bit = 0;
                cur_byte += 1;
            }

            samples.push(val as f64 / max_val);
            bit_offset += bps as usize;
        }

        Ok(Self::Sampled {
            domain,
            range,
            size,
            bps,
            encode,
            decode,
            samples,
            n_outputs,
        })
    }

    fn parse_exponential(
        dict: &PdfDict,
        domain: Vec<[f64; 2]>,
        range: Vec<[f64; 2]>,
    ) -> Result<Self, PdfError> {
        let n = dict
            .get_f64(b"N")
            .ok_or(PdfError::Other("exponential function missing N".into()))?;

        let n_outputs = if !range.is_empty() { range.len() } else { 1 };

        let c0 = dict
            .get_array(b"C0")
            .map(|arr| arr.iter().filter_map(|o| o.as_f64()).collect())
            .unwrap_or_else(|| vec![0.0; n_outputs]);

        let c1 = dict
            .get_array(b"C1")
            .map(|arr| arr.iter().filter_map(|o| o.as_f64()).collect())
            .unwrap_or_else(|| vec![1.0; n_outputs]);

        Ok(Self::Exponential {
            domain,
            range,
            c0,
            c1,
            n,
        })
    }

    fn parse_stitching(
        dict: &PdfDict,
        domain: Vec<[f64; 2]>,
        range: Vec<[f64; 2]>,
        resolver: &Resolver,
    ) -> Result<Self, PdfError> {
        let fn_arr = dict
            .get_array(b"Functions")
            .ok_or(PdfError::Other("stitching missing Functions".into()))?;

        let mut functions = Vec::with_capacity(fn_arr.len());
        for fn_obj in fn_arr {
            functions.push(PdfFunction::parse(fn_obj, resolver)?);
        }

        let bounds: Vec<f64> = dict
            .get_array(b"Bounds")
            .ok_or(PdfError::Other("stitching missing Bounds".into()))?
            .iter()
            .filter_map(|o| o.as_f64())
            .collect();

        let encode = parse_domain_range(dict, b"Encode")
            .unwrap_or_else(|_| functions.iter().map(|_| [0.0, 1.0]).collect());

        Ok(Self::Stitching {
            domain,
            range,
            functions,
            bounds,
            encode,
        })
    }

    fn parse_calculator(
        obj: &PdfObj,
        domain: Vec<[f64; 2]>,
        range: Vec<[f64; 2]>,
        resolver: &Resolver,
    ) -> Result<Self, PdfError> {
        let data = resolver.stream_data_from_obj(obj)?;
        let code = std::str::from_utf8(&data)
            .map_err(|_| PdfError::Other("calculator function: invalid UTF-8".into()))?;
        let tokens = parse_calc_tokens(code)?;
        Ok(Self::Calculator {
            domain,
            range,
            tokens,
        })
    }
}

// === Parse helpers ===

fn parse_domain_range(dict: &PdfDict, key: &[u8]) -> Result<Vec<[f64; 2]>, PdfError> {
    let arr = dict
        .get_array(key)
        .ok_or_else(|| PdfError::Other(format!("missing /{}", String::from_utf8_lossy(key))))?;
    let vals: Vec<f64> = arr.iter().filter_map(|o| o.as_f64()).collect();
    Ok(vals
        .chunks(2)
        .map(|c| [c[0], c.get(1).copied().unwrap_or(c[0])])
        .collect())
}

// === Evaluation ===

fn clamp(x: f64, lo: f64, hi: f64) -> f64 {
    x.max(lo).min(hi)
}

fn interpolate(x: f64, x_min: f64, x_max: f64, y_min: f64, y_max: f64) -> f64 {
    if (x_max - x_min).abs() < 1e-30 {
        return y_min;
    }
    y_min + (x - x_min) * (y_max - y_min) / (x_max - x_min)
}

#[allow(clippy::too_many_arguments)]
fn evaluate_sampled(
    inputs: &[f64],
    domain: &[[f64; 2]],
    range: &[[f64; 2]],
    size: &[u32],
    encode: &[[f64; 2]],
    decode: &[[f64; 2]],
    samples: &[f64],
    n_outputs: usize,
) -> Vec<f64> {
    let n_inputs = domain.len();

    // Clamp and encode inputs
    let mut encoded = Vec::with_capacity(n_inputs);
    for i in 0..n_inputs.min(inputs.len()) {
        let x = clamp(inputs[i], domain[i][0], domain[i][1]);
        let e = interpolate(x, domain[i][0], domain[i][1], encode[i][0], encode[i][1]);
        let e = clamp(e, 0.0, (size[i] as f64) - 1.0);
        encoded.push(e);
    }

    // For 1D input, simple linear interpolation
    if n_inputs == 1 && !encoded.is_empty() {
        let e = encoded[0];
        let i0 = e.floor() as usize;
        let i1 = (i0 + 1).min(size[0] as usize - 1);
        let frac = e - e.floor();

        let mut result = Vec::with_capacity(n_outputs);
        for j in 0..n_outputs {
            let s0 = samples.get(i0 * n_outputs + j).copied().unwrap_or(0.0);
            let s1 = samples.get(i1 * n_outputs + j).copied().unwrap_or(0.0);
            let val = s0 + frac * (s1 - s0);
            let decoded = if j < decode.len() {
                interpolate(val, 0.0, 1.0, decode[j][0], decode[j][1])
            } else {
                val
            };
            let clamped = if j < range.len() {
                clamp(decoded, range[j][0], range[j][1])
            } else {
                decoded
            };
            result.push(clamped);
        }
        return result;
    }

    // Multi-dimensional: multilinear interpolation
    // For N inputs, interpolate across 2^N corners of the hypercube
    let n = n_inputs.min(encoded.len());

    // Compute floor indices and fractional parts for each dimension
    let mut i0s = Vec::with_capacity(n);
    let mut fracs = Vec::with_capacity(n);
    for dim in 0..n {
        let e = encoded[dim];
        let lo = e.floor() as usize;
        let lo = lo.min(size[dim] as usize - 2); // ensure lo+1 is valid
        i0s.push(lo);
        fracs.push(e - lo as f64);
    }

    // Compute strides for each dimension.
    // PDF spec: first input varies fastest, so dim 0 has the smallest stride.
    let mut strides = vec![0usize; n];
    strides[0] = n_outputs;
    for dim in 1..n {
        strides[dim] = strides[dim - 1] * size[dim - 1] as usize;
    }

    // Iterate over 2^n corners and accumulate weighted contributions
    let n_corners = 1usize << n;
    let mut result = vec![0.0f64; n_outputs];
    for corner in 0..n_corners {
        let mut weight = 1.0f64;
        let mut index = 0usize;
        for dim in 0..n {
            if corner & (1 << dim) != 0 {
                weight *= fracs[dim];
                index += (i0s[dim] + 1) * strides[dim];
            } else {
                weight *= 1.0 - fracs[dim];
                index += i0s[dim] * strides[dim];
            }
        }
        for (j, r) in result.iter_mut().enumerate() {
            *r += weight * samples.get(index + j).copied().unwrap_or(0.0);
        }
    }

    // Decode and clamp
    for j in 0..n_outputs {
        if j < decode.len() {
            result[j] = interpolate(result[j], 0.0, 1.0, decode[j][0], decode[j][1]);
        }
        if j < range.len() {
            result[j] = clamp(result[j], range[j][0], range[j][1]);
        }
    }
    result
}

fn evaluate_exponential(
    inputs: &[f64],
    domain: &[[f64; 2]],
    range: &[[f64; 2]],
    c0: &[f64],
    c1: &[f64],
    n: f64,
) -> Vec<f64> {
    let x = if !inputs.is_empty() && !domain.is_empty() {
        clamp(inputs[0], domain[0][0], domain[0][1])
    } else {
        0.0
    };

    let x_n = x.powf(n);
    let mut result = Vec::with_capacity(c0.len());
    for i in 0..c0.len() {
        let val = c0[i] + x_n * (c1.get(i).copied().unwrap_or(1.0) - c0[i]);
        let clamped = if i < range.len() {
            clamp(val, range[i][0], range[i][1])
        } else {
            val
        };
        result.push(clamped);
    }
    result
}

fn evaluate_stitching(
    inputs: &[f64],
    domain: &[[f64; 2]],
    range: &[[f64; 2]],
    functions: &[PdfFunction],
    bounds: &[f64],
    encode: &[[f64; 2]],
) -> Vec<f64> {
    if functions.is_empty() {
        return vec![0.0];
    }

    let x = if !inputs.is_empty() && !domain.is_empty() {
        clamp(inputs[0], domain[0][0], domain[0][1])
    } else {
        0.0
    };

    // Find which sub-function to use
    let mut k = 0;
    for (i, &b) in bounds.iter().enumerate() {
        if x < b {
            k = i;
            break;
        }
        k = i + 1;
    }
    k = k.min(functions.len() - 1);

    // Determine domain bounds for this sub-function
    let d_lo = if k == 0 {
        domain.first().map(|d| d[0]).unwrap_or(0.0)
    } else {
        bounds[k - 1]
    };
    let d_hi = if k >= bounds.len() {
        domain.first().map(|d| d[1]).unwrap_or(1.0)
    } else {
        bounds[k]
    };

    // Encode
    let enc = encode.get(k).copied().unwrap_or([0.0, 1.0]);
    let x_enc = interpolate(x, d_lo, d_hi, enc[0], enc[1]);

    let mut result = functions[k].evaluate(&[x_enc]);

    // Clamp to range
    for (i, val) in result.iter_mut().enumerate() {
        if i < range.len() {
            *val = clamp(*val, range[i][0], range[i][1]);
        }
    }
    result
}

fn evaluate_calculator(
    inputs: &[f64],
    domain: &[[f64; 2]],
    range: &[[f64; 2]],
    tokens: &[CalcToken],
) -> Vec<f64> {
    // Clamp inputs to domain
    let mut stack: Vec<f64> = Vec::with_capacity(16);
    for (i, &x) in inputs.iter().enumerate() {
        let clamped = if i < domain.len() {
            clamp(x, domain[i][0], domain[i][1])
        } else {
            x
        };
        stack.push(clamped);
    }

    execute_calc_tokens(&mut stack, tokens);

    // Clamp outputs to range
    let n_out = range.len();
    let mut result = Vec::with_capacity(n_out);
    for i in 0..n_out {
        let val = if i < stack.len() {
            stack[stack.len() - n_out + i]
        } else {
            0.0
        };
        result.push(clamp(val, range[i][0], range[i][1]));
    }
    result
}

fn execute_calc_tokens(stack: &mut Vec<f64>, tokens: &[CalcToken]) {
    for token in tokens {
        match token {
            CalcToken::Number(n) => stack.push(*n),
            CalcToken::Bool(b) => stack.push(if *b { 1.0 } else { 0.0 }),
            CalcToken::True => stack.push(1.0),
            CalcToken::False => stack.push(0.0),

            // Arithmetic
            CalcToken::Add => bin_op(stack, |a, b| a + b),
            CalcToken::Sub => bin_op(stack, |a, b| a - b),
            CalcToken::Mul => bin_op(stack, |a, b| a * b),
            CalcToken::Div => bin_op(stack, |a, b| if b != 0.0 { a / b } else { 0.0 }),
            CalcToken::Idiv => bin_op(stack, |a, b| {
                if b != 0.0 {
                    ((a as i64) / (b as i64)) as f64
                } else {
                    0.0
                }
            }),
            CalcToken::Mod => bin_op(stack, |a, b| {
                if b != 0.0 {
                    ((a as i64) % (b as i64)) as f64
                } else {
                    0.0
                }
            }),
            CalcToken::Neg => un_op(stack, |a| -a),
            CalcToken::Abs => un_op(stack, |a| a.abs()),
            CalcToken::Ceiling => un_op(stack, |a| a.ceil()),
            CalcToken::Floor => un_op(stack, |a| a.floor()),
            CalcToken::Round => un_op(stack, |a| a.round()),
            CalcToken::Truncate => un_op(stack, |a| a.trunc()),
            CalcToken::Sqrt => un_op(stack, |a| a.sqrt()),
            CalcToken::Exp => bin_op(stack, |a, b| a.powf(b)),
            CalcToken::Ln => un_op(stack, |a| a.ln()),
            CalcToken::Log => un_op(stack, |a| a.log10()),
            CalcToken::Sin => un_op(stack, |a| a.to_radians().sin()),
            CalcToken::Cos => un_op(stack, |a| a.to_radians().cos()),
            CalcToken::Atan => bin_op(stack, |a, b| {
                let deg = a.atan2(b).to_degrees();
                if deg < 0.0 { deg + 360.0 } else { deg }
            }),

            // Relational
            CalcToken::Eq => bin_op(stack, |a, b| if (a - b).abs() < 1e-10 { 1.0 } else { 0.0 }),
            CalcToken::Ne => bin_op(stack, |a, b| if (a - b).abs() >= 1e-10 { 1.0 } else { 0.0 }),
            CalcToken::Gt => bin_op(stack, |a, b| if a > b { 1.0 } else { 0.0 }),
            CalcToken::Ge => bin_op(stack, |a, b| if a >= b { 1.0 } else { 0.0 }),
            CalcToken::Lt => bin_op(stack, |a, b| if a < b { 1.0 } else { 0.0 }),
            CalcToken::Le => bin_op(stack, |a, b| if a <= b { 1.0 } else { 0.0 }),
            CalcToken::And => bin_op(stack, |a, b| ((a as i64) & (b as i64)) as f64),
            CalcToken::Or => bin_op(stack, |a, b| ((a as i64) | (b as i64)) as f64),
            CalcToken::Xor => bin_op(stack, |a, b| ((a as i64) ^ (b as i64)) as f64),
            CalcToken::Not => un_op(stack, |a| if a == 0.0 { 1.0 } else { 0.0 }),
            CalcToken::Bitshift => bin_op(stack, |a, b| {
                let n = a as i64;
                let shift = b as i32;
                if shift > 0 {
                    (n << shift) as f64
                } else {
                    (n >> (-shift)) as f64
                }
            }),

            // Stack
            CalcToken::Dup => {
                if let Some(&top) = stack.last() {
                    stack.push(top);
                }
            }
            CalcToken::Exch => {
                let len = stack.len();
                if len >= 2 {
                    stack.swap(len - 1, len - 2);
                }
            }
            CalcToken::Pop => {
                stack.pop();
            }
            CalcToken::Copy => {
                if let Some(&n) = stack.last() {
                    stack.pop();
                    let n = n as usize;
                    let len = stack.len();
                    if n <= len {
                        let items: Vec<f64> = stack[len - n..].to_vec();
                        stack.extend_from_slice(&items);
                    }
                }
            }
            CalcToken::Index => {
                if let Some(&n) = stack.last() {
                    stack.pop();
                    let idx = n as usize;
                    let len = stack.len();
                    if idx < len {
                        stack.push(stack[len - 1 - idx]);
                    }
                }
            }
            CalcToken::Roll => {
                let len = stack.len();
                if len >= 2 {
                    let j = stack.pop().unwrap() as i32;
                    let n = stack.pop().unwrap() as usize;
                    if n > 0 && n <= stack.len() {
                        let start = stack.len() - n;
                        let j = ((j % n as i32) + n as i32) as usize % n;
                        let mut temp: Vec<f64> = stack[start..].to_vec();
                        temp.rotate_right(j);
                        stack[start..].copy_from_slice(&temp);
                    }
                }
            }

            // Conditional
            CalcToken::If(body) => {
                if let Some(&cond) = stack.last() {
                    stack.pop();
                    if cond != 0.0 {
                        execute_calc_tokens(stack, body);
                    }
                }
            }
            CalcToken::IfElse(if_body, else_body) => {
                if let Some(&cond) = stack.last() {
                    stack.pop();
                    if cond != 0.0 {
                        execute_calc_tokens(stack, if_body);
                    } else {
                        execute_calc_tokens(stack, else_body);
                    }
                }
            }

            // Conversion
            CalcToken::Cvi => un_op(stack, |a| a.trunc()),
            CalcToken::Cvr => {} // already f64
        }
    }
}

fn bin_op(stack: &mut Vec<f64>, f: impl FnOnce(f64, f64) -> f64) {
    if stack.len() >= 2 {
        let b = stack.pop().unwrap();
        let a = stack.pop().unwrap();
        stack.push(f(a, b));
    }
}

fn un_op(stack: &mut Vec<f64>, f: impl FnOnce(f64) -> f64) {
    if let Some(a) = stack.pop() {
        stack.push(f(a));
    }
}

// === Token parser for Type 4 calculator ===

fn parse_calc_tokens(code: &str) -> Result<Vec<CalcToken>, PdfError> {
    let code = code.trim();
    // Strip outer { }
    let code = if code.starts_with('{') && code.ends_with('}') {
        &code[1..code.len() - 1]
    } else {
        code
    };

    parse_token_sequence(code)
}

fn parse_token_sequence(code: &str) -> Result<Vec<CalcToken>, PdfError> {
    let mut tokens = Vec::new();
    let mut chars = code.chars().peekable();

    while let Some(&ch) = chars.peek() {
        if ch.is_whitespace() {
            chars.next();
            continue;
        }

        if ch == '{' {
            chars.next();
            // Find matching }
            let body = collect_brace_body(&mut chars)?;
            let body_tokens = parse_token_sequence(&body)?;

            // Check if next non-ws token is "if" or "ifelse"
            // Skip whitespace
            while chars.peek().is_some_and(|c| c.is_whitespace()) {
                chars.next();
            }

            // Peek at next word
            let saved: String = chars.clone().collect();
            if saved.starts_with('{') {
                // This might be the if-body in an ifelse
                chars.next(); // skip {
                let else_body = collect_brace_body(&mut chars)?;
                let else_tokens = parse_token_sequence(&else_body)?;
                // Skip whitespace
                while chars.peek().is_some_and(|c| c.is_whitespace()) {
                    chars.next();
                }
                // Expect "ifelse"
                let word = collect_word(&mut chars);
                if word == "ifelse" {
                    tokens.push(CalcToken::IfElse(body_tokens, else_tokens));
                } else {
                    // Not ifelse — push both bodies and the word
                    tokens.push(CalcToken::If(body_tokens));
                    tokens.push(CalcToken::If(else_tokens));
                    if let Some(tok) = word_to_token(&word) {
                        tokens.push(tok);
                    }
                }
            } else {
                let word = collect_word(&mut chars);
                if word == "if" {
                    tokens.push(CalcToken::If(body_tokens));
                } else {
                    // Just a procedure body — shouldn't happen in Type 4, but handle gracefully
                    tokens.push(CalcToken::If(body_tokens));
                    if let Some(tok) = word_to_token(&word) {
                        tokens.push(tok);
                    }
                }
            }
            continue;
        }

        // Collect a word
        let word = collect_word(&mut chars);
        if word.is_empty() {
            chars.next(); // skip unknown char
            continue;
        }

        // Try as number first
        if let Ok(n) = word.parse::<f64>() {
            tokens.push(CalcToken::Number(n));
        } else if let Some(tok) = word_to_token(&word) {
            tokens.push(tok);
        }
        // else skip unknown
    }

    Ok(tokens)
}

fn collect_brace_body(
    chars: &mut std::iter::Peekable<std::str::Chars>,
) -> Result<String, PdfError> {
    let mut body = String::new();
    let mut depth = 1;
    for ch in chars.by_ref() {
        if ch == '{' {
            depth += 1;
            body.push(ch);
        } else if ch == '}' {
            depth -= 1;
            if depth == 0 {
                return Ok(body);
            }
            body.push(ch);
        } else {
            body.push(ch);
        }
    }
    Err(PdfError::Other(
        "unterminated { in calculator function".into(),
    ))
}

fn collect_word(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
    let mut word = String::new();
    while let Some(&ch) = chars.peek() {
        if ch.is_whitespace() || ch == '{' || ch == '}' {
            break;
        }
        word.push(ch);
        chars.next();
    }
    word
}

fn word_to_token(word: &str) -> Option<CalcToken> {
    Some(match word {
        "add" => CalcToken::Add,
        "sub" => CalcToken::Sub,
        "mul" => CalcToken::Mul,
        "div" => CalcToken::Div,
        "idiv" => CalcToken::Idiv,
        "mod" => CalcToken::Mod,
        "neg" => CalcToken::Neg,
        "abs" => CalcToken::Abs,
        "ceiling" => CalcToken::Ceiling,
        "floor" => CalcToken::Floor,
        "round" => CalcToken::Round,
        "truncate" => CalcToken::Truncate,
        "sqrt" => CalcToken::Sqrt,
        "exp" => CalcToken::Exp,
        "ln" => CalcToken::Ln,
        "log" => CalcToken::Log,
        "sin" => CalcToken::Sin,
        "cos" => CalcToken::Cos,
        "atan" => CalcToken::Atan,
        "eq" => CalcToken::Eq,
        "ne" => CalcToken::Ne,
        "gt" => CalcToken::Gt,
        "ge" => CalcToken::Ge,
        "lt" => CalcToken::Lt,
        "le" => CalcToken::Le,
        "and" => CalcToken::And,
        "or" => CalcToken::Or,
        "xor" => CalcToken::Xor,
        "not" => CalcToken::Not,
        "bitshift" => CalcToken::Bitshift,
        "dup" => CalcToken::Dup,
        "exch" => CalcToken::Exch,
        "pop" => CalcToken::Pop,
        "copy" => CalcToken::Copy,
        "index" => CalcToken::Index,
        "roll" => CalcToken::Roll,
        "cvi" => CalcToken::Cvi,
        "cvr" => CalcToken::Cvr,
        "true" => CalcToken::True,
        "false" => CalcToken::False,
        "if" | "ifelse" => return None, // handled by brace logic
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exponential_function() {
        let f = PdfFunction::Exponential {
            domain: vec![[0.0, 1.0]],
            range: vec![[0.0, 1.0], [0.0, 1.0], [0.0, 1.0]],
            c0: vec![1.0, 0.0, 0.0],
            c1: vec![0.0, 0.0, 1.0],
            n: 1.0,
        };
        let result = f.evaluate(&[0.0]);
        assert_eq!(result, vec![1.0, 0.0, 0.0]);

        let result = f.evaluate(&[1.0]);
        assert_eq!(result, vec![0.0, 0.0, 1.0]);

        let result = f.evaluate(&[0.5]);
        assert!((result[0] - 0.5).abs() < 1e-10);
    }

    #[test]
    fn calculator_simple() {
        let tokens = parse_calc_tokens("{ 2 mul }").unwrap();
        let f = PdfFunction::Calculator {
            domain: vec![[0.0, 1.0]],
            range: vec![[0.0, 2.0]],
            tokens,
        };
        let result = f.evaluate(&[0.5]);
        assert!((result[0] - 1.0).abs() < 1e-10);
    }
}
