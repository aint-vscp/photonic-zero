//! The robust soliton degree distribution and deterministic droplet planning.
//!
//! An LT droplet is the XOR of some number of source blocks. *How many* is
//! drawn from the robust soliton distribution, which is engineered so that the
//! peeling decoder almost always has exactly one degree-1 droplet available to
//! consume at each step: enough degree-1 droplets to get started, a spike near
//! `K/R` to cover the blocks that would otherwise be missed, and a long thin
//! tail of high-degree droplets to mop up.
//!
//! *Which* blocks are chosen is decided by [`crate::prng::SplitMix64`] seeded
//! from the session id and the frame index, so the receiver reconstructs the
//! selection from the frame header alone.

use crate::fmath;
use crate::prng::SplitMix64;
use alloc::vec;
use alloc::vec::Vec;

/// Tuning parameters for the robust soliton distribution.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SolitonParams {
    /// Scaling constant on the spike position. Smaller values move the spike
    /// to higher degrees. Typical range 0.01 to 0.2.
    pub c: f64,
    /// Target decode failure probability after `K + overhead` droplets.
    pub delta: f64,
}

impl Default for SolitonParams {
    /// `c = 0.1`, `delta = 0.05`.
    ///
    /// These are the values PZ transmits with unless a profile overrides them.
    /// They favour low overhead at the small block counts (tens to low
    /// thousands) that a screen-to-camera link actually produces.
    fn default() -> Self {
        Self {
            c: 0.1,
            delta: 0.05,
        }
    }
}

/// A cached cumulative degree distribution for a fixed block count.
#[derive(Debug, Clone)]
pub struct DegreeTable {
    k: usize,
    /// `cdf[i]` is the probability that a drawn degree is at most `i + 1`.
    cdf: Vec<f64>,
}

impl DegreeTable {
    /// Build the robust soliton table for `k` source blocks.
    ///
    /// `k` must be at least 1.
    #[must_use]
    pub fn new(k: usize, params: SolitonParams) -> Self {
        assert!(k > 0, "pz-fountain: block count must be non-zero");
        if k == 1 {
            return Self { k, cdf: vec![1.0] };
        }

        let kf = k as f64;

        // Ideal soliton: rho(1) = 1/K, rho(i) = 1/(i(i-1)).
        let mut w = vec![0.0f64; k + 1]; // 1-based
        w[1] = 1.0 / kf;
        for (i, weight) in w.iter_mut().enumerate().skip(2) {
            let fi = i as f64;
            *weight = 1.0 / (fi * (fi - 1.0));
        }

        // Robust component: R = c * ln(K/delta) * sqrt(K).
        let r = params.c * fmath::ln(kf / params.delta) * fmath::sqrt(kf);
        if r > 0.0 {
            // pivot = floor(K/R), the degree carrying the spike.
            let pivot = (kf / r) as usize;
            if pivot >= 1 {
                for (i, weight) in w.iter_mut().enumerate().take(pivot.min(k + 1)).skip(1) {
                    *weight += r / ((i as f64) * kf);
                }
                if pivot <= k {
                    w[pivot] += r * fmath::ln(r / params.delta) / kf;
                }
            }
        }

        let total: f64 = w[1..=k].iter().sum();
        let mut cdf = vec![0.0f64; k];
        let mut acc = 0.0;
        for i in 1..=k {
            acc += w[i] / total;
            cdf[i - 1] = acc;
        }
        // Guard against accumulated rounding leaving the last entry below 1.
        cdf[k - 1] = 1.0;

        Self { k, cdf }
    }

    /// Number of source blocks this table was built for.
    #[must_use]
    pub const fn block_count(&self) -> usize {
        self.k
    }

    /// The cumulative distribution, for inspection and conformance testing.
    #[must_use]
    pub fn cdf(&self) -> &[f64] {
        &self.cdf
    }

    /// Draw a degree in `[1, k]`.
    pub fn sample_degree(&self, rng: &mut SplitMix64) -> usize {
        let u = rng.next_f64();
        // Smallest degree whose cumulative probability exceeds u.
        for (i, &c) in self.cdf.iter().enumerate() {
            if u < c {
                return i + 1;
            }
        }
        self.k
    }

    /// The set of source blocks a given frame mixes together.
    ///
    /// Frames `0 .. k` are *systematic*: frame `i` carries source block `i`
    /// verbatim. Under good conditions the receiver therefore finishes in
    /// exactly `k` frames with no coding overhead at all, while frames from
    /// `k` onward are rateless repair droplets that recover from any pattern
    /// of loss.
    ///
    /// The returned indices are distinct and sorted ascending.
    ///
    /// # Session isolation
    ///
    /// The systematic prefix is deliberately **independent of `session_id`**:
    /// frame `i` carries block `i` for every session. Only the repair droplets
    /// past frame `k` are session-bound. A decoder that blindly absorbs frames
    /// from a foreign session will therefore happily consume its systematic
    /// prefix.
    ///
    /// This layer does not, and cannot, police that. Callers **must** check the
    /// session id in the frame header and drop foreign frames before calling
    /// [`crate::Decoder::absorb`]. `pz-core` does exactly that.
    #[must_use]
    pub fn plan(&self, session_id: u32, frame_index: u32) -> Vec<u32> {
        if (frame_index as usize) < self.k {
            return vec![frame_index];
        }

        let mut rng = SplitMix64::for_frame(session_id, frame_index);
        let degree = self.sample_degree(&mut rng).min(self.k).max(1);

        let mut chosen = Vec::with_capacity(degree);
        let mut seen = vec![false; self.k];
        while chosen.len() < degree {
            let idx = rng.below(self.k as u64) as usize;
            if !seen[idx] {
                seen[idx] = true;
                chosen.push(idx as u32);
            }
        }
        chosen.sort_unstable();
        chosen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cdf_is_monotonic_and_normalised() {
        for k in [1usize, 2, 5, 17, 100, 1000] {
            let t = DegreeTable::new(k, SolitonParams::default());
            assert_eq!(t.cdf().len(), k);
            let mut prev = 0.0;
            for (i, &c) in t.cdf().iter().enumerate() {
                assert!(c >= prev - 1e-12, "k={k} not monotonic at {i}");
                assert!((0.0..=1.0 + 1e-12).contains(&c), "k={k} cdf {c} at {i}");
                prev = c;
            }
            assert!(
                (t.cdf()[k - 1] - 1.0).abs() < 1e-12,
                "k={k} does not reach 1"
            );
        }
    }

    #[test]
    fn degree_one_is_common_enough_to_bootstrap() {
        // Without a healthy supply of degree-1 droplets the peeling decoder
        // can never start.
        let k = 200;
        let t = DegreeTable::new(k, SolitonParams::default());
        assert!(
            t.cdf()[0] > 0.005,
            "P(degree = 1) too small: {}",
            t.cdf()[0]
        );
    }

    #[test]
    fn sampled_degrees_are_in_range() {
        let k = 64;
        let t = DegreeTable::new(k, SolitonParams::default());
        let mut rng = SplitMix64::new(99);
        for _ in 0..20_000 {
            let d = t.sample_degree(&mut rng);
            assert!((1..=k).contains(&d), "degree {d} out of range");
        }
    }

    #[test]
    fn systematic_prefix_maps_frame_to_its_own_block() {
        let t = DegreeTable::new(50, SolitonParams::default());
        for i in 0..50u32 {
            assert_eq!(t.plan(1234, i), vec![i]);
        }
    }

    #[test]
    fn repair_droplets_are_distinct_sorted_and_in_range() {
        let k = 80;
        let t = DegreeTable::new(k, SolitonParams::default());
        for frame in (k as u32)..(k as u32 + 2000) {
            let plan = t.plan(7, frame);
            assert!(!plan.is_empty(), "empty plan at frame {frame}");
            assert!(plan.len() <= k);
            assert!(plan.iter().all(|&i| (i as usize) < k));
            let mut sorted = plan.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted, plan, "plan not sorted/distinct at frame {frame}");
        }
    }

    #[test]
    fn plans_are_deterministic_across_calls() {
        let t = DegreeTable::new(30, SolitonParams::default());
        for frame in 30..200u32 {
            assert_eq!(t.plan(42, frame), t.plan(42, frame));
        }
    }

    #[test]
    fn different_sessions_produce_different_plans() {
        let t = DegreeTable::new(64, SolitonParams::default());
        let mut differences = 0;
        for frame in 64..200u32 {
            if t.plan(1, frame) != t.plan(2, frame) {
                differences += 1;
            }
        }
        assert!(differences > 100, "sessions barely differ: {differences}");
    }

    #[test]
    fn single_block_always_plans_block_zero() {
        let t = DegreeTable::new(1, SolitonParams::default());
        for frame in 0..50u32 {
            assert_eq!(t.plan(0, frame), vec![0]);
        }
    }
}
