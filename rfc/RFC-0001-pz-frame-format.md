# RFC-0001: The Photonic Zero Frame Format

| | |
|---|---|
| **Status** | Draft |
| **Wire format version** | 1 |
| **Document version** | 0.1.0 |
| **Date** | 2026-08-05 |
| **Reference implementation** | `crates/pz-core` in this repository |

## Abstract

Photonic Zero (PZ) is a rateless optical data link between a display and a
camera. A transmitter renders an endless stream of two-dimensional colour frames;
a receiver captures them with an ordinary camera and reconstructs the original
byte string. No back channel of any kind is required, and the receiver need not
observe any particular frame.

This document specifies wire format version 1 completely enough to implement
from scratch. Where this document and the reference implementation disagree,
that is a defect in one of them and should be reported.

## Status of this memo

This is a draft. **The wire format is not frozen** and may change before
version 1.0 of the reference implementation.

The key words MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT,
RECOMMENDED, MAY and OPTIONAL are to be interpreted as described in RFC 2119.

---

## 1. Terminology

| Term | Meaning |
|---|---|
| **cell** | One square element of the grid. The smallest addressable unit. |
| **module** | Synonym for cell, used when discussing physical size. |
| **grid** | The `N` by `N` array of cells making up one frame. |
| **frame** | One complete rendered image of the grid. |
| **droplet** | The fountain-coded payload carried by one frame. |
| **block** | One fixed-size piece of the source message. |
| **session** | One complete transmission of one message. |
| **symbol** | One byte at the Reed-Solomon layer. |

### 1.1 Conventions

- All multi-byte integers on the wire are **big endian** unless stated otherwise.
  (The two-byte lengths inside the deflate stream of a PNG are little endian, but
  that is PNG's format, not PZ's.)
- Bit strings are packed **most significant bit first**.
- Cell coordinates are `(row, col)`, both zero-based, with row 0 at the top.
- Grid-space coordinates are continuous: cell `(row, col)` has its centre at
  `x = col + 0.5`, `y = row + 0.5`.
- `a / b` on integers denotes floor division. `ceil(a / b)` is written
  explicitly.

---

## 2. Overview

```
   message bytes
        |
        |  Section 8: container = CRC-32 prefix || message
        v
   +--------------------------------------------------+
   |  Section 7: fountain layer                       |
   |  message split into K blocks; frame i carries a  |
   |  droplet that is the XOR of a chosen subset      |
   +--------------------------------------------------+
        |  droplet (constant size within a session)
        |  Section 8.2: frame payload = droplet || CRC-32(droplet || index)
        v
   +--------------------------------------------------+
   |  Section 6: Reed-Solomon layer                   |
   |  payload split into B blocks, each RS(n,k), then |
   |  interleaved across the frame                    |
   +--------------------------------------------------+
        |  coded symbols
        v
   +--------------------------------------------------+
   |  Section 4: colour modulation, 1/2/3 bits a cell |
   |  Section 5: header, RS(32,16), always monochrome |
   |  Section 3: frame geometry                       |
   +--------------------------------------------------+
        |
        v  photons
```

An implementation MUST implement Sections 3 through 9 to interoperate.

---

## 3. Frame geometry

### 3.1 Grid sizes

A frame is `N` by `N` cells. Version 1 defines five sizes:

| Code | `N` | Total cells |
|---:|---:|---:|
| 0 | 33 | 1089 |
| 1 | 49 | 2401 |
| 2 | 65 | 4225 |
| 3 | 81 | 6561 |
| 4 | 97 | 9409 |

`N = 16 * code + 33`. Sizes are spaced 16 apart so that a receiver estimating
`N` from the spacing of the finder patterns has a tolerance of plus or minus 8
cells before it could select the wrong size.

Implementations MUST reject any other grid code.

### 3.2 Cell roles

Every cell has exactly one role, assigned in this order. Earlier rules win.

```
     0        7 8                          N-9 N-8      N-1
   0 +--------+----+--------------------------+--------+
     | finder |    |        palette           | finder |
     |  7x7   |    |    (see 3.2.4)           |  7x7   |
   6 |        |....|.... timing row .........|        |
   7 +--------+                              +--------+
   8 |   :                                        :   |
     |   :                                        :   |
     | timing                                   timing|
     | column                                   column|
     |   :         header cells, then data        :   |
     |   :         in raster order                :   |
 N-9 |   :                                        :   |
 N-8 +--------+                              +--------+
     | finder |                              | finder |
     |  7x7   |                              |  7x7   |
 N-1 +--------+------------------------------+--------+
```

#### 3.2.1 Finder patterns

Four finder patterns, one at each corner. Each reserves an 8 by 8 block:

| Corner | Reserved block (rows, cols) | Pattern origin (row, col) |
|---|---|---|
| Top left | `0..8`, `0..8` | `(0, 0)` |
| Top right | `0..8`, `N-8..N` | `(0, N-7)` |
| Bottom left | `N-8..N`, `0..8` | `(N-7, 0)` |
| Bottom right | `N-8..N`, `N-8..N` | `(N-7, N-7)` |

Every cell of the reserved block that is not part of the 7 by 7 pattern is
**white** (colour code `0b111`). This is the *separator*: it stops payload cells
from extending the pattern's run lengths and defeating the detector.

Within the 7 by 7 pattern at origin `(r0, c0)`, the cell at offset `(dr, dc)`
for `0 <= dr, dc < 7` is:

```
ring  = min(dr, dc, 6 - dr, 6 - dc)
dark  = (ring == 0) or (ring >= 2)
```

which is the familiar concentric square: a one-cell dark border, a one-cell
white ring, and a 3 by 3 dark core. A line through its centre crosses runs in
the ratio **1:1:3:1:1**, which is invariant under scale, rotation and moderate
perspective — the property that makes it findable without knowing where or how
big the code is.

All four patterns are **identical**. This is deliberate (Section 9.2).

#### 3.2.2 Timing rulers

- Row 6, columns `8` through `N-9` inclusive.
- Column 6, rows `8` through `N-9` inclusive.

A timing cell is dark when its varying index is even, white when odd.

#### 3.2.3 Reserved cell colours

Finder, separator and timing cells are structural: they are identical in every
frame of every session and carry no data.

#### 3.2.4 Colour calibration patches

Eight reference patches, each 2 by 2 cells, occupy rows `0..4` and columns
`px..px+8` where

```
px = (N - 8) / 2
```

The patch at block position `(by, bx)` for `0 <= by < 2`, `0 <= bx < 4` displays
colour code `by * 4 + bx`. The eight patches therefore span all eight corners of
the RGB cube.

Implementations MUST place the patches here and MUST re-derive their colour
thresholds from them on **every** frame (Section 4.3).

#### 3.2.5 Header and data cells

Every cell not claimed above is assigned by walking the grid in **raster order**
(row 0 left to right, then row 1, and so on):

- The **first 256** such cells are header cells.
- All remaining cells are data cells.

This rule requires no per-size geometry table.

### 3.3 Verified cell counts

| `N` | Total | Structural | Palette | Header | Data |
|---:|---:|---:|---:|---:|---:|
| 33 | 1089 | 290 | 32 | 256 | 511 |
| 49 | 2401 | 322 | 32 | 256 | 1791 |
| 65 | 4225 | 354 | 32 | 256 | 3583 |
| 81 | 6561 | 386 | 32 | 256 | 5887 |
| 97 | 9409 | 418 | 32 | 256 | 8703 |

Structural is `4 * 64 + 2 * (N - 16)`: four 8 by 8 blocks plus two timing rulers.

### 3.4 Fiducial reference points

The centres of the four finder patterns, in grid coordinates, in clockwise
order:

```
  top left     ( 3.5,      3.5    )
  top right    ( N - 3.5,  3.5    )
  bottom right ( N - 3.5,  N - 3.5)
  bottom left  ( 3.5,      N - 3.5)
```

These are the source points for the receiver's homography (Section 10.2).
Adjacent centres are `N - 7` cells apart.

### 3.5 Rendering

A frame SHOULD be rendered with a **quiet zone** of at least 4 cells of
background on all sides. Without it the outermost run of a corner finder merges
into whatever surrounds the display and the ratio test fails.

Each cell SHOULD be rendered as a solid square of at least 4 pixels on a side at
the capture resolution.

---

## 4. Colour

### 4.1 Colour codes

A cell displays one of eight colours, identified by a 3-bit code. Bit 0 is red,
bit 1 green, bit 2 blue. Each channel is fully on or fully off.

| Code | R | G | B | Colour |
|---:|:-:|:-:|:-:|---|
| `000` | 0 | 0 | 0 | black |
| `001` | 255 | 0 | 0 | red |
| `010` | 0 | 255 | 0 | green |
| `011` | 255 | 255 | 0 | yellow |
| `100` | 0 | 0 | 255 | blue |
| `101` | 255 | 0 | 255 | magenta |
| `110` | 0 | 255 | 255 | cyan |
| `111` | 255 | 255 | 255 | white |

Treating the three channels as three independent one-bit sub-channels makes
demodulation three threshold comparisons rather than a nearest-neighbour search
in colour space.

### 4.2 Colour modes

| Code | Name | Bits/cell | Alphabet |
|---:|---|---:|---|
| 0 | `Mono` | 1 | black, white |
| 1 | `Rgb4` | 2 | black, yellow, magenta, cyan |
| 2 | `Rgb8` | 3 | all eight |

Modulation from a data value to a colour code:

```
Mono:  0 -> 000    1 -> 111
Rgb4:  0 -> 000    1 -> 011    2 -> 101    3 -> 110
Rgb8:  value -> value
```

**`Rgb4` uses exactly the four even-weight codewords.** An odd number of set
channel bits is therefore not a legal codeword, so any single channel that reads
incorrectly produces a detectably illegal cell. The receiver cannot tell which
channel failed, and does not need to: it MUST report such a cell as an erasure
(Section 6.4). This costs one third of the raw bit rate relative to `Rgb8` and
buys error *detection* at every cell.

Header cells are ALWAYS modulated as `Mono`, whatever the frame's colour mode.

### 4.3 Calibration

A receiver MUST derive its decision thresholds from the eight calibration
patches of the frame it is currently decoding. Auto-exposure, auto-white-balance,
display gamma and ambient light all move the measured value of any given colour;
fixed thresholds fail as soon as a lamp is switched on.

For each channel `ch` in `{0, 1, 2}`, let `P[v][ch]` be the mean measured value
of patch `v`. Then

```
low[ch]  = mean of P[v][ch] over the four v with bit ch clear
high[ch] = mean of P[v][ch] over the four v with bit ch set
threshold[ch] = (low[ch] + high[ch]) / 2
half[ch]      = (high[ch] - low[ch]) / 2
```

A channel reads as 1 when its measured value exceeds `threshold[ch]`.

For `Mono`, the same construction is applied to luma computed as
`0.299 R + 0.587 G + 0.114 B`, using patch `000` as the low reference and patch
`111` as the high reference.

### 4.4 Confidence

Each channel decision carries a confidence:

```
confidence[ch] = min(1, |value - threshold[ch]| / half[ch])
```

with confidence 0 when `half[ch] <= 1`, which means the channel collapsed and
nothing can be recovered from it.

A cell's confidence is the **minimum** over the channels it used. Confidence
drives erasure marking (Section 6.4); a receiver that discards it will still
interoperate but will repair roughly half as much damage.

---

## 5. The frame header

### 5.1 Purpose

Every frame is self-describing. A receiver that joins a transmission already in
progress decodes one frame and learns the grid size, colour mode, parity ratio,
session, frame index and total message length. There is no handshake and no
manifest frame to miss.

The header is consequently the frame's single point of failure, and gets the
strongest coding in the format.

### 5.2 Layout

Sixteen bytes:

```
 byte  0   version (4 bits) | colour mode (4 bits)
 byte  1   grid size (4 bits) | parity code (4 bits)
 bytes 2-3   session id       u16
 bytes 4-7   frame index      u32
 bytes 8-11  payload length   u32   (length of the container, Section 8.1)
 byte  12  flags
 byte  13  reserved, MUST be 0
 bytes 14-15 CRC-16 over bytes 0..13
```

Flags:

| Bit | Meaning |
|---:|---|
| 0 | Container carries a 4-byte CRC-32 prefix. MUST be 1 in version 1. |
| 1-7 | Reserved, MUST be 0. |

`version` MUST be 1. A receiver MUST reject any other value, any undefined
colour mode, any undefined grid code, and any parity code above 7.

### 5.3 CRC-16

CRC-16/CCITT-FALSE: polynomial `0x1021`, initial value `0xFFFF`, no input or
output reflection, no final XOR.

Check value: `crc16("123456789") = 0x29B1`.

### 5.4 Error correction

The 16 header bytes are expanded to 32 by a systematic **RS(32, 16)** code over
GF(2^8) (Section 6.1), repairing up to 8 unknown errors or 16 erasures.

The 32 bytes are written MSB first into the 256 header cells in the order given
by Section 3.2.5, one bit per cell, modulated as `Mono`.

### 5.5 Worked example

```
version = 1, mode = Rgb8 (2), grid = G49 (1), parity code = 3,
session = 0xBEEF, frame index = 7, payload length = 1000, flags = 0x01

plain (16 bytes):
  12 13 BE EF 00 00 00 07 00 00 03 E8 01 00 F7 86

after RS(32,16):
  12 13 BE EF 00 00 00 07 00 00 03 E8 01 00 F7 86
  36 E7 84 D7 62 2D C8 8A 92 A5 4F DD FA 4E E3 B1
```

---

## 6. The Reed-Solomon layer

### 6.1 The field

GF(2^8) generated by the primitive polynomial

```
x^8 + x^4 + x^3 + x^2 + 1        (0x11D)
```

with `2` as the primitive element. This is the same field QR codes use.
Addition and subtraction are XOR.

### 6.2 Encoding

An RS(n, k) code appends `n - k` parity symbols to `k` data symbols. Encoding is
**systematic**: the first `k` symbols of the codeword are the data unchanged.

The generator polynomial is

```
g(x) = product over i in [0, n-k) of (x - a^i)
```

where `a = 2`. The parity is the remainder of `data * x^(n-k)` modulo `g(x)`.

### 6.3 Capacity derivation

Given the grid, colour mode and parity code, both ends compute:

```
data_cells = from Section 3.3
T          = data_cells * bits_per_cell / 8          (floor)
blocks     = max(1, ceil(T / 255))
block_n    = T / blocks                              (floor)
ratio      = PARITY_RATIOS[parity_code]
parity     = floor(block_n * ratio + 0.5)  clamped to [2, block_n - 1]
block_k    = block_n - parity
frame_payload = blocks * block_k
droplet_size  = frame_payload - 4
```

`block_n` MUST be at least 8 and `frame_payload` MUST exceed 4, otherwise the
combination is invalid.

```
PARITY_RATIOS = [0.10, 0.16, 0.22, 0.28, 0.34, 0.40, 0.50, 0.60]
```

`blocks * block_n` symbols are written to cells; any remaining bits of the data
region are padding and MUST be ignored by the receiver.

Selected values (the full table is emitted by
`cargo run -p pz-core --example vectors`):

| `N` | mode | parity | cells | T | blocks | `block_n` | `block_k` | droplet |
|---:|---|---:|---:|---:|---:|---:|---:|---:|
| 33 | mono | 3 | 511 | 63 | 1 | 63 | 45 | 41 |
| 49 | rgb8 | 3 | 1791 | 671 | 3 | 223 | 161 | 479 |
| 65 | rgb4 | 3 | 3583 | 895 | 4 | 223 | 161 | 640 |
| 97 | rgb8 | 0 | 8703 | 3263 | 13 | 251 | 226 | 2934 |

### 6.4 Interleaving

Symbol `s` of block `b` is written at position

```
s * blocks + b
```

in the coded symbol stream.

Without interleaving, a thumb over one corner would destroy one block outright
while leaving its neighbours untouched, and Reed-Solomon cannot repair a block
that is more than half gone regardless of how healthy the others are. With it,
any burst of `blocks` consecutive damaged symbols costs each block exactly one
symbol.

The coded symbol stream is then packed MSB first into the data cells, in the
order of Section 3.2.5, `bits_per_cell` bits per cell.

### 6.5 Decoding

A decoder MUST repair any combination of `e` errors and `f` erasures satisfying

```
2e + f <= n - k
```

A symbol's confidence is the minimum confidence of the cells that contributed
bits to it. A symbol whose confidence falls below the erasure threshold SHOULD
be reported to the decoder as an erasure. The RECOMMENDED threshold is `0.28`.

Because flagging more erasures than the parity budget guarantees failure even
when the data is repairable, an implementation MUST cap the erasures it declares
per block at `n - k`, keeping the least confident. If decoding with hints fails,
an implementation SHOULD retry once treating all damage as unknown errors, since
the confidence estimates may themselves have been wrong.

---

## 7. The fountain layer

### 7.1 Rationale

A camera drops frames. A screen cannot hear a retransmission request. A rateless
code removes the question entirely: the transmitter emits droplets forever and
the receiver stops when it has enough, whichever ones those were.

### 7.2 The PRNG

All pseudo-randomness is **SplitMix64**, chosen because it is a fixed sequence
of 64-bit wrapping operations with no platform-dependent behaviour and is about
ten lines to reimplement anywhere.

```
next(state):
    state = state + 0x9E3779B97F4A7C15        (mod 2^64)
    z = state
    z = (z XOR (z >> 30)) * 0xBF58476D1CE4E5B9  (mod 2^64)
    z = (z XOR (z >> 27)) * 0x94D049BB133111EB  (mod 2^64)
    return z XOR (z >> 31)
```

Seeding for a frame:

```
state = (session_id << 32) | frame_index
```

A uniform double in `[0, 1)` is `(next() >> 11) * 2^-53`. A uniform integer in
`[0, m)` is `next() mod m`; the resulting bias is of order `m / 2^64` and is
specified rather than corrected because every implementation must agree exactly.

Test vectors, seed 0:

```
E220A8397B1DCDAF  6E789E6AA1B965F4  06C45D188009454F  F88BB8A8724C81EC
```

Session `0x1234`, frame 7:

```
D4C018AD4E9409EE  FFDA285B4166C43A  751E866FDE109F3F  27BA1389A1765190
```

### 7.3 The degree distribution

The robust soliton distribution over degrees `1..K`, with `c = 0.1` and
`delta = 0.05` fixed in version 1. **These parameters are not carried on the
wire.** An implementation MUST use these values.

Ideal soliton:

```
rho(1) = 1/K
rho(i) = 1 / (i * (i - 1))          for 2 <= i <= K
```

Robust component, with `R = c * ln(K / delta) * sqrt(K)` and
`pivot = floor(K / R)`:

```
tau(i) = R / (i * K)                for 1 <= i < pivot
tau(pivot) = R * ln(R / delta) / K  if pivot <= K
tau(i) = 0                          otherwise
```

The distribution is `mu(i) = (rho(i) + tau(i)) / Z` where `Z` is the sum. The
cumulative distribution `cdf[i-1] = sum of mu(1..i)`, with `cdf[K-1]` forced to
exactly 1 to absorb rounding.

For `K = 1` the distribution is degenerate: `cdf = [1.0]`.

Verified values:

```
K=4    cdf[0..4] = 0.231181267 0.531578400 0.649706933 1.000000000
K=16   cdf[0..6] = 0.109842721 0.413853469 0.527962186 0.591402444 0.633298139 0.944646803
K=64   cdf[0..6] = 0.062315958 0.385419582 0.501962097 0.564654009 0.604921547 0.633534835
```

### 7.4 Deterministic transcendental functions

`ln` and `sqrt` above are the only floating-point functions PZ depends on, and
two implementations that disagree by one bit can compute different degree tables,
select different blocks for the same frame, and fail to interoperate.

Platform math libraries are **not** bit-reproducible across targets. IEEE-754
`+`, `-`, `*` and `/` **are** exactly specified. Implementations MUST therefore
compute these functions using only those four operations, as follows.

**Square root**, by Newton-Raphson from a bit-pattern estimate:

```
if x == 0 or x is infinite: return x
guess = bits_to_double((double_to_bits(x) >> 1) + 0x1FF8000000000000)
repeat 6 times: guess = 0.5 * (guess + x / guess)
```

**Natural logarithm**, by decomposing `x = m * 2^e` with `m` in `[1, 2)` and
summing the `atanh` series:

```
s = (m - 1) / (m + 1)
ln(m) = 2 * sum over i in [0, 24) of s^(2i+1) / (2i + 1)
ln(x) = ln(m) + e * ln(2)
ln(2) = 0.693147180559945309417232121458176568
```

Subnormal inputs are first scaled by `2^53` and the exponent compensated.

### 7.5 Blocks and droplets

The container (Section 8.1) is split into `K = ceil(container_length /
droplet_size)` blocks of `droplet_size` bytes, the last zero-padded.

The set of blocks mixed into frame `i` is:

```
if i < K:
    plan = [i]                                  # systematic prefix
else:
    rng = SplitMix64(session_id, i)
    d   = smallest degree with cdf[d-1] > rng.next_double(), clamped to [1, K]
    plan = d distinct indices, each drawn as rng.next() mod K,
           rejecting and redrawing on a repeat
```

The droplet is the XOR of the planned blocks.

**Frames `0` through `K-1` are systematic**: frame `i` carries block `i`
verbatim. A clean capture therefore completes in exactly `K` frames with no
coding overhead, and the fountain is paid for only when frames are actually lost.

Verified plans for session `0x0001`, `K = 16`:

```
frame 0    -> [0]
frame 15   -> [15]
frame 16   -> [7, 8, 9, 10, 13, 14]
frame 17   -> [8, 10]
frame 18   -> [1, 4, 6, 15]
frame 100  -> [1, 3, 7, 8, 10, 14]
```

### 7.6 Session isolation

The systematic prefix is **independent of the session id**: frame `i` carries
block `i` for every session. A receiver that absorbed another transmitter's
systematic frames would therefore corrupt its own decode.

A receiver MUST discard any frame whose header session id differs from the
session it is currently receiving. This check is the only thing making the
shared prefix safe, and it MUST be performed before the droplet reaches the
fountain decoder.

### 7.7 Decoding

The receiver runs the standard peeling algorithm: reduce each incoming droplet
by XORing out every block already known; if one unknown block remains, that
droplet *is* that block, which may unlock further droplets in a cascade.

Droplets may arrive in any order, may be duplicated, and may be missing.

---

## 8. Payload framing

### 8.1 The container

```
container = CRC-32(message) || message
```

with the checksum big endian. `payload_len` in the header is the length of the
**container**, that is `4 + len(message)`.

After the fountain layer completes, a receiver MUST verify this checksum before
delivering anything. The fountain can in principle converge on bytes that are
not the message if a corrupted droplet slipped through; returning those silently
would be far worse than reporting the failure.

### 8.2 The frame payload seal

```
frame_payload = droplet || CRC-32(droplet || frame_index_be)
```

The checksum is bound to the frame index, not merely to the droplet bytes. A
droplet is meaningless without the index that says which blocks it mixes; a
header that was repaired to the wrong index would otherwise poison the fountain
decoder silently.

A receiver MUST verify this before absorbing the droplet.

### 8.3 CRC-32

CRC-32/ISO-HDLC, as used by zlib, PNG and Ethernet: reflected polynomial
`0xEDB88320`, initial value `0xFFFFFFFF`, final XOR `0xFFFFFFFF`.

Check value: `crc32("123456789") = 0xCBF43926`.

### 8.4 Session identifier

The session id distinguishes concurrent transmissions in one field of view. A
transmitter MAY choose it freely. The reference implementation derives a stable
one from the message so that re-encoding the same bytes produces the same
stream:

```
c = CRC-32(message)
session_id = (c XOR (c >> 16)) mod 2^16
```

This is a convention, not a requirement.

---

## 9. Transmission

A transmitter renders frame `0`, then frame `1`, and so on without bound,
incrementing the frame index. It MUST NOT stop at frame `K`; the frames beyond
the systematic prefix are what makes the link resilient.

Frame indices MAY wrap, but a session SHOULD NOT exceed `2^32` frames.

The display rate is not specified. Higher rates raise throughput and lower the
chance any given frame is captured cleanly.

---

## 10. Reception

A receiver is offered images. Most will be unusable — this is normal, not an
error condition.

### 10.1 Locating the frame

1. Convert to greyscale.
2. Binarise with a **local** threshold. A single global threshold fails on
   screen captures: a reflection in one corner, a lens vignette, or an uneven
   backlight all shift the correct cutoff across the image. The reference
   implementation compares each pixel against the mean of a window around it,
   computed from a summed-area table.
3. Scan every row for five consecutive runs in the ratio 1:1:3:1:1, starting
   dark. Confirm each candidate by repeating the test down the column through
   it, then back across the row, and require the horizontal and vertical module
   estimates to agree. Cluster the surviving hits.
4. Four clusters are expected. If more are found, choose the four whose
   quadrilateral has the most nearly equal opposite sides and diagonals.

### 10.2 Establishing the transform

Order the four centres into a consistent cyclic sequence with a known winding.
The longest of the six pairwise distances is a diagonal of the quadrilateral, so
alternating the two diagonals walks the perimeter; normalise the winding by the
sign of the shoelace sum.

Estimate the grid size from the mean side length:

```
N_estimate = mean_side_px / module_px + 7
```

and snap to the nearest supported size.

Solve the projective transform carrying the four grid-space reference points of
Section 3.4 onto the four image-space centres. A projective transform is
REQUIRED, not affine: a camera at an angle produces genuine foreshortening,
which three points cannot express.

### 10.3 Resolving rotation

All four finders are identical, so which image corner is the frame's top left is
unknown. A receiver MUST try all four rotations of the correspondence and accept
whichever produces a header that passes RS(32,16) and its CRC-16.

This is cheaper and stricter than any geometric tie-break, and has the useful
consequence that a frame decodes at any orientation, including upside down.

### 10.4 Sampling

Read each cell at its centre in grid coordinates, mapped through the transform.
Implementations SHOULD average several sub-samples over the middle of the cell
rather than reading a single pixel: it suppresses sensor noise, and staying away
from the edges prevents a neighbouring cell bleeding in after an imperfect fit.
The reference implementation averages a 3 by 3 pattern spanning the middle 56%
of the cell.

### 10.5 Full procedure

1. Locate, order, and establish the transform (10.1, 10.2).
2. For each candidate rotation and grid size:
   a. Sample every cell.
   b. Calibrate from the eight patches (Section 4.3).
   c. Demodulate the 256 header cells as `Mono`, repair with RS(32,16), verify
      the CRC-16, and validate every enumerated field.
   d. If the header's grid size disagrees with the sampling grid, reject this
      candidate: the data cells are misaligned.
3. Derive the capacity plan from the header (Section 6.3).
4. Demodulate the data cells in the header's colour mode, recording confidence.
5. De-interleave and repair each Reed-Solomon block (Section 6.5).
6. Verify the frame seal against the header's frame index (Section 8.2).
7. Discard the frame if its session id differs from the session in progress
   (Section 7.6); otherwise absorb the droplet.
8. When the fountain completes, verify the container checksum (Section 8.1)
   before delivering the message.

---

## 11. Security considerations

**PZ is a transport. It is not a security protocol.** It provides:

- **No confidentiality.** Frames are plainly visible. Anyone who can see, or
  photograph, or screen-record the display can decode the payload. There is no
  encryption anywhere in this specification.
- **No authenticity.** Any display can emit a valid stream. Nothing in a frame
  attests to its origin.
- **No integrity against a deliberate attacker.** The CRCs and Reed-Solomon
  detect accidental corruption. They are not message authentication codes, and
  an attacker who controls the display can produce any payload with valid
  checksums.
- **No replay protection.** A recording decodes exactly like the original.

Applications MUST encrypt and authenticate the payload before handing it to PZ,
exactly as they would before placing it on an untrusted network.

### 11.1 What the physical channel does provide

Visible light does not pass through walls. Successfully decoding a live stream
is evidence of line of sight to the display at the time of transmission, and no
network stack, radio, pairing or listening socket is involved.

This property is weaker than it first appears. A camera pointed through a
window, a mirror, a telephoto lens, or a screen recorder running on the
transmitting host all defeat it. "Air-gapped" describes the network, not the
room.

### 11.2 Denial of service

A receiver's work is bounded by the image size and the announced frame
parameters, but a hostile transmitter can waste a receiver's processing time.
`payload_len` is attacker-controlled and drives allocation; implementations MUST
impose a ceiling. The reference implementation uses 64 MiB.

### 11.3 Side channels

Nothing in this specification is constant-time. The Galois field arithmetic uses
lookup tables. Implementations SHOULD NOT process secret material with a PZ
codec on a platform where timing or cache side channels matter.

---

## 12. Registries

### 12.1 Wire format versions

| Version | Status |
|---:|---|
| 0 | Reserved |
| 1 | This document |
| 2-15 | Unassigned |

### 12.2 Colour modes

| Code | Name | Status |
|---:|---|---|
| 0 | `Mono` | This document |
| 1 | `Rgb4` | This document |
| 2 | `Rgb8` | This document |
| 3-15 | | Unassigned |

### 12.3 Grid sizes

| Code | `N` | Status |
|---:|---:|---|
| 0-4 | 33, 49, 65, 81, 97 | This document |
| 5-15 | | Unassigned |

### 12.4 Parity codes

| Code | Ratio |
|---:|---:|
| 0-7 | 0.10, 0.16, 0.22, 0.28, 0.34, 0.40, 0.50, 0.60 |
| 8-15 | Unassigned |

### 12.5 Extension policy

The reserved header byte and the unassigned enumerated values are the intended
extension points. Because the header CRC covers all of them, a version 1
receiver rejects a frame using an unassigned value rather than misinterpreting
it.

Any change to frame geometry, the header layout, the capacity derivation, the
degree distribution or the PRNG is a **breaking change** and requires a new wire
format version.

---

## 13. Conformance

An implementation claiming conformance to version 1 MUST:

1. Reproduce every vector in `cargo run -p pz-core --example vectors`.
2. Interoperate with the reference implementation in both directions, across
   every combination of grid size, colour mode and parity code.
3. Perform the session isolation check of Section 7.6.
4. Verify both checksums of Section 8 before delivering a message.
5. Reject unassigned enumerated values.

An implementation MAY omit confidence-driven erasure marking (Section 4.4) and
remain interoperable, at roughly half the damage tolerance.

---

## 14. References

- L. Luby, "LT Codes", *Proc. 43rd Annual IEEE Symposium on Foundations of
  Computer Science*, 2002.
- A. Shokrollahi, "Raptor Codes", *IEEE Transactions on Information Theory*,
  2006.
- ISO/IEC 18004, *QR Code bar code symbology specification* — the source of the
  finder pattern approach and the GF(2^8) field polynomial used here.
- S. Bradner, "Key words for use in RFCs to Indicate Requirement Levels",
  RFC 2119, 1997.
