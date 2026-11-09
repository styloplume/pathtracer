//! Pixel reconstruction filters, applied by filter importance sampling: each camera
//! sample's offset from the pixel center is drawn from the (separable) filter itself,
//! so every sample has weight 1 and stays inside its own pixel's accumulator. That
//! keeps rows independent for the threaded renderer and works with the progressive
//! preview, at the cost of slightly more noise than splatting.

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FilterKind {
    Box,
    Tent,
    Gaussian,
    BlackmanHarris,
}

impl FilterKind {
    pub fn parse(s: &str) -> Option<FilterKind> {
        match s.to_ascii_lowercase().replace(['_', ' '], "-").as_str() {
            "box" => Some(FilterKind::Box),
            "tent" | "triangle" => Some(FilterKind::Tent),
            "gaussian" | "gauss" => Some(FilterKind::Gaussian),
            "blackman-harris" | "blackmanharris" | "bh" => Some(FilterKind::BlackmanHarris),
            _ => None,
        }
    }

    /// Radius in pixels used when the scene does not give one.
    pub fn default_radius(self) -> f32 {
        match self {
            FilterKind::Box => 0.5,
            FilterKind::Tent => 1.0,
            FilterKind::Gaussian | FilterKind::BlackmanHarris => 1.5,
        }
    }

    /// 1D filter profile on [-radius, radius]; non-negative everywhere.
    fn eval(self, x: f32, radius: f32) -> f32 {
        let r = radius;
        match self {
            FilterKind::Box => 1.0,
            FilterKind::Tent => (1.0 - x.abs() / r).max(0.0),
            FilterKind::Gaussian => {
                // pbrt-style: sigma = r / 3, shifted so the filter reaches 0 at the radius.
                let s2 = 2.0 * (r / 3.0) * (r / 3.0);
                ((-x * x / s2).exp() - (-r * r / s2).exp()).max(0.0)
            }
            FilterKind::BlackmanHarris => {
                let t = 2.0 * std::f32::consts::PI * (x + r) / (2.0 * r);
                (0.35875 - 0.48829 * t.cos() + 0.14128 * (2.0 * t).cos() - 0.01168 * (3.0 * t).cos()).max(0.0)
            }
        }
    }
}

const TABLE: usize = 1024;

/// Tabulated inverse CDF of a 1D filter profile.
pub struct Filter {
    icdf: Vec<f32>,
}

impl Filter {
    pub fn new(kind: FilterKind, radius: f32) -> Filter {
        let r = radius.max(1e-3);
        const N: usize = 4096;
        let dx = 2.0 * r / N as f32;
        let x = |i: usize| -r + i as f32 * dx;
        let mut cdf = vec![0.0f64; N + 1];
        for i in 0..N {
            cdf[i + 1] = cdf[i] + 0.5 * (kind.eval(x(i), r) + kind.eval(x(i + 1), r)) as f64 * dx as f64;
        }
        let total = cdf[N];
        let mut icdf = Vec::with_capacity(TABLE);
        let mut i = 0;
        for j in 0..TABLE {
            let u = j as f64 / (TABLE - 1) as f64 * total;
            while i < N - 1 && cdf[i + 1] < u {
                i += 1;
            }
            let span = cdf[i + 1] - cdf[i];
            let t = if span > 0.0 { ((u - cdf[i]) / span).clamp(0.0, 1.0) } else { 0.0 };
            icdf.push(x(i) + t as f32 * dx);
        }
        Filter { icdf }
    }

    /// Maps a uniform `u` in [0, 1) to an offset from the pixel center, in pixels.
    #[inline]
    pub fn sample(&self, u: f32) -> f32 {
        let f = u * (TABLE - 1) as f32;
        let i = (f as usize).min(TABLE - 2);
        let t = f - i as f32;
        self.icdf[i] + (self.icdf[i + 1] - self.icdf[i]) * t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moments(kind: FilterKind, r: f32) -> (f32, f32, f32) {
        let f = Filter::new(kind, r);
        let n = 100_000;
        let (mut mean, mut var, mut maxabs) = (0.0f64, 0.0f64, 0.0f32);
        for i in 0..n {
            let x = f.sample((i as f32 + 0.5) / n as f32);
            mean += x as f64;
            var += (x * x) as f64;
            maxabs = maxabs.max(x.abs());
        }
        ((mean / n as f64) as f32, (var / n as f64) as f32, maxabs)
    }

    #[test]
    fn sampled_distributions_match_filters() {
        let (m, v, a) = moments(FilterKind::Box, 0.5);
        assert!(m.abs() < 1e-3 && (v - 0.25 / 3.0).abs() < 1e-3 && a <= 0.5);
        let (m, v, a) = moments(FilterKind::Tent, 1.0);
        assert!(m.abs() < 1e-3 && (v - 1.0 / 6.0).abs() < 2e-3 && a <= 1.0);
        for kind in [FilterKind::Gaussian, FilterKind::BlackmanHarris] {
            let (m, v, a) = moments(kind, 1.5);
            assert!(m.abs() < 1e-3 && a <= 1.5, "{kind:?}");
            // Both are much more concentrated than a box of the same radius.
            assert!(v < 1.5 * 1.5 / 3.0 * 0.5, "{kind:?} variance {v}");
        }
    }
}
