//! Forward error correction primitives for the Photonic Zero (PZ) optical
//! protocol.
//!
//! This crate is the *spatial* half of PZ's two-layer error correction. It
//! repairs damage inside a single captured frame: glare, focus blur, a finger
//! over one corner, a dead row of pixels. The *temporal* half, which repairs
//! whole frames that the camera never saw, lives in `pz-fountain`.
//!
//! Nothing here is specific to optics; it is a standalone, dependency-free
//! Reed-Solomon implementation that happens to be tuned for the erasure-rich
//! conditions a camera produces.
//!
//! # Example
//!
//! ```
//! use pz_fec::ReedSolomon;
//!
//! // 32 symbols total, 16 of them data: repairs 8 errors or 16 erasures.
//! let rs = ReedSolomon::new(32, 16).unwrap();
//! let message = b"sixteen bytes!!!";
//! let mut code = rs.encode(message).unwrap();
//!
//! // Corrupt four symbols; we know where two of them are.
//! code[0] ^= 0xFF;
//! code[1] ^= 0xFF;
//! code[20] ^= 0xFF;
//! code[21] ^= 0xFF;
//!
//! rs.decode(&mut code, &[0, 1]).unwrap();
//! assert_eq!(&code[..16], message);
//! ```
//!
//! # `no_std`
//!
//! Disable the `std` feature to build against `core` + `alloc` only.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

#[cfg(feature = "std")]
extern crate std;

pub mod crc;
pub mod gf;
pub mod poly;
pub mod rs;

pub use crc::{crc16, crc32};
pub use rs::{FecError, ReedSolomon};
