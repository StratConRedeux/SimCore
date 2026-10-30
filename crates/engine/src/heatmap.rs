//! The signal heatmap: signal strength per cell around the command node, with distant
//! emitters aggregated in a far-field quadtree.

use simcore_rules::signal_strength_sq;

use crate::grid::SpatialGrid;
use crate::{Affiliation, EmsProfile, Gone, Position};

/// Size and falloff of a signal heatmap. All values are data.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeatmapSpec {
    /// Cell edge in m.
    pub cell_size_m: f64,
    /// Cells east-west and north-south, centred on the command node.
    pub cells_x: u32,
    pub cells_y: u32,
    /// Minimum distance in the falloff, in m (see [`simcore_rules::signal_strength`]).
    pub d_min_m: f64,
    /// Emitters farther than this from a cell centre (horizontally, in m) are ignored.
    pub cutoff_m: f64,
    /// Far-field opening ratio: the emitters of a grid cell, which lie within `r` of their
    /// power-weighted centroid, are summed as one source at the centroid (with a quadrupole
    /// correction) for map cells whose distance to that disc is at least `theta * 2r` (and
    /// `d_min_m`), and only if the whole disc is within the cutoff of them. Larger is more
    /// accurate and slower; [`HeatmapSpec::DEFAULT_THETA`] is a good start. `f64::INFINITY`
    /// ([`HeatmapSpec::EXACT`]) sums every emitter exactly; NaN does too, but is not a
    /// supported value. Values below 1 count as 1.
    pub theta: f64,
}

impl HeatmapSpec {
    /// Most cells a heatmap may have (4096 x 4096).
    pub const MAX_CELLS: u64 = 1 << 24;

    /// A suggested [`HeatmapSpec::theta`].
    pub const DEFAULT_THETA: f64 = 2.0;

    /// The [`HeatmapSpec::theta`] that sums every emitter exactly.
    pub const EXACT: f64 = f64::INFINITY;

    /// Centre of cell `(i, j)` relative to the command node, in m.
    fn cell_center(&self, i: u32, j: u32) -> (f64, f64) {
        (
            (f64::from(i) + 0.5 - f64::from(self.cells_x) / 2.0) * self.cell_size_m,
            (f64::from(j) + 0.5 - f64::from(self.cells_y) / 2.0) * self.cell_size_m,
        )
    }

    fn is_valid(&self) -> bool {
        let cells = u64::from(self.cells_x) * u64::from(self.cells_y);
        self.cell_size_m > 0.0
            && self.cell_size_m.is_finite()
            && self.cutoff_m > 0.0
            && self.cutoff_m.is_finite()
            && cells > 0
            && cells <= Self::MAX_CELLS
    }

    /// The opening ratio in use, or `None` for the exact sum.
    fn far_field_theta(&self) -> Option<f64> {
        (self.theta < f64::INFINITY).then(|| self.theta.max(1.0))
    }
}

/// Signal strength per cell around the command node, from units of one affiliation.
///
/// Cell `(i, j)` is `values()[j * cells_x + i]`; `i` grows east, `j` north. Each cell holds the
/// sum of `power / max(d, d_min)^2` (W/m^2, unnormalised) over currently emitting units within
/// the cutoff, where `d` is the horizontal distance from the cell centre. With a finite
/// [`HeatmapSpec::theta`], distant groups of emitters count as one source at their centroid.
#[derive(Debug, Default)]
pub struct Heatmap {
    spec: Option<HeatmapSpec>,
    origin: Position,
    values: Vec<f32>,
    emitters: Vec<(f64, f64, f64)>,
    grid: SpatialGrid,
    far: FarField,
    /// Candidates for the current block of cells, relative to the block centre.
    near: Candidates,
    /// Far-field aggregates for the current block, relative to the block centre.
    aggregates: Aggregates,
}

/// Aggregates for one block of cells, as columns padded to a multiple of [`LANES`]: centroid,
/// summed power and second moments about the centroid.
#[derive(Debug, Default)]
struct Aggregates {
    x: Vec<f64>,
    y: Vec<f64>,
    power: Vec<f64>,
    qxx: Vec<f64>,
    qxy: Vec<f64>,
    qyy: Vec<f64>,
}

impl Aggregates {
    fn clear(&mut self) {
        self.x.clear();
        self.y.clear();
        self.power.clear();
        self.qxx.clear();
        self.qxy.clear();
        self.qyy.clear();
    }

    fn push(&mut self, x: f64, y: f64, power: f64, (qxx, qxy, qyy): (f64, f64, f64)) {
        self.x.push(x);
        self.y.push(y);
        self.power.push(power);
        self.qxx.push(qxx);
        self.qxy.push(qxy);
        self.qyy.push(qyy);
    }
}

/// Emitters near one block of cells, as columns padded to a multiple of [`LANES`].
#[derive(Debug, Default)]
struct Candidates {
    x: Vec<f64>,
    y: Vec<f64>,
    power: Vec<f64>,
}

impl Candidates {
    fn clear(&mut self) {
        self.x.clear();
        self.y.clear();
        self.power.clear();
    }

    fn push(&mut self, x: f64, y: f64, power: f64) {
        self.x.push(x);
        self.y.push(y);
        self.power.push(power);
    }
}

/// Heatmap cells are evaluated in blocks of `BLOCK x BLOCK` sharing one candidate list.
const BLOCK: u32 = 8;
/// Independent partial sums per cell, so the inner loop can use SIMD. A fixed count and order
/// keep the result deterministic.
const LANES: usize = 4;

/// Levels below the far-field root: the finest grid cell is `1 / 2^DEPTH` of the root's edge.
const DEPTH: u32 = 24;
/// A far-field cell holding at most this many emitters is not split further.
const LEAF: usize = 4;

/// One cell of the far-field quadtree: the aggregate of the emitters in it.
#[derive(Clone, Copy, Debug)]
struct Node {
    /// Power-weighted centroid relative to the command node, in m, and summed power in W.
    x: f64,
    y: f64,
    power: f64,
    /// Power-weighted second moments about the centroid (`xx`, `xy`, `yy`), in W m^2.
    q: (f64, f64, f64),
    /// No emitter is farther than this from the centroid, in m.
    r: f64,
    /// The cell's south-west corner relative to the command node, and its edge, in m.
    x0: f64,
    y0: f64,
    size: f64,
    /// Its emitters are `FarField::points[start..end]`.
    start: u32,
    end: u32,
    /// Index of the first node after this one's subtree.
    skip: u32,
    leaf: bool,
}

/// Emitters in a quadtree of grid cells, each with its summed power and centroid.
///
/// The root is the square around the emitters' bounding box. Emitters are sorted by the Morton
/// code of their finest cell, so every cell's emitters are contiguous. Nodes are stored depth
/// first, children in Morton order, and a cell is only stored where its emitters split between
/// children (or it is a leaf), so there are fewer than `2n` nodes.
#[derive(Debug, Default)]
struct FarField {
    /// `(Morton code, emitter index)`, sorted.
    order: Vec<(u64, u32)>,
    /// Emitters in `order`, relative to the command node: `(x, y, power)`.
    points: Vec<(f64, f64, f64)>,
    nodes: Vec<Node>,
    /// Root square: south-west corner and edge.
    root: (f64, f64, f64),
}

/// Bits of `v` (below `DEPTH`) spread to the even bit positions.
#[inline]
fn spread(v: u64) -> u64 {
    (0..DEPTH).fold(0, |acc, b| acc | (((v >> b) & 1) << (2 * b)))
}

/// The inverse of [`spread`] for the even bit positions.
#[inline]
fn compact(v: u64) -> u64 {
    (0..DEPTH).fold(0, |acc, b| acc | (((v >> (2 * b)) & 1) << b))
}

impl FarField {
    /// Rebuilds the tree over `emitters` (world positions), relative to `origin`.
    fn build(&mut self, emitters: &[(f64, f64, f64)], origin: Position) {
        self.order.clear();
        self.points.clear();
        self.nodes.clear();
        let n = emitters.len();
        if n == 0 {
            return;
        }
        // Exact size on first build; no-ops while the emitter count is stable.
        self.order.reserve(n);
        self.points.reserve(n);
        self.nodes.reserve(2 * n);
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for &(x, y, _) in emitters {
            let (x, y) = (x - origin.x, y - origin.y);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
        let edge = (x1 - x0).max(y1 - y0);
        let edge = if edge > 0.0 { edge } else { 1.0 };
        self.root = (x0, y0, edge);
        let cells = (1u64 << DEPTH) as f64;
        let last = (1u64 << DEPTH) - 1;
        let q = |v: f64, v0: f64| (((v - v0) / edge * cells).floor() as u64).min(last);
        for (i, &(x, y, _)) in emitters.iter().enumerate() {
            let (x, y) = (x - origin.x, y - origin.y);
            let code = spread(q(x, x0)) | (spread(q(y, y0)) << 1);
            self.order.push((code, i as u32));
        }
        // Codes are paired with unique indices, so the unstable sort is deterministic.
        self.order.sort_unstable();
        for &(_, i) in &self.order {
            let (x, y, power) = emitters[i as usize];
            self.points.push((x - origin.x, y - origin.y, power));
        }
        self.node(0, 0, n);
    }

    /// Appends the subtree for `order[start..end]`, which share a cell at `level`, and returns
    /// its summed power and power-weighted coordinates.
    fn node(&mut self, mut level: u32, start: usize, end: usize) -> (f64, f64, f64) {
        let leaf = |level: u32| level == DEPTH || end - start <= LEAF;
        // A cell whose emitters all fall into one child is not stored: the child stands for it.
        while !leaf(level) {
            let shift = 2 * (DEPTH - 1 - level);
            if (self.order[start].0 >> shift) & 3 != (self.order[end - 1].0 >> shift) & 3 {
                break;
            }
            level += 1;
        }
        let index = self.nodes.len();
        let (rx, ry, edge) = self.root;
        let size = edge / (1u64 << level) as f64;
        let prefix = self.order[start].0 >> (2 * (DEPTH - level));
        self.nodes.push(Node {
            x: 0.0,
            y: 0.0,
            power: 0.0,
            q: (0.0, 0.0, 0.0),
            r: 0.0,
            x0: rx + compact(prefix) as f64 * size,
            y0: ry + compact(prefix >> 1) as f64 * size,
            size,
            start: start as u32,
            end: end as u32,
            skip: 0,
            leaf: leaf(level),
        });
        let (mut p, mut px, mut py) = (0.0, 0.0, 0.0);
        if leaf(level) {
            for &(x, y, power) in &self.points[start..end] {
                p += power;
                px += power * x;
                py += power * y;
            }
        } else {
            let shift = 2 * (DEPTH - 1 - level);
            let mut s = start;
            for child in 0..4 {
                let e = s + self.order[s..end].partition_point(|&(k, _)| (k >> shift) & 3 <= child);
                if e > s {
                    let (cp, cx, cy) = self.node(level + 1, s, e);
                    p += cp;
                    px += cx;
                    py += cy;
                }
                s = e;
            }
        }
        let skip = self.nodes.len() as u32;
        let (cx, cy) = (px / p, py / p);
        // Second moments and radius about the centroid: from the emitters for a leaf, from the
        // children (shifted to this centroid) otherwise.
        let (mut q, mut r) = ((0.0, 0.0, 0.0), 0.0f64);
        let mut add = |(dx, dy): (f64, f64), w: f64, cq: (f64, f64, f64), cr: f64| {
            q.0 += cq.0 + w * dx * dx;
            q.1 += cq.1 + w * dx * dy;
            q.2 += cq.2 + w * dy * dy;
            r = r.max((dx * dx + dy * dy).sqrt() + cr);
        };
        if leaf(level) {
            for &(x, y, power) in &self.points[start..end] {
                add((x - cx, y - cy), power, (0.0, 0.0, 0.0), 0.0);
            }
        } else {
            let mut k = index + 1;
            while k < skip as usize {
                let c = &self.nodes[k];
                add((c.x - cx, c.y - cy), c.power, c.q, c.r);
                k = c.skip as usize;
            }
        }
        // The farthest corner of the cell also bounds the radius.
        let node = &self.nodes[index];
        let fx = (cx - node.x0).abs().max((node.x0 + node.size - cx).abs());
        let fy = (cy - node.y0).abs().max((node.y0 + node.size - cy).abs());
        let r = r.min((fx * fx + fy * fy).sqrt());
        let node = &mut self.nodes[index];
        (node.x, node.y, node.power, node.q, node.r, node.skip) = (cx, cy, p, q, r, skip);
        (p, px, py)
    }

    /// Fills `near` and `far` with the sources for a block whose cell centres span
    /// `[x0, x1] x [y0, y1]` (relative to the command node), relative to the block centre
    /// `(bx, by)`. A cell whose emitter disc (radius `r` about the centroid) is at least
    /// `theta * 2r`, `d_min` and more than zero from every centre, and wholly within the cutoff
    /// of every centre, goes into `far` as one aggregate; the emitters of the leaves that are
    /// not go into `near`, and the caller applies the cutoff to each of them.
    fn gather(
        &self,
        (near, far): (&mut Candidates, &mut Aggregates),
        (x0, y0, x1, y1): (f64, f64, f64, f64),
        (bx, by): (f64, f64),
        theta: f64,
        d_min: f64,
        (cutoff, reach): (f64, f64),
    ) {
        let d_min = d_min.max(0.0);
        let mut k = 0;
        while k < self.nodes.len() {
            let n = &self.nodes[k];
            // Nearest distance from the centres' box to the cell.
            let dx = (n.x0 - x1).max(x0 - (n.x0 + n.size)).max(0.0);
            let dy = (n.y0 - y1).max(y0 - (n.y0 + n.size)).max(0.0);
            let near2 = dx * dx + dy * dy;
            // Nearest and farthest distance from the centres' box to the centroid.
            let nx = (x0 - n.x).max(n.x - x1).max(0.0);
            let ny = (y0 - n.y).max(n.y - y1).max(0.0);
            let fx = (n.x - x0).abs().max((n.x - x1).abs());
            let fy = (n.y - y0).abs().max((n.y - y1).abs());
            let gap = (nx * nx + ny * ny).sqrt() - n.r;
            // Written so that NaN (from overflowing coordinates) opens the cell.
            let is_far = gap > 0.0
                && gap >= d_min
                && gap >= theta * 2.0 * n.r
                && (fx * fx + fy * fy).sqrt() + n.r <= cutoff;
            if near2 > cutoff * cutoff {
                // Beyond the cutoff of every cell in the block.
            } else if is_far {
                far.push(n.x - bx, n.y - by, n.power, n.q);
            } else if n.leaf {
                for &(x, y, power) in &self.points[n.start as usize..n.end as usize] {
                    let (dx, dy) = (x - bx, y - by);
                    if dx * dx + dy * dy <= reach * reach {
                        near.push(dx, dy, power);
                    }
                }
            } else {
                k += 1;
                continue;
            }
            k = n.skip as usize;
        }
    }
}

/// The aggregates' signal at `(px, py)` (relative to the block centre): `power / d^2` plus the
/// quadrupole term `-tr(Q) / d^4 + 4 d.Q.d / d^6` of the expansion about each centroid. The
/// dipole term is zero about the centroid. `gather` keeps every centre beyond `d_min` and the
/// cutoff out of reach, so neither applies here.
fn far_field_sum(far: &Aggregates, (px, py): (f64, f64)) -> f64 {
    fn chunks(v: &[f64]) -> core::slice::Iter<'_, [f64; LANES]> {
        v.as_chunks::<LANES>().0.iter()
    }
    let mut acc = [0.0f64; LANES];
    let lanes = chunks(&far.x)
        .zip(chunks(&far.y))
        .zip(chunks(&far.power))
        .zip(chunks(&far.qxx).zip(chunks(&far.qxy)).zip(chunks(&far.qyy)));
    for (((xs, ys), ps), ((qxx, qxy), qyy)) in lanes {
        for l in 0..LANES {
            let (dx, dy) = (xs[l] - px, ys[l] - py);
            let inv = 1.0 / (dx * dx + dy * dy);
            let dqd = dx * dx * qxx[l] + 2.0 * dx * dy * qxy[l] + dy * dy * qyy[l];
            let quad = inv * (4.0 * dqd * inv - (qxx[l] + qyy[l]));
            acc[l] += inv * (ps[l] + quad);
        }
    }
    (acc[0] + acc[1]) + (acc[2] + acc[3])
}

impl Heatmap {
    /// The spec of the last build, or `None` if it was invalid (the map is then empty).
    pub fn spec(&self) -> Option<HeatmapSpec> {
        self.spec
    }

    /// Row-major cell strengths; empty when the spec was invalid.
    pub fn values(&self) -> &[f32] {
        &self.values
    }

    /// Strength of cell `(i, j)`.
    pub fn get(&self, i: u32, j: u32) -> Option<f32> {
        let s = self.spec?;
        (i < s.cells_x && j < s.cells_y).then(|| self.values[(j * s.cells_x + i) as usize])
    }

    /// Centre of cell `(i, j)` in m relative to the command node (x east, y north).
    pub fn cell_center(&self, i: u32, j: u32) -> Option<(f64, f64)> {
        self.spec.map(|s| s.cell_center(i, j))
    }

    /// World position of the command node when the map was built.
    pub fn origin(&self) -> Position {
        self.origin
    }

    /// Reused buffers' capacity, for allocation checks.
    pub fn capacity(&self) -> usize {
        self.values.capacity()
    }

    pub(crate) fn build<'a>(
        &mut self,
        spec: HeatmapSpec,
        origin: Position,
        units: impl Iterator<Item = (&'a Position, &'a Affiliation, &'a EmsProfile, &'a Gone)>,
        side: u8,
    ) {
        self.values.clear();
        self.emitters.clear();
        self.origin = origin;
        if !spec.is_valid() {
            self.spec = None;
            return;
        }
        self.spec = Some(spec);
        // Only emitters within the cutoff of the map's area can contribute.
        let half_x = f64::from(spec.cells_x) * spec.cell_size_m / 2.0 + spec.cutoff_m;
        let half_y = f64::from(spec.cells_y) * spec.cell_size_m / 2.0 + spec.cutoff_m;
        for (p, a, ems, gone) in units {
            let power = f64::from(ems.current_power_w());
            if !gone.0
                && a.0 == side
                && power > 0.0
                && (p.x - origin.x).abs() <= half_x
                && (p.y - origin.y).abs() <= half_y
            {
                self.emitters.push((p.x, p.y, power));
            }
        }
        // A canonical order, so the sums (and their rounding) do not depend on the ECS's
        // iteration order. Equal entries are interchangeable, so ties need no tiebreak.
        self.emitters.sort_unstable_by(|a, b| {
            (a.0.total_cmp(&b.0))
                .then(a.1.total_cmp(&b.1))
                .then(a.2.total_cmp(&b.2))
        });
        let theta = spec.far_field_theta();
        let em = &self.emitters;
        if theta.is_some() {
            self.far.build(em, origin);
        } else {
            self.grid
                .build(em.len(), spec.cutoff_m / 2.0, |i| (em[i].0, em[i].1));
        }
        self.values
            .resize((spec.cells_x * spec.cells_y) as usize, 0.0);
        if em.is_empty() {
            return;
        }
        let cutoff2 = spec.cutoff_m * spec.cutoff_m;
        let block_half_diag =
            f64::from(BLOCK) * spec.cell_size_m * core::f64::consts::FRAC_1_SQRT_2;
        let reach = spec.cutoff_m + block_half_diag;
        for bj in (0..spec.cells_y).step_by(BLOCK as usize) {
            for bi in (0..spec.cells_x).step_by(BLOCK as usize) {
                // Block centre, relative to the command node and in world metres.
                let (c0x, c0y) = spec.cell_center(bi, bj);
                let off = (f64::from(BLOCK) - 1.0) / 2.0 * spec.cell_size_m;
                let (rx, ry) = (c0x + off, c0y + off);
                let (bx, by) = (origin.x + rx, origin.y + ry);
                let near = &mut self.near;
                let far = &mut self.aggregates;
                near.clear();
                far.clear();
                if let Some(theta) = theta {
                    // The block's own cell centres, which can be fewer than BLOCK at the edges.
                    let (i1, j1) = (
                        (bi + BLOCK).min(spec.cells_x) - 1,
                        (bj + BLOCK).min(spec.cells_y) - 1,
                    );
                    let (c1x, c1y) = spec.cell_center(i1, j1);
                    self.far.gather(
                        (near, far),
                        (c0x, c0y, c1x, c1y),
                        (rx, ry),
                        theta,
                        spec.d_min_m,
                        (spec.cutoff_m, reach),
                    );
                } else {
                    // Every emitter within the cutoff of any cell in the block, relative to it.
                    self.grid.for_each_near(bx, by, reach, |k| {
                        let (ex, ey, power) = em[k as usize];
                        let (dx, dy) = (ex - bx, ey - by);
                        if dx * dx + dy * dy <= reach * reach {
                            near.push(dx, dy, power);
                        }
                        true
                    });
                }
                if near.x.is_empty() && far.x.is_empty() {
                    continue;
                }
                // Pad with zero-power entries beyond the cutoff.
                while !near.x.len().is_multiple_of(LANES) {
                    near.push(2.0 * reach, 0.0, 0.0);
                }
                while !far.x.len().is_multiple_of(LANES) {
                    far.push(2.0 * reach, 0.0, 0.0, (0.0, 0.0, 0.0));
                }
                for j in bj..(bj + BLOCK).min(spec.cells_y) {
                    for i in bi..(bi + BLOCK).min(spec.cells_x) {
                        let (cx, cy) = spec.cell_center(i, j);
                        let (px, py) = (origin.x + cx - bx, origin.y + cy - by);
                        let mut acc = [0.0f64; LANES];
                        let lanes = near
                            .x
                            .as_chunks::<LANES>()
                            .0
                            .iter()
                            .zip(near.y.as_chunks::<LANES>().0.iter())
                            .zip(near.power.as_chunks::<LANES>().0.iter());
                        for ((xs, ys), ps) in lanes {
                            for l in 0..LANES {
                                let (dx, dy) = (xs[l] - px, ys[l] - py);
                                let d2 = dx * dx + dy * dy;
                                let s = signal_strength_sq(ps[l], d2, spec.d_min_m);
                                acc[l] += if d2 <= cutoff2 { s } else { 0.0 };
                            }
                        }
                        let mut sum = (acc[0] + acc[1]) + (acc[2] + acc[3]);
                        if !far.x.is_empty() {
                            sum += far_field_sum(far, (px, py));
                        }
                        self.values[(j * spec.cells_x + i) as usize] = sum as f32;
                    }
                }
            }
        }
    }
}
