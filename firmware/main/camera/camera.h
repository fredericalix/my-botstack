// SPDX-License-Identifier: MIT
//
// The CoreS3 camera (GC0308 on DVP) through esp_video's V4L2-style device,
// one still at a time: 320x240 YUYV, encoded to JPEG in software (the
// ESP32-S3 has no JPEG engine). 640x480 overran the DVP copy task while
// audio ran (torn, dark frames). The stream runs only while a photo is taken;
// the first frames are skipped so exposure and white balance settle.
#pragma once

#include <cstddef>
#include <cstdint>

#include "botstack_board.h"

namespace camera {

constexpr int kWidth = 320;
constexpr int kHeight = 240;

class Camera : public botstack::CameraPort {
public:
    /// Power the sensor, open the device, map its buffers. Once, at boot.
    bool start() override;
    /// One JPEG. On success `*jpeg` is a PSRAM buffer the caller frees with free().
    bool take(uint8_t** jpeg, size_t* size) override;

private:
    static constexpr int kBuffers = 2;
    int fd_ = -1;
    uint8_t* buffers_[kBuffers] = {};
    bool ready_ = false;
};

}  // namespace camera
