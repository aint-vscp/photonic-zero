/**
 * Photonic Zero demo.
 *
 * Everything runs in this tab. There is no backend and no upload: the file is
 * read into an ArrayBuffer, encoded by the WebAssembly module, and painted to a
 * canvas. The receiving device decodes camera frames the same way.
 *
 * Memory is treated as a liability rather than a cache. Encoder and decoder
 * handles are wasm allocations that the garbage collector cannot see, so every
 * one of them is explicitly freed, and the recovered payload lives in an object
 * URL that is revoked on navigation, on tab hide, and before any new transfer.
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

import { load, ProgressKind, PzError } from './lib/index.mjs';

const $ = (id) => document.getElementById(id);
const MB = 1024 * 1024;

/** Everything holding wasm memory or a blob, so teardown is one call. */
const owned = { encoder: null, decoder: null, stageEncoder: null, url: null, stream: null };

let pz;

// --------------------------------------------------------------------- boot

try {
  pz = await load(fetch('./lib/pz.wasm'));
  $('masthead-meta').textContent = `wire format ${pz.protocolVersion}`;
} catch (error) {
  document.body.innerHTML =
    `<p style="padding:3rem;font-family:monospace">Could not load the WebAssembly module: ${error.message}</p>`;
  throw error;
}

// -------------------------------------------------------------- the stage
/**
 * The pinned canvas behind the prose, running a real encoder.
 *
 * This is the whole argument for not pre-rendering a video: what scrolls past
 * is the actual protocol, generating actual droplets from actual bytes.
 */

const stage = (() => {
  const decoder = pz.decoder();
  owned.decoder = decoder;

  // A small payload so the loop completes often enough to stay interesting.
  const payload = new Uint8Array(2048);
  crypto.getRandomValues(payload);
  const encoder = pz.encode(payload, { profile: 'balanced' });
  owned.stageEncoder = encoder;

  // Rendered but never painted: the numbers on the loss beat are only honest
  // if a real frame really went through a real decoder. Four pixels per cell
  // is the cheapest size that still decodes reliably.
  const MODULE_PX = 4;

  let index = 0;
  let dropRate = 0;
  let running = true;
  let last = 0;
  // Counted here, not on the decoder: a frame dropped in flight is never
  // offered, so the decoder has no way to know it existed. That gap is
  // precisely what the loss beat is trying to show.
  let emitted = 0;
  let lost = 0;

  const readout = $('loss-readout');
  const meter = $('loss-meter');

  function paint(now) {
    if (!running) return;
    // Deliberately slow. This exists to make the counters move at a readable
    // pace, not to animate anything, and it must not compete with the page.
    if (now - last > 220) {
      last = now;
      const frame = encoder.frameRGBA(index, { modulePx: MODULE_PX });

      // Feed the frame through a decoder, optionally dropping some,
      // so the readout on the "loss" beat is measuring a real transfer.
      emitted++;
      if (dropRate === 0 || Math.random() > dropRate) {
        const status = decoder.ingestRGBA(frame.width, frame.height, frame.data);
        if (status.kind === ProgressKind.Complete) {
          decoder.reset();
          emitted = 0;
          lost = 0;
          index = 0;
        }
      } else {
        lost++;
      }
      updateReadout();
      index++;
    }
    requestAnimationFrame(paint);
  }

  function updateReadout() {
    if (!readout.dataset.live) return;
    const set = (key, value) => { readout.querySelector(`[data-k="${key}"]`).textContent = value; };
    set('emitted', emitted);
    set('lost', lost);
    set('seen', decoder.framesSeen);
    set('blocks', `${Math.round(decoder.progress * encoder.blockCount)} / ${encoder.blockCount}`);
    meter.style.width = `${(decoder.progress * 100).toFixed(1)}%`;
  }

  requestAnimationFrame(paint);

  return {
    setDropRate(rate) { dropRate = rate; },
    liveReadout(on) { if (on) readout.dataset.live = '1'; else delete readout.dataset.live; },
    pause() { running = false; },
    resume() { if (!running) { running = true; requestAnimationFrame(paint); } },
  };
})();

// Scroll drives the stage. Each beat asks the encoder for something different.
//
// The callback tracks a ratio per beat and applies only the most visible one.
// Applying every intersecting entry in turn lets whichever happened to be last
// in the batch win, which during a fast scroll is not the beat on screen.
const beats = document.querySelectorAll('[data-beat]');
const ratios = new Map();
let current = null;

const observer = new IntersectionObserver(
  (entries) => {
    for (const entry of entries) {
      ratios.set(entry.target.dataset.beat, entry.isIntersecting ? entry.intersectionRatio : 0);
    }

    let best = null;
    let bestRatio = 0;
    for (const [beat, ratio] of ratios) {
      if (ratio > bestRatio) { best = beat; bestRatio = ratio; }
    }
    if (best === null || best === current) return;
    current = best;

    // Only run the sample transfer while its own beat is on screen. Off-screen
    // it is pure waste: nothing is displayed and no counter is being read.
    const onLossBeat = best === 'loss';
    stage.setDropRate(onLossBeat ? 0.25 : 0);
    stage.liveReadout(onLossBeat);
    if (onLossBeat) stage.resume(); else stage.pause();
  },
  { threshold: [0, 0.25, 0.5, 0.75, 1] },
);
for (const beat of beats) observer.observe(beat);

// ---------------------------------------------------------------- sending

const fileInput = $('file');
const drop = $('drop');
const plan = $('plan');
const transmit = $('transmit');

let pending = null; // { bytes, name }

/**
 * A one-byte container so the receiver knows whether to inflate.
 *
 * This is a demo-layer wrapper, not part of the protocol: PZ carries opaque
 * bytes and has no opinion about their contents. Compressing before
 * transmitting is the cheapest way to send more, because the optical channel
 * is thousands of times slower than the CPU on either end — spending
 * milliseconds to avoid seconds of transmission is always the right trade.
 */
const RAW = 0;
const GZIP = 1;

async function pack(bytes) {
  if (typeof CompressionStream !== 'function') {
    return { body: bytes, codec: RAW, ratio: 1 };
  }
  try {
    const stream = new Blob([bytes]).stream().pipeThrough(new CompressionStream('gzip'));
    const squeezed = new Uint8Array(await new Response(stream).arrayBuffer());
    // Incompressible input comes back slightly larger. Send the original then.
    if (squeezed.length >= bytes.length) return { body: bytes, codec: RAW, ratio: 1 };
    return { body: squeezed, codec: GZIP, ratio: bytes.length / squeezed.length };
  } catch {
    return { body: bytes, codec: RAW, ratio: 1 };
  }
}

async function unpack(bytes) {
  const codec = bytes[0];
  const body = bytes.subarray(1);
  if (codec !== GZIP) return body;
  const stream = new Blob([body]).stream().pipeThrough(new DecompressionStream('gzip'));
  return new Uint8Array(await new Response(stream).arrayBuffer());
}

function withHeader(codec, body) {
  const out = new Uint8Array(body.length + 1);
  out[0] = codec;
  out.set(body, 1);
  return out;
}

function describe(bytes) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < MB) return `${(bytes / 1024).toFixed(1)} kB`;
  return `${(bytes / MB).toFixed(2)} MB`;
}

async function buildPlan() {
  if (!pending) return;
  releaseEncoder();

  const profile = $('profile').value;
  const { body, codec, ratio } = await pack(pending.bytes);
  const wire = withHeader(codec, body);

  let encoder;
  try {
    encoder = pz.encode(wire, { profile });
  } catch (error) {
    if (!(error instanceof PzError)) throw error;
    alert(`Cannot encode that file: ${error.message}`);
    return;
  }
  owned.encoder = encoder;

  // Estimate from the rate actually being emitted, not the display refresh.
  // The 1.15 is fountain overhead; the 0.75 is the share of emitted frames a
  // hand-held camera really captures cleanly.
  const emitFps = Number($('rate').value);
  const seconds = (encoder.blockCount * 1.15) / emitFps / 0.75;

  plan.hidden = false;
  plan.querySelector('[data-k="size"]').textContent =
    ratio > 1.02
      ? `${describe(pending.bytes.length)} → ${describe(wire.length)} (${ratio.toFixed(1)}x)`
      : `${describe(pending.bytes.length)} · ${pending.name}`;
  plan.querySelector('[data-k="blocks"]').textContent = `${encoder.blockCount} frames min`;
  plan.querySelector('[data-k="session"]').textContent =
    `0x${encoder.sessionId.toString(16).toUpperCase().padStart(4, '0')}`;
  plan.querySelector('[data-k="eta"]').textContent =
    seconds < 90 ? `~${Math.ceil(seconds)}s` : `~${Math.ceil(seconds / 60)} min`;

  transmit.disabled = false;
}

function releaseEncoder() {
  if (owned.encoder) { owned.encoder.free(); owned.encoder = null; }
}

function accept(file) {
  file.arrayBuffer().then((buffer) => {
    const bytes = new Uint8Array(buffer);
    if (bytes.length === 0) { alert('That file is empty.'); return; }
    if (bytes.length > 4 * MB) {
      alert(`${describe(bytes.length)} would take a very long time. Try something under 4 MB.`);
      return;
    }
    pending = { bytes, name: file.name };
    drop.querySelector('.drop-primary').textContent = file.name;
    drop.querySelector('.drop-secondary').textContent = describe(bytes.length);
    buildPlan();
  });
}

fileInput.addEventListener('change', () => {
  if (fileInput.files[0]) accept(fileInput.files[0]);
});

for (const type of ['dragenter', 'dragover']) {
  drop.addEventListener(type, (event) => { event.preventDefault(); drop.classList.add('is-over'); });
}
for (const type of ['dragleave', 'drop']) {
  drop.addEventListener(type, () => drop.classList.remove('is-over'));
}
drop.addEventListener('drop', (event) => {
  event.preventDefault();
  if (event.dataTransfer.files[0]) accept(event.dataTransfer.files[0]);
});

$('sample').addEventListener('click', () => {
  // Random bytes, so nothing about the demo is compressible or pre-baked.
  const bytes = new Uint8Array(MB);
  for (let offset = 0; offset < bytes.length; offset += 65536) {
    crypto.getRandomValues(bytes.subarray(offset, Math.min(offset + 65536, bytes.length)));
  }
  pending = { bytes, name: 'sample-1mb.bin' };
  drop.querySelector('.drop-primary').textContent = 'sample-1mb.bin';
  drop.querySelector('.drop-secondary').textContent = '1.00 MB of random bytes';
  buildPlan();
});

$('profile').addEventListener('change', () => { syncInkField(); buildPlan(); });
$('rate').addEventListener('change', buildPlan);

// A payload small enough to finish in a couple of seconds. Worth confirming the
// camera can lock on at all before committing to a transfer measured in minutes.
$('tiny').addEventListener('click', () => {
  const bytes = new Uint8Array(2048);
  crypto.getRandomValues(bytes);
  pending = { bytes, name: 'test-2kb.bin' };
  drop.querySelector('.drop-primary').textContent = 'test-2kb.bin';
  drop.querySelector('.drop-secondary').textContent = '2 kB · lock-on check';
  buildPlan();
});

/**
 * Ink only means anything in a two-level mode.
 *
 * In the colour profiles the palette *is* the data, so offering a colour
 * picker there would promise something the protocol cannot honour.
 */
function syncInkField() {
  const monochrome = ['mono', 'robust'].includes($('profile').value);
  $('ink-field').hidden = !monochrome;
}

$('ink').addEventListener('input', () => {
  const note = $('ink-note');
  const hex = $('ink').value;
  // Same Rec. 601 rule the library enforces, mirrored here so the feedback is
  // immediate rather than arriving as a thrown error on Transmit.
  const [r, g, b] = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16));
  const contrast = 1 - (0.299 * r + 0.587 * g + 0.114 * b) / 255;
  const usable = contrast >= 0.45;
  note.textContent = usable ? `${hex} · contrast ${contrast.toFixed(2)}` : 'too light to decode';
  note.classList.toggle('is-bad', !usable);
  transmit.disabled = !pending || !usable;
});

syncInkField();

// ------------------------------------------------------------------- beam

const beam = $('beam');
const beamCanvas = $('beam-canvas');
const beamCtx = beamCanvas.getContext('2d', { alpha: false });
let beamStop = null;

function startBeam() {
  const encoder = owned.encoder;
  if (!encoder) return;

  stage.pause();
  beam.hidden = false;

  // Size the code to the short edge of the viewport, at whole pixels per cell
  // so nothing lands on a half-pixel and softens the edges the decoder needs.
  const quiet = 4;
  const cells = encoder.modules + quiet * 2;
  const available = Math.min(window.innerWidth, window.innerHeight) * 0.96;
  const modulePx = Math.max(2, Math.floor(available / cells));
  const side = cells * modulePx;
  beamCanvas.width = beamCanvas.height = side;

  const label = $('beam-frame');
  let index = 0;
  let raf = 0;
  let wake = null;

  // Keep the screen awake: the transfer is long enough that a phone or laptop
  // will otherwise dim mid-transmission and cost the receiver its lock.
  navigator.wakeLock?.request('screen').then((sentinel) => { wake = sentinel; }).catch(() => {});

  const monochrome = ['mono', 'robust'].includes($('profile').value);
  const ink = monochrome ? $('ink').value : undefined;
  const emitFps = Number($('rate').value);

  // Hold each droplet for a whole period.
  //
  // Emitting one per animation frame runs at the display's refresh rate, 60 Hz
  // or more, against a camera capturing at about 30. Every exposure then
  // straddles a screen update and returns an image that is part one droplet and
  // part the next; those fail the frame CRC, so essentially nothing decodes.
  // Holding well below the capture rate guarantees whole frames instead.
  const period = 1000 / emitFps;
  let shownAt = 0;

  function tick(now) {
    if (now - shownAt >= period) {
      shownAt = now;
      const frame = encoder.frameRGBA(index, { modulePx, quietZone: quiet, ink });
      beamCtx.putImageData(new ImageData(frame.data, frame.width, frame.height), 0, 0);
      label.textContent = `frame ${index} · ${emitFps}/s`;
      index++;
    }
    raf = requestAnimationFrame(tick);
  }
  raf = requestAnimationFrame(tick);

  beamStop = () => {
    cancelAnimationFrame(raf);
    wake?.release().catch(() => {});
    beam.hidden = true;
    beamStop = null;
    stage.resume();
  };
}

transmit.addEventListener('click', startBeam);
beam.addEventListener('click', () => beamStop?.());
addEventListener('keydown', (event) => { if (event.key === 'Escape') beamStop?.(); });

// ---------------------------------------------------------------- receiving

const cam = $('cam');
const camCanvas = $('cam-canvas');
const camCtx = camCanvas.getContext('2d', { alpha: false, willReadFrequently: true });
const progress = $('progress');
const doneBox = $('done');
let receiving = false;

async function startCamera() {
  if (receiving) { stopCamera(); return; }

  let stream;
  try {
    stream = await navigator.mediaDevices.getUserMedia({
      video: { facingMode: { ideal: 'environment' }, width: { ideal: 1920 }, height: { ideal: 1080 } },
    });
  } catch (error) {
    alert(
      error.name === 'NotAllowedError'
        ? 'Camera permission was declined. The demo needs it to see the frames.'
        : `Could not open the camera: ${error.message}`,
    );
    return;
  }

  owned.stream = stream;
  cam.srcObject = stream;
  await cam.play();
  $('cam-empty').hidden = true;
  $('camera').textContent = 'Stop camera';
  progress.hidden = false;
  doneBox.hidden = true;
  receiving = true;

  releaseResult();
  const decoder = pz.decoder();
  const viewport = document.querySelector('.viewport');
  const meter = $('progress-meter');
  const state = $('progress-state');

  camCanvas.width = cam.videoWidth;
  camCanvas.height = cam.videoHeight;

  // Drive off actual camera frames where the browser exposes them. Animation
  // frames run at display refresh, roughly twice the camera's rate, so half
  // the work decoded an image already decoded — expensive on a phone, and the
  // wasted cycles come straight out of the real capture rate.
  const schedule = cam.requestVideoFrameCallback
    ? (fn) => cam.requestVideoFrameCallback(() => fn())
    : (fn) => requestAnimationFrame(fn);

  function pump() {
    if (!receiving) { decoder.free(); return; }

    camCtx.drawImage(cam, 0, 0, camCanvas.width, camCanvas.height);
    const image = camCtx.getImageData(0, 0, camCanvas.width, camCanvas.height);

    let status;
    try {
      status = decoder.ingestRGBA(image.width, image.height, image.data);
    } catch {
      schedule(pump);
      return;
    }

    const locked = status.kind !== ProgressKind.NotFound;
    viewport.classList.toggle('is-locked', locked);
    state.textContent =
      status.kind === ProgressKind.NotFound ? 'searching'
      : status.kind === ProgressKind.Rejected ? 'frame seen, unusable'
      : status.complete ? 'complete' : 'receiving';

    $('progress-pct').textContent = `${Math.round(status.fraction * 100)}%`;
    meter.style.width = `${status.fraction * 100}%`;
    meter.classList.toggle('is-locked', status.complete);

    const p = progress.querySelector.bind(progress);
    p('[data-k="seen"]').textContent = decoder.framesSeen;
    p('[data-k="accepted"]').textContent = decoder.framesAccepted;
    // recovered/total come straight off the decoder, so they stay correct
    // through a miss and are exact on completion.
    p('[data-k="blocks"]').textContent = `${status.recovered} / ${status.total}`;
    p('[data-k="session"]').textContent =
      decoder.sessionId === null
        ? '—'
        : `0x${decoder.sessionId.toString(16).toUpperCase().padStart(4, '0')}`;

    if (status.complete) {
      finish(decoder.result());
      decoder.free();
      stopCamera();
      return;
    }
    schedule(pump);
  }
  schedule(pump);
}

async function finish(raw) {
  releaseResult();
  let bytes;
  try {
    bytes = await unpack(raw);
  } catch (error) {
    alert(`Recovered the bytes but could not decompress them: ${error.message}`);
    return;
  }
  const blob = new Blob([bytes], { type: 'application/octet-stream' });
  owned.url = URL.createObjectURL(blob);

  const save = $('save');
  save.href = owned.url;
  save.download = `photonic-zero-${Date.now()}.bin`;
  $('done-size').textContent = describe(bytes.length);
  doneBox.hidden = false;
  progress.hidden = true;
}

function stopCamera() {
  receiving = false;
  owned.stream?.getTracks().forEach((track) => track.stop());
  owned.stream = null;
  cam.srcObject = null;
  $('cam-empty').hidden = false;
  $('camera').textContent = 'Start camera';
  document.querySelector('.viewport').classList.remove('is-locked');
  // Leaving a frozen "searching 0%" on screen reads as a stalled transfer
  // rather than a stopped one. The completion panel keeps its own state.
  if (doneBox.hidden) progress.hidden = true;
}

/** Revoke the object URL. Holding one keeps the whole payload alive in memory. */
function releaseResult() {
  if (owned.url) { URL.revokeObjectURL(owned.url); owned.url = null; }
}

$('camera').addEventListener('click', startCamera);
$('again').addEventListener('click', () => {
  releaseResult();
  doneBox.hidden = true;
  startCamera();
});

// ---------------------------------------------------------------- teardown

// Hiding the tab is the strongest signal we get that the result is not being
// used. Release it then rather than waiting for a navigation that may not come.
document.addEventListener('visibilitychange', () => {
  if (document.visibilityState === 'hidden' && doneBox.hidden) releaseResult();
});

addEventListener('pagehide', () => {
  releaseResult();
  stopCamera();
  beamStop?.();
  for (const key of ['encoder', 'decoder', 'stageEncoder']) {
    owned[key]?.free();
    owned[key] = null;
  }
});
