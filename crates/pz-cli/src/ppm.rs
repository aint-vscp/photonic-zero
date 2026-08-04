//! Binary PPM (Netpbm P6) reading and writing.
//!
//! PPM is here because it is the simplest possible way to get pixels in and
//! out of the tool without a dependency: a short ASCII header followed by raw
//! RGB bytes. `ffmpeg`, ImageMagick, GIMP and most other things read and write
//! it, so a capture from any source can be piped through one conversion and
//! fed straight to `pz decode`.
//!
//! The library itself takes any RGB or RGBA buffer, so a real application
//! never needs this.

use pz_core::render::RgbImage;

/// Serialise an image as binary PPM.
pub fn write(img: &RgbImage) -> Vec<u8> {
    let mut out = Vec::with_capacity(img.data.len() + 32);
    out.extend_from_slice(format!("P6\n{} {}\n255\n", img.width, img.height).as_bytes());
    out.extend_from_slice(&img.data);
    out
}

/// Parse a binary PPM.
///
/// Accepts the `P6` maxval-255 form, which is what every tool produces by
/// default, and tolerates comments in the header.
pub fn read(bytes: &[u8]) -> Result<RgbImage, String> {
    let mut pos = 0usize;

    let token = |pos: &mut usize| -> Result<String, String> {
        // Skip whitespace and full-line comments.
        loop {
            while *pos < bytes.len() && bytes[*pos].is_ascii_whitespace() {
                *pos += 1;
            }
            if *pos < bytes.len() && bytes[*pos] == b'#' {
                while *pos < bytes.len() && bytes[*pos] != b'\n' {
                    *pos += 1;
                }
            } else {
                break;
            }
        }
        let start = *pos;
        while *pos < bytes.len() && !bytes[*pos].is_ascii_whitespace() {
            *pos += 1;
        }
        if start == *pos {
            return Err("unexpected end of PPM header".to_string());
        }
        String::from_utf8(bytes[start..*pos].to_vec()).map_err(|_| "bad PPM header".to_string())
    };

    let magic = token(&mut pos)?;
    if magic != "P6" {
        return Err(format!("expected a binary PPM (P6), found {magic}"));
    }
    let width: usize = token(&mut pos)?
        .parse()
        .map_err(|_| "bad PPM width".to_string())?;
    let height: usize = token(&mut pos)?
        .parse()
        .map_err(|_| "bad PPM height".to_string())?;
    let maxval: usize = token(&mut pos)?
        .parse()
        .map_err(|_| "bad PPM maxval".to_string())?;
    if maxval != 255 {
        return Err(format!(
            "only 8-bit PPM is supported, found maxval {maxval}"
        ));
    }

    // Exactly one whitespace byte separates the header from the pixel data.
    pos += 1;

    let needed = width * height * 3;
    if bytes.len() < pos + needed {
        return Err(format!(
            "PPM truncated: expected {needed} pixel bytes, found {}",
            bytes.len().saturating_sub(pos)
        ));
    }

    Ok(RgbImage {
        width,
        height,
        data: bytes[pos..pos + needed].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> RgbImage {
        let mut img = RgbImage::new(3, 2);
        for y in 0..2 {
            for x in 0..3 {
                img.set(x, y, [(x * 40) as u8, (y * 90) as u8, 7]);
            }
        }
        img
    }

    #[test]
    fn round_trips() {
        let img = sample();
        let encoded = write(&img);
        let decoded = read(&encoded).unwrap();
        assert_eq!(decoded.width, img.width);
        assert_eq!(decoded.height, img.height);
        assert_eq!(decoded.data, img.data);
    }

    #[test]
    fn header_is_well_formed() {
        let encoded = write(&sample());
        assert!(encoded.starts_with(b"P6\n3 2\n255\n"));
    }

    #[test]
    fn tolerates_comments_in_the_header() {
        let mut input = b"P6\n# written by something else\n3 2\n255\n".to_vec();
        input.extend_from_slice(&[0u8; 18]);
        let img = read(&input).unwrap();
        assert_eq!((img.width, img.height), (3, 2));
    }

    #[test]
    fn rejects_the_ascii_variant() {
        let err = read(b"P3\n1 1\n255\n0 0 0\n").unwrap_err();
        assert!(err.contains("P6"), "unhelpful error: {err}");
    }

    #[test]
    fn rejects_sixteen_bit_samples() {
        let err = read(b"P6\n1 1\n65535\n").unwrap_err();
        assert!(err.contains("8-bit"), "unhelpful error: {err}");
    }

    #[test]
    fn rejects_truncated_pixel_data() {
        let err = read(b"P6\n10 10\n255\nshort").unwrap_err();
        assert!(err.contains("truncated"), "unhelpful error: {err}");
    }

    #[test]
    fn rejects_garbage() {
        assert!(read(b"").is_err());
        assert!(read(b"not an image").is_err());
    }
}
