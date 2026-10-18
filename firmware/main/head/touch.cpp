// SPDX-License-Identifier: MIT
#include "head/touch.h"

#include "bsp/m5stack_core_s3.h"
#include "esp_heap_caps.h"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

namespace head {
namespace {

const char* TAG = "touch";

constexpr uint8_t kAddress = 0x68;
constexpr int kPollMs = 50;

// Si12T registers, set up as the StackChan firmware does.
constexpr uint8_t kSensitivityFirst = 0x02;  // 0x02..0x06, one per channel pair
constexpr uint8_t kSensitivityLast = 0x06;
constexpr uint8_t kSensitivity = 0x33;       // low type, level 3
constexpr uint8_t kCtrl1 = 0x08;
constexpr uint8_t kCtrl2 = 0x09;
constexpr uint8_t kChannelRegisters[] = {0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F};  // reference reset, hold, calibration hold
constexpr uint8_t kOutput = 0x10;            // 2 bits per zone: 0 none .. 3 high

}  // namespace

bool Touch::start(Callback on_touch) {
    on_touch_ = on_touch;
    if (bsp_i2c_init() != ESP_OK) {
        return false;
    }
    i2c_device_config_t config = {};
    config.dev_addr_length = I2C_ADDR_BIT_LEN_7;
    config.device_address = kAddress;
    config.scl_speed_hz = 100000;
    if (i2c_master_bus_add_device(bsp_i2c_get_handle(), &config, &device_) != ESP_OK) {
        ESP_LOGE(TAG, "cannot add the touch controller");
        return false;
    }
    bool ok = true;
    for (uint8_t reg : kChannelRegisters) {
        ok = ok && write(reg, 0x00);  // all channels on, calibrating
    }
    // Software reset, then auto mode with sleep enabled; interrupt on middle and high.
    ok = ok && write(kCtrl2, 0x0F) && write(kCtrl2, 0x07) && write(kCtrl1, 0x22);
    for (uint8_t reg = kSensitivityFirst; reg <= kSensitivityLast; ++reg) {
        ok = ok && write(reg, kSensitivity);
    }
    if (!ok) {
        ESP_LOGE(TAG, "touch controller does not answer");
        return false;
    }
    ESP_LOGI(TAG, "head touch ready");
    // While the robot speaks, core 0 has 2-5 % idle and a priority 2 task
    // there missed touches: core 1, above LVGL (4). Each poll is one short I2C read.
    xTaskCreatePinnedToCoreWithCaps(task, "touch", 3072, this, 5, nullptr, 1, MALLOC_CAP_SPIRAM);
    return true;
}

void Touch::task(void* arg) {
    auto* self = static_cast<Touch*>(arg);
    bool touched = false;
    while (true) {
        vTaskDelay(pdMS_TO_TICKS(kPollMs));
        uint8_t output = 0;
        if (!self->read(kOutput, output)) {
            continue;
        }
        const bool now = (output & 0x3F) != 0;  // any of the three zones
        if (now && !touched) {
            ESP_LOGI(TAG, "touched (0x%02X)", output);
            self->on_touch_();
        }
        touched = now;
    }
}

bool Touch::write(uint8_t reg, uint8_t value) {
    const uint8_t buffer[2] = {reg, value};
    return i2c_master_transmit(device_, buffer, sizeof(buffer), 100) == ESP_OK;
}

bool Touch::read(uint8_t reg, uint8_t& value) {
    return i2c_master_transmit_receive(device_, &reg, 1, &value, 1, 100) == ESP_OK;
}

}  // namespace head
