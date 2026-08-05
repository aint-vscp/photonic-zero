# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project aims
to follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html) from 1.0
onwards.

While PZ is pre-1.0 the **wire format may change in any minor release**. Version
1.0 will freeze it.

## [Unreleased]

### Planned
- Java, Swift, Go and Dart bindings. The C ABI is the portability layer.
- A browser playground: one tab transmits, another decodes from `getUserMedia`.
- A general PNG reader, which currently handles only the stored-deflate subset
  the library writes itself.
- Real-camera test corpora to complement the synthetic optical simulation.

## [0.1.0] - 2026-08-05

Initial release. Wire format version 1.

### Added

**Protocol**
- Rateless screen-to-camera transfer built on LT fountain codes, with a
  systematic prefix so a clean capture finishes at zero coding overhead.
- Two-layer forward error correction: interleaved Reed-Solomon over GF(2^8)
  within each frame, LT fountain codes across frames.
- Confidence-driven erasure marking. The demodulator reports how far each cell
  sat from its decision boundary, and marginal cells become Reed-Solomon
  erasures, which cost half as much to repair as errors.
- Three colour modes: `Mono` (1 bit/cell), `Rgb4` (2 bits/cell using only the
  four even-weight codewords, giving per-cell error detection) and `Rgb8`
  (3 bits/cell).
- Five grid sizes from 33x33 to 97x97, spaced so the decoder's estimate of the
  grid from marker spacing has plus or minus 8 cells of tolerance.
- A self-describing 16-byte frame header under RS(32,16), always modulated in
  black and white, so a receiver joining a stream mid-transmission needs no
  handshake.
- Four 7x7 finder patterns supporting a full projective homography, with frame
  rotation resolved by trying all four orientations against the header CRC.
- Per-frame colour calibration from eight reference patches, so auto-exposure
  and auto-white-balance do not defeat the thresholds.
- Deterministic `ln` and `sqrt` built only from IEEE-754 arithmetic, so the
  fountain degree distribution is bit-identical across platforms and languages.

**Crates**
- `pz-fec` — GF(2^8), Reed-Solomon errors-and-erasures, CRC-16 and CRC-32.
- `pz-fountain` — LT fountain codes, SplitMix64, robust soliton distribution.
- `pz-vision` — adaptive thresholding, finder detection, homography, cell
  sampling.
- `pz-core` — the protocol, plus a renderer and a dependency-free PNG writer.
- `pz-cli` — the `pz` tool: `encode`, `decode`, `info`, `selftest`.
- `pz-ffi` — C ABI, with `include/photonic_zero.h` and a header-only C++17 RAII
  wrapper in `include/photonic_zero.hpp`.
- `pz-wasm` — WebAssembly ABI.

All core crates have **zero external dependencies** and build `no_std` with
`alloc`. `pz-core` compiles for `wasm32-unknown-unknown`.

**Packages**
- `photonic-zero` on npm — a ~100 kB WebAssembly build with TypeScript
  definitions, usable in browsers and Node, plus an `npx photonic-zero` CLI.
  Built without `wasm-bindgen` so the package has no dependencies of its own
  and the JavaScript glue stays readable.
- `photonic-zero` on PyPI — `abi3` wheels, so one wheel per platform covers
  CPython 3.8 and every later version. The decode releases the GIL.
- `pz-core`, `pz-fec`, `pz-fountain`, `pz-vision` and `pz-cli` on crates.io.

**Documentation**
- `rfc/RFC-0001-pz-frame-format.md`, the normative wire format specification.
- A complete C example under `examples/c/`.

### Testing
- 214 tests in the workspace, 9 across the C ABI, 10 across the WebAssembly
  ABI, 21 in the JavaScript package and 25 in the Python package.
- `crates/pz-core/tests/optical_loop.rs` decodes through a simulated camera with
  perspective, defocus, colour cast, sensor noise, partial occlusion and
  arbitrary rotation.
- Randomised stress tests: 400 trials of Reed-Solomon at random parameters
  within the correction radius, and 40 fountain transfers at up to 60% frame
  loss.

[Unreleased]: https://github.com/aint-vscp/photonic-zero/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/aint-vscp/photonic-zero/releases/tag/v0.1.0
