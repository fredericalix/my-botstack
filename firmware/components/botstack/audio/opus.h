// SPDX-License-Identifier: MIT
//
// Opus through esp_audio_codec: the microphone as 16 kHz mono 60 ms packets
// (what the bridge decodes today), the voice as 24 kHz mono 60 ms packets.
#pragma once

#include <cstddef>
#include <cstdint>
#include <vector>

namespace audio {

class OpusEncoder {
public:
    bool open(int sample_rate, int bitrate);
    /// One 60 ms frame of PCM in, one packet out. False on error.
    bool encode(const int16_t* pcm, std::vector<uint8_t>& packet);
    size_t frame_samples() const { return frame_samples_; }

private:
    void* handle_ = nullptr;
    size_t frame_samples_ = 0;
    int max_packet_ = 0;
};

class OpusDecoder {
public:
    bool open(int sample_rate);
    /// One packet in, its PCM appended to `pcm`. False on error.
    bool decode(const uint8_t* packet, size_t size, std::vector<int16_t>& pcm);

private:
    void* handle_ = nullptr;
    std::vector<int16_t> buffer_;
};

}  // namespace audio
