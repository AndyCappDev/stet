// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Graphics state: transforms, paths, colors, and rendering parameters.

use crate::display_list::DisplayList;
use crate::object::PsObject;
use std::sync::Arc;

/// Round to 10 decimal places to eliminate floating-point artifacts.
/// Matches PostForge's `Decimal.quantize(Decimal('0.0000000001'))`.
#[inline]
fn round10(v: f64) -> f64 {
    (v * 1e10).round() / 1e10
}

/// Affine transformation matrix `[a, b, c, d, tx, ty]`.
///
/// Transforms point (x, y) to:
///   x' = a*x + c*y + tx
///   y' = b*x + d*y + ty
#[derive(Clone, Copy, Debug)]
pub struct Matrix {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub tx: f64,
    pub ty: f64,
}

impl Matrix {
    /// Identity matrix.
    pub fn identity() -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            tx: 0.0,
            ty: 0.0,
        }
    }

    /// Create from 6 components.
    pub fn new(a: f64, b: f64, c: f64, d: f64, tx: f64, ty: f64) -> Self {
        Self { a, b, c, d, tx, ty }
    }

    /// Translation matrix.
    pub fn translate(tx: f64, ty: f64) -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            tx,
            ty,
        }
    }

    /// Scaling matrix.
    pub fn scale(sx: f64, sy: f64) -> Self {
        Self {
            a: sx,
            b: 0.0,
            c: 0.0,
            d: sy,
            tx: 0.0,
            ty: 0.0,
        }
    }

    /// Rotation matrix (angle in degrees).
    pub fn rotate(angle_degrees: f64) -> Self {
        let rad = angle_degrees.to_radians();
        let cos = rad.cos();
        let sin = rad.sin();
        Self {
            a: cos,
            b: sin,
            c: -sin,
            d: cos,
            tx: 0.0,
            ty: 0.0,
        }
    }

    /// Matrix multiplication: self * other.
    /// Applies `other` first, then `self`.
    /// Results rounded to 10 decimal places (matching PostForge).
    pub fn multiply(&self, other: &Matrix) -> Matrix {
        Matrix {
            a: round10(self.a * other.a + self.c * other.b),
            b: round10(self.b * other.a + self.d * other.b),
            c: round10(self.a * other.c + self.c * other.d),
            d: round10(self.b * other.c + self.d * other.d),
            tx: round10(self.a * other.tx + self.c * other.ty + self.tx),
            ty: round10(self.b * other.tx + self.d * other.ty + self.ty),
        }
    }

    /// Concatenate: PostScript `concat` semantics (CTM' = M × CTM in row-vector convention).
    ///
    /// PostScript uses row-vector convention: point × matrix. The PLRM defines
    /// `concat` as CTM' = M × CTM where M is applied in user space first, then CTM
    /// transforms to device space. In our column-vector `multiply`, this is
    /// `self.multiply(other)` (i.e., CTM × M in column-vector = M × CTM in row-vector).
    pub fn concat(&self, other: &Matrix) -> Matrix {
        self.multiply(other)
    }

    /// Transform a point (x, y) → (x', y').
    /// Results rounded to 10 decimal places (matching PostForge).
    pub fn transform_point(&self, x: f64, y: f64) -> (f64, f64) {
        (
            round10(self.a * x + self.c * y + self.tx),
            round10(self.b * x + self.d * y + self.ty),
        )
    }

    /// Transform a distance vector (dx, dy) → (dx', dy') (no translation).
    /// Results rounded to 10 decimal places (matching PostForge).
    pub fn transform_delta(&self, dx: f64, dy: f64) -> (f64, f64) {
        (
            round10(self.a * dx + self.c * dy),
            round10(self.b * dx + self.d * dy),
        )
    }

    /// Determinant of the 2×2 portion.
    /// Result rounded to 10 decimal places (matching PostForge).
    pub fn determinant(&self) -> f64 {
        round10(self.a * self.d - self.b * self.c)
    }

    /// Inverse matrix. Returns None if singular (det == 0).
    /// Results rounded to 10 decimal places (matching PostForge).
    pub fn invert(&self) -> Option<Matrix> {
        let det = self.determinant();
        if det.abs() < 1e-15 {
            return None;
        }
        let inv_det = 1.0 / det;
        Some(Matrix {
            a: round10(self.d * inv_det),
            b: round10(-self.b * inv_det),
            c: round10(-self.c * inv_det),
            d: round10(self.a * inv_det),
            tx: round10((self.c * self.ty - self.d * self.tx) * inv_det),
            ty: round10((self.b * self.tx - self.a * self.ty) * inv_det),
        })
    }

    /// Get components as array of 6 f64.
    pub fn to_array(&self) -> [f64; 6] {
        [self.a, self.b, self.c, self.d, self.tx, self.ty]
    }
}

/// Path segment in device space (transformed through CTM at construction time).
#[derive(Clone, Debug)]
pub enum PathSegment {
    MoveTo(f64, f64),
    LineTo(f64, f64),
    CurveTo {
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
        x3: f64,
        y3: f64,
    },
    ClosePath,
}

/// A PostScript path (collection of segments in device space).
#[derive(Clone, Debug)]
pub struct PsPath {
    pub segments: Vec<PathSegment>,
}

impl PsPath {
    pub fn new() -> Self {
        Self {
            segments: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    pub fn clear(&mut self) {
        self.segments.clear();
    }
}

impl Default for PsPath {
    fn default() -> Self {
        Self::new()
    }
}

/// Line cap style.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineCap {
    Butt = 0,
    Round = 1,
    Square = 2,
}

impl LineCap {
    pub fn from_i32(v: i32) -> Option<Self> {
        match v {
            0 => Some(Self::Butt),
            1 => Some(Self::Round),
            2 => Some(Self::Square),
            _ => None,
        }
    }
}

/// Line join style.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineJoin {
    Miter = 0,
    Round = 1,
    Bevel = 2,
}

impl LineJoin {
    pub fn from_i32(v: i32) -> Option<Self> {
        match v {
            0 => Some(Self::Miter),
            1 => Some(Self::Round),
            2 => Some(Self::Bevel),
            _ => None,
        }
    }
}

/// Fill rule for path filling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillRule {
    NonZeroWinding,
    EvenOdd,
}

/// Device color in RGB (internal representation) with optional native CMYK.
#[derive(Clone, Debug)]
pub struct DeviceColor {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    /// Native CMYK components for lossless roundtrip when color space is CMYK.
    pub native_cmyk: Option<(f64, f64, f64, f64)>,
}

impl DeviceColor {
    pub fn from_gray(gray: f64) -> Self {
        Self {
            r: gray,
            g: gray,
            b: gray,
            native_cmyk: None,
        }
    }

    pub fn from_rgb(r: f64, g: f64, b: f64) -> Self {
        Self {
            r,
            g,
            b,
            native_cmyk: None,
        }
    }

    pub fn from_cmyk(c: f64, m: f64, y: f64, k: f64) -> Self {
        Self {
            r: 1.0 - (c + k).min(1.0),
            g: 1.0 - (m + k).min(1.0),
            b: 1.0 - (y + k).min(1.0),
            native_cmyk: Some((c, m, y, k)),
        }
    }

    /// Create from CMYK, converting through ICC profile if available.
    /// Falls back to PLRM formula when ICC is unavailable.
    pub fn from_cmyk_icc(c: f64, m: f64, y: f64, k: f64, icc: &mut crate::icc::IccCache) -> Self {
        if let Some((r, g, b)) = icc.convert_cmyk(c, m, y, k) {
            Self {
                r,
                g,
                b,
                native_cmyk: Some((c, m, y, k)),
            }
        } else {
            Self::from_cmyk(c, m, y, k)
        }
    }

    pub fn from_hsb(h: f64, s: f64, b: f64) -> Self {
        if s == 0.0 {
            return Self::from_gray(b);
        }
        if b == 0.0 {
            return Self::from_gray(0.0);
        }

        let mut hue = h * 6.0;
        if hue >= 6.0 {
            hue = 0.0;
        }

        let sector = hue as i32;
        let frac = hue - sector as f64;

        let p = b * (1.0 - s);
        let q = b * (1.0 - s * frac);
        let t = b * (1.0 - s * (1.0 - frac));

        let (r, g, bl) = match sector {
            0 => (b, t, p),
            1 => (q, b, p),
            2 => (p, b, t),
            3 => (p, q, b),
            4 => (t, p, b),
            _ => (b, p, q),
        };

        Self {
            r,
            g,
            b: bl,
            native_cmyk: None,
        }
    }

    /// Convert to gray using NTSC luma.
    pub fn to_gray(&self) -> f64 {
        0.3 * self.r + 0.59 * self.g + 0.11 * self.b
    }

    /// Convert to CMYK (uses native CMYK if available for lossless roundtrip).
    pub fn to_cmyk(&self) -> (f64, f64, f64, f64) {
        if let Some(cmyk) = self.native_cmyk {
            return cmyk;
        }
        let c = 1.0 - self.r;
        let m = 1.0 - self.g;
        let y = 1.0 - self.b;
        let k = c.min(m).min(y);
        (
            (c - k).clamp(0.0, 1.0),
            (m - k).clamp(0.0, 1.0),
            (y - k).clamp(0.0, 1.0),
            k.clamp(0.0, 1.0),
        )
    }

    /// Convert to HSB.
    pub fn to_hsb(&self) -> (f64, f64, f64) {
        let max_val = self.r.max(self.g).max(self.b);
        let min_val = self.r.min(self.g).min(self.b);
        let diff = max_val - min_val;

        let brightness = max_val;
        let saturation = if max_val == 0.0 { 0.0 } else { diff / max_val };

        let hue = if diff == 0.0 {
            0.0
        } else if max_val == self.r {
            let mut h = (self.g - self.b) / diff;
            if h < 0.0 {
                h += 6.0;
            }
            h / 6.0
        } else if max_val == self.g {
            ((self.b - self.r) / diff + 2.0) / 6.0
        } else {
            ((self.r - self.g) / diff + 4.0) / 6.0
        };

        (hue, saturation, brightness)
    }

    /// Black (default color).
    pub fn black() -> Self {
        Self {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            native_cmyk: None,
        }
    }

    /// Apply sRGB gamma companding (linear → gamma-corrected).
    fn srgb_gamma(u: f64) -> f64 {
        if u <= 0.0031308 {
            12.92 * u
        } else {
            1.055 * u.powf(1.0 / 2.4) - 0.055
        }
    }

    /// Convert CIE XYZ to sRGB.
    fn from_xyz(x: f64, y: f64, z: f64) -> Self {
        // IEC 61966-2-1 sRGB D65 XYZ → linear RGB matrix
        let lr = 3.2404542 * x + (-1.5371385) * y + (-0.4985314) * z;
        let lg = (-0.9692660) * x + 1.8760108 * y + 0.0415560 * z;
        let lb = 0.0556434 * x + (-0.2040259) * y + 1.0572252 * z;

        Self {
            r: Self::srgb_gamma(lr.max(0.0)).clamp(0.0, 1.0),
            g: Self::srgb_gamma(lg.max(0.0)).clamp(0.0, 1.0),
            b: Self::srgb_gamma(lb.max(0.0)).clamp(0.0, 1.0),
            native_cmyk: None,
        }
    }

    /// Apply a column-major 3×3 matrix to a 3-element vector.
    fn apply_matrix_3x3(mat: &[f64; 9], v: &[f64; 3]) -> [f64; 3] {
        [
            mat[0] * v[0] + mat[3] * v[1] + mat[6] * v[2],
            mat[1] * v[0] + mat[4] * v[1] + mat[7] * v[2],
            mat[2] * v[0] + mat[5] * v[1] + mat[8] * v[2],
        ]
    }

    /// Linear interpolation in a pre-evaluated decode table.
    /// Table has N entries mapping input [0,1] to output values.
    fn decode_lookup(table: &[f64], value: f64) -> f64 {
        let n = table.len();
        if n < 2 {
            return table.first().copied().unwrap_or(value);
        }
        let idx = value * (n - 1) as f64;
        let i0 = (idx as usize).min(n - 2);
        let frac = idx - i0 as f64;
        table[i0] + (table[i0 + 1] - table[i0]) * frac
    }

    /// Convert CIEBasedABC color to sRGB.
    ///
    /// Pipeline: input → RangeABC clamp → DecodeABC → MatrixABC → RangeLMN clamp → DecodeLMN → MatrixLMN → XYZ → sRGB
    pub fn from_cie_abc(a: f64, b: f64, c: f64, params: &CieAbcParams) -> Self {
        // RangeABC clamp
        let mut a = a.clamp(params.range_abc[0], params.range_abc[1]);
        let mut b = b.clamp(params.range_abc[2], params.range_abc[3]);
        let mut c = c.clamp(params.range_abc[4], params.range_abc[5]);

        // DecodeABC (pre-evaluated lookup tables)
        if let Some(ref tables) = params.decode_abc {
            // Normalize to [0,1] for table lookup
            let ra = params.range_abc[1] - params.range_abc[0];
            let rb = params.range_abc[3] - params.range_abc[2];
            let rc = params.range_abc[5] - params.range_abc[4];
            let na = if ra > 0.0 {
                (a - params.range_abc[0]) / ra
            } else {
                0.0
            };
            let nb = if rb > 0.0 {
                (b - params.range_abc[2]) / rb
            } else {
                0.0
            };
            let nc = if rc > 0.0 {
                (c - params.range_abc[4]) / rc
            } else {
                0.0
            };
            a = Self::decode_lookup(&tables[0], na);
            b = Self::decode_lookup(&tables[1], nb);
            c = Self::decode_lookup(&tables[2], nc);
        }

        // MatrixABC (column-major 3×3)
        let lmn = Self::apply_matrix_3x3(&params.matrix_abc, &[a, b, c]);

        // RangeLMN clamp
        let mut l = lmn[0].clamp(params.range_lmn[0], params.range_lmn[1]);
        let mut m = lmn[1].clamp(params.range_lmn[2], params.range_lmn[3]);
        let mut n = lmn[2].clamp(params.range_lmn[4], params.range_lmn[5]);

        // DecodeLMN (pre-evaluated lookup tables)
        if let Some(ref tables) = params.decode_lmn {
            let rl = params.range_lmn[1] - params.range_lmn[0];
            let rm = params.range_lmn[3] - params.range_lmn[2];
            let rn = params.range_lmn[5] - params.range_lmn[4];
            let nl = if rl > 0.0 {
                (l - params.range_lmn[0]) / rl
            } else {
                0.0
            };
            let nm = if rm > 0.0 {
                (m - params.range_lmn[2]) / rm
            } else {
                0.0
            };
            let nn = if rn > 0.0 {
                (n - params.range_lmn[4]) / rn
            } else {
                0.0
            };
            l = Self::decode_lookup(&tables[0], nl);
            m = Self::decode_lookup(&tables[1], nm);
            n = Self::decode_lookup(&tables[2], nn);
        }

        // MatrixLMN → XYZ
        let xyz = Self::apply_matrix_3x3(&params.matrix_lmn, &[l, m, n]);

        Self::from_xyz(xyz[0], xyz[1], xyz[2])
    }

    /// Convert CIEBasedA color to sRGB.
    ///
    /// Pipeline: input → RangeA clamp → DecodeA → MatrixA → RangeLMN clamp → DecodeLMN → MatrixLMN → XYZ → sRGB
    pub fn from_cie_a(a: f64, params: &CieAParams) -> Self {
        // RangeA clamp
        let mut a = a.clamp(params.range_a[0], params.range_a[1]);

        // DecodeA (pre-evaluated lookup table)
        if let Some(ref table) = params.decode_a {
            let ra = params.range_a[1] - params.range_a[0];
            let na = if ra > 0.0 {
                (a - params.range_a[0]) / ra
            } else {
                0.0
            };
            a = Self::decode_lookup(table, na);
        }

        // MatrixA: 3-element vector multiplied by scalar A
        let lmn = [
            params.matrix_a[0] * a,
            params.matrix_a[1] * a,
            params.matrix_a[2] * a,
        ];

        // RangeLMN clamp
        let mut l = lmn[0].clamp(params.range_lmn[0], params.range_lmn[1]);
        let mut m = lmn[1].clamp(params.range_lmn[2], params.range_lmn[3]);
        let mut n = lmn[2].clamp(params.range_lmn[4], params.range_lmn[5]);

        // DecodeLMN (pre-evaluated lookup tables)
        if let Some(ref tables) = params.decode_lmn {
            let rl = params.range_lmn[1] - params.range_lmn[0];
            let rm = params.range_lmn[3] - params.range_lmn[2];
            let rn = params.range_lmn[5] - params.range_lmn[4];
            let nl = if rl > 0.0 {
                (l - params.range_lmn[0]) / rl
            } else {
                0.0
            };
            let nm = if rm > 0.0 {
                (m - params.range_lmn[2]) / rm
            } else {
                0.0
            };
            let nn = if rn > 0.0 {
                (n - params.range_lmn[4]) / rn
            } else {
                0.0
            };
            l = Self::decode_lookup(&tables[0], nl);
            m = Self::decode_lookup(&tables[1], nm);
            n = Self::decode_lookup(&tables[2], nn);
        }

        // MatrixLMN → XYZ
        let xyz = Self::apply_matrix_3x3(&params.matrix_lmn, &[l, m, n]);

        Self::from_xyz(xyz[0], xyz[1], xyz[2])
    }

    /// Convert CIEBasedDEF color to sRGB via pre-converted trilinear interpolation table.
    pub fn from_cie_def(d: f64, e: f64, f: f64, params: &CieDefParams) -> Self {
        let (m1, m2, m3) = (params.m1, params.m2, params.m3);
        if m1 < 2 || m2 < 2 || m3 < 2 {
            return Self::from_gray(0.0);
        }

        // Normalize DEF to [0, m-1] indices using RangeDEF
        let d_range = params.range_def[1] - params.range_def[0];
        let e_range = params.range_def[3] - params.range_def[2];
        let f_range = params.range_def[5] - params.range_def[4];

        let di = if d_range > 0.0 {
            ((d - params.range_def[0]) / d_range * (m1 - 1) as f64).clamp(0.0, (m1 - 1) as f64)
        } else {
            0.0
        };
        let ei = if e_range > 0.0 {
            ((e - params.range_def[2]) / e_range * (m2 - 1) as f64).clamp(0.0, (m2 - 1) as f64)
        } else {
            0.0
        };
        let fi = if f_range > 0.0 {
            ((f - params.range_def[4]) / f_range * (m3 - 1) as f64).clamp(0.0, (m3 - 1) as f64)
        } else {
            0.0
        };

        // Trilinear interpolation in ABC space
        let di0 = (di as usize).min(m1 - 2);
        let ei0 = (ei as usize).min(m2 - 2);
        let fi0 = (fi as usize).min(m3 - 2);
        let di1 = di0 + 1;
        let ei1 = ei0 + 1;
        let fi1 = fi0 + 1;
        let dd = di - di0 as f64;
        let de = ei - ei0 as f64;
        let df = fi - fi0 as f64;

        let stride_e = m3;
        let stride_d = m2 * m3;

        let mut abc = [0.0f64; 3];
        for (ch, table) in [&params.a_table, &params.b_table, &params.c_table]
            .iter()
            .enumerate()
        {
            let c000 = table[di0 * stride_d + ei0 * stride_e + fi0];
            let c001 = table[di0 * stride_d + ei0 * stride_e + fi1];
            let c010 = table[di0 * stride_d + ei1 * stride_e + fi0];
            let c011 = table[di0 * stride_d + ei1 * stride_e + fi1];
            let c100 = table[di1 * stride_d + ei0 * stride_e + fi0];
            let c101 = table[di1 * stride_d + ei0 * stride_e + fi1];
            let c110 = table[di1 * stride_d + ei1 * stride_e + fi0];
            let c111 = table[di1 * stride_d + ei1 * stride_e + fi1];

            let c00 = c000 * (1.0 - df) + c001 * df;
            let c01 = c010 * (1.0 - df) + c011 * df;
            let c10 = c100 * (1.0 - df) + c101 * df;
            let c11 = c110 * (1.0 - df) + c111 * df;

            let c0 = c00 * (1.0 - de) + c01 * de;
            let c1 = c10 * (1.0 - de) + c11 * de;

            abc[ch] = c0 * (1.0 - dd) + c1 * dd;
        }

        // Convert interpolated ABC through the CIE pipeline
        Self::from_cie_abc(abc[0], abc[1], abc[2], &params.abc_params)
    }

    /// Convert CIEBasedDEFG color to sRGB via pre-converted nearest-neighbor 4D table.
    pub fn from_cie_defg(d: f64, e: f64, f: f64, g: f64, params: &CieDefgParams) -> Self {
        let (m1, m2, m3, m4) = (params.m1, params.m2, params.m3, params.m4);
        if m1 == 0 || m2 == 0 || m3 == 0 || m4 == 0 {
            return Self::from_gray(0.0);
        }

        // Normalize DEFG to [0, m-1] and round to nearest
        let d_range = params.range_defg[1] - params.range_defg[0];
        let e_range = params.range_defg[3] - params.range_defg[2];
        let f_range = params.range_defg[5] - params.range_defg[4];
        let g_range = params.range_defg[7] - params.range_defg[6];

        let di = if d_range > 0.0 {
            ((d - params.range_defg[0]) / d_range * (m1 - 1) as f64 + 0.5) as usize
        } else {
            0
        }
        .min(m1 - 1);
        let ei = if e_range > 0.0 {
            ((e - params.range_defg[2]) / e_range * (m2 - 1) as f64 + 0.5) as usize
        } else {
            0
        }
        .min(m2 - 1);
        let fi = if f_range > 0.0 {
            ((f - params.range_defg[4]) / f_range * (m3 - 1) as f64 + 0.5) as usize
        } else {
            0
        }
        .min(m3 - 1);
        let gi = if g_range > 0.0 {
            ((g - params.range_defg[6]) / g_range * (m4 - 1) as f64 + 0.5) as usize
        } else {
            0
        }
        .min(m4 - 1);

        let idx = di * m2 * m3 * m4 + ei * m3 * m4 + fi * m4 + gi;
        if idx >= params.a_table.len() {
            return Self::from_gray(0.0);
        }

        // Look up ABC values and convert through CIE pipeline
        Self::from_cie_abc(
            params.a_table[idx],
            params.b_table[idx],
            params.c_table[idx],
            &params.abc_params,
        )
    }
}

/// Color space identifier.
#[derive(Clone, Debug)]
pub enum ColorSpace {
    DeviceGray,
    DeviceRGB,
    DeviceCMYK,
    /// Indexed color space: `[/Indexed base hival lookup]`.
    /// `lookup_proc` is `Some(proc_object)` when the lookup is a procedure that
    /// needs to be pre-evaluated via exec_sync during setcolorspace.
    Indexed {
        base: Box<ColorSpace>,
        hival: u32,
        lookup: Vec<u8>,
        lookup_proc: Option<PsObject>,
    },
    /// CIE-based ABC color space (3 components): `[/CIEBasedABC dict]`.
    CIEBasedABC {
        params: Arc<CieAbcParams>,
        dict_entity: crate::object::EntityId,
    },
    /// CIE-based A color space (1 component): `[/CIEBasedA dict]`.
    CIEBasedA {
        params: Arc<CieAParams>,
        dict_entity: crate::object::EntityId,
    },
    /// CIE-based DEF color space (3 components → 3D table → ABC → sRGB).
    CIEBasedDEF {
        params: Arc<CieDefParams>,
        dict_entity: crate::object::EntityId,
    },
    /// CIE-based DEFG color space (4 components → 4D table → ABC → sRGB).
    CIEBasedDEFG {
        params: Arc<CieDefgParams>,
        dict_entity: crate::object::EntityId,
    },
    /// ICC-based color space: `[/ICCBased dict]` where dict has /N components.
    /// When `profile_hash` is Some, colors are converted through the ICC profile.
    /// Falls back to device space based on N (1=Gray, 3=RGB, 4=CMYK).
    ICCBased {
        dict_entity: crate::object::EntityId,
        n: u32,
        profile_hash: Option<crate::icc::ProfileHash>,
    },
    /// Separation color space: `[/Separation name alternativeSpace tintTransform]`.
    /// Single tint component mapped to alternative space via tint transform procedure.
    Separation {
        name: Vec<u8>,
        alt_space: Box<ColorSpace>,
        tint_transform: crate::object::PsObject,
        num_alt_components: u32,
    },
    /// DeviceN color space: `[/DeviceN names alternativeSpace tintTransform]`.
    /// N tint components mapped to alternative space via tint transform procedure.
    DeviceN {
        names: Vec<Vec<u8>>,
        num_colorants: u32,
        alt_space: Box<ColorSpace>,
        tint_transform: crate::object::PsObject,
        num_alt_components: u32,
    },
}

impl PartialEq for ColorSpace {
    fn eq(&self, other: &Self) -> bool {
        use ColorSpace::*;
        match (self, other) {
            (DeviceGray, DeviceGray) | (DeviceRGB, DeviceRGB) | (DeviceCMYK, DeviceCMYK) => true,
            (
                Indexed {
                    base: b1,
                    hival: h1,
                    lookup: l1,
                    ..
                },
                Indexed {
                    base: b2,
                    hival: h2,
                    lookup: l2,
                    ..
                },
            ) => b1 == b2 && h1 == h2 && l1 == l2,
            (
                CIEBasedABC {
                    dict_entity: d1, ..
                },
                CIEBasedABC {
                    dict_entity: d2, ..
                },
            ) => d1 == d2,
            (
                CIEBasedA {
                    dict_entity: d1, ..
                },
                CIEBasedA {
                    dict_entity: d2, ..
                },
            ) => d1 == d2,
            (
                CIEBasedDEF {
                    dict_entity: d1, ..
                },
                CIEBasedDEF {
                    dict_entity: d2, ..
                },
            ) => d1 == d2,
            (
                CIEBasedDEFG {
                    dict_entity: d1, ..
                },
                CIEBasedDEFG {
                    dict_entity: d2, ..
                },
            ) => d1 == d2,
            (
                ICCBased {
                    dict_entity: d1,
                    n: n1,
                    ..
                },
                ICCBased {
                    dict_entity: d2,
                    n: n2,
                    ..
                },
            ) => d1 == d2 && n1 == n2,
            (
                Separation {
                    name: name1,
                    alt_space: a1,
                    tint_transform: t1,
                    num_alt_components: n1,
                },
                Separation {
                    name: name2,
                    alt_space: a2,
                    tint_transform: t2,
                    num_alt_components: n2,
                },
            ) => name1 == name2 && a1 == a2 && t1 == t2 && n1 == n2,
            (
                DeviceN {
                    names: names1,
                    num_colorants: nc1,
                    alt_space: a1,
                    tint_transform: t1,
                    num_alt_components: n1,
                },
                DeviceN {
                    names: names2,
                    num_colorants: nc2,
                    alt_space: a2,
                    tint_transform: t2,
                    num_alt_components: n2,
                },
            ) => names1 == names2 && nc1 == nc2 && a1 == a2 && t1 == t2 && n1 == n2,
            _ => false,
        }
    }
}

/// Extracted parameters for CIEBasedABC color conversion.
#[derive(Clone, Debug)]
pub struct CieAbcParams {
    pub range_abc: [f64; 6], // [min_a, max_a, min_b, max_b, min_c, max_c]
    pub decode_abc: Option<[Vec<f64>; 3]>, // 256-point pre-evaluated tables
    pub matrix_abc: [f64; 9], // Column-major 3×3 (default identity)
    pub range_lmn: [f64; 6], // [min_l, max_l, min_m, max_m, min_n, max_n]
    pub decode_lmn: Option<[Vec<f64>; 3]>, // 256-point pre-evaluated tables
    pub matrix_lmn: [f64; 9], // Column-major 3×3 (default identity)
    pub white_point: [f64; 3], // CIE WhitePoint [Xw, Yw, Zw] (default D65)
}

impl Default for CieAbcParams {
    fn default() -> Self {
        Self {
            range_abc: [0.0, 1.0, 0.0, 1.0, 0.0, 1.0],
            decode_abc: None,
            matrix_abc: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            range_lmn: [0.0, 1.0, 0.0, 1.0, 0.0, 1.0],
            decode_lmn: None,
            matrix_lmn: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            white_point: [0.9505, 1.0, 1.089], // D65
        }
    }
}

/// Extracted parameters for CIEBasedA color conversion.
#[derive(Clone, Debug)]
pub struct CieAParams {
    pub range_a: [f64; 2],                 // [min, max]
    pub decode_a: Option<Vec<f64>>,        // 256-point pre-evaluated table
    pub matrix_a: [f64; 3],                // 3-element vector (default [1,1,1])
    pub range_lmn: [f64; 6],               // [min_l, max_l, min_m, max_m, min_n, max_n]
    pub decode_lmn: Option<[Vec<f64>; 3]>, // 256-point pre-evaluated tables
    pub matrix_lmn: [f64; 9],              // Column-major 3×3 (default identity)
    pub white_point: [f64; 3],             // CIE WhitePoint [Xw, Yw, Zw] (default D65)
}

impl Default for CieAParams {
    fn default() -> Self {
        Self {
            range_a: [0.0, 1.0],
            decode_a: None,
            matrix_a: [1.0, 1.0, 1.0],
            range_lmn: [0.0, 1.0, 0.0, 1.0, 0.0, 1.0],
            decode_lmn: None,
            matrix_lmn: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            white_point: [0.9505, 1.0, 1.089], // D65
        }
    }
}

/// Pre-converted parameters for CIEBasedDEF color space (3D table → RGB).
#[derive(Clone, Debug)]
pub struct CieDefParams {
    pub range_def: [f64; 6],      // [min_d, max_d, min_e, max_e, min_f, max_f]
    pub m1: usize,                // table dimension 1
    pub m2: usize,                // table dimension 2
    pub m3: usize,                // table dimension 3
    pub a_table: Vec<f64>,        // ABC-space A values (m1*m2*m3)
    pub b_table: Vec<f64>,        // ABC-space B values (m1*m2*m3)
    pub c_table: Vec<f64>,        // ABC-space C values (m1*m2*m3)
    pub abc_params: CieAbcParams, // CIE ABC pipeline params (with pre-evaluated decode tables)
}

/// Parameters for CIEBasedDEFG color space (4D table → ABC → CIE pipeline).
#[derive(Clone, Debug)]
pub struct CieDefgParams {
    pub range_defg: [f64; 8], // [min_d, max_d, min_e, max_e, min_f, max_f, min_g, max_g]
    pub m1: usize,
    pub m2: usize,
    pub m3: usize,
    pub m4: usize,
    pub a_table: Vec<f64>,        // ABC-space A values (m1*m2*m3*m4)
    pub b_table: Vec<f64>,        // ABC-space B values (m1*m2*m3*m4)
    pub c_table: Vec<f64>,        // ABC-space C values (m1*m2*m3*m4)
    pub abc_params: CieAbcParams, // CIE ABC pipeline params
}

/// Pattern instance data created by `makepattern`.
#[derive(Clone)]
pub struct PatternData {
    /// Pattern type: 1 = tiling, 2 = shading.
    pub pattern_type: i32,
    /// Paint type: 1 = colored, 2 = uncolored.
    pub paint_type: i32,
    /// Tiling type: 1 = constant spacing, 2 = no distortion, 3 = fast.
    pub tiling_type: i32,
    /// Bounding box [llx, lly, urx, ury] in pattern space.
    pub bbox: [f64; 4],
    /// X step between tile origins.
    pub xstep: f64,
    /// Y step between tile origins.
    pub ystep: f64,
    /// Combined matrix: matrix_arg × CTM at makepattern time.
    pub pattern_matrix: Matrix,
    /// Pre-rendered display list from executing PaintProc.
    pub cached_display_list: DisplayList,
}

/// Dash pattern for stroked paths.
#[derive(Clone, Debug)]
pub struct DashPattern {
    pub array: Vec<f64>,
    pub offset: f64,
}

impl DashPattern {
    pub fn solid() -> Self {
        Self {
            array: Vec::new(),
            offset: 0.0,
        }
    }
}

impl Default for DashPattern {
    fn default() -> Self {
        Self::solid()
    }
}

/// Entry on the graphics state stack, tracking whether it was created by
/// `save` (implicit gsave) or `gsave`.
#[derive(Clone, Debug)]
pub struct GstateEntry {
    pub state: GraphicsState,
    /// True if created by `save`, false if by `gsave`.
    /// `grestore` skips save-created entries; `grestoreall` stops at them.
    pub saved_by_save: bool,
}

/// Complete graphics state (cloned for gsave/grestore).
#[derive(Clone, Debug)]
pub struct GraphicsState {
    pub ctm: Matrix,
    pub color: DeviceColor,
    pub color_space: ColorSpace,
    pub path: PsPath,
    pub current_point: Option<(f64, f64)>,
    pub clip_path: Option<PsPath>,
    pub clip_path_version: u32,
    pub line_width: f64,
    pub line_cap: LineCap,
    pub line_join: LineJoin,
    pub miter_limit: f64,
    pub dash_pattern: DashPattern,
    pub flatness: f64,
    pub stroke_adjust: bool,
    pub overprint: bool,
    pub smoothness: f64,
    pub default_ctm: Matrix,

    // Clip save/restore stack (per graphics state)
    pub clip_stack: Vec<Option<PsPath>>,

    // Current font (set by setfont, used by show operators)
    pub current_font: Option<crate::object::PsObject>,

    // Root font for composite font hierarchy (set during Type 0 rendering).
    // rootfont returns this if set, otherwise falls back to current_font.
    pub root_font: Option<crate::object::PsObject>,

    // Page device dict (EntityId into DictStore)
    pub page_device: Option<crate::object::EntityId>,

    // Halftone screen parameters (set by setscreen/setcolorscreen/sethalftone)
    pub screen_freq: f64,
    pub screen_angle: f64,
    pub screen_proc: Option<crate::object::PsObject>,
    /// Per-component color screen: [red, green, blue, gray] × (freq, angle, proc)
    pub color_screen: Option<[(f64, f64, crate::object::PsObject); 4]>,
    /// Halftone dictionary (set by sethalftone)
    pub halftone: Option<crate::object::PsObject>,

    // Transfer functions
    pub transfer_function: Option<crate::object::PsObject>,
    /// Per-component transfer: [red, green, blue, gray]
    pub color_transfer: Option<[crate::object::PsObject; 4]>,
    /// Pre-sampled transfer function (256 entries). None = identity.
    pub sampled_transfer: Option<Arc<Vec<f64>>>,
    /// Pre-sampled per-component transfer \[R, G, B, Gray\].
    pub sampled_color_transfer: Option<[Option<Arc<Vec<f64>>>; 4]>,
    /// Pre-computed halftone screen for PDF output. None = default (suppressed).
    pub precomputed_halftone: Option<Arc<crate::device::HalftoneScreen>>,
    /// Pre-computed per-component halftone \[R, G, B, Gray\] (from setcolorscreen).
    pub precomputed_color_halftone: Option<[Option<Arc<crate::device::HalftoneScreen>>; 4]>,

    // Black generation / undercolor removal
    pub black_generation: Option<crate::object::PsObject>,
    pub undercolor_removal: Option<crate::object::PsObject>,
    /// Pre-sampled black generation function (256 entries, domain [0,1] → range [0,1]).
    pub sampled_black_generation: Option<Arc<Vec<f64>>>,
    /// Pre-sampled undercolor removal function (256 entries, domain [0,1] → range [-1,1]).
    pub sampled_ucr: Option<Arc<Vec<f64>>>,

    // Color rendering dictionary
    pub color_rendering: Option<crate::object::PsObject>,

    /// Rendering intent: 0=RelativeColorimetric, 1=AbsoluteColorimetric,
    /// 2=Perceptual, 3=Saturation. Default is RelativeColorimetric.
    pub rendering_intent: u8,

    // Pattern state (set by setpattern, consumed by fill/eofill)
    /// Index into `Context.pattern_store` for the active tiling pattern.
    pub current_pattern: Option<u32>,
    /// Underlying color for uncolored (PaintType 2) patterns.
    pub pattern_underlying_color: Option<DeviceColor>,

    // Userpath bounding box (set by setbbox, cleared by newpath)
    pub bbox: Option<[f64; 4]>,

    /// Tint values from the most recent setcolor (for Separation/DeviceN).
    /// 1 value for Separation, N values for DeviceN. None for device color spaces.
    pub tint_values: Option<Vec<f64>>,

    /// Cached pre-sampled tint lookup table for the current Separation/DeviceN color space.
    /// Set when setcolorspace installs a Separation/DeviceN space.
    pub cached_tint_table: Option<Arc<crate::device::TintLookupTable>>,
}

impl GraphicsState {
    /// Create default graphics state (PostScript initial state).
    pub fn new() -> Self {
        Self {
            ctm: Matrix::identity(),
            color: DeviceColor::black(),
            color_space: ColorSpace::DeviceGray,
            path: PsPath::new(),
            current_point: None,
            clip_path: None,
            clip_path_version: 0,
            line_width: 1.0,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            miter_limit: 10.0,
            dash_pattern: DashPattern::solid(),
            flatness: 1.0,
            stroke_adjust: false,
            overprint: false,
            smoothness: 1.0,
            default_ctm: Matrix::identity(),
            clip_stack: Vec::new(),
            current_font: None,
            root_font: None,
            page_device: None,
            screen_freq: 60.0,
            screen_angle: 45.0,
            screen_proc: None,
            color_screen: None,
            halftone: None,
            transfer_function: None,
            color_transfer: None,
            sampled_transfer: None,
            sampled_color_transfer: None,
            precomputed_halftone: None,
            precomputed_color_halftone: None,
            black_generation: None,
            undercolor_removal: None,
            sampled_black_generation: None,
            sampled_ucr: None,
            color_rendering: None,
            rendering_intent: 0, // RelativeColorimetric
            current_pattern: None,
            pattern_underlying_color: None,
            bbox: None,
            tint_values: None,
            cached_tint_table: None,
        }
    }
}

impl Default for GraphicsState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_matrix_identity() {
        let m = Matrix::identity();
        let (x, y) = m.transform_point(3.0, 4.0);
        assert!((x - 3.0).abs() < 1e-10);
        assert!((y - 4.0).abs() < 1e-10);
    }

    #[test]
    fn test_matrix_translate() {
        let m = Matrix::translate(10.0, 20.0);
        let (x, y) = m.transform_point(3.0, 4.0);
        assert!((x - 13.0).abs() < 1e-10);
        assert!((y - 24.0).abs() < 1e-10);
    }

    #[test]
    fn test_matrix_scale() {
        let m = Matrix::scale(2.0, 3.0);
        let (x, y) = m.transform_point(5.0, 7.0);
        assert!((x - 10.0).abs() < 1e-10);
        assert!((y - 21.0).abs() < 1e-10);
    }

    #[test]
    fn test_matrix_rotate_90() {
        let m = Matrix::rotate(90.0);
        let (x, y) = m.transform_point(1.0, 0.0);
        assert!(x.abs() < 1e-10);
        assert!((y - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_matrix_multiply() {
        let t = Matrix::translate(10.0, 0.0);
        let s = Matrix::scale(2.0, 2.0);
        // scale then translate: t * s
        let m = t.multiply(&s);
        let (x, y) = m.transform_point(5.0, 3.0);
        assert!((x - 20.0).abs() < 1e-10);
        assert!((y - 6.0).abs() < 1e-10);
    }

    #[test]
    fn test_matrix_concat() {
        // PostScript concat: CTM = other * CTM
        let ctm = Matrix::identity();
        let t = Matrix::translate(10.0, 20.0);
        let result = ctm.concat(&t);
        let (x, y) = result.transform_point(0.0, 0.0);
        assert!((x - 10.0).abs() < 1e-10);
        assert!((y - 20.0).abs() < 1e-10);
    }

    #[test]
    fn test_matrix_invert() {
        let m = Matrix::new(2.0, 0.0, 0.0, 3.0, 10.0, 20.0);
        let inv = m.invert().unwrap();
        let (x, y) = m.transform_point(5.0, 7.0);
        let (x2, y2) = inv.transform_point(x, y);
        // Tolerance allows for rounding to 10 decimal places in each step
        assert!((x2 - 5.0).abs() < 1e-8);
        assert!((y2 - 7.0).abs() < 1e-8);
    }

    #[test]
    fn test_matrix_invert_singular() {
        let m = Matrix::new(0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        assert!(m.invert().is_none());
    }

    #[test]
    fn test_matrix_transform_delta() {
        let m = Matrix::translate(100.0, 200.0);
        let (dx, dy) = m.transform_delta(5.0, 3.0);
        assert!((dx - 5.0).abs() < 1e-10);
        assert!((dy - 3.0).abs() < 1e-10);
    }

    #[test]
    fn test_color_from_gray() {
        let c = DeviceColor::from_gray(0.5);
        assert!((c.r - 0.5).abs() < 1e-10);
        assert!((c.g - 0.5).abs() < 1e-10);
        assert!((c.b - 0.5).abs() < 1e-10);
    }

    #[test]
    fn test_color_from_cmyk() {
        // Pure cyan: c=1, m=0, y=0, k=0 → r=0, g=1, b=1
        let c = DeviceColor::from_cmyk(1.0, 0.0, 0.0, 0.0);
        assert!((c.r - 0.0).abs() < 1e-10);
        assert!((c.g - 1.0).abs() < 1e-10);
        assert!((c.b - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_color_from_hsb() {
        // Pure red: h=0, s=1, b=1
        let c = DeviceColor::from_hsb(0.0, 1.0, 1.0);
        assert!((c.r - 1.0).abs() < 1e-10);
        assert!((c.g - 0.0).abs() < 1e-10);
        assert!((c.b - 0.0).abs() < 1e-10);

        // Pure green: h=1/3, s=1, b=1
        let c = DeviceColor::from_hsb(1.0 / 3.0, 1.0, 1.0);
        assert!((c.r - 0.0).abs() < 1e-10);
        assert!((c.g - 1.0).abs() < 1e-10);
        assert!((c.b - 0.0).abs() < 1e-10);
    }

    #[test]
    fn test_color_gray_roundtrip() {
        let c = DeviceColor::from_gray(0.5);
        let gray = c.to_gray();
        assert!((gray - 0.5).abs() < 1e-10);
    }

    #[test]
    fn test_color_hsb_roundtrip() {
        let c = DeviceColor::from_hsb(0.6, 0.8, 0.9);
        let (h, s, b) = c.to_hsb();
        assert!((h - 0.6).abs() < 0.01);
        assert!((s - 0.8).abs() < 0.01);
        assert!((b - 0.9).abs() < 0.01);
    }

    #[test]
    fn test_linecap_from_i32() {
        assert_eq!(LineCap::from_i32(0), Some(LineCap::Butt));
        assert_eq!(LineCap::from_i32(1), Some(LineCap::Round));
        assert_eq!(LineCap::from_i32(2), Some(LineCap::Square));
        assert_eq!(LineCap::from_i32(3), None);
    }

    #[test]
    fn test_linejoin_from_i32() {
        assert_eq!(LineJoin::from_i32(0), Some(LineJoin::Miter));
        assert_eq!(LineJoin::from_i32(1), Some(LineJoin::Round));
        assert_eq!(LineJoin::from_i32(2), Some(LineJoin::Bevel));
        assert_eq!(LineJoin::from_i32(3), None);
    }

    #[test]
    fn test_default_graphics_state() {
        let gs = GraphicsState::new();
        assert_eq!(gs.line_width, 1.0);
        assert_eq!(gs.line_cap, LineCap::Butt);
        assert_eq!(gs.line_join, LineJoin::Miter);
        assert_eq!(gs.miter_limit, 10.0);
        assert!(gs.path.is_empty());
        assert!(gs.current_point.is_none());
        assert!(gs.clip_path.is_none());
        assert_eq!(gs.flatness, 1.0);
        assert!(!gs.stroke_adjust);
    }
}
