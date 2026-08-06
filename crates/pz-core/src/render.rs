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
    /// Colour substituted for black, or `None` to leave frames as generated.
    ///
    /// Every cell that would be drawn pure black is drawn in this colour
    /// instead: the data cells of a [`ColorMode::Mono`] frame, the four corner
    /// markers, and the black calibration patch. Because the decoder derives
    /// its thresholds from that patch rather than assuming black, recolouring
    /// stays decodable without telling the receiver anything.
    ///
    /// [`ColorMode::Mono`]: crate::ColorMode
    ///
    /// # Contrast
    /// The ink must stay clearly darker than [`Self::background`]; the
    /// demodulator separates the two levels by how far each cell sits from the
    /// midpoint between them. A deep brand colour is fine, a pastel is not.
    /// [`RenderOptions::contrast_ok`] checks a candidate.
    ///
    /// This only recolours what was already monochrome. In `Rgb4` and `Rgb8`
    /// the colours *are* the data, so the other seven are left alone.
    pub ink: Option<[u8; 3]>,
}

impl Default for RenderOptions {
    /// 8 pixels per cell with a 4-cell white margin.
    fn default() -> Self {
        Self {
            module_px: 8,
            quiet_zone: 4,
            background: [255, 255, 255],
            ink: None,
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
            ink: None,
        }
    }

    /// Pixel size of a frame rendered with these options.
    #[must_use]
    pub fn output_size(&self, modules: usize) -> usize {
        (modules + self.quiet_zone * 2) * self.module_px
    }

    /// Whether [`Self::ink`] separates from [`Self::background`] well enough to
    /// demodulate.
    ///
    /// Uses relative luminance, because the demodulator thresholds on
    /// brightness rather than hue: a saturated red and a saturated green look
    /// far apart to a person and nearly identical to the decision rule. The
    /// 0.45 floor is deliberately conservative, since a camera will lose
    /// contrast that a rendered PNG still has.
    #[must_use]
    pub fn contrast_ok(&self) -> bool {
        let Some(ink) = self.ink else { return true };
        luminance(self.background) - luminance(ink) >= 0.45
    }
}

/// Relative luminance in `[0, 1]`, Rec. 601 weights.
fn luminance(rgb: [u8; 3]) -> f64 {
    let [r, g, b] = rgb;
    (0.299 * f64::from(r) + 0.587 * f64::from(g) + 0.114 * f64::from(b)) / 255.0
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
            let mut rgb = frame.color_at(row, col);
            // Only pure black is substituted. That is exactly the set of cells
            // carrying no colour information: mono data, the corner markers,
            // and the black reference patch the decoder calibrates against.
            if let Some(ink) = options.ink {
                if rgb == [0, 0, 0] {
                    rgb = ink;
                }
            }
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

    /// A recoloured mono frame must still decode.
    ///
    /// This is the whole justification for `ink`: the decoder calibrates from
    /// the frame's own reference patches, so it never assumed the dark level
    /// was black in the first place.
    #[test]
    fn a_recoloured_mono_frame_still_decodes() {
        use crate::decoder::Decoder;
        use crate::{Progress, RgbView};

        let payload = b"ink is a rendering concern, not a protocol one";
        let encoder = Encoder::new(payload, EncoderConfig::mono()).unwrap();

        // A deep indigo. Dark enough to demodulate, nothing like black.
        let ink = [26, 22, 84];
        let opts = RenderOptions {
            module_px: 6,
            quiet_zone: 4,
            background: [255, 255, 255],
            ink: Some(ink),
        };
        assert!(opts.contrast_ok(), "indigo on white should pass the check");

        let frame = encoder.frame(0).unwrap();
        let img = render(&frame, &opts);

        // The ink really is on the page, and pure black really is not.
        let inside = opts.quiet_zone * opts.module_px;
        assert_eq!(img.get(inside, inside), ink, "finder should be inked");
        assert!(
            !img.data.chunks_exact(3).any(|px| px == [0, 0, 0]),
            "no pure black should survive recolouring",
        );

        let mut decoder = Decoder::new();
        let mut done = None;
        for index in 0..400u32 {
            let image = render(&encoder.frame(index).unwrap(), &opts);
            let view = RgbView::rgb(image.width, image.height, &image.data).unwrap();
            if let Progress::Complete(bytes) = decoder.ingest_image(&view).unwrap() {
                done = Some(bytes);
                break;
            }
        }
        assert_eq!(
            done.as_deref(),
            Some(&payload[..]),
            "recoloured transfer failed"
        );
    }

    #[test]
    fn contrast_check_rejects_ink_too_close_to_the_background() {
        let pale = RenderOptions {
            ink: Some([210, 200, 180]),
            ..RenderOptions::default()
        };
        assert!(!pale.contrast_ok(), "a pastel must be rejected");

        let deep = RenderOptions {
            ink: Some([12, 40, 30]),
            ..RenderOptions::default()
        };
        assert!(deep.contrast_ok(), "a deep colour must be accepted");

        assert!(
            RenderOptions::default().contrast_ok(),
            "no ink is always fine"
        );
    }

    #[test]
    fn output_size_matches_the_options() {
        let e = Encoder::new(b"render me", EncoderConfig::default()).unwrap();
        let frame = e.frame(0).unwrap();
        let opts = RenderOptions {
            module_px: 6,
            quiet_zone: 3,
            background: [255, 255, 255],
            ink: None,
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
            ink: None,
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
            ink: None,
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
