# Contributing to Photonic Zero

Thanks for looking. PZ is early, the wire format is still movable, and there is
a lot of interesting work left.

## Getting set up

```console
$ git clone https://github.com/aint-vscp/photonic-zero
$ cd photonic-zero
$ cargo test --workspace
```

On Windows, run `. .\scripts\dev-env.ps1` first. It puts the Rust toolchain and
a 64-bit mingw-w64 linker on `PATH` — note that a stale 32-bit MinGW.org install
will shadow the right compiler and fail at link time with `cannot find -lbcrypt`,
which the script detects and works around.

Before opening a pull request:

```console
$ cargo fmt --all
$ cargo clippy --workspace --all-targets -- -D warnings
$ cargo test --workspace
$ cargo check -p pz-fec -p pz-fountain -p pz-vision -p pz-core --no-default-features
$ cargo test --manifest-path crates/pz-ffi/Cargo.toml
```

The `no_std` check is not optional. Three of the four core crates support
`no_std` + `alloc`, and it is easy to break by reaching for a `std`-only float
method — `f64::round`, `floor`, `sqrt` and `ln` all live in `std`, not `core`.

## House style

**No emoji.** Not in code, comments, documentation, commit messages or issue
titles. Use words, or an inline SVG where a picture genuinely helps.

**Comments explain why, never what.** The code already says what it does.

```rust
// Bad: loop over the erasure positions and zero them
// Good: Erased symbols carry no information; zero them so they cannot bias
//       the syndromes.
```

If a piece of code exists because of a subtlety — a borrow-checker dance, a
numerical trap, a spec requirement, a bug someone hit — say so. Those comments
are the valuable ones.

**Every public item gets a doc comment**, and the crates deny missing docs.

**No `unwrap` or `expect` on a path a user can reach.** Return an error. Tests
and examples may unwrap freely.

**Errors are values.** The FFI layer is the only place that catches panics, and
it does so because unwinding across the C ABI is undefined behaviour.

## Tests

New behaviour needs tests. Tests should read as claims about the system, not as
exercises of the code:

```rust
#[test]
fn erasure_hints_double_the_repairable_damage() { ... }
#[test]
fn the_frame_crc_is_bound_to_the_frame_index() { ... }
```

If you fix a bug, the test should fail before the fix and pass after it. Say so
in the pull request.

The suite that matters most is `crates/pz-core/tests/optical_loop.rs`, which
decodes through a simulated camera. If you touch the vision pipeline, the frame
layout, or the demodulator, run it and expect to have to think about the result.

## Changing the wire format

PZ is a protocol before it is a library. Any change to what goes over the link —
frame geometry, header fields, colour codes, capacity derivation, the fountain
degree distribution, the PRNG — needs, in the same pull request:

1. An update to `rfc/RFC-0001-pz-frame-format.md`.
2. Updated conformance test vectors if the change affects them.
3. A note in `CHANGELOG.md`.

An implementation in another language must be able to interoperate from the RFC
alone. If the RFC and the Rust disagree, that is a bug in one of them, and which
one is a discussion worth having in an issue first.

Do not add an external crate dependency to `pz-fec`, `pz-fountain`, `pz-vision`,
`pz-core` or `pz-cli` without discussing it first. The zero-dependency core is a
deliberate property, not an accident.

## Good places to start

- Bindings: JavaScript/WebAssembly, Python, Java, Swift, Go, Dart. The C ABI in
  `include/photonic_zero.h` is the portability layer and is stable enough to
  target.
- A browser playground: one tab displays, another decodes from `getUserMedia`.
- A general PNG reader for the CLI. It currently reads only the stored-deflate
  subset it writes itself.
- Real-camera test corpora. Synthetic degradation is not the same as a real
  sensor, and a corpus of hard captures would be genuinely valuable.
- Performance. Nothing has been profiled yet.

## Pull requests

- Branch from `main`, keep the change focused, write a description that explains
  the reasoning rather than restating the diff.
- Sign off your commits (`git commit -s`) to certify the
  [Developer Certificate of Origin](https://developercertificate.org/).
- By contributing you agree that your work is dual licensed under MIT and
  Apache-2.0, matching the project.

## Conduct

See [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md). Security issues go to
[SECURITY.md](SECURITY.md), not to a public issue.
