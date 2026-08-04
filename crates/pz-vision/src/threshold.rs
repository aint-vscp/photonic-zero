//! Turning a camera frame into a clean black-and-white image.
//!
//! A single global threshold fails immediately on screen captures: a lamp
//! reflected in one corner, a vignette from a cheap lens, or an LCD whose
//! backlight is brighter in the middle all shift the "correct" cutoff across
//! the frame. Every one of those makes part of the image read as uniformly
//! light or uniformly dark.
//!
//! The local mean threshold here compares each pixel against the average of a
//! window around it, computed in constant time per pixel from a summed-area
//! table. Slow illumination gradients cancel out because they move the pixel
//! and its neighbourhood together.

use crate::{BinaryImage, GrayImage};
use alloc::vec;
use alloc::vec::Vec;

/// Summed-area table for constant-time rectangle sums.
struct Integral {
    w: usize,
    h: usize,
    /// `(w + 1) * (h + 1)` entries so the origin row and column are zero.
    sums: Vec<u64>,
}

impl Integral {
    fn new(img: &GrayImage) -> Self {
        let (w, h) = (img.width, img.height);
        let mut sums = vec![0u64; (w + 1) * (h + 1)];
        for y in 0..h {
            let mut row_acc = 0u64;
            for x in 0..w {
                row_acc += img.data[y * w + x] as u64;
                sums[(y + 1) * (w + 1) + (x + 1)] = sums[y * (w + 1) + (x + 1)] + row_acc;
            }
        }
        Self { w, h, sums }
    }

    /// Sum over the inclusive rectangle, clamped to the image bounds.
    fn rect(&self, x0: usize, y0: usize, x1: usize, y1: usize) -> (u64, u64) {
        let x1 = x1.min(self.w.saturating_sub(1));
        let y1 = y1.min(self.h.saturating_sub(1));
        let stride = self.w + 1;
        let a = self.sums[y0 * stride + x0];
        let b = self.sums[y0 * stride + (x1 + 1)];
        let c = self.sums[(y1 + 1) * stride + x0];
        let d = self.sums[(y1 + 1) * stride + (x1 + 1)];
        let sum = d + a - b - c;
        let count = ((x1 + 1 - x0) * (y1 + 1 - y0)) as u64;
        (sum, count)
    }
}

/// Binarise using a local mean over a `window` by `window` neighbourhood.
///
/// A pixel becomes dark when its value is below the local mean minus `bias`.
/// The bias suppresses speckle in flat regions, where noise alone would
/// otherwise push half the pixels to each side of the mean.
///
/// `window` is clamped to at least 3 and forced odd.
#[must_use]
pub fn adaptive_threshold(img: &GrayImage, window: usize, bias: i32) -> BinaryImage {
    let (w, h) = (img.width, img.height);
    let mut out = BinaryImage::new(w, h);
    if w == 0 || h == 0 {
        return out;
    }

    let win = window.max(3) | 1;
    let radius = win / 2;
    let integral = Integral::new(img);

    for y in 0..h {
        let y0 = y.saturating_sub(radius);
        let y1 = y + radius;
        for x in 0..w {
            let x0 = x.saturating_sub(radius);
            let x1 = x + radius;
            let (sum, count) = integral.rect(x0, y0, x1, y1);
            let mean = (sum / count) as i32;
            let value = img.data[y * w + x] as i32;
            out.set(x, y, value < mean - bias);
        }
    }
    out
}

/// Pick a sensible window size for an image: roughly one eighth of the shorter
/// side, which comfortably spans several PZ cells at any realistic capture
/// distance.
#[must_use]
pub fn default_window(width: usize, height: usize) -> usize {
    let short = width.min(height);
    (short / 8).clamp(15, 199) | 1
}

/// Otsu's method: the global threshold that minimises intra-class variance.
///
/// Kept as a fallback for synthetic or already-clean images, where the local
/// mean can over-fit to noise in perfectly flat regions.
#[must_use]
pub fn otsu_threshold(img: &GrayImage) -> u8 {
    let mut histogram = [0u64; 256];
    for &p in &img.data {
        histogram[p as usize] += 1;
    }
    let total: u64 = img.data.len() as u64;
    if total == 0 {
        return 128;
    }

    let sum_all: u64 = histogram
        .iter()
        .enumerate()
        .map(|(i, &c)| i as u64 * c)
        .sum();

    let mut sum_bg = 0u64;
    let mut weight_bg = 0u64;
    let mut best_variance = -1.0f64;
    let mut best = 128u8;

    for (t, &count) in histogram.iter().enumerate() {
        weight_bg += count;
        if weight_bg == 0 {
            continue;
        }
        let weight_fg = total - weight_bg;
        if weight_fg == 0 {
            break;
        }
        sum_bg += t as u64 * count;

        let mean_bg = sum_bg as f64 / weight_bg as f64;
        let mean_fg = (sum_all - sum_bg) as f64 / weight_fg as f64;
        let delta = mean_bg - mean_fg;
        let variance = weight_bg as f64 * weight_fg as f64 * delta * delta;

        if variance > best_variance {
            best_variance = variance;
            best = t as u8;
        }
    }
    best
}

/// Binarise against a single global threshold.
#[must_use]
pub fn global_threshold(img: &GrayImage, threshold: u8) -> BinaryImage {
    let mut out = BinaryImage::new(img.width, img.height);
    for y in 0..img.height {
        for x in 0..img.width {
            out.set(x, y, img.data[y * img.width + x] <= threshold);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient_image(w: usize, h: usize) -> GrayImage {
        // A left-to-right illumination ramp with a checkerboard on top: any
        // global threshold must fail, a local one must not.
        let mut img = GrayImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let ramp = (x * 200 / w) as i32; // 0..200 background
                let checker = if (x / 4 + y / 4) % 2 == 0 { 30 } else { -30 };
                img.data[y * w + x] = (ramp + checker).clamp(0, 255) as u8;
            }
        }
        img
    }

    #[test]
    fn local_threshold_survives_an_illumination_gradient() {
        let img = gradient_image(128, 64);
        let bin = adaptive_threshold(&img, 15, 2);

        // The checkerboard should be recovered across the whole width, not
        // just where the ramp happens to straddle the mean.
        for band in 0..4 {
            let x = 16 + band * 28;
            let dark = bin.get(x, 8);
            let light = bin.get(x + 4, 8);
            assert_ne!(dark, light, "checker lost in band {band} at x={x}");
        }
    }

    #[test]
    fn global_threshold_would_have_failed_that_image() {
        // Demonstrates why the local method is needed at all.
        let img = gradient_image(128, 64);
        let t = otsu_threshold(&img);
        let bin = global_threshold(&img, t);
        let mut lost = 0;
        for band in 0..4 {
            let x = 16 + band * 28;
            if bin.get(x, 8) == bin.get(x + 4, 8) {
                lost += 1;
            }
        }
        assert!(
            lost > 0,
            "the gradient image was meant to defeat a global cut"
        );
    }

    #[test]
    fn otsu_splits_a_bimodal_image() {
        let mut img = GrayImage::new(64, 64);
        for (i, p) in img.data.iter_mut().enumerate() {
            *p = if i % 2 == 0 { 20 } else { 230 };
        }
        let t = otsu_threshold(&img);
        assert!(
            (20..230).contains(&t),
            "threshold {t} did not separate modes"
        );
    }

    #[test]
    fn empty_image_is_handled() {
        let img = GrayImage::new(0, 0);
        let bin = adaptive_threshold(&img, 15, 2);
        assert_eq!(bin.width, 0);
        assert_eq!(otsu_threshold(&img), 128);
    }

    #[test]
    fn window_size_is_odd_and_bounded() {
        assert_eq!(default_window(1920, 1080) % 2, 1);
        assert!(default_window(64, 64) >= 15);
        assert!(default_window(8000, 8000) <= 199);
    }

    #[test]
    fn uniform_image_produces_no_dark_pixels() {
        let mut img = GrayImage::new(32, 32);
        img.data.fill(128);
        let bin = adaptive_threshold(&img, 15, 5);
        assert!(
            (0..32).all(|y| (0..32).all(|x| !bin.get(x, y))),
            "flat grey must not produce speckle"
        );
    }
}
