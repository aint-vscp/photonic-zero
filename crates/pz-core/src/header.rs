//! The 16-byte frame header and its RS(32,16) armour.
//!
//! Every frame is self-describing. A receiver that walks up to a stream
//! already in progress, having missed the beginning, can decode a single frame
//! and learn everything it needs: the grid size, the colour mode, the parity
//! ratio, which session this is, which frame index it is, and how long the
//! whole message will be. There is no separate handshake or manifest frame to
//! miss.
//!
//! That makes the header the single point of failure for the frame, so it gets
//! the heaviest protection in the format: 16 bytes of content expanded to 32
//! by a rate-1/2 Reed-Solomon code, modulated in plain black and white even
//! when the payload is using eight colours. Half the header can be destroyed
//! and it still parses.
//!
//! ```text
//! byte  0   version (4 bits) | colour mode (4 bits)
//! byte  1   grid size (4 bits) | parity ratio index (4 bits)
//! bytes 2-3   session id            u16 big endian
//! bytes 4-7   frame index           u32 big endian
//! bytes 8-11  payload length        u32 big endian
//! byte  12  flags
//! byte  13  reserved, must be zero
//! bytes 14-15 CRC-16/CCITT-FALSE over bytes 0..13
//! ```

use crate::color::ColorMode;
use crate::layout::GridSize;
use crate::PzError;
use alloc::vec::Vec;
use pz_fec::{crc16, ReedSolomon};

/// Wire format version this implementation speaks.
pub const PROTOCOL_VERSION: u8 = 1;

/// Header size before error correction.
pub const HEADER_BYTES: usize = 16;

/// Header size after RS(32,16) expansion.
pub const HEADER_CODE_BYTES: usize = 32;

/// Flag bit: the reassembled payload begins with a 4-byte CRC-32 of the user
/// data. Always set in version 1.
pub const FLAG_CRC32_PREFIX: u8 = 0b0000_0001;

/// The per-frame metadata that makes a frame self-describing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    /// Wire format version.
    pub version: u8,
    /// Colour mode used by the data cells.
    pub mode: ColorMode,
    /// Grid size of this frame.
    pub grid: GridSize,
    /// Index into the parity ratio table.
    pub parity_code: u8,
    /// Identifies the transmission; frames from other sessions are discarded.
    pub session_id: u16,
    /// Which droplet this frame carries.
    pub frame_index: u32,
    /// Total length of the transmitted container in bytes.
    pub payload_len: u32,
    /// Feature flags, see [`FLAG_CRC32_PREFIX`].
    pub flags: u8,
}

impl FrameHeader {
    /// Serialise to the 16-byte on-wire form, including the trailing CRC.
    #[must_use]
    pub fn encode(&self) -> [u8; HEADER_BYTES] {
        let mut b = [0u8; HEADER_BYTES];
        b[0] = (self.version << 4) | (self.mode.code() & 0x0F);
        b[1] = (self.grid.code() << 4) | (self.parity_code & 0x0F);
        b[2..4].copy_from_slice(&self.session_id.to_be_bytes());
        b[4..8].copy_from_slice(&self.frame_index.to_be_bytes());
        b[8..12].copy_from_slice(&self.payload_len.to_be_bytes());
        b[12] = self.flags;
        b[13] = 0;
        let crc = crc16(&b[..14]);
        b[14..16].copy_from_slice(&crc.to_be_bytes());
        b
    }

    /// Parse the 16-byte form, validating the CRC and every enumerated field.
    ///
    /// # Errors
    /// Returns [`PzError::HeaderCorrupt`] on a CRC mismatch, and
    /// [`PzError::UnsupportedFormat`] for a version, mode or grid size this
    /// build does not implement.
    pub fn decode(bytes: &[u8]) -> Result<Self, PzError> {
        if bytes.len() < HEADER_BYTES {
            return Err(PzError::HeaderCorrupt);
        }
        let expected = crc16(&bytes[..14]);
        let actual = u16::from_be_bytes([bytes[14], bytes[15]]);
        if expected != actual {
            return Err(PzError::HeaderCorrupt);
        }

        let version = bytes[0] >> 4;
        if version != PROTOCOL_VERSION {
            return Err(PzError::UnsupportedFormat);
        }
        let mode = ColorMode::from_code(bytes[0] & 0x0F).ok_or(PzError::UnsupportedFormat)?;
        let grid = GridSize::from_code(bytes[1] >> 4).ok_or(PzError::UnsupportedFormat)?;
        let parity_code = bytes[1] & 0x0F;
        if parity_code as usize >= crate::frame::PARITY_RATIOS.len() {
            return Err(PzError::UnsupportedFormat);
        }

        Ok(Self {
            version,
            mode,
            grid,
            parity_code,
            session_id: u16::from_be_bytes([bytes[2], bytes[3]]),
            frame_index: u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            payload_len: u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            flags: bytes[12],
        })
    }

    /// Serialise and expand to the 32-byte error-corrected form.
    ///
    /// # Errors
    /// Returns [`PzError::Fec`] only if the fixed RS parameters are rejected,
    /// which cannot happen for the compiled-in constants.
    pub fn encode_protected(&self) -> Result<Vec<u8>, PzError> {
        let rs = ReedSolomon::new(HEADER_CODE_BYTES, HEADER_BYTES)?;
        Ok(rs.encode(&self.encode())?)
    }

    /// Repair and parse the 32-byte error-corrected form.
    ///
    /// `erasures` are indices of symbols the demodulator flagged as unreliable.
    ///
    /// # Errors
    /// Returns [`PzError::HeaderCorrupt`] when the damage exceeds what
    /// RS(32,16) can repair.
    pub fn decode_protected(code: &[u8], erasures: &[usize]) -> Result<Self, PzError> {
        if code.len() != HEADER_CODE_BYTES {
            return Err(PzError::HeaderCorrupt);
        }
        let rs = ReedSolomon::new(HEADER_CODE_BYTES, HEADER_BYTES)?;
        let mut buf = code.to_vec();

        // Try with the erasure hints first; if the demodulator's confidence
        // estimates were themselves wrong, fall back to treating everything as
        // an unknown error, which costs twice as much but ignores bad hints.
        if rs.decode(&mut buf, erasures).is_err() {
            buf.copy_from_slice(code);
            rs.decode(&mut buf, &[])
                .map_err(|_| PzError::HeaderCorrupt)?;
        }
        Self::decode(&buf[..HEADER_BYTES])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> FrameHeader {
        FrameHeader {
            version: PROTOCOL_VERSION,
            mode: ColorMode::Rgb8,
            grid: GridSize::G65,
            parity_code: 3,
            session_id: 0xBEEF,
            frame_index: 123_456,
            payload_len: 1_048_576,
            flags: FLAG_CRC32_PREFIX,
        }
    }

    #[test]
    fn round_trips_through_the_plain_form() {
        let h = sample();
        let bytes = h.encode();
        assert_eq!(bytes.len(), HEADER_BYTES);
        assert_eq!(FrameHeader::decode(&bytes).unwrap(), h);
    }

    #[test]
    fn round_trips_through_the_protected_form() {
        let h = sample();
        let code = h.encode_protected().unwrap();
        assert_eq!(code.len(), HEADER_CODE_BYTES);
        assert_eq!(FrameHeader::decode_protected(&code, &[]).unwrap(), h);
    }

    #[test]
    fn every_field_survives_the_round_trip() {
        for mode in [ColorMode::Mono, ColorMode::Rgb4, ColorMode::Rgb8] {
            for grid in GridSize::ALL {
                for parity_code in 0..8u8 {
                    let h = FrameHeader {
                        version: PROTOCOL_VERSION,
                        mode,
                        grid,
                        parity_code,
                        session_id: 0x1234,
                        frame_index: u32::MAX,
                        payload_len: 42,
                        flags: FLAG_CRC32_PREFIX,
                    };
                    let code = h.encode_protected().unwrap();
                    assert_eq!(FrameHeader::decode_protected(&code, &[]).unwrap(), h);
                }
            }
        }
    }

    #[test]
    fn survives_eight_corrupted_symbols() {
        let h = sample();
        let mut code = h.encode_protected().unwrap();
        // RS(32,16) corrects up to (32-16)/2 = 8 unknown errors.
        for i in 0..8 {
            code[i * 3 % HEADER_CODE_BYTES] ^= 0xA5;
        }
        assert_eq!(FrameHeader::decode_protected(&code, &[]).unwrap(), h);
    }

    #[test]
    fn survives_sixteen_erasures() {
        let h = sample();
        let mut code = h.encode_protected().unwrap();
        let erasures: Vec<usize> = (0..16).map(|i| i * 2).collect();
        for &e in &erasures {
            code[e] = 0x00;
        }
        assert_eq!(FrameHeader::decode_protected(&code, &erasures).unwrap(), h);
    }

    #[test]
    fn ignores_wrong_erasure_hints() {
        // The demodulator can be wrong about which cells are unreliable. A bad
        // hint must not make an otherwise-repairable header fail.
        let h = sample();
        let mut code = h.encode_protected().unwrap();
        code[0] ^= 0xFF;
        code[1] ^= 0xFF;
        // Point the hints at completely healthy symbols.
        let bogus: Vec<usize> = (20..32).collect();
        assert_eq!(FrameHeader::decode_protected(&code, &bogus).unwrap(), h);
    }

    #[test]
    fn rejects_damage_beyond_the_correction_radius() {
        let h = sample();
        let mut code = h.encode_protected().unwrap();
        for (i, b) in code.iter_mut().enumerate() {
            if i % 2 == 0 {
                *b ^= 0x5A;
            }
        }
        assert!(matches!(
            FrameHeader::decode_protected(&code, &[]),
            Err(PzError::HeaderCorrupt)
        ));
    }

    #[test]
    fn rejects_a_bad_crc() {
        let h = sample();
        let mut bytes = h.encode();
        bytes[5] ^= 0x01;
        assert!(matches!(
            FrameHeader::decode(&bytes),
            Err(PzError::HeaderCorrupt)
        ));
    }

    #[test]
    fn rejects_a_future_version() {
        let mut h = sample();
        h.version = 2;
        let bytes = h.encode();
        assert!(matches!(
            FrameHeader::decode(&bytes),
            Err(PzError::UnsupportedFormat)
        ));
    }

    #[test]
    fn rejects_unknown_enumerated_values() {
        let h = sample();
        let mut bytes = h.encode();
        // Mode 0x0F does not exist. Recompute the CRC so we are testing the
        // enum check, not the checksum.
        bytes[0] = (PROTOCOL_VERSION << 4) | 0x0F;
        let crc = crc16(&bytes[..14]);
        bytes[14..16].copy_from_slice(&crc.to_be_bytes());
        assert!(matches!(
            FrameHeader::decode(&bytes),
            Err(PzError::UnsupportedFormat)
        ));

        // Grid code 0x0F does not exist either.
        let mut bytes = h.encode();
        bytes[1] = 0xF0 | 3;
        let crc = crc16(&bytes[..14]);
        bytes[14..16].copy_from_slice(&crc.to_be_bytes());
        assert!(matches!(
            FrameHeader::decode(&bytes),
            Err(PzError::UnsupportedFormat)
        ));
    }

    #[test]
    fn rejects_short_input() {
        assert!(matches!(
            FrameHeader::decode(&[0u8; 4]),
            Err(PzError::HeaderCorrupt)
        ));
        assert!(matches!(
            FrameHeader::decode_protected(&[0u8; 8], &[]),
            Err(PzError::HeaderCorrupt)
        ));
    }

    #[test]
    fn reserved_byte_is_zero_on_the_wire() {
        assert_eq!(sample().encode()[13], 0);
    }
}
