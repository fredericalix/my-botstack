// SPDX-License-Identifier: MIT
#include "head/head.h"

#include <algorithm>
#include <cmath>
#include <cstring>

#include "bsp/m5stack_core_s3.h"
#include "driver/uart.h"
#include "esp_heap_caps.h"
#include "esp_log.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "nvs.h"

namespace head {
namespace {

const char* TAG = "head";

constexpr uart_port_t kUart = UART_NUM_1;
constexpr int kBaud = 1000000;
constexpr int kTxPin = 6;
constexpr int kRxPin = 7;
constexpr TickType_t kReplyTimeout = pdMS_TO_TICKS(6);

// PY32 IO expander in the base: pin 0 switches the servo power (VM).
constexpr uint8_t kExpanderAddress = 0x6F;
constexpr uint8_t kRegVersion = 0x02;
constexpr uint8_t kRegMode = 0x03;      // 1 = output
constexpr uint8_t kRegOutput = 0x05;
constexpr uint8_t kRegPullUp = 0x09;
constexpr uint8_t kRegPullDown = 0x0B;
constexpr uint8_t kPowerBit = 1 << 0;

// SCS0009 (Feetech SCSCL): instructions and memory table; words are big-endian.
constexpr uint8_t kRead = 0x02;
constexpr uint8_t kWrite = 0x03;
constexpr uint8_t kSyncWrite = 0x83;
constexpr uint8_t kBroadcast = 0xFE;
constexpr uint8_t kAngleLimits = 9;  // min (2), max (2); both 0 = PWM mode
constexpr uint8_t kTorque = 40;
constexpr uint8_t kGoal = 42;        // position (2), time ms (2), speed (2)
constexpr uint8_t kPresentPosition = 56;
constexpr int kRawMax = 1000;        // beyond, readings are not positions
constexpr float kStepsPerDegree = 3.2f;  // 0.3125 degree a step

constexpr int kTickMs = 20;           // 50 Hz, the servo's goal time
constexpr int kRetryMs = 10000;       // servos missing at boot: try again this often
constexpr int64_t kReleaseUs = 1'000'000;  // torque off after this long at rest

uint8_t checksum(const uint8_t* bytes, size_t length) {
    uint8_t sum = 0;
    for (size_t i = 0; i < length; ++i) {
        sum += bytes[i];
    }
    return ~sum;
}

}  // namespace

bool Head::start() {
    // Zeros calibrated by the StackChan firmware; read only, never written here.
    // Read here, in the main task: the head task's stack is in PSRAM.
    nvs_handle_t nvs;
    if (nvs_open("servo", NVS_READONLY, &nvs) == ESP_OK) {
        for (Axis* axis : {&yaw_, &pitch_}) {
            char key[] = "zero_pos_0";
            key[sizeof(key) - 2] = (char)('0' + axis->id);
            int32_t zero = -1;
            if (nvs_get_i32(nvs, key, &zero) == ESP_OK && zero >= 0 && zero <= kRawMax) {
                axis->zero = (int)zero;
            }
        }
        nvs_close(nvs);
    }
    ready_ = connect();
    // The task keeps trying while the servos are missing, then moves the head.
    xTaskCreatePinnedToCoreWithCaps(task, "head", 4096, this, 4, nullptr, 0, MALLOC_CAP_SPIRAM);
    return ready_;
}

bool Head::connect() {
    if (!power_on()) {
        return false;
    }

    uart_config_t config = {};
    config.baud_rate = kBaud;
    config.data_bits = UART_DATA_8_BITS;
    config.parity = UART_PARITY_DISABLE;
    config.stop_bits = UART_STOP_BITS_1;
    config.flow_ctrl = UART_HW_FLOWCTRL_DISABLE;
    config.source_clk = UART_SCLK_DEFAULT;
    if ((!uart_is_driver_installed(kUart) && uart_driver_install(kUart, 256, 0, 0, nullptr, 0) != ESP_OK) ||
        uart_param_config(kUart, &config) != ESP_OK ||
        uart_set_pin(kUart, kTxPin, kRxPin, UART_PIN_NO_CHANGE, UART_PIN_NO_CHANGE) != ESP_OK) {
        ESP_LOGE(TAG, "servo bus setup failed");
        return false;
    }

    for (Axis* axis : {&yaw_, &pitch_}) {
        int raw = 0;
        if (!read_position(axis->id, raw)) {
            ESP_LOGE(TAG, "servo %d does not answer", axis->id);
            return false;
        }
        // The StackChan firmware turns the yaw servo continuously in PWM mode,
        // which clears its angle limits; give it its position range back.
        uint8_t limits[4] = {};
        if (read(axis->id, kAngleLimits, limits, sizeof(limits)) && std::all_of(limits, limits + 4, [](uint8_t b) { return b == 0; })) {
            const uint8_t range[4] = {0x00, 0x00, 0x03, 0xFF};  // 0 .. 1023
            ESP_LOGW(TAG, "servo %d was in PWM mode; back to position mode", axis->id);
            write(axis->id, kAngleLimits, range, sizeof(range));
        }
        axis->position = axis->target = std::clamp((raw - axis->zero) / kStepsPerDegree, axis->min, axis->max);
        ESP_LOGI(TAG, "servo %d: zero %d, at %.1f degrees", axis->id, axis->zero, axis->position);
    }
    set_torque(false);
    return true;
}

bool Head::set_angles(float yaw, float pitch, int speed) {
    if (!ready_) {
        return false;
    }
    std::lock_guard<std::mutex> lock(lock_);
    want_yaw_ = yaw;
    want_pitch_ = pitch;
    want_speed_ = std::clamp(speed, 0, 1000);
    pending_ = true;
    return true;
}

void Head::task(void* arg) {
    auto* self = static_cast<Head*>(arg);
    for (int tries = 1; !self->ready_; ++tries) {
        vTaskDelay(pdMS_TO_TICKS(kRetryMs));
        if (self->connect()) {
            ESP_LOGW(TAG, "head ready after %d tries", tries + 1);
            self->ready_ = true;
        }
    }
    const float dt = kTickMs / 1000.0f;
    float stiffness = 0;
    bool moving = false, torque = false;
    int64_t rest_since = 0;
    TickType_t wake = xTaskGetTickCount();
    while (true) {
        vTaskDelayUntil(&wake, pdMS_TO_TICKS(kTickMs));
        bool request = false;
        float yaw = 0, pitch = 0;
        int speed = 0;
        {
            std::lock_guard<std::mutex> lock(self->lock_);
            if (self->pending_) {
                self->pending_ = false;
                request = true;
                yaw = self->want_yaw_;
                pitch = self->want_pitch_;
                speed = self->want_speed_;
            }
        }
        if (request) {
            if (!torque) {
                // The head may have been turned by hand while it was limp.
                for (Axis* axis : {&self->yaw_, &self->pitch_}) {
                    int raw = 0;
                    if (self->read_position(axis->id, raw)) {
                        axis->position = std::clamp((raw - axis->zero) / kStepsPerDegree, axis->min, axis->max);
                        axis->velocity = 0;
                    }
                }
                self->write_goals();
                self->set_torque(true);
                torque = true;
            }
            self->yaw_.target = std::clamp(yaw, self->yaw_.min, self->yaw_.max);
            self->pitch_.target = std::clamp(pitch, self->pitch_.min, self->pitch_.max);
            // The StackChan firmware's mapping: stiffness grows with the square
            // of the speed, damping keeps it critical (no overshoot).
            const float s = speed / 1000.0f;
            stiffness = 10.0f + s * s * 640.0f;
            moving = true;
        }
        if (moving) {
            self->step(stiffness, dt);
            bool settled = true;
            for (Axis* axis : {&self->yaw_, &self->pitch_}) {
                if (std::fabs(axis->position - axis->target) > 0.2f || std::fabs(axis->velocity) > 1.0f) {
                    settled = false;
                }
            }
            if (settled) {
                for (Axis* axis : {&self->yaw_, &self->pitch_}) {
                    axis->position = axis->target;
                    axis->velocity = 0;
                }
                moving = false;
                rest_since = esp_timer_get_time();
            }
            self->write_goals();
        } else if (torque && esp_timer_get_time() - rest_since > kReleaseUs) {
            self->set_torque(false);
            torque = false;
        }
    }
}

void Head::step(float stiffness, float dt) {
    const float damping = 2.0f * std::sqrt(stiffness);
    for (Axis* axis : {&yaw_, &pitch_}) {
        const float force = stiffness * (axis->target - axis->position) - damping * axis->velocity;
        axis->velocity += force * dt;
        axis->position += axis->velocity * dt;
    }
}

int Head::to_raw(const Axis& axis, float degrees) {
    return std::clamp(axis.zero + (int)std::lround(degrees * kStepsPerDegree), 0, kRawMax);
}

bool Head::power_on() {
    if (bsp_i2c_init() != ESP_OK) {
        ESP_LOGE(TAG, "no I2C bus");
        return false;
    }
    i2c_device_config_t config = {};
    config.dev_addr_length = I2C_ADDR_BIT_LEN_7;
    config.device_address = kExpanderAddress;
    config.scl_speed_hz = 100000;
    if (expander_ == nullptr && i2c_master_bus_add_device(bsp_i2c_get_handle(), &config, &expander_) != ESP_OK) {
        ESP_LOGE(TAG, "cannot add the IO expander");
        return false;
    }
    auto read_reg = [this](uint8_t reg, uint8_t& value) {
        return i2c_master_transmit_receive(expander_, &reg, 1, &value, 1, 100) == ESP_OK;
    };
    auto set_bit = [this, &read_reg](uint8_t reg, bool on) {
        uint8_t value = 0;
        if (!read_reg(reg, value)) {
            return false;
        }
        value = on ? (value | kPowerBit) : (value & ~kPowerBit);
        const uint8_t buffer[2] = {reg, value};
        return i2c_master_transmit(expander_, buffer, sizeof(buffer), 100) == ESP_OK;
    };
    // The expander can be slow to boot after a power on.
    uint8_t version = 0;
    for (int i = 0; i < 6 && !(read_reg(kRegVersion, version) && version != 0 && version != 0xFF); ++i) {
        vTaskDelay(pdMS_TO_TICKS(200));
    }
    if (version == 0 || version == 0xFF) {
        ESP_LOGE(TAG, "IO expander not found");
        return false;
    }
    if (!set_bit(kRegMode, true) || !set_bit(kRegPullDown, false) || !set_bit(kRegPullUp, true) ||
        !set_bit(kRegOutput, true)) {
        ESP_LOGE(TAG, "cannot switch the servo power on");
        return false;
    }
    ESP_LOGI(TAG, "servo power on (IO expander version 0x%02X)", version);
    vTaskDelay(pdMS_TO_TICKS(200));  // the servos boot
    return true;
}

bool Head::read_position(int id, int& raw) {
    uint8_t word[2];
    if (!read(id, kPresentPosition, word, sizeof(word))) {
        return false;
    }
    raw = (word[0] << 8) | word[1];
    return raw <= kRawMax;
}

void Head::send(const uint8_t* packet, size_t length) {
    uart_flush_input(kUart);
    uart_write_bytes(kUart, packet, length);
    uart_wait_tx_done(kUart, pdMS_TO_TICKS(10));
}

bool Head::write(int id, uint8_t address, const uint8_t* data, size_t length) {
    uint8_t packet[16] = {0xFF, 0xFF, (uint8_t)id, (uint8_t)(length + 3), kWrite, address};
    if (length > sizeof(packet) - 7) {
        return false;
    }
    std::memcpy(packet + 6, data, length);
    packet[6 + length] = checksum(packet + 2, 4 + length);
    send(packet, 7 + length);
    return receive(id, nullptr, 0);
}

bool Head::read(int id, uint8_t address, uint8_t* data, size_t length) {
    uint8_t packet[8] = {0xFF, 0xFF, (uint8_t)id, 4, kRead, address, (uint8_t)length};
    packet[7] = checksum(packet + 2, 5);
    send(packet, sizeof(packet));
    return receive(id, data, length);
}

/// A status packet: FF FF id length error data... checksum.
bool Head::receive(int id, uint8_t* data, size_t length) {
    uint8_t reply[24];
    const size_t want = 6 + length;
    if (want > sizeof(reply)) {
        return false;
    }
    int got = uart_read_bytes(kUart, reply, want, kReplyTimeout);
    if (got != (int)want || reply[0] != 0xFF || reply[1] != 0xFF || reply[2] != id || reply[3] != length + 2 ||
        reply[want - 1] != checksum(reply + 2, want - 3)) {
        return false;
    }
    if (data != nullptr) {
        std::memcpy(data, reply + 5, length);
    }
    return true;
}

/// Both goals in one broadcast packet (no reply).
void Head::write_goals() {
    uint8_t packet[22] = {0xFF, 0xFF, kBroadcast, 18, kSyncWrite, kGoal, 6};
    size_t at = 7;
    for (const Axis* axis : {&yaw_, &pitch_}) {
        const int raw = to_raw(*axis, axis->position);
        const uint8_t entry[7] = {(uint8_t)axis->id, (uint8_t)(raw >> 8), (uint8_t)raw, 0, kTickMs, 0, 0};
        std::memcpy(packet + at, entry, sizeof(entry));
        at += sizeof(entry);
    }
    packet[at] = checksum(packet + 2, at - 2);
    send(packet, at + 1);
}

void Head::set_torque(bool on) {
    uint8_t packet[12] = {0xFF, 0xFF, kBroadcast, 8, kSyncWrite, kTorque, 1,
                          (uint8_t)yaw_.id, (uint8_t)on, (uint8_t)pitch_.id, (uint8_t)on};
    packet[11] = checksum(packet + 2, 9);
    send(packet, sizeof(packet));
}

}  // namespace head
