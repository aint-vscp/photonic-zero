//! Photonic Zero (PZ): an optical data link made of a screen and a camera.
//!
//! PZ encodes bytes into a stream of high-density colour frames, displays them,
//! and reconstructs the message from a video capture of that display. No radio,
//! no pairing, no network - just line of sight.
//!
//! Where a QR code is one static image holding a fixed number of bytes, a PZ
//! transmission is a *rateless stream*. The transmitter never stops emitting
//! frames and never needs to know which ones were seen. The receiver watches
//! until it has enough, whichever ones those happened to be.
//!
//! # The stack
//!
//! ```text
//!   user bytes
//!       |  CRC-32 container                    <- detects a wrong reassembly
//!   +---v-------------------------------------------------+
//!   |  pz-fountain   rateless LT codes                    |  temporal FEC:
//!   |                systematic prefix + repair droplets   |  recovers whole
//!   +---+-------------------------------------------------+  lost frames
//!       |  one droplet per frame
//!       |  CRC-32 bound to the frame index     <- never feed a bad droplet in
//!   +---v-------------------------------------------------+
//!   |  pz-fec        Reed-Solomon, interleaved             |  spatial FEC:
//!   |                errors and erasures                   |  glare, blur,
//!   +---+-------------------------------------------------+  a thumb
//!       |  symbols -> cells
//!   +---v-------------------------------------------------+
//!   |  colour modulation   1, 2 or 3 bits per cell         |
//!   |  + self-describing RS(32,16) header                  |
//!   +---+-------------------------------------------------+
//!       |  pixels on a screen
//!       ~   ~   ~   ~   photons   ~   ~   ~   ~
//!       |  pixels in a camera
//!   +---v-------------------------------------------------+
//!   |  pz-vision     threshold, find markers, un-warp,     |
//!   |                sample cells with confidence          |
//!   +-----------------------------------------------------+
//! ```
//!
//! # Sending
//!
//! ```
//! use pz_core::{Encoder, EncoderConfig};
//!
//! let encoder = Encoder::new(b"transfer me over light", EncoderConfig::default())?;
//!
//! // Draw these in a loop, forever. Frame 0 is as good a place to stop as any.
//! let frame = encoder.frame(0)?;
//! assert_eq!(frame.modules(), 49);
//! # Ok::<(), pz_core::PzError>(())
//! ```
//!
//! # Receiving
//!
//! ```
//! use pz_core::{Decoder, Encoder, EncoderConfig, Progress};
//!
//! let message = b"transfer me over light";
//! let encoder = Encoder::new(message, EncoderConfig::default())?;
//! let mut decoder = Decoder::new();
//!
//! for index in 0.. {
//!     let frame = encoder.frame(index)?;
//!     if let Progress::Complete(bytes) = decoder.ingest_frame(&frame)? {
//!         assert_eq!(bytes, message);
//!         break;
//!     }
//! }
//! # Ok::<(), pz_core::PzError>(())
//! ```
//!
//! # `no_std`
//!
//! Disable the `std` feature to build against `core` + `alloc` only. The
//! `png` feature (on by default) adds a small dependency-free PNG writer.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

#[cfg(feature = "std")]
extern crate std;

pub mod color;
pub mod decoder;
pub mod encoder;
pub mod frame;
pub mod header;
pub mod layout;
pub mod render;

#[cfg(feature = "png")]
pub mod png;

pub use color::{Calibration, ColorMode};
pub use decoder::{Decoder, Progress};
pub use encoder::{Encoder, EncoderConfig, Frame};
pub use frame::{FrameProfile, DEFAULT_PARITY_CODE, PARITY_RATIOS};
pub use header::{FrameHeader, PROTOCOL_VERSION};
pub use layout::{GridSize, Layout, Role};
pub use render::{RenderOptions, RgbImage};

pub use pz_fountain::SolitonParams;
pub use pz_vision::RgbView;

/// Anything that can go wrong encoding or decoding a PZ transmission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PzError {
    /// A Reed-Solomon operation failed.
    Fec(pz_fec::FecError),
    /// A fountain coding operation failed.
    Fountain(pz_fountain::FountainError),
    /// The frame header could not be repaired or did not checksum.
    HeaderCorrupt,
    /// The header parsed but names a version, mode or grid this build does not
    /// implement.
    UnsupportedFormat,
    /// The chosen grid and mode leave no usable payload capacity.
    CapacityTooSmall,
    /// A buffer did not have the length the operation requires.
    WrongLength,
    /// The payload is empty.
    EmptyPayload,
    /// The payload is longer than a single session can address.
    PayloadTooLarge,
    /// A frame's data could not be repaired.
    FrameCorrupt,
    /// No PZ frame could be located in the image.
    NoFrameDetected,
    /// The frame belongs to a different session than the one in progress.
    SessionMismatch,
    /// A checksum did not match.
    ChecksumMismatch,
}

impl core::fmt::Display for PzError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Fec(e) => write!(f, "reed-solomon: {e}"),
            Self::Fountain(e) => write!(f, "fountain: {e}"),
            Self::HeaderCorrupt => f.write_str("frame header could not be recovered"),
            Self::UnsupportedFormat => f.write_str("unsupported PZ format version or parameter"),
            Self::CapacityTooSmall => f.write_str("grid and mode leave no payload capacity"),
            Self::WrongLength => f.write_str("buffer length does not match"),
            Self::EmptyPayload => f.write_str("payload is empty"),
            Self::PayloadTooLarge => f.write_str("payload exceeds the maximum session size"),
            Self::FrameCorrupt => f.write_str("frame data could not be repaired"),
            Self::NoFrameDetected => f.write_str("no PZ frame found in the image"),
            Self::SessionMismatch => f.write_str("frame belongs to a different session"),
            Self::ChecksumMismatch => f.write_str("checksum mismatch"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for PzError {}

impl From<pz_fec::FecError> for PzError {
    fn from(e: pz_fec::FecError) -> Self {
        Self::Fec(e)
    }
}

impl From<pz_fountain::FountainError> for PzError {
    fn from(e: pz_fountain::FountainError) -> Self {
        Self::Fountain(e)
    }
}

/// Largest payload a single PZ session can carry, in bytes.
///
/// The limit comes from the fountain layer addressing blocks with a `u32`
/// frame index; long before this the transfer time becomes the real
/// constraint.
pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;
