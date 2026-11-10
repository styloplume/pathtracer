//! Path guiding with an SD-tree ("Practical Path Guiding for Efficient Light-Transport
//! Simulation", Müller, Gross & Novák 2017).
//!
//! A spatial binary tree over the scene bounds holds, in each leaf, a quadtree over the
//! sphere of directions (cylindrical coordinates `(cos theta, phi)` mapped to the unit
//! square, which preserves area). Paths deposit their incident radiance estimates
//! (`radiance / pdf`) into the *building* trees; at the end of each training iteration
//! the spatial tree splits leaves that received many records, the quadtrees are refined
//! where they hold more than 1% of the energy, and the collected statistics become the
//! *sampling* distribution for the next iteration. Iterations double in length (1, 2, 4,
//! ... samples per pixel), so later, better distributions get the most samples.
//!
//! The integrator samples directions from a 50/50 mixture of the BSDF and the guiding
//! distribution (one-sample MIS with the mixture pdf), which keeps it robust where the
//! learned distribution is poor. This helps most where unguided paths rarely find the
//! light: indoor scenes lit through openings, and caustics seen on diffuse surfaces.

use crate::geometry::Aabb;
use crate::math::{PI, Vec3};
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

/// Probability of sampling the BSDF rather than the guiding distribution.
pub const BSDF_FRACTION: f32 = 0.5;
/// Quadtree nodes holding more than this fraction of the energy are subdivided.
const DTREE_THRESHOLD: f32 = 0.01;
const DTREE_MAX_DEPTH: u32 = 20;
/// Spatial leaves split after `c * sqrt(spp of the iteration)` records.
const STREE_THRESHOLD: f32 = 12_000.0;
const STREE_MAX_DEPTH: u32 = 48;

/// An f32 that threads can add to concurrently.
#[derive(Default)]
struct AtomicF32(AtomicU32);

impl AtomicF32 {
    #[inline]
    fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Relaxed))
    }
    #[inline]
    fn add(&self, v: f32) {
        let _ = self.0.try_update(Relaxed, Relaxed, |b| Some((f32::from_bits(b) + v).to_bits()));
    }
}

/// A quadtree node: the energy in each quadrant and the child subdividing it (0 = leaf).
/// Quadrant `i` has `x >= 1/2` if `i & 1` and `y >= 1/2` if `i & 2`.
#[derive(Default)]
struct QNode {
    sum: [AtomicF32; 4],
    child: [u32; 4],
}

impl QNode {
    #[inline]
    fn sums(&self) -> [f32; 4] {
        std::array::from_fn(|i| self.sum[i].get())
    }
}

/// Directional distribution: a quadtree over the unit square of direction coordinates.
struct DTree {
    nodes: Vec<QNode>,
}

impl Clone for DTree {
    fn clone(&self) -> DTree {
        let nodes = self
            .nodes
            .iter()
            .map(|n| QNode {
                sum: std::array::from_fn(|i| AtomicF32(AtomicU32::new(n.sum[i].0.load(Relaxed)))),
                child: n.child,
            })
            .collect();
        DTree { nodes }
    }
}

#[inline]
fn quadrant(p: &mut [f32; 2]) -> usize {
    let mut i = 0;
    for (k, v) in p.iter_mut().enumerate() {
        *v *= 2.0;
        if *v >= 1.0 {
            *v -= 1.0;
            i |= 1 << k;
        }
    }
    i
}

impl DTree {
    fn new() -> DTree {
        DTree { nodes: vec![QNode::default()] }
    }

    fn total(&self) -> f32 {
        self.nodes[0].sums().iter().sum()
    }

    fn record(&self, mut p: [f32; 2], v: f32) {
        let mut n = 0;
        loop {
            let i = quadrant(&mut p);
            let node = &self.nodes[n];
            node.sum[i].add(v);
            if node.child[i] == 0 {
                return;
            }
            n = node.child[i] as usize;
        }
    }

    /// Density over the unit square (integrates to 1).
    fn pdf(&self, mut p: [f32; 2]) -> f32 {
        let (mut n, mut pdf) = (0, 1.0);
        loop {
            let s = self.nodes[n].sums();
            let total: f32 = s.iter().sum();
            if total <= 0.0 {
                return pdf;
            }
            let i = quadrant(&mut p);
            pdf *= 4.0 * s[i] / total;
            let c = self.nodes[n].child[i];
            if c == 0 || pdf <= 0.0 {
                return pdf;
            }
            n = c as usize;
        }
    }

    /// Samples a point of the unit square proportionally to the stored energy.
    fn sample(&self, (mut u0, mut u1): (f32, f32)) -> [f32; 2] {
        let (mut n, mut origin, mut size) = (0, [0.0f32; 2], 1.0f32);
        loop {
            let s = self.nodes[n].sums();
            let total: f32 = s.iter().sum();
            if total <= 0.0 {
                break;
            }
            // Pick the column with u0, then the row within it with u1.
            let left = s[0] + s[2];
            let mut i = 0;
            if u0 * total < left {
                u0 = (u0 * total / left).min(1.0 - f32::EPSILON);
            } else {
                u0 = ((u0 * total - left) / (total - left)).clamp(0.0, 1.0 - f32::EPSILON);
                i |= 1;
            }
            let (bottom, col) = (s[i], s[i] + s[i | 2]);
            if u1 * col < bottom {
                u1 = (u1 * col / bottom).min(1.0 - f32::EPSILON);
            } else {
                u1 = ((u1 * col - bottom) / (col - bottom)).clamp(0.0, 1.0 - f32::EPSILON);
                i |= 2;
            }
            size *= 0.5;
            origin[0] += if i & 1 != 0 { size } else { 0.0 };
            origin[1] += if i & 2 != 0 { size } else { 0.0 };
            let c = self.nodes[n].child[i];
            if c == 0 {
                break;
            }
            n = c as usize;
        }
        [origin[0] + u0 * size, origin[1] + u1 * size]
    }

    /// A fresh (empty) tree, subdivided where this one holds more than the threshold
    /// fraction of the energy; quadrants this tree does not subdivide are assumed uniform.
    fn refined(&self) -> DTree {
        let total = self.total();
        let mut out = DTree { nodes: vec![QNode::default()] };
        if total <= 0.0 {
            return out;
        }
        // (old node or None if beyond the old tree, its quadrant energies, new node, depth)
        let mut stack = vec![(Some(0usize), self.nodes[0].sums(), 0usize, 1u32)];
        while let Some((old, energy, new, depth)) = stack.pop() {
            for (i, &e) in energy.iter().enumerate() {
                if depth >= DTREE_MAX_DEPTH || e <= total * DTREE_THRESHOLD {
                    continue;
                }
                let old_child = old.map(|o| self.nodes[o].child[i]).filter(|&c| c != 0).map(|c| c as usize);
                let child_energy = old_child.map_or([e / 4.0; 4], |c| self.nodes[c].sums());
                let idx = out.nodes.len();
                out.nodes.push(QNode::default());
                out.nodes[new].child[i] = idx as u32;
                stack.push((old_child, child_energy, idx, depth + 1));
            }
        }
        out
    }
}

struct Wrapper {
    sampling: DTree,
    building: DTree,
    records: AtomicU32,
}

impl Clone for Wrapper {
    fn clone(&self) -> Wrapper {
        Wrapper {
            sampling: self.sampling.clone(),
            building: self.building.clone(),
            records: AtomicU32::new(self.records.load(Relaxed)),
        }
    }
}

#[derive(Clone, Copy)]
struct SNode {
    axis: u8,
    /// Index of the first of two children; 0 for a leaf.
    children: u32,
    /// The leaf's directional distributions.
    wrapper: u32,
}

/// The SD-tree. During a render pass it is shared read-only between threads (records are
/// atomic additions); `end_iteration` restructures it between passes.
pub struct Guide {
    min: Vec3,
    size: f32,
    nodes: Vec<SNode>,
    wrappers: Vec<Wrapper>,
    can_sample: bool,
    recording: bool,
}

/// Maps a direction to the unit square (equal-area cylindrical projection).
#[inline]
fn dir_to_square(d: Vec3) -> [f32; 2] {
    let mut phi = d.y.atan2(d.x);
    if phi < 0.0 {
        phi += 2.0 * PI;
    }
    [f32::midpoint(d.z.clamp(-1.0, 1.0), 1.0).min(1.0 - f32::EPSILON), (phi / (2.0 * PI)).min(1.0 - f32::EPSILON)]
}

#[inline]
fn square_to_dir(p: [f32; 2]) -> Vec3 {
    let cos = 2.0 * p[0] - 1.0;
    let sin = (1.0 - cos * cos).max(0.0).sqrt();
    let (s, c) = (2.0 * PI * p[1]).sin_cos();
    Vec3::new(sin * c, sin * s, cos)
}

impl Guide {
    pub fn new(bounds: &Aabb) -> Guide {
        let (min, size) = if bounds.is_empty() {
            (Vec3::splat(-1.0), 2.0)
        } else {
            let ext = (bounds.max - bounds.min).max_elem().max(1e-3) * 1.001;
            (bounds.min - Vec3::splat(ext * 0.0005), ext)
        };
        Guide {
            min,
            size,
            nodes: vec![SNode { axis: 0, children: 0, wrapper: 0 }],
            wrappers: vec![Wrapper { sampling: DTree::new(), building: DTree::new(), records: AtomicU32::new(0) }],
            can_sample: false,
            recording: true,
        }
    }

    pub fn recording(&self) -> bool {
        self.recording
    }

    /// The spatial leaf containing `p`.
    pub fn leaf(&self, p: Vec3) -> u32 {
        let q = (p - self.min) * (1.0 / self.size);
        let mut q = [q.x, q.y, q.z].map(|v| if v.is_finite() { v.clamp(0.0, 1.0 - f32::EPSILON) } else { 0.5 });
        let mut n = 0;
        loop {
            let node = self.nodes[n];
            if node.children == 0 {
                return node.wrapper;
            }
            let a = node.axis as usize;
            q[a] *= 2.0;
            n = node.children as usize;
            if q[a] >= 1.0 {
                q[a] -= 1.0;
                n += 1;
            }
        }
    }

    /// True if the leaf has a learned distribution to sample.
    pub fn has_distribution(&self, leaf: u32) -> bool {
        self.can_sample && self.wrappers[leaf as usize].sampling.total() > 0.0
    }

    /// Solid-angle pdf of guided sampling of world direction `d` in `leaf`.
    pub fn pdf(&self, leaf: u32, d: Vec3) -> f32 {
        self.wrappers[leaf as usize].sampling.pdf(dir_to_square(d)) / (4.0 * PI)
    }

    /// A guided world direction for `leaf` and its solid-angle pdf.
    pub fn sample(&self, leaf: u32, u: (f32, f32)) -> (Vec3, f32) {
        let t = &self.wrappers[leaf as usize].sampling;
        let p = t.sample(u);
        let d = square_to_dir(p);
        (d, t.pdf(dir_to_square(d)) / (4.0 * PI))
    }

    /// Deposits an incident radiance estimate (radiance / pdf of the direction).
    pub fn record(&self, leaf: u32, d: Vec3, value: f32) {
        let w = &self.wrappers[leaf as usize];
        w.records.fetch_add(1, Relaxed);
        if value > 0.0 && value.is_finite() {
            w.building.record(dir_to_square(d), value);
        }
    }

    /// Ends a training iteration of `iter_spp` samples per pixel: splits busy spatial
    /// leaves, then turns the collected statistics into the sampling distributions.
    pub fn end_iteration(&mut self, iter_spp: u32) {
        let threshold = STREE_THRESHOLD * (iter_spp.max(1) as f32).sqrt();
        let mut stack = vec![(0usize, 0u32)];
        while let Some((n, depth)) = stack.pop() {
            let node = self.nodes[n];
            if node.children != 0 {
                stack.push((node.children as usize, depth + 1));
                stack.push((node.children as usize + 1, depth + 1));
                continue;
            }
            let w = &self.wrappers[node.wrapper as usize];
            let records = w.records.load(Relaxed);
            if records as f32 <= threshold || depth >= STREE_MAX_DEPTH {
                continue;
            }
            // Split: both halves start from the parent's distributions and half its records.
            w.records.store(records / 2, Relaxed);
            let copy = w.clone();
            let second = self.wrappers.len() as u32;
            self.wrappers.push(copy);
            let first_child = self.nodes.len() as u32;
            let axis = (node.axis + 1) % 3;
            self.nodes.push(SNode { axis, children: 0, wrapper: node.wrapper });
            self.nodes.push(SNode { axis, children: 0, wrapper: second });
            self.nodes[n].children = first_child;
            stack.push((first_child as usize, depth + 1));
            stack.push((first_child as usize + 1, depth + 1));
        }
        for w in &mut self.wrappers {
            let refined = w.building.refined();
            w.sampling = std::mem::replace(&mut w.building, refined);
            w.records.store(0, Relaxed);
        }
        self.can_sample = true;
    }

    /// Freezes the distributions: no more recording.
    pub fn stop_recording(&mut self) {
        self.recording = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    #[test]
    fn direction_mapping_round_trips() {
        let mut rng = Rng::new(1, 1);
        for _ in 0..1000 {
            let d = crate::sampling::uniform_sphere(rng.f32(), rng.f32());
            assert!((square_to_dir(dir_to_square(d)) - d).length() < 1e-3);
        }
    }

    /// After training on radiance from one cone of directions, the guide samples that cone
    /// most of the time, and its pdf matches its samples and integrates to one.
    #[test]
    fn learns_a_peaked_distribution() {
        let bounds = Aabb { min: Vec3::splat(-1.0), max: Vec3::splat(1.0) };
        let mut g = Guide::new(&bounds);
        let light = Vec3::new(0.3, 0.8, 0.52).normalize();
        let mut rng = Rng::new(2, 2);
        let p = Vec3::new(0.1, 0.2, 0.3);
        for iteration in 0..4 {
            let leaf = g.leaf(p);
            for _ in 0..20_000 {
                let d = crate::sampling::uniform_sphere(rng.f32(), rng.f32());
                let l = if d.dot(light) > 0.95 { 10.0 } else { 0.1 };
                g.record(leaf, d, l * 4.0 * PI);
            }
            g.end_iteration(1 << iteration);
        }
        let leaf = g.leaf(p);
        assert!(g.has_distribution(leaf));
        let n = 20_000;
        let mut inside = 0;
        for _ in 0..n {
            let (d, pdf) = g.sample(leaf, (rng.f32(), rng.f32()));
            assert!((pdf - g.pdf(leaf, d)).abs() <= 1e-3 * pdf.max(1.0), "{pdf} vs {}", g.pdf(leaf, d));
            if d.dot(light) > 0.95 {
                inside += 1;
            }
        }
        // The cone is 2.5% of the sphere but holds ~72% of the energy.
        assert!(inside as f32 / n as f32 > 0.6, "{inside}");
        let total: f32 =
            (0..200_000).map(|_| g.pdf(leaf, crate::sampling::uniform_sphere(rng.f32(), rng.f32()))).sum::<f32>()
                / 200_000.0
                * 4.0
                * PI;
        assert!((total - 1.0).abs() < 0.03, "pdf integral {total}");
    }

    #[test]
    fn busy_leaves_split() {
        let bounds = Aabb { min: Vec3::ZERO, max: Vec3::ONE };
        let mut g = Guide::new(&bounds);
        let mut rng = Rng::new(3, 3);
        for _ in 0..100_000 {
            let p = Vec3::new(rng.f32(), rng.f32(), rng.f32());
            g.record(g.leaf(p), Vec3::Z, 1.0);
        }
        g.end_iteration(1);
        assert!(g.wrappers.len() >= 8, "{} leaves", g.wrappers.len());
        let (a, b) = (g.leaf(Vec3::splat(0.1)), g.leaf(Vec3::splat(0.9)));
        assert_ne!(a, b);
    }
}
