/**
 * Photonic Zero for JavaScript.
 *
 * A thin, hand-written layer over the WebAssembly module. There is no
 * wasm-bindgen here: the module exports a flat C ABI and this file does the
 * pointer arithmetic, which keeps the download small and the glue readable.
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

/** Profile presets, as (grid code, colour mode code, parity code). */
const PROFILES = {
  /** 49x49, 8 colours, 28% parity. The default. */
  balanced: [1, 2, 3],
  /** 33x33, black and white, 40% parity. For bad light or a bad camera. */
  robust: [0, 0, 5],
  /** 97x97, 8 colours, 16% parity. Needs a close, steady capture. */
  fast: [4, 2, 1],
  /** 65x65, 4 colours with per-cell parity, 28% parity. */
  resilient: [2, 1, 3],
  /** 97x97, two levels, 28% parity. Single-colour, and takes an `ink`. */
  mono: [4, 0, 3],
};

/** Outcome codes returned by the decoder. */
export const ProgressKind = Object.freeze({
  NotFound: 0,
  Rejected: 1,
  Progressed: 2,
  Complete: 3,
});

/** Raised when the library cannot do what was asked. */
export class PzError extends Error {
  constructor(message) {
    super(message);
    this.name = 'PzError';
  }
}

/** Require a whole number in `[min, max]`, or explain why it is not one. */
function whole(value, name, min, max) {
  if (typeof value !== 'number' || !Number.isInteger(value) || value < min || value > max) {
    throw new PzError(`${name} must be an integer in [${min}, ${max}], got ${value}`);
  }
  return value;
}

/**
 * Settle render options to the values the ABI will actually use.
 *
 * WebAssembly truncates a fractional argument and the Rust side clamps
 * `module_px` up to 1, so computing pixel dimensions from the raw JavaScript
 * value can disagree with the buffer that comes back. Rejecting the bad input
 * outright beats papering over the mismatch.
 */
function renderOptions({ modulePx = 8, quietZone = 4, ink } = {}) {
  return {
    modulePx: whole(modulePx, 'modulePx', 1, 256),
    quietZone: whole(quietZone, 'quietZone', 0, 256),
    ink: parseInk(ink),
  };
}

/**
 * Normalise an ink colour to the packed integer the ABI takes, or -1 for none.
 *
 * Accepts `"#1a2b3c"`, `"#abc"`, `0x1a2b3c` or `[r, g, b]`. The dark-enough
 * check is enforced here rather than in the module because a caller who picks
 * an unusable brand colour wants to hear about it while writing the code, not
 * as a transfer that quietly never locks on.
 */
function parseInk(ink) {
  if (ink === undefined || ink === null) return -1;

  let rgb;
  if (Array.isArray(ink)) {
    if (ink.length !== 3) throw new PzError('ink array must be [r, g, b]');
    rgb = ink.map((c) => whole(c, 'ink channel', 0, 255));
  } else if (typeof ink === 'number') {
    rgb = [(ink >> 16) & 255, (ink >> 8) & 255, ink & 255];
    whole(ink, 'ink', 0, 0xffffff);
  } else if (typeof ink === 'string') {
    let hex = ink.trim().replace(/^#/, '');
    if (hex.length === 3) hex = hex.replace(/./g, (c) => c + c);
    if (!/^[0-9a-fA-F]{6}$/.test(hex)) throw new PzError(`ink is not a colour: ${ink}`);
    rgb = [0, 2, 4].map((i) => parseInt(hex.slice(i, i + 2), 16));
  } else {
    throw new PzError(`ink must be a hex string, a number or [r, g, b], got ${typeof ink}`);
  }

  // Rec. 601 luminance against a white page, matching pz-core's own check.
  const luma = (0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2]) / 255;
  if (1 - luma < 0.45) {
    throw new PzError(
      `ink #${rgb.map((c) => c.toString(16).padStart(2, '0')).join('')} is too light to ` +
        'demodulate against white; use a deeper colour',
    );
  }
  return (rgb[0] << 16) | (rgb[1] << 8) | rgb[2];
}

/** Largest RGBA render this will attempt, in bytes. */
const MAX_RENDER_BYTES = 256 * 1024 * 1024;

/**
 * Refuse a render the module could not allocate.
 *
 * Bounding each option on its own is not enough: they multiply. Left
 * unchecked the module traps on a failed allocation, which kills the whole
 * instance, and a trap is not something a caller can catch and recover from.
 */
function checkRenderSize(modules, modulePx, quietZone) {
  const side = (modules + quietZone * 2) * modulePx;
  const bytes = side * side * 4;
  if (bytes > MAX_RENDER_BYTES) {
    throw new PzError(
      `render of ${side}x${side} needs ${Math.round(bytes / 1048576)} MiB, ` +
        `over the ${MAX_RENDER_BYTES / 1048576} MiB limit; reduce modulePx or quietZone`,
    );
  }
  return side;
}

/**
 * Instantiate the WebAssembly module.
 *
 * @param {ArrayBuffer|Uint8Array|Response|Promise<Response>} [source]
 *   The module bytes. Omit it in Node and the bundled `pz.wasm` is read from
 *   disk; in a browser, pass the result of `fetch()` or an ArrayBuffer.
 * @returns {Promise<Pz>}
 */
export async function load(source) {
  let instance;

  if (source === undefined) {
    // Node: read the module that ships alongside this file.
    const { readFile } = await import('node:fs/promises');
    const { fileURLToPath } = await import('node:url');
    const path = fileURLToPath(new URL('./pz.wasm', import.meta.url));
    const bytes = await readFile(path);
    ({ instance } = await WebAssembly.instantiate(bytes, {}));
  } else if (source instanceof ArrayBuffer || ArrayBuffer.isView(source)) {
    ({ instance } = await WebAssembly.instantiate(source, {}));
  } else {
    // A Response, or a promise of one, from fetch().
    //
    // Streaming compilation insists on `application/wasm`, and plenty of
    // static hosts serve .wasm as octet-stream. Falling back to the buffered
    // path costs one copy and turns a hard failure into a working load.
    try {
      ({ instance } = await WebAssembly.instantiateStreaming(source, {}));
    } catch (error) {
      const response = await source;
      if (typeof response?.arrayBuffer !== 'function') throw error;
      ({ instance } = await WebAssembly.instantiate(await response.arrayBuffer(), {}));
    }
  }

  return new Pz(instance);
}

/** The instantiated library. */
export class Pz {
  constructor(instance) {
    this.exports = instance.exports;
    if (typeof this.exports.pz_alloc !== 'function') {
      throw new PzError('not a Photonic Zero wasm module');
    }
  }

  /** Wire format version the module implements. */
  get protocolVersion() {
    return this.exports.pz_protocol_version();
  }

  /**
   * The module's linear memory as bytes.
   *
   * Re-read on every access: growing the heap detaches the old ArrayBuffer,
   * so a cached view silently becomes useless.
   */
  get memory() {
    return new Uint8Array(this.exports.memory.buffer);
  }

  /** Copy `bytes` into the module's memory. Returns [pointer, length]. */
  _write(bytes) {
    const view = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
    if (view.length === 0) throw new PzError('payload is empty');
    const ptr = this.exports.pz_alloc(view.length);
    if (ptr === 0) throw new PzError('wasm allocation failed');
    this.memory.set(view, ptr);
    return [ptr, view.length];
  }

  /** Copy `len` bytes out of the module's memory, then free them. */
  _take(ptr, len) {
    if (ptr === 0) return null;
    const copy = this.memory.slice(ptr, ptr + len);
    this.exports.pz_free(ptr, len);
    return copy;
  }

  /** Read a usize the module wrote through an out-pointer. */
  _readLen(ptr) {
    return new DataView(this.exports.memory.buffer).getUint32(ptr, true);
  }

  /** Allocate scratch space for one out-parameter. */
  _scratch() {
    const ptr = this.exports.pz_alloc(4);
    if (ptr === 0) throw new PzError('wasm allocation failed');
    return ptr;
  }

  /**
   * Prepare a transmission.
   *
   * @param {Uint8Array|ArrayBuffer|string} payload
   * @param {{profile?: string, grid?: number, mode?: number, parity?: number,
   *          sessionId?: number}} [options]
   * @returns {Encoder}
   */
  encode(payload, options = {}) {
    const bytes =
      typeof payload === 'string' ? new TextEncoder().encode(payload) : payload;

    const preset = PROFILES[options.profile ?? 'balanced'];
    if (!preset) {
      throw new PzError(
        `unknown profile ${options.profile}; expected one of ${Object.keys(PROFILES).join(', ')}`,
      );
    }
    // The overrides cross the ABI as raw bytes, where a float is truncated and
    // an out-of-range value wraps. `parity: 259` silently becoming 3 would
    // build a frame the caller never asked for.
    const grid = options.grid === undefined ? preset[0] : whole(options.grid, 'grid', 0, 4);
    const mode = options.mode === undefined ? preset[1] : whole(options.mode, 'mode', 0, 2);
    const parity =
      options.parity === undefined ? preset[2] : whole(options.parity, 'parity', 0, 7);
    // A session id is 16 bits on the wire. Catching that here gives a usable
    // message; letting it through returns a null handle and a vague one.
    const session =
      options.sessionId === undefined || options.sessionId === null
        ? -1
        : whole(options.sessionId, 'sessionId', 0, 0xffff);

    const [ptr, len] = this._write(bytes);
    const handle = this.exports.pz_encoder_new(ptr, len, grid, mode, parity, session);
    this.exports.pz_free(ptr, len);

    if (handle === 0) {
      throw new PzError('could not create an encoder for that payload and configuration');
    }
    return new Encoder(this, handle);
  }

  /** Create a decoder. */
  decoder() {
    const handle = this.exports.pz_decoder_new();
    if (handle === 0) throw new PzError('could not create a decoder');
    return new Decoder(this, handle);
  }
}

/** Splits a message into an endless stream of frames. */
export class Encoder {
  constructor(pz, handle) {
    this._pz = pz;
    this._handle = handle;
  }

  _check() {
    if (this._handle === 0) throw new PzError('encoder has been freed');
  }

  /** Minimum frames a receiver needs under perfect conditions. */
  get blockCount() {
    this._check();
    return this._pz.exports.pz_encoder_block_count(this._handle);
  }

  /** Session id stamped into every frame. */
  get sessionId() {
    this._check();
    return this._pz.exports.pz_encoder_session_id(this._handle);
  }

  /** Cells per side. */
  get modules() {
    this._check();
    return this._pz.exports.pz_encoder_modules(this._handle);
  }

  /** Payload bytes carried per frame. */
  get dropletSize() {
    this._check();
    return this._pz.exports.pz_encoder_droplet_size(this._handle);
  }

  /**
   * One frame as RGBA pixels, ready for `ctx.putImageData`.
   *
   * @returns {{width: number, height: number, data: Uint8ClampedArray}}
   */
  frameRGBA(index, options = {}) {
    this._check();
    whole(index, 'index', 0, 0xffffffff);
    const { modulePx, quietZone, ink } = renderOptions(options);
    const side = checkRenderSize(this.modules, modulePx, quietZone);
    const out = this._pz._scratch();
    const ptr = this._pz.exports.pz_encoder_frame_rgba(
      this._handle, index, modulePx, quietZone, ink, out,
    );
    const len = this._pz._readLen(out);
    this._pz.exports.pz_free(out, 4);
    const data = this._pz._take(ptr, len);
    if (data === null) throw new PzError(`could not render frame ${index}`);

    return {
      width: side,
      height: side,
      data: new Uint8ClampedArray(data.buffer, data.byteOffset, data.length),
    };
  }

  /** One frame as a complete PNG file. */
  framePNG(index, options = {}) {
    this._check();
    whole(index, 'index', 0, 0xffffffff);
    const { modulePx, quietZone, ink } = renderOptions(options);
    checkRenderSize(this.modules, modulePx, quietZone);
    const out = this._pz._scratch();
    const ptr = this._pz.exports.pz_encoder_frame_png(
      this._handle, index, modulePx, quietZone, ink, out,
    );
    const len = this._pz._readLen(out);
    this._pz.exports.pz_free(out, 4);
    const data = this._pz._take(ptr, len);
    if (data === null) throw new PzError(`could not render frame ${index}`);
    return data;
  }

  /** One frame as raw colour codes, one byte per cell, row-major. */
  frameCells(index) {
    this._check();
    whole(index, 'index', 0, 0xffffffff);
    const out = this._pz._scratch();
    const ptr = this._pz.exports.pz_encoder_frame_cells(this._handle, index, out);
    const len = this._pz._readLen(out);
    this._pz.exports.pz_free(out, 4);
    const data = this._pz._take(ptr, len);
    if (data === null) throw new PzError(`could not build frame ${index}`);
    return data;
  }

  /** Release the handle. Safe to call more than once. */
  free() {
    if (this._handle !== 0) {
      this._pz.exports.pz_encoder_free(this._handle);
      this._handle = 0;
    }
  }
}

/** Accumulates frames until the message is complete. */
export class Decoder {
  constructor(pz, handle) {
    this._pz = pz;
    this._handle = handle;
  }

  _check() {
    if (this._handle === 0) throw new PzError('decoder has been freed');
  }

  _status(kind) {
    if (kind === -1) throw new PzError('invalid image passed to the decoder');
    if (kind === -2) throw new PzError('could not read that PNG');
    return {
      kind,
      complete: kind === ProgressKind.Complete,
      recovered: this._pz.exports.pz_decoder_recovered(this._handle),
      total: this._pz.exports.pz_decoder_total(this._handle),
      fraction: this._pz.exports.pz_decoder_progress(this._handle),
      frameIndex: this._pz.exports.pz_decoder_frame_index(this._handle),
    };
  }

  /**
   * Offer a captured frame as RGBA, e.g. from `ctx.getImageData`.
   *
   * `NotFound` and `Rejected` are routine: most frames of a hand-held camera
   * are unusable, which is exactly why the code is rateless.
   */
  ingestRGBA(width, height, data) {
    this._check();
    const [ptr, len] = this._pz._write(data);
    const kind = this._pz.exports.pz_decoder_ingest_rgba(this._handle, width, height, ptr, len);
    this._pz.exports.pz_free(ptr, len);
    return this._status(kind);
  }

  /** Offer a PNG file produced by this library. */
  ingestPNG(bytes) {
    this._check();
    const [ptr, len] = this._pz._write(bytes);
    const kind = this._pz.exports.pz_decoder_ingest_png(this._handle, ptr, len);
    this._pz.exports.pz_free(ptr, len);
    return this._status(kind);
  }

  /** Fraction of the message recovered, in [0, 1]. */
  get progress() {
    this._check();
    return this._pz.exports.pz_decoder_progress(this._handle);
  }

  /** Images offered so far. */
  get framesSeen() {
    this._check();
    return this._pz.exports.pz_decoder_frames_seen(this._handle);
  }

  /** Frames that decoded and were absorbed. */
  get framesAccepted() {
    this._check();
    return this._pz.exports.pz_decoder_frames_accepted(this._handle);
  }

  /** Session being received, or null if none has been locked on to. */
  get sessionId() {
    this._check();
    const id = this._pz.exports.pz_decoder_session_id(this._handle);
    return id < 0 ? null : id;
  }

  /** The completed message, or null if decoding has not finished. */
  result() {
    this._check();
    const out = this._pz._scratch();
    const ptr = this._pz.exports.pz_decoder_result(this._handle, out);
    const len = this._pz._readLen(out);
    this._pz.exports.pz_free(out, 4);
    return this._pz._take(ptr, len);
  }

  /** Forget the current session so a new transmission can be received. */
  reset() {
    this._check();
    this._pz.exports.pz_decoder_reset(this._handle);
  }

  /** Release the handle. Safe to call more than once. */
  free() {
    if (this._handle !== 0) {
      this._pz.exports.pz_decoder_free(this._handle);
      this._handle = 0;
    }
  }
}

export default { load, ProgressKind, PzError };
