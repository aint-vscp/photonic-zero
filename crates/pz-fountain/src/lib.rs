//! Rateless LT fountain coding for the Photonic Zero (PZ) optical protocol.
//!
//! This is the *temporal* half of PZ's error correction: it repairs whole
//! frames the camera never saw. `pz-fec` repairs damage *inside* a frame.
//!
//! # Why a fountain code
//!
//! A screen refreshing at 60 Hz and a camera capturing at "30 fps" are not
//! synchronised, and never will be. The camera drops frames when the OS
//! schedules something else, when autofocus hunts, when a hand shakes. A
//! conventional chunked transfer would need a back channel to say "resend
//! chunk 47" - but a screen cannot hear.
//!
//! A fountain code removes the question. The transmitter emits an endless
//! stream of *droplets*, each an XOR of a pseudo-randomly chosen subset of the
//! source blocks. The receiver does not care *which* droplets it catches, only
//! *how many*: collect a little more than `K` and the message falls out.
//!
//! PZ sends the first `K` frames systematically (frame `i` carries block `i`
//! verbatim), so a clean capture finishes with zero coding overhead, and only
//! pays for the fountain when frames are actually lost.
//!
//! # Example
//!
//! ```
//! use pz_fountain::{Decoder, Encoder, SolitonParams};
//!
//! let message = b"the quick brown fox jumps over the lazy dog, repeatedly";
//! let params = SolitonParams::default();
//! let enc = Encoder::new(message, 8, 0xABCD, params).unwrap();
//!
//! let mut dec = Decoder::new(enc.block_count(), 8, message.len(), 0xABCD, params).unwrap();
//!
//! // Feed frames in, dropping every third one to simulate a shaky camera.
//! let mut frame = 0u32;
//! while !dec.is_complete() {
//!     if frame % 3 != 2 {
//!         dec.absorb(frame, &enc.droplet(frame)).unwrap();
//!     }
//!     frame += 1;
//!     assert!(frame < 10_000, "should have decoded long ago");
//! }
//!
//! assert_eq!(dec.take().unwrap(), message);
//! ```
//!
//! # `no_std`
//!
//! Disable the `std` feature to build against `core` + `alloc` only. The
//! transcendental functions the degree distribution needs are implemented in
//! [`fmath`] rather than pulled from the platform math library, both to avoid
//! the dependency and to guarantee bit-identical results across languages.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

#[cfg(feature = "std")]
extern crate std;

pub mod fmath;
pub mod prng;
pub mod soliton;

use alloc::vec;
use alloc::vec::Vec;

pub use prng::SplitMix64;
pub use soliton::{DegreeTable, SolitonParams};

/// Errors produced by the fountain codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FountainError {
    /// A droplet did not have the block size this decoder expects.
    WrongBlockSize,
    /// The block size or block count was zero.
    InvalidParams,
}

impl core::fmt::Display for FountainError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            Self::WrongBlockSize => "droplet length does not match the block size",
            Self::InvalidParams => "block size and block count must be non-zero",
        };
        f.write_str(s)
    }
}

#[cfg(feature = "std")]
impl std::error::Error for FountainError {}

/// How far along a [`Decoder`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeState {
    /// More droplets are needed.
    Incomplete {
        /// Source blocks recovered so far.
        recovered: usize,
        /// Source blocks in the message.
        total: usize,
    },
    /// Every source block has been recovered.
    Complete,
}

#[inline]
fn xor_into(dst: &mut [u8], src: &[u8]) {
    for (d, s) in dst.iter_mut().zip(src.iter()) {
        *d ^= *s;
    }
}

/// Splits a message into blocks and emits an endless stream of droplets.
#[derive(Debug, Clone)]
pub struct Encoder {
    blocks: Vec<u8>,
    block_size: usize,
    block_count: usize,
    session_id: u32,
    table: DegreeTable,
    payload_len: usize,
}

impl Encoder {
    /// Split `payload` into `block_size` byte blocks, zero-padding the last.
    ///
    /// # Errors
    /// Returns [`FountainError::InvalidParams`] if `block_size` is zero or the
    /// payload is empty.
    pub fn new(
        payload: &[u8],
        block_size: usize,
        session_id: u32,
        params: SolitonParams,
    ) -> Result<Self, FountainError> {
        if block_size == 0 || payload.is_empty() {
            return Err(FountainError::InvalidParams);
        }
        let block_count = payload.len().div_ceil(block_size);
        let mut blocks = vec![0u8; block_count * block_size];
        blocks[..payload.len()].copy_from_slice(payload);
        Ok(Self {
            blocks,
            block_size,
            block_count,
            session_id,
            table: DegreeTable::new(block_count, params),
            payload_len: payload.len(),
        })
    }

    /// Number of source blocks, i.e. the minimum number of frames a receiver
    /// needs under perfect conditions.
    #[must_use]
    pub const fn block_count(&self) -> usize {
        self.block_count
    }

    /// Size of each block in bytes.
    #[must_use]
    pub const fn block_size(&self) -> usize {
        self.block_size
    }

    /// Length of the original payload before zero padding.
    #[must_use]
    pub const fn payload_len(&self) -> usize {
        self.payload_len
    }

    /// The session id these droplets are bound to.
    #[must_use]
    pub const fn session_id(&self) -> u32 {
        self.session_id
    }

    /// Produce the droplet for `frame_index`. Defined for every `u32`.
    #[must_use]
    pub fn droplet(&self, frame_index: u32) -> Vec<u8> {
        let plan = self.table.plan(self.session_id, frame_index);
        let mut out = vec![0u8; self.block_size];
        for &i in &plan {
            let start = (i as usize) * self.block_size;
            xor_into(&mut out, &self.blocks[start..start + self.block_size]);
        }
        out
    }

    /// The block indices mixed into `frame_index`, for diagnostics.
    #[must_use]
    pub fn plan(&self, frame_index: u32) -> Vec<u32> {
        self.table.plan(self.session_id, frame_index)
    }
}

/// A droplet that could not be resolved yet.
#[derive(Debug, Clone)]
struct Pending {
    /// Source blocks still unknown in this droplet.
    indices: Vec<u32>,
    /// The droplet with all currently known blocks already XORed out.
    data: Vec<u8>,
}

/// Collects droplets until the message can be reconstructed.
///
/// The decoder runs the standard peeling (belief propagation) algorithm: any
/// droplet that reduces to a single unknown block *is* that block, which may
/// unlock further droplets in a cascade.
#[derive(Debug, Clone)]
pub struct Decoder {
    block_count: usize,
    block_size: usize,
    payload_len: usize,
    session_id: u32,
    table: DegreeTable,
    solved: Vec<Option<Vec<u8>>>,
    solved_count: usize,
    pending: Vec<Option<Pending>>,
    /// For each source block, the pending slots that still reference it.
    refs: Vec<Vec<usize>>,
    absorbed: usize,
    useful: usize,
}

impl Decoder {
    /// Create a decoder for a message of `payload_len` bytes carried in
    /// `block_count` blocks of `block_size` bytes.
    ///
    /// # Errors
    /// Returns [`FountainError::InvalidParams`] if any size is zero.
    pub fn new(
        block_count: usize,
        block_size: usize,
        payload_len: usize,
        session_id: u32,
        params: SolitonParams,
    ) -> Result<Self, FountainError> {
        if block_count == 0 || block_size == 0 {
            return Err(FountainError::InvalidParams);
        }
        Ok(Self {
            block_count,
            block_size,
            payload_len,
            session_id,
            table: DegreeTable::new(block_count, params),
            solved: vec![None; block_count],
            solved_count: 0,
            pending: Vec::new(),
            refs: vec![Vec::new(); block_count],
            absorbed: 0,
            useful: 0,
        })
    }

    /// Number of source blocks in the message.
    #[must_use]
    pub const fn block_count(&self) -> usize {
        self.block_count
    }

    /// Source blocks recovered so far.
    #[must_use]
    pub const fn recovered(&self) -> usize {
        self.solved_count
    }

    /// Total droplets fed in, including redundant ones.
    #[must_use]
    pub const fn absorbed(&self) -> usize {
        self.absorbed
    }

    /// Droplets that carried at least some new information.
    #[must_use]
    pub const fn useful(&self) -> usize {
        self.useful
    }

    /// Fraction of the message recovered, in `[0, 1]`.
    #[must_use]
    pub fn progress(&self) -> f64 {
        self.solved_count as f64 / self.block_count as f64
    }

    /// Whether every source block has been recovered.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.solved_count == self.block_count
    }

    /// Feed in one droplet.
    ///
    /// Droplets may arrive in any order, may be duplicated, and may be missing
    /// entirely. Feeding the same frame twice is harmless.
    ///
    /// # Errors
    /// Returns [`FountainError::WrongBlockSize`] if the droplet length does
    /// not match.
    pub fn absorb(
        &mut self,
        frame_index: u32,
        droplet: &[u8],
    ) -> Result<DecodeState, FountainError> {
        if droplet.len() != self.block_size {
            return Err(FountainError::WrongBlockSize);
        }
        self.absorbed += 1;
        if self.is_complete() {
            return Ok(DecodeState::Complete);
        }

        let mut indices = self.table.plan(self.session_id, frame_index);
        let mut data = droplet.to_vec();

        // Fold out everything we already know.
        let solved = &self.solved;
        indices.retain(|&i| match &solved[i as usize] {
            Some(block) => {
                xor_into(&mut data, block);
                false
            }
            None => true,
        });

        if indices.is_empty() {
            // Entirely redundant.
            return Ok(self.state());
        }
        self.useful += 1;

        let mut queue: Vec<usize> = Vec::new();
        if indices.len() == 1 {
            let idx = indices[0] as usize;
            self.solve(idx, data, &mut queue);
        } else {
            let slot = self.pending.len();
            for &i in &indices {
                self.refs[i as usize].push(slot);
            }
            self.pending.push(Some(Pending { indices, data }));
        }

        self.propagate(&mut queue);
        Ok(self.state())
    }

    fn solve(&mut self, index: usize, data: Vec<u8>, queue: &mut Vec<usize>) {
        if self.solved[index].is_none() {
            self.solved[index] = Some(data);
            self.solved_count += 1;
            queue.push(index);
        }
    }

    /// Cascade newly solved blocks through the pending droplets.
    fn propagate(&mut self, queue: &mut Vec<usize>) {
        while let Some(index) = queue.pop() {
            let block = match &self.solved[index] {
                Some(b) => b.clone(),
                None => continue,
            };
            let slots = core::mem::take(&mut self.refs[index]);
            for slot in slots {
                let Some(entry) = self.pending[slot].as_mut() else {
                    continue;
                };
                if let Some(pos) = entry.indices.iter().position(|&x| x as usize == index) {
                    entry.indices.swap_remove(pos);
                    xor_into(&mut entry.data, &block);
                }
                match entry.indices.len() {
                    0 => {
                        // Fully explained by blocks we already had.
                        self.pending[slot] = None;
                    }
                    1 => {
                        let taken = self.pending[slot].take().expect("slot occupied");
                        let target = taken.indices[0] as usize;
                        self.solve(target, taken.data, queue);
                    }
                    _ => {}
                }
            }
        }
    }

    fn state(&self) -> DecodeState {
        if self.is_complete() {
            DecodeState::Complete
        } else {
            DecodeState::Incomplete {
                recovered: self.solved_count,
                total: self.block_count,
            }
        }
    }

    /// Reassemble the message, or `None` if decoding is not finished.
    ///
    /// The result is truncated to the payload length supplied at construction.
    #[must_use]
    pub fn assemble(&self) -> Option<Vec<u8>> {
        if !self.is_complete() {
            return None;
        }
        let mut out = Vec::with_capacity(self.block_count * self.block_size);
        for block in &self.solved {
            out.extend_from_slice(block.as_ref()?);
        }
        if self.payload_len <= out.len() {
            out.truncate(self.payload_len);
        }
        Some(out)
    }

    /// Consume the decoder and return the message.
    #[must_use]
    pub fn take(self) -> Option<Vec<u8>> {
        self.assemble()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(len: usize, seed: u64) -> Vec<u8> {
        let mut rng = SplitMix64::new(seed);
        (0..len).map(|_| (rng.next_u64() & 0xFF) as u8).collect()
    }

    /// Run a transfer where `keep(frame)` decides whether the camera caught it.
    fn run_transfer(
        payload: &[u8],
        block_size: usize,
        session: u32,
        keep: impl Fn(u32) -> bool,
        max_frames: u32,
    ) -> Option<(Vec<u8>, u32, usize)> {
        let params = SolitonParams::default();
        let enc = Encoder::new(payload, block_size, session, params).unwrap();
        let mut dec = Decoder::new(
            enc.block_count(),
            block_size,
            payload.len(),
            session,
            params,
        )
        .unwrap();

        for frame in 0..max_frames {
            if keep(frame) {
                dec.absorb(frame, &enc.droplet(frame)).unwrap();
            }
            if dec.is_complete() {
                return Some((dec.take().unwrap(), frame + 1, enc.block_count()));
            }
        }
        None
    }

    #[test]
    fn perfect_channel_finishes_in_exactly_k_frames() {
        // The systematic prefix means a clean capture has zero overhead.
        let payload = message(4096, 1);
        let (out, frames, k) = run_transfer(&payload, 128, 0x1111, |_| true, 10_000).unwrap();
        assert_eq!(out, payload);
        assert_eq!(k, 32);
        assert_eq!(frames, 32, "expected zero overhead on a clean channel");
    }

    #[test]
    fn recovers_from_heavy_uniform_loss() {
        let payload = message(8192, 2);
        for drop_every in [2u32, 3, 4, 5, 10] {
            let (out, _, _) =
                run_transfer(&payload, 256, 0x2222, |f| f % drop_every != 0, 200_000).unwrap();
            assert_eq!(out, payload, "failed dropping every {drop_every}th frame");
        }
    }

    #[test]
    fn recovers_from_long_bursts_of_loss() {
        // Camera looked away for 40 frames at a time.
        let payload = message(6000, 3);
        let (out, _, _) =
            run_transfer(&payload, 200, 0x3333, |f| (f / 40) % 2 == 0, 200_000).unwrap();
        assert_eq!(out, payload);
    }

    #[test]
    fn recovers_when_the_start_is_missed_entirely() {
        // Receiver arrived late and never saw the systematic prefix.
        let payload = message(4000, 4);
        let enc = Encoder::new(&payload, 100, 0x4444, SolitonParams::default()).unwrap();
        let k = enc.block_count();
        let (out, _, _) =
            run_transfer(&payload, 100, 0x4444, move |f| f as usize >= k * 2, 500_000).unwrap();
        assert_eq!(out, payload, "must decode from repair droplets alone");
    }

    #[test]
    fn overhead_is_modest_when_the_prefix_is_lost() {
        let payload = message(20_000, 5);
        let block_size = 200;
        let enc = Encoder::new(&payload, block_size, 0x5555, SolitonParams::default()).unwrap();
        let k = enc.block_count();
        let mut dec = Decoder::new(
            k,
            block_size,
            payload.len(),
            0x5555,
            SolitonParams::default(),
        )
        .unwrap();

        // Skip the systematic prefix so this measures the fountain itself.
        let mut frame = k as u32;
        while !dec.is_complete() {
            dec.absorb(frame, &enc.droplet(frame)).unwrap();
            frame += 1;
            assert!(frame < 100_000, "decoder never converged");
        }
        let overhead = dec.absorbed() as f64 / k as f64;
        assert_eq!(dec.take().unwrap(), payload);
        // Robust soliton on K=100 should land well under 2x.
        assert!(
            overhead < 2.0,
            "overhead {overhead:.2}x is too high for k={k}"
        );
    }

    #[test]
    fn duplicate_and_out_of_order_droplets_are_harmless() {
        let payload = message(2048, 6);
        let params = SolitonParams::default();
        let enc = Encoder::new(&payload, 64, 0x6666, params).unwrap();
        let k = enc.block_count();
        let mut dec = Decoder::new(k, 64, payload.len(), 0x6666, params).unwrap();

        // Feed backwards, and feed everything twice.
        let mut frames: Vec<u32> = (0..(k as u32 * 3)).collect();
        frames.reverse();
        for f in frames {
            dec.absorb(f, &enc.droplet(f)).unwrap();
            dec.absorb(f, &enc.droplet(f)).unwrap();
        }
        assert!(dec.is_complete());
        assert_eq!(dec.take().unwrap(), payload);
    }

    #[test]
    fn single_block_message_works() {
        let payload = b"short".to_vec();
        let (out, frames, k) = run_transfer(&payload, 64, 0x7777, |_| true, 100).unwrap();
        assert_eq!(out, payload);
        assert_eq!(k, 1);
        assert_eq!(frames, 1);
    }

    #[test]
    fn payload_shorter_than_block_is_padded_and_trimmed() {
        let payload = b"abc".to_vec();
        let (out, _, _) = run_transfer(&payload, 128, 0x8888, |_| true, 100).unwrap();
        assert_eq!(out, payload);
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn payload_exactly_fills_blocks() {
        let payload = message(512, 9);
        let (out, _, k) = run_transfer(&payload, 128, 0x9999, |_| true, 1000).unwrap();
        assert_eq!(out, payload);
        assert_eq!(k, 4);
    }

    #[test]
    fn rejects_wrong_droplet_size() {
        let params = SolitonParams::default();
        let mut dec = Decoder::new(4, 32, 128, 0, params).unwrap();
        assert_eq!(
            dec.absorb(0, &[0u8; 16]).unwrap_err(),
            FountainError::WrongBlockSize
        );
    }

    #[test]
    fn rejects_invalid_construction() {
        let params = SolitonParams::default();
        assert_eq!(
            Encoder::new(b"x", 0, 0, params).unwrap_err(),
            FountainError::InvalidParams
        );
        assert_eq!(
            Encoder::new(b"", 8, 0, params).unwrap_err(),
            FountainError::InvalidParams
        );
        assert_eq!(
            Decoder::new(0, 8, 0, 0, params).unwrap_err(),
            FountainError::InvalidParams
        );
    }

    #[test]
    fn incomplete_decoder_yields_nothing() {
        let params = SolitonParams::default();
        let payload = message(1024, 10);
        let enc = Encoder::new(&payload, 64, 0xAAAA, params).unwrap();
        let mut dec = Decoder::new(enc.block_count(), 64, payload.len(), 0xAAAA, params).unwrap();
        dec.absorb(0, &enc.droplet(0)).unwrap();
        assert!(!dec.is_complete());
        assert!(dec.assemble().is_none());
        assert!(dec.progress() > 0.0 && dec.progress() < 1.0);
    }

    #[test]
    fn systematic_prefix_is_session_independent_by_design() {
        // Frame i carries block i verbatim for i < k, whatever the session id.
        // This is deliberate: it is what gives a clean capture zero overhead.
        // Session isolation is therefore NOT this layer's job - it is enforced
        // one level up, where the frame header is parsed and a droplet from a
        // foreign session is discarded before it ever reaches `absorb`.
        let params = SolitonParams::default();
        let a = Encoder::new(&message(2048, 11), 64, 0xAAAA, params).unwrap();
        let b = Encoder::new(&message(2048, 12), 64, 0xBBBB, params).unwrap();
        for f in 0..a.block_count() as u32 {
            assert_eq!(a.plan(f), b.plan(f), "systematic frame {f} should match");
        }
    }

    #[test]
    fn repair_droplets_are_session_bound() {
        // Past the systematic prefix the two sessions diverge, so a foreign
        // repair droplet corrupts rather than completes a decode. The header
        // layer must reject it; this test documents what happens if it does
        // not.
        let params = SolitonParams::default();
        let payload = message(2048, 11);
        let enc = Encoder::new(&payload, 64, 0xAAAA, params).unwrap();
        let k = enc.block_count();

        let mut diverged = 0;
        for f in (k as u32)..(k as u32 + 200) {
            let mine = DegreeTable::new(k, params).plan(0xBBBB, f);
            if mine != enc.plan(f) {
                diverged += 1;
            }
        }
        assert!(
            diverged > 150,
            "repair droplets barely differ between sessions: {diverged}/200"
        );

        // Feeding only foreign repair droplets must not reconstruct the
        // message.
        let mut dec = Decoder::new(k, 64, payload.len(), 0xBBBB, params).unwrap();
        for f in (k as u32)..(k as u32 * 6) {
            let _ = dec.absorb(f, &enc.droplet(f));
        }
        if let Some(out) = dec.assemble() {
            assert!(out != payload, "cross-session decode must not succeed");
        }
    }

    #[test]
    fn randomised_loss_patterns_always_converge() {
        for trial in 0..40u64 {
            let mut rng = SplitMix64::new(0xC0FFEE + trial);
            let len = 500 + (rng.next_u64() % 8000) as usize;
            let block_size = 32 + (rng.next_u64() % 200) as usize;
            let payload = message(len, trial);
            let loss = rng.next_u64() % 60; // up to 60% loss
            let mut drop_rng = SplitMix64::new(0xFACE + trial);

            let params = SolitonParams::default();
            let enc = Encoder::new(&payload, block_size, trial as u32, params).unwrap();
            let mut dec = Decoder::new(
                enc.block_count(),
                block_size,
                payload.len(),
                trial as u32,
                params,
            )
            .unwrap();

            let mut frame = 0u32;
            while !dec.is_complete() {
                if drop_rng.below(100) >= loss {
                    dec.absorb(frame, &enc.droplet(frame)).unwrap();
                }
                frame += 1;
                assert!(
                    frame < 500_000,
                    "trial {trial}: no convergence (len={len}, loss={loss}%)"
                );
            }
            assert_eq!(dec.take().unwrap(), payload, "trial {trial}");
        }
    }
}
