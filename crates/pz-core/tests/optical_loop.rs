//! End-to-end tests through a simulated camera.
//!
//! Every other test in this repository decodes cell values that were handed
//! over directly. These decode *pixels*, through the real computer vision
//! path, after putting the frame through the indignities a phone camera
//! inflicts on a screen: perspective, defocus, an auto-exposure that crushes
//! the range, a warm white balance, and sensor noise.
//!
//! This is the test that would catch a protocol that only works on paper.

use pz_core::render::{render, RenderOptions, RgbImage};
use pz_core::{Decoder, Encoder, EncoderConfig, GridSize, Progress, RgbView};
use pz_vision::{Homography, Point};

/// A deterministic noise source, so a failure is always reproducible.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Signed noise in `[-amount, amount]`.
    fn noise(&mut self, amount: i32) -> i32 {
        if amount == 0 {
            return 0;
        }
        (self.next() % (2 * amount as u64 + 1)) as i32 - amount
    }
}

/// How the simulated camera mistreats the frame.
#[derive(Debug, Clone, Copy)]
struct Camera {
    /// Output image size in pixels.
    out: usize,
    /// Corner offsets as a fraction of the output size, applied to the
    /// screen's four corners to produce a perspective view. All zero means the
    /// camera is dead-on.
    skew: [(f64, f64); 4],
    /// Number of 3x3 box blur passes. Two is a noticeably soft focus.
    blur: usize,
    /// Per-channel multiplicative gain, simulating white balance.
    gain: [f64; 3],
    /// Per-channel additive offset, simulating lifted blacks.
    bias: [f64; 3],
    /// Peak absolute sensor noise per channel.
    noise: i32,
    /// Seed for the noise generator.
    seed: u64,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            out: 520,
            skew: [(0.0, 0.0); 4],
            blur: 0,
            gain: [1.0, 1.0, 1.0],
            bias: [0.0, 0.0, 0.0],
            noise: 0,
            seed: 0x5EED,
        }
    }
}

fn box_blur(img: &RgbImage) -> RgbImage {
    let mut out = RgbImage::new(img.width, img.height);
    for y in 0..img.height {
        for x in 0..img.width {
            let mut acc = [0u32; 3];
            let mut count = 0u32;
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let nx = x as i32 + dx;
                    let ny = y as i32 + dy;
                    if nx < 0 || ny < 0 || nx >= img.width as i32 || ny >= img.height as i32 {
                        continue;
                    }
                    let p = img.get(nx as usize, ny as usize);
                    for c in 0..3 {
                        acc[c] += p[c] as u32;
                    }
                    count += 1;
                }
            }
            out.set(
                x,
                y,
                [
                    (acc[0] / count) as u8,
                    (acc[1] / count) as u8,
                    (acc[2] / count) as u8,
                ],
            );
        }
    }
    out
}

/// Photograph a rendered frame.
fn photograph(source: &RgbImage, cam: &Camera) -> RgbImage {
    let size = cam.out;
    let mut out = RgbImage::new(size, size);

    // The screen occupies the middle 80% of the shot, sitting on a grey desk.
    let margin = size as f64 * 0.10;
    let base = [
        (margin, margin),
        (size as f64 - margin, margin),
        (size as f64 - margin, size as f64 - margin),
        (margin, size as f64 - margin),
    ];
    let quad: Vec<Point> = base
        .iter()
        .zip(cam.skew.iter())
        .map(|(&(x, y), &(dx, dy))| Point::new(x + dx * size as f64, y + dy * size as f64))
        .collect();

    let sw = source.width as f64;
    let sh = source.height as f64;
    let src_rect = [
        Point::new(0.0, 0.0),
        Point::new(sw, 0.0),
        Point::new(sw, sh),
        Point::new(0.0, sh),
    ];
    let dest_quad: [Point; 4] = [quad[0], quad[1], quad[2], quad[3]];

    // Map destination pixels back into the source image.
    let inverse = Homography::from_correspondences(&dest_quad, &src_rect)
        .expect("simulated camera quad is degenerate");

    let mut rng = Rng(cam.seed);
    for y in 0..size {
        for x in 0..size {
            let p = inverse.apply(Point::new(x as f64 + 0.5, y as f64 + 0.5));
            let rgb = if p.x < 0.0 || p.y < 0.0 || p.x >= sw || p.y >= sh {
                [150, 150, 150] // the desk
            } else {
                source.get(p.x as usize, p.y as usize)
            };
            out.set(x, y, rgb);
        }
    }

    for _ in 0..cam.blur {
        out = box_blur(&out);
    }

    // Exposure, white balance and sensor noise.
    for y in 0..size {
        for x in 0..size {
            let p = out.get(x, y);
            let mut q = [0u8; 3];
            for c in 0..3 {
                let v = p[c] as f64 * cam.gain[c] + cam.bias[c] + rng.noise(cam.noise) as f64;
                q[c] = v.clamp(0.0, 255.0) as u8;
            }
            out.set(x, y, q);
        }
    }

    out
}

/// Push one frame through the camera and try to decode it.
fn shoot_and_decode(
    encoder: &Encoder,
    decoder: &mut Decoder,
    index: u32,
    cam: &Camera,
    render_opts: &RenderOptions,
) -> Progress {
    let frame = encoder.frame(index).unwrap();
    let rendered = render(&frame, render_opts);
    let shot = photograph(&rendered, cam);
    let view = RgbView::rgb(shot.width, shot.height, &shot.data).unwrap();
    decoder.ingest_image(&view).unwrap()
}

fn config_for(grid: GridSize) -> EncoderConfig {
    EncoderConfig {
        grid,
        session_id: Some(0x02A0),
        ..EncoderConfig::default()
    }
}

#[test]
fn decodes_a_dead_on_capture() {
    let payload = b"photonic zero over a simulated lens";
    let encoder = Encoder::new(payload, EncoderConfig::default()).unwrap();
    let mut decoder = Decoder::new();
    let opts = RenderOptions {
        module_px: 8,
        quiet_zone: 4,
        background: [255, 255, 255],
        ink: None,
    };

    let progress = shoot_and_decode(&encoder, &mut decoder, 0, &Camera::default(), &opts);
    match progress {
        Progress::Complete(bytes) => assert_eq!(bytes, payload),
        other => panic!("dead-on capture failed to decode: {other:?}"),
    }
}

#[test]
fn decodes_through_perspective() {
    let payload = b"held at an angle, as one does";
    let encoder = Encoder::new(payload, EncoderConfig::default()).unwrap();
    let mut decoder = Decoder::new();
    let opts = RenderOptions::default();

    // Top edge pushed in, bottom edge pushed out: looking up at a screen.
    let cam = Camera {
        skew: [(0.055, 0.02), (-0.055, 0.02), (0.02, -0.01), (-0.02, -0.01)],
        ..Camera::default()
    };

    match shoot_and_decode(&encoder, &mut decoder, 0, &cam, &opts) {
        Progress::Complete(bytes) => assert_eq!(bytes, payload),
        other => panic!("perspective capture failed: {other:?}"),
    }
}

#[test]
fn decodes_through_perspective_blur_and_noise() {
    let payload = b"soft focus, warm light, cheap sensor";
    let encoder = Encoder::new(payload, EncoderConfig::default()).unwrap();
    let mut decoder = Decoder::new();
    let opts = RenderOptions {
        module_px: 10,
        quiet_zone: 4,
        background: [255, 255, 255],
        ink: None,
    };

    let cam = Camera {
        out: 640,
        skew: [
            (0.03, 0.015),
            (-0.04, 0.01),
            (0.015, -0.02),
            (-0.01, -0.015),
        ],
        blur: 2,
        gain: [0.80, 0.74, 0.62], // warm cast, compressed range
        bias: [28.0, 30.0, 40.0], // lifted blacks
        noise: 10,
        ..Camera::default()
    };

    match shoot_and_decode(&encoder, &mut decoder, 0, &cam, &opts) {
        Progress::Complete(bytes) => assert_eq!(bytes, payload),
        other => panic!("degraded capture failed: {other:?}"),
    }
}

#[test]
fn decodes_a_multi_frame_payload_through_the_lens() {
    // A payload that needs many frames, captured through a moving camera that
    // misses some of them entirely.
    let payload: Vec<u8> = (0..4000).map(|i| (i * 37 % 251) as u8).collect();
    let encoder = Encoder::new(&payload, EncoderConfig::default()).unwrap();
    assert!(
        encoder.block_count() > 5,
        "payload should span several frames, got {}",
        encoder.block_count()
    );

    let mut decoder = Decoder::new();
    let opts = RenderOptions::default();

    let mut index = 0u32;
    let mut captured = 0;
    loop {
        // The camera shakes: the skew changes every frame, and one frame in
        // three is missed entirely.
        if index % 3 != 2 {
            let wobble = (index % 5) as f64 * 0.006;
            let cam = Camera {
                skew: [
                    (0.02 + wobble, 0.01),
                    (-0.03, 0.01 + wobble),
                    (0.01, -0.02),
                    (-0.015 - wobble, -0.01),
                ],
                blur: 1,
                gain: [0.9, 0.88, 0.82],
                bias: [16.0, 18.0, 22.0],
                noise: 6,
                seed: 0x5EED + index as u64,
                ..Camera::default()
            };
            captured += 1;
            if let Progress::Complete(bytes) =
                shoot_and_decode(&encoder, &mut decoder, index, &cam, &opts)
            {
                assert_eq!(bytes, payload);
                assert!(
                    decoder.frames_accepted() >= encoder.block_count(),
                    "accepted {} frames for {} blocks",
                    decoder.frames_accepted(),
                    encoder.block_count()
                );
                return;
            }
        }
        index += 1;
        assert!(
            index < 400,
            "never completed: captured {captured}, accepted {}, progress {:.0}%",
            decoder.frames_accepted(),
            decoder.progress() * 100.0
        );
    }
}

#[test]
fn decodes_every_grid_size_through_the_lens() {
    for grid in GridSize::ALL {
        let payload = b"grid sweep";
        let encoder = Encoder::new(payload, config_for(grid)).unwrap();
        let mut decoder = Decoder::new();

        // Bigger grids need more pixels per cell to stay resolvable.
        let opts = RenderOptions {
            module_px: 8,
            quiet_zone: 4,
            background: [255, 255, 255],
            ink: None,
        };
        let cam = Camera {
            out: opts.output_size(grid.modules()) * 5 / 4,
            ..Camera::default()
        };

        match shoot_and_decode(&encoder, &mut decoder, 0, &cam, &opts) {
            Progress::Complete(bytes) => assert_eq!(bytes, payload, "{grid:?}"),
            other => panic!("{grid:?} failed through the lens: {other:?}"),
        }
    }
}

fn rotate90(img: &RgbImage) -> RgbImage {
    let mut out = RgbImage::new(img.height, img.width);
    for y in 0..img.height {
        for x in 0..img.width {
            out.set(img.height - 1 - y, x, img.get(x, y));
        }
    }
    out
}

#[test]
fn decodes_a_capture_at_any_rotation() {
    // Four identical corner markers mean the frame's orientation is not
    // encoded geometrically; it is recovered by trying each rotation against
    // the header CRC. A phone held sideways, or upside down, must still work.
    let payload = b"rotation should not matter";
    let encoder = Encoder::new(payload, EncoderConfig::default()).unwrap();
    let frame = encoder.frame(0).unwrap();
    let rendered = render(&frame, &RenderOptions::default());

    let mut shot = photograph(&rendered, &Camera::default());
    for turns in 0..4 {
        let mut decoder = Decoder::new();
        let view = RgbView::rgb(shot.width, shot.height, &shot.data).unwrap();
        match decoder.ingest_image(&view).unwrap() {
            Progress::Complete(bytes) => assert_eq!(bytes, payload, "at {turns} turns"),
            other => panic!("rotation of {turns} quarter turns failed: {other:?}"),
        }
        shot = rotate90(&shot);
    }
}

#[test]
fn decodes_a_rotated_and_skewed_capture() {
    let payload = b"sideways and tilted";
    let encoder = Encoder::new(payload, EncoderConfig::default()).unwrap();
    let frame = encoder.frame(0).unwrap();
    let rendered = render(&frame, &RenderOptions::default());
    let cam = Camera {
        skew: [
            (0.04, 0.015),
            (-0.045, 0.02),
            (0.02, -0.015),
            (-0.02, -0.01),
        ],
        blur: 1,
        noise: 5,
        ..Camera::default()
    };
    let shot = rotate90(&photograph(&rendered, &cam));

    let mut decoder = Decoder::new();
    let view = RgbView::rgb(shot.width, shot.height, &shot.data).unwrap();
    match decoder.ingest_image(&view).unwrap() {
        Progress::Complete(bytes) => assert_eq!(bytes, payload),
        other => panic!("rotated and skewed capture failed: {other:?}"),
    }
}

#[test]
fn reports_not_found_on_an_image_with_no_frame() {
    let mut decoder = Decoder::new();
    let mut img = RgbImage::new(300, 300);
    let mut rng = Rng(1);
    for y in 0..300 {
        for x in 0..300 {
            let v = (rng.next() & 0xFF) as u8;
            img.set(x, y, [v, v, v]);
        }
    }
    let view = RgbView::rgb(img.width, img.height, &img.data).unwrap();
    let progress = decoder.ingest_image(&view).unwrap();
    assert!(
        matches!(progress, Progress::NotFound | Progress::Rejected),
        "random noise should not decode, got {progress:?}"
    );
    assert_eq!(decoder.frames_accepted(), 0);
}

#[test]
fn a_partially_occluded_frame_still_decodes() {
    // A thumb over one corner of the screen, covering part of the data but
    // leaving the three finder patterns visible.
    let payload = b"occlusion test payload";
    let encoder = Encoder::new(payload, EncoderConfig::resilient()).unwrap();
    let mut decoder = Decoder::new();
    let opts = RenderOptions {
        module_px: 9,
        quiet_zone: 4,
        background: [255, 255, 255],
        ink: None,
    };

    let frame = encoder.frame(0).unwrap();
    let mut rendered = render(&frame, &opts);

    // Occlude a block in the middle-right, away from the finders.
    let w = rendered.width;
    for y in (w * 55 / 100)..(w * 70 / 100) {
        for x in (w * 60 / 100)..(w * 85 / 100) {
            rendered.set(x, y, [90, 70, 60]);
        }
    }

    let shot = photograph(&rendered, &Camera::default());
    let view = RgbView::rgb(shot.width, shot.height, &shot.data).unwrap();
    match decoder.ingest_image(&view).unwrap() {
        Progress::Complete(bytes) => assert_eq!(bytes, payload),
        other => panic!("occluded frame failed: {other:?}"),
    }
}
