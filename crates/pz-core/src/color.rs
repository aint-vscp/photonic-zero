//! Colour modulation, per-frame calibration, and confidence-aware demodulation.
//!
//! # The three modes
//!
//! | Mode   | Bits/cell | Alphabet                     | Built-in detection |
//! |--------|-----------|------------------------------|--------------------|
//! | `Mono` | 1         | black, white                 | none               |
//! | `Rgb4` | 2         | black, yellow, magenta, cyan | 1 parity bit       |
//! | `Rgb8` | 3         | all eight RGB corners        | none               |
//!
//! Every mode treats the three colour channels as three independent one-bit
//! sub-channels, so demodulation is three threshold comparisons rather than a
//! nearest-neighbour search in colour space. A cell is `(r, g, b)` bits packed
//! low to high.
//!
//! # Why `Rgb4` earns its keep
//!
//! `Rgb4` uses only the four even-weight codewords: `000`, `011`, `101`, `110`.
//! An odd number of ones is therefore impossible, so any *single* channel that
//! reads wrong turns a legal codeword into an illegal one. The decoder cannot
//! tell which channel failed, but it does not need to: it reports the cell as
//! an **erasure**, and Reed-Solomon repairs erasures at half the cost of
//! errors. `Rgb4` carries two thirds of `Rgb8`'s bits but hands the FEC layer
//! far better information, which usually wins on a marginal capture.
//!
//! # Why calibrate every frame
//!
//! Auto-exposure, auto-white-balance, screen gamma, ambient light and the
//! camera's own colour matrix all move the measured value of "red". Fixed
//! thresholds fail the moment a lamp comes on. Each frame therefore carries
//! eight reference patches, and the thresholds are re-derived from them on
//! every single capture.

use alloc::vec::Vec;

/// How many bits each cell carries, and from which alphabet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    /// One bit per cell: black and white. The most robust mode.
    Mono,
    /// Two bits per cell from the four even-weight colours, with single
    /// channel error *detection* via the parity constraint.
    Rgb4,
    /// Three bits per cell using all eight RGB corners. Highest throughput.
    Rgb8,
}

impl ColorMode {
    /// Bits carried by one cell.
    #[must_use]
    pub const fn bits_per_cell(self) -> usize {
        match self {
            ColorMode::Mono => 1,
            ColorMode::Rgb4 => 2,
            ColorMode::Rgb8 => 3,
        }
    }

    /// The 4-bit code written into the frame header.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            ColorMode::Mono => 0,
            ColorMode::Rgb4 => 1,
            ColorMode::Rgb8 => 2,
        }
    }

    /// Parse a header mode code.
    #[must_use]
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(ColorMode::Mono),
            1 => Some(ColorMode::Rgb4),
            2 => Some(ColorMode::Rgb8),
            _ => None,
        }
    }

    /// Number of distinct symbols this mode can send per cell.
    #[must_use]
    pub const fn alphabet_size(self) -> usize {
        1 << self.bits_per_cell()
    }
}

/// The four even-weight codewords used by [`ColorMode::Rgb4`].
///
/// Index by the 2-bit data value; the result is a 3-bit `(r, g, b)` colour.
pub const RGB4_CODEWORDS: [u8; 4] = [0b000, 0b011, 0b101, 0b110];

/// Full-swing RGB for each 3-bit colour code.
///
/// Bit 0 is red, bit 1 green, bit 2 blue.
#[must_use]
pub const fn color_of(code: u8) -> [u8; 3] {
    [
        if code & 0b001 != 0 { 255 } else { 0 },
        if code & 0b010 != 0 { 255 } else { 0 },
        if code & 0b100 != 0 { 255 } else { 0 },
    ]
}

/// Map a data value to the colour code that should be displayed.
#[must_use]
pub const fn modulate(mode: ColorMode, value: u8) -> u8 {
    match mode {
        // Bit set means white, so a mono frame reads as ordinary ink on paper.
        ColorMode::Mono => {
            if value & 1 != 0 {
                0b111
            } else {
                0b000
            }
        }
        ColorMode::Rgb4 => RGB4_CODEWORDS[(value & 0b11) as usize],
        ColorMode::Rgb8 => value & 0b111,
    }
}

/// BT.601 luma from an RGB triple.
#[must_use]
pub fn luma(rgb: [f64; 3]) -> f64 {
    0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2]
}

/// What the demodulator concluded about one cell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellReading {
    /// The recovered data value, meaningful only when `usable` is true.
    pub value: u8,
    /// How far the sample sat from the nearest decision boundary, normalised
    /// to `[0, 1]`. Zero means the sample landed exactly on the threshold.
    pub confidence: f64,
    /// False when the cell is known-bad, e.g. an `Rgb4` parity violation.
    pub usable: bool,
}

/// Per-frame colour thresholds derived from the eight calibration patches.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Calibration {
    /// Mean measured colour of each of the eight reference patches.
    patches: [[f64; 3]; 8],
    /// Per-channel mean when that channel's bit is 0.
    low: [f64; 3],
    /// Per-channel mean when that channel's bit is 1.
    high: [f64; 3],
    luma_low: f64,
    luma_high: f64,
}

impl Calibration {
    /// Derive thresholds from the eight measured patch colours, indexed by
    /// colour code.
    #[must_use]
    pub fn from_patches(patches: [[f64; 3]; 8]) -> Self {
        let mut low = [0.0f64; 3];
        let mut high = [0.0f64; 3];
        for ch in 0..3 {
            let mut lo = 0.0;
            let mut hi = 0.0;
            for (code, patch) in patches.iter().enumerate() {
                // Four codes have this channel off and four have it on.
                if code & (1 << ch) != 0 {
                    hi += patch[ch];
                } else {
                    lo += patch[ch];
                }
            }
            low[ch] = lo / 4.0;
            high[ch] = hi / 4.0;
        }

        Self {
            patches,
            low,
            high,
            luma_low: luma(patches[0b000]),
            luma_high: luma(patches[0b111]),
        }
    }

    /// A neutral calibration assuming an ideal, uncorrected display.
    ///
    /// Only useful for synthetic input; real captures should always calibrate
    /// from the patches in the frame.
    #[must_use]
    pub fn ideal() -> Self {
        let mut patches = [[0.0f64; 3]; 8];
        for (code, patch) in patches.iter_mut().enumerate() {
            let rgb = color_of(code as u8);
            *patch = [rgb[0] as f64, rgb[1] as f64, rgb[2] as f64];
        }
        Self::from_patches(patches)
    }

    /// Measured colour of one reference patch.
    #[must_use]
    pub fn patch(&self, code: u8) -> [f64; 3] {
        self.patches[(code & 0b111) as usize]
    }

    /// Whether the patches separated well enough to trust this calibration.
    ///
    /// A frame photographed with the wrong exposure can crush every patch to
    /// the same value, at which point every threshold is noise.
    #[must_use]
    pub fn is_usable(&self) -> bool {
        let luma_gap = self.luma_high - self.luma_low;
        if luma_gap < 20.0 {
            return false;
        }
        // At least one chroma channel must also separate, or colour modes are
        // hopeless even though mono might still work.
        (0..3).any(|ch| self.high[ch] - self.low[ch] > 20.0)
    }

    /// Decide one channel bit, with confidence.
    fn channel_bit(&self, ch: usize, value: f64) -> (bool, f64) {
        let lo = self.low[ch];
        let hi = self.high[ch];
        let threshold = (lo + hi) / 2.0;
        let half = (hi - lo) / 2.0;
        if half <= 1.0 {
            // Channel collapsed: report the bit but with no confidence at all.
            return (value > threshold, 0.0);
        }
        let margin = ((value - threshold) / half).abs().min(1.0);
        (value > threshold, margin)
    }

    fn luma_bit(&self, value: [f64; 3]) -> (bool, f64) {
        let l = luma(value);
        let threshold = (self.luma_low + self.luma_high) / 2.0;
        let half = (self.luma_high - self.luma_low) / 2.0;
        if half <= 1.0 {
            return (l > threshold, 0.0);
        }
        let margin = ((l - threshold) / half).abs().min(1.0);
        (l > threshold, margin)
    }

    /// Demodulate one sampled cell.
    #[must_use]
    pub fn demodulate(&self, mode: ColorMode, rgb: [u8; 3]) -> CellReading {
        let v = [rgb[0] as f64, rgb[1] as f64, rgb[2] as f64];

        match mode {
            ColorMode::Mono => {
                let (bit, confidence) = self.luma_bit(v);
                CellReading {
                    value: u8::from(bit),
                    confidence,
                    usable: true,
                }
            }
            ColorMode::Rgb8 => {
                let mut value = 0u8;
                let mut confidence = 1.0f64;
                for (ch, &measured) in v.iter().enumerate() {
                    let (bit, c) = self.channel_bit(ch, measured);
                    if bit {
                        value |= 1 << ch;
                    }
                    confidence = confidence.min(c);
                }
                CellReading {
                    value,
                    confidence,
                    usable: true,
                }
            }
            ColorMode::Rgb4 => {
                let mut code = 0u8;
                let mut confidence = 1.0f64;
                for (ch, &measured) in v.iter().enumerate() {
                    let (bit, c) = self.channel_bit(ch, measured);
                    if bit {
                        code |= 1 << ch;
                    }
                    confidence = confidence.min(c);
                }
                // Even parity is the whole point of this mode.
                match RGB4_CODEWORDS.iter().position(|&w| w == code) {
                    Some(value) => CellReading {
                        value: value as u8,
                        confidence,
                        usable: true,
                    },
                    None => CellReading {
                        value: 0,
                        confidence: 0.0,
                        usable: false,
                    },
                }
            }
        }
    }
}

/// Average a set of sampled colours into a single patch measurement.
#[must_use]
pub fn mean_color(samples: &[[u8; 3]]) -> [f64; 3] {
    if samples.is_empty() {
        return [0.0; 3];
    }
    let mut acc = [0.0f64; 3];
    for s in samples {
        for ch in 0..3 {
            acc[ch] += s[ch] as f64;
        }
    }
    let n = samples.len() as f64;
    [acc[0] / n, acc[1] / n, acc[2] / n]
}

/// Pack a sequence of `bits_per_cell`-wide values into bytes, most significant
/// bit first.
#[must_use]
pub fn pack_bits(values: &[u8], bits_per_cell: usize, byte_len: usize) -> Vec<u8> {
    let mut out = alloc::vec![0u8; byte_len];
    let mut bit_pos = 0usize;
    let total_bits = byte_len * 8;
    for &v in values {
        for b in (0..bits_per_cell).rev() {
            if bit_pos >= total_bits {
                return out;
            }
            if v >> b & 1 != 0 {
                out[bit_pos / 8] |= 0x80 >> (bit_pos % 8);
            }
            bit_pos += 1;
        }
    }
    out
}

/// Split bytes into `bits_per_cell`-wide values, most significant bit first.
#[must_use]
pub fn unpack_bits(bytes: &[u8], bits_per_cell: usize, count: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(count);
    let total_bits = bytes.len() * 8;
    let mut bit_pos = 0usize;
    for _ in 0..count {
        let mut v = 0u8;
        for _ in 0..bits_per_cell {
            let bit = if bit_pos < total_bits {
                (bytes[bit_pos / 8] >> (7 - bit_pos % 8)) & 1
            } else {
                0
            };
            v = (v << 1) | bit;
            bit_pos += 1;
        }
        out.push(v);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_codes_round_trip() {
        for m in [ColorMode::Mono, ColorMode::Rgb4, ColorMode::Rgb8] {
            assert_eq!(ColorMode::from_code(m.code()), Some(m));
        }
        assert!(ColorMode::from_code(7).is_none());
    }

    #[test]
    fn color_codes_map_to_rgb_corners() {
        assert_eq!(color_of(0b000), [0, 0, 0]);
        assert_eq!(color_of(0b001), [255, 0, 0]);
        assert_eq!(color_of(0b010), [0, 255, 0]);
        assert_eq!(color_of(0b100), [0, 0, 255]);
        assert_eq!(color_of(0b111), [255, 255, 255]);
    }

    #[test]
    fn rgb4_codewords_all_have_even_weight() {
        for w in RGB4_CODEWORDS {
            assert_eq!(w.count_ones() % 2, 0, "codeword {w:03b} is odd weight");
        }
        // And they must be distinct.
        for (i, a) in RGB4_CODEWORDS.iter().enumerate() {
            for b in &RGB4_CODEWORDS[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn ideal_calibration_demodulates_every_symbol() {
        let cal = Calibration::ideal();
        assert!(cal.is_usable());

        for value in 0..8u8 {
            let code = modulate(ColorMode::Rgb8, value);
            let reading = cal.demodulate(ColorMode::Rgb8, color_of(code));
            assert!(reading.usable);
            assert_eq!(reading.value, value, "rgb8 value {value}");
            assert!(
                reading.confidence > 0.9,
                "confidence {}",
                reading.confidence
            );
        }

        for value in 0..4u8 {
            let code = modulate(ColorMode::Rgb4, value);
            let reading = cal.demodulate(ColorMode::Rgb4, color_of(code));
            assert!(reading.usable);
            assert_eq!(reading.value, value, "rgb4 value {value}");
        }

        for value in 0..2u8 {
            let code = modulate(ColorMode::Mono, value);
            let reading = cal.demodulate(ColorMode::Mono, color_of(code));
            assert_eq!(reading.value, value, "mono value {value}");
        }
    }

    #[test]
    fn rgb4_flags_a_single_channel_error_as_an_erasure() {
        let cal = Calibration::ideal();
        // Yellow is 011. Knock the red channel down and it becomes 010, which
        // has odd weight and is not a legal codeword.
        let broken = [10, 250, 10];
        let reading = cal.demodulate(ColorMode::Rgb4, broken);
        assert!(!reading.usable, "parity violation should be unusable");
        assert_eq!(reading.confidence, 0.0);
    }

    #[test]
    fn rgb8_cannot_detect_the_same_error() {
        // The same corruption in Rgb8 silently decodes to a wrong value; this
        // is exactly the trade-off Rgb4 exists to avoid.
        let cal = Calibration::ideal();
        let reading = cal.demodulate(ColorMode::Rgb8, [10, 250, 10]);
        assert!(reading.usable);
        assert_eq!(reading.value, 0b010);
    }

    #[test]
    fn confidence_falls_to_zero_at_the_decision_boundary() {
        let cal = Calibration::ideal();
        // Mid grey sits exactly on every threshold.
        let reading = cal.demodulate(ColorMode::Rgb8, [128, 128, 128]);
        assert!(
            reading.confidence < 0.05,
            "boundary sample should have no confidence, got {}",
            reading.confidence
        );
        // A clean sample should be confident.
        let clean = cal.demodulate(ColorMode::Rgb8, [255, 0, 0]);
        assert!(clean.confidence > 0.9);
    }

    #[test]
    fn calibration_tracks_a_washed_out_capture() {
        // Simulate a camera that crushes blacks to 60 and whites to 200 and
        // adds a warm cast. Fixed thresholds at 128 would misread the blue
        // channel; a calibrated one must not.
        let mut patches = [[0.0f64; 3]; 8];
        for (code, patch) in patches.iter_mut().enumerate() {
            let rgb = color_of(code as u8);
            for ch in 0..3 {
                let base = if rgb[ch] > 0 { 200.0 } else { 60.0 };
                let cast = [18.0, 4.0, -12.0][ch];
                patch[ch] = base + cast;
            }
        }
        let cal = Calibration::from_patches(patches);
        assert!(cal.is_usable());

        for value in 0..8u8 {
            let code = modulate(ColorMode::Rgb8, value);
            let ideal = color_of(code);
            let measured = [
                (if ideal[0] > 0 { 218.0 } else { 78.0 }) as u8,
                (if ideal[1] > 0 { 204.0 } else { 64.0 }) as u8,
                (if ideal[2] > 0 { 188.0 } else { 48.0 }) as u8,
            ];
            let reading = cal.demodulate(ColorMode::Rgb8, measured);
            assert_eq!(reading.value, value, "washed out value {value}");
        }
    }

    #[test]
    fn collapsed_calibration_is_rejected() {
        // Every patch the same: nothing can be recovered.
        let patches = [[100.0f64; 3]; 8];
        let cal = Calibration::from_patches(patches);
        assert!(!cal.is_usable());
        let reading = cal.demodulate(ColorMode::Rgb8, [100, 100, 100]);
        assert_eq!(reading.confidence, 0.0);
    }

    #[test]
    fn mean_color_averages() {
        assert_eq!(mean_color(&[[0, 0, 0], [255, 255, 255]]), [127.5; 3]);
        assert_eq!(mean_color(&[]), [0.0; 3]);
    }

    #[test]
    fn bit_packing_round_trips_for_every_mode() {
        for bits in [1usize, 2, 3] {
            let count = 100;
            let mask = (1u8 << bits) - 1;
            let values: Vec<u8> = (0..count)
                .map(|i| (i as u8).wrapping_mul(37) & mask)
                .collect();
            let byte_len = count * bits / 8;
            let packed = pack_bits(&values, bits, byte_len);
            let unpacked = unpack_bits(&packed, bits, count);
            // Only whole bytes survive; compare the prefix that fits.
            let usable = byte_len * 8 / bits;
            assert_eq!(&unpacked[..usable], &values[..usable], "bits={bits}");
        }
    }

    #[test]
    fn packing_is_msb_first() {
        // Two 3-bit values 0b101, 0b011 -> 101011... in the first byte.
        let packed = pack_bits(&[0b101, 0b011], 3, 1);
        assert_eq!(packed[0] >> 2, 0b101011);
    }

    #[test]
    fn packing_truncates_rather_than_panics() {
        let packed = pack_bits(&[7; 100], 3, 2);
        assert_eq!(packed.len(), 2);
        let unpacked = unpack_bits(&[0xFF], 3, 50);
        assert_eq!(unpacked.len(), 50);
    }
}
