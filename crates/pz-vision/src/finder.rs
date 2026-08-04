//! Locating the fiducial markers that pin a PZ frame to the image.
//!
//! PZ reuses the run-length signature that makes QR finder patterns so easy to
//! spot: a line crossing the centre of a concentric square hits dark, light,
//! dark, light, dark runs whose lengths are in a fixed ratio, *independently of
//! scale, rotation and perspective*. Scanning every row for that ratio costs
//! one pass and needs no prior knowledge of where the code is or how big it is.
//!
//! A hit on one row is only a candidate. It is confirmed by re-running the same
//! test down the column through the candidate, then back across the row, which
//! rejects the stray horizontal streaks that random data cells produce.

use crate::geom::{cluster, Point};
use crate::BinaryImage;
use alloc::vec::Vec;

/// The 7x7 concentric square used at three corners: 1:1:3:1:1.
pub const FINDER_RATIOS: [f64; 5] = [1.0, 1.0, 3.0, 1.0, 1.0];

/// The 5x5 concentric square used at the fourth corner: 1:1:1:1:1.
pub const CORNER_RATIOS: [f64; 5] = [1.0, 1.0, 1.0, 1.0, 1.0];

/// A located fiducial marker.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FinderPattern {
    /// Centre in image coordinates.
    pub center: Point,
    /// Estimated width of one grid cell in pixels.
    pub module: f64,
}

/// One maximal run of identical pixels along a scan line.
#[derive(Debug, Clone, Copy)]
struct Run {
    dark: bool,
    start: usize,
    len: usize,
}

impl Run {
    fn center(&self) -> f64 {
        self.start as f64 + self.len as f64 / 2.0
    }
    fn end(&self) -> usize {
        self.start + self.len
    }
}

fn runs_in_row(bin: &BinaryImage, y: usize, x0: usize, x1: usize) -> Vec<Run> {
    let mut runs = Vec::new();
    if x1 <= x0 {
        return runs;
    }
    let mut current = bin.get(x0, y);
    let mut start = x0;
    for x in (x0 + 1)..x1 {
        let v = bin.get(x, y);
        if v != current {
            runs.push(Run {
                dark: current,
                start,
                len: x - start,
            });
            current = v;
            start = x;
        }
    }
    runs.push(Run {
        dark: current,
        start,
        len: x1 - start,
    });
    runs
}

fn runs_in_col(bin: &BinaryImage, x: usize, y0: usize, y1: usize) -> Vec<Run> {
    let mut runs = Vec::new();
    if y1 <= y0 {
        return runs;
    }
    let mut current = bin.get(x, y0);
    let mut start = y0;
    for y in (y0 + 1)..y1 {
        let v = bin.get(x, y);
        if v != current {
            runs.push(Run {
                dark: current,
                start,
                len: y - start,
            });
            current = v;
            start = y;
        }
    }
    runs.push(Run {
        dark: current,
        start,
        len: y1 - start,
    });
    runs
}

/// Test whether five consecutive runs starting at `i` match the ratio
/// template. Returns the centre coordinate along the scan line and the implied
/// module size.
fn match_window(runs: &[Run], i: usize, ratios: &[f64; 5]) -> Option<(f64, f64)> {
    if i + 5 > runs.len() {
        return None;
    }
    let w = &runs[i..i + 5];
    // Must start dark and alternate.
    if !w[0].dark || w[1].dark || !w[2].dark || w[3].dark || !w[4].dark {
        return None;
    }

    let total: usize = w.iter().map(|r| r.len).sum();
    let units: f64 = ratios.iter().sum();
    if (total as f64) < units {
        return None;
    }
    let module = total as f64 / units;
    if module < 1.0 {
        return None;
    }

    // Each run must be within half its own expected width of the ideal. The
    // tolerance scales with the ratio so the wide centre run is not held to an
    // unfairly tight absolute bound.
    for (run, &ratio) in w.iter().zip(ratios.iter()) {
        let expected = ratio * module;
        let allowed = (expected / 2.0).max(0.75);
        if (run.len as f64 - expected).abs() > allowed {
            return None;
        }
    }

    Some((w[2].center(), module))
}

/// Find the run containing `pos`, then test the window centred on it.
fn match_through(runs: &[Run], pos: usize, ratios: &[f64; 5]) -> Option<(f64, f64)> {
    let idx = runs.iter().position(|r| pos >= r.start && pos < r.end())?;
    if idx < 2 {
        return None;
    }
    match_window(runs, idx - 2, ratios)
}

/// Scan a region of the image for markers matching `ratios`.
///
/// `step` skips rows to trade sensitivity for speed; 1 examines every row.
fn scan_region(
    bin: &BinaryImage,
    ratios: &[f64; 5],
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
    step: usize,
) -> Vec<(Point, f64)> {
    let mut hits: Vec<(Point, f64)> = Vec::new();
    let step = step.max(1);

    let mut y = y0;
    while y < y1 {
        let row = runs_in_row(bin, y, x0, x1);
        for i in 0..row.len() {
            let Some((cx, module)) = match_window(&row, i, ratios) else {
                continue;
            };

            // Confirm down the column through the candidate centre.
            let xi = cx as usize;
            if xi >= bin.width {
                continue;
            }
            let col = runs_in_col(bin, xi, y0, y1);
            let Some((cy, vmodule)) = match_through(&col, y, ratios) else {
                continue;
            };

            // The horizontal and vertical module estimates must agree, or this
            // is a streak rather than a square.
            if (module - vmodule).abs() > module.max(vmodule) * 0.5 {
                continue;
            }

            // Re-confirm across the row through the refined centre.
            let yi = cy as usize;
            if yi >= bin.height {
                continue;
            }
            let row2 = runs_in_row(bin, yi, x0, x1);
            let Some((cx2, hmodule)) = match_through(&row2, xi, ratios) else {
                continue;
            };

            let module = (module + vmodule + hmodule) / 3.0;
            hits.push((Point::new(cx2, cy), module));
        }
        y += step;
    }
    hits
}

/// Locate every 7x7 finder pattern in the image.
///
/// Returns cluster centres, strongest first, with no assumption about how many
/// there should be.
#[must_use]
pub fn find_finder_patterns(bin: &BinaryImage) -> Vec<FinderPattern> {
    let hits = scan_region(bin, &FINDER_RATIOS, 0, 0, bin.width, bin.height, 1);
    if hits.is_empty() {
        return Vec::new();
    }
    let radius = hits.iter().map(|h| h.1).sum::<f64>() / hits.len() as f64 * 2.0;
    cluster(&hits, radius.max(2.0))
        .into_iter()
        .map(|(center, module)| FinderPattern { center, module })
        .collect()
}

/// Order three finder patterns into `(top_left, top_right, bottom_left)`.
///
/// The top-left marker is the one at the right angle of the L the three
/// markers form. Which of the other two is "top right" is then decided by the
/// sign of the cross product, which also tells us the frame's handedness.
#[must_use]
pub fn order_finders(
    patterns: &[FinderPattern],
) -> Option<(FinderPattern, FinderPattern, FinderPattern)> {
    if patterns.len() < 3 {
        return None;
    }
    // Use the three strongest candidates.
    let p = &patterns[..3];

    // The corner is opposite the longest side of the triangle.
    let d01 = p[0].center.dist2(p[1].center);
    let d12 = p[1].center.dist2(p[2].center);
    let d02 = p[0].center.dist2(p[2].center);

    let (corner, a, b) = if d01 >= d12 && d01 >= d02 {
        (p[2], p[0], p[1])
    } else if d12 >= d01 && d12 >= d02 {
        (p[0], p[1], p[2])
    } else {
        (p[1], p[0], p[2])
    };

    // Cross product of (a - corner) x (b - corner). In image coordinates y
    // grows downwards, so a negative z means `a` is counter-clockwise from
    // `b`, which puts it on the "top right" arm.
    let ax = a.center.x - corner.center.x;
    let ay = a.center.y - corner.center.y;
    let bx = b.center.x - corner.center.x;
    let by = b.center.y - corner.center.y;
    let cross = ax * by - ay * bx;

    if cross < 0.0 {
        Some((corner, b, a))
    } else {
        Some((corner, a, b))
    }
}

/// Search a small neighbourhood for the 5x5 corner marker.
///
/// The 1:1:1:1:1 signature is far weaker than the finder's and appears by
/// chance all over a dense data grid, so this is deliberately *not* a global
/// search: the caller predicts where the marker should be from the three
/// finders, and this confirms it locally.
#[must_use]
pub fn find_corner_marker(bin: &BinaryImage, approx: Point, radius: f64) -> Option<Point> {
    let r = radius.max(2.0);
    let x0 = (approx.x - r).max(0.0) as usize;
    let y0 = (approx.y - r).max(0.0) as usize;
    let x1 = ((approx.x + r) as usize + 1).min(bin.width);
    let y1 = ((approx.y + r) as usize + 1).min(bin.height);
    if x1 <= x0 + 5 || y1 <= y0 + 5 {
        return None;
    }

    let hits = scan_region(bin, &CORNER_RATIOS, x0, y0, x1, y1, 1);
    if hits.is_empty() {
        return None;
    }

    // Closest confirmed hit to the prediction wins.
    let mut best = hits[0];
    let mut best_d = best.0.dist2(approx);
    for &h in &hits[1..] {
        let d = h.0.dist2(approx);
        if d < best_d {
            best = h;
            best_d = d;
        }
    }
    Some(best.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GrayImage;

    /// Draw a concentric square marker of `units` modules per side.
    fn draw_marker(img: &mut GrayImage, cx: usize, cy: usize, module: usize, units: usize) {
        let half = units * module / 2;
        for dy in 0..(units * module) {
            for dx in 0..(units * module) {
                let mx = (dx / module) as i32;
                let my = (dy / module) as i32;
                let edge = (units - 1) as i32;
                let ring = mx.min(my).min(edge - mx).min(edge - my);
                // ring 0 = outer dark, ring 1 = light, ring >= 2 = dark core
                let dark = ring == 0 || ring >= 2;
                let x = cx + dx - half;
                let y = cy + dy - half;
                if x < img.width && y < img.height {
                    img.data[y * img.width + x] = if dark { 0 } else { 255 };
                }
            }
        }
    }

    fn blank(w: usize, h: usize) -> GrayImage {
        let mut g = GrayImage::new(w, h);
        g.data.fill(255);
        g
    }

    fn binarise(g: &GrayImage) -> BinaryImage {
        crate::threshold::global_threshold(g, 128)
    }

    #[test]
    fn finds_a_single_finder_pattern() {
        let mut img = blank(200, 200);
        draw_marker(&mut img, 100, 100, 4, 7);
        let bin = binarise(&img);
        let found = find_finder_patterns(&bin);
        assert_eq!(found.len(), 1, "expected exactly one marker, got {found:?}");
        assert!((found[0].center.x - 100.0).abs() < 2.0);
        assert!((found[0].center.y - 100.0).abs() < 2.0);
        assert!(
            (found[0].module - 4.0).abs() < 1.0,
            "module estimate {} should be near 4",
            found[0].module
        );
    }

    #[test]
    fn finds_three_finders_and_orders_them() {
        let mut img = blank(400, 400);
        let m = 4;
        draw_marker(&mut img, 60, 60, m, 7); // top left
        draw_marker(&mut img, 340, 60, m, 7); // top right
        draw_marker(&mut img, 60, 340, m, 7); // bottom left
        let bin = binarise(&img);

        let found = find_finder_patterns(&bin);
        assert_eq!(found.len(), 3, "got {} markers", found.len());

        let (tl, tr, bl) = order_finders(&found).unwrap();
        assert!(
            (tl.center.x - 60.0).abs() < 3.0 && (tl.center.y - 60.0).abs() < 3.0,
            "tl {:?}",
            tl.center
        );
        assert!(
            (tr.center.x - 340.0).abs() < 3.0 && (tr.center.y - 60.0).abs() < 3.0,
            "tr {:?}",
            tr.center
        );
        assert!(
            (bl.center.x - 60.0).abs() < 3.0 && (bl.center.y - 340.0).abs() < 3.0,
            "bl {:?}",
            bl.center
        );
    }

    #[test]
    fn ordering_is_independent_of_input_order() {
        let mut img = blank(400, 400);
        draw_marker(&mut img, 60, 60, 4, 7);
        draw_marker(&mut img, 340, 60, 4, 7);
        draw_marker(&mut img, 60, 340, 4, 7);
        let bin = binarise(&img);
        let mut found = find_finder_patterns(&bin);
        assert_eq!(found.len(), 3);

        let baseline = order_finders(&found).unwrap();
        found.reverse();
        let reversed = order_finders(&found).unwrap();
        assert!((baseline.0.center.x - reversed.0.center.x).abs() < 1.0);
        assert!((baseline.1.center.x - reversed.1.center.x).abs() < 1.0);
        assert!((baseline.2.center.x - reversed.2.center.x).abs() < 1.0);
    }

    #[test]
    fn finds_markers_at_several_scales() {
        for module in [2usize, 3, 5, 8, 12] {
            let size = 40 * module;
            let mut img = blank(size, size);
            draw_marker(&mut img, size / 2, size / 2, module, 7);
            let bin = binarise(&img);
            let found = find_finder_patterns(&bin);
            assert_eq!(found.len(), 1, "module {module}: got {}", found.len());
            assert!(
                (found[0].module - module as f64).abs() < module as f64 * 0.35,
                "module {module}: estimated {}",
                found[0].module
            );
        }
    }

    #[test]
    fn ignores_plain_squares_and_stripes() {
        let mut img = blank(200, 200);
        // A solid block, and a set of equal stripes: neither has the 1:1:3:1:1
        // signature.
        for y in 40..80 {
            for x in 40..80 {
                img.data[y * 200 + x] = 0;
            }
        }
        for y in 120..160 {
            for x in 0..200 {
                if (x / 4) % 2 == 0 {
                    img.data[y * 200 + x] = 0;
                }
            }
        }
        let bin = binarise(&img);
        assert!(
            find_finder_patterns(&bin).is_empty(),
            "false positive on non-finder shapes"
        );
    }

    #[test]
    fn corner_marker_is_found_near_a_prediction() {
        let mut img = blank(200, 200);
        draw_marker(&mut img, 150, 150, 4, 5); // 5x5 corner marker
        let bin = binarise(&img);
        let found = find_corner_marker(&bin, Point::new(146.0, 154.0), 30.0).unwrap();
        assert!((found.x - 150.0).abs() < 3.0, "x = {}", found.x);
        assert!((found.y - 150.0).abs() < 3.0, "y = {}", found.y);
    }

    #[test]
    fn corner_marker_search_fails_cleanly_when_absent() {
        let img = blank(200, 200);
        let bin = binarise(&img);
        assert!(find_corner_marker(&bin, Point::new(100.0, 100.0), 20.0).is_none());
    }

    #[test]
    fn empty_image_finds_nothing() {
        let bin = BinaryImage::new(0, 0);
        assert!(find_finder_patterns(&bin).is_empty());
        assert!(order_finders(&[]).is_none());
    }

    #[test]
    fn runs_cover_the_whole_scan_line() {
        let mut img = blank(20, 4);
        for x in 5..12 {
            img.data[x] = 0;
        }
        let bin = binarise(&img);
        let runs = runs_in_row(&bin, 0, 0, 20);
        assert_eq!(runs.iter().map(|r| r.len).sum::<usize>(), 20);
        assert_eq!(runs.len(), 3);
        assert!(!runs[0].dark && runs[1].dark && !runs[2].dark);
        assert_eq!(runs[1].start, 5);
        assert_eq!(runs[1].len, 7);
    }

    #[test]
    fn cluster_radius_does_not_merge_distinct_finders() {
        // Two well-separated finders must stay separate.
        let mut img = blank(300, 120);
        draw_marker(&mut img, 80, 60, 3, 7);
        draw_marker(&mut img, 220, 60, 3, 7);
        let bin = binarise(&img);
        let found = find_finder_patterns(&bin);
        assert_eq!(found.len(), 2, "got {found:?}");
    }
}
