// SPDX-License-Identifier: MIT
#include "net/wifi.h"

#include <algorithm>
#include <cstring>
#include <vector>

#include "esp_event.h"
#include "esp_log.h"
#include "esp_mac.h"
#include "esp_netif.h"
#include "esp_wifi.h"
#include "freertos/FreeRTOS.h"
#include "freertos/event_groups.h"
#include "nvs.h"
#include "soc/soc_caps.h"

namespace net {
namespace {

const char* TAG = "wifi";
constexpr EventBits_t kConnected = BIT0;
constexpr EventBits_t kFailed = BIT1;
constexpr int kAttemptMs = 12000;

EventGroupHandle_t events = nullptr;
bool started = false;
bool want_reconnect = false;

void on_event(void*, esp_event_base_t base, int32_t id, void*) {
    if (base == WIFI_EVENT && id == WIFI_EVENT_STA_DISCONNECTED) {
        xEventGroupClearBits(events, kConnected);
        if (want_reconnect) {
            esp_wifi_connect();
        } else {
            xEventGroupSetBits(events, kFailed);
        }
    } else if (base == IP_EVENT && id == IP_EVENT_STA_GOT_IP) {
        xEventGroupSetBits(events, kConnected);
    }
}

}  // namespace

std::vector<Network> saved_networks(const char* partition) {
    std::vector<Network> list;
    nvs_handle_t handle;
    if (nvs_open_from_partition(partition, "wifi", NVS_READONLY, &handle) != ESP_OK) {
        return list;
    }
    for (int i = 0; i < 10; i++) {
        std::string suffix = i ? std::to_string(i) : "";
        char ssid[33] = {}, password[65] = {};
        size_t ssid_len = sizeof(ssid), password_len = sizeof(password);
        if (nvs_get_str(handle, ("ssid" + suffix).c_str(), ssid, &ssid_len) == ESP_OK &&
            nvs_get_str(handle, ("password" + suffix).c_str(), password, &password_len) == ESP_OK) {
            list.push_back({ssid, password});
        }
    }
    nvs_close(handle);
    return list;
}

bool wifi_connect(const std::vector<Network>& networks, int timeout_ms) {
    if (!started) {
        events = xEventGroupCreate();
        esp_netif_init();
        esp_event_loop_create_default();
        esp_netif_create_default_wifi_sta();
        wifi_init_config_t init = WIFI_INIT_CONFIG_DEFAULT();
        esp_wifi_init(&init);
        esp_wifi_set_storage(WIFI_STORAGE_RAM);  // never write the robot's NVS
        esp_event_handler_register(WIFI_EVENT, ESP_EVENT_ANY_ID, on_event, nullptr);
        esp_event_handler_register(IP_EVENT, IP_EVENT_STA_GOT_IP, on_event, nullptr);
        esp_wifi_set_mode(WIFI_MODE_STA);
        esp_wifi_start();
        started = true;
    }
    if (networks.empty()) {
        ESP_LOGE(TAG, "no Wi-Fi network configured");
        return false;
    }
    TickType_t deadline = xTaskGetTickCount() + pdMS_TO_TICKS(timeout_ms);
    while (xTaskGetTickCount() < deadline) {
        for (const auto& network : networks) {
            wifi_config_t config = {};
            // A 32-character SSID fills the field with no terminator, as ESP-IDF allows.
            std::memcpy(config.sta.ssid, network.ssid.data(), std::min(network.ssid.size(), sizeof(config.sta.ssid)));
            std::memcpy(config.sta.password, network.password.data(),
                        std::min(network.password.size(), sizeof(config.sta.password)));
            config.sta.threshold.authmode = network.open ? WIFI_AUTH_OPEN : WIFI_AUTH_WPA2_PSK;
            ESP_LOGI(TAG, "connecting to \"%s\"", network.ssid.c_str());
            want_reconnect = false;
            xEventGroupClearBits(events, kConnected | kFailed);
            esp_wifi_set_config(WIFI_IF_STA, &config);
            esp_wifi_connect();
            EventBits_t bits = xEventGroupWaitBits(events, kConnected | kFailed, pdFALSE, pdFALSE, pdMS_TO_TICKS(kAttemptMs));
            if (bits & kConnected) {
                // Keep the link from now on, and keep latency low for voice.
                want_reconnect = true;
                esp_wifi_set_ps(WIFI_PS_NONE);
                ESP_LOGI(TAG, "connected to \"%s\"", network.ssid.c_str());
                return true;
            }
            esp_wifi_disconnect();
            if (xTaskGetTickCount() >= deadline) {
                break;
            }
        }
    }
    ESP_LOGE(TAG, "no saved network answered");
    return false;
}

bool wifi_connected() { return events != nullptr && (xEventGroupGetBits(events) & kConnected); }

std::string mac_address() {
    uint8_t mac[6] = {};
#if SOC_WIFI_SUPPORTED
    esp_read_mac(mac, ESP_MAC_WIFI_STA);
#else
    // A chip without its own Wi-Fi (the P4, whose radio is the C6): the
    // chip's base MAC from eFuse, readable before Wi-Fi is up.
    esp_read_mac(mac, ESP_MAC_BASE);
#endif
    char text[18];
    snprintf(text, sizeof(text), "%02x:%02x:%02x:%02x:%02x:%02x", mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);
    return text;
}

}  // namespace net
