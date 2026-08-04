/**
 * Photonic Zero: data over light, from any screen to any camera.
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

/** Preset encoding profiles. */
export type Profile = 'balanced' | 'robust' | 'fast' | 'resilient';

/** Outcome of offering one image to a decoder. */
export declare const ProgressKind: {
  /** No frame was located. Routine while a camera settles. */
  readonly NotFound: 0;
  /** A frame was seen but was unusable, or belonged to another session. */
  readonly Rejected: 1;
  /** A droplet was absorbed; the message is still incomplete. */
  readonly Progressed: 2;
  /** The message is complete and verified. */
  readonly Complete: 3;
};

export interface Status {
  kind: 0 | 1 | 2 | 3;
  complete: boolean;
  /** Source blocks recovered so far. */
  recovered: number;
  /** Source blocks in the message. */
  total: number;
  /** Fraction recovered, in [0, 1]. */
  fraction: number;
  frameIndex: number;
}

export interface EncodeOptions {
  profile?: Profile;
  /** Grid size code, 0 to 4, overriding the profile. */
  grid?: number;
  /** Colour mode code: 0 mono, 1 four-colour, 2 eight-colour. */
  mode?: number;
  /** Parity code, 0 to 7. Higher spends more of each frame on repair data. */
  parity?: number;
  /** Pin the session id instead of deriving it from the payload. */
  sessionId?: number;
}

export interface RenderOptions {
  /** Pixels per cell. Default 8. */
  modulePx?: number;
  /** Quiet zone in cells. Default 4. */
  quietZone?: number;
}

export interface Frame {
  width: number;
  height: number;
  data: Uint8ClampedArray;
}

export declare class PzError extends Error {}

/** Splits a message into an endless stream of frames. */
export declare class Encoder {
  /** Minimum frames a receiver needs under perfect conditions. */
  readonly blockCount: number;
  readonly sessionId: number;
  /** Cells per side. */
  readonly modules: number;
  /** Payload bytes carried per frame. */
  readonly dropletSize: number;

  /** One frame as RGBA, ready for `ctx.putImageData`. */
  frameRGBA(index: number, options?: RenderOptions): Frame;
  /** One frame as a complete PNG file. */
  framePNG(index: number, options?: RenderOptions): Uint8Array;
  /** One frame as raw colour codes, one byte per cell, row-major. */
  frameCells(index: number): Uint8Array;
  free(): void;
}

/** Accumulates frames until the message is complete. */
export declare class Decoder {
  /** Offer a captured frame, e.g. from `ctx.getImageData`. */
  ingestRGBA(width: number, height: number, data: Uint8Array | Uint8ClampedArray): Status;
  /** Offer a PNG file produced by this library. */
  ingestPNG(bytes: Uint8Array): Status;

  readonly progress: number;
  readonly framesSeen: number;
  readonly framesAccepted: number;
  /** Session being received, or null if none has been locked on to. */
  readonly sessionId: number | null;

  /** The completed message, or null if decoding has not finished. */
  result(): Uint8Array | null;
  /** Forget the current session so a new transmission can be received. */
  reset(): void;
  free(): void;
}

export declare class Pz {
  /** Wire format version the module implements. */
  readonly protocolVersion: number;
  encode(payload: Uint8Array | ArrayBuffer | string, options?: EncodeOptions): Encoder;
  decoder(): Decoder;
}

/**
 * Instantiate the WebAssembly module.
 *
 * Omit `source` in Node and the bundled module is read from disk. In a
 * browser, pass `fetch(...)` or an ArrayBuffer.
 */
export declare function load(
  source?: ArrayBuffer | Uint8Array | Response | Promise<Response>,
): Promise<Pz>;

declare const _default: {
  load: typeof load;
  ProgressKind: typeof ProgressKind;
  PzError: typeof PzError;
};
export default _default;
