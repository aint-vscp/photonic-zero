//! Points and the projective transform that undoes camera perspective.

use alloc::vec::Vec;

/// A point in floating-point image or grid coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    /// Horizontal coordinate, increasing to the right.
    pub x: f64,
    /// Vertical coordinate, increasing downwards.
    pub y: f64,
}

impl Point {
    /// Construct a point.
    #[must_use]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// Squared Euclidean distance, avoiding a square root.
    #[must_use]
    pub fn dist2(&self, other: Self) -> f64 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        dx * dx + dy * dy
    }

    /// Euclidean distance.
    #[must_use]
    pub fn dist(&self, other: Self) -> f64 {
        crate::fmath_sqrt(self.dist2(other))
    }
}

/// A 3x3 projective transform stored in row-major order with `m[8]`
/// normalised to 1.
///
/// A camera pointed at a screen from an angle turns the square grid into a
/// general quadrilateral. Only a full projective transform can undo that; an
/// affine transform cannot represent the foreshortening that makes the far
/// edge of the frame shorter than the near edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Homography {
    /// Row-major 3x3 matrix coefficients.
    pub m: [f64; 9],
}

impl Homography {
    /// The identity transform.
    #[must_use]
    pub const fn identity() -> Self {
        Self {
            m: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
        }
    }

    /// Solve for the transform carrying four `src` points onto four `dst`
    /// points.
    ///
    /// Returns `None` when the correspondence is degenerate, for example when
    /// three of the points are collinear.
    #[must_use]
    pub fn from_correspondences(src: &[Point; 4], dst: &[Point; 4]) -> Option<Self> {
        // Each correspondence contributes two rows to an 8x8 system in the
        // unknowns h0..h7, with h8 fixed at 1.
        let mut a = [[0.0f64; 9]; 8];
        for i in 0..4 {
            let (x, y) = (src[i].x, src[i].y);
            let (u, v) = (dst[i].x, dst[i].y);

            a[2 * i] = [x, y, 1.0, 0.0, 0.0, 0.0, -x * u, -y * u, u];
            a[2 * i + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -x * v, -y * v, v];
        }

        let sol = solve8(&mut a)?;
        Some(Self {
            m: [
                sol[0], sol[1], sol[2], sol[3], sol[4], sol[5], sol[6], sol[7], 1.0,
            ],
        })
    }

    /// Map a point through the transform.
    #[must_use]
    pub fn apply(&self, p: Point) -> Point {
        let m = &self.m;
        let w = m[6] * p.x + m[7] * p.y + m[8];
        if w == 0.0 {
            return Point::new(f64::NAN, f64::NAN);
        }
        Point::new(
            (m[0] * p.x + m[1] * p.y + m[2]) / w,
            (m[3] * p.x + m[4] * p.y + m[5]) / w,
        )
    }

    /// The inverse transform, if it exists.
    #[must_use]
    pub fn invert(&self) -> Option<Self> {
        let m = &self.m;
        let c = [
            m[4] * m[8] - m[5] * m[7],
            m[2] * m[7] - m[1] * m[8],
            m[1] * m[5] - m[2] * m[4],
            m[5] * m[6] - m[3] * m[8],
            m[0] * m[8] - m[2] * m[6],
            m[2] * m[3] - m[0] * m[5],
            m[3] * m[7] - m[4] * m[6],
            m[1] * m[6] - m[0] * m[7],
            m[0] * m[4] - m[1] * m[3],
        ];
        let det = m[0] * c[0] + m[1] * c[3] + m[2] * c[6];
        if det.abs() < 1e-12 {
            return None;
        }
        let mut out = [0.0f64; 9];
        for i in 0..9 {
            out[i] = c[i] / det;
        }
        // Renormalise so the bottom-right entry is 1.
        if out[8].abs() > 1e-15 {
            let s = out[8];
            for v in &mut out {
                *v /= s;
            }
        }
        Some(Self { m: out })
    }
}

/// Gaussian elimination with partial pivoting on an 8x9 augmented matrix.
fn solve8(a: &mut [[f64; 9]; 8]) -> Option<[f64; 8]> {
    for col in 0..8 {
        // Pivot on the largest magnitude entry for numerical stability.
        let mut best = col;
        for row in (col + 1)..8 {
            if a[row][col].abs() > a[best][col].abs() {
                best = row;
            }
        }
        if a[best][col].abs() < 1e-12 {
            return None;
        }
        a.swap(col, best);

        let pivot = a[col][col];
        for v in a[col].iter_mut().skip(col) {
            *v /= pivot;
        }
        // Copy the pivot row out first: it is `[f64; 9]` and therefore `Copy`,
        // which sidesteps borrowing `a` mutably and immutably at once.
        let pivot_row = a[col];
        for (row, target_row) in a.iter_mut().enumerate() {
            if row == col {
                continue;
            }
            let factor = target_row[col];
            if factor != 0.0 {
                for (k, target) in target_row.iter_mut().enumerate().skip(col) {
                    *target -= factor * pivot_row[k];
                }
            }
        }
    }

    let mut out = [0.0f64; 8];
    for (i, o) in out.iter_mut().enumerate() {
        *o = a[i][8];
        if !o.is_finite() {
            return None;
        }
    }
    Some(out)
}

/// Order four unordered corner points into `[top-left, top-right,
/// bottom-right, bottom-left]`.
///
/// Uses the centroid and the sign of each point's offset from it, which is
/// stable for any quadrilateral a camera can produce short of a full 90 degree
/// rotation.
#[must_use]
pub fn order_corners(points: &[Point]) -> Option<[Point; 4]> {
    if points.len() != 4 {
        return None;
    }
    let cx = points.iter().map(|p| p.x).sum::<f64>() / 4.0;
    let cy = points.iter().map(|p| p.y).sum::<f64>() / 4.0;

    let mut tl = None;
    let mut tr = None;
    let mut br = None;
    let mut bl = None;
    for &p in points {
        let slot = match (p.x < cx, p.y < cy) {
            (true, true) => &mut tl,
            (false, true) => &mut tr,
            (false, false) => &mut br,
            (true, false) => &mut bl,
        };
        if slot.is_some() {
            return None; // two points in the same quadrant: ambiguous
        }
        *slot = Some(p);
    }
    Some([tl?, tr?, br?, bl?])
}

/// Signed area of a polygon, twice the true area. Positive means the vertices
/// wind in the same direction as `[(0,0), (1,0), (1,1), (0,1)]` does in image
/// coordinates, where `y` grows downwards.
#[must_use]
pub fn signed_area(points: &[Point]) -> f64 {
    let mut sum = 0.0;
    for i in 0..points.len() {
        let a = points[i];
        let b = points[(i + 1) % points.len()];
        sum += a.x * b.y - b.x * a.y;
    }
    sum
}

/// Put four corner points into a consistent cyclic order with a known winding.
///
/// Unlike [`order_corners`], this makes no assumption about which corner is
/// which - it works for a quadrilateral at any rotation, including one turned
/// past 45 degrees where the "which quadrant is it in" test breaks down. The
/// caller is expected to resolve *which* corner is the top left by other
/// means; this only guarantees that walking the result visits the corners in
/// order around the shape rather than crossing through the middle.
///
/// Works by finding the longest pair, which must be a diagonal of a convex
/// quadrilateral, and interleaving the remaining two.
#[must_use]
pub fn order_quad(points: &[Point]) -> Option<[Point; 4]> {
    if points.len() != 4 {
        return None;
    }

    // The longest of the six pairwise distances is a diagonal.
    let mut best = (0usize, 1usize);
    let mut best_d = -1.0;
    for i in 0..4 {
        for j in (i + 1)..4 {
            let d = points[i].dist2(points[j]);
            if d > best_d {
                best_d = d;
                best = (i, j);
            }
        }
    }
    if best_d <= 0.0 {
        return None;
    }

    let (i, j) = best;
    let others: Vec<usize> = (0..4).filter(|k| *k != i && *k != j).collect();
    if others.len() != 2 {
        return None;
    }

    // Alternating the two diagonals walks the perimeter.
    let mut quad = [points[i], points[others[0]], points[j], points[others[1]]];
    if signed_area(&quad) < 0.0 {
        quad.swap(1, 3);
    }
    Some(quad)
}

/// Cluster points that lie within `radius` of each other, averaging each
/// cluster. Used to merge the many per-row hits a single finder produces.
#[must_use]
pub fn cluster(points: &[(Point, f64)], radius: f64) -> Vec<(Point, f64)> {
    let mut clusters: Vec<(Point, f64, usize)> = Vec::new();
    let r2 = radius * radius;

    for &(p, module) in points {
        let mut merged = false;
        for c in &mut clusters {
            if c.0.dist2(p) <= r2 {
                let n = c.2 as f64;
                c.0 = Point::new((c.0.x * n + p.x) / (n + 1.0), (c.0.y * n + p.y) / (n + 1.0));
                c.1 = (c.1 * n + module) / (n + 1.0);
                c.2 += 1;
                merged = true;
                break;
            }
        }
        if !merged {
            clusters.push((p, module, 1));
        }
    }

    // A genuine finder is crossed by several scan lines; a one-off hit is
    // almost always noise.
    clusters
        .into_iter()
        .filter(|c| c.2 >= 2)
        .map(|c| (c.0, c.1))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn approx(a: Point, b: Point, tol: f64) -> bool {
        (a.x - b.x).abs() < tol && (a.y - b.y).abs() < tol
    }

    #[test]
    fn identity_maps_points_to_themselves() {
        let h = Homography::identity();
        let p = Point::new(3.5, -2.25);
        assert!(approx(h.apply(p), p, 1e-12));
    }

    #[test]
    fn recovers_a_pure_translation_and_scale() {
        let src = [
            Point::new(0.0, 0.0),
            Point::new(1.0, 0.0),
            Point::new(1.0, 1.0),
            Point::new(0.0, 1.0),
        ];
        let dst = [
            Point::new(10.0, 20.0),
            Point::new(110.0, 20.0),
            Point::new(110.0, 120.0),
            Point::new(10.0, 120.0),
        ];
        let h = Homography::from_correspondences(&src, &dst).unwrap();
        for i in 0..4 {
            assert!(approx(h.apply(src[i]), dst[i], 1e-9), "corner {i}");
        }
        // The centre must land in the centre.
        assert!(approx(
            h.apply(Point::new(0.5, 0.5)),
            Point::new(60.0, 70.0),
            1e-9
        ));
    }

    #[test]
    fn recovers_a_perspective_warp() {
        let src = [
            Point::new(0.0, 0.0),
            Point::new(64.0, 0.0),
            Point::new(64.0, 64.0),
            Point::new(0.0, 64.0),
        ];
        // A trapezoid: the top edge is much shorter than the bottom, as when a
        // camera looks up at a screen.
        let dst = [
            Point::new(120.0, 50.0),
            Point::new(280.0, 60.0),
            Point::new(360.0, 300.0),
            Point::new(40.0, 290.0),
        ];
        let h = Homography::from_correspondences(&src, &dst).unwrap();
        for i in 0..4 {
            assert!(approx(h.apply(src[i]), dst[i], 1e-8), "corner {i}");
        }
    }

    #[test]
    fn inverse_round_trips() {
        let src = [
            Point::new(0.0, 0.0),
            Point::new(32.0, 0.0),
            Point::new(32.0, 32.0),
            Point::new(0.0, 32.0),
        ];
        let dst = [
            Point::new(15.0, 9.0),
            Point::new(300.0, 40.0),
            Point::new(280.0, 260.0),
            Point::new(30.0, 240.0),
        ];
        let h = Homography::from_correspondences(&src, &dst).unwrap();
        let inv = h.invert().unwrap();
        for gx in [0.0, 7.5, 16.0, 31.0] {
            for gy in [0.0, 3.25, 20.0, 32.0] {
                let p = Point::new(gx, gy);
                let round = inv.apply(h.apply(p));
                assert!(approx(round, p, 1e-6), "round trip at {gx},{gy}");
            }
        }
    }

    #[test]
    fn degenerate_correspondence_is_rejected() {
        let collinear = [
            Point::new(0.0, 0.0),
            Point::new(1.0, 1.0),
            Point::new(2.0, 2.0),
            Point::new(3.0, 3.0),
        ];
        let dst = [
            Point::new(0.0, 0.0),
            Point::new(1.0, 0.0),
            Point::new(1.0, 1.0),
            Point::new(0.0, 1.0),
        ];
        assert!(Homography::from_correspondences(&collinear, &dst).is_none());
    }

    #[test]
    fn orders_corners_from_any_permutation() {
        let tl = Point::new(10.0, 10.0);
        let tr = Point::new(90.0, 12.0);
        let br = Point::new(95.0, 80.0);
        let bl = Point::new(8.0, 85.0);
        for perm in [
            [tl, tr, br, bl],
            [br, bl, tl, tr],
            [tr, bl, br, tl],
            [bl, br, tr, tl],
        ] {
            let ordered = order_corners(&perm).unwrap();
            assert_eq!(ordered, [tl, tr, br, bl]);
        }
    }

    #[test]
    fn order_quad_walks_the_perimeter_at_any_rotation() {
        // A square rotated 45 degrees, which defeats quadrant-based ordering.
        let diamond = [
            Point::new(50.0, 0.0),
            Point::new(100.0, 50.0),
            Point::new(50.0, 100.0),
            Point::new(0.0, 50.0),
        ];
        for start in 0..4 {
            // Feed the points in several different orders, including ones that
            // would trace a bow-tie if taken literally.
            let shuffled = [
                diamond[start],
                diamond[(start + 2) % 4],
                diamond[(start + 1) % 4],
                diamond[(start + 3) % 4],
            ];
            let ordered = order_quad(&shuffled).unwrap();
            // A correctly ordered convex quad has |signed area| equal to the
            // true area (2 * 5000 here); a bow-tie has less.
            assert!(
                (signed_area(&ordered) - 10_000.0).abs() < 1e-6,
                "start {start}: area {}",
                signed_area(&ordered)
            );
        }
    }

    #[test]
    fn order_quad_matches_the_reference_winding() {
        let square = [
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 10.0),
            Point::new(0.0, 10.0),
        ];
        assert!(signed_area(&square) > 0.0);
        let ordered = order_quad(&square).unwrap();
        assert!(signed_area(&ordered) > 0.0, "winding was not normalised");

        // Reversed input must come back with the same winding.
        let mut reversed = square;
        reversed.reverse();
        let ordered = order_quad(&reversed).unwrap();
        assert!(signed_area(&ordered) > 0.0);
    }

    #[test]
    fn order_quad_handles_a_perspective_trapezoid() {
        let trapezoid = [
            Point::new(120.0, 50.0),
            Point::new(280.0, 60.0),
            Point::new(360.0, 300.0),
            Point::new(40.0, 290.0),
        ];
        let ordered = order_quad(&trapezoid).unwrap();
        let area = signed_area(&ordered).abs();
        assert!(area > 60_000.0, "degenerate ordering, area {area}");
    }

    #[test]
    fn order_quad_rejects_wrong_counts() {
        assert!(order_quad(&[Point::new(0.0, 0.0)]).is_none());
        assert!(order_quad(&[]).is_none());
    }

    #[test]
    fn cluster_merges_nearby_hits_and_drops_singletons() {
        let pts = vec![
            (Point::new(10.0, 10.0), 3.0),
            (Point::new(10.5, 10.2), 3.2),
            (Point::new(11.0, 9.8), 2.9),
            (Point::new(200.0, 200.0), 4.0), // lone hit, should be dropped
        ];
        let out = cluster(&pts, 3.0);
        assert_eq!(out.len(), 1);
        assert!((out[0].0.x - 10.5).abs() < 0.6);
    }
}
