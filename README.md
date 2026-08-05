# Photonic Zero

**PZ** — *Photonic Zero*. Data over light, from any screen to any camera.
Zero radio, zero pairing, zero network.

Photonic Zero encodes bytes into a stream of high-density colour frames, displays
them, and reconstructs the message from a video capture of that display. The only
thing crossing the gap is light.

```
   sender's screen                              receiver's camera
   +-----------------+                          +-----------------+
   |  # #  ####  # # |                          |                 |
   |  ##  ## # ##  # |   ~~~~ photons ~~~~>     |   decoding...   |
   |  # #### #  ###  |                          |   87%           |
   +-----------------+                          +-----------------+
        no wifi                                    no bluetooth
        no pairing                                 no network permission
```

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at
your option.

---

## Status

**v0.1.0 — early. The wire format is not yet frozen and has not been
independently audited.** The Rust implementation is complete and tested
(214 tests, including an end-to-end suite that decodes through a simulated
camera with perspective, defocus, colour cast and sensor noise). Do not deploy
it as your only defence against anything that matters yet.

---

## Why not just an animated QR code

A QR code is one static image holding a fixed number of bytes. To send more you
must show a sequence — and the instant you do, you have a protocol problem: the
receiver will miss frames, and **a screen cannot hear**. There is no back
channel over which to say "resend frame 47".

Most animated-barcode schemes answer this by looping forever and hoping the
receiver eventually sees each frame at least once. That is the coupon collector
problem, and it is slow and unbounded in the worst case.

PZ answers it differently. It is a **rateless** stream:

- The transmitter emits an endless sequence of *droplets*, each an XOR of a
  pseudo-randomly chosen subset of the message's blocks.
- The receiver does not care **which** droplets it catches, only **how many**.
  Collect a little over the minimum and the message falls out.
- Frames `0..K` are sent *systematically* (frame `i` carries block `i` verbatim),
  so a clean capture finishes with zero coding overhead and only pays for the
  fountain when frames are actually lost.

Nothing ever needs to be requested, acknowledged, or retransmitted.

| | QR code | PZ |
|---|---|---|
| Payload | Fixed, up to ~3 KB | Unbounded stream |
| Frame loss | Fatal, rescan | Absorbed by design |
| Back channel | Not needed (one frame) | Not needed (rateless) |
| Bits per cell | 1 | 1, 2 or 3 |
| Error correction | Reed-Solomon | Reed-Solomon **and** LT fountain codes |
| Orientation | 3 finders, fixed | 4 finders, decodes at any rotation |
| Perspective | Affine (+ alignment patterns) | Full projective homography |

---

## How it works

```
   user bytes
       |  CRC-32 container                    <- detects a wrong reassembly
   +---v-------------------------------------------------+
   |  pz-fountain   rateless LT codes                    |  TEMPORAL FEC:
   |                systematic prefix + repair droplets   |  recovers whole
   +---+-------------------------------------------------+  lost frames
       |  one droplet per frame
       |  CRC-32 bound to the frame index     <- never feed a bad droplet in
   +---v-------------------------------------------------+
   |  pz-fec        Reed-Solomon, interleaved             |  SPATIAL FEC:
   |                errors and erasures                   |  glare, blur,
   +---+-------------------------------------------------+  a thumb
       |  symbols -> cells
   +---v-------------------------------------------------+
   |  colour modulation   1, 2 or 3 bits per cell         |
   |  + self-describing RS(32,16) header                  |
   +---+-------------------------------------------------+
       |  pixels on a screen
       ~   ~   ~   ~   photons   ~   ~   ~   ~
       |  pixels in a camera
   +---v-------------------------------------------------+
   |  pz-vision     threshold, find markers, un-warp,     |
   |                sample cells with confidence          |
   +-----------------------------------------------------+
```

Two layers of error correction, because a screen-to-camera link fails in two
completely different ways:

- **Spatially**, inside a frame — a lamp reflected off the screen, a soft focus,
  a finger over one corner. Reed-Solomon over GF(2⁸) repairs this, interleaved so
  that a localised blob of damage is spread thinly across every block instead of
  destroying one entirely.
- **Temporally**, between frames — the camera drops one because the OS scheduled
  something else, autofocus hunted, or a hand shook. LT fountain codes repair
  this without ever asking for a retransmission.

### The parts that were interesting to build

**Erasure-aware demodulation.** Reed-Solomon repairs an *erasure* (damage at a
known position) for half the price of an *error* (damage at an unknown position):
it corrects any `e` errors and `f` erasures satisfying `2e + f <= n - k`. So the
demodulator does not just guess each cell's value — it reports a confidence, and
cells that landed near a decision boundary are handed to the FEC layer as "I do
not know" rather than as a coin flip. Cheap to compute, and it roughly doubles
the damage a frame survives.

**A colour mode that detects its own errors.** `Rgb4` uses only the four
*even-weight* codewords of the 3-bit colour space — black, yellow, magenta, cyan.
An odd number of channel bits is therefore impossible, so any single channel that
reads wrong turns a legal codeword into an illegal one. The decoder cannot tell
*which* channel failed, but it does not need to: it marks the cell as an erasure.
`Rgb4` carries two thirds of `Rgb8`'s raw bits but hands the FEC layer far better
information, and usually wins on a marginal capture.

**Four finder patterns, not three.** QR locates itself from three. Three points
define an *affine* transform, which cannot express perspective — and a phone held
at an angle to a screen produces genuine perspective, where the far edge is
shorter than the near edge. PZ uses four identical 7×7 finders, which is exactly
what a projective homography needs. Identical markers leave the rotation
ambiguous, so the decoder tries all four and lets the CRC-protected header
arbitrate: cheaper and stricter than any geometric tie-break, and it means a
frame decodes upside down for free.

**Per-frame colour calibration.** Auto-exposure, auto-white-balance, screen
gamma and ambient light all move what "red" measures as. Every frame therefore
carries eight reference patches spanning the RGB corners, and every threshold is
re-derived from the frame in front of the camera rather than assumed.

**Deterministic floating point.** The fountain code's degree distribution depends
on `ln` and `sqrt`. Platform math libraries are not bit-reproducible, and two
implementations that disagree by one bit can select different blocks for the same
frame and fail to interoperate. PZ specifies both functions as algorithms built
only from IEEE-754 `+ - * /`, which every conforming implementation reproduces
exactly.

**Zero dependencies.** The entire Rust core — Galois field arithmetic,
Reed-Solomon, fountain codes, the computer vision pipeline, and a PNG writer —
has no external crates at all. For a protocol positioned around air-gapped
security, a supply chain you can read in an afternoon is a feature.

---

## Install

| Ecosystem | Install | Package |
|---|---|---|
| Rust | `cargo add pz-core` | [`pz-core`](https://crates.io/crates/pz-core) |
| Rust CLI | `cargo install pz-cli` | [`pz-cli`](https://crates.io/crates/pz-cli) |
| JavaScript | `npm install photonic-zero` | [`photonic-zero`](https://www.npmjs.com/package/photonic-zero) |
| Anything, no install | `npx photonic-zero selftest` | |
| Python | `pip install photonic-zero` | [`photonic-zero`](https://pypi.org/project/photonic-zero/) |
| C / C++ | `include/photonic_zero.h`, `.hpp` | build `crates/pz-ffi` |

The JavaScript package is a ~100 kB WebAssembly build with no dependencies and
works in browsers and Node. The Python package ships `abi3` wheels, so one wheel
per platform covers CPython 3.8 and every later version.

## Quick start

### Rust

```toml
[dependencies]
pz-core = "0.1"
```

```rust
use pz_core::{Decoder, Encoder, EncoderConfig, Progress};

// Sending: the stream is endless, so just keep drawing.
let encoder = Encoder::new(b"transfer me over light", EncoderConfig::default())?;
for index in 0.. {
    let frame = encoder.frame(index)?;   // draw frame.color_at(row, col)
    # break;
}

// Receiving: feed it camera frames until it says it has enough.
let mut decoder = Decoder::new();
let view = pz_core::RgbView::rgb(width, height, &pixels).unwrap();
match decoder.ingest_image(&view)? {
    Progress::Complete(bytes) => println!("got {} bytes", bytes.len()),
    Progress::Progressed { recovered, total, .. } => println!("{recovered}/{total}"),
    Progress::NotFound | Progress::Rejected => {}   // routine, keep going
}
```

`Progress::NotFound` and `Progress::Rejected` are normal. Most captured frames of
a hand-held camera are unusable; that is exactly why the code is rateless.

### Command line

```console
$ cargo install --path crates/pz-cli

$ pz encode secret.txt -o frames --profile robust
encoded 86 bytes
  grid        33x33 cells, Mono, parity 5
  per frame   34 bytes
  minimum     3 frames
  written     8 frames to frames
  session     0x4BA2
  at 30 fps   about 0.1s if the receiver catches 4 frames in 5

$ pz decode frames/*.png -o recovered.txt
recovered 86 bytes from 3 of 3 images

$ pz selftest
  robust      33 cells    34 B/frame  minimum  242 frames  used  324 (1.34x)  OK
  balanced    49 cells   479 B/frame  minimum   18 frames  used   25 (1.39x)  OK
  resilient   65 cells   640 B/frame  minimum   13 frames  used   17 (1.31x)  OK
  fast        97 cells  2739 B/frame  minimum    3 frames  used    3 (1.00x)  OK

all profiles round-tripped 8192 bytes with 25% frame loss
```

`pz info` prints the full capacity table.

### C

```c
#include "photonic_zero.h"

pz_status st;
pz_config cfg = pz_config_default();
pz_encoder *enc = pz_encoder_new((const uint8_t *)"hello", 5, &cfg, &st);

pz_decoder *dec = pz_decoder_new();
pz_progress p;
for (uint32_t i = 0; i < 64; i++) {
    pz_frame *f = pz_encoder_frame(enc, i, &st);
    pz_decoder_ingest_frame(dec, f, &p, &st);
    pz_frame_free(f);
    if (p.kind == PZ_PROGRESS_COMPLETE) break;
}

pz_buffer out = pz_decoder_result(dec, &st);
pz_buffer_free(out);
pz_decoder_free(dec);
pz_encoder_free(enc);
```

Build the static library with
`cargo build --release --manifest-path crates/pz-ffi/Cargo.toml`, then see
[`examples/c/roundtrip.c`](examples/c/roundtrip.c) for a complete, compilable
program.

### C++

[`include/photonic_zero.hpp`](include/photonic_zero.hpp) is a header-only C++17
RAII layer over the same ABI — handles free themselves, buffers become
`std::vector`, failures become exceptions.

```cpp
pz::Encoder encoder{"transfer me over light"};
pz::Decoder decoder;
for (uint32_t i = 0; ; ++i) {
    if (decoder.ingest(encoder.frame(i)).complete()) break;
}
std::vector<uint8_t> message = decoder.result();
```

### JavaScript and TypeScript

```console
npm install photonic-zero
```

```js
import { load, ProgressKind } from 'photonic-zero';

// In Node the bundled module is read from disk; in a browser pass a fetch().
const pz = await load();

const encoder = pz.encode('transfer me over light');
const { width, height, data } = encoder.frameRGBA(0, { modulePx: 8 });
ctx.putImageData(new ImageData(data, width, height), 0, 0);

const decoder = pz.decoder();
const frame = ctx.getImageData(0, 0, canvas.width, canvas.height);
if (decoder.ingestRGBA(frame.width, frame.height, frame.data).complete) {
  console.log(new TextDecoder().decode(decoder.result()));
}
```

TypeScript definitions are included. See
[`packages/js/README.md`](packages/js/README.md) for the browser camera
walkthrough.

### Python

```console
pip install photonic-zero
```

```python
import photonic_zero as pz

encoder = pz.encode(b"transfer me over light", profile="balanced")
open("frame0.png", "wb").write(encoder.png(0))

decoder = pz.Decoder()
status = decoder.ingest(width, height, pixels)   # RGB or RGBA bytes
if status.complete:
    print(decoder.result)
```

The decode releases the GIL, so it will not block other threads. See
[`bindings/python/README.md`](bindings/python/README.md).

### Other languages

The C ABI in [`include/photonic_zero.h`](include/photonic_zero.h) is the
portability layer: anything with an FFI can call PZ today. Java, Swift, Go and
Dart bindings are open as `help wanted` issues.

---

## Capacity

Measured, not estimated — reproduce with `pz info`.

| Grid | Mode | Parity | Data cells | Bytes/frame | KB/s @30fps | KB/s @60fps |
|-----:|------|-------:|-----------:|------------:|------------:|------------:|
| 33 | mono | 5 | 511 | 34 | 1.0 | 2.0 |
| 33 | rgb8 | 3 | 511 | 134 | 3.9 | 7.9 |
| 49 | mono | 3 | 1791 | 157 | 4.6 | 9.2 |
| 49 | rgb8 | 3 | 1791 | 479 | 14.0 | 28.1 |
| 65 | rgb4 | 3 | 3583 | 640 | 18.8 | 37.5 |
| 65 | rgb8 | 3 | 3583 | 962 | 28.2 | 56.4 |
| 97 | rgb8 | 1 | 8703 | 2739 | 80.2 | 160.5 |

The top row is roughly 660 kbit/s at 30 fps. Throughput assumes every displayed
frame is captured; a real camera misses some, and the self test above shows the
fountain absorbing 25% frame loss for 1.0x to 1.4x the minimum frame count.

**Profiles:** `robust` (33/mono/40% parity) for bad light or a bad camera,
`balanced` (49/rgb8/28%) as the default, `resilient` (65/rgb4/28%) when you want
per-cell error detection, `fast` (97/rgb8/16%) for a close, steady, well-lit
capture.

---

## Limitations

Stated plainly, because a protocol that oversells itself is worse than useless.

- **Camera frame rate is the hard ceiling.** A photodiode-based optical link
  reaches gigabits; a camera-based one is bounded by frames per second times bits
  per frame. PZ is right for credentials, keys, configuration and documents —
  not for video.
- **PZ is a transport, not a security protocol.** It provides **no
  confidentiality and no authenticity** on its own. Anyone who can see the screen
  can read the bytes. Encrypt and sign your payload before handing it to PZ.
  What PZ gives you is a channel that does not traverse a network and cannot pass
  through a wall.
- **Direct sunlight and specular glare** can defeat the calibration patches.
- **Rolling shutter** on cheap sensors can tear a frame during fast motion; that
  frame is simply rejected and the fountain moves on.
- **The wire format is not frozen.** It may change before 1.0.
- Not audited. Not battle-tested. Treat 0.1 as what it says on the tin.

---

## Where this is useful

- **Air-gapped signing.** Stream a transaction to an offline device, verify it on
  that device's own screen, stream the signature back. Malware on the connected
  workstation never touches the key.
- **Out-of-band authentication.** A login challenge rendered on the compromised
  machine's screen, solved on a phone, answered over cellular — the two channels
  never share a failure domain.
- **Zero-touch provisioning.** Push Wi-Fi credentials and certificates to a
  device that has no network yet, without opening a temporary access point.
- **RF-restricted environments.** Operating theatres, aircraft, shielded rooms,
  test ranges.
- **Anti-replay physical presence.** Light does not pass through walls, so
  decoding a live stream is evidence of line of sight.

---

## Repository layout

```
crates/
  pz-fec        GF(2^8), Reed-Solomon errors-and-erasures, CRC-16/32
  pz-fountain   LT fountain codes, SplitMix64, robust soliton, deterministic ln/sqrt
  pz-vision     thresholding, finder detection, homography, cell sampling
  pz-core       the protocol: layout, header, frame, colour, encoder, decoder, render, PNG
  pz-cli        the `pz` command line tool
  pz-ffi        C ABI
  pz-wasm       WebAssembly ABI
packages/js/    the npm package, with the `npx photonic-zero` CLI
bindings/python/ the PyPI package, built with maturin
include/        photonic_zero.h, photonic_zero.hpp
examples/c/     a complete C program
rfc/            RFC-0001, the normative wire format specification
```

`pz-fec`, `pz-fountain`, `pz-vision` and `pz-core` all build `no_std` with
`--no-default-features` (they need `alloc`), and `pz-core` compiles for
`wasm32-unknown-unknown`.

---

## Building and testing

```console
$ cargo test --workspace              # 214 tests
$ cargo test -p pz-core --test optical_loop   # decode through a simulated camera
$ cargo check -p pz-fec -p pz-fountain -p pz-vision -p pz-core --no-default-features
$ cargo check -p pz-core --target wasm32-unknown-unknown
$ cargo test --manifest-path crates/pz-ffi/Cargo.toml    # 9 tests
$ cargo test --manifest-path crates/pz-wasm/Cargo.toml   # 10 tests

$ cd packages/js && npm run build && npm test             # 19 tests
$ maturin build --manifest-path bindings/python/Cargo.toml --out dist
$ pip install --no-index --find-links dist photonic-zero  # 25 tests
$ pytest bindings/python/tests -q
```

On Windows, `. .\scripts\dev-env.ps1` puts the Rust toolchain and a 64-bit
mingw-w64 linker on `PATH`.

The suite that matters most is `crates/pz-core/tests/optical_loop.rs`. Every
other test hands the decoder cell values directly; those tests hand it **pixels**,
through the real computer vision path, after putting the frame through
perspective, defocus, an auto-exposure that crushes the range, a warm white
balance, and sensor noise. It is the test that would catch a protocol that only
works on paper.

---

## Specification

[`rfc/RFC-0001-pz-frame-format.md`](rfc/RFC-0001-pz-frame-format.md) is the
normative wire format: frame geometry, the colour codes, the header layout, the
capacity derivation, the Reed-Solomon and fountain layers, and the conformance
test vectors an independent implementation needs. The Rust code is the reference
implementation, not the definition.

---

## Contributing

Contributions are very welcome — see [CONTRIBUTING.md](CONTRIBUTING.md). Good
places to start are marked `good first issue`. The bindings for
JavaScript/WebAssembly, Python, Swift, Go and Dart are all open, as is a real
browser playground.

For security reports, see [SECURITY.md](SECURITY.md).

---

## Acknowledgements

PZ stands on well-established work: Luby's LT codes, the Reed-Solomon literature,
and the finder-pattern approach that QR codes made ubiquitous. The contribution
here is the combination — two-layer FEC with confidence-driven erasures, applied
to a screen-to-camera channel — not the individual pieces.

Built by [The Pieza](https://thepieza.com).
