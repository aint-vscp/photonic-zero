"""Photonic Zero: data over light, from any screen to any camera.

Photonic Zero encodes bytes into a stream of colour frames, displays them, and
reconstructs the message from a camera capture. A transmission has no fixed
length and needs no back channel: a screen cannot hear, so PZ uses a rateless
fountain code and the receiver watches until it has enough frames, whichever
ones those happened to be.

Sending::

    import photonic_zero as pz

    encoder = pz.encode(b"transfer me over light", profile="balanced")
    for index in range(encoder.block_count * 2):
        open(f"frame{index:03}.png", "wb").write(encoder.png(index))

Receiving::

    decoder = pz.Decoder()
    for image in captured_frames:            # RGB or RGBA bytes
        status = decoder.ingest(width, height, image)
        if status.complete:
            print(decoder.result)
            break

`ProgressKind.NotFound` and `ProgressKind.Rejected` are routine: most frames of
a hand-held camera are unusable, which is exactly why the code is rateless.

Photonic Zero is a *transport*, not a security protocol. It provides no
confidentiality and no authenticity; anyone who can see the screen can read the
bytes. Encrypt and sign your payload before handing it to PZ.
"""

from ._photonic_zero import (  # noqa: F401
    PROTOCOL_VERSION,
    Decoder,
    Encoder,
    Frame,
    PhotonicZeroError,
    ProgressKind,
    Status,
    __version__,
    encode,
    profile_info,
)

__all__ = [
    "PROTOCOL_VERSION",
    "Decoder",
    "Encoder",
    "Frame",
    "PhotonicZeroError",
    "ProgressKind",
    "Status",
    "__version__",
    "encode",
    "profile_info",
    "PROFILES",
]

#: The preset profiles, from most robust to fastest.
PROFILES = ("robust", "balanced", "resilient", "fast", "mono")
