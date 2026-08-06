//! Recovering a message from captured frames.
//!
//! The decoder is fed images one at a time and keeps state between them. Each
//! image goes through: locate the frame, un-warp it, sample every cell,
//! calibrate the colours, repair the header, repair the data, verify the frame
//! checksum, and hand the droplet to the fountain decoder. Any step may fail,
//! and failure is entirely ordinary - most captured frames of a moving phone
//! are unusable, which is exactly why the code is rateless.

use crate::color::{mean_color, pack_bits, Calibration, ColorMode};
use crate::encoder::Frame;
use crate::frame::FrameProfile;
use crate::header::{FrameHeader, HEADER_CODE_BYTES};
use crate::layout::{GridSize, Layout, HEADER_CELLS};
use crate::PzError;
use alloc::vec;
use alloc::vec::Vec;
use pz_fec::crc32;
use pz_fountain::SolitonParams;
use pz_vision::{
    adaptive_threshold, default_window, find_corner_marker, find_finder_patterns, order_finders,
    sample_cell, FinderPattern, Homography, Point, RgbView,
};

/// Cells whose confidence falls below this are reported to Reed-Solomon as
/// erasures.
///
/// Erasures cost half as much as errors, so being generous here pays off - but
/// only up to the parity budget, which [`FrameProfile::decode_symbols`]
/// enforces by keeping the least confident hints and discarding the rest.
pub const DEFAULT_ERASURE_THRESHOLD: f64 = 0.28;

/// Fraction of a cell sampled when reading its colour. Staying away from the
/// edges is what stops a neighbouring cell bleeding in after an imperfect fit.
const SAMPLE_EXTENT: f64 = 0.28;

/// Sub-samples per axis within a cell.
const SAMPLE_OVERSAMPLE: usize = 3;

/// A frame located in an image.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Detection {
    /// Grid size this detection assumes.
    pub grid: GridSize,
    /// Transform from grid coordinates to image coordinates.
    pub homography: Homography,
    /// Estimated cell size in pixels.
    pub module_px: f64,
    /// Whether the fourth corner marker was actually confirmed, as opposed to
    /// being predicted affinely from the other three. A predicted corner still
    /// decodes at modest viewing angles but loses true perspective correction.
    pub corner_confirmed: bool,
}

/// A frame that decoded successfully.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedFrame {
    /// The frame's header.
    pub header: FrameHeader,
    /// The capacity plan implied by the header.
    pub profile: FrameProfile,
    /// The fountain droplet this frame carried.
    pub droplet: Vec<u8>,
}

/// What ingesting an image achieved.
#[derive(Debug, Clone, PartialEq)]
pub enum Progress {
    /// No PZ frame could be located.
    NotFound,
    /// A frame was located but could not be decoded, or belonged to another
    /// session. Utterly routine while a camera settles.
    Rejected,
    /// A droplet was absorbed and the message is still incomplete.
    Progressed {
        /// Session being received.
        session_id: u16,
        /// Index of the frame just absorbed.
        frame_index: u32,
        /// Source blocks recovered so far.
        recovered: usize,
        /// Source blocks in the message.
        total: usize,
    },
    /// The message is complete and verified.
    Complete(Vec<u8>),
}

impl Progress {
    /// Fraction of the message recovered, in `[0, 1]`.
    #[must_use]
    pub fn fraction(&self) -> f64 {
        match self {
            Progress::Complete(_) => 1.0,
            Progress::Progressed {
                recovered, total, ..
            } => {
                if *total == 0 {
                    0.0
                } else {
                    *recovered as f64 / *total as f64
                }
            }
            _ => 0.0,
        }
    }
}

/// Decode a frame that has already been sampled into per-cell colours.
///
/// This is the half of the pipeline with no computer vision in it: given the
/// colours, recover the bytes. Useful directly when the cells are already
/// known, and the seam at which the protocol can be tested without a camera.
///
/// # Errors
/// Returns [`PzError::HeaderCorrupt`], [`PzError::FrameCorrupt`] or
/// [`PzError::ChecksumMismatch`] depending on how far it got.
pub fn decode_sampled(
    grid: GridSize,
    colors: &[[u8; 3]],
    erasure_threshold: f64,
) -> Result<DecodedFrame, PzError> {
    let layout = Layout::new(grid);
    let n = layout.modules();
    if colors.len() != n * n {
        return Err(PzError::WrongLength);
    }

    // --- Calibrate against this frame's own reference patches --------------
    let mut patches = [[0.0f64; 3]; 8];
    for (value, cells) in layout.palette_cells() {
        let samples: Vec<[u8; 3]> = cells.iter().map(|&i| colors[i as usize]).collect();
        patches[value as usize] = mean_color(&samples);
    }
    let calibration = Calibration::from_patches(patches);

    // --- Header ------------------------------------------------------------
    let mut header_bits = Vec::with_capacity(HEADER_CELLS);
    let mut bit_confidence = Vec::with_capacity(HEADER_CELLS);
    for &cell in layout.header_cells() {
        let reading = calibration.demodulate(ColorMode::Mono, colors[cell as usize]);
        header_bits.push(reading.value);
        bit_confidence.push(reading.confidence);
    }
    let header_bytes = pack_bits(&header_bits, 1, HEADER_CODE_BYTES);

    let mut header_erasures: Vec<usize> = Vec::new();
    for byte in 0..HEADER_CODE_BYTES {
        let worst = bit_confidence[byte * 8..(byte + 1) * 8]
            .iter()
            .copied()
            .fold(f64::INFINITY, f64::min);
        if worst < erasure_threshold {
            header_erasures.push(byte);
        }
    }
    // RS(32,16) pays for at most n - k = 16 erasures. Declaring more than the
    // budget guarantees failure even when the header is repairable, so past
    // that point the hints are worse than useless and are dropped entirely.
    if header_erasures.len() > HEADER_CODE_BYTES - crate::header::HEADER_BYTES {
        header_erasures.clear();
    }

    let header = FrameHeader::decode_protected(&header_bytes, &header_erasures)?;
    if header.grid != grid {
        // The header parsed but describes a different geometry, so our
        // sampling grid was wrong and every data cell is misaligned.
        return Err(PzError::UnsupportedFormat);
    }

    // --- Data ---------------------------------------------------------------
    let profile = FrameProfile::new(header.grid, header.mode, header.parity_code)?;
    let bits_per_cell = header.mode.bits_per_cell();

    let data_cells = layout.data_cells();
    let mut values = Vec::with_capacity(data_cells.len());
    let mut cell_confidence = Vec::with_capacity(data_cells.len());
    for &cell in data_cells {
        let reading = calibration.demodulate(header.mode, colors[cell as usize]);
        if reading.usable {
            values.push(reading.value);
            cell_confidence.push(reading.confidence);
        } else {
            // An Rgb4 parity violation: the value is meaningless, but knowing
            // it is meaningless is worth more than a guess.
            values.push(0);
            cell_confidence.push(0.0);
        }
    }

    let coded = profile.coded_symbols();
    let symbols = pack_bits(&values, bits_per_cell, coded);

    // A symbol is only as trustworthy as the least trustworthy cell that
    // contributed a bit to it.
    let mut symbol_confidence = vec![1.0f64; coded];
    for (cell_index, &confidence) in cell_confidence.iter().enumerate() {
        let first_bit = cell_index * bits_per_cell;
        let last_bit = first_bit + bits_per_cell - 1;
        let lo = first_bit / 8;
        if lo >= coded {
            // Past the coded region: these cells are padding.
            break;
        }
        let hi = (last_bit / 8).min(coded - 1);
        for slot in &mut symbol_confidence[lo..=hi] {
            *slot = slot.min(confidence);
        }
    }

    let payload = profile.decode_symbols(&symbols, &symbol_confidence, erasure_threshold)?;
    let droplet = profile.open_payload(&payload, header.frame_index)?;

    Ok(DecodedFrame {
        header,
        profile,
        droplet,
    })
}

/// Choose the three finder candidates that best form an isosceles right
/// triangle, which is what three corners of a square must look like under any
/// perspective a camera can produce.
fn best_triple(patterns: &[FinderPattern]) -> Vec<FinderPattern> {
    if patterns.len() <= 3 {
        return patterns.to_vec();
    }
    // Cap the search; more than a handful of candidates means the image is
    // noise anyway.
    let limit = patterns.len().min(8);
    let mut best: Option<Vec<FinderPattern>> = None;
    let mut best_score = f64::INFINITY;

    for i in 0..limit {
        for j in (i + 1)..limit {
            for k in (j + 1)..limit {
                let triple = [patterns[i], patterns[j], patterns[k]];
                let Some((tl, tr, bl)) = order_finders(&triple) else {
                    continue;
                };
                let d1 = tl.center.dist(tr.center);
                let d2 = tl.center.dist(bl.center);
                if d1 < 2.0 || d2 < 2.0 {
                    continue;
                }
                // Two legs of equal length, meeting at a right angle.
                let leg_mismatch = (d1 - d2).abs() / d1.max(d2);
                let v1 = (tr.center.x - tl.center.x, tr.center.y - tl.center.y);
                let v2 = (bl.center.x - tl.center.x, bl.center.y - tl.center.y);
                let squareness = (v1.0 * v2.0 + v1.1 * v2.1).abs() / (d1 * d2);
                let score = leg_mismatch + squareness;
                if score < best_score {
                    best_score = score;
                    best = Some(triple.to_vec());
                }
            }
        }
    }
    best.unwrap_or_else(|| patterns[..3].to_vec())
}

/// Pick the four finder candidates that best form a square under perspective.
///
/// Scored by how close the quadrilateral is to a parallelogram with equal
/// diagonals, which any square remains under a mild projective transform.
fn best_quad(patterns: &[FinderPattern]) -> Option<[FinderPattern; 4]> {
    if patterns.len() < 4 {
        return None;
    }
    if patterns.len() == 4 {
        return Some([patterns[0], patterns[1], patterns[2], patterns[3]]);
    }

    let limit = patterns.len().min(8);
    let mut best: Option<[FinderPattern; 4]> = None;
    let mut best_score = f64::INFINITY;

    for a in 0..limit {
        for b in (a + 1)..limit {
            for c in (b + 1)..limit {
                for d in (c + 1)..limit {
                    let group = [patterns[a], patterns[b], patterns[c], patterns[d]];
                    let pts: Vec<Point> = group.iter().map(|p| p.center).collect();
                    let Some(quad) = pz_vision::order_quad(&pts) else {
                        continue;
                    };
                    let area = pz_vision::signed_area(&quad).abs();
                    if area < 16.0 {
                        continue;
                    }
                    // Opposite sides should match, and so should the diagonals.
                    let s0 = quad[0].dist(quad[1]);
                    let s1 = quad[1].dist(quad[2]);
                    let s2 = quad[2].dist(quad[3]);
                    let s3 = quad[3].dist(quad[0]);
                    let d0 = quad[0].dist(quad[2]);
                    let d1 = quad[1].dist(quad[3]);
                    let longest = s0.max(s1).max(s2).max(s3).max(1e-6);
                    let score = ((s0 - s2).abs() + (s1 - s3).abs() + (d0 - d1).abs()) / longest;
                    if score < best_score {
                        best_score = score;
                        best = Some(group);
                    }
                }
            }
        }
    }
    best
}

/// Build detections from four ordered corner points.
fn detections_from_quad(quad: [Point; 4], module: f64, confirmed: bool) -> Vec<Detection> {
    // Adjacent finder centres are (N - 7) cells apart.
    let perimeter = quad[0].dist(quad[1])
        + quad[1].dist(quad[2])
        + quad[2].dist(quad[3])
        + quad[3].dist(quad[0]);
    let estimated_modules = perimeter / 4.0 / module + 7.0;

    let mut candidates = GridSize::ALL;
    candidates.sort_by(|a, b| {
        let da = (a.modules() as f64 - estimated_modules).abs();
        let db = (b.modules() as f64 - estimated_modules).abs();
        da.partial_cmp(&db).unwrap_or(core::cmp::Ordering::Equal)
    });

    let mut out = Vec::new();
    for grid in candidates.iter().take(2) {
        let layout = Layout::new(*grid);
        let m = layout.marker_points();
        let src = [
            Point::new(m[0].0, m[0].1),
            Point::new(m[1].0, m[1].1),
            Point::new(m[2].0, m[2].1),
            Point::new(m[3].0, m[3].1),
        ];
        // All four markers look alike, so which image corner is the frame's
        // top left is unknown. Try every rotation; the header CRC decides.
        for rotation in 0..4 {
            let dst = [
                quad[rotation % 4],
                quad[(rotation + 1) % 4],
                quad[(rotation + 2) % 4],
                quad[(rotation + 3) % 4],
            ];
            if let Some(homography) = Homography::from_correspondences(&src, &dst) {
                out.push(Detection {
                    grid: *grid,
                    homography,
                    module_px: module,
                    corner_confirmed: confirmed,
                });
            }
        }
    }
    out
}

/// Locate a PZ frame in an image.
///
/// Returns candidate detections, most likely first. Several are returned
/// because neither the grid size nor the frame's rotation can be settled from
/// the markers alone; the caller tries them in order and lets the header
/// checksum arbitrate, which is a far stricter test than any geometric
/// heuristic.
#[must_use]
pub fn detect(img: &RgbView<'_>) -> Vec<Detection> {
    if img.width < 32 || img.height < 32 {
        return Vec::new();
    }
    let gray = img.to_gray();
    let window = default_window(img.width, img.height);
    let binary = adaptive_threshold(&gray, window, 6);

    let patterns = find_finder_patterns(&binary);
    if patterns.len() < 3 {
        return Vec::new();
    }

    // Preferred path: all four corners found, giving a true homography.
    if let Some(group) = best_quad(&patterns) {
        let module = group.iter().map(|p| p.module).sum::<f64>() / 4.0;
        if module >= 0.75 {
            let pts: Vec<Point> = group.iter().map(|p| p.center).collect();
            if let Some(quad) = pz_vision::order_quad(&pts) {
                let detections = detections_from_quad(quad, module, true);
                if !detections.is_empty() {
                    return detections;
                }
            }
        }
    }

    // Fallback: one corner is occluded or was missed. Three points can only
    // support an affine fit, so the fourth is predicted as the parallelogram
    // corner and refined locally if the small search finds it. This decodes a
    // near-flat capture but degrades as the viewing angle grows, because a
    // perspective view of a square is not a parallelogram.
    let triple = best_triple(&patterns);
    let Some((tl, tr, bl)) = order_finders(&triple) else {
        return Vec::new();
    };
    let module = (tl.module + tr.module + bl.module) / 3.0;
    if module < 0.75 {
        return Vec::new();
    }

    let predicted = Point::new(
        tr.center.x + bl.center.x - tl.center.x,
        tr.center.y + bl.center.y - tl.center.y,
    );
    let found = find_corner_marker(&binary, predicted, module * 6.0);
    let confirmed = found.is_some();
    let br = found.unwrap_or(predicted);

    let span_px = (tl.center.dist(tr.center) + tl.center.dist(bl.center)) / 2.0;
    let estimated_modules = span_px / module + 7.0;
    let mut candidates = GridSize::ALL;
    candidates.sort_by(|a, b| {
        let da = (a.modules() as f64 - estimated_modules).abs();
        let db = (b.modules() as f64 - estimated_modules).abs();
        da.partial_cmp(&db).unwrap_or(core::cmp::Ordering::Equal)
    });

    let mut out = Vec::new();
    for grid in candidates.iter().take(3) {
        let layout = Layout::new(*grid);
        let m = layout.marker_points();
        let src = [
            Point::new(m[0].0, m[0].1),
            Point::new(m[1].0, m[1].1),
            Point::new(m[2].0, m[2].1),
            Point::new(m[3].0, m[3].1),
        ];
        let dst = [tl.center, tr.center, br, bl.center];
        if let Some(homography) = Homography::from_correspondences(&src, &dst) {
            out.push(Detection {
                grid: *grid,
                homography,
                module_px: module,
                corner_confirmed: confirmed,
            });
        }
    }
    out
}

/// Read every cell of a detected frame.
#[must_use]
pub fn sample_frame(img: &RgbView<'_>, detection: &Detection) -> Vec<[u8; 3]> {
    let n = detection.grid.modules();
    let mut out = Vec::with_capacity(n * n);
    for row in 0..n {
        for col in 0..n {
            out.push(sample_cell(
                img,
                &detection.homography,
                col as f64 + 0.5,
                row as f64 + 0.5,
                SAMPLE_EXTENT,
                SAMPLE_OVERSAMPLE,
            ));
        }
    }
    out
}

struct Session {
    id: u16,
    profile: FrameProfile,
    container_len: usize,
    fountain: pz_fountain::Decoder,
}

/// Accumulates frames until the message is complete.
#[derive(Default)]
pub struct Decoder {
    session: Option<Session>,
    soliton: SolitonParams,
    erasure_threshold: f64,
    frames_seen: usize,
    frames_accepted: usize,
    finished: Option<Vec<u8>>,
}

impl core::fmt::Debug for Decoder {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Decoder")
            .field("session_id", &self.session.as_ref().map(|s| s.id))
            .field("frames_seen", &self.frames_seen)
            .field("frames_accepted", &self.frames_accepted)
            .field("finished", &self.finished.is_some())
            .finish()
    }
}

impl Decoder {
    /// A decoder using the standard fountain parameters.
    #[must_use]
    pub fn new() -> Self {
        Self {
            session: None,
            soliton: SolitonParams::default(),
            erasure_threshold: DEFAULT_ERASURE_THRESHOLD,
            frames_seen: 0,
            frames_accepted: 0,
            finished: None,
        }
    }

    /// A decoder for a transmitter using non-standard fountain parameters.
    ///
    /// The degree distribution is not carried in the frame header, so both
    /// ends must be configured identically. Version 1 of the wire format fixes
    /// these at their defaults; anything else is experimental and will not
    /// interoperate with other implementations.
    #[must_use]
    pub fn with_soliton(soliton: SolitonParams) -> Self {
        Self {
            soliton,
            ..Self::new()
        }
    }

    /// Override the confidence below which a cell is treated as an erasure.
    #[must_use]
    pub fn with_erasure_threshold(mut self, threshold: f64) -> Self {
        self.erasure_threshold = threshold;
        self
    }

    /// Session id being received, once one has been locked on to.
    #[must_use]
    pub fn session_id(&self) -> Option<u16> {
        self.session.as_ref().map(|s| s.id)
    }

    /// Images or frames offered so far.
    #[must_use]
    pub const fn frames_seen(&self) -> usize {
        self.frames_seen
    }

    /// Frames that decoded and were absorbed.
    #[must_use]
    pub const fn frames_accepted(&self) -> usize {
        self.frames_accepted
    }

    /// Fraction of the message recovered, in `[0, 1]`.
    #[must_use]
    pub fn progress(&self) -> f64 {
        if self.finished.is_some() {
            return 1.0;
        }
        self.session.as_ref().map_or(0.0, |s| s.fountain.progress())
    }

    /// Source blocks recovered so far.
    ///
    /// Equals [`Self::total`] once decoding has finished, and is zero before a
    /// session has been locked on to.
    #[must_use]
    pub fn recovered(&self) -> usize {
        self.session.as_ref().map_or(0, |s| s.fountain.recovered())
    }

    /// Source blocks in the message, or zero before a session has been locked
    /// on to.
    #[must_use]
    pub fn total(&self) -> usize {
        self.session
            .as_ref()
            .map_or(0, |s| s.fountain.block_count())
    }

    /// The completed message, if decoding has finished.
    #[must_use]
    pub fn result(&self) -> Option<&[u8]> {
        self.finished.as_deref()
    }

    /// Forget the current session so a new transmission can be received.
    ///
    /// Both counters clear, not just one. They describe the reception in
    /// progress, and leaving `frames_seen` running while `frames_accepted`
    /// restarts makes the pair read as an implausible acceptance rate.
    pub fn reset(&mut self) {
        self.session = None;
        self.finished = None;
        self.frames_accepted = 0;
        self.frames_seen = 0;
    }

    /// Offer a captured image.
    ///
    /// Returns [`Progress::NotFound`] when no frame is visible and
    /// [`Progress::Rejected`] when one is visible but unusable; neither is an
    /// error.
    ///
    /// # Errors
    /// Only propagates failures that indicate a genuine protocol problem, such
    /// as a payload larger than this build supports.
    pub fn ingest_image(&mut self, img: &RgbView<'_>) -> Result<Progress, PzError> {
        self.frames_seen += 1;
        let detections = detect(img);
        if detections.is_empty() {
            return Ok(Progress::NotFound);
        }
        for detection in &detections {
            let colors = sample_frame(img, detection);
            match decode_sampled(detection.grid, &colors, self.erasure_threshold) {
                Ok(frame) => return self.absorb(frame),
                Err(_) => continue,
            }
        }
        Ok(Progress::Rejected)
    }

    /// Offer a frame directly, skipping detection and sampling.
    ///
    /// This is the ideal-channel path: useful for tests, for transports that
    /// already know the cell values, and for measuring the protocol
    /// independently of the optics.
    ///
    /// # Errors
    /// See [`Decoder::ingest_image`].
    pub fn ingest_frame(&mut self, frame: &Frame) -> Result<Progress, PzError> {
        self.frames_seen += 1;
        let Some(grid) = GridSize::from_modules(frame.modules()) else {
            return Ok(Progress::NotFound);
        };
        match decode_sampled(grid, &frame.to_colors(), self.erasure_threshold) {
            Ok(decoded) => self.absorb(decoded),
            Err(_) => Ok(Progress::Rejected),
        }
    }

    /// Offer already-sampled cell colours.
    ///
    /// # Errors
    /// See [`Decoder::ingest_image`].
    pub fn ingest_colors(
        &mut self,
        grid: GridSize,
        colors: &[[u8; 3]],
    ) -> Result<Progress, PzError> {
        self.frames_seen += 1;
        match decode_sampled(grid, colors, self.erasure_threshold) {
            Ok(decoded) => self.absorb(decoded),
            Err(_) => Ok(Progress::Rejected),
        }
    }

    /// Absorb a decoded frame into the running session.
    ///
    /// # Errors
    /// Returns [`PzError::PayloadTooLarge`] if the header announces a message
    /// this build will not allocate for.
    pub fn absorb(&mut self, decoded: DecodedFrame) -> Result<Progress, PzError> {
        if let Some(done) = &self.finished {
            return Ok(Progress::Complete(done.clone()));
        }

        // This is the check that makes the fountain layer's session-agnostic
        // systematic prefix safe: a frame from another transmitter never
        // reaches the fountain decoder.
        if let Some(session) = &self.session {
            if session.id != decoded.header.session_id {
                return Ok(Progress::Rejected);
            }
        } else {
            let container_len = decoded.header.payload_len as usize;
            if container_len == 0 || container_len > crate::MAX_PAYLOAD_BYTES {
                return Err(PzError::PayloadTooLarge);
            }
            let droplet_size = decoded.profile.droplet_size();
            let block_count = container_len.div_ceil(droplet_size);
            let fountain = pz_fountain::Decoder::new(
                block_count,
                droplet_size,
                container_len,
                u32::from(decoded.header.session_id),
                self.soliton,
            )?;
            self.session = Some(Session {
                id: decoded.header.session_id,
                profile: decoded.profile,
                container_len,
                fountain,
            });
        }

        let session = self.session.as_mut().expect("session was just installed");

        // A frame whose geometry disagrees with the session's would produce
        // droplets of the wrong size.
        if decoded.droplet.len() != session.profile.droplet_size() {
            return Ok(Progress::Rejected);
        }

        session
            .fountain
            .absorb(decoded.header.frame_index, &decoded.droplet)?;
        self.frames_accepted += 1;

        if session.fountain.is_complete() {
            let container = session.fountain.assemble().ok_or(PzError::FrameCorrupt)?;
            if container.len() < 4 || container.len() != session.container_len {
                return Err(PzError::ChecksumMismatch);
            }
            let stored =
                u32::from_be_bytes([container[0], container[1], container[2], container[3]]);
            let user = container[4..].to_vec();
            if crc32(&user) != stored {
                // The fountain converged on bytes that are not the message.
                // Extremely unlikely, but silently returning them would be far
                // worse than reporting the failure.
                return Err(PzError::ChecksumMismatch);
            }
            self.finished = Some(user.clone());
            return Ok(Progress::Complete(user));
        }

        Ok(Progress::Progressed {
            session_id: session.id,
            frame_index: decoded.header.frame_index,
            recovered: session.fountain.recovered(),
            total: session.fountain.block_count(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoder::{Encoder, EncoderConfig};

    fn transfer(payload: &[u8], config: EncoderConfig) -> (usize, Vec<u8>) {
        let encoder = Encoder::new(payload, config).unwrap();
        let mut decoder = Decoder::new();
        for index in 0..100_000u32 {
            let frame = encoder.frame(index).unwrap();
            if let Progress::Complete(bytes) = decoder.ingest_frame(&frame).unwrap() {
                return (index as usize + 1, bytes);
            }
        }
        panic!("transfer never completed");
    }

    #[test]
    fn round_trips_a_short_message() {
        let payload = b"photonic zero";
        let (frames, out) = transfer(payload, EncoderConfig::default());
        assert_eq!(out, payload);
        assert_eq!(frames, 1, "a short message should fit in one frame");
    }

    #[test]
    fn round_trips_across_every_profile() {
        let payload: Vec<u8> = (0..3000).map(|i| (i * 31 % 251) as u8).collect();
        for config in [
            EncoderConfig::default(),
            EncoderConfig::robust(),
            EncoderConfig::fast(),
            EncoderConfig::resilient(),
        ] {
            let (_, out) = transfer(&payload, config);
            assert_eq!(out, payload, "profile {config:?} failed");
        }
    }

    #[test]
    fn round_trips_every_grid_and_mode_combination() {
        let payload: Vec<u8> = (0..900).map(|i| (i % 97) as u8).collect();
        for grid in GridSize::ALL {
            for mode in [ColorMode::Mono, ColorMode::Rgb4, ColorMode::Rgb8] {
                let config = EncoderConfig {
                    grid,
                    mode,
                    ..EncoderConfig::default()
                };
                let (_, out) = transfer(&payload, config);
                assert_eq!(out, payload, "{grid:?} {mode:?} failed");
            }
        }
    }

    #[test]
    fn recovers_when_most_frames_are_dropped() {
        let payload: Vec<u8> = (0..20_000).map(|i| (i % 253) as u8).collect();
        let encoder = Encoder::new(&payload, EncoderConfig::default()).unwrap();
        let mut decoder = Decoder::new();

        let mut index = 0u32;
        loop {
            // Keep only one frame in four.
            if index % 4 == 0 {
                let frame = encoder.frame(index).unwrap();
                if let Progress::Complete(bytes) = decoder.ingest_frame(&frame).unwrap() {
                    assert_eq!(bytes, payload);
                    break;
                }
            }
            index += 1;
            assert!(index < 200_000, "never converged");
        }
        assert!(decoder.progress() >= 1.0);
    }

    #[test]
    fn frames_from_another_session_are_rejected() {
        let a = Encoder::new(
            b"session A payload that is long enough to need several frames",
            EncoderConfig {
                session_id: Some(0xAAAA),
                grid: GridSize::G33,
                mode: ColorMode::Mono,
                ..EncoderConfig::default()
            },
        )
        .unwrap();
        let b = Encoder::new(
            b"session B payload that is also long enough to need frames!!!",
            EncoderConfig {
                session_id: Some(0xBBBB),
                grid: GridSize::G33,
                mode: ColorMode::Mono,
                ..EncoderConfig::default()
            },
        )
        .unwrap();

        let mut decoder = Decoder::new();
        decoder.ingest_frame(&a.frame(0).unwrap()).unwrap();
        assert_eq!(decoder.session_id(), Some(0xAAAA));

        // Interleave the other session's frames; they must all bounce.
        let before = decoder.frames_accepted();
        for i in 0..5u32 {
            let progress = decoder.ingest_frame(&b.frame(i).unwrap()).unwrap();
            assert_eq!(progress, Progress::Rejected, "frame {i} was not rejected");
        }
        assert_eq!(decoder.frames_accepted(), before);
    }

    #[test]
    fn a_corrupted_frame_is_rejected_not_absorbed() {
        let encoder = Encoder::new(&vec![9u8; 5000], EncoderConfig::default()).unwrap();
        let mut decoder = Decoder::new();

        let frame = encoder.frame(0).unwrap();
        let mut colors = frame.to_colors();
        // Destroy most of the grid.
        for (i, c) in colors.iter_mut().enumerate() {
            if i % 3 != 0 {
                *c = [128, 128, 128];
            }
        }
        let progress = decoder
            .ingest_colors(encoder.config().grid, &colors)
            .unwrap();
        assert_eq!(progress, Progress::Rejected);
        assert_eq!(decoder.frames_accepted(), 0);
    }

    #[test]
    fn tolerates_noise_within_the_correction_budget() {
        let payload: Vec<u8> = (0..2000).map(|i| (i % 199) as u8).collect();
        let encoder = Encoder::new(&payload, EncoderConfig::resilient()).unwrap();
        let mut decoder = Decoder::new();

        let mut index = 0u32;
        loop {
            let frame = encoder.frame(index).unwrap();
            let mut colors = frame.to_colors();
            // Corrupt a scattering of data cells in every frame.
            let layout = encoder.layout();
            for (i, &cell) in layout.data_cells().iter().enumerate() {
                if i % 23 == 0 {
                    colors[cell as usize] = [255, 255, 255];
                }
            }
            if let Progress::Complete(bytes) = decoder
                .ingest_colors(encoder.config().grid, &colors)
                .unwrap()
            {
                assert_eq!(bytes, payload);
                break;
            }
            index += 1;
            assert!(index < 50_000, "never converged under noise");
        }
    }

    #[test]
    fn decoder_reset_allows_a_new_session() {
        let a = Encoder::new(b"first message", EncoderConfig::default()).unwrap();
        let b = Encoder::new(b"second message", EncoderConfig::default()).unwrap();
        let mut decoder = Decoder::new();

        assert!(matches!(
            decoder.ingest_frame(&a.frame(0).unwrap()).unwrap(),
            Progress::Complete(_)
        ));
        decoder.reset();
        assert!(decoder.session_id().is_none());
        match decoder.ingest_frame(&b.frame(0).unwrap()).unwrap() {
            Progress::Complete(bytes) => assert_eq!(bytes, b"second message"),
            other => panic!("expected completion, got {other:?}"),
        }
    }

    #[test]
    fn progress_reports_a_sensible_fraction() {
        let payload: Vec<u8> = (0..30_000).map(|i| i as u8).collect();
        let encoder = Encoder::new(&payload, EncoderConfig::default()).unwrap();
        let mut decoder = Decoder::new();

        let mut last = 0.0;
        for index in 0..5u32 {
            let progress = decoder
                .ingest_frame(&encoder.frame(index).unwrap())
                .unwrap();
            let f = progress.fraction();
            assert!(f >= last, "progress went backwards");
            assert!((0.0..=1.0).contains(&f));
            last = f;
        }
        assert!(last > 0.0);
    }

    #[test]
    fn rejects_a_grid_size_mismatch() {
        let encoder = Encoder::new(b"grid mismatch", EncoderConfig::default()).unwrap();
        let frame = encoder.frame(0).unwrap();
        // Claim the frame is a different size than it is.
        let err = decode_sampled(GridSize::G65, &frame.to_colors(), 0.28);
        assert!(err.is_err());
    }

    #[test]
    fn decode_sampled_validates_input_length() {
        assert!(matches!(
            decode_sampled(GridSize::G49, &[[0, 0, 0]; 10], 0.28),
            Err(PzError::WrongLength)
        ));
    }

    #[test]
    fn detect_ignores_tiny_images() {
        let data = vec![0u8; 16 * 16 * 3];
        let view = RgbView::rgb(16, 16, &data).unwrap();
        assert!(detect(&view).is_empty());
    }
}
