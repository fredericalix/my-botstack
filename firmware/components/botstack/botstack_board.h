// SPDX-License-Identifier: MIT
//
// What a board may add to the shared firmware: a head and a camera. net::Link
// takes a pointer to each, null when the board has none (the bridge then gets
// "Not available on this firmware." for their tools, and `status` says "none").
#pragma once

#include <cstddef>
#include <cstdint>

namespace botstack {

class HeadPort {
public:
    virtual ~HeadPort() = default;
    virtual bool ready() const = 0;
    /// Move to (yaw, pitch) degrees at speed 0..1000. Returns at once; false
    /// when the head is not ready.
    virtual bool set_angles(float yaw, float pitch, int speed) = 0;
};

class CameraPort {
public:
    virtual ~CameraPort() = default;
    /// Once, from the link's camera task.
    virtual bool start() = 0;
    /// One JPEG. On success `*jpeg` is a buffer the caller frees with free().
    virtual bool take(uint8_t** jpeg, size_t* size) = 0;
};

}  // namespace botstack
