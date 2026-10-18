// SPDX-License-Identifier: MIT
//
// The board's audio codec, implemented by each project (the StackChan's is
// M5Codec in firmware/main/m5_codec.cpp).
#pragma once

#include <cstddef>
#include <cstdint>

namespace audio {

constexpr int kCodecRate = 24000;

/// The board's audio codec: duplex at kCodecRate. read() gives 2 interleaved
/// channels per frame: the microphone, then a reference channel the AFE does
/// not use (echo cancellation takes the played signal; see audio.cpp).
class Codec {
public:
    virtual ~Codec() = default;
    virtual bool init(int volume) = 0;
    /// Blocks until `frames` are read.
    virtual bool read(int16_t* interleaved, size_t frames) = 0;
    /// Mono frames to the speaker; blocks while the output is full.
    virtual bool write(const int16_t* mono, size_t frames) = 0;
    virtual void set_volume(int volume) = 0;
};

}  // namespace audio
