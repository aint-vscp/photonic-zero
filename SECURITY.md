# Security Policy

## Reporting a vulnerability

Email **vashpuno2004@gmail.com** with `[photonic-zero security]` in the subject.
Please do not open a public issue for a vulnerability.

Include what you have: affected version or commit, a description, and a
reproduction if you have one. You will get an acknowledgement within 72 hours
and an assessment within 7 days. If a fix is warranted we will agree a
disclosure timeline with you, and you will be credited unless you prefer not to
be.

## Supported versions

| Version | Supported |
|---------|-----------|
| 0.1.x   | Yes       |

PZ is pre-1.0. Only the latest minor version receives fixes.

## What PZ is, and is not

This matters more than usual here, because "air-gapped optical transfer" sounds
like a security product and PZ is not one.

**PZ is a transport.** It moves bytes across a screen-to-camera gap. On its own
it provides:

- **No confidentiality.** The frames are plainly visible. Anyone who can see the
  screen, or photograph it, or find it in a screen recording, can decode the
  payload. There is no encryption anywhere in the format.
- **No authenticity.** Any display can emit a valid PZ stream. Nothing in a
  frame proves who produced it.
- **No integrity against a deliberate attacker.** The CRCs and Reed-Solomon
  detect *accidental* corruption. They are not MACs and an attacker who controls
  the display can produce any payload with valid checksums.
- **No replay protection.** A recording of a PZ stream decodes exactly like the
  original.

**Encrypt and sign your payload before handing it to PZ**, exactly as you would
before putting it on any untrusted network.

## What the physical channel does give you

- **Light does not pass through walls.** Decoding a stream is evidence of line of
  sight to the display at the time of transmission. That is a genuine property,
  and it is the reason to reach for PZ over a radio.
- **No network stack is involved.** No SSID, no pairing, no listening socket, no
  driver. The receiving device needs a camera and nothing else.
- **No RF emission.** Useful where radio is restricted rather than merely
  inconvenient.

Note that the first property is weaker than it sounds: a camera pointed through a
window, a mirror, or a screen recording on the transmitting host all defeat it.
"Air-gapped" describes the network, not the room.

## Implementation security

Within its scope, PZ tries to be a well-behaved library:

- `pz-fec`, `pz-fountain`, `pz-vision`, `pz-core` and `pz-cli` are
  `#![forbid(unsafe_code)]`.
- `pz-ffi` uses `unsafe` because a C ABI requires it. Every entry point
  null-checks its pointers, validates buffer lengths against the dimensions it
  was given, and catches panics at the boundary so that a Rust panic never
  unwinds into C.
- The core crates have **no external dependencies at all**, so the supply chain
  is the Rust standard library and nothing else.
- All decoder inputs are treated as hostile: lengths are checked before
  indexing, and a malformed frame is rejected rather than trusted.

Known gaps, stated rather than hidden:

- Nothing here is constant-time. The Galois field arithmetic uses lookup tables
  and is not intended to process secrets.
- The decoder's work is bounded by the image size and the frame parameters, but
  a hostile stream can waste a receiver's CPU. There is no rate limiting.
- Memory use scales with the announced payload length, which is bounded by
  `MAX_PAYLOAD_BYTES` (64 MiB) but is otherwise attacker-influenced.
