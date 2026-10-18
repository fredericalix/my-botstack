// SPDX-License-Identifier: MIT
//
// Rational polyphase resampler (up L, down M) with a Hann-windowed sinc, for
// 24 kHz <-> 16 kHz. Mono, streaming: keeps its history between calls.
#pragma once

#include <cstddef>
#include <cstdint>
#include <vector>

namespace audio {

class Resampler {
public:
    Resampler(int up, int down, int taps_per_phase = 16);
    /// Appends the resampled frames of `in` to `out`.
    void process(const int16_t* in, size_t frames, std::vector<int16_t>& out);

private:
    int up_, down_, taps_;
    std::vector<float> coeffs_;   // up_ phases x taps_
    std::vector<float> history_;  // last taps_ input samples
    int phase_ = 0;               // next output position in the up-sampled grid, mod up_
};

}  // namespace audio
