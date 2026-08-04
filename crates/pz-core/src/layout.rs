//! Frame geometry: what lives in every cell of a PZ grid.
//!
//! A PZ frame is an `N` by `N` grid of square cells. Some cells are structural
//! and identical in every frame; the rest carry data. The split is computed
//! here, once, and both the encoder and the decoder walk it in the same order.
//!
//! ```text
//!   +-------+---------------+-------+
//!   | TL    |    palette    |    TR |   TL/TR/BL  7x7 finder patterns
//!   | 7x7   |  8x4 patches  |   7x7 |   palette   8 reference colours
//!   +-------+               +-------+   timing    alternating rulers
//!   |     . . timing . . . . . . .  |   BR        5x5 corner marker
//!   |  t                            |
//!   |  i     header (256 cells)     |
//!   |  m     then data cells        |
//!   |  i                            |
//!   |  n                            |
//!   |  g                     +------+
//!   | BL                     |  BR  |
//!   | 7x7                    | 5x5  |
//!   +------------------------+------+
//! ```
//!
//! # Why four markers and not three
//!
//! QR codes locate themselves from three finder patterns. Three points define
//! an affine transform, which can express rotation, scale and shear but *not*
//! perspective. Hold a phone at an angle to a screen and the far edge of the
//! frame is genuinely shorter than the near edge; an affine fit spreads that
//! error across the grid and the cells near one corner sample the wrong place.
//!
//! A fourth marker gives four correspondences, which is exactly what a
//! projective transform needs. The fourth is smaller (5x5) because by the time
//! we look for it the other three already tell us where it must be, so it only
//! has to be confirmed, not discovered.

use alloc::vec;
use alloc::vec::Vec;

/// Number of cells reserved for the frame header.
///
/// The header is 16 bytes protected by RS(32,16), and is always modulated in
/// black and white regardless of the frame's colour mode: 32 bytes x 8 bits =
/// 256 cells. Everything else in the frame is undecodable until the header
/// parses, so it gets the most robust coding in the format.
pub const HEADER_CELLS: usize = 256;

/// Supported grid sizes, in cells per side.
///
/// Sizes are spaced 16 apart so that the decoder's estimate of `N` from the
/// finder spacing has a tolerance of plus or minus 8 cells before it could
/// possibly snap to the wrong one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GridSize {
    /// 33x33: smallest, largest cells, best for long range or poor cameras.
    G33,
    /// 49x49: the default.
    G49,
    /// 65x65.
    G65,
    /// 81x81.
    G81,
    /// 97x97: highest capacity, needs a close and steady camera.
    G97,
}

impl GridSize {
    /// Every supported size, ascending.
    pub const ALL: [GridSize; 5] = [
        GridSize::G33,
        GridSize::G49,
        GridSize::G65,
        GridSize::G81,
        GridSize::G97,
    ];

    /// Cells per side.
    #[must_use]
    pub const fn modules(self) -> usize {
        match self {
            GridSize::G33 => 33,
            GridSize::G49 => 49,
            GridSize::G65 => 65,
            GridSize::G81 => 81,
            GridSize::G97 => 97,
        }
    }

    /// The 4-bit code written into the frame header.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            GridSize::G33 => 0,
            GridSize::G49 => 1,
            GridSize::G65 => 2,
            GridSize::G81 => 3,
            GridSize::G97 => 4,
        }
    }

    /// Parse a header grid code.
    #[must_use]
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(GridSize::G33),
            1 => Some(GridSize::G49),
            2 => Some(GridSize::G65),
            3 => Some(GridSize::G81),
            4 => Some(GridSize::G97),
            _ => None,
        }
    }

    /// The size with exactly this many cells per side, if it is supported.
    #[must_use]
    pub const fn from_modules(modules: usize) -> Option<Self> {
        match modules {
            33 => Some(GridSize::G33),
            49 => Some(GridSize::G49),
            65 => Some(GridSize::G65),
            81 => Some(GridSize::G81),
            97 => Some(GridSize::G97),
            _ => None,
        }
    }

    /// The supported size closest to a measured cell count.
    #[must_use]
    pub fn nearest(modules: f64) -> Self {
        let mut best = GridSize::G49;
        let mut best_d = f64::INFINITY;
        for g in GridSize::ALL {
            let d = (g.modules() as f64 - modules).abs();
            if d < best_d {
                best_d = d;
                best = g;
            }
        }
        best
    }
}

/// What a cell is used for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Structural: a fixed colour identical in every frame.
    Fixed,
    /// One of the eight colour calibration patches.
    Palette,
    /// Carries one bit of the error-corrected frame header.
    Header,
    /// Carries payload bits.
    Data,
}

/// Precomputed geometry for one grid size.
#[derive(Debug, Clone)]
pub struct Layout {
    size: GridSize,
    n: usize,
    roles: Vec<Role>,
    /// Colour code for structural cells, indexed like `roles`.
    fixed: Vec<u8>,
    header_cells: Vec<u32>,
    data_cells: Vec<u32>,
}

/// Colour code for solid black in the 3-bit RGB encoding.
pub const BLACK: u8 = 0b000;
/// Colour code for solid white in the 3-bit RGB encoding.
pub const WHITE: u8 = 0b111;

impl Layout {
    /// Build the layout for a grid size. Cheap enough to call per frame, but
    /// worth caching in a hot render loop.
    #[must_use]
    pub fn new(size: GridSize) -> Self {
        let n = size.modules();
        let mut roles = vec![Role::Data; n * n];
        let mut fixed = vec![BLACK; n * n];

        let idx = |r: usize, c: usize| r * n + c;

        // --- Four 7x7 finder patterns with their separators -----------------
        // The reserved block is 8x8: the 7x7 pattern plus a one-cell light
        // separator on the two inner sides, which stops payload cells from
        // extending the pattern's run lengths and confusing the detector.
        //
        // All four corners carry the *same* pattern so that all four are found
        // by the same strong 1:1:3:1:1 scan. Four points is what a projective
        // transform needs; predicting the fourth corner from the other three
        // assumes a parallelogram, and a perspective view is not one.
        //
        // Identical markers leave the frame's rotation ambiguous. That is
        // resolved downstream by trying all four and letting the CRC-protected
        // header arbitrate - which is both cheaper and stricter than any
        // geometric tie-break, and comes with the bonus that a frame decodes
        // upside down.
        for (r0, c0, fr, fc) in [
            (0usize, 0usize, 0usize, 0usize), // top left
            (0, n - 8, 0, n - 7),             // top right
            (n - 8, 0, n - 7, 0),             // bottom left
            (n - 8, n - 8, n - 7, n - 7),     // bottom right
        ] {
            for r in r0..r0 + 8 {
                for c in c0..c0 + 8 {
                    roles[idx(r, c)] = Role::Fixed;
                    fixed[idx(r, c)] = WHITE; // separator default
                }
            }
            for dr in 0..7 {
                for dc in 0..7 {
                    let ring = dr.min(dc).min(6 - dr).min(6 - dc);
                    let dark = ring == 0 || ring >= 2;
                    fixed[idx(fr + dr, fc + dc)] = if dark { BLACK } else { WHITE };
                }
            }
        }

        // --- Timing rulers --------------------------------------------------
        // Alternating cells give the decoder a way to check its grid estimate
        // against the image without trusting the header first.
        for c in 8..=(n - 9) {
            roles[idx(6, c)] = Role::Fixed;
            fixed[idx(6, c)] = if c % 2 == 0 { BLACK } else { WHITE };
        }
        for r in 8..=(n - 9) {
            roles[idx(r, 6)] = Role::Fixed;
            fixed[idx(r, 6)] = if r % 2 == 0 { BLACK } else { WHITE };
        }

        // --- Colour calibration patches ------------------------------------
        // Eight 2x2 patches spanning the RGB corners, centred along the top
        // edge between the two upper finders. Sampling these every frame is
        // what lets the decoder survive auto-white-balance and glare: the
        // thresholds are re-derived from the frame in front of it rather than
        // assumed.
        let px = (n - 8) / 2;
        for by in 0..2 {
            for bx in 0..4 {
                let value = (by * 4 + bx) as u8;
                for dr in 0..2 {
                    for dc in 0..2 {
                        let r = by * 2 + dr;
                        let c = px + bx * 2 + dc;
                        roles[idx(r, c)] = Role::Palette;
                        fixed[idx(r, c)] = value;
                    }
                }
            }
        }

        // --- Header then data, in raster order ------------------------------
        let mut header_cells = Vec::with_capacity(HEADER_CELLS);
        let mut data_cells = Vec::new();
        for r in 0..n {
            for c in 0..n {
                if roles[idx(r, c)] == Role::Data {
                    if header_cells.len() < HEADER_CELLS {
                        roles[idx(r, c)] = Role::Header;
                        header_cells.push(idx(r, c) as u32);
                    } else {
                        data_cells.push(idx(r, c) as u32);
                    }
                }
            }
        }

        Self {
            size,
            n,
            roles,
            fixed,
            header_cells,
            data_cells,
        }
    }

    /// The grid size this layout describes.
    #[must_use]
    pub const fn size(&self) -> GridSize {
        self.size
    }

    /// Cells per side.
    #[must_use]
    pub const fn modules(&self) -> usize {
        self.n
    }

    /// Role of the cell at `(row, col)`.
    #[must_use]
    pub fn role(&self, row: usize, col: usize) -> Role {
        self.roles[row * self.n + col]
    }

    /// Structural colour code at `(row, col)`.
    #[must_use]
    pub fn fixed_color(&self, row: usize, col: usize) -> u8 {
        self.fixed[row * self.n + col]
    }

    /// Linear indices of the header cells, in transmission order.
    #[must_use]
    pub fn header_cells(&self) -> &[u32] {
        &self.header_cells
    }

    /// Linear indices of the data cells, in transmission order.
    #[must_use]
    pub fn data_cells(&self) -> &[u32] {
        &self.data_cells
    }

    /// Number of payload-carrying cells.
    #[must_use]
    pub fn data_capacity_cells(&self) -> usize {
        self.data_cells.len()
    }

    /// Linear indices of the eight palette patches, one representative cell
    /// per colour, paired with the colour code they display.
    #[must_use]
    pub fn palette_cells(&self) -> Vec<(u8, Vec<u32>)> {
        let mut out: Vec<(u8, Vec<u32>)> = (0..8).map(|v| (v as u8, Vec::new())).collect();
        for r in 0..self.n {
            for c in 0..self.n {
                if self.role(r, c) == Role::Palette {
                    let v = self.fixed_color(r, c) as usize;
                    out[v].1.push((r * self.n + c) as u32);
                }
            }
        }
        out
    }

    /// Grid-space centres of the four fiducial markers, in clockwise order
    /// starting at the top left: `[top-left, top-right, bottom-right,
    /// bottom-left]`.
    ///
    /// These are the source points for the homography; the detector supplies
    /// the matching image-space points. The winding matters: the detector
    /// orders the points it found to match this one.
    #[must_use]
    pub fn marker_points(&self) -> [(f64, f64); 4] {
        let n = self.n as f64;
        [
            (3.5, 3.5),         // top left
            (n - 3.5, 3.5),     // top right
            (n - 3.5, n - 3.5), // bottom right
            (3.5, n - 3.5),     // bottom left
        ]
    }

    /// Distance in cells between the top-left and top-right finder centres.
    /// The decoder divides the measured pixel distance by this to estimate the
    /// cell size.
    #[must_use]
    pub const fn finder_span_cells(&self) -> usize {
        self.n - 7
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_grid_size_builds() {
        for g in GridSize::ALL {
            let l = Layout::new(g);
            assert_eq!(l.modules(), g.modules());
            assert_eq!(l.header_cells().len(), HEADER_CELLS);
            assert!(l.data_capacity_cells() > 0, "{g:?} has no data cells");
        }
    }

    #[test]
    fn grid_codes_round_trip() {
        for g in GridSize::ALL {
            assert_eq!(GridSize::from_code(g.code()), Some(g));
        }
        assert!(GridSize::from_code(9).is_none());
    }

    #[test]
    fn nearest_snaps_correctly_with_wide_tolerance() {
        assert_eq!(GridSize::nearest(33.0), GridSize::G33);
        assert_eq!(GridSize::nearest(40.0), GridSize::G33); // 7 away from 33, 9 from 49
        assert_eq!(GridSize::nearest(42.0), GridSize::G49); // 9 away from 33, 7 from 49
        assert_eq!(GridSize::nearest(46.0), GridSize::G49);
        assert_eq!(GridSize::nearest(52.0), GridSize::G49);
        assert_eq!(GridSize::nearest(1000.0), GridSize::G97);
        assert_eq!(GridSize::nearest(0.0), GridSize::G33);
    }

    #[test]
    fn roles_partition_the_grid() {
        for g in GridSize::ALL {
            let l = Layout::new(g);
            let n = l.modules();
            let mut counts = [0usize; 4];
            for r in 0..n {
                for c in 0..n {
                    counts[match l.role(r, c) {
                        Role::Fixed => 0,
                        Role::Palette => 1,
                        Role::Header => 2,
                        Role::Data => 3,
                    }] += 1;
                }
            }
            assert_eq!(counts.iter().sum::<usize>(), n * n);
            assert_eq!(counts[1], 32, "{g:?}: expected 8 patches of 2x2");
            assert_eq!(counts[2], HEADER_CELLS);
            assert_eq!(counts[3], l.data_capacity_cells());
        }
    }

    #[test]
    fn finder_patterns_have_the_expected_cross_section() {
        let l = Layout::new(GridSize::G49);
        // Scanning through the centre of the top-left finder must give
        // dark, light, dark x3, light, dark.
        let expect = [BLACK, WHITE, BLACK, BLACK, BLACK, WHITE, BLACK, WHITE];
        for (c, want) in expect.iter().enumerate() {
            assert_eq!(
                l.fixed_color(3, c),
                *want,
                "top-left finder row 3 column {c}"
            );
        }
    }

    #[test]
    fn all_four_corners_carry_the_same_finder() {
        let l = Layout::new(GridSize::G49);
        let n = l.modules();
        // The 1:1:3:1:1 cross-section, read through each finder's centre.
        let expect = [BLACK, WHITE, BLACK, BLACK, BLACK, WHITE, BLACK];
        for (i, want) in expect.iter().enumerate() {
            assert_eq!(l.fixed_color(3, i), *want, "top-left at {i}");
            assert_eq!(l.fixed_color(3, n - 7 + i), *want, "top-right at {i}");
            assert_eq!(l.fixed_color(n - 4, i), *want, "bottom-left at {i}");
            assert_eq!(
                l.fixed_color(n - 4, n - 7 + i),
                *want,
                "bottom-right at {i}"
            );
        }
    }

    #[test]
    fn marker_points_are_wound_clockwise() {
        for g in GridSize::ALL {
            let l = Layout::new(g);
            let p = l.marker_points();
            let mut area = 0.0;
            for i in 0..4 {
                let a = p[i];
                let b = p[(i + 1) % 4];
                area += a.0 * b.1 - b.0 * a.1;
            }
            assert!(area > 0.0, "{g:?}: marker winding is inverted");
        }
    }

    #[test]
    fn separators_surround_the_finders() {
        let l = Layout::new(GridSize::G49);
        for c in 0..8 {
            assert_eq!(l.fixed_color(7, c), WHITE, "separator row at {c}");
        }
        for r in 0..8 {
            assert_eq!(l.fixed_color(r, 7), WHITE, "separator column at {r}");
        }
    }

    #[test]
    fn timing_rulers_alternate() {
        let l = Layout::new(GridSize::G49);
        for c in 8..=(l.modules() - 9) {
            assert_eq!(l.role(6, c), Role::Fixed);
            let want = if c % 2 == 0 { BLACK } else { WHITE };
            assert_eq!(l.fixed_color(6, c), want, "timing row at {c}");
        }
    }

    #[test]
    fn palette_shows_all_eight_colours() {
        for g in GridSize::ALL {
            let l = Layout::new(g);
            let patches = l.palette_cells();
            assert_eq!(patches.len(), 8);
            for (value, cells) in patches {
                assert_eq!(
                    cells.len(),
                    4,
                    "{g:?}: colour {value} should be a 2x2 patch"
                );
            }
        }
    }

    #[test]
    fn palette_does_not_collide_with_the_finders() {
        for g in GridSize::ALL {
            let l = Layout::new(g);
            let n = l.modules();
            // The palette band occupies rows 0..4; the finders own columns
            // 0..8 and n-8..n there.
            for r in 0..4 {
                for c in 0..8 {
                    assert_ne!(l.role(r, c), Role::Palette, "{g:?}: palette hit TL");
                }
                for c in (n - 8)..n {
                    assert_ne!(l.role(r, c), Role::Palette, "{g:?}: palette hit TR");
                }
            }
        }
    }

    #[test]
    fn header_cells_precede_data_cells_in_raster_order() {
        let l = Layout::new(GridSize::G65);
        let last_header = *l.header_cells().last().unwrap();
        let first_data = l.data_cells()[0];
        assert!(last_header < first_data);
        // Both lists must be strictly ascending and disjoint.
        assert!(l.header_cells().windows(2).all(|w| w[0] < w[1]));
        assert!(l.data_cells().windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn marker_points_sit_at_the_pattern_centres() {
        let l = Layout::new(GridSize::G49);
        let n = l.modules() as f64;
        let pts = l.marker_points();
        assert_eq!(pts[0], (3.5, 3.5));
        assert_eq!(pts[1], (n - 3.5, 3.5));
        assert_eq!(pts[3], (3.5, n - 3.5));
        // The centre cell of each finder must actually be dark.
        assert_eq!(l.fixed_color(3, 3), BLACK);
        assert_eq!(l.fixed_color(3, l.modules() - 4), BLACK);
        assert_eq!(l.fixed_color(l.modules() - 4, 3), BLACK);
        assert_eq!(l.fixed_color(l.modules() - 3, l.modules() - 3), BLACK);
    }

    #[test]
    fn capacity_grows_with_grid_size() {
        let mut previous = 0;
        for g in GridSize::ALL {
            let capacity = Layout::new(g).data_capacity_cells();
            assert!(capacity > previous, "{g:?} did not increase capacity");
            previous = capacity;
        }
    }
}
