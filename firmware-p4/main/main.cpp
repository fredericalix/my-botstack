// SPDX-License-Identifier: MIT
//
// botstack firmware for the Waveshare ESP32-P4-WIFI6-Touch-LCD-4C: the bloub
// face on the 720x720 round panel, the audio path, and conversations with
// Grok through the botstack bridge. No head and no camera.
//
// The USB flash (flash.sh) writes the bootloader, the partition table, the app
// and the Wi-Fi networks; updates over the air go to the other app slot. An
// updated firmware keeps itself once the bridge has said hello; until then the
// bootloader goes back to the version before on the next reset, and an update
// that cannot start (display, audio, no Wi-Fi for about 4 minutes) restarts to
// get there.
//
// It keeps one connection to the bridge (botstack/v1). A tap anywhere on the
// screen starts a conversation; while listening it ends it; while the board
// speaks it interrupts. Wi-Fi uses the networks of the `wifi_nvs` partition
// (written by flash.sh from firmware-p4/wifi.txt), through the on-board
// ESP32-C6 (esp_hosted).

#include "audio/audio.h"
#include "bsp/esp-bsp.h"
#include "esp_heap_caps.h"
#include "esp_log.h"
#include "esp_ota_ops.h"
#include "esp_system.h"
#include "esp_timer.h"
#include "face/bloub.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "net/link.h"
#include "net/wifi.h"
#include "nvs_flash.h"
#include "p4_codec.h"

namespace {

const char* TAG = "botstack";

constexpr uint32_t kHealthMs = 60000;
// Wi-Fi rounds (60 s of tries, then 30 s of sad face) before an update still
// pending gives up: 3 rounds are about 4 minutes.
constexpr int kPendingWifiRounds = 3;
// The partition flash.sh writes from wifi.txt (partitions.csv).
constexpr const char* kWifiPartition = "wifi_nvs";

// The hello's features (docs/protocol.md).
constexpr const char* kFeatures = R"(["enroll","status","reboot","screen:round-720"])";
// The face's disc, in pixels, on the 720 px panel.
constexpr int kDiscSize = 600;
// esp_codec_dev volume: 0.5 dB per step, 100 = 0 dB. A first guess; tuned by ear.
constexpr int kVolume = 80;

bloub::Face face;
P4Codec codec;
audio::Audio sound(codec);
net::Link bridge(sound, face, nullptr, nullptr, kFeatures, net::bridge_url());

void on_tap(lv_event_t*) { bridge.tap(); }

/// True while this firmware is an update the bootloader has not kept yet.
bool update_pending() {
    const esp_partition_t* running = esp_ota_get_running_partition();
    esp_ota_img_states_t state;
    return running != nullptr && esp_ota_get_state_partition(running, &state) == ESP_OK &&
           state == ESP_OTA_IMG_PENDING_VERIFY;
}

/// An update that cannot start restarts, so the bootloader goes back to the
/// version before; a USB-flashed firmware stays (and shows what is wrong).
void give_up_if_pending(const char* why) {
    if (update_pending()) {
        ESP_LOGE(TAG, "%s: restarting, the bootloader goes back to the version before", why);
        vTaskDelay(pdMS_TO_TICKS(300));
        esp_restart();
    }
}

/// flash.sh's networks; an empty password is an open network on this board.
std::vector<net::Network> networks() {
    std::vector<net::Network> list = net::saved_networks(kWifiPartition);
    for (auto& network : list) {
        network.open = network.password.empty();
    }
    return list;
}

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

/// One `health` line a minute, for long serial logs: a slow fall of `min` or
/// `largest` is a leak or fragmentation.
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

/// Wi-Fi in the background, so the face is up at once. It never gives up:
/// after a power cut the router can come back after the board.
void network(void*) {
    face.set_expression(bloub::find_expression("somnolent"));
    int failed = 0;
    while (!net::wifi_connect(networks(), 60000)) {
        if (++failed >= kPendingWifiRounds) {
            give_up_if_pending("no Wi-Fi");
        }
        face.set_expression(bloub::find_expression("triste"));
        vTaskDelay(pdMS_TO_TICKS(30000));
        face.set_expression(bloub::find_expression("somnolent"));
    }
    face.set_expression(bloub::find_expression("neutre"));
    bridge.start();
    vTaskDelete(nullptr);
}

}  // namespace

extern "C" void app_main(void) {
    // This board's `nvs` holds only what botstack writes (its key): a full or
    // outdated partition is erased and started again. Never `wifi_nvs`.
    esp_err_t err = nvs_flash_init();
    if (err == ESP_ERR_NVS_NO_FREE_PAGES || err == ESP_ERR_NVS_NEW_VERSION_FOUND) {
        ESP_LOGW(TAG, "NVS erased (%s)", esp_err_to_name(err));
        nvs_flash_erase();
        err = nvs_flash_init();
    }
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "NVS unavailable (%s); continuing without it", esp_err_to_name(err));
    }
    // The Wi-Fi networks: a read-only partition only flash.sh writes. Never
    // erased here; without it the board has no network (flash.sh again).
    err = nvs_flash_init_partition(kWifiPartition);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "%s unavailable (%s): run firmware-p4/flash.sh", kWifiPartition, esp_err_to_name(err));
    }

    const esp_partition_t* running = esp_ota_get_running_partition();
    ESP_LOGI(TAG, "botstack-p4 firmware running from %s, reset: %s", running ? running->label : "?", reset_reason());

    lv_display_t* display = bsp_display_start();
    if (display == nullptr) {
        ESP_LOGE(TAG, "display start failed");
        give_up_if_pending("display start failed");
        return;
    }
    bsp_display_backlight_on();

    // bsp_display_lock(0) only tries the lock on this BSP (esp_lvgl_adapter);
    // -1 waits for it.
    esp_lv_adapter_lock(-1);
    lv_obj_t* screen = lv_screen_active();
    face.init(screen, kDiscSize, true);  // the round panel
    lv_obj_add_flag(screen, LV_OBJ_FLAG_CLICKABLE);
    lv_obj_add_event_cb(screen, on_tap, LV_EVENT_SHORT_CLICKED, nullptr);
    esp_lv_adapter_unlock();

    if (!sound.start(kVolume)) {
        ESP_LOGE(TAG, "audio start failed");
        give_up_if_pending("audio start failed");
        return;
    }
    sound.set_mic_enabled(false);
    xTaskCreate(lips, "lips", 2048, nullptr, 2, nullptr);
    xTaskCreate(network, "network", 6144, nullptr, 4, nullptr);
    xTaskCreatePinnedToCoreWithCaps(health, "health", 3072, nullptr, 1, nullptr, 0, MALLOC_CAP_SPIRAM);
    ESP_LOGI(TAG, "face running; free internal %u, PSRAM %u",
             (unsigned)heap_caps_get_free_size(MALLOC_CAP_INTERNAL),
             (unsigned)heap_caps_get_free_size(MALLOC_CAP_SPIRAM));
}
