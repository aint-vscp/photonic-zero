//! Checksums used by the PZ wire format.
//!
//! Both are computed bitwise rather than through a lookup table: the tables
//! would dwarf the code, these run on at most a few kilobytes per frame, and a
//! table-free implementation is trivial to re-derive in any of the binding
//! languages.

/// CRC-16/CCITT-FALSE. Polynomial `0x1021`, init `0xFFFF`, no reflection, no
/// final XOR. Used to validate the 16-byte frame header.
#[must_use]
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// CRC-32/ISO-HDLC, the checksum used by zlib, PNG and Ethernet. Polynomial
/// `0xEDB88320` (reflected), init `0xFFFFFFFF`, final XOR `0xFFFFFFFF`.
///
/// Used both to authenticate a decoded frame payload and to verify the fully
/// reassembled message.
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc16_known_vectors() {
        // The canonical CCITT-FALSE check value.
        assert_eq!(crc16(b"123456789"), 0x29B1);
        assert_eq!(crc16(b""), 0xFFFF);
    }

    #[test]
    fn crc32_known_vectors() {
        // The canonical CRC-32 check value.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0x0000_0000);
        assert_eq!(
            crc32(b"The quick brown fox jumps over the lazy dog"),
            0x414F_A339
        );
    }

    #[test]
    fn single_bit_flips_are_detected() {
        let msg = b"photonic zero frame payload";
        let base16 = crc16(msg);
        let base32 = crc32(msg);
        for byte in 0..msg.len() {
            for bit in 0..8 {
                let mut m = msg.to_vec();
                m[byte] ^= 1 << bit;
                assert_ne!(crc16(&m), base16, "crc16 missed flip {byte}:{bit}");
                assert_ne!(crc32(&m), base32, "crc32 missed flip {byte}:{bit}");
            }
        }
    }
}
