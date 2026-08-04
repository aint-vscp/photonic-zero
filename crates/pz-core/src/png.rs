//! A minimal, dependency-free PNG writer.
//!
//! PZ frames are flat blocks of solid colour, so compression buys almost
//! nothing that matters here, and pulling in a deflate implementation to save a
//! few kilobytes would be the largest dependency in the entire project. This
//! writer emits a valid PNG using deflate's *stored* block type: no
//! compression, but a completely standard file that every decoder reads.
//!
//! Output is roughly the size of the raw pixels. For a 49-cell frame at 8
//! pixels per cell that is about 990 KB before the operating system's own
//! compression - fine for writing frames to disk, and not on the hot path for
//! anything that matters.

use crate::render::RgbImage;
use alloc::vec::Vec;
use pz_fec::crc32;

const SIGNATURE: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];

/// Adler-32, the checksum zlib streams carry.
fn adler32(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);

    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

/// Wrap raw bytes in a zlib stream of uncompressed deflate blocks.
fn zlib_stored(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + raw.len() / 65535 * 5 + 16);
    // CMF = 0x78 (deflate, 32K window), FLG = 0x01 so that CMF*256+FLG is a
    // multiple of 31, as the zlib header requires.
    out.push(0x78);
    out.push(0x01);

    if raw.is_empty() {
        out.push(0x01);
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(!0u16).to_le_bytes());
    } else {
        let mut offset = 0usize;
        while offset < raw.len() {
            let len = (raw.len() - offset).min(65535);
            let final_block = offset + len >= raw.len();
            out.push(u8::from(final_block)); // BFINAL, BTYPE = 00 (stored)
            out.extend_from_slice(&(len as u16).to_le_bytes());
            out.extend_from_slice(&(!(len as u16)).to_le_bytes());
            out.extend_from_slice(&raw[offset..offset + len]);
            offset += len;
        }
    }

    out.extend_from_slice(&adler32(raw).to_be_bytes());
    out
}

/// Encode an image as a PNG byte stream.
#[must_use]
pub fn encode(img: &RgbImage) -> Vec<u8> {
    let mut out = Vec::with_capacity(img.data.len() + 1024);
    out.extend_from_slice(&SIGNATURE);

    // IHDR: 8-bit truecolour, no interlacing.
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(img.width as u32).to_be_bytes());
    ihdr.extend_from_slice(&(img.height as u32).to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(2); // colour type: truecolour RGB
    ihdr.push(0); // compression method
    ihdr.push(0); // filter method
    ihdr.push(0); // interlace method
    write_chunk(&mut out, b"IHDR", &ihdr);

    // Scanlines, each prefixed with filter type 0 (None).
    let stride = img.width * 3;
    let mut raw = Vec::with_capacity((stride + 1) * img.height);
    for y in 0..img.height {
        raw.push(0);
        raw.extend_from_slice(&img.data[y * stride..(y + 1) * stride]);
    }

    write_chunk(&mut out, b"IDAT", &zlib_stored(&raw));
    write_chunk(&mut out, b"IEND", &[]);
    out
}

/// Decode a PNG produced by [`encode`].
///
/// This reads 8-bit truecolour PNGs (with or without alpha) whose image data
/// uses deflate *stored* blocks - which is exactly what [`encode`] emits. All
/// five PNG row filters are supported, so a file that has been re-saved by
/// another tool without recompressing will still load.
///
/// It is deliberately not a general PNG decoder: implementing Huffman decoding
/// to read arbitrary PNGs would be a large amount of code for a path that only
/// exists as a convenience. Anything else returns an error telling the caller
/// to convert first.
///
/// # Errors
/// Returns a description of the first structural problem found.
pub fn decode(bytes: &[u8]) -> Result<RgbImage, alloc::string::String> {
    // With the `std` feature these come from the std prelude; without it they
    // must be imported from `alloc`. Compilers before 1.79 report the former
    // case as a redundant import, so the lint is silenced rather than the
    // import being made conditional, which would be noisier.
    #[allow(unused_imports)]
    use alloc::{format, string::ToString};

    if bytes.len() < 8 || bytes[..8] != SIGNATURE {
        return Err("not a PNG file".to_string());
    }

    let mut pos = 8usize;
    let mut width = 0usize;
    let mut height = 0usize;
    let mut channels = 0usize;
    let mut idat: Vec<u8> = Vec::new();
    let mut seen_header = false;

    while pos + 8 <= bytes.len() {
        let len = u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
            as usize;
        let kind = &bytes[pos + 4..pos + 8];
        let start = pos + 8;
        let end = start
            .checked_add(len)
            .filter(|e| e + 4 <= bytes.len())
            .ok_or_else(|| "truncated PNG chunk".to_string())?;

        match kind {
            b"IHDR" => {
                if len < 13 {
                    return Err("malformed IHDR".to_string());
                }
                width = u32::from_be_bytes([
                    bytes[start],
                    bytes[start + 1],
                    bytes[start + 2],
                    bytes[start + 3],
                ]) as usize;
                height = u32::from_be_bytes([
                    bytes[start + 4],
                    bytes[start + 5],
                    bytes[start + 6],
                    bytes[start + 7],
                ]) as usize;
                let depth = bytes[start + 8];
                let color_type = bytes[start + 9];
                let interlace = bytes[start + 12];
                if depth != 8 {
                    return Err(format!("only 8-bit PNGs are supported, found {depth}-bit"));
                }
                if interlace != 0 {
                    return Err("interlaced PNGs are not supported".to_string());
                }
                channels = match color_type {
                    2 => 3,
                    6 => 4,
                    other => {
                        return Err(format!(
                            "only truecolour PNGs are supported, found colour type {other}"
                        ))
                    }
                };
                seen_header = true;
            }
            b"IDAT" => idat.extend_from_slice(&bytes[start..end]),
            b"IEND" => break,
            _ => {}
        }
        pos = end + 4;
    }

    if !seen_header {
        return Err("PNG has no IHDR".to_string());
    }
    if width == 0 || height == 0 {
        return Err("PNG has zero extent".to_string());
    }
    if idat.len() < 6 {
        return Err("PNG has no image data".to_string());
    }

    // Walk the zlib stream, accepting only stored blocks.
    let mut raw: Vec<u8> = Vec::with_capacity((width * channels + 1) * height);
    let mut p = 2usize; // skip the 2-byte zlib header
    loop {
        if p + 5 > idat.len() {
            return Err("truncated deflate stream".to_string());
        }
        let header = idat[p];
        let final_block = header & 1 != 0;
        let block_type = (header >> 1) & 0b11;
        if block_type != 0 {
            return Err(
                "this PNG uses compressed image data, which pz cannot read; \
                 convert it first, e.g. `ffmpeg -i in.png out.ppm`"
                    .to_string(),
            );
        }
        let len = u16::from_le_bytes([idat[p + 1], idat[p + 2]]) as usize;
        let nlen = u16::from_le_bytes([idat[p + 3], idat[p + 4]]);
        if len as u16 != !nlen {
            return Err("corrupt deflate block length".to_string());
        }
        let data_start = p + 5;
        let data_end = data_start
            .checked_add(len)
            .filter(|e| *e <= idat.len())
            .ok_or_else(|| "truncated deflate block".to_string())?;
        raw.extend_from_slice(&idat[data_start..data_end]);
        p = data_end;
        if final_block {
            break;
        }
    }

    let stride = width * channels;
    if raw.len() < (stride + 1) * height {
        return Err(format!(
            "PNG image data is short: expected {} bytes, found {}",
            (stride + 1) * height,
            raw.len()
        ));
    }

    // Undo the per-row filters. Each row is prefixed with its filter type and
    // may refer back to the row above and to the pixel `channels` bytes to the
    // left, both already unfiltered by the time we reach them.
    let mut image = RgbImage::new(width, height);
    let mut previous: Vec<u8> = vec![0u8; stride];
    let mut current: Vec<u8> = vec![0u8; stride];

    for y in 0..height {
        let row_start = y * (stride + 1);
        let filter = raw[row_start];
        current.copy_from_slice(&raw[row_start + 1..row_start + 1 + stride]);

        for i in 0..stride {
            let a = if i >= channels {
                current[i - channels]
            } else {
                0
            } as i32;
            let b = previous[i] as i32;
            let c = if i >= channels {
                previous[i - channels]
            } else {
                0
            } as i32;
            let x = current[i] as i32;

            current[i] = match filter {
                0 => x,
                1 => x + a,
                2 => x + b,
                3 => x + (a + b) / 2,
                4 => {
                    // Paeth: pick whichever neighbour the gradient predicts.
                    let p = a + b - c;
                    let pa = (p - a).abs();
                    let pb = (p - b).abs();
                    let pc = (p - c).abs();
                    let pred = if pa <= pb && pa <= pc {
                        a
                    } else if pb <= pc {
                        b
                    } else {
                        c
                    };
                    x + pred
                }
                other => return Err(format!("unknown PNG row filter {other}")),
            } as u8;
        }

        for x in 0..width {
            let i = x * channels;
            image.set(x, y, [current[i], current[i + 1], current[i + 2]]);
        }
        previous.copy_from_slice(&current);
    }

    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_image() -> RgbImage {
        let mut img = RgbImage::new(7, 5);
        for y in 0..5 {
            for x in 0..7 {
                img.set(x, y, [(x * 30) as u8, (y * 50) as u8, 128]);
            }
        }
        img
    }

    #[test]
    fn adler32_matches_known_vectors() {
        assert_eq!(adler32(b""), 1);
        assert_eq!(adler32(b"a"), 0x0062_0062);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn starts_with_the_png_signature() {
        let png = encode(&test_image());
        assert_eq!(&png[..8], &SIGNATURE);
    }

    #[test]
    fn chunk_layout_is_well_formed() {
        let png = encode(&test_image());
        let mut pos = 8;
        let mut kinds = Vec::new();

        while pos + 8 <= png.len() {
            let len =
                u32::from_be_bytes([png[pos], png[pos + 1], png[pos + 2], png[pos + 3]]) as usize;
            let kind = [png[pos + 4], png[pos + 5], png[pos + 6], png[pos + 7]];
            let data_start = pos + 8;
            let data_end = data_start + len;
            assert!(data_end + 4 <= png.len(), "chunk overruns the file");

            // Verify the chunk CRC exactly as a decoder would.
            let mut crc_input = Vec::new();
            crc_input.extend_from_slice(&kind);
            crc_input.extend_from_slice(&png[data_start..data_end]);
            let stored = u32::from_be_bytes([
                png[data_end],
                png[data_end + 1],
                png[data_end + 2],
                png[data_end + 3],
            ]);
            assert_eq!(crc32(&crc_input), stored, "bad CRC on chunk {kind:?}");

            kinds.push(kind);
            pos = data_end + 4;
        }

        assert_eq!(pos, png.len(), "trailing bytes after the last chunk");
        assert_eq!(kinds.first().unwrap(), b"IHDR");
        assert_eq!(kinds.last().unwrap(), b"IEND");
    }

    #[test]
    fn ihdr_describes_the_image() {
        let img = test_image();
        let png = encode(&img);
        let w = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
        let h = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
        assert_eq!(w as usize, img.width);
        assert_eq!(h as usize, img.height);
        assert_eq!(png[24], 8, "bit depth");
        assert_eq!(png[25], 2, "colour type");
    }

    #[test]
    fn zlib_header_passes_the_modulo_31_check() {
        let stream = zlib_stored(b"some data");
        let check = (stream[0] as u32) * 256 + stream[1] as u32;
        assert_eq!(check % 31, 0, "invalid zlib header");
    }

    #[test]
    fn stored_blocks_have_complementary_lengths() {
        let raw = alloc::vec![0u8; 100_000]; // forces multiple blocks
        let stream = zlib_stored(&raw);
        let mut pos = 2;
        let mut total = 0usize;
        let mut blocks = 0;
        loop {
            let final_block = stream[pos] & 1 != 0;
            let len = u16::from_le_bytes([stream[pos + 1], stream[pos + 2]]);
            let nlen = u16::from_le_bytes([stream[pos + 3], stream[pos + 4]]);
            assert_eq!(len, !nlen, "LEN and NLEN must be complements");
            total += len as usize;
            blocks += 1;
            pos += 5 + len as usize;
            if final_block {
                break;
            }
        }
        assert_eq!(total, raw.len());
        assert!(blocks > 1, "expected the data to span several blocks");
        assert_eq!(
            pos + 4,
            stream.len(),
            "adler32 should follow the last block"
        );
    }

    #[test]
    fn decode_round_trips_our_own_output() {
        let img = test_image();
        let decoded = decode(&encode(&img)).unwrap();
        assert_eq!(decoded.width, img.width);
        assert_eq!(decoded.height, img.height);
        assert_eq!(decoded.data, img.data);
    }

    #[test]
    fn decode_round_trips_a_large_multi_block_image() {
        // Forces the deflate stream past the 65535-byte block boundary.
        let mut img = RgbImage::new(200, 200);
        for y in 0..200 {
            for x in 0..200 {
                img.set(
                    x,
                    y,
                    [(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8],
                );
            }
        }
        let decoded = decode(&encode(&img)).unwrap();
        assert_eq!(decoded.data, img.data);
    }

    #[test]
    fn decode_rejects_non_png() {
        assert!(decode(b"").is_err());
        assert!(decode(b"P6\n1 1\n255\n").is_err());
    }

    #[test]
    fn decode_reports_compressed_data_helpfully() {
        // Forge a PNG whose IDAT claims a fixed-Huffman block.
        let img = test_image();
        let mut png = encode(&img);
        // Locate IDAT and flip the block type of the first deflate block.
        let mut pos = 8;
        while pos + 8 <= png.len() {
            let len =
                u32::from_be_bytes([png[pos], png[pos + 1], png[pos + 2], png[pos + 3]]) as usize;
            if &png[pos + 4..pos + 8] == b"IDAT" {
                png[pos + 8 + 2] |= 0b010; // BTYPE = 01, fixed Huffman
                break;
            }
            pos += 12 + len;
        }
        let err = decode(&png).unwrap_err();
        assert!(
            err.contains("convert"),
            "error should tell the user what to do, got: {err}"
        );
    }

    #[test]
    fn decode_rejects_unsupported_bit_depth() {
        let img = test_image();
        let mut png = encode(&img);
        png[24] = 16; // IHDR bit depth
        let err = decode(&png).unwrap_err();
        assert!(err.contains("8-bit"), "got: {err}");
    }

    #[test]
    fn decode_handles_every_row_filter() {
        // Hand-build a 2x2 RGB image whose rows use filters 1 and 4, wrapped in
        // stored deflate blocks, to prove the unfilter step is real.
        let width = 2usize;
        let height = 2usize;
        let stride = width * 3;

        // Row 0, filter 1 (Sub): first pixel literal, second is a delta.
        // Row 1, filter 4 (Paeth).
        let mut raw = Vec::new();
        raw.push(1u8);
        raw.extend_from_slice(&[10, 20, 30, 5, 5, 5]); // -> 10,20,30 then 15,25,35
        raw.push(4u8);
        raw.extend_from_slice(&[1, 1, 1, 0, 0, 0]); // Paeth against row 0

        let mut png = Vec::new();
        png.extend_from_slice(&SIGNATURE);
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&(width as u32).to_be_bytes());
        ihdr.extend_from_slice(&(height as u32).to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        write_chunk(&mut png, b"IHDR", &ihdr);
        write_chunk(&mut png, b"IDAT", &zlib_stored(&raw));
        write_chunk(&mut png, b"IEND", &[]);

        let img = decode(&png).unwrap();
        assert_eq!(img.get(0, 0), [10, 20, 30]);
        assert_eq!(img.get(1, 0), [15, 25, 35]);
        // Row 1 Paeth with a=0 (left edge), b=row0, c=0 predicts b.
        assert_eq!(img.get(0, 1), [11, 21, 31]);
        let _ = stride;
    }

    #[test]
    fn empty_image_still_produces_a_valid_file() {
        let png = encode(&RgbImage::new(0, 0));
        assert_eq!(&png[..8], &SIGNATURE);
        assert!(png.len() > 8);
    }

    #[test]
    fn round_trips_pixel_data_through_the_stored_stream() {
        // Decode our own IDAT back and confirm the scanlines survive.
        let img = test_image();
        let png = encode(&img);

        // Find IDAT.
        let mut pos = 8;
        let mut idat = Vec::new();
        while pos + 8 <= png.len() {
            let len =
                u32::from_be_bytes([png[pos], png[pos + 1], png[pos + 2], png[pos + 3]]) as usize;
            let kind = &png[pos + 4..pos + 8];
            if kind == b"IDAT" {
                idat = png[pos + 8..pos + 8 + len].to_vec();
                break;
            }
            pos += 12 + len;
        }
        assert!(!idat.is_empty());

        // Walk the stored blocks and concatenate.
        let mut raw = Vec::new();
        let mut p = 2;
        loop {
            let final_block = idat[p] & 1 != 0;
            let len = u16::from_le_bytes([idat[p + 1], idat[p + 2]]) as usize;
            raw.extend_from_slice(&idat[p + 5..p + 5 + len]);
            p += 5 + len;
            if final_block {
                break;
            }
        }

        let stride = img.width * 3;
        for y in 0..img.height {
            let start = y * (stride + 1);
            assert_eq!(raw[start], 0, "filter byte");
            assert_eq!(
                &raw[start + 1..start + 1 + stride],
                &img.data[y * stride..(y + 1) * stride],
                "scanline {y}"
            );
        }
    }
}
