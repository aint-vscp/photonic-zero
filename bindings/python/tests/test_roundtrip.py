"""Tests for the Photonic Zero Python bindings.

Run with: pytest

SPDX-License-Identifier: MIT OR Apache-2.0
"""

import pytest

import photonic_zero as pz


def payload_of(length: int) -> bytes:
    return bytes((i * 37 + 11) & 0xFF for i in range(length))


def transfer(payload: bytes, *, profile: str = "balanced", keep=lambda i: True):
    """Drive a full transfer through the real optical path, dropping frames."""
    encoder = pz.encode(payload, profile=profile)
    decoder = pz.Decoder()

    for index in range(20000):
        if not keep(index):
            continue
        width, height, pixels = encoder.rgb(index, module_px=4)
        if decoder.ingest(width, height, pixels).complete:
            return decoder.result, decoder.frames_accepted, encoder.block_count
    raise AssertionError("never converged")


def test_module_metadata():
    assert pz.PROTOCOL_VERSION == 1
    assert isinstance(pz.__version__, str)
    assert pz.PROFILES == ("robust", "balanced", "resilient", "fast")


def test_round_trips_a_short_message():
    message = b"photonic zero over python"
    recovered, _, blocks = transfer(message)
    assert recovered == message
    assert blocks == 1


def test_round_trips_a_multi_frame_payload():
    payload = payload_of(4096)
    recovered, _, blocks = transfer(payload)
    assert recovered == payload
    assert blocks > 1


def test_recovers_when_frames_are_dropped():
    payload = payload_of(4096)
    recovered, _, _ = transfer(payload, keep=lambda i: i % 3 != 2)
    assert recovered == payload


@pytest.mark.parametrize("profile", pz.PROFILES)
def test_every_profile_round_trips(profile):
    payload = payload_of(2048)
    recovered, _, _ = transfer(payload, profile=profile)
    assert recovered == payload


def test_ingest_frame_bypasses_the_camera_path():
    payload = payload_of(1000)
    encoder = pz.encode(payload)
    decoder = pz.Decoder()

    for index in range(64):
        if decoder.ingest_frame(encoder.frame(index)).complete:
            break
    assert decoder.result == payload


def test_encoder_exposes_a_sensible_plan():
    encoder = pz.encode(payload_of(10000))
    assert encoder.modules == 49
    assert encoder.droplet_size == 479
    assert encoder.payload_len == 10000
    assert encoder.block_count > 1
    assert 0 <= encoder.session_id <= 0xFFFF
    assert encoder.estimated_seconds(30.0, 0.8) > 0


def test_png_output_is_a_png():
    encoder = pz.encode(b"png please")
    data = encoder.png(0, module_px=4)
    assert data[:8] == b"\x89PNG\r\n\x1a\n"


def test_frame_accessors():
    encoder = pz.encode(b"frame accessors")
    frame = encoder.frame(7)
    assert frame.index == 7
    assert frame.modules == 49
    assert len(frame.cells) == 49 * 49
    assert len(frame.rgb()) == 49 * 49 * 3
    assert "index=7" in repr(frame)


def test_derived_session_is_stable_and_content_dependent():
    assert pz.encode(b"same").session_id == pz.encode(b"same").session_id
    assert pz.encode(b"same").session_id != pz.encode(b"other").session_id


def test_pinned_session_is_honoured():
    assert pz.encode(b"pinned", session_id=0x1234).session_id == 0x1234


def test_status_is_truthy_only_when_complete():
    encoder = pz.encode(payload_of(20000))
    decoder = pz.Decoder()
    status = decoder.ingest_frame(encoder.frame(0))
    assert not status
    assert not status.complete
    assert status.kind == pz.ProgressKind.Progressed
    assert 0 < status.fraction < 1


def test_decoder_reset_allows_a_new_session():
    decoder = pz.Decoder()
    first = pz.encode(b"first message")
    assert decoder.ingest_frame(first.frame(0)).complete
    assert decoder.result == b"first message"

    decoder.reset()
    assert decoder.session_id is None

    second = pz.encode(b"second message")
    assert decoder.ingest_frame(second.frame(0)).complete
    assert decoder.result == b"second message"


def test_empty_payload_is_rejected():
    with pytest.raises(pz.PhotonicZeroError):
        pz.encode(b"")


def test_unknown_profile_is_rejected():
    with pytest.raises(ValueError, match="unknown profile"):
        pz.encode(b"x", profile="nonsense")


def test_short_buffer_is_rejected_rather_than_read_out_of_bounds():
    decoder = pz.Decoder()
    with pytest.raises(Exception, match="bytes"):
        decoder.ingest(1000, 1000, b"\x00" * 16)


def test_bad_channel_count_is_rejected():
    decoder = pz.Decoder()
    with pytest.raises(ValueError, match="channels"):
        decoder.ingest(4, 4, b"\x00" * 100, channels=2)


def test_rgba_input_is_accepted():
    encoder = pz.encode(b"rgba path")
    decoder = pz.Decoder()
    width, height, rgb = encoder.rgb(0, module_px=4)

    rgba = bytearray()
    for i in range(0, len(rgb), 3):
        rgba += rgb[i : i + 3]
        rgba.append(255)

    assert decoder.ingest(width, height, bytes(rgba), channels=4).complete
    assert decoder.result == b"rgba path"


def test_noise_does_not_decode():
    decoder = pz.Decoder()
    side = 200
    noise = bytes((i * 97 + 13) & 0xFF for i in range(side * side * 3))
    status = decoder.ingest(side, side, noise)
    assert status.kind in (pz.ProgressKind.NotFound, pz.ProgressKind.Rejected)
    assert decoder.result is None


def test_counters_are_correct_on_completion_and_survive_a_miss():
    """`recovered`/`total` must mean what the docstring says at every stage.

    Reading them off the `Progress` variant meant a routine `NotFound`
    reported 0/0 mid-transfer, and `Complete` reported 0/0 at the one moment
    a caller most wants the numbers.
    """
    encoder = pz.Encoder(b"\x5a" * 4096)
    decoder = pz.Decoder()

    status = None
    for index in range(64):
        width, height, pixels = encoder.rgb(index, module_px=4)
        status = decoder.ingest(width, height, pixels)
        if status.complete:
            break

    assert status is not None and status.complete, "transfer never completed"
    assert status.total > 1
    assert status.recovered == status.total
    assert status.fraction == 1.0

    # A blank image finds nothing, and must not zero the counters.
    side = 200
    miss = decoder.ingest(side, side, b"\xff" * (side * side * 3))
    assert miss.kind == pz.ProgressKind.NotFound
    assert miss.total == status.total
    assert miss.recovered == status.recovered


def test_counters_track_a_partial_transfer():
    """Mid-transfer the counters must be real, not zero."""
    encoder = pz.Encoder(b"\x11" * 4096)
    decoder = pz.Decoder()

    width, height, pixels = encoder.rgb(0, module_px=4)
    status = decoder.ingest(width, height, pixels)

    assert status.kind == pz.ProgressKind.Progressed
    assert not status.complete
    assert status.total > 1
    assert 0 < status.recovered < status.total


def test_profile_info():
    modules, bits, cells, droplet = pz.profile_info("fast")
    assert modules == 97
    assert bits == 3
    assert cells == 8703
    assert droplet == 2739
    with pytest.raises(ValueError):
        pz.profile_info("nope")
