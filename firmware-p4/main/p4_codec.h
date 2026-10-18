// SPDX-License-Identifier: MIT
//
// Waveshare ESP32-P4 4C audio: ES8311 DAC (power amplifier on GPIO 53) and
// ES7210 microphone ADC on one duplex I2S bus at 24 kHz, both set up by the
// board's BSP. There is no hardware reference channel: read() returns the
// microphone and a silent second channel (echo cancellation uses the played
// signal, see audio.cpp).
#pragma once

#include <cstddef>
#include <cstdint>
#include <vector>

#include "audio/codec.h"
#include "esp_codec_dev.h"

class P4Codec : public audio::Codec {
public:
    bool init(int volume) override;
    /// Interleaved [mic, 0] frames; blocks until `frames` are read.
    bool read(int16_t* interleaved, size_t frames) override;
    /// Mono frames to the speaker; blocks while the I2S DMA is full.
    bool write(const int16_t* mono, size_t frames) override;
    void set_volume(int volume) override;

private:
    esp_codec_dev_handle_t input_ = nullptr;
    esp_codec_dev_handle_t output_ = nullptr;
    std::vector<int16_t> mono_;
};
