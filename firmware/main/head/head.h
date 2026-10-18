// SPDX-License-Identifier: MIT
//
// The head: two Feetech SCS0009 servos on a half-duplex serial bus (UART1,
// 1 Mbit/s, GPIO 6 TX / 7 RX), id 1 for yaw and id 2 for pitch. Their power
// (VM) is pin 0 of the PY32 IO expander in the StackChan base (I2C 0x6F, on
// the internal bus).
//
// Angles are in degrees from the calibrated zero: yaw -128 (the robot's left)
// to 128, pitch 3 (level) to 87 (up). One raw servo step is 0.3125 degree. The
// zero of each servo is the one the StackChan firmware calibrated (NVS
// namespace `servo`, `zero_pos_1` / `zero_pos_2`), read only; without it, the
// factory defaults.
//
// A task moves both servos along a critically damped spring at 50 Hz, like the
// StackChan firmware, so a new target never jerks the head. Once the head has
// been at rest for a while the torque is released: the servos stop humming
// into the microphone and the head can be turned by hand. The next move starts
// from where the head actually is.
//
// When the servos do not answer at boot, the task tries again every 10 s, so
// a head that was slow to wake does not stay dead until the next reset.
#pragma once

#include <atomic>
#include <mutex>

#include "botstack_board.h"
#include "driver/i2c_master.h"

namespace head {

class Head : public botstack::HeadPort {
public:
    /// Powers the servos and finds them. False when the bus or the servos do
    /// not answer yet; the head keeps trying in the background.
    bool start();
    bool ready() const override { return ready_; }

    /// Move to (yaw, pitch) degrees. speed 0..1000 sets the spring stiffness
    /// (200 is slow, 500 brisk). Returns at once; false when the head is not ready.
    bool set_angles(float yaw, float pitch, int speed) override;

private:
    struct Axis {
        int id;
        int zero;            // raw position of 0 degrees
        float min, max;      // degrees
        float position = 0;  // degrees, where the spring is
        float velocity = 0;  // degrees per second
        float target = 0;
    };

    static void task(void* self);
    void step(float stiffness, float dt);
    bool connect();
    bool power_on();
    bool read_position(int id, int& raw);
    bool write(int id, uint8_t address, const uint8_t* data, size_t length);
    bool read(int id, uint8_t address, uint8_t* data, size_t length);
    void send(const uint8_t* packet, size_t length);
    bool receive(int id, uint8_t* data, size_t length);
    void write_goals();
    void set_torque(bool on);
    static int to_raw(const Axis& axis, float degrees);

    i2c_master_dev_handle_t expander_ = nullptr;
    std::atomic<bool> ready_{false};
    Axis yaw_{1, 460, -128, 128};
    Axis pitch_{2, 620, 3, 87};

    std::mutex lock_;  // the request below, set by the link, taken by the task
    bool pending_ = false;
    float want_yaw_ = 0, want_pitch_ = 0;
    int want_speed_ = 300;
};

}  // namespace head
