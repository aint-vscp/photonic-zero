//! SplitMix64, the deterministic pseudo-random generator the PZ wire format
//! is defined against.
//!
//! The transmitter never tells the receiver which source blocks a frame mixes
//! together; it only sends the frame index. Both sides derive the same block
//! selection by seeding this generator identically. That is what makes the
//! protocol rateless: any implementation, in any language, reproduces the same
//! stream from the same seed.
//!
//! SplitMix64 was chosen because it is a fixed sequence of 64-bit wrapping
//! adds, multiplies, xors and shifts. It has no platform-dependent behaviour
//! and is about ten lines to reimplement in C, JavaScript (via `BigInt`),
//! Python or Java.

/// A SplitMix64 generator.
#[derive(Debug, Clone)]
pub struct SplitMix64 {
    state: u64,
}

/// The SplitMix64 increment, the 64-bit golden ratio constant.
pub const GOLDEN_GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;

impl SplitMix64 {
    /// Create a generator from a raw 64-bit seed.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Seed the generator for one PZ frame.
    ///
    /// The session id occupies the high 32 bits and the frame index the low
    /// 32, so two concurrent sessions in the same field of view never draw the
    /// same block selections.
    #[must_use]
    pub const fn for_frame(session_id: u32, frame_index: u32) -> Self {
        Self::new(((session_id as u64) << 32) | (frame_index as u64))
    }

    /// Advance the generator and return the next 64-bit output.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(GOLDEN_GAMMA);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform double in `[0, 1)`, using the top 53 bits (the full mantissa).
    pub fn next_f64(&mut self) -> f64 {
        // 2^-53, exact in binary floating point.
        const SCALE: f64 = 1.0 / 9_007_199_254_740_992.0;
        ((self.next_u64() >> 11) as f64) * SCALE
    }

    /// Uniform integer in `[0, n)` by modular reduction.
    ///
    /// Modulo introduces a bias of order `n / 2^64`, which is negligible for
    /// the block counts PZ uses (`n` is at most a few tens of thousands). It is
    /// specified rather than corrected because every implementation must agree
    /// bit for bit, and plain modulo is the easiest rule to reproduce.
    ///
    /// # Panics
    /// Panics if `n` is zero.
    pub fn below(&mut self, n: u64) -> u64 {
        assert!(n != 0, "pz-fountain: bound must be non-zero");
        self.next_u64() % n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_reference_vectors() {
        // Reference outputs for SplitMix64 seeded with 0, as published with
        // the original algorithm. These pin the constants and shift amounts.
        let mut r = SplitMix64::new(0);
        assert_eq!(r.next_u64(), 0xE220_A839_7B1D_CDAF);
        assert_eq!(r.next_u64(), 0x6E78_9E6A_A1B9_65F4);
        assert_eq!(r.next_u64(), 0x06C4_5D18_8009_454F);
    }

    #[test]
    fn frame_seeding_is_distinct_per_session_and_frame() {
        let a = SplitMix64::for_frame(1, 0).next_u64();
        let b = SplitMix64::for_frame(2, 0).next_u64();
        let c = SplitMix64::for_frame(1, 1).next_u64();
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(b, c);
    }

    #[test]
    fn is_reproducible() {
        let mut a = SplitMix64::for_frame(0xDEAD, 42);
        let mut b = SplitMix64::for_frame(0xDEAD, 42);
        for _ in 0..1000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn f64_stays_in_unit_interval() {
        let mut r = SplitMix64::new(12345);
        let mut sum = 0.0;
        const N: usize = 20_000;
        for _ in 0..N {
            let v = r.next_f64();
            assert!((0.0..1.0).contains(&v), "out of range: {v}");
            sum += v;
        }
        // The mean of a uniform [0,1) sample should sit near 0.5.
        let mean = sum / N as f64;
        assert!((mean - 0.5).abs() < 0.02, "mean was {mean}");
    }

    #[test]
    fn below_is_in_range_and_covers_the_space() {
        let mut r = SplitMix64::new(7);
        let mut hits = [0usize; 10];
        for _ in 0..10_000 {
            let v = r.below(10) as usize;
            assert!(v < 10);
            hits[v] += 1;
        }
        // Every bucket should be hit a reasonable number of times.
        assert!(hits.iter().all(|&h| h > 700), "poor coverage: {hits:?}");
    }
}
