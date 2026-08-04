//! Drawing frames as pixels.

use crate::encoder::Frame;
use alloc::vec;
use alloc::vec::Vec;

/// How a frame is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderOptions {
    /// Pixels per cell. Larger is easier for the camera to resolve.
    pub module_px: usize,
    /// Cells of blank margin around the frame.
    ///
    /// The detector needs light space outside the corner markers to measure
    /// their outer edge against. Without a margin, a frame drawn flush to the
    /// edge of a dark window is much harder to find.
    pub quiet_zone: usize,
    /// Colour of the quiet zone.
    pub background: [u8; 3],
}

impl Default for RenderOptions {
    /// 8 pixels per cell with a 4-cell white margin.
    fn default() -> Self {
        Self {
            module_px: 8,
            quiet_zone: 4,
            background: [255, 255, 255],
        }
    }
}

impl RenderOptions {
    /// Options that fill roughly `target_px` pixels on the longest side.
    #[must_use]
    pub fn to_fit(modules: usize, target_px: usize) -> Self {
        let quiet_zone = 4;
        let total_cells = modules + quiet_zone * 2;
        let module_px = (target_px / total_cells.max(1)).max(1);
        Self {
            module_px,
            quiet_zone,
            background: [255, 255, 255],
        }
    }

    /// Pixel size of a frame rendered with these options.
    #[must_use]
    pub fn output_size(&self, modules: usize) -> usize {
        (modules + self.quiet_zone * 2) * self.module_px
    }
}

/// An 8-bit RGB image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbImage {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// Row-major RGB triples.
    pub data: Vec<u8>,
}

impl RgbImage {
    /// Allocate a black image.
    #[must_use]
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            data: vec![0u8; width * height * 3],
        }
    }

    /// Read a pixel.
    #[must_use]
    pub fn get(&self, x: usize, y: usize) -> [u8; 3] {
        let i = (y * self.width + x) * 3;
        [self.data[i], self.data[i + 1], self.data[i + 2]]
    }

    /// Write a pixel.
    pub fn set(&mut self, x: usize, y: usize, rgb: [u8; 3]) {
        if x >= self.width || y >= self.height {
            return;
        }
        let i = (y * self.width + x) * 3;
        self.data[i] = rgb[0];
        self.data[i + 1] = rgb[1];
        self.data[i + 2] = rgb[2];
    }

    /// Convert to RGBA, as browser `ImageData` and most GPU paths expect.
    #[must_use]
    pub fn to_rgba(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.width * self.height * 4);
        for px in self.data.chunks_exact(3) {
            out.extend_from_slice(px);
            out.push(255);
        }
        out
    }
}

/// Draw a frame.
#[must_use]
pub fn render(frame: &Frame, options: &RenderOptions) -> RgbImage {
    let n = frame.modules();
    let scale = options.module_px.max(1);
    let quiet = options.quiet_zone;
    let size = (n + quiet * 2) * scale;

    let mut img = RgbImage::new(size, size);
    // Quiet zone first, then paint cells over it.
    for chunk in img.data.chunks_exact_mut(3) {
        chunk.copy_from_slice(&options.background);
    }

    for row in 0..n {
        for col in 0..n {
            let rgb = frame.color_at(row, col);
            let x0 = (col + quiet) * scale;
            let y0 = (row + quiet) * scale;
            for dy in 0..scale {
                for dx in 0..scale {
                    img.set(x0 + dx, y0 + dy, rgb);
                }
            }
        }
    }
    img
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoder::{Encoder, EncoderConfig};
    use crate::layout::BLACK;

    #[test]
    fn output_size_matches_the_options() {
        let e = Encoder::new(b"render me", EncoderConfig::default()).unwrap();
        let frame = e.frame(0).unwrap();
        let opts = RenderOptions {
            module_px: 6,
            quiet_zone: 3,
            background: [255, 255, 255],
        };
        let img = render(&frame, &opts);
        let expected = (49 + 6) * 6;
        assert_eq!(img.width, expected);
        assert_eq!(img.height, expected);
        assert_eq!(opts.output_size(49), expected);
        assert_eq!(img.data.len(), expected * expected * 3);
    }

    #[test]
    fn quiet_zone_is_background_coloured() {
        let e = Encoder::new(b"quiet", EncoderConfig::default()).unwrap();
        let frame = e.frame(0).unwrap();
        let opts = RenderOptions {
            module_px: 4,
            quiet_zone: 2,
            background: [255, 255, 255],
        };
        let img = render(&frame, &opts);
        assert_eq!(img.get(0, 0), [255, 255, 255]);
        assert_eq!(img.get(img.width - 1, img.height - 1), [255, 255, 255]);
        // Just inside the quiet zone is the top-left finder, which is dark.
        let inside = opts.quiet_zone * opts.module_px;
        assert_eq!(img.get(inside, inside), [0, 0, 0]);
    }

    #[test]
    fn cells_are_drawn_as_solid_blocks() {
        let e = Encoder::new(b"blocks", EncoderConfig::default()).unwrap();
        let frame = e.frame(0).unwrap();
        let opts = RenderOptions {
            module_px: 5,
            quiet_zone: 1,
            background: [255, 255, 255],
        };
        let img = render(&frame, &opts);

        // Every pixel of cell (3,3) - the finder centre - must be black.
        let x0 = (3 + 1) * 5;
        let y0 = (3 + 1) * 5;
        assert_eq!(frame.code_at(3, 3), BLACK);
        for dy in 0..5 {
            for dx in 0..5 {
                assert_eq!(img.get(x0 + dx, y0 + dy), [0, 0, 0], "at {dx},{dy}");
            }
        }
    }

    #[test]
    fn to_fit_lands_near_the_target() {
        let opts = RenderOptions::to_fit(49, 800);
        let size = opts.output_size(49);
        assert!(size <= 800, "overshot: {size}");
        assert!(size > 700, "undershot badly: {size}");
    }

    #[test]
    fn to_fit_never_produces_a_zero_scale() {
        let opts = RenderOptions::to_fit(97, 10);
        assert!(opts.module_px >= 1);
        assert!(opts.output_size(97) > 0);
    }

    #[test]
    fn rgba_conversion_is_opaque() {
        let img = RgbImage::new(2, 2);
        let rgba = img.to_rgba();
        assert_eq!(rgba.len(), 16);
        assert!(rgba.chunks_exact(4).all(|p| p[3] == 255));
    }

    #[test]
    fn out_of_bounds_writes_are_ignored() {
        let mut img = RgbImage::new(2, 2);
        img.set(99, 99, [1, 2, 3]);
        assert_eq!(img.get(0, 0), [0, 0, 0]);
    }
}
