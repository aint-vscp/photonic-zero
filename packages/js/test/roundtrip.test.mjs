/**
 * Tests for the JavaScript bindings.
 *
 * Run with: node --test test/
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { load, ProgressKind, PzError } from '../src/index.mjs';

const pz = await load();

function payloadOf(length) {
  const bytes = new Uint8Array(length);
  for (let i = 0; i < length; i++) bytes[i] = (i * 37 + 11) & 0xff;
  return bytes;
}

/** Drive a full transfer, optionally dropping frames. */
function transfer(payload, { profile = 'balanced', keep = () => true, modulePx = 4 } = {}) {
  const encoder = pz.encode(payload, { profile });
  const decoder = pz.decoder();
  try {
    for (let index = 0; index < 20000; index++) {
      if (keep(index)) {
        const { width, height, data } = encoder.frameRGBA(index, { modulePx });
        if (decoder.ingestRGBA(width, height, data).complete) {
          return { bytes: decoder.result(), frames: decoder.framesAccepted, encoder, decoder };
        }
      }
    }
    throw new Error('never converged');
  } finally {
    encoder.free();
    decoder.free();
  }
}

test('module reports the wire format version', () => {
  assert.equal(pz.protocolVersion, 1);
});

test('round trips a short string through the optical path', () => {
  const message = 'photonic zero over javascript';
  const encoder = pz.encode(message);
  const decoder = pz.decoder();

  let out = null;
  for (let index = 0; index < 64 && out === null; index++) {
    const { width, height, data } = encoder.frameRGBA(index, { modulePx: 4 });
    if (decoder.ingestRGBA(width, height, data).complete) out = decoder.result();
  }

  assert.ok(out, 'decoding never completed');
  assert.equal(new TextDecoder().decode(out), message);
  encoder.free();
  decoder.free();
});

test('round trips a multi-frame binary payload', () => {
  const payload = payloadOf(4096);
  const { bytes } = transfer(payload);
  assert.deepEqual(bytes, payload);
});

test('recovers when one frame in three is dropped', () => {
  const payload = payloadOf(4096);
  const { bytes } = transfer(payload, { keep: (i) => i % 3 !== 2 });
  assert.deepEqual(bytes, payload);
});

test('every profile round trips', () => {
  const payload = payloadOf(2048);
  for (const profile of ['balanced', 'robust', 'fast', 'resilient']) {
    const { bytes } = transfer(payload, { profile });
    assert.deepEqual(bytes, payload, `profile ${profile} failed`);
  }
});

test('PNG frames decode back', () => {
  const payload = payloadOf(1500);
  const encoder = pz.encode(payload);
  const decoder = pz.decoder();

  let out = null;
  for (let index = 0; index < 64 && out === null; index++) {
    const png = encoder.framePNG(index, { modulePx: 4 });
    assert.deepEqual(
      Array.from(png.slice(0, 8)),
      [137, 80, 78, 71, 13, 10, 26, 10],
      'not a PNG',
    );
    if (decoder.ingestPNG(png).complete) out = decoder.result();
  }

  assert.deepEqual(out, payload);
  encoder.free();
  decoder.free();
});

test('encoder exposes a sensible plan', () => {
  const encoder = pz.encode(payloadOf(10000), { profile: 'balanced' });
  assert.equal(encoder.modules, 49);
  assert.equal(encoder.dropletSize, 479);
  assert.ok(encoder.blockCount > 1);
  assert.ok(encoder.sessionId >= 0 && encoder.sessionId <= 0xffff);
  encoder.free();
});

test('a pinned session id is honoured', () => {
  const encoder = pz.encode('pinned', { sessionId: 0x1234 });
  assert.equal(encoder.sessionId, 0x1234);
  encoder.free();
});

test('the same payload yields the same derived session', () => {
  const a = pz.encode('same bytes');
  const b = pz.encode('same bytes');
  const c = pz.encode('other bytes');
  assert.equal(a.sessionId, b.sessionId);
  assert.notEqual(a.sessionId, c.sessionId);
  for (const e of [a, b, c]) e.free();
});

test('frames render at the expected size', () => {
  const encoder = pz.encode('geometry');
  const { width, height, data } = encoder.frameRGBA(0, { modulePx: 6, quietZone: 4 });
  const expected = (49 + 8) * 6;
  assert.equal(width, expected);
  assert.equal(height, expected);
  assert.equal(data.length, expected * expected * 4);
  encoder.free();
});

test('random noise does not decode', () => {
  const decoder = pz.decoder();
  const side = 200;
  const data = new Uint8Array(side * side * 4);
  for (let i = 0; i < data.length; i++) data[i] = (i * 2654435761) & 0xff;
  const status = decoder.ingestRGBA(side, side, data);
  assert.ok(status.kind === ProgressKind.NotFound || status.kind === ProgressKind.Rejected);
  assert.equal(decoder.result(), null);
  decoder.free();
});

test('an empty payload is rejected', () => {
  assert.throws(() => pz.encode(new Uint8Array(0)), PzError);
});

test('an unknown profile is rejected with a helpful message', () => {
  assert.throws(
    () => pz.encode('x', { profile: 'nonsense' }),
    (error) => error instanceof PzError && /unknown profile/.test(error.message),
  );
});

test('using a freed handle throws rather than crashing', () => {
  const encoder = pz.encode('freed');
  encoder.free();
  encoder.free(); // must be idempotent
  assert.throws(() => encoder.frameRGBA(0), PzError);
});

test('a decoder can be reset for a new session', () => {
  const first = pz.encode('first message');
  const second = pz.encode('second message');
  const decoder = pz.decoder();

  const a = first.frameRGBA(0, { modulePx: 4 });
  decoder.ingestRGBA(a.width, a.height, a.data);
  assert.ok(decoder.result());

  decoder.reset();
  assert.equal(decoder.sessionId, null);

  const b = second.frameRGBA(0, { modulePx: 4 });
  decoder.ingestRGBA(b.width, b.height, b.data);
  assert.equal(new TextDecoder().decode(decoder.result()), 'second message');

  first.free();
  second.free();
  decoder.free();
});

test('a bad image length is reported rather than read out of bounds', () => {
  const decoder = pz.decoder();
  assert.throws(() => decoder.ingestRGBA(1000, 1000, new Uint8Array(16)), PzError);
  decoder.free();
});
