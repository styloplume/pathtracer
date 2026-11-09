//! Basic linear algebra: vectors, rays, orthonormal frames and affine transforms.

use std::ops::{Add, AddAssign, Div, DivAssign, Index, Mul, MulAssign, Neg, Sub, SubAssign};

pub const PI: f32 = std::f32::consts::PI;
pub const INV_PI: f32 = 1.0 / PI;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub const ZERO: Vec3 = Vec3::new(0.0, 0.0, 0.0);
    pub const ONE: Vec3 = Vec3::new(1.0, 1.0, 1.0);
    pub const Z: Vec3 = Vec3::new(0.0, 0.0, 1.0);

    #[inline]
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Vec3 { x, y, z }
    }
    #[inline]
    pub const fn splat(v: f32) -> Self {
        Vec3::new(v, v, v)
    }
    #[inline]
    pub fn dot(self, o: Vec3) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }
    #[inline]
    pub fn cross(self, o: Vec3) -> Vec3 {
        Vec3::new(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
    }
    #[inline]
    pub fn length_squared(self) -> f32 {
        self.dot(self)
    }
    #[inline]
    pub fn length(self) -> f32 {
        self.length_squared().sqrt()
    }
    #[inline]
    pub fn normalize(self) -> Vec3 {
        self * (1.0 / self.length())
    }
    #[inline]
    pub fn min(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x.min(o.x), self.y.min(o.y), self.z.min(o.z))
    }
    #[inline]
    pub fn max(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x.max(o.x), self.y.max(o.y), self.z.max(o.z))
    }
    #[inline]
    pub fn max_elem(self) -> f32 {
        self.x.max(self.y).max(self.z)
    }
    #[inline]
    pub fn abs(self) -> Vec3 {
        Vec3::new(self.x.abs(), self.y.abs(), self.z.abs())
    }
    #[inline]
    pub fn recip(self) -> Vec3 {
        Vec3::new(1.0 / self.x, 1.0 / self.y, 1.0 / self.z)
    }
    #[inline]
    pub fn is_black(self) -> bool {
        self.x <= 0.0 && self.y <= 0.0 && self.z <= 0.0
    }
    #[inline]
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }
    /// Rec. 709 relative luminance.
    #[inline]
    pub fn luminance(self) -> f32 {
        0.2126 * self.x + 0.7152 * self.y + 0.0722 * self.z
    }
    #[inline]
    pub fn lerp(self, o: Vec3, t: f32) -> Vec3 {
        self * (1.0 - t) + o * t
    }
}

impl Index<usize> for Vec3 {
    type Output = f32;
    #[inline]
    fn index(&self, i: usize) -> &f32 {
        match i {
            0 => &self.x,
            1 => &self.y,
            _ => &self.z,
        }
    }
}

macro_rules! impl_binop {
    ($tr:ident, $f:ident, $atr:ident, $af:ident, $op:tt) => {
        impl $tr for Vec3 {
            type Output = Vec3;
            #[inline]
            fn $f(self, o: Vec3) -> Vec3 {
                Vec3::new(self.x $op o.x, self.y $op o.y, self.z $op o.z)
            }
        }
        impl $tr<f32> for Vec3 {
            type Output = Vec3;
            #[inline]
            fn $f(self, s: f32) -> Vec3 {
                Vec3::new(self.x $op s, self.y $op s, self.z $op s)
            }
        }
        impl $atr for Vec3 {
            #[inline]
            fn $af(&mut self, o: Vec3) {
                *self = *self $op o;
            }
        }
        impl $atr<f32> for Vec3 {
            #[inline]
            fn $af(&mut self, s: f32) {
                *self = *self $op s;
            }
        }
    };
}
impl_binop!(Add, add, AddAssign, add_assign, +);
impl_binop!(Sub, sub, SubAssign, sub_assign, -);
impl_binop!(Mul, mul, MulAssign, mul_assign, *);
impl_binop!(Div, div, DivAssign, div_assign, /);

impl Mul<Vec3> for f32 {
    type Output = Vec3;
    #[inline]
    fn mul(self, v: Vec3) -> Vec3 {
        v * self
    }
}

impl Neg for Vec3 {
    type Output = Vec3;
    #[inline]
    fn neg(self) -> Vec3 {
        Vec3::new(-self.x, -self.y, -self.z)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Ray {
    pub o: Vec3,
    pub d: Vec3,
}

impl Ray {
    #[inline]
    pub fn new(o: Vec3, d: Vec3) -> Self {
        Ray { o, d }
    }
}

/// Offsets a surface point along the geometric normal, to the side `dir` points to,
/// so that rays leaving the surface do not re-intersect it.
#[inline]
pub fn offset_origin(p: Vec3, ng: Vec3, dir: Vec3) -> Vec3 {
    let eps = 1e-4 * (1.0 + p.abs().max_elem());
    if dir.dot(ng) >= 0.0 { p + ng * eps } else { p - ng * eps }
}

/// Orthonormal basis with `n` as the local +Z axis (Duff et al. 2017).
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub t: Vec3,
    pub b: Vec3,
    pub n: Vec3,
}

impl Frame {
    #[inline]
    pub fn from_normal(n: Vec3) -> Frame {
        let s = if n.z >= 0.0 { 1.0 } else { -1.0 };
        let a = -1.0 / (s + n.z);
        let b = n.x * n.y * a;
        Frame {
            t: Vec3::new(1.0 + s * n.x * n.x * a, s * b, -s * n.x),
            b: Vec3::new(b, s + n.y * n.y * a, -n.y),
            n,
        }
    }
    #[inline]
    pub fn to_local(&self, v: Vec3) -> Vec3 {
        Vec3::new(v.dot(self.t), v.dot(self.b), v.dot(self.n))
    }
    #[inline]
    pub fn to_world(&self, v: Vec3) -> Vec3 {
        self.t * v.x + self.b * v.y + self.n * v.z
    }
}

/// Row-major 3x3 matrix.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mat3 {
    pub m: [[f32; 3]; 3],
}

impl Mat3 {
    pub fn scale(s: Vec3) -> Mat3 {
        Mat3 { m: [[s.x, 0.0, 0.0], [0.0, s.y, 0.0], [0.0, 0.0, s.z]] }
    }
    pub fn rotate_x(deg: f32) -> Mat3 {
        let (s, c) = deg.to_radians().sin_cos();
        Mat3 { m: [[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]] }
    }
    pub fn rotate_y(deg: f32) -> Mat3 {
        let (s, c) = deg.to_radians().sin_cos();
        Mat3 { m: [[c, 0.0, s], [0.0, 1.0, 0.0], [-s, 0.0, c]] }
    }
    pub fn rotate_z(deg: f32) -> Mat3 {
        let (s, c) = deg.to_radians().sin_cos();
        Mat3 { m: [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]] }
    }
    pub fn mul(&self, o: &Mat3) -> Mat3 {
        let mut r = [[0.0f32; 3]; 3];
        for (i, row) in r.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                *v = (0..3).map(|k| self.m[i][k] * o.m[k][j]).sum();
            }
        }
        Mat3 { m: r }
    }
    #[inline]
    pub fn mul_vec(&self, v: Vec3) -> Vec3 {
        let m = &self.m;
        Vec3::new(
            m[0][0] * v.x + m[0][1] * v.y + m[0][2] * v.z,
            m[1][0] * v.x + m[1][1] * v.y + m[1][2] * v.z,
            m[2][0] * v.x + m[2][1] * v.y + m[2][2] * v.z,
        )
    }
    pub fn transpose(&self) -> Mat3 {
        let m = &self.m;
        Mat3 { m: [[m[0][0], m[1][0], m[2][0]], [m[0][1], m[1][1], m[2][1]], [m[0][2], m[1][2], m[2][2]]] }
    }
    pub fn determinant(&self) -> f32 {
        let a = &self.m;
        a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1]) - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
            + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0])
    }
    pub fn inverse(&self) -> Option<Mat3> {
        let det = self.determinant();
        if det.abs() < 1e-30 {
            return None;
        }
        let a = &self.m;
        let i = 1.0 / det;
        Some(Mat3 {
            m: [
                [
                    (a[1][1] * a[2][2] - a[1][2] * a[2][1]) * i,
                    (a[0][2] * a[2][1] - a[0][1] * a[2][2]) * i,
                    (a[0][1] * a[1][2] - a[0][2] * a[1][1]) * i,
                ],
                [
                    (a[1][2] * a[2][0] - a[1][0] * a[2][2]) * i,
                    (a[0][0] * a[2][2] - a[0][2] * a[2][0]) * i,
                    (a[0][2] * a[1][0] - a[0][0] * a[1][2]) * i,
                ],
                [
                    (a[1][0] * a[2][1] - a[1][1] * a[2][0]) * i,
                    (a[0][1] * a[2][0] - a[0][0] * a[2][1]) * i,
                    (a[0][0] * a[1][1] - a[0][1] * a[1][0]) * i,
                ],
            ],
        })
    }
}

/// Affine transform: scale, then rotate (X, then Y, then Z, in degrees), then translate.
#[derive(Clone, Copy, Debug)]
pub struct Transform {
    pub m: Mat3,
    pub t: Vec3,
    normal_m: Mat3,
}

impl Transform {
    pub fn new(translate: Vec3, rotate_deg: Vec3, scale: Vec3) -> Result<Transform, String> {
        let m = Mat3::rotate_z(rotate_deg.z)
            .mul(&Mat3::rotate_y(rotate_deg.y))
            .mul(&Mat3::rotate_x(rotate_deg.x))
            .mul(&Mat3::scale(scale));
        let inv = m.inverse().ok_or("transform is degenerate (zero scale?)")?;
        Ok(Transform { m, t: translate, normal_m: inv.transpose() })
    }
    #[inline]
    pub fn point(&self, p: Vec3) -> Vec3 {
        self.m.mul_vec(p) + self.t
    }
    #[inline]
    pub fn normal(&self, n: Vec3) -> Vec3 {
        self.normal_m.mul_vec(n).normalize()
    }
    /// True if the transform mirrors geometry (triangle winding must be flipped).
    pub fn flips_handedness(&self) -> bool {
        self.m.determinant() < 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_is_orthonormal() {
        for n in [Vec3::new(0.0, 0.0, 1.0), Vec3::new(0.0, 0.0, -1.0), Vec3::new(1.0, 2.0, -3.0).normalize()] {
            let f = Frame::from_normal(n);
            assert!(f.t.dot(f.b).abs() < 1e-5 && f.t.dot(f.n).abs() < 1e-5 && f.b.dot(f.n).abs() < 1e-5);
            let v = Vec3::new(0.3, -0.5, 0.8);
            assert!((f.to_world(f.to_local(v)) - v).length() < 1e-5);
        }
    }

    #[test]
    fn inverse_roundtrip() {
        let m = Mat3::rotate_y(30.0).mul(&Mat3::scale(Vec3::new(2.0, 3.0, 0.5)));
        let id = m.mul(&m.inverse().unwrap());
        for i in 0..3 {
            for j in 0..3 {
                assert!((id.m[i][j] - if i == j { 1.0 } else { 0.0 }).abs() < 1e-5);
            }
        }
    }
}
