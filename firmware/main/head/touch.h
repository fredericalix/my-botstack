// SPDX-License-Identifier: MIT
//
// The touch pad on top of the head: an Si12T capacitive controller (I2C 0x68,
// on the internal bus) with three zones, front to back. A task reads it every
// 50 ms and calls back once per touch, when a finger lands on any zone; the
// finger must lift before the next touch counts.
#pragma once

#include "driver/i2c_master.h"

namespace head {

class Touch {
public:
    using Callback = void (*)();

    /// Sets the controller up and starts polling. False when it does not answer.
    bool start(Callback on_touch);

private:
    static void task(void* self);
    bool write(uint8_t reg, uint8_t value);
    bool read(uint8_t reg, uint8_t& value);

    i2c_master_dev_handle_t device_ = nullptr;
    Callback on_touch_ = nullptr;
};

}  // namespace head
