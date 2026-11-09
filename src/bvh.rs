//! Bounding volume hierarchy built with binned SAH (surface area heuristic).

use crate::geometry::Aabb;
use crate::math::Vec3;

#[derive(Clone, Copy, Debug)]
pub struct Node {
    pub bounds: Aabb,
    /// Leaf: first index into `Bvh::indices`. Interior: index of the left child
    /// (the right child is always `first + 1`).
    pub first: u32,
    /// Number of primitives for a leaf, 0 for an interior node.
    pub count: u32,
}

pub struct Bvh {
    pub nodes: Vec<Node>,
    /// Primitive indices, reordered so that each leaf references a contiguous range.
    pub indices: Vec<u32>,
}

const BINS: usize = 16;
const MAX_LEAF: usize = 8;
/// Cost of traversing a node relative to intersecting one primitive.
const TRAVERSAL_COST: f32 = 1.0;

impl Bvh {
    pub fn build(prim_bounds: &[Aabb]) -> Bvh {
        let n = prim_bounds.len();
        let mut indices: Vec<u32> = (0..n as u32).collect();
        let mut nodes = Vec::with_capacity(2 * n.max(1));
        if n > 0 {
            let centroids: Vec<Vec3> = prim_bounds.iter().map(|b| b.centroid()).collect();
            nodes.push(Node { bounds: Aabb::EMPTY, first: 0, count: 0 });
            let mut b = Builder { nodes: &mut nodes, indices: &mut indices, bounds: prim_bounds, centroids: &centroids };
            b.subdivide(0, 0, n);
        }
        Bvh { nodes, indices }
    }

    pub fn depth(&self) -> usize {
        fn rec(nodes: &[Node], i: usize) -> usize {
            let n = &nodes[i];
            if n.count > 0 { 1 } else { 1 + rec(nodes, n.first as usize).max(rec(nodes, n.first as usize + 1)) }
        }
        if self.nodes.is_empty() { 0 } else { rec(&self.nodes, 0) }
    }
}

struct Builder<'a> {
    nodes: &'a mut Vec<Node>,
    indices: &'a mut [u32],
    bounds: &'a [Aabb],
    centroids: &'a [Vec3],
}

impl Builder<'_> {
    fn subdivide(&mut self, node: usize, start: usize, end: usize) {
        let mut bb = Aabb::EMPTY;
        let mut cb = Aabb::EMPTY;
        for &i in &self.indices[start..end] {
            bb.grow(&self.bounds[i as usize]);
            cb.grow_point(self.centroids[i as usize]);
        }
        self.nodes[node].bounds = bb;
        let count = end - start;
        if count <= 2 {
            self.make_leaf(node, start, count);
            return;
        }

        // Find the best split plane over all three axes.
        let ext = cb.max - cb.min;
        let mut best_cost = f32::INFINITY;
        let mut best_axis = 0;
        let mut best_split = 0;
        for axis in 0..3 {
            if ext[axis] <= 0.0 {
                continue;
            }
            let scale = BINS as f32 / ext[axis];
            let mut bin_bounds = [Aabb::EMPTY; BINS];
            let mut bin_count = [0u32; BINS];
            for &i in &self.indices[start..end] {
                let b = bin_of(self.centroids[i as usize][axis], cb.min[axis], scale);
                bin_count[b] += 1;
                bin_bounds[b].grow(&self.bounds[i as usize]);
            }
            let mut left_area = [0.0f32; BINS];
            let mut left_count = [0u32; BINS];
            let mut acc = Aabb::EMPTY;
            let mut c = 0;
            for b in 0..BINS - 1 {
                acc.grow(&bin_bounds[b]);
                c += bin_count[b];
                left_area[b] = acc.surface_area();
                left_count[b] = c;
            }
            acc = Aabb::EMPTY;
            c = 0;
            for b in (1..BINS).rev() {
                acc.grow(&bin_bounds[b]);
                c += bin_count[b];
                if left_count[b - 1] == 0 || c == 0 {
                    continue;
                }
                let cost = left_area[b - 1] * left_count[b - 1] as f32 + acc.surface_area() * c as f32;
                if cost < best_cost {
                    best_cost = cost;
                    best_axis = axis;
                    best_split = b;
                }
            }
        }

        let mid = if best_cost.is_finite() {
            let leaf_cost = bb.surface_area() * count as f32;
            if count <= MAX_LEAF && TRAVERSAL_COST * bb.surface_area() + best_cost >= leaf_cost {
                self.make_leaf(node, start, count);
                return;
            }
            let scale = BINS as f32 / ext[best_axis];
            let (min, centroids) = (cb.min[best_axis], self.centroids);
            start + partition(&mut self.indices[start..end], |i| {
                bin_of(centroids[i as usize][best_axis], min, scale) < best_split
            })
        } else {
            // All centroids coincide: no plane separates them.
            if count <= MAX_LEAF {
                self.make_leaf(node, start, count);
                return;
            }
            start + count / 2
        };

        let left = self.nodes.len();
        let placeholder = Node { bounds: Aabb::EMPTY, first: 0, count: 0 };
        self.nodes.push(placeholder);
        self.nodes.push(placeholder);
        self.nodes[node].first = left as u32;
        self.nodes[node].count = 0;
        self.subdivide(left, start, mid);
        self.subdivide(left + 1, mid, end);
    }

    fn make_leaf(&mut self, node: usize, start: usize, count: usize) {
        self.nodes[node].first = start as u32;
        self.nodes[node].count = count as u32;
    }
}

#[inline]
fn bin_of(c: f32, min: f32, scale: f32) -> usize {
    (((c - min) * scale) as usize).min(BINS - 1)
}

/// In-place partition; returns the number of elements satisfying `pred`.
fn partition(s: &mut [u32], pred: impl Fn(u32) -> bool) -> usize {
    let (mut i, mut j) = (0, s.len());
    while i < j {
        if pred(s[i]) {
            i += 1;
        } else {
            j -= 1;
            s.swap(i, j);
        }
    }
    i
}
