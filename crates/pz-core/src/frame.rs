//! Frame capacity planning and the per-frame Reed-Solomon layer.
//!
//! Everything about how many bytes fit in a frame is derived here from three
//! header fields - grid size, colour mode, parity ratio - so the transmitter
//! and receiver always agree without negotiating.
//!
//! # Interleaving
//!
//! A frame's symbols are split into several Reed-Solomon blocks. If block 0
//! occupied the first stripe of cells and block 1 the next, a thumb over one
//! corner would destroy a single block entirely while leaving the others
//! untouched - and RS cannot repair a block that is more than half gone, no
//! matter how healthy its neighbours are.
//!
//! So symbols are interleaved: symbol `s` of block `b` is written at position
//! `s * blocks + b`. Any burst of `blocks` consecutive damaged symbols now
//! costs each block exactly one symbol. The damage is spread until it is thin
//! enough for every block to absorb.

use crate::color::{unpack_bits, ColorMode};
use crate::layout::{GridSize, Layout};
use crate::PzError;
use alloc::vec;
use alloc::vec::Vec;
use pz_fec::{crc32, ReedSolomon};

/// Selectable parity ratios, indexed by the header's 4-bit parity code.
///
/// The ratio is the fraction of each Reed-Solomon block spent on parity. Lower
/// means more payload per frame; higher means each frame survives more damage.
pub const PARITY_RATIOS: [f64; 8] = [0.10, 0.16, 0.22, 0.28, 0.34, 0.40, 0.50, 0.60];

/// Parity code used unless the caller chooses otherwise: 28% parity, which
/// repairs roughly one damaged symbol in seven per block.
pub const DEFAULT_PARITY_CODE: u8 = 3;

/// Bytes of CRC-32 appended to each frame payload.
pub const FRAME_CRC_BYTES: usize = 4;

/// How a frame's cells are divided into Reed-Solomon blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameProfile {
    grid: GridSize,
    mode: ColorMode,
    parity_code: u8,
    data_cells: usize,
    symbols_total: usize,
    blocks: usize,
    block_n: usize,
    block_k: usize,
    frame_payload: usize,
}

impl FrameProfile {
    /// Derive the profile from the three header fields that determine it.
    ///
    /// # Errors
    /// Returns [`PzError::CapacityTooSmall`] if the combination leaves no room
    /// for a payload, and [`PzError::UnsupportedFormat`] for an unknown parity
    /// code.
    pub fn new(grid: GridSize, mode: ColorMode, parity_code: u8) -> Result<Self, PzError> {
        let ratio = *PARITY_RATIOS
            .get(parity_code as usize)
            .ok_or(PzError::UnsupportedFormat)?;

        let data_cells = Layout::new(grid).data_capacity_cells();
        let symbols_total = data_cells * mode.bits_per_cell() / 8;

        // Reed-Solomon over GF(2^8) cannot exceed 255 symbols per block, so
        // split into the fewest blocks that respect that, then size them
        // evenly. Any remainder is padding.
        let blocks = symbols_total.div_ceil(255).max(1);
        let block_n = symbols_total / blocks;
        if block_n < 8 {
            return Err(PzError::CapacityTooSmall);
        }

        // `f64::round` is std-only; the product is always positive here, so
        // adding a half before the truncating cast rounds to nearest.
        let parity = ((block_n as f64) * ratio + 0.5) as usize;
        let parity = parity.clamp(2, block_n - 1);
        let block_k = block_n - parity;
        let frame_payload = blocks * block_k;

        if frame_payload <= FRAME_CRC_BYTES {
            return Err(PzError::CapacityTooSmall);
        }

        Ok(Self {
            grid,
            mode,
            parity_code,
            data_cells,
            symbols_total,
            blocks,
            block_n,
            block_k,
            frame_payload,
        })
    }

    /// Grid size this profile targets.
    #[must_use]
    pub const fn grid(&self) -> GridSize {
        self.grid
    }

    /// Colour mode this profile targets.
    #[must_use]
    pub const fn mode(&self) -> ColorMode {
        self.mode
    }

    /// Parity code this profile targets.
    #[must_use]
    pub const fn parity_code(&self) -> u8 {
        self.parity_code
    }

    /// Payload-carrying cells in the grid.
    #[must_use]
    pub const fn data_cells(&self) -> usize {
        self.data_cells
    }

    /// Number of Reed-Solomon blocks per frame.
    #[must_use]
    pub const fn blocks(&self) -> usize {
        self.blocks
    }

    /// Symbols per Reed-Solomon block.
    #[must_use]
    pub const fn block_n(&self) -> usize {
        self.block_n
    }

    /// Data symbols per Reed-Solomon block.
    #[must_use]
    pub const fn block_k(&self) -> usize {
        self.block_k
    }

    /// Coded symbols actually written to cells.
    #[must_use]
    pub const fn coded_symbols(&self) -> usize {
        self.blocks * self.block_n
    }

    /// Bytes of frame payload, including the trailing frame CRC.
    #[must_use]
    pub const fn frame_payload(&self) -> usize {
        self.frame_payload
    }

    /// Bytes of fountain droplet carried per frame.
    #[must_use]
    pub const fn droplet_size(&self) -> usize {
        self.frame_payload - FRAME_CRC_BYTES
    }

    /// Symbols each block can repair when every fault is an unknown error.
    #[must_use]
    pub const fn correctable_errors(&self) -> usize {
        (self.block_n - self.block_k) / 2
    }

    /// Symbols each block can repair when every fault is a flagged erasure.
    #[must_use]
    pub const fn correctable_erasures(&self) -> usize {
        self.block_n - self.block_k
    }

    /// Payload throughput in bytes per second at a given frame rate, ignoring
    /// fountain overhead.
    #[must_use]
    pub fn bytes_per_second(&self, fps: f64) -> f64 {
        self.droplet_size() as f64 * fps
    }

    /// Reed-Solomon codec for this profile's blocks.
    fn codec(&self) -> Result<ReedSolomon, PzError> {
        Ok(ReedSolomon::new(self.block_n, self.block_k)?)
    }

    /// Encode one frame payload into the interleaved symbol stream.
    ///
    /// # Errors
    /// Returns [`PzError::WrongLength`] unless `payload.len()` equals
    /// [`FrameProfile::frame_payload`].
    pub fn encode_symbols(&self, payload: &[u8]) -> Result<Vec<u8>, PzError> {
        if payload.len() != self.frame_payload {
            return Err(PzError::WrongLength);
        }
        let rs = self.codec()?;
        let mut out = vec![0u8; self.coded_symbols()];
        for b in 0..self.blocks {
            let data = &payload[b * self.block_k..(b + 1) * self.block_k];
            let code = rs.encode(data)?;
            for (s, &symbol) in code.iter().enumerate() {
                out[s * self.blocks + b] = symbol;
            }
        }
        Ok(out)
    }

    /// Repair and de-interleave a received symbol stream.
    ///
    /// `confidence` runs parallel to `symbols`; any symbol below
    /// `erasure_threshold` becomes an erasure hint. Because erasures cost half
    /// as much as errors, but only up to the parity budget, each block keeps
    /// its least confident symbols as erasures and lets Reed-Solomon treat the
    /// rest as ordinary errors.
    ///
    /// # Errors
    /// Returns [`PzError::FrameCorrupt`] if any block is beyond repair.
    pub fn decode_symbols(
        &self,
        symbols: &[u8],
        confidence: &[f64],
        erasure_threshold: f64,
    ) -> Result<Vec<u8>, PzError> {
        if symbols.len() != self.coded_symbols() {
            return Err(PzError::WrongLength);
        }
        let rs = self.codec()?;
        let mut payload = Vec::with_capacity(self.frame_payload);

        let budget = self.correctable_erasures();

        for b in 0..self.blocks {
            let mut block = vec![0u8; self.block_n];
            let mut weak: Vec<(usize, f64)> = Vec::new();
            for (s, slot) in block.iter_mut().enumerate() {
                let g = s * self.blocks + b;
                *slot = symbols[g];
                let c = confidence.get(g).copied().unwrap_or(1.0);
                if c < erasure_threshold {
                    weak.push((s, c));
                }
            }

            // Keep only the least confident hints, up to what the code can pay
            // for. Flagging more erasures than the parity budget guarantees a
            // decode failure even when the data is repairable.
            weak.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(core::cmp::Ordering::Equal));
            weak.truncate(budget);
            let erasures: Vec<usize> = weak.iter().map(|(s, _)| *s).collect();

            if rs.decode(&mut block, &erasures).is_err() {
                // The hints may themselves have been wrong; retry blind.
                for (s, slot) in block.iter_mut().enumerate() {
                    *slot = symbols[s * self.blocks + b];
                }
                rs.decode(&mut block, &[])
                    .map_err(|_| PzError::FrameCorrupt)?;
            }
            payload.extend_from_slice(&block[..self.block_k]);
        }

        Ok(payload)
    }

    /// Turn a droplet into the frame payload written to cells: the droplet
    /// followed by a CRC-32 binding it to its frame index.
    ///
    /// Binding the checksum to the index matters. A droplet is only meaningful
    /// alongside the frame index that says which blocks it mixes; if a
    /// corrupted header produced the wrong index the droplet would poison the
    /// fountain decoder silently. Covering both makes that mismatch loud.
    #[must_use]
    pub fn seal_payload(&self, droplet: &[u8], frame_index: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.frame_payload);
        out.extend_from_slice(droplet);
        out.resize(self.droplet_size(), 0);
        let mut checked = out.clone();
        checked.extend_from_slice(&frame_index.to_be_bytes());
        out.extend_from_slice(&crc32(&checked).to_be_bytes());
        out
    }

    /// Verify and strip the frame CRC, returning the droplet.
    ///
    /// # Errors
    /// Returns [`PzError::ChecksumMismatch`] if the payload does not match its
    /// frame index.
    pub fn open_payload(&self, payload: &[u8], frame_index: u32) -> Result<Vec<u8>, PzError> {
        if payload.len() != self.frame_payload {
            return Err(PzError::WrongLength);
        }
        let split = self.droplet_size();
        let droplet = &payload[..split];
        let stored = u32::from_be_bytes([
            payload[split],
            payload[split + 1],
            payload[split + 2],
            payload[split + 3],
        ]);

        let mut checked = droplet.to_vec();
        checked.extend_from_slice(&frame_index.to_be_bytes());
        if crc32(&checked) != stored {
            return Err(PzError::ChecksumMismatch);
        }
        Ok(droplet.to_vec())
    }

    /// Convert an interleaved symbol stream into per-cell values.
    #[must_use]
    pub fn symbols_to_cells(&self, symbols: &[u8]) -> Vec<u8> {
        unpack_bits(symbols, self.mode.bits_per_cell(), self.data_cells)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_profile() -> Vec<FrameProfile> {
        let mut out = Vec::new();
        for grid in GridSize::ALL {
            for mode in [ColorMode::Mono, ColorMode::Rgb4, ColorMode::Rgb8] {
                for parity_code in 0..8u8 {
                    if let Ok(p) = FrameProfile::new(grid, mode, parity_code) {
                        out.push(p);
                    }
                }
            }
        }
        out
    }

    #[test]
    fn every_combination_produces_a_usable_profile() {
        let profiles = every_profile();
        assert_eq!(profiles.len(), 5 * 3 * 8, "some combination was rejected");
        for p in profiles {
            assert!(p.block_n() <= 255, "{p:?}");
            assert!(p.block_k() >= 1, "{p:?}");
            assert!(p.block_k() < p.block_n(), "{p:?}");
            assert!(p.droplet_size() >= 1, "{p:?}");
            assert!(p.coded_symbols() <= p.symbols_total, "{p:?}");
        }
    }

    #[test]
    fn higher_parity_means_smaller_payload() {
        let mut previous = usize::MAX;
        for parity_code in 0..8u8 {
            let p = FrameProfile::new(GridSize::G65, ColorMode::Rgb8, parity_code).unwrap();
            assert!(
                p.droplet_size() < previous,
                "parity {parity_code} did not shrink the payload"
            );
            previous = p.droplet_size();
        }
    }

    #[test]
    fn more_colours_means_more_payload() {
        let mono = FrameProfile::new(GridSize::G65, ColorMode::Mono, 3).unwrap();
        let rgb4 = FrameProfile::new(GridSize::G65, ColorMode::Rgb4, 3).unwrap();
        let rgb8 = FrameProfile::new(GridSize::G65, ColorMode::Rgb8, 3).unwrap();
        assert!(mono.droplet_size() < rgb4.droplet_size());
        assert!(rgb4.droplet_size() < rgb8.droplet_size());
    }

    #[test]
    fn rejects_an_unknown_parity_code() {
        assert!(matches!(
            FrameProfile::new(GridSize::G49, ColorMode::Mono, 9),
            Err(PzError::UnsupportedFormat)
        ));
    }

    #[test]
    fn symbols_round_trip_undamaged() {
        for p in every_profile() {
            let payload: Vec<u8> = (0..p.frame_payload()).map(|i| (i * 7) as u8).collect();
            let symbols = p.encode_symbols(&payload).unwrap();
            assert_eq!(symbols.len(), p.coded_symbols());
            let conf = vec![1.0f64; symbols.len()];
            let out = p.decode_symbols(&symbols, &conf, 0.25).unwrap();
            assert_eq!(out, payload, "{p:?}");
        }
    }

    #[test]
    fn interleaving_spreads_a_burst_across_every_block() {
        let p = FrameProfile::new(GridSize::G97, ColorMode::Rgb8, 3).unwrap();
        assert!(p.blocks() > 1, "need a multi-block profile for this test");

        let payload: Vec<u8> = (0..p.frame_payload()).map(|i| (i % 251) as u8).collect();
        let mut symbols = p.encode_symbols(&payload).unwrap();

        // Destroy a contiguous run: with interleaving each block loses only
        // burst/blocks symbols.
        let burst = p.correctable_errors() * p.blocks();
        for s in symbols.iter_mut().take(burst) {
            *s ^= 0xFF;
        }

        let conf = vec![1.0f64; symbols.len()];
        let out = p.decode_symbols(&symbols, &conf, 0.25).unwrap();
        assert_eq!(
            out, payload,
            "a burst of {burst} symbols should be repairable"
        );
    }

    #[test]
    fn erasure_hints_double_the_repairable_damage() {
        let p = FrameProfile::new(GridSize::G65, ColorMode::Rgb8, 4).unwrap();
        let payload: Vec<u8> = (0..p.frame_payload()).map(|i| (i % 253) as u8).collect();
        let clean = p.encode_symbols(&payload).unwrap();

        // Damage every block by more than the error radius but within the
        // erasure radius.
        let per_block = p.correctable_errors() + p.correctable_errors() / 2;
        let mut symbols = clean.clone();
        let mut conf = vec![1.0f64; symbols.len()];
        for b in 0..p.blocks() {
            for s in 0..per_block {
                let g = s * p.blocks() + b;
                symbols[g] ^= 0xAA;
                conf[g] = 0.0; // demodulator knew these were unreliable
            }
        }

        // With hints it decodes.
        assert_eq!(
            p.decode_symbols(&symbols, &conf, 0.25).unwrap(),
            payload,
            "erasure hints should have carried this"
        );

        // Without hints the same damage is beyond the error radius.
        let blind = vec![1.0f64; symbols.len()];
        assert!(
            p.decode_symbols(&symbols, &blind, 0.25).is_err(),
            "this damage should exceed the blind error radius"
        );
    }

    #[test]
    fn too_many_erasure_hints_are_capped_not_fatal() {
        let p = FrameProfile::new(GridSize::G65, ColorMode::Rgb8, 3).unwrap();
        let payload: Vec<u8> = (0..p.frame_payload()).map(|i| i as u8).collect();
        let symbols = p.encode_symbols(&payload).unwrap();
        // A pessimistic demodulator flags everything as unreliable while the
        // data is in fact perfect.
        let conf = vec![0.0f64; symbols.len()];
        assert_eq!(p.decode_symbols(&symbols, &conf, 0.25).unwrap(), payload);
    }

    #[test]
    fn reports_failure_when_damage_is_unrepairable() {
        let p = FrameProfile::new(GridSize::G49, ColorMode::Rgb8, 0).unwrap();
        let payload: Vec<u8> = (0..p.frame_payload()).map(|i| i as u8).collect();
        let mut symbols = p.encode_symbols(&payload).unwrap();
        for s in symbols.iter_mut() {
            *s ^= 0x5A;
        }
        let conf = vec![1.0f64; symbols.len()];
        assert!(matches!(
            p.decode_symbols(&symbols, &conf, 0.25),
            Err(PzError::FrameCorrupt)
        ));
    }

    #[test]
    fn frame_payload_seals_and_opens() {
        let p = FrameProfile::new(GridSize::G49, ColorMode::Rgb4, 3).unwrap();
        let droplet: Vec<u8> = (0..p.droplet_size()).map(|i| (i * 3) as u8).collect();
        let sealed = p.seal_payload(&droplet, 77);
        assert_eq!(sealed.len(), p.frame_payload());
        assert_eq!(p.open_payload(&sealed, 77).unwrap(), droplet);
    }

    #[test]
    fn the_frame_crc_is_bound_to_the_frame_index() {
        let p = FrameProfile::new(GridSize::G49, ColorMode::Rgb4, 3).unwrap();
        let droplet: Vec<u8> = (0..p.droplet_size()).map(|i| i as u8).collect();
        let sealed = p.seal_payload(&droplet, 77);
        // Same bytes, wrong index: must be rejected rather than silently
        // poisoning the fountain decoder.
        assert!(matches!(
            p.open_payload(&sealed, 78),
            Err(PzError::ChecksumMismatch)
        ));
    }

    #[test]
    fn a_corrupted_payload_fails_its_crc() {
        let p = FrameProfile::new(GridSize::G49, ColorMode::Mono, 3).unwrap();
        let droplet: Vec<u8> = (0..p.droplet_size()).map(|i| i as u8).collect();
        let mut sealed = p.seal_payload(&droplet, 5);
        sealed[0] ^= 0x01;
        assert!(matches!(
            p.open_payload(&sealed, 5),
            Err(PzError::ChecksumMismatch)
        ));
    }

    #[test]
    fn throughput_estimate_scales_with_frame_rate() {
        let p = FrameProfile::new(GridSize::G65, ColorMode::Rgb8, 3).unwrap();
        assert!((p.bytes_per_second(30.0) - p.droplet_size() as f64 * 30.0).abs() < 1e-9);
        assert!(p.bytes_per_second(60.0) > p.bytes_per_second(30.0));
    }
}
