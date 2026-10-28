//! Coarse heightfield terrain for line-of-sight checks.

use bevy_ecs::prelude::*;

/// A regular grid of terrain heights, sampled bilinearly, used to block line of sight.
///
/// Sample `(i, j)` sits at world `(origin_x + i * spacing_m, origin_y + j * spacing_m)` with
/// height `heights_m[j * nx + i]`. Outside the grid the nearest edge value applies. The
/// default is flat terrain, on which everything is visible.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct Terrain {
    origin_x: f64,
    origin_y: f64,
    spacing_m: f64,
    nx: u32,
    ny: u32,
    heights_m: Vec<f32>,
    los_samples: u32,
}

impl Terrain {
    /// Most line-of-sight samples per check that [`Terrain::heightfield`] accepts.
    pub const MAX_LOS_SAMPLES: u32 = 1024;

    /// Flat terrain: line of sight is always clear.
    pub fn flat() -> Terrain {
        Terrain::default()
    }

    /// A heightfield of `nx * ny` samples `spacing_m` apart, row-major from `(origin_x,
    /// origin_y)`. Line-of-sight checks test `los_samples` evenly spaced interior points.
    ///
    /// Returns `None` unless `nx, ny >= 1`, `heights_m.len() == nx * ny`, every height is
    /// finite, the origin and a positive spacing are finite, and `los_samples` is at most
    /// [`Terrain::MAX_LOS_SAMPLES`].
    pub fn heightfield(
        origin_x: f64,
        origin_y: f64,
        spacing_m: f64,
        nx: u32,
        ny: u32,
        heights_m: Vec<f32>,
        los_samples: u32,
    ) -> Option<Terrain> {
        let ok = nx >= 1
            && ny >= 1
            && heights_m.len() as u64 == u64::from(nx) * u64::from(ny)
            && heights_m.iter().all(|h| h.is_finite())
            && origin_x.is_finite()
            && origin_y.is_finite()
            && spacing_m > 0.0
            && spacing_m.is_finite()
            && los_samples <= Self::MAX_LOS_SAMPLES;
        ok.then_some(Terrain {
            origin_x,
            origin_y,
            spacing_m,
            nx,
            ny,
            heights_m,
            los_samples,
        })
    }

    /// Whether this is flat terrain (no heightfield).
    pub fn is_flat(&self) -> bool {
        self.heights_m.is_empty()
    }

    /// Terrain height in m at world `(x, y)`; 0 on flat terrain.
    pub fn height_at(&self, x: f64, y: f64) -> f64 {
        if self.is_flat() {
            return 0.0;
        }
        let max_i = f64::from(self.nx - 1);
        let max_j = f64::from(self.ny - 1);
        // NaN clamps to NaN; treat it as the origin sample.
        let gx = ((x - self.origin_x) / self.spacing_m).clamp(0.0, max_i);
        let gy = ((y - self.origin_y) / self.spacing_m).clamp(0.0, max_j);
        let (gx, gy) = (
            if gx.is_nan() { 0.0 } else { gx },
            if gy.is_nan() { 0.0 } else { gy },
        );
        let (i0, j0) = (gx.floor() as u32, gy.floor() as u32);
        let (i1, j1) = ((i0 + 1).min(self.nx - 1), (j0 + 1).min(self.ny - 1));
        let (fx, fy) = (gx - f64::from(i0), gy - f64::from(j0));
        let h = |i: u32, j: u32| f64::from(self.heights_m[(j * self.nx + i) as usize]);
        let a = h(i0, j0) + (h(i1, j0) - h(i0, j0)) * fx;
        let b = h(i0, j1) + (h(i1, j1) - h(i0, j1)) * fx;
        a + (b - a) * fy
    }

    /// Whether the straight segment from `a` to `b` (world m) clears the terrain.
    ///
    /// Tests `los_samples` points at `t = k / (los_samples + 1)`, `k = 1..=los_samples`: the
    /// line is blocked if the terrain at any of them is higher than the line. The end points
    /// themselves are not tested, so units sitting on the ground can still see each other.
    /// Flat terrain is always clear.
    pub fn line_of_sight(&self, a: [f64; 3], b: [f64; 3]) -> bool {
        if self.is_flat() {
            return true;
        }
        let inv = 1.0 / (f64::from(self.los_samples) + 1.0);
        for k in 1..=self.los_samples {
            let t = f64::from(k) * inv;
            let x = a[0] + (b[0] - a[0]) * t;
            let y = a[1] + (b[1] - a[1]) * t;
            let z = a[2] + (b[2] - a[2]) * t;
            if self.height_at(x, y) > z {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bilinear_heights_and_ridge() {
        // 3 x 2 grid, 100 m spacing: a 50 m ridge along x = 100.
        let t = Terrain::heightfield(
            0.0,
            0.0,
            100.0,
            3,
            2,
            vec![0.0, 50.0, 0.0, 0.0, 50.0, 0.0],
            9,
        )
        .unwrap();
        assert_eq!(t.height_at(100.0, 50.0), 50.0);
        assert_eq!(t.height_at(50.0, 0.0), 25.0);
        assert_eq!(t.height_at(-1e6, 1e6), 0.0);
        assert!(!t.line_of_sight([0.0, 50.0, 10.0], [200.0, 50.0, 10.0]));
        assert!(t.line_of_sight([0.0, 50.0, 60.0], [200.0, 50.0, 60.0]));
        assert!(Terrain::flat().line_of_sight([0.0; 3], [1e6, 0.0, -1e6]));
        assert!(Terrain::heightfield(0.0, 0.0, 1.0, 2, 2, vec![0.0; 3], 4).is_none());
        assert!(Terrain::heightfield(0.0, 0.0, 0.0, 1, 1, vec![0.0], 4).is_none());
        let max = Terrain::MAX_LOS_SAMPLES;
        assert!(Terrain::heightfield(0.0, 0.0, 1.0, 1, 1, vec![0.0], max).is_some());
        assert!(Terrain::heightfield(0.0, 0.0, 1.0, 1, 1, vec![0.0], max + 1).is_none());
        assert!(Terrain::heightfield(0.0, 0.0, 1.0, 1, 1, vec![0.0], u32::MAX).is_none());
    }
}
