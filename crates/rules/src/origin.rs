//! Floating origin: `f64` world coordinates to `f32` coordinates relative to a local origin.

use crate::Vec3;

/// `world - origin` in m, as `f32`.
///
/// The subtraction happens in `f64` first, so precision is relative to the origin rather than
/// to the world's zero: anywhere within ~100 km of the origin the error stays under 1 cm.
#[inline]
pub fn to_local_f32(world: f64, origin: f64) -> f32 {
    (world - origin) as f32
}

/// [`to_local_f32`] per component.
#[inline]
pub fn to_local(world: Vec3, origin: Vec3) -> [f32; 3] {
    [
        to_local_f32(world.x, origin.x),
        to_local_f32(world.y, origin.y),
        to_local_f32(world.z, origin.z),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SplitMix64, so the property test needs no dependency.
    fn next(state: &mut u64) -> f64 {
        *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 53) as f64
    }

    #[test]
    fn error_under_a_centimetre_across_ranges() {
        let mut s = 0x5eed;
        let mut worst: f64 = 0.0;
        for i in 0..200_000 {
            // Spread origins over magnitudes from 1 m to 10,000 km, and offsets up to 100 km.
            let origin_mag = 10f64.powf(7.0 * next(&mut s));
            let offset_mag = 10f64.powf(5.0 * next(&mut s)) * if i % 7 == 0 { 0.0 } else { 1.0 };
            let o = Vec3::new(
                origin_mag * (2.0 * next(&mut s) - 1.0),
                origin_mag * (2.0 * next(&mut s) - 1.0),
                origin_mag * (2.0 * next(&mut s) - 1.0),
            );
            let d = Vec3::new(
                offset_mag * (2.0 * next(&mut s) - 1.0),
                offset_mag * (2.0 * next(&mut s) - 1.0),
                offset_mag * (2.0 * next(&mut s) - 1.0),
            );
            // Compare against the intended offset, so f64 rounding of `o + d` counts too.
            let l = to_local(o + d, o);
            for (got, want) in l.iter().zip([d.x, d.y, d.z]) {
                worst = worst.max((f64::from(*got) - want).abs());
            }
        }
        assert!(worst < 0.01, "worst floating-origin error {worst} m");
    }

    #[test]
    fn matches_the_engine_cast() {
        assert_eq!(to_local_f32(1_000_000.25, 1_000_000.0), 0.25);
        assert_eq!(to_local_f32(-3.0, 2.0), -5.0);
        assert_eq!(
            to_local(Vec3::new(1.0, 2.0, 3.0), Vec3::ZERO),
            [1.0, 2.0, 3.0]
        );
    }
}
