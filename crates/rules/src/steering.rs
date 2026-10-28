//! Turn-limited pursuit steering.

use crate::Vec3;

/// Position (m) and velocity (m/s) of a moving point.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Kinematic {
    pub position: Vec3,
    pub velocity: Vec3,
}

/// How a pursuer steers. All values are data.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PursuitParams {
    /// Constant speed in m/s. Zero or less (or not finite) means the pursuer stops.
    pub speed: f64,
    /// Maximum heading change in rad/s. Zero or less means it cannot turn.
    pub max_turn_rate: f64,
    /// Lead factor in `[0, 1]` (clamped). 0 aims at the target itself (pure pursuit); 1 aims at
    /// where the target will be after the pursuer's straight-line time to reach it.
    pub lead: f64,
}

/// The pursuer's velocity after one tick of `dt` seconds.
///
/// The pursuer aims at a lead point, `target + target_velocity * (lead * distance / speed)`, and
/// rotates its heading toward it by at most `max_turn_rate * dt` radians, keeping `speed`.
///
/// Degenerate inputs, all finite:
/// - zero or invalid speed returns zero velocity;
/// - a pursuer with no heading (zero velocity) points straight at the aim point, or east when
///   it is on top of it;
/// - an aim point on top of the pursuer keeps the current heading;
/// - a target directly behind turns in the horizontal plane (to the left), or about the east
///   axis when the heading is vertical.
pub fn pursuit_velocity(
    pursuer: Kinematic,
    target: Kinematic,
    params: PursuitParams,
    dt: f64,
) -> Vec3 {
    let speed = params.speed;
    if !(speed > 0.0 && speed.is_finite()) {
        return Vec3::ZERO;
    }
    let lead = if params.lead.is_finite() {
        params.lead.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let to_target = target.position - pursuer.position;
    let time_to_go = to_target.length() / speed;
    let mut aim = to_target;
    if lead > 0.0 && time_to_go.is_finite() {
        let lead_aim = to_target + target.velocity * (lead * time_to_go);
        if lead_aim.is_finite() {
            aim = lead_aim;
        }
    }
    let aim = aim.normalized();
    let heading = match (pursuer.velocity.normalized(), aim) {
        (Some(h), _) => h,
        (None, Some(a)) => return a * speed,
        (None, None) => return Vec3::X * speed,
    };
    let Some(aim) = aim else {
        return heading * speed;
    };

    let max_turn = params.max_turn_rate * dt;
    let max_turn = if max_turn.is_finite() {
        max_turn.max(0.0)
    } else {
        0.0
    };
    let cos = heading.dot(aim).clamp(-1.0, 1.0);
    // Component of the aim perpendicular to the heading: the direction to turn in.
    let perp = aim - heading * cos;
    let angle = perp.length().atan2(cos);
    if angle <= max_turn {
        return aim * speed;
    }
    let turn_dir = perp.normalized().unwrap_or_else(|| {
        // Target directly behind: turn left in the horizontal plane, or about east if vertical.
        Vec3::Z
            .cross(heading)
            .normalized()
            .unwrap_or_else(|| heading.cross(Vec3::X).normalized().unwrap_or(Vec3::X))
    });
    let dir = heading * max_turn.cos() + turn_dir * max_turn.sin();
    dir.normalized().unwrap_or(heading) * speed
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f64 = 1.0 / 15.0;

    fn k(p: Vec3, v: Vec3) -> Kinematic {
        Kinematic {
            position: p,
            velocity: v,
        }
    }

    fn params(speed: f64, max_turn_rate: f64, lead: f64) -> PursuitParams {
        PursuitParams {
            speed,
            max_turn_rate,
            lead,
        }
    }

    fn angle(a: Vec3, b: Vec3) -> f64 {
        a.cross(b).length().atan2(a.dot(b))
    }

    #[test]
    fn turns_by_at_most_the_limit_and_keeps_speed() {
        let p = k(Vec3::ZERO, Vec3::new(100.0, 0.0, 0.0));
        let t = k(Vec3::new(0.0, 1000.0, 0.0), Vec3::ZERO);
        let v = pursuit_velocity(p, t, params(100.0, 0.6, 0.0), DT);
        assert!((v.length() - 100.0).abs() < 1e-9);
        assert!((angle(p.velocity, v) - 0.6 * DT).abs() < 1e-12);
        assert!(v.y > 0.0, "turns toward the target");

        // Within the limit it points straight at the target.
        let t = k(Vec3::new(1000.0, 10.0, 0.0), Vec3::ZERO);
        let v = pursuit_velocity(p, t, params(100.0, 0.6, 0.0), DT);
        assert!(angle(v, t.position).abs() < 1e-12);
    }

    #[test]
    fn lead_aims_ahead_of_a_crossing_target() {
        let p = k(Vec3::ZERO, Vec3::new(0.0, 100.0, 0.0));
        let t = k(Vec3::new(0.0, 1000.0, 0.0), Vec3::new(50.0, 0.0, 0.0));
        let pure = pursuit_velocity(p, t, params(100.0, 10.0, 0.0), DT);
        let lead = pursuit_velocity(p, t, params(100.0, 10.0, 0.5), DT);
        assert!(pure.x.abs() < 1e-9);
        // Lead point: 1000 m / 100 m/s = 10 s; 0.5 * 10 s * 50 m/s = 250 m east.
        assert!((lead.x / lead.y - 0.25).abs() < 1e-12);
    }

    #[test]
    fn degenerate_inputs_are_finite() {
        let cases = [
            // Same position.
            (k(Vec3::ZERO, Vec3::X), k(Vec3::ZERO, Vec3::ZERO), 100.0),
            // Same position, no heading.
            (k(Vec3::ZERO, Vec3::ZERO), k(Vec3::ZERO, Vec3::ZERO), 100.0),
            // Zero speed.
            (k(Vec3::ZERO, Vec3::X), k(Vec3::X * 50.0, Vec3::ZERO), 0.0),
            // Target directly behind.
            (
                k(Vec3::ZERO, Vec3::X),
                k(Vec3::X * -50.0, Vec3::ZERO),
                100.0,
            ),
            // Target directly behind a vertical heading.
            (
                k(Vec3::ZERO, Vec3::Z),
                k(Vec3::Z * -50.0, Vec3::ZERO),
                100.0,
            ),
            // Huge lead with a huge target speed.
            (
                k(Vec3::ZERO, Vec3::X),
                k(Vec3::X * 1e300, Vec3::X * 1e300),
                1e-300,
            ),
        ];
        for (p, t, speed) in cases {
            for (rate, lead) in [(1.0, 0.0), (0.0, 1.0), (f64::INFINITY, 0.5), (-1.0, 2.0)] {
                let v = pursuit_velocity(p, t, params(speed, rate, lead), DT);
                assert!(v.is_finite(), "{p:?} {t:?} {speed} {rate} {lead} -> {v:?}");
            }
        }
        // Target behind turns by the limit rather than flipping.
        let p = k(Vec3::ZERO, Vec3::X * 10.0);
        let t = k(Vec3::X * -50.0, Vec3::ZERO);
        let v = pursuit_velocity(p, t, params(10.0, 1.5, 0.0), DT);
        assert!((angle(p.velocity, v) - 1.5 * DT).abs() < 1e-12);
        assert!(v.z.abs() < 1e-12, "turns in the horizontal plane");
        // No heading: points at the target.
        let v = pursuit_velocity(k(Vec3::ZERO, Vec3::ZERO), t, params(10.0, 0.0, 0.0), DT);
        assert_eq!(v, Vec3::X * -10.0);
        assert_eq!(
            pursuit_velocity(p, t, params(f64::NAN, 1.0, 0.0), DT),
            Vec3::ZERO
        );
    }
}
