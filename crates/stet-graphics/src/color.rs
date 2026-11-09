// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Color types and CIE color space parameters.

/// Line cap style.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum LineCap {
    #[default]
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum LineJoin {
    #[default]
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum FillRule {
    #[default]
    NonZeroWinding,
    EvenOdd,
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

/// Device color in RGB (internal representation) with optional native CMYK.
#[derive(Clone, Debug)]
pub struct DeviceColor {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    /// Native CMYK components for lossless roundtrip when color space is CMYK.
    /// For DeviceN/Separation paints this is the *full* alt-CMYK tint transform
    /// (process + spot contributions combined).
    pub native_cmyk: Option<(f64, f64, f64, f64)>,
    /// Process-colorant-only CMYK contribution. Populated by DeviceN/Separation
    /// paints so the overprint tracker can record only what actually lands on
    /// process plates (C/M/Y/K), leaving any spot colorant's alt-CMYK out of
    /// the process buffer. `None` means "use `native_cmyk` as the process
    /// contribution" (pure DeviceCMYK / DeviceGray / DeviceRGB).
    pub process_cmyk: Option<(f64, f64, f64, f64)>,
}

impl DeviceColor {
    pub fn from_gray(gray: f64) -> Self {
        Self {
            r: gray,
            g: gray,
            b: gray,
            native_cmyk: None,
            process_cmyk: None,
        }
    }

    pub fn from_rgb(r: f64, g: f64, b: f64) -> Self {
        Self {
            r,
            g,
            b,
            native_cmyk: None,
            process_cmyk: None,
        }
    }

    pub fn from_cmyk(c: f64, m: f64, y: f64, k: f64) -> Self {
        Self {
            r: 1.0 - (c + k).min(1.0),
            g: 1.0 - (m + k).min(1.0),
            b: 1.0 - (y + k).min(1.0),
            native_cmyk: Some((c, m, y, k)),
            process_cmyk: None,
        }
    }

    /// Create from CMYK, converting through ICC profile if available,
    /// relative colorimetric. Falls back to PLRM formula when ICC is
    /// unavailable.
    pub fn from_cmyk_icc(c: f64, m: f64, y: f64, k: f64, icc: &mut crate::icc::IccCache) -> Self {
        Self::from_cmyk_icc_with_intent(
            c,
            m,
            y,
            k,
            crate::icc::IccRenderingIntent::RelativeColorimetric,
            icc,
        )
    }

    /// Create from CMYK, converting through the default CMYK profile with
    /// `intent` if one is available. Falls back to PLRM formula when ICC is
    /// unavailable.
    pub fn from_cmyk_icc_with_intent(
        c: f64,
        m: f64,
        y: f64,
        k: f64,
        intent: crate::icc::IccRenderingIntent,
        icc: &mut crate::icc::IccCache,
    ) -> Self {
        if let Some((r, g, b)) = icc.convert_cmyk_with_intent(c, m, y, k, intent) {
            Self {
                r,
                g,
                b,
                native_cmyk: Some((c, m, y, k)),
                process_cmyk: None,
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
            process_cmyk: None,
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
            process_cmyk: None,
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

    /// Convert CIE XYZ (D65-adapted) to sRGB.
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
            process_cmyk: None,
        }
    }

    /// Bradford chromatic adaptation: adapt XYZ from source white point to D65.
    fn adapt_xyz_to_d65(x: f64, y: f64, z: f64, src_wp: &[f64; 3]) -> [f64; 3] {
        // D65 white point (sRGB standard illuminant)
        const D65: [f64; 3] = [0.95047, 1.0, 1.08883];

        // Skip adaptation if source is already D65
        if (src_wp[0] - D65[0]).abs() < 1e-3
            && (src_wp[1] - D65[1]).abs() < 1e-3
            && (src_wp[2] - D65[2]).abs() < 1e-3
        {
            return [x, y, z];
        }

        // Bradford matrix (XYZ → LMS cone space), column-major
        const M: [f64; 9] = [
            0.8951, -0.7502, 0.0389, 0.2664, 1.7135, -0.0685, -0.1614, 0.0367, 1.0296,
        ];
        // Inverse Bradford matrix (LMS → XYZ), column-major
        const M_INV: [f64; 9] = [
            0.9869929, 0.4323053, -0.0085287, -0.1470543, 0.5183603, 0.0400428, 0.1599627,
            0.0492912, 0.9684867,
        ];

        // Convert source and D65 white points to LMS
        let lms_src = Self::apply_matrix_3x3(&M, src_wp);
        let lms_d65 = Self::apply_matrix_3x3(&M, &D65);

        // Diagonal scaling in LMS space
        let s0 = if lms_src[0].abs() > 1e-10 {
            lms_d65[0] / lms_src[0]
        } else {
            1.0
        };
        let s1 = if lms_src[1].abs() > 1e-10 {
            lms_d65[1] / lms_src[1]
        } else {
            1.0
        };
        let s2 = if lms_src[2].abs() > 1e-10 {
            lms_d65[2] / lms_src[2]
        } else {
            1.0
        };

        // Adapt: M_inv × diag(s) × M × [x, y, z]
        let lms = Self::apply_matrix_3x3(&M, &[x, y, z]);
        let scaled = [lms[0] * s0, lms[1] * s1, lms[2] * s2];
        Self::apply_matrix_3x3(&M_INV, &scaled)
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

    /// Convert CIE L\*a\*b\* to sRGB.
    ///
    /// PDF Lab color spaces specify a white point, but Lab is perceptually
    /// uniform — the same (L\*, a\*, b\*) coordinates represent the same
    /// perceived color regardless of the declared white point.  We convert
    /// directly through D65 (the sRGB reference illuminant), which makes
    /// the specified white point irrelevant and avoids Bradford adaptation
    /// errors for extreme/non-physical white points.
    ///
    /// `range` is the a\*/b\* clamp range: [a_min, a_max, b_min, b_max].
    pub fn from_lab(l_star: f64, a_star: f64, b_star: f64, range: &[f64; 4]) -> Self {
        // D65 white point (sRGB reference illuminant)
        const D65: [f64; 3] = [0.95047, 1.0, 1.08883];

        let l_star = l_star.clamp(0.0, 100.0);
        let a_star = clamp_to_range(a_star, range[0], range[1]);
        let b_star = clamp_to_range(b_star, range[2], range[3]);

        let fy = (l_star + 16.0) / 116.0;
        let fx = a_star / 500.0 + fy;
        let fz = fy - b_star / 200.0;

        let x = D65[0] * Self::lab_f_inv(fx);
        let y = D65[1] * Self::lab_f_inv(fy);
        let z = D65[2] * Self::lab_f_inv(fz);

        Self::from_xyz(x, y, z)
    }

    fn lab_f_inv(t: f64) -> f64 {
        if t > 6.0 / 29.0 {
            t * t * t
        } else {
            3.0 * (6.0 / 29.0) * (6.0 / 29.0) * (t - 4.0 / 29.0)
        }
    }

    /// Convert CIEBasedABC color to sRGB.
    pub fn from_cie_abc(a: f64, b: f64, c: f64, params: &CieAbcParams) -> Self {
        let mut a = clamp_to_range(a, params.range_abc[0], params.range_abc[1]);
        let mut b = clamp_to_range(b, params.range_abc[2], params.range_abc[3]);
        let mut c = clamp_to_range(c, params.range_abc[4], params.range_abc[5]);

        if let Some(ref tables) = params.decode_abc {
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

        let lmn = Self::apply_matrix_3x3(&params.matrix_abc, &[a, b, c]);

        let mut l = clamp_to_range(lmn[0], params.range_lmn[0], params.range_lmn[1]);
        let mut m = clamp_to_range(lmn[1], params.range_lmn[2], params.range_lmn[3]);
        let mut n = clamp_to_range(lmn[2], params.range_lmn[4], params.range_lmn[5]);

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

        let xyz = Self::apply_matrix_3x3(&params.matrix_lmn, &[l, m, n]);

        // Chromatic adaptation from source white point to D65
        let xyz = Self::adapt_xyz_to_d65(xyz[0], xyz[1], xyz[2], &params.white_point);
        Self::from_xyz(xyz[0], xyz[1], xyz[2])
    }

    /// Convert CIEBasedA color to sRGB.
    pub fn from_cie_a(a: f64, params: &CieAParams) -> Self {
        let mut a = clamp_to_range(a, params.range_a[0], params.range_a[1]);

        if let Some(ref table) = params.decode_a {
            let ra = params.range_a[1] - params.range_a[0];
            let na = if ra > 0.0 {
                (a - params.range_a[0]) / ra
            } else {
                0.0
            };
            a = Self::decode_lookup(table, na);
        }

        let lmn = [
            params.matrix_a[0] * a,
            params.matrix_a[1] * a,
            params.matrix_a[2] * a,
        ];

        let mut l = clamp_to_range(lmn[0], params.range_lmn[0], params.range_lmn[1]);
        let mut m = clamp_to_range(lmn[1], params.range_lmn[2], params.range_lmn[3]);
        let mut n = clamp_to_range(lmn[2], params.range_lmn[4], params.range_lmn[5]);

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

        let xyz = Self::apply_matrix_3x3(&params.matrix_lmn, &[l, m, n]);

        // Chromatic adaptation from source white point to D65
        let xyz = Self::adapt_xyz_to_d65(xyz[0], xyz[1], xyz[2], &params.white_point);
        Self::from_xyz(xyz[0], xyz[1], xyz[2])
    }

    /// Convert CIEBasedDEF color to sRGB: through `/DecodeDEF` and
    /// `/RangeHIJ` to a position in the 3D table, interpolated trilinearly.
    pub fn from_cie_def(d: f64, e: f64, f: f64, params: &CieDefParams) -> Self {
        let dims = [params.m1, params.m2, params.m3];
        if dims.iter().any(|&m| m < 2) {
            return Self::from_gray(0.0);
        }
        let decode = params.decode_def.as_ref();
        let mut coords = [0.0; 3];
        for (i, v) in [d, e, f].into_iter().enumerate() {
            coords[i] = table_coord(
                v,
                [params.range_def[2 * i], params.range_def[2 * i + 1]],
                decode.map(|t| t[i].as_slice()),
                [params.range_hij[2 * i], params.range_hij[2 * i + 1]],
                dims[i],
            );
        }
        let tables = [&params.a_table, &params.b_table, &params.c_table];
        let Some(abc) = interpolate_table(tables, &dims, &coords) else {
            return Self::from_gray(0.0);
        };
        Self::from_cie_abc(abc[0], abc[1], abc[2], &params.abc_params)
    }

    /// Convert CIEBasedDEFG color to sRGB: through `/DecodeDEFG` and
    /// `/RangeHIJK` to a position in the 4D table, interpolated
    /// quadrilinearly, as Ghostscript does.
    pub fn from_cie_defg(d: f64, e: f64, f: f64, g: f64, params: &CieDefgParams) -> Self {
        let dims = [params.m1, params.m2, params.m3, params.m4];
        if dims.iter().any(|&m| m < 2) {
            return Self::from_gray(0.0);
        }
        let decode = params.decode_defg.as_ref();
        let mut coords = [0.0; 4];
        for (i, v) in [d, e, f, g].into_iter().enumerate() {
            coords[i] = table_coord(
                v,
                [params.range_defg[2 * i], params.range_defg[2 * i + 1]],
                decode.map(|t| t[i].as_slice()),
                [params.range_hijk[2 * i], params.range_hijk[2 * i + 1]],
                dims[i],
            );
        }
        let tables = [&params.a_table, &params.b_table, &params.c_table];
        let Some(abc) = interpolate_table(tables, &dims, &coords) else {
            return Self::from_gray(0.0);
        };
        Self::from_cie_abc(abc[0], abc[1], abc[2], &params.abc_params)
    }
}

/// Where one component of a CIEBasedDEF or CIEBasedDEFG colour falls on its
/// table axis of `m` points (PLRM): clipped to its input range `range_in`,
/// through its decode table if it has one (sampled over `range_in`, as the
/// ABC decode tables are), clipped to `range_out` — the `/RangeHIJ` or
/// `/RangeHIJK` pair, which spans the axis — and scaled to `0..m-1`.
fn table_coord(
    v: f64,
    range_in: [f64; 2],
    decode: Option<&[f64]>,
    range_out: [f64; 2],
    m: usize,
) -> f64 {
    let v = clamp_to_range(v, range_in[0], range_in[1]);
    let v = match decode {
        Some(table) => {
            let span = range_in[1] - range_in[0];
            let t = if span != 0.0 {
                (v - range_in[0]) / span
            } else {
                0.0
            };
            DeviceColor::decode_lookup(table, clamp_to_range(t, 0.0, 1.0))
        }
        None => v,
    };
    let v = clamp_to_range(v, range_out[0], range_out[1]);
    let span = range_out[1] - range_out[0];
    let t = if span != 0.0 {
        (v - range_out[0]) / span
    } else {
        0.0
    };
    clamp_to_range(t * (m - 1) as f64, 0.0, (m - 1) as f64)
}

/// Interpolate the A, B and C tables of a CIEBasedDEF or CIEBasedDEFG space
/// at `coords`, a position on each axis of `dims` points (the last axis
/// varying fastest in the tables): the weighted sum of the 2ⁿ entries around
/// it. `None` when a table is shorter than `dims` says, which the parser
/// does not let happen.
fn interpolate_table(tables: [&Vec<f64>; 3], dims: &[usize], coords: &[f64]) -> Option<[f64; 3]> {
    let n = dims.len();
    let total: usize = dims.iter().product();
    if tables.iter().any(|t| t.len() < total) {
        return None;
    }
    // The cell's lower corner and the position within it, per axis.
    let mut base = [0usize; 4];
    let mut frac = [0.0f64; 4];
    let mut stride = [0usize; 4];
    let mut s = 1;
    for i in (0..n).rev() {
        stride[i] = s;
        s *= dims[i];
        let lo = (coords[i] as usize).min(dims[i] - 2);
        base[i] = lo;
        frac[i] = coords[i] - lo as f64;
    }
    let mut abc = [0.0; 3];
    for corner in 0..(1usize << n) {
        let mut weight = 1.0;
        let mut index = 0;
        for i in 0..n {
            let up = (corner >> i) & 1 == 1;
            weight *= if up { frac[i] } else { 1.0 - frac[i] };
            index += (base[i] + up as usize) * stride[i];
        }
        if weight == 0.0 {
            continue;
        }
        for (ch, table) in tables.iter().enumerate() {
            abc[ch] += table[index] * weight;
        }
    }
    Some(abc)
}

/// Clip `value` to the range from `a` to `b`, whichever way round they are.
///
/// For ranges a file supplies — a colour space's `/RangeABC` or `/Range`, a
/// function's `/Domain` — which are not to be trusted to be in order.
/// `f64::clamp` panics on a reversed or NaN pair, so `/RangeABC [1 0 0 1 0 1]`
/// in a PostScript program, or a Lab `/Range [100 -100 -100 100]` in a PDF,
/// took the process down. A reversed pair is taken as the range it spans; a
/// NaN bound leaves the other bound to stand for both, and a NaN value gives
/// the lower bound.
pub fn clamp_to_range(value: f64, a: f64, b: f64) -> f64 {
    value.max(a.min(b)).min(a.max(b))
}

impl Default for DeviceColor {
    fn default() -> Self {
        Self::black()
    }
}

/// Extracted parameters for CIEBasedABC color conversion.
#[derive(Clone, Debug)]
pub struct CieAbcParams {
    pub range_abc: [f64; 6],
    pub decode_abc: Option<[Vec<f64>; 3]>,
    pub matrix_abc: [f64; 9],
    pub range_lmn: [f64; 6],
    pub decode_lmn: Option<[Vec<f64>; 3]>,
    pub matrix_lmn: [f64; 9],
    pub white_point: [f64; 3],
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
            white_point: [0.9505, 1.0, 1.089],
        }
    }
}

/// Extracted parameters for CIEBasedA color conversion.
#[derive(Clone, Debug)]
pub struct CieAParams {
    pub range_a: [f64; 2],
    pub decode_a: Option<Vec<f64>>,
    pub matrix_a: [f64; 3],
    pub range_lmn: [f64; 6],
    pub decode_lmn: Option<[Vec<f64>; 3]>,
    pub matrix_lmn: [f64; 9],
    pub white_point: [f64; 3],
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
            white_point: [0.9505, 1.0, 1.089],
        }
    }
}

/// Pre-converted parameters for CIEBasedDEF color space (3D table → RGB).
///
/// A colour runs DEF → clipped to `range_def` → `decode_def` → HIJ, clipped
/// to `range_hij` → the table, interpolated → ABC → [`CieAbcParams`].
///
/// New fields may be added, so outside this crate start from
/// [`Default`] and set the fields.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct CieDefParams {
    pub range_def: [f64; 6],
    /// `/RangeHIJ`: the span of each table axis, `[0 1]` for each by default.
    pub range_hij: [f64; 6],
    /// `/DecodeDEF`, each procedure sampled over its `range_def` pair, as
    /// [`CieAbcParams::decode_abc`] is. `None` for the identity.
    pub decode_def: Option<[Vec<f64>; 3]>,
    pub m1: usize,
    pub m2: usize,
    pub m3: usize,
    pub a_table: Vec<f64>,
    pub b_table: Vec<f64>,
    pub c_table: Vec<f64>,
    pub abc_params: CieAbcParams,
}

/// Parameters for CIEBasedDEFG color space (4D table → ABC → CIE pipeline).
///
/// A colour runs DEFG → clipped to `range_defg` → `decode_defg` → HIJK,
/// clipped to `range_hijk` → the table, interpolated → ABC → [`CieAbcParams`].
///
/// New fields may be added, so outside this crate start from
/// [`Default`] and set the fields.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct CieDefgParams {
    pub range_defg: [f64; 8],
    /// `/RangeHIJK`: the span of each table axis, `[0 1]` for each by default.
    pub range_hijk: [f64; 8],
    /// `/DecodeDEFG`, each procedure sampled over its `range_defg` pair, as
    /// [`CieAbcParams::decode_abc`] is. `None` for the identity.
    pub decode_defg: Option<[Vec<f64>; 4]>,
    pub m1: usize,
    pub m2: usize,
    pub m3: usize,
    pub m4: usize,
    pub a_table: Vec<f64>,
    pub b_table: Vec<f64>,
    pub c_table: Vec<f64>,
    pub abc_params: CieAbcParams,
}

impl Default for CieDefParams {
    /// Identity ranges and decoding, and no table yet.
    fn default() -> Self {
        Self {
            range_def: [0.0, 1.0, 0.0, 1.0, 0.0, 1.0],
            range_hij: [0.0, 1.0, 0.0, 1.0, 0.0, 1.0],
            decode_def: None,
            m1: 0,
            m2: 0,
            m3: 0,
            a_table: Vec::new(),
            b_table: Vec::new(),
            c_table: Vec::new(),
            abc_params: CieAbcParams::default(),
        }
    }
}

impl Default for CieDefgParams {
    /// Identity ranges and decoding, and no table yet.
    fn default() -> Self {
        Self {
            range_defg: [0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0],
            range_hijk: [0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0],
            decode_defg: None,
            m1: 0,
            m2: 0,
            m3: 0,
            m4: 0,
            a_table: Vec::new(),
            b_table: Vec::new(),
            c_table: Vec::new(),
            abc_params: CieAbcParams::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_color_from_gray() {
        let c = DeviceColor::from_gray(0.5);
        assert!((c.r - 0.5).abs() < 1e-10);
        assert!((c.g - 0.5).abs() < 1e-10);
        assert!((c.b - 0.5).abs() < 1e-10);
    }

    #[test]
    fn test_color_from_cmyk() {
        let c = DeviceColor::from_cmyk(1.0, 0.0, 0.0, 0.0);
        assert!((c.r - 0.0).abs() < 1e-10);
        assert!((c.g - 1.0).abs() < 1e-10);
        assert!((c.b - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_color_from_hsb() {
        let c = DeviceColor::from_hsb(0.0, 1.0, 1.0);
        assert!((c.r - 1.0).abs() < 1e-10);
        assert!((c.g - 0.0).abs() < 1e-10);
        assert!((c.b - 0.0).abs() < 1e-10);

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
    fn clamp_to_range_takes_either_order_and_never_panics() {
        assert_eq!(clamp_to_range(5.0, 0.0, 1.0), 1.0);
        assert_eq!(clamp_to_range(5.0, 1.0, 0.0), 1.0);
        assert_eq!(clamp_to_range(-5.0, 1.0, 0.0), 0.0);
        assert_eq!(clamp_to_range(0.5, f64::NAN, 1.0), 1.0);
        assert_eq!(clamp_to_range(f64::NAN, 0.0, 1.0), 0.0);
    }

    /// A 2×2×2×2 DEFG table whose A, B and C are each taken from one axis
    /// (D, E and F; G leaves them alone), so interpolation is visible in the
    /// ABC values the table produces.
    fn axis_table() -> CieDefgParams {
        let mut a = Vec::new();
        let mut b = Vec::new();
        let mut c = Vec::new();
        for d in 0..2 {
            for e in 0..2 {
                for f in 0..2 {
                    for _g in 0..2 {
                        a.push(d as f64);
                        b.push(e as f64);
                        c.push(f as f64);
                    }
                }
            }
        }
        CieDefgParams {
            range_defg: [0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0],
            range_hijk: [0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0],
            decode_defg: None,
            m1: 2,
            m2: 2,
            m3: 2,
            m4: 2,
            a_table: a,
            b_table: b,
            c_table: c,
            abc_params: CieAbcParams::default(),
        }
    }

    /// The colour the ABC stage gives `abc`, to compare a table's output with.
    fn via_abc(abc: [f64; 3], params: &CieDefgParams) -> DeviceColor {
        DeviceColor::from_cie_abc(abc[0], abc[1], abc[2], &params.abc_params)
    }

    fn same_color(x: &DeviceColor, y: &DeviceColor) -> bool {
        (x.r - y.r).abs() < 1e-9 && (x.g - y.g).abs() < 1e-9 && (x.b - y.b).abs() < 1e-9
    }

    #[test]
    fn defg_table_is_interpolated_between_entries() {
        let p = axis_table();
        // Halfway along D and a quarter along E: A 0.5, B 0.25. The nearest
        // entry, which stet used to take, would give 1 and 0.
        let got = DeviceColor::from_cie_defg(0.5, 0.25, 0.0, 0.7, &p);
        assert!(same_color(&got, &via_abc([0.5, 0.25, 0.0], &p)));
    }

    #[test]
    fn defg_decode_and_range_hijk_place_the_colour_on_the_table() {
        let mut p = axis_table();
        // DecodeDEFG inverts D, and RangeHIJK makes E's axis span 0..2.
        let invert: Vec<f64> = (0..256).map(|i| 1.0 - i as f64 / 255.0).collect();
        let identity: Vec<f64> = (0..256).map(|i| i as f64 / 255.0).collect();
        p.decode_defg = Some([invert, identity.clone(), identity.clone(), identity]);
        p.range_hijk = [0.0, 1.0, 0.0, 2.0, 0.0, 1.0, 0.0, 1.0];
        let got = DeviceColor::from_cie_defg(0.25, 1.0, 0.0, 0.0, &p);
        assert!(same_color(&got, &via_abc([0.75, 0.5, 0.0], &p)));
    }
}
