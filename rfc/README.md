# Photonic Zero RFCs

This directory holds the normative specifications for the PZ wire format. The
Rust code in `crates/` is the *reference implementation*; these documents are
the definition. Where they disagree, that is a defect and should be reported as
an issue.

## Index

| RFC | Title | Status |
|---|---|---|
| [0001](RFC-0001-pz-frame-format.md) | The Photonic Zero Frame Format | Draft |

## Why a specification at all

PZ is a protocol before it is a library. The point is that an implementation in
Swift, Go, C or a hardware description language can interoperate with this one,
and that is only true if there is something to implement *against* other than
several thousand lines of Rust.

The RFC therefore states the wire format independently of any implementation
choice: bit orders, field layouts, exact algorithms for the parts where "any
reasonable method" is not good enough, and conformance vectors to check against.

## Conformance vectors

The vectors quoted in RFC-0001 are generated from the reference implementation:

```console
$ cargo run -p pz-core --example vectors
```

An independent implementation should reproduce that output exactly. CI runs it
on every push, so a change to the wire format is visible in the diff.

## Changing a specification

Any change to what goes over the link — frame geometry, header fields, colour
codes, capacity derivation, the fountain degree distribution, the PRNG — needs,
in the same pull request:

1. The RFC updated.
2. Conformance vectors regenerated if affected.
3. A `CHANGELOG.md` entry.
4. Tests that would have failed before the change.

While PZ is pre-1.0 the wire format may change in any minor release. Version 1.0
will freeze it, after which a breaking change requires a new wire format version
number and a new RFC.

## Proposing a new RFC

Open an issue describing the problem first. Specifications are expensive to
change once implementations exist, so the discussion is worth having before the
document is written.

Number new documents sequentially and follow the structure of RFC-0001: an
abstract, terminology, normative sections using RFC 2119 keywords, security
considerations, and a registry section for anything enumerated.
