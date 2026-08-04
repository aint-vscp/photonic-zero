/*
 * Photonic Zero from C: encode a message, stream frames into a decoder, and
 * get the bytes back.
 *
 * Build (after `cargo build --release --manifest-path crates/pz-ffi/Cargo.toml`):
 *
 *   Linux/macOS:
 *     cc -I include examples/c/roundtrip.c \
 *        crates/pz-ffi/target/release/libpz_ffi.a -lm -lpthread -ldl -o roundtrip
 *
 *   Windows (mingw):
 *     gcc -I include examples/c/roundtrip.c \
 *        crates/pz-ffi/target/release/libpz_ffi.a \
 *        -lws2_32 -luserenv -lbcrypt -lntdll -o roundtrip.exe
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

#include "photonic_zero.h"

#include <stdio.h>
#include <string.h>

int main(void) {
    const char *message =
        "Photonic Zero carries this sentence over light, through a C ABI, "
        "with no network in sight.";
    const size_t message_len = strlen(message);

    printf("photonic-zero %s\n", pz_version());

    pz_status status = PZ_STATUS_INTERNAL;
    pz_config config = pz_config_default();

    pz_encoder *encoder =
        pz_encoder_new((const uint8_t *)message, message_len, &config, &status);
    if (encoder == NULL) {
        fprintf(stderr, "encoder: %s\n", pz_status_message(status));
        return 1;
    }

    printf("  grid          %zu x %zu cells\n", pz_encoder_modules(encoder),
           pz_encoder_modules(encoder));
    printf("  payload       %zu bytes\n", pz_encoder_payload_len(encoder));
    printf("  per frame     %zu bytes\n", pz_encoder_droplet_size(encoder));
    printf("  frames needed %zu\n", pz_encoder_block_count(encoder));
    printf("  session       0x%04X\n", pz_encoder_session_id(encoder));

    pz_decoder *decoder = pz_decoder_new();
    if (decoder == NULL) {
        fprintf(stderr, "decoder: allocation failed\n");
        pz_encoder_free(encoder);
        return 1;
    }

    /* The stream is endless, so loop until the decoder says it has enough.
     * A real receiver would be pulling images off a camera here instead. */
    pz_progress progress;
    memset(&progress, 0, sizeof(progress));

    uint32_t index = 0;
    for (; index < 10000; index++) {
        pz_frame *frame = pz_encoder_frame(encoder, index, &status);
        if (frame == NULL) {
            fprintf(stderr, "frame %u: %s\n", index, pz_status_message(status));
            break;
        }
        pz_decoder_ingest_frame(decoder, frame, &progress, &status);
        pz_frame_free(frame);

        if (status != PZ_STATUS_OK) {
            fprintf(stderr, "ingest %u: %s\n", index, pz_status_message(status));
            break;
        }
        if (progress.kind == PZ_PROGRESS_COMPLETE) {
            break;
        }
    }

    int exit_code = 1;
    if (progress.kind == PZ_PROGRESS_COMPLETE) {
        pz_buffer result = pz_decoder_result(decoder, &status);
        if (status == PZ_STATUS_OK) {
            printf("  frames used   %zu\n", pz_decoder_frames_accepted(decoder));
            printf("  recovered     %zu bytes\n", result.len);

            if (result.len == message_len &&
                memcmp(result.data, message, message_len) == 0) {
                printf("\n  \"%.*s\"\n", (int)result.len, (const char *)result.data);
                printf("\nround trip OK\n");
                exit_code = 0;
            } else {
                fprintf(stderr, "\nMISMATCH: recovered bytes differ\n");
            }
        } else {
            fprintf(stderr, "result: %s\n", pz_status_message(status));
        }
        pz_buffer_free(result);
    } else {
        fprintf(stderr, "never completed after %u frames\n", index);
    }

    pz_decoder_free(decoder);
    pz_encoder_free(encoder);
    return exit_code;
}
