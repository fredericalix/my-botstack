// SPDX-License-Identifier: MIT
#include "net/link.h"

#include <algorithm>
#include <cctype>
#include <cmath>
#include <cstring>
#include <vector>

#include "audio/opus.h"
#include "cJSON.h"
#include "esp_app_desc.h"
#include "esp_crt_bundle.h"
#include "esp_heap_caps.h"
#include "esp_https_ota.h"
#include "esp_log.h"
#include "esp_ota_ops.h"
#include "esp_system.h"
#include "esp_timer.h"
#include "esp_wifi.h"
#include "net/vision.h"
#include "net/wifi.h"
#include "nvs.h"

namespace net {
namespace {

const char* TAG = "link";
constexpr int64_t kHelloTimeoutUs = 6'000'000;
constexpr int kBackoffMinMs = 1000;
constexpr int kBackoffMaxMs = 8000;  // 1, 2, 4, 8 s; the watchdog restarts before a 16 s wait
// A WebSocket ping every 5 s, so even an idle link hears the bridge (its pong)
// that often. Nothing from the bridge for 20 s, connected or not: restart.
constexpr int kPingSec = 5;
constexpr int kPongTimeoutSec = 10;
constexpr int64_t kUnreachableUs = 20'000'000;
constexpr int kVoiceQueue = 48;  // packets of 60 ms: more than the bridge sends ahead
constexpr uint32_t kControlStack = 12288;
constexpr uint32_t kMicStack = 40960;  // measured: 25.7 kB (Opus encode + TLS write)
constexpr uint32_t kVoiceStack = 32768;
constexpr uint32_t kCameraStack = 32768;  // JPEG encode, then TLS
// In internal RAM, unlike the others: it writes the flash, which a task with
// its stack in PSRAM cannot do. It only exists while an update is installed.
constexpr uint32_t kUpdateStack = 8192;
constexpr int kUpdateIdleMs = 1000;  // look this often for a pause between conversations
constexpr uint32_t kKeepStack = 4096;  // internal RAM too: it writes otadata
constexpr uint32_t kKeyStack = 4096;  // internal RAM too: it writes NVS
constexpr int64_t kStatusUs = 60'000'000;
constexpr char kKeyNamespace[] = "botstack";
constexpr char kKeyName[] = "key";

void log_stack_use(const char* name, uint32_t size, int& reported, int packets) {
    if (reported < 0 && packets >= 20) {
        reported = (int)(size - uxTaskGetStackHighWaterMark(nullptr));
        ESP_LOGI(TAG, "%s stack: %d of %u bytes used", name, reported, (unsigned)size);
    }
}

struct Packet {
    uint16_t size;
    uint8_t data[];
};

int64_t now_us() { return esp_timer_get_time(); }

// firmware/robot-token, embedded at build time (components/botstack/CMakeLists.txt).
extern const char robot_token[] asm("_binary_robot_token_start");

/// The bootstrap key (firmware/robot-token), without the file's trailing newline.
std::string bootstrap_key() {
    std::string key(robot_token);
    while (!key.empty() && std::isspace((unsigned char)key.back())) {
        key.pop_back();
    }
    return key;
}

}  // namespace

// firmware/bridge-url, embedded at build time like the robot token.
extern const char bridge_url_file[] asm("_binary_bridge_url_start");

std::string bridge_url() {
    std::string url(bridge_url_file);
    while (!url.empty() && std::isspace((unsigned char)url.back())) {
        url.pop_back();
    }
    return url;
}

namespace {

// The key the link and the update download send; set by Link::use_key.
std::mutex current_key_lock;
std::string current_key;

/// This robot's own key from NVS (`enroll`), or empty. Reads the flash: not
/// from a task whose stack is in PSRAM.
std::string stored_key() {
    nvs_handle_t nvs;
    if (nvs_open(kKeyNamespace, NVS_READONLY, &nvs) != ESP_OK) {
        return "";
    }
    char key[65] = {};
    size_t size = sizeof(key);
    const esp_err_t err = nvs_get_str(nvs, kKeyName, key, &size);
    nvs_close(nvs);
    return err == ESP_OK ? std::string(key) : std::string();
}

/// Writes this robot's own key, or erases it when empty. Writes the flash:
/// only from a task whose stack is in internal RAM.
bool store_key(const std::string& key) {
    nvs_handle_t nvs;
    if (nvs_open(kKeyNamespace, NVS_READWRITE, &nvs) != ESP_OK) {
        return false;
    }
    esp_err_t err = key.empty() ? nvs_erase_key(nvs, kKeyName) : nvs_set_str(nvs, kKeyName, key.c_str());
    if (err == ESP_ERR_NVS_NOT_FOUND) {
        err = ESP_OK;  // nothing to erase
    }
    if (err == ESP_OK) {
        err = nvs_commit(nvs);
    }
    nvs_close(nvs);
    return err == ESP_OK;
}

bool is_hex(const std::string& text, size_t length) {
    return text.size() == length && std::all_of(text.begin(), text.end(), [](char c) { return std::isxdigit((unsigned char)c); });
}

const char* reset_reason_name(esp_reset_reason_t reason) {
    switch (reason) {
        case ESP_RST_POWERON: return "poweron";
        case ESP_RST_EXT: return "external";
        case ESP_RST_SW: return "software";
        case ESP_RST_PANIC: return "panic";
        case ESP_RST_INT_WDT: return "int_wdt";
        case ESP_RST_TASK_WDT: return "task_wdt";
        case ESP_RST_WDT: return "wdt";
        case ESP_RST_DEEPSLEEP: return "deepsleep";
        case ESP_RST_BROWNOUT: return "brownout";
        case ESP_RST_SDIO: return "sdio";
        default: return "unknown";
    }
}

/// A firmware's build: the first 8 bytes of its ELF SHA-256 in hex. The
/// bridge reads the same bytes in a published image.
std::string build_of(const esp_app_desc_t& desc) {
    char hex[17];
    for (int i = 0; i < 8; ++i) {
        std::snprintf(hex + 2 * i, 3, "%02x", desc.app_elf_sha256[i]);
    }
    return hex;
}

std::string own_build() { return build_of(*esp_app_get_description()); }

/// Marks the running firmware valid, then ends.
void keep_firmware(void*) {
    esp_ota_mark_app_valid_cancel_rollback();
    vTaskDelete(nullptr);
}

/// The update download carries the robot's MAC and key, like the link.
esp_err_t authorize_download(esp_http_client_handle_t client) {
    std::string key;
    {
        std::lock_guard<std::mutex> lock(current_key_lock);
        key = current_key;
    }
    esp_http_client_set_header(client, "Device-Id", mac_address().c_str());
    esp_http_client_set_header(client, "Authorization", ("Bearer " + key).c_str());
    return ESP_OK;
}

/// A camera shutter: two short clicks of decaying noise.
void play_shutter(audio::Audio& sound) {
    constexpr int kRate = audio::kSpeakerRate;
    std::vector<int16_t> pcm(kRate * 110 / 1000, 0);
    uint32_t seed = 0x2545F491;
    auto click = [&](int start_ms, int length_ms, float peak) {
        const size_t start = (size_t)(kRate * start_ms / 1000);
        const int length = kRate * length_ms / 1000;
        for (int i = 0; i < length && start + i < pcm.size(); ++i) {
            seed = seed * 1664525u + 1013904223u;
            const float noise = (float)(int32_t)seed / 2147483648.0f;
            pcm[start + i] = (int16_t)(noise * std::exp(-6.0f * i / length) * peak * 32767.0f);
        }
    };
    click(0, 20, 0.3f);
    click(65, 40, 0.2f);
    sound.play(pcm.data(), pcm.size());
}

const char* string_of(const cJSON* message, const char* key) {
    const cJSON* item = cJSON_GetObjectItem(message, key);
    return cJSON_IsString(item) ? item->valuestring : "";
}

double number_of(const cJSON* message, const char* key, double fallback) {
    const cJSON* item = cJSON_GetObjectItem(message, key);
    return cJSON_IsNumber(item) ? item->valuedouble : fallback;
}

}  // namespace

void Link::start() {
    // transport_ws prints the whole upgrade request, key included, when its
    // write fails (an error-level log): silence it.
    esp_log_level_set("transport_ws", ESP_LOG_NONE);
    if (url_.rfind("wss://", 0) != 0) {
        // Never send the key in clear text, or to a mistyped file.
        ESP_LOGE(TAG, "firmware/bridge-url must start with wss://; not connecting");
        face_.set_expression(bloub::find_expression("triste"));
        return;
    }
    if (bootstrap_key().empty()) {
        ESP_LOGE(TAG, "no bootstrap key: an unenrolled robot will be refused");
    }
    // Here, in the network task whose stack is in internal RAM: it reads the flash.
    const std::string own = stored_key();
    own_key_ = is_hex(own, 64);
    use_key(own_key_ ? own : bootstrap_key());
    ESP_LOGI(TAG, "connecting with %s", own_key_ ? "this robot's own key" : "the bootstrap key");
    events_ = xQueueCreate(32, sizeof(Event));
    voice_ = xQueueCreate(kVoiceQueue, sizeof(Packet*));
    photos_ = xQueueCreate(1, sizeof(PhotoJob));
    heard_us_.store(now_us());
    // Stacks in PSRAM: the WebSocket client needs the internal RAM for its task.
    // Sending goes through TLS in the caller's task, and the Opus encoder keeps
    // large arrays on the stack: the microphone task overflowed 8 kB and 24 kB.
    // The stack sizes are measured and logged after the first packets.
    xTaskCreatePinnedToCoreWithCaps(control_task, "link", kControlStack, this, 5, nullptr, 0, MALLOC_CAP_SPIRAM);
    // Core 1 runs the audio front end; encoding there too starved it.
    xTaskCreatePinnedToCoreWithCaps(mic_task, "mic_send", kMicStack, this, 6, nullptr, 0, MALLOC_CAP_SPIRAM);
    xTaskCreatePinnedToCoreWithCaps(voice_task, "voice", kVoiceStack, this, 7, nullptr, 0, MALLOC_CAP_SPIRAM);
    if (camera_ != nullptr) {
        xTaskCreatePinnedToCoreWithCaps(camera_task, "camera", kCameraStack, this, 3, nullptr, 0, MALLOC_CAP_SPIRAM);
    }
}

/// The key for the next connection and download. Never log headers_.
void Link::use_key(const std::string& key) {
    headers_ = "Device-Id: " + mac_address() + "\r\nAuthorization: Bearer " + key + "\r\n";
    std::lock_guard<std::mutex> lock(current_key_lock);
    current_key = key;
}

/// Starts key_task unless it runs already: then it writes the latest key too.
void Link::start_key_task() {
    if (key_task_running_.exchange(true)) {
        return;
    }
    if (xTaskCreate(key_task, "key", kKeyStack, this, 2, nullptr) != pdPASS) {
        ESP_LOGE(TAG, "no memory for the key task");
        key_task_running_.store(false);
    }
}

/// Writes (or erases) the robot's own key, then tells the control task. Its
/// stack is in internal RAM: a task with a PSRAM stack cannot write the flash.
/// One at a time: a key that came while it wrote is written next, so NVS and
/// saved_key_ always end on the last key the bridge sent.
void Link::key_task(void* arg) {
    auto* self = static_cast<Link*>(arg);
    std::string key, id;
    while (true) {
        {
            std::lock_guard<std::mutex> lock(self->key_lock_);
            key = self->key_to_save_;
            id = self->enroll_id_;
        }
        const bool ok = store_key(key);
        std::lock_guard<std::mutex> lock(self->key_lock_);
        if (key == self->key_to_save_ && id == self->enroll_id_) {
            self->saved_key_ = key;
            self->saved_id_ = id;
            self->saved_ok_ = ok;
            // Under the lock: a key set after this starts a new task.
            self->key_task_running_.store(false);
            break;
        }
    }
    ESP_LOGI(TAG, "key task: %u bytes of stack left", (unsigned)uxTaskGetStackHighWaterMark(nullptr));
    self->post({Kind::KeySaved, State::Offline, 0});
    vTaskDelete(nullptr);
}

void Link::tap() {
    if (updating_.load()) {
        return;  // a few seconds; the robot restarts right after
    }
    // The control task can lag by most of a second while the robot speaks
    // (core 0 is nearly full): silence the speaker here, in the caller's task.
    const bool interrupting = state_.load() != State::Offline && (voice_open_.load() || sound_.playing());
    if (interrupting) {
        stop_voice();
    }
    post({Kind::Tap, State::Offline, 0, false, interrupting});
}

void Link::post(const Event& event) {
    // A tap can come before Wi-Fi is up and the link has started.
    if (events_ != nullptr) {
        xQueueSend(events_, &event, 0);
    }
}

void Link::control_task(void* arg) {
    auto* self = static_cast<Link*>(arg);
    self->connect();
    Event event;
    while (true) {
        // Poll faster while waiting for the speaker to drain.
        TickType_t wait = pdMS_TO_TICKS(self->played_pending_ ? 20 : 200);
        if (xQueueReceive(self->events_, &event, wait) == pdTRUE) {
            self->handle(event);
        }
        int64_t now = now_us();
        if (self->played_pending_ && self->drained()) {
            self->played_pending_ = false;
            self->send_text(R"({"type":"played"})");
        }
        if (self->hello_deadline_ != 0 && now > self->hello_deadline_) {
            ESP_LOGW(TAG, "no hello from the bridge");
            self->disconnect();
        }
        if (self->status_at_ != 0 && now > self->status_at_) {
            self->status_at_ = now + kStatusUs;
            self->send_status();
        }
        // `reboot`: like an update, only at a pause between conversations.
        // Never while an update writes the flash: it restarts by itself.
        if (self->reboot_pending_ && !self->updating_.load() && self->online_.load() &&
            self->state_.load() == State::Idle && !self->voice_open_.load() && !self->sound_.playing()) {
            ESP_LOGW(TAG, "restarting, as the bridge asked");
            vTaskDelay(pdMS_TO_TICKS(300));
            esp_restart();
        }
        if (self->reconnect_at_ != 0 && now > self->reconnect_at_) {
            self->connect();
        }
        if (now - self->heard_us_.load() > kUnreachableUs) {
            ESP_LOGE(TAG, "bridge unreachable for %lld s; restarting", (long long)(kUnreachableUs / 1000000));
            esp_restart();
        }
    }
}

void Link::handle(const Event& event) {
    switch (event.kind) {
        case Kind::Tap: {
            State state = state_.load();
            // Whatever the bridge last said, a tap while the speaker played
            // interrupts (tap() already silenced it).
            if (event.ok) {
                state = State::Speaking;
            }
            switch (state) {
                case State::Offline:
                    ESP_LOGW(TAG, "tap: no link to the bridge");
                    face_.play(bloub::Animation::Alert);
                    break;
                case State::Idle:
                    ESP_LOGI(TAG, "tap: start");
                    send_text(R"({"type":"talk","action":"start"})");
                    break;
                case State::Listening:
                case State::Thinking:
                    ESP_LOGI(TAG, "tap: end of conversation");
                    send_text(R"({"type":"talk","action":"stop"})");
                    break;
                case State::Speaking:
                    ESP_LOGI(TAG, "tap: interrupt");
                    // Silence now; the bridge answers with `speech end`.
                    stop_voice();
                    send_text(R"({"type":"interrupt"})");
                    break;
            }
            break;
        }
        case Kind::Connected:
            ESP_LOGI(TAG, "connected; hello");
            // The project name is the hardware type: the bridge offers updates
            // built for it (botstack-fw on the StackChan, botstack-p4 on the P4).
            send_text(R"({"type":"hello","protocol":"botstack/v1","firmware":")" +
                      std::string(esp_app_get_description()->project_name) + R"(","build":")" + own_build() +
                      R"(","mic":{"codec":"opus","rate":16000,"frame_ms":60},)"
                      R"("speaker":{"codec":"opus","rate":24000,"frame_ms":60},"features":)" + features_ + "}");
            break;
        case Kind::Hello:
            ESP_LOGI(TAG, "bridge ready");
            // Keep this firmware: after an update, the bootloader goes back to
            // the version before on the next reset until the new one gets here.
            // It writes otadata, so not from this task, whose stack is in PSRAM.
            if (!firmware_kept_) {
                firmware_kept_ = true;
                xTaskCreate(keep_firmware, "keep", kKeepStack, nullptr, 2, nullptr);
            }
            hello_deadline_ = 0;
            backoff_ms_ = kBackoffMinMs;
            online_.store(true);
            if (state_.load() == State::Offline) {
                set_state(State::Idle, false);
            }
            send_status();
            status_at_ = now_us() + kStatusUs;
            break;
        case Kind::State:
            if (online_.load()) {
                set_state(event.state, event.mic);
            }
            break;
        case Kind::SpeechStart:
            // A new speech before `played` replaces the one that ended.
            played_pending_ = false;
            break;
        case Kind::SpeechEnd:
            played_pending_ = true;
            break;
        case Kind::SpeechCancel:
            // Barge-in: the user talked over the robot. Silence now, then `played`.
            stop_voice();
            played_pending_ = true;
            break;
        case Kind::Tool:
            send_tool_result(event.id, false, "Not available on this firmware.");
            break;
        case Kind::Head:
            send_tool_result(event.id, event.ok, event.ok ? "Done." : "The head does not answer.");
            break;
        case Kind::Enroll:
            start_key_task();
            break;
        case Kind::Unauthorized:
            // The bootstrap key itself refused: nothing to fall back to.
            if (!own_key_) {
                break;
            }
            ESP_LOGW(TAG, "the bridge refused this robot's own key; back to the bootstrap key");
            own_key_ = false;
            use_key(bootstrap_key());
            {
                std::lock_guard<std::mutex> lock(key_lock_);
                key_to_save_.clear();
                enroll_id_.clear();
            }
            start_key_task();
            break;
        case Kind::KeySaved: {
            std::string key, id;
            bool ok;
            {
                std::lock_guard<std::mutex> lock(key_lock_);
                key = saved_key_;
                id = saved_id_;
                ok = saved_ok_;
            }
            if (!ok) {
                ESP_LOGE(TAG, "the robot's key could not be written");
            } else if (!key.empty()) {
                own_key_ = true;
                use_key(key);  // from the next connection
                send_text(R"({"type":"enrolled","id":")" + id + R"("})");
                ESP_LOGI(TAG, "own key saved (id %s)", id.c_str());
            }
            break;
        }
        case Kind::Reboot:
            ESP_LOGW(TAG, "the bridge asks for a restart");
            reboot_pending_ = true;
            break;
        case Kind::Disconnected:
            if (reconnect_at_ == 0) {
                ESP_LOGW(TAG, "link lost");
                disconnect();
            }
            break;
    }
}

void Link::send_tool_result(uint32_t id, bool ok, const char* result) {
    cJSON* message = cJSON_CreateObject();
    cJSON_AddStringToObject(message, "type", "tool_result");
    cJSON_AddNumberToObject(message, "id", id);
    cJSON_AddBoolToObject(message, "ok", ok);
    cJSON_AddStringToObject(message, "result", result);
    char* text = cJSON_PrintUnformatted(message);
    send_text(text);
    cJSON_free(text);
    cJSON_Delete(message);
}

/// The robot's health for the bridge (fleet_device).
void Link::send_status() {
    wifi_ap_record_t ap = {};
    const int rssi = esp_wifi_sta_get_ap_info(&ap) == ESP_OK ? ap.rssi : 0;
    cJSON* message = cJSON_CreateObject();
    cJSON_AddStringToObject(message, "type", "status");
    cJSON_AddNumberToObject(message, "rssi", rssi);
    cJSON_AddNumberToObject(message, "heap_internal", (double)heap_caps_get_free_size(MALLOC_CAP_INTERNAL));
    cJSON_AddNumberToObject(message, "heap_psram", (double)heap_caps_get_free_size(MALLOC_CAP_SPIRAM));
    cJSON_AddNumberToObject(message, "uptime_s", (double)(now_us() / 1000000));
    cJSON_AddStringToObject(message, "reset_reason", reset_reason_name(esp_reset_reason()));
    cJSON_AddStringToObject(message, "head", head_ ? (head_->ready() ? "ok" : "missing") : "none");
    cJSON_AddStringToObject(message, "camera", camera_ ? (camera_ok_.load() ? "ok" : "missing") : "none");
    char* text = cJSON_PrintUnformatted(message);
    send_text(text);
    cJSON_free(text);
    cJSON_Delete(message);
}

void Link::set_state(State state, bool mic) {
    state_.store(state);
    mic_on_.store(mic);
    sound_.set_mic_enabled(mic);
    face_.set_microphone(mic);
    switch (state) {
        case State::Offline:
        case State::Idle:
            face_.set_state(bloub::State::Idle);
            break;
        case State::Listening:
            face_.set_state(bloub::State::Listening);
            break;
        case State::Thinking:
            face_.set_state(bloub::State::Thinking);
            break;
        case State::Speaking:
            face_.set_state(bloub::State::Speaking);
            break;
    }
}

void Link::connect() {
    reconnect_at_ = 0;
    if (!wifi_connected()) {
        // Wi-Fi reconnects on its own; try again later.
        reconnect_at_ = now_us() + (int64_t)backoff_ms_ * 1000;
        return;
    }
    esp_websocket_client_config_t config = {};
    config.uri = url_.c_str();
    config.headers = headers_.c_str();
    config.buffer_size = 4096;
    config.crt_bundle_attach = esp_crt_bundle_attach;
    config.disable_auto_reconnect = true;  // our own backoff
    config.task_stack = 8192;
    config.task_prio = 6;
    config.network_timeout_ms = 10000;
    config.ping_interval_sec = kPingSec;
    config.pingpong_timeout_sec = kPongTimeoutSec;
    esp_websocket_client_handle_t client = esp_websocket_client_init(&config);
    if (client == nullptr) {
        reconnect_at_ = now_us() + (int64_t)backoff_ms_ * 1000;
        return;
    }
    esp_websocket_register_events(client, WEBSOCKET_EVENT_ANY, on_ws, this);
    {
        std::lock_guard<std::mutex> lock(client_lock_);
        client_ = client;
    }
    text_buffer_.clear();
    frame_opcode_ = 0;
    hello_deadline_ = now_us() + kHelloTimeoutUs;
    ESP_LOGI(TAG, "connecting to %s", url_.c_str());
    if (esp_websocket_client_start(client) != ESP_OK) {
        disconnect();
    }
}

/// Close the socket and schedule the next attempt.
void Link::disconnect() {
    online_.store(false);
    set_state(State::Offline, false);
    stop_voice();
    played_pending_ = false;
    hello_deadline_ = 0;
    status_at_ = 0;
    esp_websocket_client_handle_t client = nullptr;
    {
        std::lock_guard<std::mutex> lock(client_lock_);
        client = client_;
        client_ = nullptr;
    }
    if (client != nullptr) {
        esp_websocket_client_close(client, pdMS_TO_TICKS(1000));
        esp_websocket_client_destroy(client);
    }
    ESP_LOGI(TAG, "reconnecting in %d ms", backoff_ms_);
    reconnect_at_ = now_us() + (int64_t)backoff_ms_ * 1000;
    backoff_ms_ = std::min(backoff_ms_ * 2, kBackoffMaxMs);
}

void Link::stop_voice() {
    voice_open_.store(false);
    Packet* packet;
    while (xQueueReceive(voice_, &packet, 0) == pdTRUE) {
        free(packet);
    }
    sound_.stop_playback();
}

bool Link::drained() const {
    return uxQueueMessagesWaiting(voice_) == 0 && !decoding_.load() && sound_.queued_ms() == 0;
}

void Link::send_text(const std::string& text) {
    std::lock_guard<std::mutex> lock(client_lock_);
    if (client_ != nullptr && esp_websocket_client_is_connected(client_)) {
        esp_websocket_client_send_text(client_, text.c_str(), text.size(), pdMS_TO_TICKS(1000));
    }
}

void Link::on_ws(void* arg, esp_event_base_t, int32_t id, void* event_data) {
    auto* self = static_cast<Link*>(arg);
    auto* data = static_cast<esp_websocket_event_data_t*>(event_data);
    if (data->client != self->client_.load()) {
        return;  // a client already closed
    }
    switch (id) {
        case WEBSOCKET_EVENT_CONNECTED:
            self->post({Kind::Connected, State::Offline, 0});
            break;
        case WEBSOCKET_EVENT_DISCONNECTED:
        case WEBSOCKET_EVENT_CLOSED:
            self->post({Kind::Disconnected, State::Offline, 0});
            break;
        case WEBSOCKET_EVENT_DATA: {
            self->heard_us_.store(now_us());  // any frame, pongs included
            // A frame may arrive in pieces; continuation frames keep the first opcode.
            if (data->op_code == 0x1 || data->op_code == 0x2) {
                self->frame_opcode_ = data->op_code;
            } else if (data->op_code != 0x0) {
                break;  // ping, pong, close
            }
            if (self->frame_opcode_ == 0x2) {
                // Voice: one Opus packet per message, small enough to come whole.
                if (data->payload_offset != 0 || data->data_len != data->payload_len || data->data_len <= 0) {
                    break;
                }
                if (!self->voice_open_.load()) {
                    break;  // after a tap, the rest of the speech is dropped
                }
                auto* packet = static_cast<Packet*>(malloc(sizeof(Packet) + data->data_len));
                if (packet == nullptr) {
                    break;
                }
                packet->size = data->data_len;
                std::memcpy(packet->data, data->data_ptr, data->data_len);
                if (xQueueSend(self->voice_, &packet, 0) != pdTRUE) {
                    free(packet);
                }
            } else if (self->frame_opcode_ == 0x1) {
                if (data->payload_offset == 0) {
                    self->text_buffer_.clear();
                }
                self->text_buffer_.append(data->data_ptr, data->data_len);
                if (data->payload_offset + data->data_len >= data->payload_len) {
                    self->on_text(self->text_buffer_);
                }
            }
            break;
        }
        case WEBSOCKET_EVENT_ERROR:
            // A refused key shows as the upgrade's HTTP status.
            if (data->error_handle.esp_ws_handshake_status_code == 401) {
                self->post({Kind::Unauthorized, State::Offline, 0});
            }
            break;
        default:
            break;
    }
}

/// Runs in the WebSocket task. The face is thread-safe; everything that sends
/// or changes the link state goes through the control task.
void Link::on_text(const std::string& text) {
    cJSON* message = cJSON_Parse(text.c_str());
    if (message == nullptr) {
        return;
    }
    const char* kind = string_of(message, "type");
    if (std::strcmp(kind, "hello") == 0) {
        if (std::strcmp(string_of(message, "protocol"), "botstack/v1") == 0) {
            const cJSON* vision = cJSON_GetObjectItem(message, "vision");
            {
                std::lock_guard<std::mutex> lock(vision_lock_);
                vision_url_ = string_of(vision, "url");
                vision_token_ = string_of(vision, "token");
            }
            // Here, not in the control task: an `expression` right after the
            // hello (the wary face of a locked robot) must not be overwritten.
            face_.set_expression(bloub::find_expression("neutre"));
            post({Kind::Hello, State::Offline, 0});
        }
    } else if (std::strcmp(kind, "firmware") == 0) {
        offer_update(string_of(message, "build"), string_of(message, "url"));
    } else if (std::strcmp(kind, "state") == 0) {
        const char* value = string_of(message, "value");
        static const struct {
            const char* name;
            State state;
        } states[] = {{"idle", State::Idle},
                      {"listening", State::Listening},
                      {"thinking", State::Thinking},
                      {"speaking", State::Speaking}};
        // Without `mic`, the microphone streams while listening and thinking.
        const cJSON* mic = cJSON_GetObjectItem(message, "mic");
        for (const auto& s : states) {
            if (std::strcmp(s.name, value) == 0) {
                bool on = cJSON_IsBool(mic) ? cJSON_IsTrue(mic) : (s.state == State::Listening || s.state == State::Thinking);
                post({Kind::State, s.state, 0, on});
            }
        }
    } else if (std::strcmp(kind, "speech") == 0) {
        const char* action = string_of(message, "action");
        if (std::strcmp(action, "start") == 0) {
            // Set here, in the task that also receives the packets that follow.
            voice_open_.store(true);
            post({Kind::SpeechStart, State::Offline, 0});
        } else if (std::strcmp(action, "end") == 0) {
            post({Kind::SpeechEnd, State::Offline, 0});
        } else if (std::strcmp(action, "cancel") == 0) {
            voice_open_.store(false);  // drop packets already on their way
            post({Kind::SpeechCancel, State::Offline, 0});
        }
    } else if (std::strcmp(kind, "expression") == 0) {
        int index = bloub::find_expression(string_of(message, "value"));
        if (index >= 0) {
            face_.set_expression(index);
        }
    } else if (std::strcmp(kind, "animation") == 0) {
        bloub::Animation animation = bloub::find_animation(string_of(message, "value"));
        if (animation != bloub::Animation::None) {
            face_.play(animation);
        }
    } else if (std::strcmp(kind, "tool") == 0) {
        const cJSON* id = cJSON_GetObjectItem(message, "id");
        const char* name = string_of(message, "name");
        ESP_LOGI(TAG, "tool %s", name);
        if (cJSON_IsNumber(id) && std::strcmp(name, "camera.take_photo") == 0 && camera_ != nullptr) {
            PhotoJob job = {};
            job.id = (uint32_t)id->valuedouble;
            std::strncpy(job.question, string_of(cJSON_GetObjectItem(message, "args"), "question"), sizeof(job.question) - 1);
            if (xQueueSend(photos_, &job, 0) != pdTRUE) {
                // A photo is already being taken; the bridge times this one out.
                ESP_LOGW(TAG, "camera busy");
            }
        } else if (cJSON_IsNumber(id) && std::strcmp(name, "head.set_angles") == 0 && head_ != nullptr) {
            const cJSON* args = cJSON_GetObjectItem(message, "args");
            bool ok = head_->set_angles((float)number_of(args, "yaw", 0), (float)number_of(args, "pitch", 0),
                                        (int)number_of(args, "speed", 300));
            post({Kind::Head, State::Offline, (uint32_t)id->valuedouble, false, ok});
        } else if (cJSON_IsNumber(id)) {
            post({Kind::Tool, State::Offline, (uint32_t)id->valuedouble});
        }
    } else if (std::strcmp(kind, "enroll") == 0) {
        // The key never reaches the log.
        const std::string key = string_of(message, "key");
        const std::string id = string_of(message, "id");
        if (is_hex(key, 64) && is_hex(id, 8)) {
            {
                std::lock_guard<std::mutex> lock(key_lock_);
                key_to_save_ = key;
                enroll_id_ = id;
            }
            post({Kind::Enroll, State::Offline, 0});
        } else {
            ESP_LOGW(TAG, "enroll: malformed key");
        }
    } else if (std::strcmp(kind, "reboot") == 0) {
        post({Kind::Reboot, State::Offline, 0});
    }
    cJSON_Delete(message);
}

void Link::mic_task(void* arg) {
    auto* self = static_cast<Link*>(arg);
    audio::OpusEncoder encoder;
    if (!encoder.open(audio::kMicRate, 24000)) {
        // A PSRAM-stack task cannot delete itself; stay parked.
        vTaskSuspend(nullptr);
    }
    std::vector<int16_t> frame(encoder.frame_samples());
    std::vector<uint8_t> packet;
    size_t filled = 0;
    int used = -1, sent = 0;
    float mic_sq = 0;
    size_t mic_n = 0;
    while (true) {
        if (!self->mic_on_.load()) {
            filled = 0;
            vTaskDelay(pdMS_TO_TICKS(20));
            continue;
        }
        filled += self->sound_.read_mic(frame.data() + filled, frame.size() - filled, pdMS_TO_TICKS(100));
        if (filled < frame.size()) {
            continue;
        }
        filled = 0;
        // Levels every ~2 s: what goes out, the raw microphone, the speaker
        // reference, and the echo canceller's output. While the robot speaks,
        // microphone minus AFE out is the echo removed.
        for (int16_t sample : frame) {
            mic_sq += (float)sample * sample;
        }
        mic_n += frame.size();
        if (mic_n >= audio::kMicRate * 2) {
            float rms = std::sqrt(mic_sq / mic_n) / 32768.0f;
            float raw = 0, clean = 0, ref = 0;
            int peak = 0, clipped = 0;
            self->sound_.take_levels(raw, clean);
            self->sound_.take_reference(ref, peak, clipped);
            auto db = [](float x) { return 20 * std::log10(std::max(x, 1e-6f)); };
            ESP_LOGI(TAG, "uplink%s: sent %.1f | mic %.1f, loopback %.1f (peak %d, clipped %d), AFE out %.1f dBFS, echo %d ms",
                     self->sound_.playing() ? " (playing)" : "", db(rms), db(raw), db(ref), peak, clipped, db(clean),
                     self->sound_.echo_delay_ms());
            mic_sq = 0;
            mic_n = 0;
        }
        if (!encoder.encode(frame.data(), packet)) {
            continue;
        }
        {
            std::lock_guard<std::mutex> lock(self->client_lock_);
            if (self->client_ != nullptr && esp_websocket_client_is_connected(self->client_)) {
                esp_websocket_client_send_bin(self->client_, (const char*)packet.data(), packet.size(), pdMS_TO_TICKS(500));
            }
        }
        log_stack_use("mic_send", kMicStack, used, ++sent);
    }
}

void Link::camera_task(void* arg) {
    auto* self = static_cast<Link*>(arg);
    bool camera = self->camera_->start();
    self->camera_ok_.store(camera);
    PhotoJob job;
    while (true) {
        if (xQueueReceive(self->photos_, &job, portMAX_DELAY) != pdTRUE) {
            continue;
        }
        if (!camera) {
            self->send_tool_result(job.id, false, "The camera did not start.");
            continue;
        }
        self->face_.play(bloub::Animation::Wide);  // looking
        play_shutter(self->sound_);                 // and heard, every photo
        uint8_t* jpeg = nullptr;
        size_t size = 0;
        heap_caps_monitor_local_minimum_free_size_start();
        bool taken = self->camera_->take(&jpeg, &size);
        const size_t capture_low = heap_caps_get_minimum_free_size(MALLOC_CAP_INTERNAL);
        heap_caps_monitor_local_minimum_free_size_stop();
        if (!taken) {
            self->send_tool_result(job.id, false, "The camera gave no picture.");
            continue;
        }
        std::string url, token, answer;
        {
            std::lock_guard<std::mutex> lock(self->vision_lock_);
            url = self->vision_url_;
            token = self->vision_token_;
        }
        heap_caps_monitor_local_minimum_free_size_start();
        bool ok = ask_vision(url, token, mac_address(), job.question, jpeg, size, answer);
        const size_t upload_low = heap_caps_get_minimum_free_size(MALLOC_CAP_INTERNAL);
        heap_caps_monitor_local_minimum_free_size_stop();
        free(jpeg);
        // The lowest free internal RAM while capturing and while uploading:
        // the robot's TLS and Wi-Fi need some left at every moment.
        ESP_LOGI(TAG, "vision %s (%u bytes of answer); internal low %u capturing, %u uploading",
                 ok ? "answered" : "failed", (unsigned)answer.size(), (unsigned)capture_low, (unsigned)upload_low);
        self->send_tool_result(job.id, ok, answer.c_str());
    }
}

/// The bridge has published a firmware. Runs in the WebSocket task.
void Link::offer_update(const std::string& build, const std::string& url) {
    if (build.size() != 16 || build == own_build() || url.rfind("https://", 0) != 0) {
        return;
    }
    // A build that did not reach the bridge was rolled back: not again.
    if (const esp_partition_t* invalid = esp_ota_get_last_invalid_partition()) {
        esp_app_desc_t desc;
        if (esp_ota_get_partition_description(invalid, &desc) == ESP_OK && build_of(desc) == build) {
            ESP_LOGW(TAG, "firmware %s was rolled back; not installing it again", build.c_str());
            return;
        }
    }
    {
        std::lock_guard<std::mutex> lock(update_lock_);
        if (build == update_failed_) {
            return;
        }
        update_build_ = build;
        update_url_ = url;
    }
    if (!update_running_.exchange(true) &&
        xTaskCreate(update_task, "update", kUpdateStack, this, 2, nullptr) != pdPASS) {
        ESP_LOGE(TAG, "no memory for the update task");
        update_running_.store(false);
    }
}

/// Waits for a pause between conversations, installs the offered firmware and
/// restarts into it. On failure the build is not tried again until the next boot.
void Link::update_task(void* arg) {
    auto* self = static_cast<Link*>(arg);
    // Flash writes stall the face and the speaker: only while nothing happens.
    while (!(self->online_.load() && self->state_.load() == State::Idle && !self->voice_open_.load() &&
             !self->sound_.playing())) {
        vTaskDelay(pdMS_TO_TICKS(kUpdateIdleMs));
    }
    std::string build, url;
    {
        std::lock_guard<std::mutex> lock(self->update_lock_);
        build = self->update_build_;
        url = self->update_url_;
    }
    self->updating_.store(true);
    self->face_.set_expression(bloub::find_expression("somnolent"));
    if (install(build, url)) {
        ESP_LOGW(TAG, "firmware %s installed; restarting", build.c_str());
        vTaskDelay(pdMS_TO_TICKS(300));
        esp_restart();
    }
    {
        std::lock_guard<std::mutex> lock(self->update_lock_);
        self->update_failed_ = build;
    }
    self->face_.set_expression(bloub::find_expression("neutre"));
    self->updating_.store(false);
    self->update_running_.store(false);
    vTaskDelete(nullptr);
}

/// Downloads `build` into the other app partition. esp_https_ota_finish checks
/// the signature against the key of the running firmware, then selects it for
/// the next boot.
bool Link::install(const std::string& build, const std::string& url) {
    ESP_LOGW(TAG, "installing firmware %s", build.c_str());
    esp_http_client_config_t http = {};
    http.url = url.c_str();
    http.crt_bundle_attach = esp_crt_bundle_attach;
    http.timeout_ms = 30000;
    http.buffer_size = 4096;
    http.buffer_size_tx = 2048;  // the request carries the robot's key
    esp_https_ota_config_t config = {};
    config.http_config = &http;
    config.http_client_init_cb = authorize_download;
    esp_https_ota_handle_t ota = nullptr;
    esp_err_t err = esp_https_ota_begin(&config, &ota);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "update download failed: %s", esp_err_to_name(err));
        return false;
    }
    esp_app_desc_t desc;
    if (esp_https_ota_get_img_desc(ota, &desc) != ESP_OK ||
        std::strcmp(desc.project_name, esp_app_get_description()->project_name) != 0 || build_of(desc) != build) {
        ESP_LOGE(TAG, "update: not the firmware offered");
        esp_https_ota_abort(ota);
        return false;
    }
    while ((err = esp_https_ota_perform(ota)) == ESP_ERR_HTTPS_OTA_IN_PROGRESS) {
    }
    if (err != ESP_OK || !esp_https_ota_is_complete_data_received(ota)) {
        ESP_LOGE(TAG, "update download failed: %s", esp_err_to_name(err));
        esp_https_ota_abort(ota);
        return false;
    }
    err = esp_https_ota_finish(ota);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "update refused (signature or image): %s", esp_err_to_name(err));
        return false;
    }
    return true;
}

void Link::voice_task(void* arg) {
    auto* self = static_cast<Link*>(arg);
    audio::OpusDecoder decoder;
    if (!decoder.open(audio::kSpeakerRate)) {
        // A PSRAM-stack task cannot delete itself; stay parked.
        vTaskSuspend(nullptr);
    }
    std::vector<int16_t> pcm;
    Packet* packet;
    int used = -1, decoded = 0;
    while (true) {
        // Peek, mark, then take: drained() never sees the packet in neither place.
        if (xQueuePeek(self->voice_, &packet, portMAX_DELAY) != pdTRUE) {
            continue;
        }
        self->decoding_.store(true);
        // A barge-in (stop_voice) may have taken and freed the peeked packet
        // meanwhile: only a packet received here is ours.
        if (xQueueReceive(self->voice_, &packet, 0) != pdTRUE) {
            self->decoding_.store(false);
            continue;
        }
        pcm.clear();
        if (self->voice_open_.load() && decoder.decode(packet->data, packet->size, pcm)) {
            self->sound_.play(pcm.data(), pcm.size());
            log_stack_use("voice", kVoiceStack, used, ++decoded);
        }
        free(packet);
        self->decoding_.store(false);
    }
}

}  // namespace net
