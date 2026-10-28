//! Swept closest approach between two points moving linearly over one tick.

use crate::Vec3;

/// When and how close two swept points come.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Approach {
    /// Time of closest approach in s, in `[0, dt]`.
    pub time: f64,
    /// Distance at that time in m.
    pub distance: f64,
}

/// Closest approach of `a` and `b` over `[0, dt]` seconds, each moving at constant velocity
/// from its position at time 0.
///
/// This is continuous collision detection: fast units that pass each other between ticks
/// still register. Equal velocities (or `dt <= 0`) give time 0.
pub fn closest_approach(a0: Vec3, va: Vec3, b0: Vec3, vb: Vec3, dt: f64) -> Approach {
    let r = b0 - a0;
    let w = vb - va;
    let w2 = w.length_squared();
    let dt = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
    let time = if w2 > 0.0 && w2.is_finite() {
        let t = -r.dot(w) / w2;
        if t.is_finite() { t.clamp(0.0, dt) } else { 0.0 }
    } else {
        0.0
    };
    Approach {
        time,
        distance: (r + w * time).length(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analytic_cases() {
        // Head-on along x: meet at t = 100 / 200 = 0.5 s.
        let a = closest_approach(
            Vec3::ZERO,
            Vec3::X * 100.0,
            Vec3::X * 100.0,
            Vec3::X * -100.0,
            1.0,
        );
        assert_eq!(a.time, 0.5);
        assert_eq!(a.distance, 0.0);

        // Passing with a 3 m lateral offset: closest at t = 0.5, distance 3.
        let a = closest_approach(
            Vec3::ZERO,
            Vec3::X * 100.0,
            Vec3::new(100.0, 3.0, 0.0),
            Vec3::X * -100.0,
            1.0,
        );
        assert_eq!(a.time, 0.5);
        assert!((a.distance - 3.0).abs() < 1e-12);

        // Closest point after the tick: clamped to dt.
        let a = closest_approach(Vec3::ZERO, Vec3::X, Vec3::X * 10.0, Vec3::ZERO, 2.0);
        assert_eq!(a.time, 2.0);
        assert_eq!(a.distance, 8.0);

        // Separating: clamped to 0.
        let a = closest_approach(Vec3::ZERO, -Vec3::X, Vec3::X * 10.0, Vec3::ZERO, 2.0);
        assert_eq!(a.time, 0.0);
        assert_eq!(a.distance, 10.0);

        // Same velocity: constant separation.
        let a = closest_approach(Vec3::ZERO, Vec3::X, Vec3::Z * 4.0, Vec3::X, 1.0);
        assert_eq!((a.time, a.distance), (0.0, 4.0));

        // Bad dt.
        let a = closest_approach(Vec3::ZERO, Vec3::X, Vec3::X, Vec3::ZERO, f64::NAN);
        assert_eq!((a.time, a.distance), (0.0, 1.0));
    }

    #[test]
    fn catches_a_skip_past_between_ticks() {
        // 3,000 m/s for 1/15 s is 200 m. The target sits 2 m off the path, mid-tick; both
        // endpoint distances are ~100 m.
        let dt = 1.0 / 15.0;
        let a0 = Vec3::ZERO;
        let va = Vec3::X * 3000.0;
        let b = Vec3::new(100.0, 2.0, 0.0);
        assert!((b - a0).length() > 99.0 && (b - (a0 + va * dt)).length() > 99.0);
        let a = closest_approach(a0, va, b, Vec3::ZERO, dt);
        assert!((a.distance - 2.0).abs() < 1e-9);
        assert!((a.time - dt / 2.0).abs() < 1e-12);
    }
}
