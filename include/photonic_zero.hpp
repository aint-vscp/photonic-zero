// photonic_zero.hpp - C++17 wrapper for Photonic Zero (PZ)
//
// A header-only RAII layer over the C API. Handles free themselves, buffers
// become std::vector, and failures become exceptions instead of out-parameters.
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// https://github.com/thepieza/photonic-zero
//
//   #include "photonic_zero.hpp"
//
//   pz::Encoder encoder{data, pz::Config::balanced()};
//   pz::Decoder decoder;
//   for (uint32_t i = 0; ; ++i) {
//       auto progress = decoder.ingest(encoder.frame(i));
//       if (progress.kind == pz::ProgressKind::Complete) break;
//   }
//   std::vector<uint8_t> message = decoder.result();

#ifndef PHOTONIC_ZERO_HPP
#define PHOTONIC_ZERO_HPP

#include "photonic_zero.h"

#include <cstdint>
#include <memory>
#include <stdexcept>
#include <string>
#include <vector>

namespace pz {

/// Thrown when the library reports anything other than PZ_STATUS_OK.
class Error : public std::runtime_error {
public:
    explicit Error(pz_status status)
        : std::runtime_error(std::string("photonic-zero: ") +
                             pz_status_message(status)),
          status_(status) {}

    /// The underlying status code.
    pz_status status() const noexcept { return status_; }

private:
    pz_status status_;
};

namespace detail {

inline void check(pz_status status) {
    if (status != PZ_STATUS_OK) {
        throw Error(status);
    }
}

/// Takes ownership of a pz_buffer and copies it into a vector.
inline std::vector<uint8_t> consume(pz_buffer buffer) {
    std::vector<uint8_t> out;
    if (buffer.data != nullptr && buffer.len > 0) {
        out.assign(buffer.data, buffer.data + buffer.len);
    }
    pz_buffer_free(buffer);
    return out;
}

struct EncoderDeleter {
    void operator()(pz_encoder *p) const noexcept { pz_encoder_free(p); }
};
struct DecoderDeleter {
    void operator()(pz_decoder *p) const noexcept { pz_decoder_free(p); }
};
struct FrameDeleter {
    void operator()(pz_frame *p) const noexcept { pz_frame_free(p); }
};

} // namespace detail

/// Encoder configuration, with the same presets the Rust API offers.
struct Config {
    pz_config raw;

    /// 49x49, 8 colours, 28% parity.
    static Config balanced() { return Config{pz_config_default()}; }
    /// 33x33, black and white, 40% parity.
    static Config robust() { return Config{pz_config_robust()}; }
    /// 97x97, 8 colours, 16% parity.
    static Config fast() { return Config{pz_config_fast()}; }
    /// 65x65, 4 colours with per-cell parity, 28% parity.
    static Config resilient() { return Config{pz_config_resilient()}; }

    /// Pin the session id instead of deriving it from the payload.
    Config &session(uint16_t id) {
        raw.session_id = static_cast<int32_t>(id);
        return *this;
    }
};

enum class ProgressKind {
    NotFound = PZ_PROGRESS_NOT_FOUND,
    Rejected = PZ_PROGRESS_REJECTED,
    Progressed = PZ_PROGRESS_PROGRESSED,
    Complete = PZ_PROGRESS_COMPLETE,
};

struct Progress {
    ProgressKind kind = ProgressKind::NotFound;
    uint16_t session_id = 0;
    uint32_t frame_index = 0;
    std::size_t recovered = 0;
    std::size_t total = 0;
    double fraction = 0.0;

    bool complete() const noexcept { return kind == ProgressKind::Complete; }

    static Progress from(const pz_progress &p) {
        Progress out;
        out.kind = static_cast<ProgressKind>(p.kind);
        out.session_id = p.session_id;
        out.frame_index = p.frame_index;
        out.recovered = p.recovered;
        out.total = p.total;
        out.fraction = p.fraction;
        return out;
    }
};

/// One frame of a transmission.
class Frame {
public:
    explicit Frame(pz_frame *raw) : handle_(raw) {}

    /// Cells per side.
    std::size_t modules() const { return pz_frame_modules(handle_.get()); }
    /// The frame index this frame carries.
    uint32_t index() const { return pz_frame_index(handle_.get()); }
    /// Colour codes, row-major, one byte per cell.
    std::vector<uint8_t> cells() const {
        return detail::consume(pz_frame_cells(handle_.get()));
    }
    /// RGB triples, row-major, three bytes per cell.
    std::vector<uint8_t> rgb() const {
        return detail::consume(pz_frame_rgb(handle_.get()));
    }

    const pz_frame *raw() const noexcept { return handle_.get(); }

private:
    std::unique_ptr<pz_frame, detail::FrameDeleter> handle_;
};

/// A square RGB image.
struct Image {
    std::size_t size = 0; ///< Side length in pixels.
    std::vector<uint8_t> rgb;
};

/// Splits a message into an endless stream of frames.
class Encoder {
public:
    Encoder(const std::vector<uint8_t> &payload, Config config = Config::balanced()) {
        pz_status status = PZ_STATUS_INTERNAL;
        pz_encoder *raw =
            pz_encoder_new(payload.data(), payload.size(), &config.raw, &status);
        detail::check(status);
        if (raw == nullptr) {
            throw Error(PZ_STATUS_INTERNAL);
        }
        handle_.reset(raw);
    }

    Encoder(const std::string &payload, Config config = Config::balanced())
        : Encoder(std::vector<uint8_t>(payload.begin(), payload.end()), config) {}

    /// Minimum frames a receiver needs under perfect conditions.
    std::size_t block_count() const { return pz_encoder_block_count(handle_.get()); }
    uint16_t session_id() const { return pz_encoder_session_id(handle_.get()); }
    std::size_t payload_len() const { return pz_encoder_payload_len(handle_.get()); }
    /// Payload bytes carried per frame.
    std::size_t droplet_size() const { return pz_encoder_droplet_size(handle_.get()); }
    /// Cells per side.
    std::size_t modules() const { return pz_encoder_modules(handle_.get()); }

    double estimated_seconds(double fps, double capture_ratio) const {
        return pz_encoder_estimated_seconds(handle_.get(), fps, capture_ratio);
    }

    /// Build one frame. Defined for every index.
    Frame frame(uint32_t index) const {
        pz_status status = PZ_STATUS_INTERNAL;
        pz_frame *raw = pz_encoder_frame(handle_.get(), index, &status);
        detail::check(status);
        if (raw == nullptr) {
            throw Error(PZ_STATUS_INTERNAL);
        }
        return Frame(raw);
    }

    /// Render a frame to a square RGB image.
    Image render(uint32_t index, std::size_t module_px = 8,
                 std::size_t quiet_zone = 4) const {
        pz_status status = PZ_STATUS_INTERNAL;
        std::size_t size = 0;
        pz_buffer buffer = pz_encoder_render_rgb(handle_.get(), index, module_px,
                                                 quiet_zone, &size, &status);
        detail::check(status);
        Image image;
        image.size = size;
        image.rgb = detail::consume(buffer);
        return image;
    }

    /// Render a frame as a complete PNG file.
    std::vector<uint8_t> png(uint32_t index, std::size_t module_px = 8,
                             std::size_t quiet_zone = 4) const {
        pz_status status = PZ_STATUS_INTERNAL;
        pz_buffer buffer =
            pz_encoder_render_png(handle_.get(), index, module_px, quiet_zone, &status);
        detail::check(status);
        return detail::consume(buffer);
    }

private:
    std::unique_ptr<pz_encoder, detail::EncoderDeleter> handle_;
};

/// Accumulates frames until the message is complete.
class Decoder {
public:
    Decoder() : handle_(pz_decoder_new()) {
        if (!handle_) {
            throw Error(PZ_STATUS_INTERNAL);
        }
    }

    /// Offer a tightly packed RGB image.
    Progress ingest_rgb(std::size_t width, std::size_t height,
                        const uint8_t *data, std::size_t len) {
        pz_status status = PZ_STATUS_INTERNAL;
        pz_progress progress{};
        pz_decoder_ingest_rgb(handle_.get(), width, height, data, len, &progress,
                              &status);
        detail::check(status);
        return Progress::from(progress);
    }

    /// Offer a tightly packed RGBA image.
    Progress ingest_rgba(std::size_t width, std::size_t height,
                         const uint8_t *data, std::size_t len) {
        pz_status status = PZ_STATUS_INTERNAL;
        pz_progress progress{};
        pz_decoder_ingest_rgba(handle_.get(), width, height, data, len, &progress,
                               &status);
        detail::check(status);
        return Progress::from(progress);
    }

    /// Offer a frame directly, bypassing the camera path.
    Progress ingest(const Frame &frame) {
        pz_status status = PZ_STATUS_INTERNAL;
        pz_progress progress{};
        pz_decoder_ingest_frame(handle_.get(), frame.raw(), &progress, &status);
        detail::check(status);
        return Progress::from(progress);
    }

    double progress() const { return pz_decoder_progress(handle_.get()); }
    std::size_t frames_seen() const { return pz_decoder_frames_seen(handle_.get()); }
    std::size_t frames_accepted() const {
        return pz_decoder_frames_accepted(handle_.get());
    }

    /// Forget the current session so a new transmission can be received.
    void reset() { pz_decoder_reset(handle_.get()); }

    /// The completed message. Throws Error(PZ_STATUS_NOT_COMPLETE) if decoding
    /// has not finished.
    std::vector<uint8_t> result() const {
        pz_status status = PZ_STATUS_INTERNAL;
        pz_buffer buffer = pz_decoder_result(handle_.get(), &status);
        detail::check(status);
        return detail::consume(buffer);
    }

private:
    std::unique_ptr<pz_decoder, detail::DecoderDeleter> handle_;
};

/// Library version, e.g. "0.1.0".
inline std::string version() { return std::string(pz_version()); }

} // namespace pz

#endif // PHOTONIC_ZERO_HPP
