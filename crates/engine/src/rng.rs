//! Small seeded PRNG (PCG32, XSH-RR variant). Deterministic across platforms.

/// PCG32 generator: 64-bit state, 32-bit output.
#[derive(Clone, Debug)]
pub struct Pcg32 {
    state: u64,
    inc: u64,
}

impl Pcg32 {
    const MULT: u64 = 6_364_136_223_846_793_005;

    /// Seeds the generator. `stream` selects one of 2^63 independent sequences.
    pub fn new(seed: u64, stream: u64) -> Self {
        let mut rng = Pcg32 {
            state: 0,
            inc: (stream << 1) | 1,
        };
        rng.next_u32();
        rng.state = rng.state.wrapping_add(seed);
        rng.next_u32();
        rng
    }

    /// The raw `(state, increment)`, for snapshots.
    pub fn state(&self) -> (u64, u64) {
        (self.state, self.inc)
    }

    /// A generator continuing from [`Pcg32::state`]. The increment is forced odd, as PCG
    /// requires.
    pub fn from_state((state, inc): (u64, u64)) -> Pcg32 {
        Pcg32 {
            state,
            inc: inc | 1,
        }
    }

    pub fn next_u32(&mut self) -> u32 {
        let old = self.state;
        self.state = old.wrapping_mul(Self::MULT).wrapping_add(self.inc);
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        let rot = (old >> 59) as u32;
        xorshifted.rotate_right(rot)
    }

    /// Uniform in `[0, 1)` with 53 bits of precision.
    pub fn next_f64(&mut self) -> f64 {
        let hi = u64::from(self.next_u32()) << 21;
        let lo = u64::from(self.next_u32()) >> 11;
        ((hi | lo) as f64) * (1.0 / (1u64 << 53) as f64)
    }

    /// Uniform in `[lo, hi)`.
    pub fn range_f64(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next_f64()
    }

    /// Uniform in `[0, n)` (Lemire's method, unbiased). `n` must be non-zero.
    pub fn below(&mut self, n: u32) -> u32 {
        let mut m = u64::from(self.next_u32()) * u64::from(n);
        if (m as u32) < n {
            let threshold = n.wrapping_neg() % n;
            while (m as u32) < threshold {
                m = u64::from(self.next_u32()) * u64::from(n);
            }
        }
        (m >> 32) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_vector() {
        // Reference output of pcg32_srandom_r(42, 54) from the PCG C library demo.
        let mut rng = Pcg32::new(42, 54);
        let expected = [
            0xa15c02b7, 0x7b47f409, 0xba1d3330, 0x83d2f293, 0xbfa4784b, 0xcbed606e,
        ];
        for e in expected {
            assert_eq!(rng.next_u32(), e);
        }
    }

    #[test]
    fn floats_in_range() {
        let mut rng = Pcg32::new(1, 1);
        for _ in 0..10_000 {
            let f = rng.next_f64();
            assert!((0.0..1.0).contains(&f));
            assert!(rng.below(7) < 7);
        }
    }
}
