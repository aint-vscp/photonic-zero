//! Camera-side computer vision for the Photonic Zero (PZ) optical protocol.
//!
//! This crate turns a photograph of a screen into grid cell colours. It is the
//! part of PZ that deals with physical reality: perspective, blur, glare,
//! rolling shutter, and a camera that has no idea where the code is.
//!
//! The pipeline is deliberately small and dependency-free:
//!
//! 1. [`GrayImage`] from the captured RGB, then [`threshold::adaptive_threshold`]
//!    to a [`BinaryImage`] that survives uneven lighting.
//! 2. [`finder::find_finder_patterns`] locates the three 7x7 corner markers by
//!    their scale-invariant 1:1:3:1:1 run signature.
//! 3. [`finder::find_corner_marker`] confirms the fourth corner near where the
//!    first three predict it to be.
//! 4. [`geom::Homography::from_correspondences`] solves for the projective
//!    transform from grid coordinates to image coordinates.
//! 5. [`sample_cell`] reads each cell back through that transform, averaging a
//!    small patch so a single hot pixel cannot flip a bit.
//!
//! Everything above works in *grid coordinates*: cell `(col, row)` has its
//! centre at `(col + 0.5, row + 0.5)`. The caller (`pz-core`) owns the layout
//! and tells this crate which grid points the markers correspond to.
//!
//! # `no_std`
//!
//! Disable the `std` feature to build against `core` + `alloc` only.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

#[cfg(feature = "std")]
extern crate std;

pub mod finder;
pub mod geom;
pub mod threshold;

use alloc::vec;
use alloc::vec::Vec;

pub use finder::{find_corner_marker, find_finder_patterns, order_finders, FinderPattern};
pub use geom::{order_corners, order_quad, signed_area, Homography, Point};
pub use threshold::{adaptive_threshold, default_window, global_threshold, otsu_threshold};

/// Square root without the platform math library, so this crate stays
/// dependency-free and `no_std`.
///
/// Unlike the copy in `pz-fountain`, nothing on the wire depends on this
/// value, so it only needs to be accurate, not bit-reproducible.
pub(crate) fn fmath_sqrt(x: f64) -> f64 {
    if x <= 0.0 || !x.is_finite() {
        return if x.is_nan() || x < 0.0 {
            f64::NAN
        } else {
            x.max(0.0)
        };
    }
    let mut y = f64::from_bits((x.to_bits() >> 1) + 0x1FF8_0000_0000_0000);
    for _ in 0..6 {
        y = 0.5 * (y + x / y);
    }
    y
}

/// An 8-bit grayscale image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrayImage {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// Row-major luminance samples, `width * height` long.
    pub data: Vec<u8>,
}

impl GrayImage {
    /// Allocate a black image.
    #[must_use]
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            data: vec![0u8; width * height],
        }
    }

    /// Read a pixel, returning 0 outside the image.
    #[must_use]
    pub fn get(&self, x: usize, y: usize) -> u8 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        self.data[y * self.width + x]
    }
}

/// A packed one-bit-per-pixel image where `true` means "dark".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryImage {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// One byte per pixel, `0` or `1`. Byte-per-pixel rather than bit-packed
    /// because the finder scan reads neighbours constantly and the branchless
    /// indexing is worth the memory.
    pub data: Vec<u8>,
}

impl BinaryImage {
    /// Allocate an all-light image.
    #[must_use]
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            data: vec![0u8; width * height],
        }
    }

    /// Whether the pixel is dark. Out-of-bounds reads return `false`.
    #[must_use]
    #[inline]
    pub fn get(&self, x: usize, y: usize) -> bool {
        if x >= self.width || y >= self.height {
            return false;
        }
        self.data[y * self.width + x] != 0
    }

    /// Set a pixel. Out-of-bounds writes are ignored.
    #[inline]
    pub fn set(&mut self, x: usize, y: usize, dark: bool) {
        if x < self.width && y < self.height {
            self.data[y * self.width + x] = u8::from(dark);
        }
    }

    /// Count of dark pixels, for diagnostics.
    #[must_use]
    pub fn dark_count(&self) -> usize {
        self.data.iter().filter(|&&v| v != 0).count()
    }
}

/// A borrowed view of an interleaved 8-bit RGB image.
///
/// Borrowing rather than owning means a caller can hand over a camera buffer,
/// a canvas `ImageData`, or a JNI array without copying it first.
#[derive(Debug, Clone, Copy)]
pub struct RgbView<'a> {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// Bytes per row. Equal to `width * channels` for tightly packed buffers.
    pub stride: usize,
    /// Bytes per pixel: 3 for RGB, 4 for RGBA (the alpha byte is ignored).
    pub channels: usize,
    /// The pixel data.
    pub data: &'a [u8],
}

impl<'a> RgbView<'a> {
    /// Wrap a tightly packed RGB buffer.
    ///
    /// # Errors
    /// Returns `None` if the buffer is too small.
    #[must_use]
    pub fn rgb(width: usize, height: usize, data: &'a [u8]) -> Option<Self> {
        if data.len() < width * height * 3 {
            return None;
        }
        Some(Self {
            width,
            height,
            stride: width * 3,
            channels: 3,
            data,
        })
    }

    /// Wrap a tightly packed RGBA buffer, such as a browser `ImageData`.
    ///
    /// # Errors
    /// Returns `None` if the buffer is too small.
    #[must_use]
    pub fn rgba(width: usize, height: usize, data: &'a [u8]) -> Option<Self> {
        if data.len() < width * height * 4 {
            return None;
        }
        Some(Self {
            width,
            height,
            stride: width * 4,
            channels: 4,
            data,
        })
    }

    /// Read a pixel, clamping coordinates to the image edge.
    #[must_use]
    #[inline]
    pub fn get(&self, x: usize, y: usize) -> [u8; 3] {
        let x = x.min(self.width.saturating_sub(1));
        let y = y.min(self.height.saturating_sub(1));
        let i = y * self.stride + x * self.channels;
        if i + 2 >= self.data.len() {
            return [0, 0, 0];
        }
        [self.data[i], self.data[i + 1], self.data[i + 2]]
    }

    /// Bilinear sample at fractional coordinates.
    #[must_use]
    pub fn sample_bilinear(&self, x: f64, y: f64) -> [u8; 3] {
        if self.width == 0 || self.height == 0 {
            return [0, 0, 0];
        }
        let x = x.clamp(0.0, (self.width - 1) as f64);
        let y = y.clamp(0.0, (self.height - 1) as f64);
        let x0 = x as usize;
        let y0 = y as usize;
        let x1 = (x0 + 1).min(self.width - 1);
        let y1 = (y0 + 1).min(self.height - 1);
        let fx = x - x0 as f64;
        let fy = y - y0 as f64;

        let p00 = self.get(x0, y0);
        let p10 = self.get(x1, y0);
        let p01 = self.get(x0, y1);
        let p11 = self.get(x1, y1);

        let mut out = [0u8; 3];
        for c in 0..3 {
            let top = p00[c] as f64 * (1.0 - fx) + p10[c] as f64 * fx;
            let bottom = p01[c] as f64 * (1.0 - fx) + p11[c] as f64 * fx;
            // `f64::round` lives in std. Adding a half before the clamp gives
            // the same result here because the interpolated value is a
            // weighted average of two bytes and so is never negative, and a
            // float-to-int cast truncates.
            out[c] = (top * (1.0 - fy) + bottom * fy + 0.5).clamp(0.0, 255.0) as u8;
        }
        out
    }

    /// Convert to grayscale using the ITU-R BT.601 luma weights.
    #[must_use]
    pub fn to_gray(&self) -> GrayImage {
        let mut g = GrayImage::new(self.width, self.height);
        for y in 0..self.height {
            for x in 0..self.width {
                let [r, gg, b] = self.get(x, y);
                // (77 r + 150 g + 29 b) / 256
                let luma = (77 * r as u32 + 150 * gg as u32 + 29 * b as u32) >> 8;
                g.data[y * self.width + x] = luma as u8;
            }
        }
        g
    }
}

/// Average the colour over one grid cell.
///
/// `oversample` sub-samples are taken per axis across the middle
/// `2 * half_extent` of the cell. Sampling a patch rather than a single point
/// is what makes the decoder tolerant of sensor noise, JPEG ringing and the
/// slight misalignment left over after the homography fit; keeping away from
/// the cell edges is what stops a neighbouring cell bleeding in.
#[must_use]
pub fn sample_cell(
    img: &RgbView<'_>,
    h: &Homography,
    gx: f64,
    gy: f64,
    half_extent: f64,
    oversample: usize,
) -> [u8; 3] {
    let n = oversample.max(1);
    let mut acc = [0u32; 3];
    let mut count = 0u32;

    for iy in 0..n {
        for ix in 0..n {
            let (ox, oy) = if n == 1 {
                (0.0, 0.0)
            } else {
                let step = 2.0 * half_extent / (n - 1) as f64;
                (
                    -half_extent + ix as f64 * step,
                    -half_extent + iy as f64 * step,
                )
            };
            let p = h.apply(Point::new(gx + ox, gy + oy));
            if !p.x.is_finite() || !p.y.is_finite() {
                continue;
            }
            let c = img.sample_bilinear(p.x, p.y);
            acc[0] += c[0] as u32;
            acc[1] += c[1] as u32;
            acc[2] += c[2] as u32;
            count += 1;
        }
    }

    if count == 0 {
        return [0, 0, 0];
    }
    [
        (acc[0] / count) as u8,
        (acc[1] / count) as u8,
        (acc[2] / count) as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqrt_helper_is_accurate() {
        for x in [0.0f64, 1.0, 2.0, 9.0, 1e6, 1e-6] {
            let got = fmath_sqrt(x);
            let want = x.sqrt();
            assert!((got - want).abs() <= 1e-9 * (1.0 + want), "sqrt({x})");
        }
        assert!(fmath_sqrt(-1.0).is_nan());
    }

    #[test]
    fn rgb_view_rejects_short_buffers() {
        let data = [0u8; 10];
        assert!(RgbView::rgb(4, 4, &data).is_none());
        assert!(RgbView::rgba(2, 2, &data).is_none());
        let ok = [0u8; 48];
        assert!(RgbView::rgb(4, 4, &ok).is_some());
    }

    #[test]
    fn rgba_view_skips_the_alpha_channel() {
        let mut data = vec![0u8; 2 * 2 * 4];
        // Pixel (1,0) = red, fully transparent.
        data[4] = 255;
        data[7] = 0;
        let view = RgbView::rgba(2, 2, &data).unwrap();
        assert_eq!(view.get(1, 0), [255, 0, 0]);
    }

    #[test]
    fn bilinear_interpolates_between_pixels() {
        // Two pixels: black then white.
        let data = [0u8, 0, 0, 255, 255, 255];
        let view = RgbView::rgb(2, 1, &data).unwrap();
        assert_eq!(view.sample_bilinear(0.0, 0.0), [0, 0, 0]);
        assert_eq!(view.sample_bilinear(1.0, 0.0), [255, 255, 255]);
        let mid = view.sample_bilinear(0.5, 0.0);
        assert!((mid[0] as i32 - 128).abs() <= 1, "midpoint was {mid:?}");
    }

    #[test]
    fn gray_conversion_weights_green_most() {
        let data = [255u8, 0, 0, 0, 255, 0, 0, 0, 255];
        let view = RgbView::rgb(3, 1, &data).unwrap();
        let g = view.to_gray();
        assert!(g.data[1] > g.data[0], "green should be brighter than red");
        assert!(g.data[0] > g.data[2], "red should be brighter than blue");
    }

    #[test]
    fn sample_cell_averages_a_patch() {
        // 4x4 image, left half black, right half white.
        let mut data = vec![0u8; 4 * 4 * 3];
        for y in 0..4 {
            for x in 2..4 {
                let i = (y * 4 + x) * 3;
                data[i] = 255;
                data[i + 1] = 255;
                data[i + 2] = 255;
            }
        }
        let view = RgbView::rgb(4, 4, &data).unwrap();
        let h = Homography::identity();

        let left = sample_cell(&view, &h, 0.5, 1.5, 0.3, 3);
        let right = sample_cell(&view, &h, 3.0, 1.5, 0.3, 3);
        assert!(left[0] < 60, "left sample {left:?} should be dark");
        assert!(right[0] > 200, "right sample {right:?} should be light");
    }

    #[test]
    fn binary_image_bounds_are_safe() {
        let mut b = BinaryImage::new(4, 4);
        b.set(10, 10, true); // ignored
        assert!(!b.get(10, 10));
        b.set(1, 1, true);
        assert!(b.get(1, 1));
        assert_eq!(b.dark_count(), 1);
    }
}
