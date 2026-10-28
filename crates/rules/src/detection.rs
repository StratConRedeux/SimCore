//! Detection range and signal falloff.

use crate::Vec3;

/// Range in m at which a passive sensor hears an emitter of `power_w`.
///
/// `range = base_range_m * sqrt(power_w / reference_power_w)`, clamped to `[0, max_range_m]`:
/// a sensor hears an emitter of `reference_power_w` out to `base_range_m`. A non-positive or
/// non-finite power, reference power or base range gives 0, as does a non-finite max range,
/// except that an infinite power or base range (with the other inputs valid) reaches the max
/// range. An undefined ratio (both infinite, or an infinite base over an infinite reference)
/// gives 0.
pub fn detection_range(
    base_range_m: f64,
    power_w: f64,
    reference_power_w: f64,
    max_range_m: f64,
) -> f64 {
    if !(base_range_m > 0.0 && power_w > 0.0 && reference_power_w > 0.0 && max_range_m > 0.0)
        || !max_range_m.is_finite()
    {
        return 0.0;
    }
    let range = base_range_m * (power_w / reference_power_w).sqrt();
    // inf / inf or inf * 0 is NaN, which min() would turn into max_range_m.
    if range.is_nan() {
        return 0.0;
    }
    range.min(max_range_m)
}

/// Whether `emitter` is within `range_m` of `sensor` (3D distance, inclusive).
#[inline]
pub fn in_detection_range(sensor: Vec3, emitter: Vec3, range_m: f64) -> bool {
    (emitter - sensor).length_squared() <= range_m * range_m
}

/// Inverse-square signal strength at `distance_m` from an emitter of `power_w`:
/// `power_w / max(distance_m, d_min_m)^2`, in W/m^2 (unnormalised, no 4π).
///
/// `d_min_m` keeps the value bounded near the emitter. Returns 0 when the clamped distance is
/// not positive or the result is not finite.
#[inline]
pub fn signal_strength(power_w: f64, distance_m: f64, d_min_m: f64) -> f64 {
    let d = distance_m.max(d_min_m);
    if d.is_nan() || d <= 0.0 {
        return 0.0;
    }
    let s = power_w / (d * d);
    if s.is_finite() { s } else { 0.0 }
}

/// [`signal_strength`] from a squared distance, `power_w / max(distance_sq_m2, d_min_m^2)`,
/// which saves a square root per pair. Equal to it up to rounding.
#[inline]
pub fn signal_strength_sq(power_w: f64, distance_sq_m2: f64, d_min_m: f64) -> f64 {
    let d_min = d_min_m.max(0.0);
    // A zero or NaN distance gives an infinite or NaN quotient, mapped to 0 by the one check.
    let s = power_w / distance_sq_m2.max(d_min * d_min);
    if s.is_finite() { s } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_scales_with_sqrt_power_and_clamps() {
        assert_eq!(detection_range(1000.0, 10.0, 10.0, 1e9), 1000.0);
        assert_eq!(detection_range(1000.0, 40.0, 10.0, 1e9), 2000.0);
        assert_eq!(detection_range(1000.0, 2.5, 10.0, 1e9), 500.0);
        assert_eq!(detection_range(1000.0, 1e6, 10.0, 5000.0), 5000.0);
        assert_eq!(detection_range(1000.0, f64::INFINITY, 10.0, 5000.0), 5000.0);
        for bad in [0.0, -1.0, f64::NAN] {
            assert_eq!(detection_range(bad, 10.0, 10.0, 1e9), 0.0);
            assert_eq!(detection_range(1000.0, bad, 10.0, 1e9), 0.0);
            assert_eq!(detection_range(1000.0, 10.0, bad, 1e9), 0.0);
            assert_eq!(detection_range(1000.0, 10.0, 10.0, bad), 0.0);
        }
        assert_eq!(detection_range(1000.0, 10.0, 10.0, f64::INFINITY), 0.0);
        let inf = f64::INFINITY;
        assert_eq!(detection_range(1000.0, 10.0, inf, 5000.0), 0.0);
        assert_eq!(detection_range(1000.0, inf, inf, 5000.0), 0.0);
        assert_eq!(detection_range(inf, 10.0, inf, 5000.0), 0.0);
        assert_eq!(detection_range(inf, 10.0, 10.0, 5000.0), 5000.0);

        let s = Vec3::ZERO;
        assert!(in_detection_range(s, Vec3::new(600.0, 800.0, 0.0), 1000.0));
        assert!(!in_detection_range(s, Vec3::new(600.0, 800.0, 1.0), 1000.0));
    }

    #[test]
    fn inverse_square_falloff() {
        assert_eq!(signal_strength(100.0, 10.0, 1.0), 1.0);
        assert_eq!(signal_strength(100.0, 5.0, 1.0), 4.0);
        assert_eq!(signal_strength(100.0, 0.0, 2.0), 25.0);
        assert_eq!(signal_strength(100.0, 0.5, 2.0), 25.0);
        assert_eq!(signal_strength(100.0, 0.0, 0.0), 0.0);
        assert_eq!(signal_strength(100.0, f64::NAN, f64::NAN), 0.0);
        for (d, d_min) in [
            (10.0, 1.0),
            (5.0, 1.0),
            (0.0, 2.0),
            (0.5, 2.0),
            (0.0, 0.0),
            (3.0, -1.0),
        ] {
            assert_eq!(
                signal_strength_sq(100.0, d * d, d_min),
                signal_strength(100.0, d, d_min),
                "{d} {d_min}"
            );
        }
    }
}
