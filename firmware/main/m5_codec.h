// SPDX-License-Identifier: MIT
//
// CoreS3 audio: ES7210 microphone ADC and AW88298 speaker amp on one duplex
// I2S bus at 24 kHz. The ES7210 also captures the speaker signal on a second
// channel, the hardware reference for echo cancellation, so the robot can be
// interrupted while it speaks. The BSP's audio setup has no reference
// channel, so this one builds its own (the same wiring the StackChan firmware
// uses: standard I2S out, 4-slot TDM in, slot 0 microphone, slot 1 reference).
#pragma once

#include <cstddef>
#include <cstdint>

#include "audio/codec.h"
#include "esp_codec_dev.h"

class M5Codec : public audio::Codec {
public:
    bool init(int volume) override;
    /// Interleaved [mic, reference] frames; blocks until `frames` are read.
    bool read(int16_t* interleaved, size_t frames) override;
    /// Mono frames to the speaker; blocks while the I2S DMA is full.
    bool write(const int16_t* mono, size_t frames) override;
    void set_volume(int volume) override;

private:
    esp_codec_dev_handle_t input_ = nullptr;
    esp_codec_dev_handle_t output_ = nullptr;
};
