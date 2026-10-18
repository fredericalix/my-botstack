// SPDX-License-Identifier: MIT
#include "audio/resampler.h"

#include <algorithm>
#include <cmath>

namespace audio {

Resampler::Resampler(int up, int down, int taps_per_phase)
    : up_(up), down_(down), taps_(taps_per_phase), coeffs_(up * taps_per_phase), history_(taps_per_phase, 0.0f) {
    // Low-pass at 90% of the lower Nyquist rate, in cycles per up-sampled sample.
    const int n = up_ * taps_;
    const float cutoff = 0.45f / std::max(up_, down_);
    const float pi = 3.14159265f;
    for (int i = 0; i < n; i++) {
        float x = i - (n - 1) / 2.0f;
        float sinc = x == 0 ? 2 * cutoff : std::sin(2 * pi * cutoff * x) / (pi * x);
        float window = 0.5f - 0.5f * std::cos(2 * pi * i / (n - 1));
        // Phase p takes taps p, p+up, p+2up, ...; scaled by up to keep unity gain.
        coeffs_[(i % up_) * taps_ + i / up_] = sinc * window * up_;
    }
}

void Resampler::process(const int16_t* in, size_t frames, std::vector<int16_t>& out) {
    for (size_t k = 0; k < frames; k++) {
        std::rotate(history_.begin(), history_.begin() + 1, history_.end());
        history_.back() = in[k];
        // Output samples fall every `down_` steps on the up-sampled grid; this
        // input sample covers the next `up_` grid positions.
        while (phase_ < up_) {
            const float* c = &coeffs_[phase_ * taps_];
            float acc = 0;
            for (int t = 0; t < taps_; t++) {
                acc += c[t] * history_[taps_ - 1 - t];
            }
            out.push_back((int16_t)std::clamp(acc, -32768.0f, 32767.0f));
            phase_ += down_;
        }
        phase_ -= up_;
    }
}

}  // namespace audio
