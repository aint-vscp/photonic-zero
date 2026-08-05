//! WebAssembly ABI for Photonic Zero.
//!
//! This exports a flat C ABI rather than using `wasm-bindgen`. That is a
//! deliberate choice: it keeps the zero-dependency property of the rest of the
//! project, needs no toolchain beyond `cargo build --target
//! wasm32-unknown-unknown`, produces a substantially smaller module, and leaves
//! the JavaScript glue as readable source rather than generated code.
//!
//! # Calling convention
//!
//! - `pz_alloc` and `pz_free` manage buffers in the module's linear memory.
//!   JavaScript writes input there before calling in, and reads output back
//!   out afterwards.
//! - Functions returning a variable-length buffer take an `out_len` pointer.
//!   The caller allocates 4 bytes for it, reads the length back, then frees
//!   the returned buffer with `pz_free(ptr, len)`.
//! - Handles are opaque pointers. Null return means failure.
//! - Nothing here unwinds: the crate is built with `panic = "abort"`.

#![deny(missing_docs)]

use std::ptr;
use std::slice;

use pz_core::render::{render, RenderOptions};
use pz_core::{ColorMode, Decoder, Encoder, EncoderConfig, GridSize, Progress, RgbView};

/// Allocate `len` bytes in the module's linear memory.
///
/// Returns null for a zero length or on allocation failure.
#[no_mangle]
pub extern "C" fn pz_alloc(len: usize) -> *mut u8 {
    if len == 0 {
        return ptr::null_mut();
    }
    // A boxed slice, not `Vec::with_capacity`: `with_capacity` promises only
    // *at least* `len` bytes, and `pz_free` reconstructs the Vec with capacity
    // exactly `len`. Deallocating with the wrong layout would be undefined, so
    // the exactness has to be guaranteed rather than assumed.
    let buffer = vec![0u8; len].into_boxed_slice();
    Box::into_raw(buffer).cast::<u8>()
}

/// Release a buffer obtained from [`pz_alloc`] or returned by this module.
///
/// # Safety
/// `ptr` and `len` must be exactly as returned. Freeing twice is undefined.
#[no_mangle]
pub unsafe extern "C" fn pz_free(ptr: *mut u8, len: usize) {
    if !ptr.is_null() && len != 0 {
        drop(Vec::from_raw_parts(ptr, len, len));
    }
}

/// Wire format version implemented by this module.
#[no_mangle]
pub extern "C" fn pz_protocol_version() -> u32 {
    pz_core::PROTOCOL_VERSION as u32
}

fn into_buffer(bytes: Vec<u8>, out_len: *mut usize) -> *mut u8 {
    if bytes.is_empty() {
        return ptr::null_mut();
    }
    // `into_boxed_slice` reallocates if it has to so that capacity == length
    // exactly. `shrink_to_fit` would not do: it is best-effort, and the
    // allocator may leave spare capacity. That distinction matters twice over,
    // because the number written to `out_len` is both the payload length the
    // caller reads and the length `pz_free` reconstructs the Vec with.
    let boxed = bytes.into_boxed_slice();
    let len = boxed.len();
    let ptr = Box::into_raw(boxed).cast::<u8>();
    if !out_len.is_null() {
        // Safety: the caller promises this points at a writable usize.
        unsafe { *out_len = len };
    }
    ptr
}

// ---------------------------------------------------------------- encoder ---

/// Opaque encoder handle.
pub struct WasmEncoder {
    inner: Encoder,
}

/// Create an encoder over `len` bytes at `payload`.
///
/// `grid`, `mode` and `parity` are the header codes; pass a negative
/// `session` to derive one from the payload. A non-negative `session` above
/// 65535 does not fit the wire format and is rejected rather than truncated.
/// Returns null on failure.
///
/// # Safety
/// `payload` must point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn pz_encoder_new(
    payload: *const u8,
    len: usize,
    grid: u8,
    mode: u8,
    parity: u8,
    session: i32,
) -> *mut WasmEncoder {
    if payload.is_null() || len == 0 {
        return ptr::null_mut();
    }
    let (Some(grid), Some(mode)) = (GridSize::from_code(grid), ColorMode::from_code(mode)) else {
        return ptr::null_mut();
    };
    // A session id is 16 bits on the wire. Truncating 65536 to 0 would hand
    // back an encoder that silently ignores the id the caller pinned.
    let session_id = if session < 0 {
        None
    } else {
        match u16::try_from(session) {
            Ok(id) => Some(id),
            Err(_) => return ptr::null_mut(),
        }
    };
    let config = EncoderConfig {
        grid,
        mode,
        parity_code: parity,
        session_id,
        soliton: pz_core::SolitonParams::default(),
    };
    match Encoder::new(slice::from_raw_parts(payload, len), config) {
        Ok(inner) => Box::into_raw(Box::new(WasmEncoder { inner })),
        Err(_) => ptr::null_mut(),
    }
}

/// Destroy an encoder.
///
/// # Safety
/// `encoder` must come from [`pz_encoder_new`] and not have been freed.
#[no_mangle]
pub unsafe extern "C" fn pz_encoder_free(encoder: *mut WasmEncoder) {
    if !encoder.is_null() {
        drop(Box::from_raw(encoder));
    }
}

macro_rules! encoder_getter {
    ($name:ident, $ret:ty, $body:expr) => {
        /// # Safety
        /// `encoder` must be a live handle.
        #[no_mangle]
        pub unsafe extern "C" fn $name(encoder: *const WasmEncoder) -> $ret {
            if encoder.is_null() {
                return Default::default();
            }
            let f: fn(&Encoder) -> $ret = $body;
            f(&(*encoder).inner)
        }
    };
}

encoder_getter!(pz_encoder_block_count, usize, |e| e.block_count());
encoder_getter!(pz_encoder_session_id, u32, |e| u32::from(e.session_id()));
encoder_getter!(pz_encoder_modules, usize, |e| e.layout().modules());
encoder_getter!(pz_encoder_droplet_size, usize, |e| e
    .profile()
    .droplet_size());

/// Cell colour codes for one frame, one byte per cell, row-major.
///
/// # Safety
/// `encoder` must be a live handle; `out_len` must be writable.
#[no_mangle]
pub unsafe extern "C" fn pz_encoder_frame_cells(
    encoder: *const WasmEncoder,
    index: u32,
    out_len: *mut usize,
) -> *mut u8 {
    if encoder.is_null() {
        return ptr::null_mut();
    }
    match (*encoder).inner.frame(index) {
        Ok(frame) => into_buffer(frame.cells().to_vec(), out_len),
        Err(_) => ptr::null_mut(),
    }
}

/// One frame rendered as RGBA pixels, ready for `putImageData`.
///
/// The side length in pixels is `(modules + 2 * quiet_zone) * module_px`.
///
/// # Safety
/// `encoder` must be a live handle; `out_len` must be writable.
#[no_mangle]
pub unsafe extern "C" fn pz_encoder_frame_rgba(
    encoder: *const WasmEncoder,
    index: u32,
    module_px: usize,
    quiet_zone: usize,
    out_len: *mut usize,
) -> *mut u8 {
    if encoder.is_null() {
        return ptr::null_mut();
    }
    match (*encoder).inner.frame(index) {
        Ok(frame) => {
            let image = render(
                &frame,
                &RenderOptions {
                    module_px: module_px.max(1),
                    quiet_zone,
                    background: [255, 255, 255],
                },
            );
            into_buffer(image.to_rgba(), out_len)
        }
        Err(_) => ptr::null_mut(),
    }
}

/// One frame encoded as a complete PNG file.
///
/// # Safety
/// `encoder` must be a live handle; `out_len` must be writable.
#[no_mangle]
pub unsafe extern "C" fn pz_encoder_frame_png(
    encoder: *const WasmEncoder,
    index: u32,
    module_px: usize,
    quiet_zone: usize,
    out_len: *mut usize,
) -> *mut u8 {
    if encoder.is_null() {
        return ptr::null_mut();
    }
    match (*encoder).inner.frame(index) {
        Ok(frame) => {
            let image = render(
                &frame,
                &RenderOptions {
                    module_px: module_px.max(1),
                    quiet_zone,
                    background: [255, 255, 255],
                },
            );
            into_buffer(pz_core::png::encode(&image), out_len)
        }
        Err(_) => ptr::null_mut(),
    }
}

// ---------------------------------------------------------------- decoder ---

/// Opaque decoder handle, carrying the outcome of the last ingest so that
/// JavaScript can read it without a struct crossing the boundary.
///
/// Block counts are deliberately *not* cached here. They are read straight off
/// the inner decoder, so a routine `NotFound` cannot clobber them and a
/// `Complete` cannot leave them stale.
pub struct WasmDecoder {
    inner: Decoder,
    kind: i32,
    frame_index: u32,
}

/// Feed one view into the decoder and translate the outcome into an ABI code.
///
/// Shared by both ingest entry points so the two cannot drift apart.
fn ingest(state: &mut WasmDecoder, view: &RgbView<'_>) -> i32 {
    let kind = match state.inner.ingest_image(view) {
        Ok(Progress::NotFound) => 0,
        Ok(Progress::Rejected) => 1,
        Ok(Progress::Progressed { frame_index, .. }) => {
            state.frame_index = frame_index;
            2
        }
        Ok(Progress::Complete(_)) => 3,
        Err(_) => -1,
    };
    state.kind = kind;
    kind
}

/// Create a decoder.
#[no_mangle]
pub extern "C" fn pz_decoder_new() -> *mut WasmDecoder {
    Box::into_raw(Box::new(WasmDecoder {
        inner: Decoder::new(),
        kind: 0,
        frame_index: 0,
    }))
}

/// Destroy a decoder.
///
/// # Safety
/// `decoder` must come from [`pz_decoder_new`] and not have been freed.
#[no_mangle]
pub unsafe extern "C" fn pz_decoder_free(decoder: *mut WasmDecoder) {
    if !decoder.is_null() {
        drop(Box::from_raw(decoder));
    }
}

/// Forget the current session so a new transmission can be received.
///
/// # Safety
/// `decoder` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn pz_decoder_reset(decoder: *mut WasmDecoder) {
    if !decoder.is_null() {
        (*decoder).inner.reset();
        (*decoder).kind = 0;
        (*decoder).frame_index = 0;
    }
}

/// Offer an RGBA image, as produced by `getImageData`.
///
/// Returns 0 not-found, 1 rejected, 2 progressed, 3 complete, or -1 on a bad
/// argument. The first three are all routine while a camera settles.
///
/// # Safety
/// `data` must point to at least `len` readable bytes, and `len` must be at
/// least `width * height * 4`.
#[no_mangle]
pub unsafe extern "C" fn pz_decoder_ingest_rgba(
    decoder: *mut WasmDecoder,
    width: usize,
    height: usize,
    data: *const u8,
    len: usize,
) -> i32 {
    if decoder.is_null() || data.is_null() || width == 0 || height == 0 {
        return -1;
    }
    if len < width * height * 4 {
        return -1;
    }
    let bytes = slice::from_raw_parts(data, len);
    let Some(view) = RgbView::rgba(width, height, bytes) else {
        return -1;
    };

    ingest(&mut *decoder, &view)
}

/// Offer a PNG file, decoding it internally.
///
/// Saves JavaScript from needing a PNG decoder to read frames back off disk.
/// Only the subset written by this library is supported; see
/// `pz_core::png::decode`.
///
/// Returns the same codes as [`pz_decoder_ingest_rgba`], with -2 meaning the
/// PNG itself could not be read.
///
/// # Safety
/// `data` must point to at least `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn pz_decoder_ingest_png(
    decoder: *mut WasmDecoder,
    data: *const u8,
    len: usize,
) -> i32 {
    if decoder.is_null() || data.is_null() || len == 0 {
        return -1;
    }
    let bytes = slice::from_raw_parts(data, len);
    let Ok(image) = pz_core::png::decode(bytes) else {
        return -2;
    };
    let Some(view) = RgbView::rgb(image.width, image.height, &image.data) else {
        return -2;
    };

    ingest(&mut *decoder, &view)
}

macro_rules! decoder_getter {
    ($name:ident, $ret:ty, $body:expr) => {
        /// # Safety
        /// `decoder` must be a live handle.
        #[no_mangle]
        pub unsafe extern "C" fn $name(decoder: *const WasmDecoder) -> $ret {
            if decoder.is_null() {
                return Default::default();
            }
            let f: fn(&WasmDecoder) -> $ret = $body;
            f(&*decoder)
        }
    };
}

decoder_getter!(pz_decoder_progress, f64, |d| d.inner.progress());
decoder_getter!(pz_decoder_recovered, usize, |d| d.inner.recovered());
decoder_getter!(pz_decoder_total, usize, |d| d.inner.total());
decoder_getter!(pz_decoder_frame_index, u32, |d| d.frame_index);
decoder_getter!(pz_decoder_frames_seen, usize, |d| d.inner.frames_seen());
decoder_getter!(pz_decoder_frames_accepted, usize, |d| d
    .inner
    .frames_accepted());

/// Session id being received, or -1 if none has been locked on to.
///
/// # Safety
/// `decoder` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn pz_decoder_session_id(decoder: *const WasmDecoder) -> i32 {
    if decoder.is_null() {
        return -1;
    }
    (*decoder).inner.session_id().map_or(-1, i32::from)
}

/// The completed message, or null if decoding has not finished.
///
/// # Safety
/// `decoder` must be a live handle; `out_len` must be writable.
#[no_mangle]
pub unsafe extern "C" fn pz_decoder_result(
    decoder: *const WasmDecoder,
    out_len: *mut usize,
) -> *mut u8 {
    if decoder.is_null() {
        return ptr::null_mut();
    }
    match (*decoder).inner.result() {
        Some(bytes) => into_buffer(bytes.to_vec(), out_len),
        None => ptr::null_mut(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exercises the exported ABI the way the JavaScript glue does, on the
    /// host target so it can actually be run.
    #[test]
    fn round_trips_through_the_wasm_abi() {
        let payload = b"across the wasm boundary";
        unsafe {
            let encoder = pz_encoder_new(payload.as_ptr(), payload.len(), 1, 2, 3, -1);
            assert!(!encoder.is_null());
            assert_eq!(pz_encoder_modules(encoder), 49);

            let decoder = pz_decoder_new();
            let mut kind = 0;
            for index in 0..64u32 {
                let mut len = 0usize;
                let rgba = pz_encoder_frame_rgba(encoder, index, 6, 4, &mut len);
                assert!(!rgba.is_null());
                let side = (49 + 8) * 6;
                assert_eq!(len, side * side * 4);

                kind = pz_decoder_ingest_rgba(decoder, side, side, rgba, len);
                pz_free(rgba, len);
                assert!(kind >= 0, "ingest reported an error");
                if kind == 3 {
                    break;
                }
            }
            assert_eq!(kind, 3, "never completed");

            let mut len = 0usize;
            let result = pz_decoder_result(decoder, &mut len);
            assert!(!result.is_null());
            assert_eq!(slice::from_raw_parts(result, len), payload);
            pz_free(result, len);

            pz_decoder_free(decoder);
            pz_encoder_free(encoder);
        }
    }

    #[test]
    fn png_export_has_a_png_signature() {
        let payload = b"png via wasm";
        unsafe {
            let encoder = pz_encoder_new(payload.as_ptr(), payload.len(), 0, 0, 3, 7);
            let mut len = 0usize;
            let png = pz_encoder_frame_png(encoder, 0, 4, 4, &mut len);
            assert!(!png.is_null());
            let bytes = slice::from_raw_parts(png, len);
            assert_eq!(&bytes[..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
            pz_free(png, len);
            pz_encoder_free(encoder);
        }
    }

    #[test]
    fn null_and_invalid_arguments_are_survivable() {
        unsafe {
            assert!(pz_encoder_new(ptr::null(), 0, 1, 2, 3, -1).is_null());
            assert_eq!(pz_encoder_modules(ptr::null()), 0);
            assert_eq!(pz_decoder_progress(ptr::null()), 0.0);
            assert_eq!(pz_decoder_session_id(ptr::null()), -1);
            assert_eq!(
                pz_decoder_ingest_rgba(ptr::null_mut(), 1, 1, ptr::null(), 0),
                -1
            );
            pz_encoder_free(ptr::null_mut());
            pz_decoder_free(ptr::null_mut());
            pz_free(ptr::null_mut(), 0);

            // An unknown grid code must be rejected rather than guessed at.
            let payload = b"x";
            assert!(pz_encoder_new(payload.as_ptr(), 1, 99, 2, 3, -1).is_null());
        }
    }

    #[test]
    fn ingest_validates_the_buffer_length() {
        let decoder = pz_decoder_new();
        let data = [0u8; 16];
        unsafe {
            assert_eq!(
                pz_decoder_ingest_rgba(decoder, 100, 100, data.as_ptr(), data.len()),
                -1
            );
            pz_decoder_free(decoder);
        }
    }

    #[test]
    fn result_is_null_before_completion() {
        let decoder = pz_decoder_new();
        let mut len = 0usize;
        unsafe {
            assert!(pz_decoder_result(decoder, &mut len).is_null());
            pz_decoder_free(decoder);
        }
    }

    #[test]
    fn alloc_and_free_round_trip() {
        unsafe {
            let ptr = pz_alloc(128);
            assert!(!ptr.is_null());
            ptr.write_bytes(0xAB, 128);
            assert_eq!(*ptr, 0xAB);
            pz_free(ptr, 128);
            assert!(pz_alloc(0).is_null());
        }
    }

    #[test]
    fn reports_the_protocol_version() {
        assert_eq!(pz_protocol_version(), 1);
    }

    /// A completed transfer must report `recovered == total`, and a routine
    /// `NotFound` afterwards must not reset either. Caching the counters in
    /// the handle used to get both of these wrong.
    #[test]
    fn counters_survive_completion_and_a_later_miss() {
        let payload = b"counters stay honest";
        unsafe {
            let encoder = pz_encoder_new(payload.as_ptr(), payload.len(), 1, 2, 3, -1);
            let decoder = pz_decoder_new();

            let mut len = 0usize;
            let rgba = pz_encoder_frame_rgba(encoder, 0, 6, 4, &mut len);
            let side = (49 + 8) * 6;
            assert_eq!(pz_decoder_ingest_rgba(decoder, side, side, rgba, len), 3);
            pz_free(rgba, len);

            let total = pz_decoder_total(decoder);
            assert!(total > 0, "total must be known once a session is locked on");
            assert_eq!(pz_decoder_recovered(decoder), total);

            // A blank image finds nothing. That must not disturb the counters.
            let blank = vec![255u8; side * side * 4];
            assert_eq!(
                pz_decoder_ingest_rgba(decoder, side, side, blank.as_ptr(), blank.len()),
                0
            );
            assert_eq!(pz_decoder_recovered(decoder), total);
            assert_eq!(pz_decoder_total(decoder), total);

            pz_decoder_free(decoder);
            pz_encoder_free(encoder);
        }
    }

    /// A session id wider than the wire format must be refused, not truncated.
    #[test]
    fn out_of_range_session_ids_are_rejected() {
        let payload = b"session range";
        unsafe {
            assert!(pz_encoder_new(payload.as_ptr(), payload.len(), 1, 2, 3, 65_536).is_null());
            let ok = pz_encoder_new(payload.as_ptr(), payload.len(), 1, 2, 3, 65_535);
            assert!(!ok.is_null());
            assert_eq!(pz_encoder_session_id(ok), 65_535);
            pz_encoder_free(ok);
        }
    }

    /// `out_len` must be the payload length, not an allocator-chosen capacity.
    #[test]
    fn reported_length_is_exact() {
        unsafe {
            let mut len = 0usize;
            // 4096 bytes is well past the point where a Vec would over-allocate
            // if it were going to.
            let payload = vec![0xC3u8; 4096];
            let encoder = pz_encoder_new(payload.as_ptr(), payload.len(), 0, 0, 5, 1);
            let cells = pz_encoder_frame_cells(encoder, 0, &mut len);
            assert!(!cells.is_null());
            assert_eq!(len, 33 * 33, "cell buffer must be exactly one per cell");
            pz_free(cells, len);
            pz_encoder_free(encoder);
        }
    }
}
