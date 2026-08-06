//! C ABI for Photonic Zero.
//!
//! This is the portability layer: anything that can call C can call PZ. The
//! C++ header in `include/photonic_zero.hpp` wraps it in RAII types, and the
//! same surface is what a Swift, Go, C#, Dart or Zig binding should target.
//!
//! # Conventions
//!
//! - Handles are opaque pointers. Every `*_new` has exactly one `*_free`.
//!   Freeing null is a no-op; freeing twice is undefined.
//! - Fallible calls take a `PzStatus*` out-parameter and return null or a
//!   zeroed value on failure. Passing null for the status is allowed.
//! - Byte buffers are returned as [`PzBuffer`], which the caller must release
//!   with [`pz_buffer_free`]. The memory comes from Rust's allocator, so it
//!   must not be freed with `free()`.
//! - Every entry point is panic-safe: a panic in Rust is caught at the
//!   boundary and reported as [`PzStatus::Internal`] rather than unwinding
//!   into C, which would be undefined behaviour.
//! - Nothing here is thread-safe on a single handle. Separate handles in
//!   separate threads are fine.

#![deny(missing_docs)]
// A C entry point that takes a buffer, its dimensions, its length, an output
// struct and a status out-parameter genuinely needs eight arguments. Splitting
// them into a struct would be more idiomatic Rust and less idiomatic C, and
// this crate exists to be idiomatic C.
#![allow(clippy::too_many_arguments)]

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::slice;

use pz_core::render::{render, RenderOptions};
use pz_core::{
    ColorMode, Decoder, Encoder, EncoderConfig, Frame, GridSize, Progress, PzError, RgbView,
};

/// Result of a fallible call.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PzStatus {
    /// Success.
    Ok = 0,
    /// A required pointer was null, or a length was inconsistent.
    InvalidArgument = 1,
    /// The payload was empty.
    EmptyPayload = 2,
    /// The payload exceeds the maximum session size.
    PayloadTooLarge = 3,
    /// The configuration leaves no usable payload capacity.
    CapacityTooSmall = 4,
    /// The frame header could not be recovered.
    HeaderCorrupt = 5,
    /// Unsupported format version or parameter.
    UnsupportedFormat = 6,
    /// Frame data could not be repaired.
    FrameCorrupt = 7,
    /// No PZ frame was found in the image.
    NoFrameDetected = 8,
    /// The frame belongs to a different session.
    SessionMismatch = 9,
    /// A checksum did not match.
    ChecksumMismatch = 10,
    /// Decoding has not finished, so there is no result to take.
    NotComplete = 11,
    /// A panic was caught at the boundary.
    Internal = 12,
}

impl From<PzError> for PzStatus {
    fn from(e: PzError) -> Self {
        match e {
            PzError::EmptyPayload => PzStatus::EmptyPayload,
            PzError::PayloadTooLarge => PzStatus::PayloadTooLarge,
            PzError::CapacityTooSmall => PzStatus::CapacityTooSmall,
            PzError::HeaderCorrupt => PzStatus::HeaderCorrupt,
            PzError::UnsupportedFormat => PzStatus::UnsupportedFormat,
            PzError::FrameCorrupt => PzStatus::FrameCorrupt,
            PzError::NoFrameDetected => PzStatus::NoFrameDetected,
            PzError::SessionMismatch => PzStatus::SessionMismatch,
            PzError::ChecksumMismatch => PzStatus::ChecksumMismatch,
            PzError::WrongLength => PzStatus::InvalidArgument,
            PzError::Fec(_) | PzError::Fountain(_) => PzStatus::FrameCorrupt,
        }
    }
}

/// How far a decode has progressed.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PzProgressKind {
    /// No frame was located in the image.
    NotFound = 0,
    /// A frame was seen but could not be used.
    Rejected = 1,
    /// A droplet was absorbed; the message is incomplete.
    Progressed = 2,
    /// The message is complete and verified.
    Complete = 3,
}

/// Detail about a decode step.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PzProgress {
    /// Which of the four outcomes occurred.
    pub kind: PzProgressKind,
    /// Session being received; meaningful when `kind` is `Progressed`.
    pub session_id: u16,
    /// Frame index just absorbed; meaningful when `kind` is `Progressed`.
    pub frame_index: u32,
    /// Source blocks recovered so far.
    pub recovered: usize,
    /// Source blocks in the message.
    pub total: usize,
    /// Fraction recovered, in `[0, 1]`.
    pub fraction: f64,
}

impl Default for PzProgress {
    fn default() -> Self {
        Self {
            kind: PzProgressKind::NotFound,
            session_id: 0,
            frame_index: 0,
            recovered: 0,
            total: 0,
            fraction: 0.0,
        }
    }
}

/// An owned byte buffer handed to the caller.
///
/// Release with [`pz_buffer_free`]. A zeroed buffer means "no data".
#[repr(C)]
#[derive(Debug)]
pub struct PzBuffer {
    /// Pointer to the bytes, or null.
    pub data: *mut u8,
    /// Number of valid bytes.
    pub len: usize,
    /// Allocated capacity. Internal; do not modify.
    pub cap: usize,
}

impl PzBuffer {
    fn empty() -> Self {
        Self {
            data: ptr::null_mut(),
            len: 0,
            cap: 0,
        }
    }

    fn from_vec(mut v: Vec<u8>) -> Self {
        let out = Self {
            data: v.as_mut_ptr(),
            len: v.len(),
            cap: v.capacity(),
        };
        std::mem::forget(v);
        out
    }
}

/// Encoder configuration.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PzConfig {
    /// Grid size code: 0 = 33x33, 1 = 49, 2 = 65, 3 = 81, 4 = 97.
    pub grid_code: u8,
    /// Colour mode code: 0 = mono, 1 = 4-colour, 2 = 8-colour.
    pub mode_code: u8,
    /// Parity ratio index, 0 to 7. Higher is more redundant.
    pub parity_code: u8,
    /// Session id, or a negative value to derive one from the payload.
    pub session_id: i32,
}

/// The balanced default: 49x49, 8 colours, 28% parity.
#[no_mangle]
pub extern "C" fn pz_config_default() -> PzConfig {
    config_from(EncoderConfig::default())
}

/// The most robust profile: 33x33, black and white, 40% parity.
#[no_mangle]
pub extern "C" fn pz_config_robust() -> PzConfig {
    config_from(EncoderConfig::robust())
}

/// The highest throughput profile: 97x97, 8 colours, 16% parity.
#[no_mangle]
pub extern "C" fn pz_config_fast() -> PzConfig {
    config_from(EncoderConfig::fast())
}

/// 65x65 in 4-colour mode: per-cell error detection at 28% parity.
#[no_mangle]
pub extern "C" fn pz_config_resilient() -> PzConfig {
    config_from(EncoderConfig::resilient())
}

fn config_from(c: EncoderConfig) -> PzConfig {
    PzConfig {
        grid_code: c.grid.code(),
        mode_code: c.mode.code(),
        parity_code: c.parity_code,
        session_id: c.session_id.map_or(-1, i32::from),
    }
}

fn config_to(c: &PzConfig) -> Option<EncoderConfig> {
    Some(EncoderConfig {
        grid: GridSize::from_code(c.grid_code)?,
        mode: ColorMode::from_code(c.mode_code)?,
        parity_code: c.parity_code,
        session_id: if c.session_id < 0 {
            None
        } else {
            Some(c.session_id as u16)
        },
        soliton: pz_core::SolitonParams::default(),
    })
}

/// Library version as a NUL-terminated string. Never null; do not free.
#[no_mangle]
pub extern "C" fn pz_version() -> *const std::os::raw::c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr().cast()
}

/// Human-readable text for a status code. Never null; do not free.
#[no_mangle]
pub extern "C" fn pz_status_message(status: PzStatus) -> *const std::os::raw::c_char {
    let s: &str = match status {
        PzStatus::Ok => "ok\0",
        PzStatus::InvalidArgument => "invalid argument\0",
        PzStatus::EmptyPayload => "payload is empty\0",
        PzStatus::PayloadTooLarge => "payload exceeds the maximum session size\0",
        PzStatus::CapacityTooSmall => "grid and mode leave no payload capacity\0",
        PzStatus::HeaderCorrupt => "frame header could not be recovered\0",
        PzStatus::UnsupportedFormat => "unsupported PZ format version or parameter\0",
        PzStatus::FrameCorrupt => "frame data could not be repaired\0",
        PzStatus::NoFrameDetected => "no PZ frame found in the image\0",
        PzStatus::SessionMismatch => "frame belongs to a different session\0",
        PzStatus::ChecksumMismatch => "checksum mismatch\0",
        PzStatus::NotComplete => "decoding has not finished\0",
        PzStatus::Internal => "internal error\0",
    };
    s.as_ptr().cast()
}

/// Release a buffer returned by this library.
///
/// # Safety
/// `buffer` must have come from a PZ function and must not have been freed
/// already.
#[no_mangle]
pub unsafe extern "C" fn pz_buffer_free(buffer: PzBuffer) {
    if !buffer.data.is_null() {
        drop(Vec::from_raw_parts(buffer.data, buffer.len, buffer.cap));
    }
}

fn set_status(out: *mut PzStatus, value: PzStatus) {
    if !out.is_null() {
        // Safety: checked non-null, and the caller promises it points at a
        // writable PzStatus.
        unsafe { *out = value };
    }
}

/// Run `f`, turning any panic into `PzStatus::Internal`.
fn guard<T>(status: *mut PzStatus, fallback: T, f: impl FnOnce() -> Result<T, PzStatus>) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(value)) => {
            set_status(status, PzStatus::Ok);
            value
        }
        Ok(Err(e)) => {
            set_status(status, e);
            fallback
        }
        Err(_) => {
            set_status(status, PzStatus::Internal);
            fallback
        }
    }
}

// ---------------------------------------------------------------- encoder ---

/// Opaque encoder handle.
pub struct PzEncoder {
    inner: Encoder,
}

/// Create an encoder for `payload`.
///
/// Returns null on failure, with the reason written to `status`.
///
/// # Safety
/// `payload` must point to at least `len` readable bytes, and `config` must
/// point to a valid [`PzConfig`].
#[no_mangle]
pub unsafe extern "C" fn pz_encoder_new(
    payload: *const u8,
    len: usize,
    config: *const PzConfig,
    status: *mut PzStatus,
) -> *mut PzEncoder {
    guard(status, ptr::null_mut(), || {
        if payload.is_null() || config.is_null() {
            return Err(PzStatus::InvalidArgument);
        }
        let bytes = slice::from_raw_parts(payload, len);
        let cfg = config_to(&*config).ok_or(PzStatus::UnsupportedFormat)?;
        let inner = Encoder::new(bytes, cfg).map_err(PzStatus::from)?;
        Ok(Box::into_raw(Box::new(PzEncoder { inner })))
    })
}

/// Destroy an encoder. Null is a no-op.
///
/// # Safety
/// `encoder` must have come from [`pz_encoder_new`] and not been freed.
#[no_mangle]
pub unsafe extern "C" fn pz_encoder_free(encoder: *mut PzEncoder) {
    if !encoder.is_null() {
        drop(Box::from_raw(encoder));
    }
}

macro_rules! enc_getter {
    ($name:ident, $ret:ty, $body:expr) => {
        /// # Safety
        /// `encoder` must be a live handle from [`pz_encoder_new`].
        #[no_mangle]
        pub unsafe extern "C" fn $name(encoder: *const PzEncoder) -> $ret {
            if encoder.is_null() {
                return Default::default();
            }
            let f: fn(&Encoder) -> $ret = $body;
            f(&(*encoder).inner)
        }
    };
}

enc_getter!(pz_encoder_block_count, usize, |e| e.block_count());
enc_getter!(pz_encoder_session_id, u16, |e| e.session_id());
enc_getter!(pz_encoder_payload_len, usize, |e| e.payload_len());
enc_getter!(pz_encoder_droplet_size, usize, |e| e
    .profile()
    .droplet_size());
enc_getter!(pz_encoder_modules, usize, |e| e.layout().modules());

/// Estimated transfer time in seconds.
///
/// # Safety
/// `encoder` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn pz_encoder_estimated_seconds(
    encoder: *const PzEncoder,
    fps: f64,
    capture_ratio: f64,
) -> f64 {
    if encoder.is_null() {
        return 0.0;
    }
    (*encoder).inner.estimated_seconds(fps, capture_ratio)
}

/// Opaque frame handle.
pub struct PzFrame {
    inner: Frame,
}

/// Build frame `index`. Defined for every index; the stream never ends.
///
/// # Safety
/// `encoder` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn pz_encoder_frame(
    encoder: *const PzEncoder,
    index: u32,
    status: *mut PzStatus,
) -> *mut PzFrame {
    guard(status, ptr::null_mut(), || {
        if encoder.is_null() {
            return Err(PzStatus::InvalidArgument);
        }
        let inner = (*encoder).inner.frame(index).map_err(PzStatus::from)?;
        Ok(Box::into_raw(Box::new(PzFrame { inner })))
    })
}

/// Destroy a frame. Null is a no-op.
///
/// # Safety
/// `frame` must have come from [`pz_encoder_frame`] and not been freed.
#[no_mangle]
pub unsafe extern "C" fn pz_frame_free(frame: *mut PzFrame) {
    if !frame.is_null() {
        drop(Box::from_raw(frame));
    }
}

/// Cells per side.
///
/// # Safety
/// `frame` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn pz_frame_modules(frame: *const PzFrame) -> usize {
    if frame.is_null() {
        return 0;
    }
    (*frame).inner.modules()
}

/// The frame index this frame carries.
///
/// # Safety
/// `frame` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn pz_frame_index(frame: *const PzFrame) -> u32 {
    if frame.is_null() {
        return 0;
    }
    (*frame).inner.index()
}

/// Copy the frame's cells as `modules * modules` colour codes.
///
/// # Safety
/// `frame` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn pz_frame_cells(frame: *const PzFrame) -> PzBuffer {
    if frame.is_null() {
        return PzBuffer::empty();
    }
    PzBuffer::from_vec((*frame).inner.cells().to_vec())
}

/// Copy the frame as one RGB triple per cell, row-major.
///
/// # Safety
/// `frame` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn pz_frame_rgb(frame: *const PzFrame) -> PzBuffer {
    if frame.is_null() {
        return PzBuffer::empty();
    }
    let mut out = Vec::with_capacity((*frame).inner.cells().len() * 3);
    for rgb in (*frame).inner.to_colors() {
        out.extend_from_slice(&rgb);
    }
    PzBuffer::from_vec(out)
}

/// Render frame `index` to an RGB image.
///
/// The side length in pixels is written to `out_size`.
///
/// # Safety
/// `encoder` must be a live handle; `out_size` may be null.
#[no_mangle]
pub unsafe extern "C" fn pz_encoder_render_rgb(
    encoder: *const PzEncoder,
    index: u32,
    module_px: usize,
    quiet_zone: usize,
    out_size: *mut usize,
    status: *mut PzStatus,
) -> PzBuffer {
    guard(status, PzBuffer::empty(), || {
        if encoder.is_null() {
            return Err(PzStatus::InvalidArgument);
        }
        let frame = (*encoder).inner.frame(index).map_err(PzStatus::from)?;
        let image = render(
            &frame,
            &RenderOptions {
                module_px: module_px.max(1),
                quiet_zone,
                background: [255, 255, 255],
                ink: None,
            },
        );
        if !out_size.is_null() {
            *out_size = image.width;
        }
        Ok(PzBuffer::from_vec(image.data))
    })
}

/// Render frame `index` as a PNG file.
///
/// # Safety
/// `encoder` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn pz_encoder_render_png(
    encoder: *const PzEncoder,
    index: u32,
    module_px: usize,
    quiet_zone: usize,
    status: *mut PzStatus,
) -> PzBuffer {
    guard(status, PzBuffer::empty(), || {
        if encoder.is_null() {
            return Err(PzStatus::InvalidArgument);
        }
        let frame = (*encoder).inner.frame(index).map_err(PzStatus::from)?;
        let image = render(
            &frame,
            &RenderOptions {
                module_px: module_px.max(1),
                quiet_zone,
                background: [255, 255, 255],
                ink: None,
            },
        );
        Ok(PzBuffer::from_vec(pz_core::png::encode(&image)))
    })
}

// ---------------------------------------------------------------- decoder ---

/// Opaque decoder handle.
pub struct PzDecoder {
    inner: Decoder,
}

/// Create a decoder.
#[no_mangle]
pub extern "C" fn pz_decoder_new() -> *mut PzDecoder {
    Box::into_raw(Box::new(PzDecoder {
        inner: Decoder::new(),
    }))
}

/// Destroy a decoder. Null is a no-op.
///
/// # Safety
/// `decoder` must have come from [`pz_decoder_new`] and not been freed.
#[no_mangle]
pub unsafe extern "C" fn pz_decoder_free(decoder: *mut PzDecoder) {
    if !decoder.is_null() {
        drop(Box::from_raw(decoder));
    }
}

/// Forget the current session so a new transmission can be received.
///
/// # Safety
/// `decoder` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn pz_decoder_reset(decoder: *mut PzDecoder) {
    if !decoder.is_null() {
        (*decoder).inner.reset();
    }
}

fn progress_to_c(p: &Progress) -> PzProgress {
    let mut out = PzProgress {
        fraction: p.fraction(),
        ..PzProgress::default()
    };
    match p {
        Progress::NotFound => out.kind = PzProgressKind::NotFound,
        Progress::Rejected => out.kind = PzProgressKind::Rejected,
        Progress::Progressed {
            session_id,
            frame_index,
            recovered,
            total,
        } => {
            out.kind = PzProgressKind::Progressed;
            out.session_id = *session_id;
            out.frame_index = *frame_index;
            out.recovered = *recovered;
            out.total = *total;
        }
        Progress::Complete(_) => {
            out.kind = PzProgressKind::Complete;
            out.fraction = 1.0;
        }
    }
    out
}

unsafe fn ingest(
    decoder: *mut PzDecoder,
    width: usize,
    height: usize,
    data: *const u8,
    len: usize,
    channels: usize,
    out_progress: *mut PzProgress,
    status: *mut PzStatus,
) {
    guard(status, (), || {
        if decoder.is_null() || data.is_null() {
            return Err(PzStatus::InvalidArgument);
        }
        if width == 0 || height == 0 || len < width * height * channels {
            return Err(PzStatus::InvalidArgument);
        }
        let bytes = slice::from_raw_parts(data, len);
        let view = if channels == 4 {
            RgbView::rgba(width, height, bytes)
        } else {
            RgbView::rgb(width, height, bytes)
        }
        .ok_or(PzStatus::InvalidArgument)?;

        let progress = (*decoder)
            .inner
            .ingest_image(&view)
            .map_err(PzStatus::from)?;
        if !out_progress.is_null() {
            *out_progress = progress_to_c(&progress);
        }
        Ok(())
    });
}

/// Offer a tightly packed 8-bit RGB image.
///
/// # Safety
/// `data` must point to at least `len` readable bytes and `len` must be at
/// least `width * height * 3`.
#[no_mangle]
pub unsafe extern "C" fn pz_decoder_ingest_rgb(
    decoder: *mut PzDecoder,
    width: usize,
    height: usize,
    data: *const u8,
    len: usize,
    out_progress: *mut PzProgress,
    status: *mut PzStatus,
) {
    ingest(decoder, width, height, data, len, 3, out_progress, status);
}

/// Offer a tightly packed 8-bit RGBA image, such as a canvas `ImageData`.
///
/// # Safety
/// `data` must point to at least `len` readable bytes and `len` must be at
/// least `width * height * 4`.
#[no_mangle]
pub unsafe extern "C" fn pz_decoder_ingest_rgba(
    decoder: *mut PzDecoder,
    width: usize,
    height: usize,
    data: *const u8,
    len: usize,
    out_progress: *mut PzProgress,
    status: *mut PzStatus,
) {
    ingest(decoder, width, height, data, len, 4, out_progress, status);
}

/// Offer a frame directly, bypassing the camera path.
///
/// # Safety
/// Both handles must be live.
#[no_mangle]
pub unsafe extern "C" fn pz_decoder_ingest_frame(
    decoder: *mut PzDecoder,
    frame: *const PzFrame,
    out_progress: *mut PzProgress,
    status: *mut PzStatus,
) {
    guard(status, (), || {
        if decoder.is_null() || frame.is_null() {
            return Err(PzStatus::InvalidArgument);
        }
        let progress = (*decoder)
            .inner
            .ingest_frame(&(*frame).inner)
            .map_err(PzStatus::from)?;
        if !out_progress.is_null() {
            *out_progress = progress_to_c(&progress);
        }
        Ok(())
    });
}

/// Fraction of the message recovered, in `[0, 1]`.
///
/// # Safety
/// `decoder` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn pz_decoder_progress(decoder: *const PzDecoder) -> f64 {
    if decoder.is_null() {
        return 0.0;
    }
    (*decoder).inner.progress()
}

/// Images offered so far.
///
/// # Safety
/// `decoder` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn pz_decoder_frames_seen(decoder: *const PzDecoder) -> usize {
    if decoder.is_null() {
        return 0;
    }
    (*decoder).inner.frames_seen()
}

/// Frames that decoded and were absorbed.
///
/// # Safety
/// `decoder` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn pz_decoder_frames_accepted(decoder: *const PzDecoder) -> usize {
    if decoder.is_null() {
        return 0;
    }
    (*decoder).inner.frames_accepted()
}

/// Write the session id to `out` and return true, or return false if no
/// session has been locked on to yet.
///
/// # Safety
/// `decoder` must be a live handle; `out` must be writable or null.
#[no_mangle]
pub unsafe extern "C" fn pz_decoder_session_id(decoder: *const PzDecoder, out: *mut u16) -> bool {
    if decoder.is_null() {
        return false;
    }
    match (*decoder).inner.session_id() {
        Some(id) => {
            if !out.is_null() {
                *out = id;
            }
            true
        }
        None => false,
    }
}

/// Copy out the completed message.
///
/// Returns an empty buffer with `PzStatus::NotComplete` if decoding has not
/// finished. The decoder keeps its own copy, so this may be called repeatedly.
///
/// # Safety
/// `decoder` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn pz_decoder_result(
    decoder: *const PzDecoder,
    status: *mut PzStatus,
) -> PzBuffer {
    guard(status, PzBuffer::empty(), || {
        if decoder.is_null() {
            return Err(PzStatus::InvalidArgument);
        }
        match (*decoder).inner.result() {
            Some(bytes) => Ok(PzBuffer::from_vec(bytes.to_vec())),
            None => Err(PzStatus::NotComplete),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Copy a buffer's contents without consuming it, so the test can still
    /// hand the original back to `pz_buffer_free`.
    fn take(buffer: &PzBuffer) -> Vec<u8> {
        assert!(!buffer.data.is_null());
        unsafe { slice::from_raw_parts(buffer.data, buffer.len).to_vec() }
    }

    #[test]
    fn round_trips_through_the_c_abi() {
        let payload = b"across the ABI boundary";
        let config = pz_config_default();
        let mut status = PzStatus::Internal;

        unsafe {
            let encoder = pz_encoder_new(payload.as_ptr(), payload.len(), &config, &mut status);
            assert_eq!(status, PzStatus::Ok);
            assert!(!encoder.is_null());
            assert_eq!(pz_encoder_modules(encoder), 49);
            assert!(pz_encoder_block_count(encoder) >= 1);

            let decoder = pz_decoder_new();
            let mut progress = PzProgress::default();

            for index in 0..64u32 {
                let frame = pz_encoder_frame(encoder, index, &mut status);
                assert_eq!(status, PzStatus::Ok);
                pz_decoder_ingest_frame(decoder, frame, &mut progress, &mut status);
                assert_eq!(status, PzStatus::Ok);
                pz_frame_free(frame);
                if progress.kind == PzProgressKind::Complete {
                    break;
                }
            }
            assert_eq!(progress.kind, PzProgressKind::Complete);

            let result = pz_decoder_result(decoder, &mut status);
            assert_eq!(status, PzStatus::Ok);
            assert_eq!(take(&result), payload);
            pz_buffer_free(result);

            pz_decoder_free(decoder);
            pz_encoder_free(encoder);
        }
    }

    #[test]
    fn null_handles_are_survivable() {
        unsafe {
            assert_eq!(pz_encoder_modules(ptr::null()), 0);
            assert_eq!(pz_frame_modules(ptr::null()), 0);
            assert_eq!(pz_decoder_progress(ptr::null()), 0.0);
            assert!(!pz_decoder_session_id(ptr::null(), ptr::null_mut()));
            pz_encoder_free(ptr::null_mut());
            pz_decoder_free(ptr::null_mut());
            pz_frame_free(ptr::null_mut());
            pz_buffer_free(PzBuffer::empty());
        }
    }

    #[test]
    fn reports_invalid_arguments() {
        let config = pz_config_default();
        let mut status = PzStatus::Ok;
        unsafe {
            let e = pz_encoder_new(ptr::null(), 0, &config, &mut status);
            assert!(e.is_null());
            assert_eq!(status, PzStatus::InvalidArgument);

            let payload = b"x";
            let bad_config = PzConfig {
                grid_code: 99,
                ..config
            };
            let e = pz_encoder_new(payload.as_ptr(), 1, &bad_config, &mut status);
            assert!(e.is_null());
            assert_eq!(status, PzStatus::UnsupportedFormat);
        }
    }

    #[test]
    fn empty_payload_is_reported() {
        let config = pz_config_default();
        let mut status = PzStatus::Ok;
        let payload: [u8; 0] = [];
        unsafe {
            let e = pz_encoder_new(payload.as_ptr(), 0, &config, &mut status);
            assert!(e.is_null());
            assert_eq!(status, PzStatus::EmptyPayload);
        }
    }

    #[test]
    fn result_before_completion_is_not_complete() {
        let decoder = pz_decoder_new();
        let mut status = PzStatus::Ok;
        unsafe {
            let buffer = pz_decoder_result(decoder, &mut status);
            assert_eq!(status, PzStatus::NotComplete);
            assert!(buffer.data.is_null());
            pz_decoder_free(decoder);
        }
    }

    #[test]
    fn renders_png_and_rgb() {
        let payload = b"render via ffi";
        let config = pz_config_default();
        let mut status = PzStatus::Ok;
        unsafe {
            let encoder = pz_encoder_new(payload.as_ptr(), payload.len(), &config, &mut status);
            let mut size = 0usize;
            let rgb = pz_encoder_render_rgb(encoder, 0, 4, 4, &mut size, &mut status);
            assert_eq!(status, PzStatus::Ok);
            assert_eq!(size, (49 + 8) * 4);
            assert_eq!(rgb.len, size * size * 3);
            pz_buffer_free(rgb);

            let png = pz_encoder_render_png(encoder, 0, 4, 4, &mut status);
            assert_eq!(status, PzStatus::Ok);
            assert_eq!(&take(&png)[..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
            pz_buffer_free(png);

            pz_encoder_free(encoder);
        }
    }

    #[test]
    fn ingest_validates_buffer_length() {
        let decoder = pz_decoder_new();
        let data = [0u8; 12];
        let mut status = PzStatus::Ok;
        let mut progress = PzProgress::default();
        unsafe {
            // Claims 100x100 but supplies 12 bytes.
            pz_decoder_ingest_rgb(
                decoder,
                100,
                100,
                data.as_ptr(),
                data.len(),
                &mut progress,
                &mut status,
            );
            assert_eq!(status, PzStatus::InvalidArgument);
            pz_decoder_free(decoder);
        }
    }

    #[test]
    fn status_messages_are_present_for_every_code() {
        for status in [
            PzStatus::Ok,
            PzStatus::InvalidArgument,
            PzStatus::EmptyPayload,
            PzStatus::PayloadTooLarge,
            PzStatus::CapacityTooSmall,
            PzStatus::HeaderCorrupt,
            PzStatus::UnsupportedFormat,
            PzStatus::FrameCorrupt,
            PzStatus::NoFrameDetected,
            PzStatus::SessionMismatch,
            PzStatus::ChecksumMismatch,
            PzStatus::NotComplete,
            PzStatus::Internal,
        ] {
            let ptr = pz_status_message(status);
            assert!(!ptr.is_null());
            let text = unsafe { std::ffi::CStr::from_ptr(ptr) };
            assert!(!text.to_str().unwrap().is_empty());
        }
    }

    #[test]
    fn presets_map_to_distinct_configurations() {
        let d = pz_config_default();
        let r = pz_config_robust();
        let f = pz_config_fast();
        assert_ne!(d.grid_code, r.grid_code);
        assert_ne!(r.mode_code, f.mode_code);
        assert!(config_to(&d).is_some());
        assert!(config_to(&r).is_some());
        assert!(config_to(&f).is_some());
        assert!(config_to(&pz_config_resilient()).is_some());
    }
}
