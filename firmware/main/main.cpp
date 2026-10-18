// SPDX-License-Identifier: MIT
//
// botstack firmware: the bloub face, the audio path, and conversations with
// Grok through the botstack bridge.
//
// The USB flash puts it in ota_1, next to the StackChan firmware in ota_0
// (same partition table, NVS and assets untouched). An update over the air
// goes to the other slot, so the first one replaces StackChan; after it, the
// two slots hold the new botstack and the one before (docs/firmware.md#updates).
// An updated firmware keeps itself once the bridge has said hello; until then
// the bootloader goes back to the version before on the next reset. While
// ota_0 still holds StackChan, a 3 s press on the screen switches back to it.
//
// It keeps one connection to the bridge (botstack/v1). A tap starts a
// conversation; while listening it ends it; while the robot speaks it
// interrupts; touching the top of the head does the same. Wi-Fi uses the
// networks the StackChan firmware saved. The head turns when the bridge asks
// (head.set_angles).

#include "audio/audio.h"
#include "bsp/m5stack_core_s3.h"
#include "camera/camera.h"
#include "esp_app_desc.h"
#include "esp_heap_caps.h"
#include "esp_log.h"
#include "esp_ota_ops.h"
#include "esp_system.h"
#include "esp_timer.h"
#include "face/bloub.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "head/head.h"
#include "head/touch.h"
#include "m5_codec.h"
#include "net/link.h"
#include "net/wifi.h"
#include "nvs_flash.h"

#include <algorithm>
#include <cstring>
#include <vector>

namespace {

const char* TAG = "botstack";

constexpr uint32_t kBackToStackChanMs = 3000;
constexpr uint32_t kLabelMs = 1500;
constexpr uint32_t kHealthMs = 60000;

// esp_codec_dev volume: 0.5 dB per step, 100 = 0 dB. 70 was too quiet across a room.
constexpr int kVolume = 90;
// The hello's features (docs/protocol.md).
constexpr const char* kFeatures = R"(["enroll","status","reboot","head","head_touch","camera","screen:rect-320x240"])";

bloub::Face face;
M5Codec codec;
audio::Audio sound(codec);
head::Head head_servos;
head::Touch head_touch;
camera::Camera camera_unit;
net::Link bridge(sound, face, &head_servos, &camera_unit, kFeatures, net::bridge_url());
lv_obj_t* label = nullptr;
lv_timer_t* label_timer = nullptr;

void show_label(const char* text) {
    lv_label_set_text(label, text);
    lv_obj_remove_flag(label, LV_OBJ_FLAG_HIDDEN);
    lv_timer_reset(label_timer);
    lv_timer_resume(label_timer);
}

void hide_label(lv_timer_t* timer) {
    lv_obj_add_flag(label, LV_OBJ_FLAG_HIDDEN);
    lv_timer_pause(timer);
}

void on_tap(lv_event_t*) { bridge.tap(); }

/// The face mouths the speaker level.
void lips(void*) {
    while (true) {
        face.set_speech_level(sound.speaker_level());
        vTaskDelay(pdMS_TO_TICKS(40));
    }
}

const char* reset_reason() {
    switch (esp_reset_reason()) {
        case ESP_RST_POWERON: return "power on";
        case ESP_RST_SW: return "restart";
        case ESP_RST_PANIC: return "panic";
        case ESP_RST_INT_WDT: return "interrupt watchdog";
        case ESP_RST_TASK_WDT: return "task watchdog";
        case ESP_RST_WDT: return "watchdog";
        case ESP_RST_BROWNOUT: return "brownout";
        case ESP_RST_DEEPSLEEP: return "deep sleep";
        case ESP_RST_USB: return "usb";
        default: return "other";
    }
}

/// One `health` line a minute, for long serial logs (firmware/serial-log.py
/// --summary): a slow fall of `min` or `largest` is a leak or fragmentation.
void health(void*) {
    while (true) {
        vTaskDelay(pdMS_TO_TICKS(kHealthMs));
        const uint32_t up = (uint32_t)(esp_timer_get_time() / 1000000);
        ESP_LOGI(TAG,
                 "health: up %lus | internal free %u min %u largest %u | dma largest %u | psram free %u min %u "
                 "largest %u | tasks %u",
                 (unsigned long)up, (unsigned)heap_caps_get_free_size(MALLOC_CAP_INTERNAL),
                 (unsigned)heap_caps_get_minimum_free_size(MALLOC_CAP_INTERNAL),
                 (unsigned)heap_caps_get_largest_free_block(MALLOC_CAP_INTERNAL),
                 (unsigned)heap_caps_get_largest_free_block(MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL),
                 (unsigned)heap_caps_get_free_size(MALLOC_CAP_SPIRAM),
                 (unsigned)heap_caps_get_minimum_free_size(MALLOC_CAP_SPIRAM),
                 (unsigned)heap_caps_get_largest_free_block(MALLOC_CAP_SPIRAM), (unsigned)uxTaskGetNumberOfTasks());
    }
}

#if CONFIG_FREERTOS_GENERATE_RUN_TIME_STATS
/// With run-time stats on (sdkconfig, for diagnosis): every 5 s, the idle
/// share of each core and the busiest tasks.
void load(void*) {
    constexpr int kMax = 40;
    auto* now = (TaskStatus_t*)heap_caps_calloc(kMax, sizeof(TaskStatus_t), MALLOC_CAP_SPIRAM);
    auto* before = (TaskStatus_t*)heap_caps_calloc(kMax, sizeof(TaskStatus_t), MALLOC_CAP_SPIRAM);
    UBaseType_t before_n = 0;
    int64_t before_us = esp_timer_get_time();
    struct Use {
        const char* name;
        float share;
    };
    std::vector<Use> uses;
    while (true) {
        vTaskDelay(pdMS_TO_TICKS(5000));
        configRUN_TIME_COUNTER_TYPE total = 0;
        const UBaseType_t n = uxTaskGetSystemState(now, kMax, &total);
        const int64_t now_us = esp_timer_get_time();
        const float span = (float)(now_us - before_us);
        float idle[2] = {0, 0};
        uses.clear();
        for (UBaseType_t i = 0; i < n; ++i) {
            configRUN_TIME_COUNTER_TYPE delta = now[i].ulRunTimeCounter;
            for (UBaseType_t j = 0; j < before_n; ++j) {
                if (before[j].xHandle == now[i].xHandle) {
                    delta = now[i].ulRunTimeCounter - before[j].ulRunTimeCounter;
                }
            }
            const float share = 100.0f * delta / span;
            if (std::strncmp(now[i].pcTaskName, "IDLE", 4) == 0) {
                idle[now[i].pcTaskName[4] == '1' ? 1 : 0] = share;
            } else {
                uses.push_back({now[i].pcTaskName, share});
            }
        }
        std::sort(uses.begin(), uses.end(), [](const Use& a, const Use& b) { return a.share > b.share; });
        char line[200];
        int at = std::snprintf(line, sizeof(line), "load: idle core0 %.0f%% core1 %.0f%% |", idle[0], idle[1]);
        for (size_t i = 0; i < uses.size() && i < 6 && at < (int)sizeof(line); ++i) {
            at += std::snprintf(line + at, sizeof(line) - at, " %s %.0f%%", uses[i].name, uses[i].share);
        }
        ESP_LOGI(TAG, "%s", line);
        std::swap(now, before);
        before_n = n;
        before_us = now_us;
    }
}
#endif

/// Wi-Fi in the background, so the face is up at once. It never gives up:
/// after a power cut the router can come back after the robot.
void network(void*) {
    face.set_expression(bloub::find_expression("somnolent"));
    while (!net::wifi_connect(net::saved_networks(), 60000)) {
        face.set_expression(bloub::find_expression("triste"));
        vTaskDelay(pdMS_TO_TICKS(30000));
        face.set_expression(bloub::find_expression("somnolent"));
    }
    face.set_expression(bloub::find_expression("neutre"));
    bridge.start();
    vTaskDelete(nullptr);
}

void back_to_stackchan(lv_event_t*) {
    const esp_partition_t* stackchan =
        esp_partition_find_first(ESP_PARTITION_TYPE_APP, ESP_PARTITION_SUBTYPE_APP_OTA_0, nullptr);
    // After the first update over the air, ota_0 holds botstack.
    esp_app_desc_t desc;
    if (stackchan != nullptr && esp_ota_get_partition_description(stackchan, &desc) == ESP_OK &&
        std::strcmp(desc.project_name, esp_app_get_description()->project_name) == 0) {
        ESP_LOGW(TAG, "ota_0 holds botstack: StackChan was replaced by an update");
        show_label("StackChan replaced");
        return;
    }
    if (stackchan == nullptr || esp_ota_set_boot_partition(stackchan) != ESP_OK) {
        ESP_LOGE(TAG, "cannot select the StackChan firmware");
        show_label("StackChan not found");
        return;
    }
    ESP_LOGW(TAG, "back to the StackChan firmware");
    show_label("StackChan...");
    lv_refr_now(nullptr);
    vTaskDelay(pdMS_TO_TICKS(300));
    esp_restart();
}

}  // namespace

extern "C" void app_main(void) {
    // Wi-Fi credentials and servo zeros live in NVS: never erase it here.
    esp_err_t err = nvs_flash_init();
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "NVS unavailable (%s); continuing without it", esp_err_to_name(err));
    }

    const esp_partition_t* running = esp_ota_get_running_partition();
    ESP_LOGI(TAG, "botstack firmware running from %s, reset: %s", running ? running->label : "?", reset_reason());

    lv_display_t* display = bsp_display_start();
    if (display == nullptr) {
        ESP_LOGE(TAG, "display start failed");
        return;  // an update is not kept: the bootloader goes back on reset
    }
    bsp_display_backlight_on();

    bsp_display_lock(0);
    lv_obj_t* screen = lv_screen_active();
    face.init(screen);

    label = lv_label_create(screen);
    lv_obj_set_style_text_color(label, lv_color_white(), 0);
    lv_obj_set_style_text_font(label, &lv_font_montserrat_20, 0);
    lv_obj_align(label, LV_ALIGN_BOTTOM_MID, 0, -6);
    lv_obj_add_flag(label, LV_OBJ_FLAG_HIDDEN);
    label_timer = lv_timer_create(hide_label, kLabelMs, nullptr);
    lv_timer_pause(label_timer);

    lv_obj_add_flag(screen, LV_OBJ_FLAG_CLICKABLE);
    lv_obj_add_event_cb(screen, on_tap, LV_EVENT_SHORT_CLICKED, nullptr);
    lv_obj_add_event_cb(screen, back_to_stackchan, LV_EVENT_LONG_PRESSED, nullptr);
    if (lv_indev_t* touch = bsp_display_get_input_dev()) {
        lv_indev_set_long_press_time(touch, kBackToStackChanMs);
    }
    bsp_display_unlock();

    if (!sound.start(kVolume)) {
        ESP_LOGE(TAG, "audio start failed");
        return;
    }
    sound.set_mic_enabled(false);
    // Without the head, Grok is told it does not answer; the rest runs, and
    // the head keeps trying to find its servos.
    if (!head_servos.start()) {
        ESP_LOGE(TAG, "head unavailable; trying again every 10 s");
    }
    // Touching the top of the head does what a tap on the screen does.
    if (!head_touch.start([] { bridge.tap(); })) {
        ESP_LOGE(TAG, "head touch unavailable");
    }
    xTaskCreate(lips, "lips", 2048, nullptr, 2, nullptr);
    xTaskCreate(network, "network", 6144, nullptr, 4, nullptr);
    xTaskCreatePinnedToCoreWithCaps(health, "health", 3072, nullptr, 1, nullptr, 0, MALLOC_CAP_SPIRAM);
#if CONFIG_FREERTOS_GENERATE_RUN_TIME_STATS
    xTaskCreatePinnedToCoreWithCaps(load, "load", 4096, nullptr, 1, nullptr, 0, MALLOC_CAP_SPIRAM);
#endif
    ESP_LOGI(TAG, "face running; free internal %u, PSRAM %u",
             (unsigned)heap_caps_get_free_size(MALLOC_CAP_INTERNAL),
             (unsigned)heap_caps_get_free_size(MALLOC_CAP_SPIRAM));
}
