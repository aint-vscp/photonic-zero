//! Prints what the detector sees for a synthetically warped frame.
//!
//! Run with: cargo run -p pz-core --example diagnose

use pz_core::decoder::{decode_sampled, detect, sample_frame};
use pz_core::render::{render, RenderOptions, RgbImage};
use pz_core::{Encoder, EncoderConfig, RgbView};
use pz_vision::{
    adaptive_threshold, default_window, find_finder_patterns, order_finders, Homography, Point,
};

fn photograph(source: &RgbImage, size: usize, skew: [(f64, f64); 4]) -> RgbImage {
    let mut out = RgbImage::new(size, size);
    let margin = size as f64 * 0.10;
    let base = [
        (margin, margin),
        (size as f64 - margin, margin),
        (size as f64 - margin, size as f64 - margin),
        (margin, size as f64 - margin),
    ];
    let quad: Vec<Point> = base
        .iter()
        .zip(skew.iter())
        .map(|(&(x, y), &(dx, dy))| Point::new(x + dx * size as f64, y + dy * size as f64))
        .collect();
    let dest: [Point; 4] = [quad[0], quad[1], quad[2], quad[3]];
    let sw = source.width as f64;
    let sh = source.height as f64;
    let src = [
        Point::new(0.0, 0.0),
        Point::new(sw, 0.0),
        Point::new(sw, sh),
        Point::new(0.0, sh),
    ];
    let inverse = Homography::from_correspondences(&dest, &src).unwrap();
    for y in 0..size {
        for x in 0..size {
            let p = inverse.apply(Point::new(x as f64 + 0.5, y as f64 + 0.5));
            let rgb = if p.x < 0.0 || p.y < 0.0 || p.x >= sw || p.y >= sh {
                [150, 150, 150]
            } else {
                source.get(p.x as usize, p.y as usize)
            };
            out.set(x, y, rgb);
        }
    }
    out
}

fn main() {
    let encoder = Encoder::new(b"diagnostic payload", EncoderConfig::default()).unwrap();
    let frame = encoder.frame(0).unwrap();
    let rendered = render(&frame, &RenderOptions::default());

    for (name, skew) in [
        ("flat", [(0.0, 0.0); 4]),
        (
            "mild",
            [(0.02, 0.01), (-0.03, 0.01), (0.01, -0.02), (-0.015, -0.01)],
        ),
        (
            "strong",
            [(0.055, 0.02), (-0.055, 0.02), (0.02, -0.01), (-0.02, -0.01)],
        ),
    ] {
        let shot = photograph(&rendered, 520, skew);
        let view = RgbView::rgb(shot.width, shot.height, &shot.data).unwrap();

        let gray = view.to_gray();
        let window = default_window(view.width, view.height);
        let binary = adaptive_threshold(&gray, window, 6);
        let patterns = find_finder_patterns(&binary);

        println!("=== {name} ===");
        println!("  finder candidates: {}", patterns.len());
        for p in patterns.iter().take(6) {
            println!(
                "    center ({:.1}, {:.1}) module {:.2}",
                p.center.x, p.center.y, p.module
            );
        }

        if let Some((tl, tr, bl)) = order_finders(&patterns) {
            let predicted = Point::new(
                tr.center.x + bl.center.x - tl.center.x,
                tr.center.y + bl.center.y - tl.center.y,
            );
            println!(
                "  affine BR prediction ({:.1}, {:.1})",
                predicted.x, predicted.y
            );
        }

        let detections = detect(&view);
        println!("  detections: {}", detections.len());
        for d in &detections {
            println!(
                "    grid {:?} module_px {:.2} corner_confirmed {}",
                d.grid, d.module_px, d.corner_confirmed
            );
            let colors = sample_frame(&view, d);
            match decode_sampled(d.grid, &colors, 0.28) {
                Ok(f) => println!("      decoded: frame {}", f.header.frame_index),
                Err(e) => println!("      decode failed: {e}"),
            }
        }
        println!();
    }
}
