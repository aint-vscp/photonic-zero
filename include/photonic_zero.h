/*
 * photonic_zero.h - C API for Photonic Zero (PZ)
 *
 * PZ is a rateless screen-to-camera optical data protocol: bytes become a
 * stream of colour frames on a display, a camera watches, and the bytes come
 * back out. No radio, no pairing, no network - just line of sight.
 *
 * Unlike a QR code, a PZ transmission has no fixed length and needs no back
 * channel. The transmitter emits frames forever; the receiver watches until it
 * has enough, and does not care which ones it caught.
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 * https://github.com/thepieza/photonic-zero
 *
 * ---------------------------------------------------------------------------
 * Memory and lifetime rules
 * ---------------------------------------------------------------------------
 *   - Every pz_*_new() has exactly one matching pz_*_free().
 *   - Passing NULL to a free function is a no-op. Freeing twice is undefined.
 *   - Buffers returned as pz_buffer must be released with pz_buffer_free().
 *     They come from Rust's allocator: do NOT call free() on buffer.data.
 *   - Fallible calls take a pz_status* out-parameter, which may be NULL if you
 *     do not care why something failed.
 *   - A panic inside the library is caught at the boundary and reported as
 *     PZ_STATUS_INTERNAL. It never unwinds into your C code.
 *   - Handles are not thread-safe individually. Distinct handles used from
 *     distinct threads are fine.
 *
 * ---------------------------------------------------------------------------
 * Minimal example
 * ---------------------------------------------------------------------------
 *   pz_status st;
 *   pz_config cfg = pz_config_default();
 *   pz_encoder *enc = pz_encoder_new((const uint8_t *)"hello", 5, &cfg, &st);
 *
 *   pz_decoder *dec = pz_decoder_new();
 *   pz_progress p;
 *   for (uint32_t i = 0; i < 64; i++) {
 *       pz_frame *f = pz_encoder_frame(enc, i, &st);
 *       pz_decoder_ingest_frame(dec, f, &p, &st);
 *       pz_frame_free(f);
 *       if (p.kind == PZ_PROGRESS_COMPLETE) break;
 *   }
 *
 *   pz_buffer out = pz_decoder_result(dec, &st);
 *   fwrite(out.data, 1, out.len, stdout);
 *   pz_buffer_free(out);
 *   pz_decoder_free(dec);
 *   pz_encoder_free(enc);
 */

#ifndef PHOTONIC_ZERO_H
#define PHOTONIC_ZERO_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* -------------------------------------------------------------- status --- */

typedef enum pz_status {
    PZ_STATUS_OK = 0,
    /** A required pointer was NULL, or a length was inconsistent. */
    PZ_STATUS_INVALID_ARGUMENT = 1,
    PZ_STATUS_EMPTY_PAYLOAD = 2,
    PZ_STATUS_PAYLOAD_TOO_LARGE = 3,
    /** The chosen grid and colour mode leave no usable payload capacity. */
    PZ_STATUS_CAPACITY_TOO_SMALL = 4,
    PZ_STATUS_HEADER_CORRUPT = 5,
    PZ_STATUS_UNSUPPORTED_FORMAT = 6,
    PZ_STATUS_FRAME_CORRUPT = 7,
    PZ_STATUS_NO_FRAME_DETECTED = 8,
    PZ_STATUS_SESSION_MISMATCH = 9,
    PZ_STATUS_CHECKSUM_MISMATCH = 10,
    /** Decoding has not finished, so there is no result to take yet. */
    PZ_STATUS_NOT_COMPLETE = 11,
    PZ_STATUS_INTERNAL = 12
} pz_status;

/** Static, NUL-terminated description of a status. Never NULL; do not free. */
const char *pz_status_message(pz_status status);

/** Library version, e.g. "0.1.0". Never NULL; do not free. */
const char *pz_version(void);

/* -------------------------------------------------------------- buffer --- */

/** An owned byte buffer. Release with pz_buffer_free(). */
typedef struct pz_buffer {
    uint8_t *data; /**< NULL when empty. */
    size_t len;    /**< Valid bytes. */
    size_t cap;    /**< Internal; do not modify. */
} pz_buffer;

void pz_buffer_free(pz_buffer buffer);

/* -------------------------------------------------------------- config --- */

/** Grid size codes: cells per side. */
enum {
    PZ_GRID_33 = 0, /**< Largest cells; best at range or with a poor camera. */
    PZ_GRID_49 = 1, /**< The default. */
    PZ_GRID_65 = 2,
    PZ_GRID_81 = 3,
    PZ_GRID_97 = 4 /**< Highest capacity; needs a close, steady capture. */
};

/** Colour mode codes: bits carried per cell. */
enum {
    PZ_MODE_MONO = 0, /**< 1 bit. Black and white. Most robust. */
    PZ_MODE_RGB4 = 1, /**< 2 bits, with a per-cell parity check. */
    PZ_MODE_RGB8 = 2  /**< 3 bits. Highest throughput. */
};

typedef struct pz_config {
    uint8_t grid_code;   /**< One of the PZ_GRID_* values. */
    uint8_t mode_code;   /**< One of the PZ_MODE_* values. */
    uint8_t parity_code; /**< 0..7. Higher spends more of each frame on FEC. */
    /** Session id, or negative to derive a stable one from the payload. */
    int32_t session_id;
} pz_config;

/** 49x49, 8 colours, 28% parity. The balanced choice. */
pz_config pz_config_default(void);
/** 33x33, black and white, 40% parity. For bad light or a bad camera. */
pz_config pz_config_robust(void);
/** 97x97, 8 colours, 16% parity. For a close, steady, well-lit capture. */
pz_config pz_config_fast(void);
/** 65x65, 4 colours, 28% parity. Per-cell error detection. */
pz_config pz_config_resilient(void);

/* ------------------------------------------------------------- encoder --- */

typedef struct pz_encoder pz_encoder;
typedef struct pz_frame pz_frame;

/**
 * Prepare a transmission. Returns NULL on failure.
 * The payload is copied; you may free it immediately afterwards.
 */
pz_encoder *pz_encoder_new(const uint8_t *payload, size_t len,
                           const pz_config *config, pz_status *status);
void pz_encoder_free(pz_encoder *encoder);

/** Minimum frames a receiver needs under perfect conditions. */
size_t pz_encoder_block_count(const pz_encoder *encoder);
uint16_t pz_encoder_session_id(const pz_encoder *encoder);
size_t pz_encoder_payload_len(const pz_encoder *encoder);
/** Payload bytes carried per frame. */
size_t pz_encoder_droplet_size(const pz_encoder *encoder);
/** Cells per side for this encoder's grid. */
size_t pz_encoder_modules(const pz_encoder *encoder);

/**
 * Estimated transfer time in seconds, given a capture rate and the fraction
 * of displayed frames the receiver actually catches.
 */
double pz_encoder_estimated_seconds(const pz_encoder *encoder, double fps,
                                    double capture_ratio);

/**
 * Build one frame. Defined for every index: the stream never runs out, so
 * loop until the receiver says it is done.
 */
pz_frame *pz_encoder_frame(const pz_encoder *encoder, uint32_t index,
                           pz_status *status);
void pz_frame_free(pz_frame *frame);

size_t pz_frame_modules(const pz_frame *frame);
uint32_t pz_frame_index(const pz_frame *frame);
/** modules*modules colour codes, row-major. Caller frees. */
pz_buffer pz_frame_cells(const pz_frame *frame);
/** modules*modules RGB triples, row-major. Caller frees. */
pz_buffer pz_frame_rgb(const pz_frame *frame);

/**
 * Render a frame to a square RGB image. The side length in pixels is written
 * to *out_size, which may be NULL. Caller frees the buffer.
 */
pz_buffer pz_encoder_render_rgb(const pz_encoder *encoder, uint32_t index,
                                size_t module_px, size_t quiet_zone,
                                size_t *out_size, pz_status *status);

/** Render a frame as a complete PNG file. Caller frees the buffer. */
pz_buffer pz_encoder_render_png(const pz_encoder *encoder, uint32_t index,
                                size_t module_px, size_t quiet_zone,
                                pz_status *status);

/* ------------------------------------------------------------- decoder --- */

typedef struct pz_decoder pz_decoder;

typedef enum pz_progress_kind {
    PZ_PROGRESS_NOT_FOUND = 0,  /**< No frame located. Entirely routine. */
    PZ_PROGRESS_REJECTED = 1,   /**< Frame seen but unusable, or foreign. */
    PZ_PROGRESS_PROGRESSED = 2, /**< Droplet absorbed; still incomplete. */
    PZ_PROGRESS_COMPLETE = 3    /**< Message complete and verified. */
} pz_progress_kind;

typedef struct pz_progress {
    pz_progress_kind kind;
    uint16_t session_id;
    uint32_t frame_index;
    size_t recovered; /**< Source blocks recovered so far. */
    size_t total;     /**< Source blocks in the message. */
    double fraction;  /**< recovered/total, in [0, 1]. */
} pz_progress;

pz_decoder *pz_decoder_new(void);
void pz_decoder_free(pz_decoder *decoder);
/** Forget the current session so a new transmission can be received. */
void pz_decoder_reset(pz_decoder *decoder);

/**
 * Offer a captured image. Tightly packed, 8 bits per channel.
 * `len` must be at least width*height*3 (or *4 for the RGBA variant).
 *
 * Most captured frames of a hand-held camera are unusable; PZ_PROGRESS_NOT_FOUND
 * and PZ_PROGRESS_REJECTED are normal and not errors.
 */
void pz_decoder_ingest_rgb(pz_decoder *decoder, size_t width, size_t height,
                           const uint8_t *data, size_t len,
                           pz_progress *out_progress, pz_status *status);

void pz_decoder_ingest_rgba(pz_decoder *decoder, size_t width, size_t height,
                            const uint8_t *data, size_t len,
                            pz_progress *out_progress, pz_status *status);

/** Offer a frame directly, bypassing the camera path. Useful for testing. */
void pz_decoder_ingest_frame(pz_decoder *decoder, const pz_frame *frame,
                             pz_progress *out_progress, pz_status *status);

double pz_decoder_progress(const pz_decoder *decoder);
size_t pz_decoder_frames_seen(const pz_decoder *decoder);
size_t pz_decoder_frames_accepted(const pz_decoder *decoder);

/** Writes the session id and returns true, or returns false if not yet locked. */
bool pz_decoder_session_id(const pz_decoder *decoder, uint16_t *out);

/**
 * Copy out the completed message. Sets PZ_STATUS_NOT_COMPLETE and returns an
 * empty buffer if decoding has not finished. The decoder keeps its own copy,
 * so this may be called more than once. Caller frees.
 */
pz_buffer pz_decoder_result(const pz_decoder *decoder, pz_status *status);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* PHOTONIC_ZERO_H */
