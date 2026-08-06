//! Turning a message into an endless stream of frames.

use crate::color::{color_of, modulate, unpack_bits, ColorMode};
use crate::frame::FrameProfile;
use crate::header::{FrameHeader, FLAG_CRC32_PREFIX, HEADER_CODE_BYTES, PROTOCOL_VERSION};
use crate::layout::{GridSize, Layout, Role, HEADER_CELLS};
use crate::{PzError, MAX_PAYLOAD_BYTES};
use alloc::vec;
use alloc::vec::Vec;
use pz_fec::crc32;
use pz_fountain::SolitonParams;

/// How a transmission is encoded.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EncoderConfig {
    /// Cells per side. Larger grids carry more per frame but need a closer or
    /// steadier camera.
    pub grid: GridSize,
    /// Bits per cell.
    pub mode: ColorMode,
    /// Index into [`crate::PARITY_RATIOS`].
    pub parity_code: u8,
    /// Distinguishes concurrent transmissions. `None` derives a stable id from
    /// the payload, so re-encoding the same bytes yields the same stream.
    pub session_id: Option<u16>,
    /// Fountain code tuning. The defaults are appropriate for screen-to-camera.
    pub soliton: SolitonParams,
}

impl Default for EncoderConfig {
    /// A 49x49 grid in 8-colour mode at 28% parity: the balanced profile.
    fn default() -> Self {
        Self {
            grid: GridSize::G49,
            mode: ColorMode::Rgb8,
            parity_code: crate::frame::DEFAULT_PARITY_CODE,
            session_id: None,
            soliton: SolitonParams::default(),
        }
    }
}

impl EncoderConfig {
    /// The most robust profile: 33x33 in black and white at 40% parity.
    ///
    /// Use when the camera is poor, the light is bad, or the payload is a
    /// short credential where reliability beats speed.
    #[must_use]
    pub fn robust() -> Self {
        Self {
            grid: GridSize::G33,
            mode: ColorMode::Mono,
            parity_code: 5,
            ..Self::default()
        }
    }

    /// Single-colour at full grid: 97x97 in black and white at 28% parity.
    ///
    /// The same two-level modulation as [`Self::robust`] on the largest grid,
    /// which buys back most of the throughput that dropping to one bit per
    /// cell costs. Pair it with [`RenderOptions::ink`] to draw it in any
    /// sufficiently dark colour.
    ///
    /// [`RenderOptions::ink`]: crate::render::RenderOptions::ink
    #[must_use]
    pub fn mono() -> Self {
        Self {
            grid: GridSize::G97,
            mode: ColorMode::Mono,
            parity_code: 3,
            ..Self::default()
        }
    }

    /// The highest-throughput profile: 97x97 in 8 colours at 16% parity.
    ///
    /// Needs a steady, well-focused, close-up capture.
    #[must_use]
    pub fn fast() -> Self {
        Self {
            grid: GridSize::G97,
            mode: ColorMode::Rgb8,
            parity_code: 1,
            ..Self::default()
        }
    }

    /// The balanced colour profile with per-cell error *detection*: 65x65 in
    /// 4-colour mode at 28% parity.
    ///
    /// Two thirds the raw bits of 8-colour, but every cell carries a parity
    /// constraint that turns a channel failure into an erasure the
    /// Reed-Solomon layer repairs at half price.
    #[must_use]
    pub fn resilient() -> Self {
        Self {
            grid: GridSize::G65,
            mode: ColorMode::Rgb4,
            parity_code: 3,
            ..Self::default()
        }
    }
}

/// One rendered frame as a grid of 3-bit colour codes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    modules: usize,
    index: u32,
    cells: Vec<u8>,
}

impl Frame {
    /// Cells per side.
    #[must_use]
    pub const fn modules(&self) -> usize {
        self.modules
    }

    /// The frame index this frame carries.
    #[must_use]
    pub const fn index(&self) -> u32 {
        self.index
    }

    /// Raw colour codes, row-major.
    #[must_use]
    pub fn cells(&self) -> &[u8] {
        &self.cells
    }

    /// Colour code at `(row, col)`.
    #[must_use]
    pub fn code_at(&self, row: usize, col: usize) -> u8 {
        self.cells[row * self.modules + col]
    }

    /// Displayed RGB at `(row, col)`.
    #[must_use]
    pub fn color_at(&self, row: usize, col: usize) -> [u8; 3] {
        color_of(self.code_at(row, col))
    }

    /// The frame as a flat list of RGB triples, row-major. This is the form
    /// [`crate::Decoder::ingest_frame`] consumes.
    #[must_use]
    pub fn to_colors(&self) -> Vec<[u8; 3]> {
        self.cells.iter().map(|&c| color_of(c)).collect()
    }
}

/// Splits a message into fountain-coded frames.
///
/// The encoder is immutable once built and [`Encoder::frame`] is pure, so
/// frames can be produced in any order, in parallel, or regenerated later.
#[derive(Debug, Clone)]
pub struct Encoder {
    config: EncoderConfig,
    layout: Layout,
    profile: FrameProfile,
    fountain: pz_fountain::Encoder,
    session_id: u16,
    container_len: usize,
    user_len: usize,
}

impl Encoder {
    /// Prepare a transmission.
    ///
    /// # Errors
    /// Returns [`PzError::EmptyPayload`] for empty input,
    /// [`PzError::PayloadTooLarge`] beyond [`MAX_PAYLOAD_BYTES`], and
    /// [`PzError::CapacityTooSmall`] if the configuration leaves no room for
    /// payload.
    pub fn new(payload: &[u8], config: EncoderConfig) -> Result<Self, PzError> {
        if payload.is_empty() {
            return Err(PzError::EmptyPayload);
        }
        if payload.len() > MAX_PAYLOAD_BYTES {
            return Err(PzError::PayloadTooLarge);
        }

        let layout = Layout::new(config.grid);
        let profile = FrameProfile::new(config.grid, config.mode, config.parity_code)?;

        // The container is a CRC-32 of the user data followed by the data
        // itself. The fountain layer can finish with the wrong bytes if a
        // droplet slipped through corrupted; this is the end-to-end check that
        // catches it.
        let checksum = crc32(payload);
        let mut container = Vec::with_capacity(payload.len() + 4);
        container.extend_from_slice(&checksum.to_be_bytes());
        container.extend_from_slice(payload);

        // Folding the checksum's halves together keeps both ends of it in play,
        // so two messages differing only in their high CRC bits still get
        // different session ids.
        let session_id = config
            .session_id
            .unwrap_or((checksum ^ (checksum >> 16)) as u16);

        let fountain = pz_fountain::Encoder::new(
            &container,
            profile.droplet_size(),
            u32::from(session_id),
            config.soliton,
        )?;

        Ok(Self {
            config,
            layout,
            profile,
            fountain,
            session_id,
            container_len: container.len(),
            user_len: payload.len(),
        })
    }

    /// The configuration in use.
    #[must_use]
    pub const fn config(&self) -> &EncoderConfig {
        &self.config
    }

    /// The frame geometry in use.
    #[must_use]
    pub const fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The capacity plan in use.
    #[must_use]
    pub const fn profile(&self) -> &FrameProfile {
        &self.profile
    }

    /// The session id stamped into every frame.
    #[must_use]
    pub const fn session_id(&self) -> u16 {
        self.session_id
    }

    /// Minimum frames a receiver needs under perfect conditions.
    #[must_use]
    pub fn block_count(&self) -> usize {
        self.fountain.block_count()
    }

    /// Length of the original message.
    #[must_use]
    pub const fn payload_len(&self) -> usize {
        self.user_len
    }

    /// Estimated seconds to transfer at a given capture rate, assuming the
    /// receiver catches `capture_ratio` of displayed frames and the fountain
    /// needs about 5% overhead.
    #[must_use]
    pub fn estimated_seconds(&self, fps: f64, capture_ratio: f64) -> f64 {
        let frames = self.block_count() as f64 * 1.05;
        let effective = (fps * capture_ratio).max(0.001);
        frames / effective
    }

    /// The header this encoder stamps on a given frame.
    #[must_use]
    pub fn header(&self, frame_index: u32) -> FrameHeader {
        FrameHeader {
            version: PROTOCOL_VERSION,
            mode: self.config.mode,
            grid: self.config.grid,
            parity_code: self.config.parity_code,
            session_id: self.session_id,
            frame_index,
            payload_len: self.container_len as u32,
            flags: FLAG_CRC32_PREFIX,
        }
    }

    /// Build one frame. Defined for every `u32`; the stream never runs out.
    ///
    /// # Errors
    /// Propagates coding failures, none of which are reachable for a
    /// successfully constructed encoder.
    pub fn frame(&self, frame_index: u32) -> Result<Frame, PzError> {
        let droplet = self.fountain.droplet(frame_index);
        let payload = self.profile.seal_payload(&droplet, frame_index);
        let symbols = self.profile.encode_symbols(&payload)?;
        let data_values = self.profile.symbols_to_cells(&symbols);

        let header_code = self.header(frame_index).encode_protected()?;
        let header_bits = unpack_bits(&header_code, 1, HEADER_CELLS);

        let n = self.layout.modules();
        let mut cells = vec![0u8; n * n];

        // Structural cells first: finders, separators, timing, palette.
        for row in 0..n {
            for col in 0..n {
                let i = row * n + col;
                cells[i] = match self.layout.role(row, col) {
                    Role::Fixed | Role::Palette => self.layout.fixed_color(row, col),
                    _ => 0,
                };
            }
        }

        // The header is always black and white, whatever the payload mode.
        for (bit, &cell) in header_bits.iter().zip(self.layout.header_cells()) {
            cells[cell as usize] = modulate(ColorMode::Mono, *bit);
        }

        for (value, &cell) in data_values.iter().zip(self.layout.data_cells()) {
            cells[cell as usize] = modulate(self.config.mode, *value);
        }

        debug_assert_eq!(header_code.len(), HEADER_CODE_BYTES);

        Ok(Frame {
            modules: n,
            index: frame_index,
            cells,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{BLACK, WHITE};

    #[test]
    fn rejects_an_empty_payload() {
        assert!(matches!(
            Encoder::new(&[], EncoderConfig::default()),
            Err(PzError::EmptyPayload)
        ));
    }

    #[test]
    fn frames_are_the_configured_size() {
        for grid in GridSize::ALL {
            let config = EncoderConfig {
                grid,
                ..EncoderConfig::default()
            };
            let e = Encoder::new(b"hello light", config).unwrap();
            let f = e.frame(0).unwrap();
            assert_eq!(f.modules(), grid.modules());
            assert_eq!(f.cells().len(), grid.modules() * grid.modules());
        }
    }

    #[test]
    fn structural_cells_are_identical_across_frames() {
        let e = Encoder::new(b"stability check", EncoderConfig::default()).unwrap();
        let a = e.frame(0).unwrap();
        let b = e.frame(999).unwrap();
        let n = a.modules();
        for row in 0..n {
            for col in 0..n {
                if matches!(e.layout().role(row, col), Role::Fixed | Role::Palette) {
                    assert_eq!(
                        a.code_at(row, col),
                        b.code_at(row, col),
                        "structural cell ({row},{col}) moved between frames"
                    );
                }
            }
        }
    }

    #[test]
    fn finder_patterns_are_drawn_correctly() {
        let e = Encoder::new(b"finders", EncoderConfig::default()).unwrap();
        let f = e.frame(0).unwrap();
        // Cross-section through the top-left finder.
        let expect = [BLACK, WHITE, BLACK, BLACK, BLACK, WHITE, BLACK, WHITE];
        for (c, want) in expect.iter().enumerate() {
            assert_eq!(f.code_at(3, c), *want, "finder column {c}");
        }
    }

    #[test]
    fn palette_patches_show_all_eight_colours() {
        let e = Encoder::new(b"palette", EncoderConfig::default()).unwrap();
        let f = e.frame(0).unwrap();
        let mut seen = [false; 8];
        let n = f.modules();
        for row in 0..n {
            for col in 0..n {
                if e.layout().role(row, col) == Role::Palette {
                    seen[f.code_at(row, col) as usize] = true;
                }
            }
        }
        assert!(seen.iter().all(|s| *s), "not all palette colours drawn");
    }

    #[test]
    fn data_cells_change_between_frames() {
        let e = Encoder::new(&vec![7u8; 4000], EncoderConfig::default()).unwrap();
        let a = e.frame(0).unwrap();
        let b = e.frame(1).unwrap();
        let changed = a
            .cells()
            .iter()
            .zip(b.cells().iter())
            .filter(|(x, y)| x != y)
            .count();
        assert!(
            changed > 100,
            "only {changed} cells differed between frames"
        );
    }

    #[test]
    fn mono_frames_use_only_black_and_white() {
        let config = EncoderConfig {
            mode: ColorMode::Mono,
            ..EncoderConfig::default()
        };
        let e = Encoder::new(b"monochrome payload", config).unwrap();
        let f = e.frame(3).unwrap();
        let n = f.modules();
        for row in 0..n {
            for col in 0..n {
                if e.layout().role(row, col) == Role::Palette {
                    continue; // the calibration patches are always in colour
                }
                let code = f.code_at(row, col);
                assert!(
                    code == BLACK || code == WHITE,
                    "cell ({row},{col}) used colour {code:03b} in mono mode"
                );
            }
        }
    }

    #[test]
    fn rgb4_frames_use_only_even_weight_codes_in_data_cells() {
        let config = EncoderConfig {
            mode: ColorMode::Rgb4,
            ..EncoderConfig::default()
        };
        let e = Encoder::new(&vec![0xA5u8; 2000], config).unwrap();
        let f = e.frame(11).unwrap();
        let n = f.modules();
        for &cell in e.layout().data_cells() {
            let code = f.cells()[cell as usize];
            assert_eq!(
                code.count_ones() % 2,
                0,
                "data cell {cell} used odd-weight code {code:03b}"
            );
        }
        let _ = n;
    }

    #[test]
    fn header_round_trips_out_of_a_frame() {
        let e = Encoder::new(b"header check", EncoderConfig::default()).unwrap();
        for index in [0u32, 1, 42, u32::MAX] {
            let f = e.frame(index).unwrap();
            // Re-read the header cells straight out of the frame.
            let bits: Vec<u8> = e
                .layout()
                .header_cells()
                .iter()
                .map(|&cell| u8::from(f.cells()[cell as usize] == WHITE))
                .collect();
            let bytes = crate::color::pack_bits(&bits, 1, HEADER_CODE_BYTES);
            let header = FrameHeader::decode_protected(&bytes, &[]).unwrap();
            assert_eq!(header.frame_index, index);
            assert_eq!(header.session_id, e.session_id());
            assert_eq!(header.grid, e.config().grid);
            assert_eq!(header.mode, e.config().mode);
        }
    }

    #[test]
    fn session_id_is_derived_and_stable() {
        let a = Encoder::new(b"same bytes", EncoderConfig::default()).unwrap();
        let b = Encoder::new(b"same bytes", EncoderConfig::default()).unwrap();
        let c = Encoder::new(b"other bytes", EncoderConfig::default()).unwrap();
        assert_eq!(a.session_id(), b.session_id());
        assert_ne!(a.session_id(), c.session_id());
    }

    #[test]
    fn explicit_session_id_is_honoured() {
        let config = EncoderConfig {
            session_id: Some(0x1234),
            ..EncoderConfig::default()
        };
        let e = Encoder::new(b"explicit", config).unwrap();
        assert_eq!(e.session_id(), 0x1234);
        assert_eq!(e.header(0).session_id, 0x1234);
    }

    #[test]
    fn block_count_tracks_payload_size() {
        let small = Encoder::new(b"tiny", EncoderConfig::default()).unwrap();
        let large = Encoder::new(&vec![0u8; 100_000], EncoderConfig::default()).unwrap();
        assert_eq!(small.block_count(), 1);
        assert!(large.block_count() > 100);
        assert!(large.estimated_seconds(30.0, 0.8) > small.estimated_seconds(30.0, 0.8));
    }

    #[test]
    fn preset_profiles_all_build() {
        for config in [
            EncoderConfig::default(),
            EncoderConfig::robust(),
            EncoderConfig::fast(),
            EncoderConfig::resilient(),
        ] {
            let e = Encoder::new(b"preset payload", config).unwrap();
            assert!(e.frame(0).is_ok());
            assert!(e.profile().droplet_size() > 0);
        }
    }

    #[test]
    fn frame_generation_is_pure() {
        let e = Encoder::new(b"determinism", EncoderConfig::default()).unwrap();
        assert_eq!(e.frame(17).unwrap(), e.frame(17).unwrap());
    }
}
