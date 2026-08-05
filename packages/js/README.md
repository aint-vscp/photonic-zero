# photonic-zero

**Data over light, from any screen to any camera.** Zero radio, zero pairing,
zero network.

Photonic Zero encodes bytes into a stream of colour frames, displays them, and
reconstructs the message from a camera capture. Unlike a QR code, a PZ
transmission has no fixed length and needs no back channel — a screen cannot
hear, so PZ uses a rateless fountain code and the receiver simply watches until
it has enough frames, whichever ones those happened to be.

This package is the WebAssembly build. No dependencies, ~100 kB of wasm, and it
works in browsers and in Node.

```console
npm install photonic-zero
```

## Command line

No install needed:

```console
$ npx photonic-zero selftest

$ npx photonic-zero encode secret.txt -o frames --profile robust
encoded 86 bytes
  grid        33x33 cells (robust)
  per frame   34 bytes
  minimum     3 frames
  written     8 frames to frames

$ npx photonic-zero decode frames -o recovered.txt
recovered 86 bytes from 3 of 3 images
```

## Sending, in a browser

```js
import { load } from 'photonic-zero';

const pz = await load(fetch(new URL('photonic-zero/pz.wasm', import.meta.url)));
const encoder = pz.encode('transfer me over light');

const canvas = document.querySelector('canvas');
const ctx = canvas.getContext('2d');

// The stream is endless: keep drawing until the receiver says it is done.
let index = 0;
setInterval(() => {
  const { width, height, data } = encoder.frameRGBA(index++, { modulePx: 8 });
  canvas.width = width;
  canvas.height = height;
  ctx.putImageData(new ImageData(data, width, height), 0, 0);
}, 1000 / 15);
```

## Receiving, from a camera

```js
import { load, ProgressKind } from 'photonic-zero';

const pz = await load(fetch(new URL('photonic-zero/pz.wasm', import.meta.url)));
const decoder = pz.decoder();

const stream = await navigator.mediaDevices.getUserMedia({ video: true });
const video = Object.assign(document.createElement('video'), { srcObject: stream });
await video.play();

const canvas = document.createElement('canvas');
const ctx = canvas.getContext('2d', { willReadFrequently: true });

function tick() {
  canvas.width = video.videoWidth;
  canvas.height = video.videoHeight;
  ctx.drawImage(video, 0, 0);
  const { data, width, height } = ctx.getImageData(0, 0, canvas.width, canvas.height);

  const status = decoder.ingestRGBA(width, height, data);
  if (status.kind === ProgressKind.Complete) {
    console.log('received', new TextDecoder().decode(decoder.result()));
    return;
  }
  // NotFound and Rejected are routine: most frames of a hand-held camera are
  // unusable, which is exactly why the code is rateless.
  requestAnimationFrame(tick);
}
tick();
```

## In Node

`load()` with no argument reads the bundled module from disk:

```js
import { load } from 'photonic-zero';
import { readFileSync, writeFileSync } from 'node:fs';

const pz = await load();
const encoder = pz.encode(readFileSync('payload.bin'));

for (let i = 0; i < encoder.blockCount * 2; i++) {
  writeFileSync(`frame${i}.png`, encoder.framePNG(i));
}
```

## Profiles

| Profile | Grid | Bits/cell | Bytes/frame | Use when |
|---|---:|---:|---:|---|
| `robust` | 33 | 1 | 34 | Bad light, poor camera, long range |
| `balanced` | 49 | 3 | 479 | The default |
| `resilient` | 65 | 2 | 640 | You want per-cell error detection |
| `fast` | 97 | 3 | 2739 | Close, steady, well-lit capture |

`fast` is roughly 660 kbit/s at 30 fps. Throughput assumes every displayed frame
is captured; a real camera misses some, and the fountain code absorbs 25% frame
loss for about 1.0x to 1.4x the minimum frame count.

## API

```ts
const pz = await load(source?);            // source: omit in Node, fetch() in a browser
pz.protocolVersion                          // wire format version (1)

const encoder = pz.encode(payload, { profile, sessionId });
encoder.blockCount                          // minimum frames under perfect conditions
encoder.modules, encoder.dropletSize, encoder.sessionId
encoder.frameRGBA(i, { modulePx, quietZone })   // { width, height, data }
encoder.framePNG(i, { modulePx, quietZone })    // Uint8Array
encoder.frameCells(i)                       // one colour code per cell
encoder.free();

const decoder = pz.decoder();
decoder.ingestRGBA(width, height, data);    // -> { kind, complete, recovered, total, fraction }
decoder.ingestPNG(bytes);
decoder.progress, decoder.framesSeen, decoder.framesAccepted, decoder.sessionId
decoder.result();                           // Uint8Array or null
decoder.reset();
decoder.free();
```

`free()` is optional but recommended in long-running code: handles live in
WebAssembly memory, which JavaScript's garbage collector cannot see.

TypeScript definitions are included.

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
