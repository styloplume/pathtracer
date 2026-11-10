//! Implicit surfaces, ray traced by sphere tracing (Hart 1996): signed distance fields
//! with smooth CSG and noise displacement, hypertextured solids (Perlin & Hoffert 1989), and
//! deterministic 3D fractals (quaternion Julia sets after Hart, Sandin & Kauffman 1989,
//! the Mandelbulb and the Menger sponge).
//!
//! A field `f` is negative inside and positive outside. If it has Lipschitz bound `L`
//! (|f(a) - f(b)| <= L |a - b|), no surface lies within f(p) / L of p, so a ray can step
//! that far safely. Exact distance fields and their (smooth) CSG have L = 1, and so do
//! fractal distance estimators. Fields without a known bound (displacements and
//! hypertextures) estimate it by sampling their gradient. Steps never shrink below half
//! the hit tolerance. A step that overshoots into the other side is caught by the sign
//! change and bisected, so a bound that is slightly too low costs accuracy at grazing
//! angles rather than leaving holes.
//!
//! Precision in f32: a hit is reported where |f| drops below the object's tolerance
//! `eps`. By default this is a small fraction of the object's size; fractals use a larger
//! one, which sets how much detail they show. Normals are the field gradient (tetrahedral
//! differences over `eps`). A ray leaving the surface starts within the tolerance shell.
//! It takes its side from the gradient and steps out of the shell before marching, so it
//! neither finds the surface it left again nor tunnels through it when it turns inwards
//! (glass).

use crate::geometry::Aabb;
use crate::math::{Transform, Trs, Vec3};
use crate::noise::Fractal;

// ---------------------------------------------------------------------------- distance fields

/// A signed distance field: primitives centred at the origin, transforms, CSG and
/// modifiers. Every node except `Displace` keeps the Lipschitz bound 1.
#[derive(Clone, Debug)]
pub enum Sdf {
    Sphere {
        radius: f32,
    },
    /// Box with half extents `half`, edges rounded by `round`.
    Box {
        half: Vec3,
        round: f32,
    },
    /// Ring in the xz plane (around +y).
    Torus {
        major: f32,
        minor: f32,
    },
    /// Capped cylinder along y.
    Cylinder {
        radius: f32,
        half_height: f32,
    },
    /// Segment `a`–`b` swept by a sphere.
    Capsule {
        a: Vec3,
        b: Vec3,
        radius: f32,
    },
    /// A child placed by an affine transform. Distances are multiplied by the smallest
    /// scale of the transform, which keeps them a lower bound.
    Transform {
        xf: Transform,
        scale: f32,
        child: Box<Sdf>,
    },
    /// Smooth union (polynomial smooth minimum over `blend` units).
    Union {
        children: Vec<Sdf>,
        blend: f32,
    },
    Intersection {
        children: Vec<Sdf>,
        blend: f32,
    },
    /// `base` minus every `cut`.
    Difference {
        base: Box<Sdf>,
        cuts: Vec<Sdf>,
        blend: f32,
    },
    /// Inflated by `radius` (rounds edges, grows the solid).
    Round {
        child: Box<Sdf>,
        radius: f32,
    },
    /// Hollowed into a shell `thickness` thick around the surface.
    Shell {
        child: Box<Sdf>,
        thickness: f32,
    },
    /// Surface moved outwards by `amount` × (noise − ½).
    Displace {
        child: Box<Sdf>,
        noise: Fractal,
        amount: f32,
    },
}

/// Polynomial smooth minimum (Quílez): equals `min` where the arguments differ by more
/// than `k`, and stays 1-Lipschitz (its slopes are a convex blend of the inputs').
#[inline]
pub fn smin(a: f32, b: f32, k: f32) -> f32 {
    if k <= 0.0 {
        return a.min(b);
    }
    let h = (k - (a - b).abs()).max(0.0) / k;
    a.min(b) - h * h * k * 0.25
}

#[inline]
pub fn smax(a: f32, b: f32, k: f32) -> f32 {
    -smin(-a, -b, k)
}

/// Noise centred on zero and clamped to [-1, 1] (bounds rely on the clamp).
#[inline]
fn noise_term(noise: &Fractal, p: Vec3) -> f32 {
    (noise.eval(p, 0.0) - 0.5).clamp(-1.0, 1.0)
}

fn grow(b: Aabb, r: f32) -> Aabb {
    let r = Vec3::splat(r.max(0.0));
    Aabb { min: b.min - r, max: b.max + r }
}

fn symmetric(e: Vec3) -> Aabb {
    Aabb { min: -e, max: e }
}

impl Sdf {
    /// Wraps `self` in the transform `trs` (nothing for the identity).
    pub fn transformed(self, trs: &Trs) -> Result<Sdf, String> {
        if *trs == Trs::IDENTITY {
            return Ok(self);
        }
        let xf = trs.transform()?;
        let s = trs.scale.abs();
        Ok(Sdf::Transform { xf, scale: s.x.min(s.y).min(s.z), child: Box::new(self) })
    }

    pub fn eval(&self, p: Vec3) -> f32 {
        match self {
            Sdf::Sphere { radius } => p.length() - radius,
            Sdf::Box { half, round } => {
                let q = p.abs() - (*half - Vec3::splat(*round));
                q.max(Vec3::ZERO).length() + q.max_elem().min(0.0) - round
            }
            Sdf::Torus { major, minor } => (p.x.hypot(p.z) - major).hypot(p.y) - minor,
            Sdf::Cylinder { radius, half_height } => {
                let (dx, dy) = (p.x.hypot(p.z) - radius, p.y.abs() - half_height);
                dx.max(dy).min(0.0) + dx.max(0.0).hypot(dy.max(0.0))
            }
            Sdf::Capsule { a, b, radius } => {
                let (pa, ba) = (p - *a, *b - *a);
                let l2 = ba.length_squared();
                let h = if l2 > 0.0 { (pa.dot(ba) / l2).clamp(0.0, 1.0) } else { 0.0 };
                (pa - ba * h).length() - radius
            }
            Sdf::Transform { xf, scale, child } => child.eval(xf.inv_point(p)) * scale,
            Sdf::Union { children, blend } => {
                children.iter().map(|c| c.eval(p)).reduce(|a, b| smin(a, b, *blend)).unwrap_or(f32::INFINITY)
            }
            Sdf::Intersection { children, blend } => {
                children.iter().map(|c| c.eval(p)).reduce(|a, b| smax(a, b, *blend)).unwrap_or(f32::INFINITY)
            }
            Sdf::Difference { base, cuts, blend } => cuts.iter().fold(base.eval(p), |d, c| smax(d, -c.eval(p), *blend)),
            Sdf::Round { child, radius } => child.eval(p) - radius,
            Sdf::Shell { child, thickness } => child.eval(p).abs() - thickness,
            Sdf::Displace { child, noise, amount } => {
                let d = child.eval(p);
                // The surface lies where |d| <= |amount|. Beyond that the displacement fades
                // out smoothly (keeping the field continuous and its slope bounded), so
                // far-away points skip the noise.
                let a = amount.abs();
                let x = ((d.abs() - a) / a.max(1e-12)).clamp(0.0, 1.0);
                let w = 1.0 - x * x * (3.0 - 2.0 * x);
                if w <= 0.0 { d } else { d - amount * w * noise_term(noise, p) }
            }
        }
    }

    /// Bounds of the solid (where the field may be negative).
    pub fn bounds(&self) -> Aabb {
        match self {
            Sdf::Sphere { radius } => symmetric(Vec3::splat(*radius)),
            Sdf::Box { half, .. } => symmetric(*half),
            Sdf::Torus { major, minor } => symmetric(Vec3::new(major + minor, *minor, major + minor)),
            Sdf::Cylinder { radius, half_height } => symmetric(Vec3::new(*radius, *half_height, *radius)),
            Sdf::Capsule { a, b, radius } => grow(Aabb { min: a.min(*b), max: a.max(*b) }, *radius),
            Sdf::Transform { xf, child, .. } => child.bounds().transformed(xf),
            Sdf::Union { children, blend } => {
                let mut b = Aabb::EMPTY;
                for c in children {
                    b.grow(&c.bounds());
                }
                // The smooth minimum fills the creases between children, never more than
                // `blend` away from them.
                grow(b, *blend)
            }
            Sdf::Intersection { children, .. } => children
                .iter()
                .map(Sdf::bounds)
                .reduce(|a, b| Aabb { min: a.min.max(b.min), max: a.max.min(b.max) })
                .unwrap_or(Aabb::EMPTY),
            Sdf::Difference { base, .. } => base.bounds(),
            Sdf::Round { child, radius } => grow(child.bounds(), *radius),
            Sdf::Shell { child, thickness } => grow(child.bounds(), *thickness),
            Sdf::Displace { child, amount, .. } => grow(child.bounds(), amount.abs()),
        }
    }

    /// True if the Lipschitz bound 1 holds (no displacement anywhere).
    pub fn is_exact(&self) -> bool {
        match self {
            Sdf::Displace { .. } => false,
            Sdf::Transform { child, .. } | Sdf::Round { child, .. } | Sdf::Shell { child, .. } => child.is_exact(),
            Sdf::Union { children, .. } | Sdf::Intersection { children, .. } => children.iter().all(Sdf::is_exact),
            Sdf::Difference { base, cuts, .. } => base.is_exact() && cuts.iter().all(Sdf::is_exact),
            _ => true,
        }
    }
}

// ---------------------------------------------------------------------------- hypertexture

/// Perlin & Hoffert's hypertexture: a soft object's density (1 deep inside, falling to 0
/// over `softness` across the surface of `base`), modulated by noise and shaped by
/// `bias` / `gain`. Its level set at `level` is a surface (eroded, cavity-filled solids)
/// and the density itself a participating medium.
#[derive(Clone, Debug)]
pub struct Hypertexture {
    pub base: Sdf,
    /// Width of the soft transition, in scene units.
    pub softness: f32,
    pub noise: Fractal,
    /// Noise added to the density: `amount` × (noise − ½).
    pub amount: f32,
    pub bias: f32,
    pub gain: f32,
}

/// Perlin's bias: remaps [0, 1] so that ½ goes to `b` (0.5 = identity).
#[inline]
pub fn bias(t: f32, b: f32) -> f32 {
    if t <= 0.0 { 0.0 } else { t.powf(b.ln() / 0.5f32.ln()) }
}

/// Perlin's gain: an S-curve (g > ½) or its inverse (g < ½) through (½, ½).
#[inline]
pub fn gain(t: f32, g: f32) -> f32 {
    if t < 0.5 { 0.5 * bias(2.0 * t, 1.0 - g) } else { 1.0 - 0.5 * bias(2.0 - 2.0 * t, 1.0 - g) }
}

impl Hypertexture {
    /// Density in [0, 1].
    pub fn density(&self, p: Vec3) -> f32 {
        let d0 = 0.5 - self.base.eval(p) / self.softness;
        // The noise term lies within ±`amount`: skip it where it cannot change the result.
        let a = self.amount.abs();
        let x = if d0 + a <= 0.0 {
            0.0
        } else if d0 - a >= 1.0 {
            1.0
        } else {
            (d0 + self.amount * noise_term(&self.noise, p)).clamp(0.0, 1.0)
        };
        gain(bias(x, self.bias), self.gain)
    }

    /// Where the density may be non-zero.
    pub fn bounds(&self) -> Aabb {
        grow(self.base.bounds(), self.softness * (0.5 + self.amount.abs()))
    }
}

// ---------------------------------------------------------------------------- fractals

/// A quaternion Julia set q -> q² + c, cut by a 3D hyperplane of 4D space: a point p of
/// the scene maps to `origin` + p.x `basis[0]` + p.y `basis[1]` + p.z `basis[2]`.
#[derive(Clone, Debug)]
pub struct Julia {
    pub c: [f32; 4],
    pub iterations: u32,
    origin: [f32; 4],
    basis: [[f32; 4]; 3],
}

/// Squared radius beyond which an orbit counts as escaped (a large one makes the distance
/// estimate accurate).
const ESCAPE2: f32 = 1e4;
/// Field value of points whose orbits never escape: inside, close to the surface (the
/// bisection then finds the boundary of the non-escaping region).
const INSIDE: f32 = -1e-6;

fn qmul(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    // (x, y, z) imaginary, w real.
    let [ax, ay, az, aw] = a;
    let [bx, by, bz, bw] = b;
    [
        aw * bx + ax * bw + ay * bz - az * by,
        aw * by - ax * bz + ay * bw + az * bx,
        aw * bz + ax * by - ay * bx + az * bw,
        aw * bw - ax * bx - ay * by - az * bz,
    ]
}

fn norm4(a: [f32; 4]) -> f32 {
    a.iter().map(|v| v * v).sum::<f32>().sqrt()
}

impl Julia {
    /// Quaternions are stored (i, j, k, real). The slice is the hyperplane
    /// {q : q · `normal` = `offset`}; its basis takes the real, i, j and k axes in that order,
    /// so the normal k maps p to (real, i, j) = (p.x, p.y, p.z).
    pub fn new(c: [f32; 4], iterations: u32, normal: [f32; 4], offset: f32) -> Result<Julia, String> {
        let len = norm4(normal);
        if !(len > 0.0 && len.is_finite()) {
            return Err("the slice normal must be a non-zero 4D vector".into());
        }
        let n = normal.map(|v| v / len);
        let dot = |a: [f32; 4], b: [f32; 4]| (0..4).map(|i| a[i] * b[i]).sum::<f32>();
        let mut basis: Vec<[f32; 4]> = Vec::new();
        for axis in [3, 0, 1, 2] {
            let mut v = [0.0f32; 4];
            v[axis] = 1.0;
            for b in std::iter::once(n).chain(basis.iter().copied()) {
                let k = dot(v, b);
                for i in 0..4 {
                    v[i] -= k * b[i];
                }
            }
            let l = norm4(v);
            if l > 1e-3 && basis.len() < 3 {
                basis.push(v.map(|x| x / l));
            }
        }
        Ok(Julia { c, iterations, origin: n.map(|v| v * offset), basis: [basis[0], basis[1], basis[2]] })
    }

    /// Radius beyond which every orbit escapes: |q| > R implies |q² + c| > |q|.
    fn escape_radius(&self) -> f32 {
        f32::midpoint(1.0, (1.0 + 4.0 * norm4(self.c)).sqrt())
    }

    /// The distance estimate. With `smooth`, orbits that never escape get the same
    /// formula instead of the inside value: a field that is continuous across the
    /// surface (for normals), but with false zeros inside the set (not for hit tests).
    fn de(&self, p: Vec3, smooth: bool) -> f32 {
        let mut z: [f32; 4] = std::array::from_fn(|i| {
            self.origin[i] + p.x * self.basis[0][i] + p.y * self.basis[1][i] + p.z * self.basis[2][i]
        });
        // |z'| of the orbit (z' = 2 z z', in norm).
        let mut dz = 1.0f32;
        let mut r2 = z.iter().map(|v| v * v).sum::<f32>();
        for _ in 0..self.iterations {
            if r2 > ESCAPE2 {
                break;
            }
            dz *= 2.0 * r2.sqrt();
            let sq = qmul(z, z);
            z = std::array::from_fn(|i| sq[i] + self.c[i]);
            r2 = z.iter().map(|v| v * v).sum::<f32>();
        }
        // The distance estimate of Hart et al.: ½ |z| ln|z| / |z'|, valid for orbits that
        // escape. One still bounded after all iterations is in the set (as far as the
        // iterations can tell): inside.
        if r2 <= ESCAPE2 && !smooth {
            return INSIDE;
        }
        let r = r2.sqrt();
        0.5 * r * r.ln() / dz
    }

    /// Surface normal from the gradient of `|z_n|` (the potential), with the Jacobian of the
    /// orbit carried along analytically (Hart et al.): smooth where differences of the
    /// distance estimate are not.
    fn normal(&self, p: Vec3) -> Vec3 {
        let mut z: [f32; 4] = std::array::from_fn(|i| {
            self.origin[i] + p.x * self.basis[0][i] + p.y * self.basis[1][i] + p.z * self.basis[2][i]
        });
        // dz/dp along each scene axis; d(z²) = z dz + dz z.
        let mut jac = self.basis;
        for _ in 0..self.iterations {
            if z.iter().map(|v| v * v).sum::<f32>() > ESCAPE2 {
                break;
            }
            for j in &mut jac {
                let (a, b) = (qmul(z, *j), qmul(*j, z));
                *j = std::array::from_fn(|i| a[i] + b[i]);
            }
            let sq = qmul(z, z);
            z = std::array::from_fn(|i| sq[i] + self.c[i]);
        }
        let g = jac.map(|j| (0..4).map(|i| z[i] * j[i]).sum::<f32>());
        Vec3::new(g[0], g[1], g[2])
    }

    fn bounds(&self) -> Aabb {
        // The slice meets the ball |q| <= R in a 3D ball around p = 0.
        let r = self.escape_radius();
        let o2: f32 = self.origin.iter().map(|v| v * v).sum();
        let r3 = (r * r - o2).max(0.0).sqrt();
        if r3 == 0.0 { Aabb::EMPTY } else { symmetric(Vec3::splat(r3)) }
    }
}

/// Escape radius of the Mandelbulb (small: z is raised to high powers).
const BULB_ESCAPE: f32 = 4.0;

/// The Mandelbulb (White & Nylander): the "triplex" power `power` of z, plus p, around +y.
/// See `Julia::de` for `smooth`.
fn mandelbulb_de(p: Vec3, power: f32, iterations: u32, smooth: bool) -> f32 {
    let mut z = p;
    let mut dr = 1.0f32;
    let mut r = z.length();
    for _ in 0..iterations {
        if r > BULB_ESCAPE {
            break;
        }
        if r > 1e-12 {
            let theta = (z.y / r).clamp(-1.0, 1.0).acos() * power;
            let phi = z.z.atan2(z.x) * power;
            dr = r.powf(power - 1.0) * power * dr + 1.0;
            let zr = r.powf(power);
            let (st, ct) = theta.sin_cos();
            let (sp, cp) = phi.sin_cos();
            z = Vec3::new(st * cp, ct, st * sp) * zr + p;
        } else {
            z = p;
        }
        r = z.length();
    }
    if r <= BULB_ESCAPE && !smooth {
        return INSIDE;
    }
    if r > 0.0 { 0.5 * r.ln() * r / dr } else { INSIDE }
}

/// The Menger sponge in [-1, 1]³ after `iterations` subdivisions (Quílez's distance bound).
fn menger_de(p: Vec3, iterations: u32) -> f32 {
    let mut d = Sdf::Box { half: Vec3::ONE, round: 0.0 }.eval(p);
    let mut s = 1.0f32;
    for _ in 0..iterations {
        let a = Vec3::new((p.x * s).rem_euclid(2.0), (p.y * s).rem_euclid(2.0), (p.z * s).rem_euclid(2.0)) - Vec3::ONE;
        s *= 3.0;
        let r = (Vec3::ONE - a.abs() * 3.0).abs();
        let (da, db, dc) = (r.x.max(r.y), r.y.max(r.z), r.z.max(r.x));
        let c = (da.min(db).min(dc) - 1.0) / s;
        d = d.max(c);
    }
    d
}

// ---------------------------------------------------------------------------- implicit surfaces

#[derive(Clone, Debug)]
pub enum Field {
    Sdf(Sdf),
    /// The surface where the hypertexture density reaches `level`.
    Hypertexture {
        h: Hypertexture,
        level: f32,
    },
    Julia(Julia),
    Mandelbulb {
        power: f32,
        iterations: u32,
    },
    Menger {
        iterations: u32,
    },
    /// Another field under an affine transform (distances × its smallest scale).
    Transformed {
        xf: Transform,
        scale: f32,
        child: Box<Field>,
    },
}

impl Field {
    pub fn eval(&self, p: Vec3) -> f32 {
        match self {
            Field::Sdf(s) => s.eval(p),
            Field::Hypertexture { h, level } => level - h.density(p),
            Field::Julia(j) => j.de(p, false),
            Field::Mandelbulb { power, iterations } => mandelbulb_de(p, *power, *iterations, false),
            Field::Menger { iterations } => menger_de(p, *iterations),
            Field::Transformed { xf, scale, child } => child.eval(xf.inv_point(p)) * scale,
        }
    }

    /// The outward normal direction (not normalized) where it is known in closed form.
    fn analytic_normal(&self, p: Vec3) -> Option<Vec3> {
        match self {
            Field::Julia(j) => Some(j.normal(p)),
            Field::Transformed { xf, child, .. } => child.analytic_normal(xf.inv_point(p)).map(|n| xf.covector(n)),
            _ => None,
        }
    }

    /// A field with the same surface that is smooth across it, for gradients: fractals
    /// would otherwise be flat inside, which quantizes difference normals.
    fn eval_smooth(&self, p: Vec3) -> f32 {
        match self {
            Field::Julia(j) => j.de(p, true),
            Field::Mandelbulb { power, iterations } => mandelbulb_de(p, *power, *iterations, true),
            Field::Transformed { xf, scale, child } => child.eval_smooth(xf.inv_point(p)) * scale,
            _ => self.eval(p),
        }
    }

    pub fn bounds(&self) -> Aabb {
        match self {
            Field::Sdf(s) => s.bounds(),
            Field::Hypertexture { h, .. } => h.bounds(),
            Field::Julia(j) => j.bounds(),
            // Orbits beyond 2^(1/(power-1)) escape; pad for the bulbs reaching out.
            Field::Mandelbulb { power, .. } => symmetric(Vec3::splat(1.2 * 2f32.powf(1.0 / (power - 1.0).max(1.0)))),
            Field::Menger { .. } => symmetric(Vec3::ONE),
            Field::Transformed { xf, child, .. } => child.bounds().transformed(xf),
        }
    }

    /// True if the Lipschitz bound 1 holds (distance estimators count as exact).
    fn is_exact(&self) -> bool {
        match self {
            Field::Sdf(s) => s.is_exact(),
            Field::Hypertexture { .. } => false,
            Field::Transformed { child, .. } => child.is_exact(),
            Field::Julia(_) | Field::Mandelbulb { .. } | Field::Menger { .. } => true,
        }
    }

    /// Fractals with fuzzy surfaces: their hit tolerance sets the detail shown.
    fn is_fractal(&self) -> bool {
        match self {
            Field::Julia(_) | Field::Mandelbulb { .. } => true,
            Field::Transformed { child, .. } => child.is_fractal(),
            _ => false,
        }
    }
}

/// User settings of an implicit surface (None = automatic).
#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    /// Hit tolerance in object units.
    pub epsilon: Option<f32>,
    /// Lipschitz bound of the field (steps are f / L).
    pub lipschitz: Option<f32>,
    pub max_steps: Option<u32>,
}

#[derive(Debug)]
pub struct Implicit {
    field: Field,
    bounds: Aabb,
    /// 1 / Lipschitz bound.
    step: f32,
    eps: f32,
    max_steps: u32,
}

/// Steps a ray leaving the surface may take to get out of the tolerance shell.
const SHELL_STEPS: u32 = 16;
/// Bisection steps refining an overshoot.
const BISECT_STEPS: u32 = 16;

/// Smallest scale of an affine transform's linear part (a lower bound when it skews).
pub fn min_scale(xf: &Transform) -> f32 {
    let c = [xf.m.col(0), xf.m.col(1), xf.m.col(2)];
    let l = c.map(Vec3::length);
    let tol = 1e-4 * l[0] * l[1].max(l[2]);
    if c[0].dot(c[1]).abs() <= tol && c[1].dot(c[2]).abs() <= tol && c[0].dot(c[2]).abs() <= tol {
        return l[0].min(l[1]).min(l[2]);
    }
    // 1 / |M^-1|_F <= the smallest singular value.
    let inv =
        [xf.inv_vector(Vec3::new(1.0, 0.0, 0.0)), xf.inv_vector(Vec3::new(0.0, 1.0, 0.0)), xf.inv_vector(Vec3::Z)];
    1.0 / inv.iter().map(|v| v.length_squared()).sum::<f32>().sqrt()
}

impl Implicit {
    pub fn new(field: Field, opts: &Options) -> Result<Implicit, String> {
        let bounds = field.bounds();
        if bounds.is_empty() || !(bounds.min.is_finite() && bounds.max.is_finite()) {
            return Err("the implicit surface is empty (or unbounded)".into());
        }
        let extent = (bounds.max - bounds.min).max_elem();
        // Exact distance fields resolve fine detail cheaply; noise and fractal fields are
        // only approximately distances, and their detail is set by the tolerance.
        let rough = field.is_fractal() || !field.is_exact();
        let eps = opts.epsilon.unwrap_or(extent * if rough { 2e-4 } else { 2e-5 });
        if !(eps > 0.0 && eps.is_finite()) {
            return Err("'epsilon' must be positive".into());
        }
        let lipschitz = match opts.lipschitz {
            Some(l) if l > 0.0 && l.is_finite() => l,
            Some(_) => return Err("'lipschitz' must be positive".into()),
            None if field.is_exact() => 1.0,
            None => estimate_lipschitz(&field, &bounds),
        };
        let max_steps = opts.max_steps.unwrap_or(if field.is_fractal() { 1024 } else { 512 }).max(1);
        Ok(Implicit { field, bounds, step: 1.0 / lipschitz, eps, max_steps })
    }

    /// The same surface under `xf` (for transforms inside instanced prototypes).
    pub fn transformed(&self, xf: &Transform) -> Result<Implicit, String> {
        let field = Field::Transformed { xf: *xf, scale: min_scale(xf), child: Box::new(self.field.clone()) };
        Implicit::new(
            field,
            &Options {
                epsilon: Some(self.eps * min_scale(xf)),
                lipschitz: Some(1.0 / self.step),
                max_steps: Some(self.max_steps),
            },
        )
    }

    pub fn bounds(&self) -> Aabb {
        self.bounds
    }

    #[cfg(test)]
    pub fn epsilon(&self) -> f32 {
        self.eps
    }

    #[cfg(test)]
    pub fn lipschitz(&self) -> f32 {
        1.0 / self.step
    }

    #[inline]
    pub fn eval(&self, p: Vec3) -> f32 {
        self.field.eval(p)
    }

    /// The field gradient by tetrahedral differences (scaled by 4 `eps`, not normalized).
    fn gradient(&self, p: Vec3) -> Vec3 {
        let h = self.eps;
        let k = [Vec3::new(1.0, -1.0, -1.0), Vec3::new(-1.0, -1.0, 1.0), Vec3::new(-1.0, 1.0, -1.0), Vec3::ONE];
        k.iter().fold(Vec3::ZERO, |g, &k| g + k * self.field.eval_smooth(p + k * h))
    }

    /// Outward unit normal at (or near) the surface point `p`.
    pub fn normal(&self, p: Vec3) -> Vec3 {
        let g = self.field.analytic_normal(p).filter(|g| g.is_finite()).unwrap_or_else(|| self.gradient(p));
        let l = g.length();
        if l > 0.0 && l.is_finite() {
            return g * (1.0 / l);
        }
        let c = p - self.bounds.centroid();
        if c.length_squared() > 0.0 { c.normalize() } else { Vec3::Z }
    }

    /// Distance to the first surface crossing along the ray (`d` unit length) in (0, `t_max`).
    pub fn intersect(&self, o: Vec3, d: Vec3, t_max: f32) -> Option<f32> {
        let inv = d.recip();
        let (a, b) = ((self.bounds.min - o) * inv, (self.bounds.max - o) * inv);
        let (near, far) = (a.min(b), a.max(b));
        let mut t = near.x.max(near.y).max(near.z).max(0.0);
        let t_end = far.x.min(far.y).min(far.z).min(t_max);
        if t > t_end || t_end.is_nan() {
            return None;
        }
        let eps = self.eps;
        let at = |t: f32| self.eval(o + d * t);
        let mut f = at(t);
        if t > 0.0 {
            // Entering the bounds from outside: outside the solid, even where the surface
            // touches the bounds (a box, the Menger sponge).
            if f < eps {
                return Some(t);
            }
        }
        // Which side the ray is on: +1 outside (looking for f < 0), -1 inside.
        let mut side = if f < 0.0 && t == 0.0 { -1.0 } else { 1.0 };
        if t == 0.0 && f.abs() < eps {
            // Leaving a surface: the side ahead is where the field is heading.
            side = if self.gradient(o + d * t).dot(d) >= 0.0 { 1.0 } else { -1.0 };
            for _ in 0..SHELL_STEPS {
                if side * f >= eps {
                    break;
                }
                t += eps;
                if t > t_end {
                    return None;
                }
                f = at(t);
            }
        }
        let mut t_prev = t;
        for _ in 0..self.max_steps {
            let v = side * f;
            if v < 0.0 {
                return Some(bisect(&at, side, t_prev, t));
            }
            if v < eps {
                return (t > 0.0).then_some(t);
            }
            t_prev = t;
            t += (v * self.step).max(0.5 * eps);
            if t >= t_end {
                // A last look at the end of the segment: the step may have jumped a crossing.
                let v_end = side * at(t_end);
                return (v_end < 0.0 && t_end > t_prev).then(|| bisect(&at, side, t_prev, t_end));
            }
            f = at(t);
        }
        None
    }
}

/// Refines a crossing between `lo` (on the ray's side) and `hi` (past the surface);
/// returns a point still on the ray's side.
fn bisect(at: &impl Fn(f32) -> f32, side: f32, mut lo: f32, mut hi: f32) -> f32 {
    for _ in 0..BISECT_STEPS {
        let mid = f32::midpoint(lo, hi);
        if side * at(mid) < 0.0 {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    lo
}

/// A Lipschitz bound for a field without a known one: the largest gradient found at
/// random points of its bounds, with a margin.
fn estimate_lipschitz(field: &Field, bounds: &Aabb) -> f32 {
    let mut rng = crate::rng::Rng::new(0x11b5, 3);
    let size = bounds.max - bounds.min;
    let h = 1e-4 * size.max_elem();
    let mut max = 0.0f32;
    for _ in 0..4096 {
        let p = bounds.min + Vec3::new(rng.f32() * size.x, rng.f32() * size.y, rng.f32() * size.z);
        let d = |e: Vec3| (field.eval(p + e * h) - field.eval(p - e * h)) / (2.0 * h);
        let g = Vec3::new(d(Vec3::new(1.0, 0.0, 0.0)), d(Vec3::new(0.0, 1.0, 0.0)), d(Vec3::Z));
        if g.is_finite() {
            max = max.max(g.length());
        }
    }
    (max * 1.5).max(1e-3)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::noise::FractalKind;
    use crate::rng::Rng;

    fn rand_dir(rng: &mut Rng) -> Vec3 {
        crate::sampling::uniform_sphere(rng.f32(), rng.f32())
    }

    fn primitives() -> Vec<Sdf> {
        vec![
            Sdf::Sphere { radius: 0.8 },
            Sdf::Box { half: Vec3::new(0.5, 0.7, 0.3), round: 0.1 },
            Sdf::Torus { major: 0.7, minor: 0.2 },
            Sdf::Cylinder { radius: 0.4, half_height: 0.6 },
            Sdf::Capsule { a: Vec3::new(-0.5, 0.0, 0.0), b: Vec3::new(0.3, 0.4, 0.0), radius: 0.25 },
        ]
    }

    /// Exact distance fields: 1-Lipschitz, with bounds holding the solid; CSG and
    /// transforms keep both properties.
    #[test]
    fn distance_fields_are_lipschitz_and_bounded() {
        let mut rng = Rng::new(1, 2);
        let mut fields = primitives();
        let trs = Trs {
            translate: Vec3::new(0.2, -0.1, 0.3),
            rotate: Vec3::new(30.0, 10.0, -50.0),
            scale: Vec3::new(1.5, 0.7, 1.0),
        };
        fields.push(Sdf::Sphere { radius: 0.5 }.transformed(&trs).unwrap());
        fields.push(Sdf::Union { children: primitives(), blend: 0.3 });
        fields.push(Sdf::Intersection {
            children: vec![Sdf::Sphere { radius: 0.8 }, Sdf::Box { half: Vec3::splat(0.6), round: 0.0 }],
            blend: 0.1,
        });
        fields.push(Sdf::Difference {
            base: Box::new(Sdf::Box { half: Vec3::splat(0.6), round: 0.0 }),
            cuts: vec![Sdf::Sphere { radius: 0.75 }],
            blend: 0.05,
        });
        fields.push(Sdf::Shell { child: Box::new(Sdf::Sphere { radius: 0.8 }), thickness: 0.05 });
        for f in &fields {
            assert!(f.is_exact());
            let b = f.bounds();
            for _ in 0..3000 {
                let p = Vec3::new(rng.f32(), rng.f32(), rng.f32()) * 4.0 - Vec3::splat(2.0);
                let q = p + rand_dir(&mut rng) * (rng.f32() * 0.3);
                let (fp, fq) = (f.eval(p), f.eval(q));
                assert!((fp - fq).abs() <= (p - q).length() * 1.0001 + 1e-5, "{f:?}: not 1-Lipschitz");
                if fp < 0.0 {
                    let inside = p.x >= b.min.x
                        && p.y >= b.min.y
                        && p.z >= b.min.z
                        && p.x <= b.max.x
                        && p.y <= b.max.y
                        && p.z <= b.max.z;
                    assert!(inside, "{f:?}: solid point {p:?} outside bounds {b:?}");
                }
            }
        }
        // Exact values.
        assert!((Sdf::Torus { major: 1.0, minor: 0.25 }.eval(Vec3::new(1.0, 0.5, 0.0)) - 0.25).abs() < 1e-6);
        assert!((Sdf::Box { half: Vec3::ONE, round: 0.0 }.eval(Vec3::new(2.0, 2.0, 0.0)) - 2f32.sqrt()).abs() < 1e-6);
        assert!(smin(0.0, 0.0, 0.4) < 0.0 && smin(0.0, 1.0, 0.4) == 0.0);
    }

    /// Sphere tracing finds a sphere where the analytic solution is, from outside and from
    /// inside, and rays leaving the surface (both ways) do not hit it again at once.
    #[test]
    fn sphere_tracing_hits_and_spawns() {
        let im = Implicit::new(Field::Sdf(Sdf::Sphere { radius: 1.0 }), &Options::default()).unwrap();
        let mut rng = Rng::new(7, 7);
        for _ in 0..2000 {
            let o = rand_dir(&mut rng) * (1.5 + 3.0 * rng.f32());
            let target = rand_dir(&mut rng) * (0.9 * rng.f32());
            let d = (target - o).normalize();
            let (t0, t1) = crate::geometry::sphere_roots(o, d, Vec3::ZERO, 1.0).unwrap();
            let t = im.intersect(o, d, f32::INFINITY).expect("hits the sphere");
            assert!((t - t0).abs() < 1e-4, "outside: {t} vs {t0}");
            // Continue from the hit point, straight on (into the sphere): finds the far side.
            let p = o + d * t;
            let n = im.normal(p);
            assert!(n.dot(p.normalize()) > 0.9999);
            let p_in = crate::math::offset_origin(p, n, d);
            let t2 = im.intersect(p_in, d, f32::INFINITY).expect("hits the far side from inside");
            assert!((t2 - (t1 - t0)).abs() < 1e-3, "inside: {t2} vs {}", t1 - t0);
            // Reflected away: no hit.
            let r = d - n * (2.0 * d.dot(n));
            assert!(im.intersect(crate::math::offset_origin(p, n, r), r, f32::INFINITY).is_none());
            // Starting exactly on the surface (no offset), both directions still behave.
            let on = n;
            assert!(im.intersect(on, n, f32::INFINITY).is_none(), "leaving outwards re-hit the surface");
            let t3 = im.intersect(on, -n, f32::INFINITY).expect("inwards from the surface");
            assert!((t3 - 2.0).abs() < 1e-3, "{t3}");
        }
        assert!(im.intersect(Vec3::new(0.0, 0.0, 5.0), Vec3::Z, f32::INFINITY).is_none());
        assert!(im.intersect(Vec3::new(0.0, 0.0, 5.0), -Vec3::Z, 3.5).is_none(), "beyond t_max");
    }

    /// A displaced surface with an estimated Lipschitz bound: every hit lies on the surface,
    /// and rays towards points known to be inside never miss.
    #[test]
    fn displaced_surface_is_found() {
        let noise = Fractal { scale: 3.0, octaves: 4.0, ..Fractal::new(FractalKind::Ridged) };
        let sdf = Sdf::Displace { child: Box::new(Sdf::Sphere { radius: 1.0 }), noise, amount: 0.3 };
        let im = Implicit::new(Field::Sdf(sdf), &Options::default()).unwrap();
        assert!(im.lipschitz() > 1.0, "estimated bound {}", im.lipschitz());
        let mut rng = Rng::new(3, 4);
        for _ in 0..1000 {
            let o = rand_dir(&mut rng) * 3.0;
            let target = rand_dir(&mut rng) * (0.5 * rng.f32());
            assert!(im.eval(target) < 0.0);
            let d = (target - o).normalize();
            let t = im.intersect(o, d, f32::INFINITY).expect("a ray to an inside point hits");
            assert!(im.eval(o + d * t).abs() < 2.0 * im.epsilon() * im.lipschitz());
            assert!(t < (target - o).length());
        }
    }

    #[test]
    fn bias_and_gain() {
        for t in [0.0, 0.1, 0.5, 0.9, 1.0] {
            assert!((bias(t, 0.5) - t).abs() < 1e-6 && (gain(t, 0.5) - t).abs() < 1e-6);
        }
        assert!((bias(0.5, 0.8) - 0.8).abs() < 1e-5);
        assert!((gain(0.5, 0.9) - 0.5).abs() < 1e-6 && gain(0.25, 0.9) < 0.25 && gain(0.75, 0.9) > 0.75);
        let h = Hypertexture {
            base: Sdf::Sphere { radius: 1.0 },
            softness: 0.4,
            noise: Fractal::new(FractalKind::Turbulence),
            amount: 0.3,
            bias: 0.4,
            gain: 0.7,
        };
        assert_eq!(h.density(Vec3::ZERO), 1.0);
        assert_eq!(h.density(Vec3::new(3.0, 0.0, 0.0)), 0.0);
        let b = h.bounds();
        let mut rng = Rng::new(5, 5);
        for _ in 0..5000 {
            let p = Vec3::new(rng.f32(), rng.f32(), rng.f32()) * 4.0 - Vec3::splat(2.0);
            let dens = h.density(p);
            assert!((0.0..=1.0).contains(&dens));
            let inside = p.x >= b.min.x
                && p.y >= b.min.y
                && p.z >= b.min.z
                && p.x <= b.max.x
                && p.y <= b.max.y
                && p.z <= b.max.z;
            assert!(dens == 0.0 || inside);
        }
    }

    /// The fractals: rays through the centre hit them inside their bounds, and the
    /// distance estimate is a safe step (no surface point found closer than it claims).
    #[test]
    fn fractals_are_hit_and_estimates_are_safe() {
        let julia = Julia::new([0.2, 0.0, 0.0, -1.0], 10, [0.0, 0.0, 1.0, 0.0], 0.0).unwrap();
        let fields =
            vec![Field::Julia(julia), Field::Mandelbulb { power: 8.0, iterations: 8 }, Field::Menger { iterations: 3 }];
        let mut rng = Rng::new(2, 9);
        for f in fields {
            let name = format!("{:?}", std::mem::discriminant(&f));
            let im = Implicit::new(f, &Options::default()).unwrap();
            let b = im.bounds();
            let mut hits = 0;
            let mut surface = Vec::new();
            for _ in 0..300 {
                let o = rand_dir(&mut rng) * 4.0;
                let d = (rand_dir(&mut rng) * 0.2 - o).normalize();
                if let Some(t) = im.intersect(o, d, f32::INFINITY) {
                    hits += 1;
                    let p = o + d * t;
                    // On the surface: within tolerance, or at the edge of the (iteration-
                    // limited) set found by bisection, where the estimate to the true set
                    // is still positive but the field turns negative just beyond.
                    let ahead = im.eval(o + d * (t + 4.0 * im.epsilon()));
                    assert!(im.eval(p) < 2.0 * im.epsilon() || ahead < 0.0, "{name}: hit off the surface");
                    assert!(p.x >= b.min.x - 1e-3 && p.x <= b.max.x + 1e-3, "{name}: hit outside the bounds");
                    surface.push(p);
                }
            }
            assert!(hits > 100, "{name}: only {hits} of 300 rays towards the centre hit");
            // The estimate at random points never exceeds the distance to surface points.
            for _ in 0..2000 {
                let p =
                    b.min + (b.max - b.min) * Vec3::new(rng.f32(), rng.f32(), rng.f32()) * 1.4 - (b.max - b.min) * 0.2;
                let de = im.eval(p);
                let nearest = surface.iter().map(|s| (*s - p).length()).fold(f32::INFINITY, f32::min);
                assert!(de <= nearest + 2.0 * im.epsilon(), "{name}: estimate {de} > distance {nearest} to a hit");
            }
        }
        assert!(Julia::new([0.0; 4], 8, [0.0; 4], 0.0).is_err());
    }

    #[test]
    fn slices_and_transforms() {
        // The k slice maps (x, y, z) to (real, i, j) = (x, y, z) and k = offset.
        let j = Julia::new([0.1, 0.2, 0.3, 0.4], 4, [0.0, 0.0, 1.0, 0.0], 0.25).unwrap();
        assert_eq!(j.basis, [[0.0, 0.0, 0.0, 1.0], [1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]]);
        assert_eq!(j.origin, [0.0, 0.0, 0.25, 0.0]);
        // A tilted slice has an orthonormal basis orthogonal to its normal.
        let n = [0.3, -0.2, 0.5, 0.8];
        let j = Julia::new([0.1, 0.2, 0.3, 0.4], 4, n, 0.1).unwrap();
        for (i, a) in j.basis.iter().enumerate() {
            assert!((0..4).map(|k| a[k] * n[k]).sum::<f32>().abs() < 1e-5);
            for b in &j.basis[i + 1..] {
                assert!((0..4).map(|k| a[k] * b[k]).sum::<f32>().abs() < 1e-5);
            }
        }
        // A transformed implicit is the original moved.
        let im = Implicit::new(Field::Sdf(Sdf::Torus { major: 0.6, minor: 0.2 }), &Options::default()).unwrap();
        let trs =
            Trs { translate: Vec3::new(1.0, 2.0, 3.0), rotate: Vec3::new(90.0, 0.0, 0.0), scale: Vec3::splat(2.0) };
        let xf = trs.transform().unwrap();
        let moved = im.transformed(&xf).unwrap();
        assert!((min_scale(&xf) - 2.0).abs() < 1e-5);
        let p = Vec3::new(0.8, 0.0, 0.0);
        assert!(moved.eval(xf.point(p)).abs() < 1e-5);
        assert!((moved.eval(xf.point(Vec3::ZERO)) - 2.0 * im.eval(Vec3::ZERO)).abs() < 1e-5);
    }
}
