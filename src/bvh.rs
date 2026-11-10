//! Bounding volume hierarchy: a binary tree built with binned SAH (surface area
//! heuristic), in parallel for large inputs, then collapsed into a 4-wide BVH whose
//! nodes store their children's bounds as structure-of-arrays. One node visit tests
//! four boxes with straight-line arithmetic the compiler vectorizes.

use crate::geometry::Aabb;
use crate::math::Vec3;

/// Four child boxes and what they point to (128 bytes, two cache lines).
#[derive(Clone, Copy, Debug)]
#[repr(C, align(64))]
pub struct Node4 {
    min: [[f32; 4]; 3],
    max: [[f32; 4]; 3],
    /// Interior child: node index. Leaf child: first index into `Bvh::indices`.
    child: [u32; 4],
    /// Primitives in a leaf child; 0 for an interior child.
    count: [u32; 4],
}

impl Node4 {
    /// Unused slots hold a box at +infinity, which no ray enters at a finite distance.
    const EMPTY: Node4 =
        Node4 { min: [[f32::INFINITY; 4]; 3], max: [[f32::INFINITY; 4]; 3], child: [0; 4], count: [0; 4] };

    fn set(&mut self, i: usize, b: &Aabb, child: u32, count: u32) {
        for k in 0..3 {
            self.min[k][i] = b.min[k];
            self.max[k][i] = b.max[k];
        }
        self.child[i] = child;
        self.count[i] = count;
    }

    /// Entry distance of the ray into each child box (infinite where it misses).
    #[inline]
    fn intersect(&self, o: [f32; 3], inv_d: [f32; 3], t_max: f32) -> [f32; 4] {
        let mut enter = [0.0f32; 4];
        let mut exit = [t_max; 4];
        for k in 0..3 {
            for i in 0..4 {
                let t0 = (self.min[k][i] - o[k]) * inv_d[k];
                let t1 = (self.max[k][i] - o[k]) * inv_d[k];
                enter[i] = enter[i].max(t0.min(t1));
                exit[i] = exit[i].min(t0.max(t1));
            }
        }
        std::array::from_fn(|i| if enter[i] <= exit[i] { enter[i] } else { f32::INFINITY })
    }

    fn bounds(&self) -> Aabb {
        let mut b = Aabb::EMPTY;
        for i in (0..4).filter(|&i| self.min[0][i].is_finite()) {
            b.grow(&Aabb {
                min: Vec3::new(self.min[0][i], self.min[1][i], self.min[2][i]),
                max: Vec3::new(self.max[0][i], self.max[1][i], self.max[2][i]),
            });
        }
        b
    }
}

pub struct Bvh {
    nodes: Vec<Node4>,
    /// Primitive indices, reordered so that each leaf references a contiguous range.
    pub indices: Vec<u32>,
    depth: usize,
}

const BINS: usize = 16;
const MAX_LEAF: usize = 8;
/// Cost of traversing a node relative to intersecting one primitive.
const TRAVERSAL_COST: f32 = 1.0;
/// Below this depth the SAH may split as unevenly as it likes; beyond it splits are
/// forced to halve the primitives, bounding the depth (and the traversal stack).
const MAX_SAH_DEPTH: usize = 40;
/// Subtrees with at least this many primitives are built on their own thread.
const PARALLEL_MIN: usize = 16 * 1024;
/// Traversal stack entries needed for a 4-wide tree `depth` nodes deep: visiting a node
/// pops it and pushes at most four children, so each level adds at most three entries.
const fn stack_size(depth: usize) -> usize {
    3 * depth + 1
}
/// Enough for any tree the builder makes: binary depth ≤ `MAX_SAH_DEPTH` + 32 median
/// levels (2^32 primitives), and collapsing never makes the 4-wide tree deeper.
const STACK_SIZE: usize = stack_size(MAX_SAH_DEPTH + 32) + 3;
/// Trees up to this many 4-wide levels use a stack of `stack_size(SMALL_DEPTH)` entries.
const SMALL_DEPTH: usize = 20;

enum BuildNode {
    Leaf { bounds: Aabb, start: u32, count: u32 },
    Inner { bounds: Aabb, children: Box<[BuildNode; 2]> },
}

impl BuildNode {
    fn bounds(&self) -> &Aabb {
        match self {
            BuildNode::Leaf { bounds, .. } | BuildNode::Inner { bounds, .. } => bounds,
        }
    }
}

struct Ctx<'a> {
    bounds: &'a [Aabb],
    centroids: &'a [Vec3],
    /// Recursion depth down to which subtrees may be built in parallel.
    parallel_depth: usize,
}

impl Bvh {
    pub fn build(prim_bounds: &[Aabb]) -> Bvh {
        let n = prim_bounds.len();
        let mut indices: Vec<u32> = (0..n as u32).collect();
        let mut bvh = Bvh { nodes: Vec::new(), indices: Vec::new(), depth: 0 };
        if n == 0 {
            return bvh;
        }
        let centroids: Vec<Vec3> = prim_bounds.iter().map(Aabb::centroid).collect();
        let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        let ctx = Ctx { bounds: prim_bounds, centroids: &centroids, parallel_depth: threads.ilog2() as usize + 2 };
        let root = ctx.build(&mut indices, 0, 0);
        bvh.nodes.reserve(n / 2 + 1);
        bvh.nodes.push(Node4::EMPTY);
        bvh.depth = match &root {
            BuildNode::Leaf { bounds, start, count } => {
                bvh.nodes[0].set(0, bounds, *start, *count);
                1
            }
            BuildNode::Inner { .. } => bvh.flatten(&root, 0),
        };
        bvh.indices = indices;
        bvh
    }

    pub fn bounds(&self) -> Aabb {
        self.nodes.first().map_or(Aabb::EMPTY, Node4::bounds)
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn depth(&self) -> usize {
        self.depth
    }

    /// Fills `nodes[at]` with up to four children of `node` (an interior node), opening
    /// the largest interior children first. Returns the depth of the subtree.
    fn flatten(&mut self, node: &BuildNode, at: usize) -> usize {
        let BuildNode::Inner { children, .. } = node else { unreachable!("flatten starts at interior nodes") };
        let mut slots: Vec<&BuildNode> = children.iter().collect();
        while slots.len() < 4 {
            let open = slots
                .iter()
                .enumerate()
                .filter(|(_, c)| matches!(c, BuildNode::Inner { .. }))
                .max_by(|a, b| a.1.bounds().surface_area().total_cmp(&b.1.bounds().surface_area()))
                .map(|(i, _)| i);
            let Some(i) = open else { break };
            let BuildNode::Inner { children, .. } = slots.swap_remove(i) else { unreachable!() };
            slots.extend(children.iter());
        }
        let mut depth = 1;
        for (i, c) in slots.into_iter().enumerate() {
            match c {
                BuildNode::Leaf { bounds, start, count } => self.nodes[at].set(i, bounds, *start, *count),
                BuildNode::Inner { bounds, .. } => {
                    let idx = self.nodes.len();
                    self.nodes.push(Node4::EMPTY);
                    self.nodes[at].set(i, bounds, idx as u32, 0);
                    depth = depth.max(1 + self.flatten(c, idx));
                }
            }
        }
        depth
    }

    /// Visits the leaves the ray may hit, nearest first. `leaf(first, count, t_max)`
    /// intersects `indices[first..first + count]`, lowers `t_max` on hits and returns
    /// true to stop the traversal.
    #[inline]
    pub fn traverse(&self, o: Vec3, d: Vec3, t_max: f32, leaf: impl FnMut(u32, u32, &mut f32) -> bool) {
        // The stack is zero-initialized for every ray, so shallow trees (nearly all of them)
        // use a small one: this alone made simple scenes trace ~10% faster.
        if self.depth <= SMALL_DEPTH {
            self.traverse_with::<{ stack_size(SMALL_DEPTH) }>(o, d, t_max, leaf);
        } else {
            self.traverse_with::<STACK_SIZE>(o, d, t_max, leaf);
        }
    }

    #[inline]
    fn traverse_with<const N: usize>(
        &self,
        o: Vec3,
        d: Vec3,
        mut t_max: f32,
        mut leaf: impl FnMut(u32, u32, &mut f32) -> bool,
    ) {
        if self.nodes.is_empty() {
            return;
        }
        let inv = d.recip();
        let (o, inv_d) = ([o.x, o.y, o.z], [inv.x, inv.y, inv.z]);
        // Entries: (node or leaf start, leaf count (0 = node), entry distance).
        let mut stack = [(0u32, 0u32, 0.0f32); N];
        let mut sp = 1;
        while sp > 0 {
            sp -= 1;
            let (idx, count, dist) = stack[sp];
            if dist > t_max {
                continue;
            }
            if count > 0 {
                if leaf(idx, count, &mut t_max) {
                    return;
                }
                continue;
            }
            let node = &self.nodes[idx as usize];
            let t = node.intersect(o, inv_d, t_max);
            // Push hit children far to near so the nearest is popped first.
            let mut order = [0usize; 4];
            let mut n = 0;
            for i in 0..4 {
                if t[i].is_finite() {
                    let mut j = n;
                    while j > 0 && t[order[j - 1]] < t[i] {
                        order[j] = order[j - 1];
                        j -= 1;
                    }
                    order[j] = i;
                    n += 1;
                }
            }
            for &i in &order[..n] {
                stack[sp] = (node.child[i], node.count[i], t[i]);
                sp += 1;
            }
        }
    }
}

impl Ctx<'_> {
    /// Builds the subtree over `indices` (which start at `offset` in the full array).
    fn build(&self, indices: &mut [u32], offset: usize, depth: usize) -> BuildNode {
        let mut bb = Aabb::EMPTY;
        let mut cb = Aabb::EMPTY;
        for &i in indices.iter() {
            bb.grow(&self.bounds[i as usize]);
            cb.grow_point(self.centroids[i as usize]);
        }
        let count = indices.len();
        let leaf = BuildNode::Leaf { bounds: bb, start: offset as u32, count: count as u32 };
        if count <= 2 {
            return leaf;
        }
        let mid = if depth < MAX_SAH_DEPTH {
            match self.sah_split(indices, &bb, &cb) {
                Split::Leaf => return leaf,
                Split::At(mid) => mid,
                Split::None if count <= MAX_LEAF => return leaf,
                Split::None => count / 2,
            }
        } else {
            self.median_split(indices, &cb)
        };

        let (left, right) = indices.split_at_mut(mid);
        let children = if depth < self.parallel_depth && count >= PARALLEL_MIN {
            std::thread::scope(|s| {
                let l = s.spawn(|| self.build(left, offset, depth + 1));
                let r = self.build(right, offset + mid, depth + 1);
                [l.join().expect("BVH build thread panicked"), r]
            })
        } else {
            [self.build(left, offset, depth + 1), self.build(right, offset + mid, depth + 1)]
        };
        BuildNode::Inner { bounds: bb, children: Box::new(children) }
    }

    fn sah_split(&self, indices: &mut [u32], bb: &Aabb, cb: &Aabb) -> Split {
        let count = indices.len();
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
            for &i in indices.iter() {
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
        if !best_cost.is_finite() {
            // All centroids coincide: no plane separates them.
            return Split::None;
        }
        let leaf_cost = bb.surface_area() * count as f32;
        if count <= MAX_LEAF && TRAVERSAL_COST * bb.surface_area() + best_cost >= leaf_cost {
            return Split::Leaf;
        }
        let scale = BINS as f32 / ext[best_axis];
        let (min, centroids) = (cb.min[best_axis], self.centroids);
        Split::At(partition(indices, |i| bin_of(centroids[i as usize][best_axis], min, scale) < best_split))
    }

    /// Splits at the median centroid along the widest axis.
    fn median_split(&self, indices: &mut [u32], cb: &Aabb) -> usize {
        let ext = cb.max - cb.min;
        let axis = if ext.x >= ext.y && ext.x >= ext.z {
            0
        } else if ext.y >= ext.z {
            1
        } else {
            2
        };
        let mid = indices.len() / 2;
        let c = self.centroids;
        indices.select_nth_unstable_by(mid, |&a, &b| c[a as usize][axis].total_cmp(&c[b as usize][axis]));
        mid
    }
}

enum Split {
    /// Partitioned; the first `n` indices go left.
    At(usize),
    /// Cheaper as a leaf.
    Leaf,
    /// No plane separates the primitives.
    None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    /// Random boxes: the BVH finds exactly the boxes a brute-force slab test finds.
    #[test]
    fn traversal_matches_brute_force() {
        let mut rng = Rng::new(7, 7);
        // Enough primitives to exercise the parallel build.
        let n = PARALLEL_MIN + 5000;
        let boxes: Vec<Aabb> = (0..n)
            .map(|i| {
                // A cluster of identical boxes (no separating plane) and spread-out ones.
                let c = if i < 40 {
                    Vec3::splat(3.0)
                } else {
                    Vec3::new(rng.f32() * 100.0, rng.f32() * 100.0, rng.f32() * 100.0)
                };
                let e = Vec3::new(rng.f32(), rng.f32(), rng.f32()) * 0.5;
                Aabb { min: c - e, max: c + e }
            })
            .collect();
        let bvh = Bvh::build(&boxes);
        let mut sorted = bvh.indices.clone();
        sorted.sort_unstable();
        assert!(sorted.iter().enumerate().all(|(i, &v)| i as u32 == v), "indices are a permutation");
        assert!(bvh.depth() < 40, "depth {}", bvh.depth());
        for _ in 0..300 {
            let o = Vec3::new(rng.f32() * 120.0 - 10.0, rng.f32() * 120.0 - 10.0, -5.0);
            let target = Vec3::new(rng.f32() * 100.0, rng.f32() * 100.0, rng.f32() * 100.0);
            let d = (target - o).normalize();
            let inv = d.recip();
            let mut expect: Vec<u32> =
                (0..n as u32).filter(|&i| boxes[i as usize].hit(o, inv, f32::INFINITY).is_finite()).collect();
            let mut got = Vec::new();
            bvh.traverse(o, d, f32::INFINITY, |first, count, _| {
                for k in first..first + count {
                    let p = bvh.indices[k as usize];
                    if boxes[p as usize].hit(o, inv, f32::INFINITY).is_finite() {
                        got.push(p);
                    }
                }
                false
            });
            expect.sort_unstable();
            got.sort_unstable();
            assert_eq!(expect, got);
        }
    }

    #[test]
    fn single_primitive_and_empty() {
        let b = Aabb { min: Vec3::splat(-1.0), max: Vec3::ONE };
        let bvh = Bvh::build(&[b]);
        let mut hits = 0;
        bvh.traverse(Vec3::new(0.0, 0.0, -5.0), Vec3::Z, f32::INFINITY, |_, c, _| {
            hits += c;
            false
        });
        assert_eq!(hits, 1);
        let empty = Bvh::build(&[]);
        assert!(empty.bounds().is_empty() && empty.node_count() == 0);
        empty.traverse(Vec3::ZERO, Vec3::Z, 1.0, |_, _, _| panic!("no leaves"));
    }
}
