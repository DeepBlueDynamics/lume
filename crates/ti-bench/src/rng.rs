//! Deterministic PRNG for the synthetic generator.
//!
//! SplitMix64, hand-rolled so ti-bench needs no `rand` dependency. Seeded from a
//! single master seed; the generator consumes it in a fixed iteration order, which
//! is what makes byte-identical regeneration possible.

#[derive(Clone, Copy)]
pub struct SplitMix64(u64);

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        SplitMix64(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform double in [0, 1).
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    /// Uniform f64 in [lo, hi).
    pub fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next_f64()
    }

    /// Uniform integer in [0, n) for n > 0.
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }

    /// Sample a value from a small gaussian-like distribution centered on `center`
    /// with `spread` as a rough standard deviation, clamped to `[lo, hi]`.
    pub fn gauss(&mut self, center: f64, spread: f64, lo: f64, hi: f64) -> f64 {
        // Sum of 3 uniforms approximates a normal distribution.
        let u = self.next_f64() + self.next_f64() + self.next_f64();
        let z = (u - 1.5) * 2.0; // approx N(0,1)
        (center + z * spread).clamp(lo, hi)
    }
}
