// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Geometry types: affine transform matrices, path segments, and paths.

/// Round to 10 decimal places to eliminate floating-point artifacts.
#[inline]
pub fn round10(v: f64) -> f64 {
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

impl Default for Matrix {
    fn default() -> Self {
        Self::identity()
    }
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
    pub fn rotate(angle: f64) -> Self {
        let rad = angle.to_radians();
        let (sin, cos) = (rad.sin(), rad.cos());
        Self {
            a: round10(cos),
            b: round10(sin),
            c: round10(-sin),
            d: round10(cos),
            tx: 0.0,
            ty: 0.0,
        }
    }

    /// Column-vector multiply: self × other.
    ///
    /// Composes two transforms: the result applies `other` first, then `self`.
    #[inline]
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

    /// PostScript `concat`: CTM = other × CTM (row-vector convention).
    ///
    /// Uses column-vector multiply internally: `self.multiply(other)`.
    #[inline]
    pub fn concat(&self, other: &Matrix) -> Matrix {
        self.multiply(other)
    }

    /// Transform a point.
    #[inline]
    pub fn transform_point(&self, x: f64, y: f64) -> (f64, f64) {
        (
            round10(self.a * x + self.c * y + self.tx),
            round10(self.b * x + self.d * y + self.ty),
        )
    }

    /// Transform a delta (no translation).
    #[inline]
    pub fn transform_delta(&self, dx: f64, dy: f64) -> (f64, f64) {
        (
            round10(self.a * dx + self.c * dy),
            round10(self.b * dx + self.d * dy),
        )
    }

    /// Determinant.
    pub fn determinant(&self) -> f64 {
        self.a * self.d - self.b * self.c
    }

    /// Inverse matrix, or None if singular.
    pub fn invert(&self) -> Option<Matrix> {
        let det = self.determinant();
        if det.abs() < 1e-20 {
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

    /// Convert to [a, b, c, d, tx, ty] array.
    pub fn to_array(&self) -> [f64; 6] {
        [self.a, self.b, self.c, self.d, self.tx, self.ty]
    }
}

/// A segment in a device-space path.
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

/// A path in device space, composed of path segments.
#[derive(Clone, Debug)]
pub struct PsPath {
    pub segments: Vec<PathSegment>,
}

impl PsPath {
    /// Create an empty path.
    pub fn new() -> Self {
        Self {
            segments: Vec::new(),
        }
    }

    /// Returns true if the path has no segments.
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Remove all segments.
    pub fn clear(&mut self) {
        self.segments.clear();
    }

    /// Transform all points through a matrix, returning a new path.
    pub fn transform(&self, m: &Matrix) -> PsPath {
        let segments = self
            .segments
            .iter()
            .map(|seg| match *seg {
                PathSegment::MoveTo(x, y) => {
                    let (tx, ty) = m.transform_point(x, y);
                    PathSegment::MoveTo(tx, ty)
                }
                PathSegment::LineTo(x, y) => {
                    let (tx, ty) = m.transform_point(x, y);
                    PathSegment::LineTo(tx, ty)
                }
                PathSegment::CurveTo {
                    x1,
                    y1,
                    x2,
                    y2,
                    x3,
                    y3,
                } => {
                    let (tx1, ty1) = m.transform_point(x1, y1);
                    let (tx2, ty2) = m.transform_point(x2, y2);
                    let (tx3, ty3) = m.transform_point(x3, y3);
                    PathSegment::CurveTo {
                        x1: tx1,
                        y1: ty1,
                        x2: tx2,
                        y2: ty2,
                        x3: tx3,
                        y3: ty3,
                    }
                }
                PathSegment::ClosePath => PathSegment::ClosePath,
            })
            .collect();
        PsPath { segments }
    }
}

impl Default for PsPath {
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
        let m = t.multiply(&s);
        let (x, y) = m.transform_point(5.0, 3.0);
        assert!((x - 20.0).abs() < 1e-10);
        assert!((y - 6.0).abs() < 1e-10);
    }

    #[test]
    fn test_matrix_concat() {
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
}
