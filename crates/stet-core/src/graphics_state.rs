// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Graphics state: transforms, paths, colors, and rendering parameters.

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

    /// Convert CIEBasedABC color to sRGB.
    ///
    /// Pipeline: input → RangeABC clamp → MatrixABC → RangeLMN clamp → MatrixLMN → XYZ → sRGB
    /// (Decode procedures are not evaluated in this static path.)
    pub fn from_cie_abc(a: f64, b: f64, c: f64, params: &CieAbcParams) -> Self {
        // RangeABC clamp
        let a = a.clamp(params.range_abc[0], params.range_abc[1]);
        let b = b.clamp(params.range_abc[2], params.range_abc[3]);
        let c = c.clamp(params.range_abc[4], params.range_abc[5]);

        // MatrixABC (column-major 3×3)
        let lmn = Self::apply_matrix_3x3(&params.matrix_abc, &[a, b, c]);

        // RangeLMN clamp
        let l = lmn[0].clamp(params.range_lmn[0], params.range_lmn[1]);
        let m = lmn[1].clamp(params.range_lmn[2], params.range_lmn[3]);
        let n = lmn[2].clamp(params.range_lmn[4], params.range_lmn[5]);

        // MatrixLMN → XYZ
        let xyz = Self::apply_matrix_3x3(&params.matrix_lmn, &[l, m, n]);

        Self::from_xyz(xyz[0], xyz[1], xyz[2])
    }

    /// Convert CIEBasedA color to sRGB.
    ///
    /// Pipeline: input → RangeA clamp → MatrixA → RangeLMN clamp → MatrixLMN → XYZ → sRGB
    /// (Decode procedures are not evaluated in this static path.)
    pub fn from_cie_a(a: f64, params: &CieAParams) -> Self {
        // RangeA clamp
        let a = a.clamp(params.range_a[0], params.range_a[1]);

        // MatrixA: 3-element vector multiplied by scalar A
        let lmn = [
            params.matrix_a[0] * a,
            params.matrix_a[1] * a,
            params.matrix_a[2] * a,
        ];

        // RangeLMN clamp
        let l = lmn[0].clamp(params.range_lmn[0], params.range_lmn[1]);
        let m = lmn[1].clamp(params.range_lmn[2], params.range_lmn[3]);
        let n = lmn[2].clamp(params.range_lmn[4], params.range_lmn[5]);

        // MatrixLMN → XYZ
        let xyz = Self::apply_matrix_3x3(&params.matrix_lmn, &[l, m, n]);

        Self::from_xyz(xyz[0], xyz[1], xyz[2])
    }
}

/// Color space identifier.
#[derive(Clone, Debug, PartialEq)]
pub enum ColorSpace {
    DeviceGray,
    DeviceRGB,
    DeviceCMYK,
    /// Indexed color space: `[/Indexed base hival lookup_bytes]`.
    Indexed {
        base: Box<ColorSpace>,
        hival: u32,
        lookup: Vec<u8>,
    },
    /// CIE-based ABC color space (3 components): `[/CIEBasedABC dict]`.
    CIEBasedABC {
        dict_entity: crate::object::EntityId,
    },
    /// CIE-based A color space (1 component): `[/CIEBasedA dict]`.
    CIEBasedA {
        dict_entity: crate::object::EntityId,
    },
    /// ICC-based color space: `[/ICCBased dict]` where dict has /N components.
    /// Falls back to device space based on N (1=Gray, 3=RGB, 4=CMYK).
    ICCBased {
        dict_entity: crate::object::EntityId,
        n: u32,
    },
    /// Separation color space: `[/Separation name alternativeSpace tintTransform]`.
    /// Single tint component mapped to alternative space via tint transform procedure.
    Separation {
        alt_space: Box<ColorSpace>,
        tint_transform: crate::object::PsObject,
        num_alt_components: u32,
    },
    /// DeviceN color space: `[/DeviceN names alternativeSpace tintTransform]`.
    /// N tint components mapped to alternative space via tint transform procedure.
    DeviceN {
        num_colorants: u32,
        alt_space: Box<ColorSpace>,
        tint_transform: crate::object::PsObject,
        num_alt_components: u32,
    },
}

/// Extracted parameters for CIEBasedABC color conversion.
#[derive(Clone, Debug)]
pub struct CieAbcParams {
    pub range_abc: [f64; 6],  // [min_a, max_a, min_b, max_b, min_c, max_c]
    pub matrix_abc: [f64; 9], // Column-major 3×3 (default identity)
    pub range_lmn: [f64; 6],  // [min_l, max_l, min_m, max_m, min_n, max_n]
    pub matrix_lmn: [f64; 9], // Column-major 3×3 (default identity)
}

impl Default for CieAbcParams {
    fn default() -> Self {
        Self {
            range_abc: [0.0, 1.0, 0.0, 1.0, 0.0, 1.0],
            matrix_abc: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            range_lmn: [0.0, 1.0, 0.0, 1.0, 0.0, 1.0],
            matrix_lmn: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
        }
    }
}

/// Extracted parameters for CIEBasedA color conversion.
#[derive(Clone, Debug)]
pub struct CieAParams {
    pub range_a: [f64; 2],    // [min, max]
    pub matrix_a: [f64; 3],   // 3-element vector (default [1,1,1])
    pub range_lmn: [f64; 6],  // [min_l, max_l, min_m, max_m, min_n, max_n]
    pub matrix_lmn: [f64; 9], // Column-major 3×3 (default identity)
}

impl Default for CieAParams {
    fn default() -> Self {
        Self {
            range_a: [0.0, 1.0],
            matrix_a: [1.0, 1.0, 1.0],
            range_lmn: [0.0, 1.0, 0.0, 1.0, 0.0, 1.0],
            matrix_lmn: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
        }
    }
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
            default_ctm: Matrix::identity(),
            clip_stack: Vec::new(),
            current_font: None,
            root_font: None,
            page_device: None,
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
