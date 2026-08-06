# photonic-zero

**Data over light, from any screen to any camera.** Zero radio, zero pairing,
zero network.

Photonic Zero encodes bytes into a stream of colour frames, displays them, and
reconstructs the message from a camera capture. A transmission has no fixed
length and needs no back channel — a screen cannot hear, so PZ uses a rateless
fountain code and the receiver simply watches until it has enough frames,
whichever ones those happened to be.

```console
pip install photonic-zero
```

Pure wheels, no runtime dependencies. The core is Rust; the wheel contains a
compiled extension module.

## Sending

```python
import photonic_zero as pz

encoder = pz.encode(b"transfer me over light", profile="balanced")
print(encoder)
# <photonic_zero.Encoder modules=49 droplet=479 blocks=1 session=0xD4A6>

# The stream is endless, so "how many frames" is a resilience choice, not a
# correctness one. Half again the minimum tolerates a receiver that misses
# roughly a third of what it sees.
for index in range(encoder.block_count + encoder.block_count // 2 + 4):
    with open(f"frame{index:05}.png", "wb") as handle:
        handle.write(encoder.png(index, module_px=8))
```

## Receiving

```python
import photonic_zero as pz

decoder = pz.Decoder()

for width, height, pixels in captured_frames:      # RGB or RGBA bytes
    status = decoder.ingest(width, height, pixels)
    if status.complete:
        print(decoder.result)
        break
    print(f"{status.recovered}/{status.total} blocks")
```

`ProgressKind.NotFound` and `ProgressKind.Rejected` are routine, not errors:
most frames of a hand-held camera are unusable, which is exactly why the code
is rateless.

## With a camera

```python
import cv2
import photonic_zero as pz

decoder = pz.Decoder()
camera = cv2.VideoCapture(0)

while True:
    ok, frame = camera.read()
    if not ok:
        break
    rgb = cv2.cvtColor(frame, cv2.COLOR_BGR2RGB)
    height, width = rgb.shape[:2]

    if decoder.ingest(width, height, rgb.tobytes()).complete:
        print("received", len(decoder.result), "bytes")
        break
```

The decode releases the GIL, so it will not block other threads.

## Profiles

| Profile | Grid | Bits/cell | Bytes/frame | Use when |
|---|---:|---:|---:|---|
| `robust` | 33 | 1 | 34 | Bad light, poor camera, long range |
| `balanced` | 49 | 3 | 479 | The default |
| `resilient` | 65 | 2 | 640 | You want per-cell error detection |
| `fast` | 97 | 3 | 2739 | Close, steady, well-lit capture |

```python
>>> pz.profile_info("fast")
(97, 3, 8703, 2739)          # modules, bits/cell, data cells, bytes/frame
```

`fast` is roughly 660 kbit/s at 30 fps.

## API

```python
pz.encode(payload, *, profile="balanced", session_id=None) -> Encoder
pz.Encoder(payload, *, profile="balanced", session_id=None)
    .block_count            # minimum frames under perfect conditions
    .modules, .droplet_size, .session_id, .payload_len
    .frame(index)                                  -> Frame
    .png(index, *, module_px=8, quiet_zone=4)      -> bytes
    .rgb(index, *, module_px=8, quiet_zone=4)      -> (width, height, bytes)
    .estimated_seconds(fps=30.0, capture_ratio=0.8)

pz.Decoder()
    .ingest(width, height, data, *, channels=3)    -> Status
    .ingest_frame(frame)                           -> Status
    .progress, .frames_seen, .frames_accepted, .session_id
    .result                 # bytes, or None if not finished
    .reset()

pz.Status: .kind .complete .recovered .total .fraction
pz.ProgressKind: NotFound | Rejected | Progressed | Complete
pz.PhotonicZeroError
```

`Status` is truthy exactly when the message is complete, so
`if decoder.ingest(...):` reads naturally.

## Security

PZ is a **transport, not a security protocol**. It provides no confidentiality
and no authenticity: anyone who can see the screen can read the bytes. Encrypt
and sign your payload before handing it to PZ. What the physical channel gives
you is a link that does not traverse a network and cannot pass through a wall.

See [SECURITY.md](https://github.com/aint-vscp/photonic-zero/blob/main/SECURITY.md).

## Licence

MIT or Apache-2.0, at your option.

Full documentation, the protocol specification and the Rust implementation:
[github.com/aint-vscp/photonic-zero](https://github.com/aint-vscp/photonic-zero)
