//! Uniform 2D spatial grid (x, y) over a set of points, rebuilt each use with reused buffers.
//!
//! Cells have a fixed size, aligned to multiples of it from the world origin. Points are stored
//! grouped by cell, row-major (y outer), in insertion order within a cell, so queries visit
//! candidates in a deterministic order. No hashing. When the points' bounding box fits in
//! `max(4096, 2n)` cells, a dense array indexes the cells (a counting sort). Otherwise (a few far
//! outliers, or a huge spread) the points are sorted by cell key and rows are found by binary
//! search, so the cell size never coarsens. Both layouts give the same candidates in the same
//! order.

/// Cell coordinates are clamped to `[-LIMIT, LIMIT]`, so huge or non-finite input cannot
/// overflow. The clamp is monotone, so clamped cells still bound every query.
const LIMIT: i64 = 1 << 30;

/// Cell coordinate of `v` for cells of `cell` metres; NaN maps to 0.
#[inline]
fn coord(v: f64, cell: f64) -> i64 {
    let c = (v / cell).floor();
    if c.is_nan() {
        0
    } else {
        c.clamp(-LIMIT as f64, LIMIT as f64) as i64
    }
}

/// Sort key of cell `(cx, cy)`: row-major, y outer. Both fit in 32 bits after the offset.
#[inline]
fn key(cx: i64, cy: i64) -> u64 {
    (((cy + LIMIT) as u64) << 32) | (cx + LIMIT) as u64
}

#[inline]
fn row_of(key: u64) -> i64 {
    (key >> 32) as i64 - LIMIT
}

/// A rebuildable grid of point indices.
#[derive(Debug, Default)]
pub(crate) struct SpatialGrid {
    cell: f64,
    len: usize,
    /// Bounding box of the points' cells, inclusive.
    min_cx: i64,
    min_cy: i64,
    max_cx: i64,
    max_cy: i64,
    /// Whether the dense layout (`starts`, `items`) is in use, else the sorted one.
    dense: bool,
    /// Dense: `starts[c]..starts[c + 1]` indexes `items` for cell `c` (row-major, y outer).
    starts: Vec<u32>,
    items: Vec<u32>,
    cursor: Vec<u32>,
    /// `(cell key, point index)` per point. Sorted in the sparse layout; scratch in the dense.
    keyed: Vec<(u64, u32)>,
}

impl SpatialGrid {
    /// Rebuilds the grid over `n` points with cells of `cell` metres.
    ///
    /// Non-finite coordinates are binned at 0, and coordinates beyond `2^30` cells from the
    /// origin are binned at that limit; callers' exact distance tests reject what does not
    /// match. Allocates only when a buffer must grow.
    pub fn build(&mut self, n: usize, cell: f64, pos: impl Fn(usize) -> (f64, f64)) {
        self.build_with(n, cell, (2 * n as u64).max(4096), pos);
    }

    /// [`SpatialGrid::build`], using the dense layout only up to `max_cells` cells.
    fn build_with(
        &mut self,
        n: usize,
        cell: f64,
        max_cells: u64,
        pos: impl Fn(usize) -> (f64, f64),
    ) {
        self.starts.clear();
        self.items.clear();
        self.keyed.clear();
        self.len = 0;
        if n == 0 || !(cell > 0.0 && cell.is_finite()) {
            return;
        }
        self.cell = cell;
        self.len = n;
        // (Empty here, so `reserve` sets the total capacity.)
        self.keyed.reserve(n);
        let (mut x0, mut y0, mut x1, mut y1) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
        for i in 0..n {
            let (x, y) = pos(i);
            let (x, y) = (
                if x.is_finite() { x } else { 0.0 },
                if y.is_finite() { y } else { 0.0 },
            );
            let (cx, cy) = (coord(x, cell), coord(y, cell));
            x0 = x0.min(cx);
            y0 = y0.min(cy);
            x1 = x1.max(cx);
            y1 = y1.max(cy);
            self.keyed.push((key(cx, cy), i as u32));
        }
        (self.min_cx, self.min_cy, self.max_cx, self.max_cy) = (x0, y0, x1, y1);
        // Each side is at most 2^31 + 1 cells, so this cannot overflow.
        let (w, h) = ((x1 - x0 + 1) as u64, (y1 - y0 + 1) as u64);
        self.dense = w.saturating_mul(h) <= max_cells;
        if !self.dense {
            // Keys are unique per (cell, index) pair, so the unstable sort is deterministic and
            // keeps insertion order within a cell.
            self.keyed.sort_unstable();
            return;
        }

        let cells = (w * h) as usize;
        // Size for the worst case up front, so the grid does not reallocate as units spread out.
        self.cursor.clear();
        self.starts.reserve(max_cells as usize + 1);
        self.cursor.reserve(max_cells as usize);
        self.starts.resize(cells + 1, 0);
        let index = |k: u64| {
            let (cx, cy) = ((k & 0xffff_ffff) as i64 - LIMIT, row_of(k));
            ((cy - y0) as u64 * w + (cx - x0) as u64) as usize
        };
        for &(k, _) in &self.keyed {
            self.starts[index(k) + 1] += 1;
        }
        for c in 0..cells {
            self.starts[c + 1] += self.starts[c];
        }
        self.cursor.extend_from_slice(&self.starts[..cells]);
        self.items.resize(n, 0);
        for &(k, i) in &self.keyed {
            let slot = &mut self.cursor[index(k)];
            self.items[*slot as usize] = i;
            *slot += 1;
        }
    }

    /// Calls `f` with the index of every point in a cell overlapping the disc of `radius`
    /// around `(x, y)`, row by row, until `f` returns `false`. A superset of the points within
    /// `radius`: callers do the exact distance test.
    pub fn for_each_near(&self, x: f64, y: f64, radius: f64, mut f: impl FnMut(u32) -> bool) {
        if self.len == 0 || !(x.is_finite() && y.is_finite() && radius >= 0.0) {
            return;
        }
        // Pad by a hair so floating-point rounding at cell edges never drops a candidate.
        let r = radius + 1e-9 * (radius + x.abs().max(y.abs())) + 1e-6;
        let cell = self.cell;
        let cx0 = coord(x - r, cell).max(self.min_cx);
        let cx1 = coord(x + r, cell).min(self.max_cx);
        let cy0 = coord(y - r, cell).max(self.min_cy);
        let cy1 = coord(y + r, cell).min(self.max_cy);
        if cx0 > cx1 || cy0 > cy1 {
            return;
        }
        if self.dense {
            let w = (self.max_cx - self.min_cx + 1) as usize;
            for cy in cy0..=cy1 {
                let row = (cy - self.min_cy) as usize * w + (cx0 - self.min_cx) as usize;
                let (a, b) = (
                    self.starts[row] as usize,
                    self.starts[row + (cx1 - cx0) as usize + 1] as usize,
                );
                // Cells in one row are contiguous in `items`.
                for &i in &self.items[a..b] {
                    if !f(i) {
                        return;
                    }
                }
            }
            return;
        }
        // Sorted layout: one binary search per occupied row, skipping empty rows.
        let mut cy = cy0;
        while cy <= cy1 {
            let (lo, hi) = (key(cx0, cy), key(cx1, cy));
            let mut k = self.keyed.partition_point(|&(q, _)| q < lo);
            while let Some(&(q, i)) = self.keyed.get(k)
                && q <= hi
            {
                if !f(i) {
                    return;
                }
                k += 1;
            }
            let Some(&(q, _)) = self.keyed.get(k) else {
                return;
            };
            cy = (cy + 1).max(row_of(q));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(g: &SpatialGrid, x: f64, y: f64, r: f64) -> Vec<u32> {
        let mut got = Vec::new();
        g.for_each_near(x, y, r, |i| {
            got.push(i);
            true
        });
        got
    }

    fn assert_superset(pts: &[(f64, f64)], g: &SpatialGrid, q: (f64, f64), r: f64) {
        let got = near(g, q.0, q.1, r);
        for (i, &(x, y)) in pts.iter().enumerate() {
            if (x - q.0).powi(2) + (y - q.1).powi(2) <= r * r {
                assert!(got.contains(&(i as u32)), "missed point {i} near {q:?}");
            }
        }
    }

    #[test]
    fn query_is_a_superset_of_the_disc_in_both_layouts() {
        let mut pts: Vec<(f64, f64)> = (0..400)
            .map(|i| {
                (
                    ((i * 37) % 101) as f64 * 10.0 - 500.0,
                    ((i * 53) % 97) as f64 * 10.0,
                )
            })
            .collect();
        let queries = [(0.0, 0.0), (-500.0, 0.0), (100.0, 100.0), (433.3, 960.0)];
        let (mut dense, mut sparse) = (SpatialGrid::default(), SpatialGrid::default());
        dense.build(pts.len(), 100.0, |i| pts[i]);
        sparse.build_with(pts.len(), 100.0, 0, |i| pts[i]);
        assert!(dense.dense && !sparse.dense);
        for &q in &queries {
            assert_superset(&pts, &dense, q, 100.0);
            assert_eq!(
                near(&dense, q.0, q.1, 100.0),
                near(&sparse, q.0, q.1, 100.0)
            );
        }
        // One far outlier switches to the sorted layout without coarsening the cells.
        pts.push((1e9, -1e9));
        let mut g = SpatialGrid::default();
        g.build(pts.len(), 100.0, |i| pts[i]);
        assert!(!g.dense);
        assert_eq!(g.cell, 100.0);
        for &q in queries.iter().chain(&[(1e9, -1e9)]) {
            assert_superset(&pts, &g, q, 100.0);
            // At most 4 x 4 cells of ~4 points each, not all 401.
            assert!(near(&g, q.0, q.1, 100.0).len() < 120);
        }
    }

    #[test]
    fn extreme_coordinates_do_not_overflow() {
        let sets: [&[(f64, f64)]; 4] = [
            &[(-1e300, 0.0), (1e300, 0.0), (0.0, 0.0)],
            &[(-1e23, 1e23), (1e23, -1e23), (0.0, 0.0), (1e23, 1e23)],
            &[(-f64::MAX, f64::MAX), (f64::MAX, -f64::MAX), (0.0, 0.0)],
            &[
                (f64::NAN, f64::INFINITY),
                (f64::NEG_INFINITY, 1.0),
                (5.0, 5.0),
            ],
        ];
        for pts in sets {
            for cell in [1e-12, 2.0, 2500.0, 1e300] {
                let mut g = SpatialGrid::default();
                g.build(pts.len(), cell, |i| pts[i]);
                for &(x, y) in pts {
                    if x.is_finite() && y.is_finite() {
                        assert_superset(pts, &g, (x, y), 1.0);
                    }
                }
                // Huge and infinite radii stay bounded.
                assert_eq!(near(&g, 0.0, 0.0, f64::INFINITY).len(), pts.len());
                assert_eq!(near(&g, 0.0, 0.0, f64::MAX).len(), pts.len());
            }
        }
        // A tiny cell over a wide span (more cells than an i64 can count).
        let pts: Vec<(f64, f64)> = (0..100)
            .map(|i| (f64::from(i) * 1.2e5 - 6e6, f64::from(i % 7) * 1e6))
            .collect();
        let mut g = SpatialGrid::default();
        g.build(pts.len(), 1e-12, |i| pts[i]);
        for &q in &pts {
            assert_superset(&pts, &g, q, 1e-3);
        }
    }
}
