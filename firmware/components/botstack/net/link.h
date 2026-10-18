// SPDX-License-Identifier: MIT
//
// The always-on link to the botstack bridge, protocol botstack/v1
// (docs/protocol.md).
//
// The WebSocket stays open while the robot is powered and reconnects with a
// growing delay (1, 2, 4, 8 s). It pings every 5 s; when nothing has come
// from the bridge for 20 s (no pong, no reconnection), the robot restarts.
// The bridge drives the robot: `state` sets the
// face and gates the microphone (listening and thinking, and speaking when
// the bridge says `mic`, so the user can talk over the robot), `speech`
// frames the voice packets (`cancel` stops them at once), `expression` and `animation`
// change the face, `tool` asks for the head (`head.set_angles`, answered at
// once while the head moves, or "The head does not answer.") or the camera (`camera.take_photo` runs in its own task: photo, JPEG, upload to the vision URL from the
// bridge's hello, then `tool_result` with the answer). A board without a head
// or a camera answers their tools "Not available on this firmware." from the
// control task, and starts no camera task. Conversations with
// Grok open and close inside the connection, and the bridge can make the robot
// speak at any time (pushed messages).
//
// A tap: idle -> `talk start`; listening or thinking -> `talk stop`;
// speaking -> `interrupt` and the speaker stops at once.
//
// `firmware` offers a published build (docs/firmware.md#updates): between
// conversations the robot downloads it, checks its signature, and restarts
// into it. The new firmware keeps itself once it has heard the bridge's hello.
//
// Fleet (docs/fleet.md): the robot connects with its own key from NVS
// (`botstack/key`) once the bridge sent one (`enroll`), else with the bootstrap
// key built into the image; a 401 on its own key erases it. It sends `status`
// (RSSI, free RAM, uptime, last reset reason, head, camera) after the hello
// and every 60 s, and restarts at the next pause when the bridge says `reboot`.
#pragma once

#include <atomic>
#include <mutex>
#include <string>

#include "audio/audio.h"
#include "botstack_board.h"
#include "esp_websocket_client.h"
#include "face/bloub.h"
#include "freertos/FreeRTOS.h"
#include "freertos/queue.h"

namespace net {

/// The bridge's URL (firmware/bridge-url, embedded at build time), without the
/// file's trailing newline. Link::start refuses one that is not `wss://`.
std::string bridge_url();

class Link {
public:
    /// `head` and `camera` may be null (the board has none); `features` is the
    /// JSON array the hello sends.
    Link(audio::Audio& sound, bloub::Face& face, botstack::HeadPort* head, botstack::CameraPort* camera,
         std::string features, std::string url)
        : sound_(sound), face_(face), head_(head), camera_(camera), features_(std::move(features)),
          url_(std::move(url)) {}
    void start();
    /// A tap on the screen or the head; any task. Silences the speaker at once.
    void tap();

private:
    /// The robot's state, as the bridge last set it. Offline: no link.
    enum class State : uint8_t { Offline, Idle, Listening, Thinking, Speaking };
    enum class Kind : uint8_t { Tap, Connected, Disconnected, Hello, State, SpeechStart, SpeechEnd, SpeechCancel, Tool, Head,
                                Enroll, KeySaved, Unauthorized, Reboot };
    struct Event {
        Kind kind;
        State state;
        uint32_t id;       // tool id
        bool mic = false;  // State: the microphone streams
        bool ok = false;   // Head: the move was accepted; Tap: the speaker was playing
    };

    static void on_ws(void* self, esp_event_base_t base, int32_t id, void* data);
    static void control_task(void* self);
    static void mic_task(void* self);
    static void voice_task(void* self);
    static void camera_task(void* self);
    static void update_task(void* self);
    static bool install(const std::string& build, const std::string& url);
    static void key_task(void* self);
    void start_key_task();
    void use_key(const std::string& key);
    void send_status();
    void offer_update(const std::string& build, const std::string& url);
    struct PhotoJob {
        uint32_t id;
        char question[384];
    };
    void send_tool_result(uint32_t id, bool ok, const char* result);

    void post(const Event& event);
    void handle(const Event& event);
    void on_text(const std::string& text);
    void send_text(const std::string& text);
    void set_state(State state, bool mic);
    void connect();
    void disconnect();
    void stop_voice();
    bool drained() const;

    audio::Audio& sound_;
    bloub::Face& face_;
    botstack::HeadPort* head_;
    botstack::CameraPort* camera_;
    std::string features_;
    std::string url_;
    std::string headers_;
    // Atomic so the WebSocket task can check it without the lock: it holds the
    // client's own lock while it delivers events, and the mic task holds ours
    // while it sends.
    std::atomic<esp_websocket_client_handle_t> client_{nullptr};
    std::mutex client_lock_;  // the mic task sends while the control task may close
    QueueHandle_t events_ = nullptr;
    QueueHandle_t voice_ = nullptr;
    QueueHandle_t photos_ = nullptr;  // PhotoJob, one at a time
    std::mutex vision_lock_;
    std::string vision_url_, vision_token_;
    std::mutex update_lock_;
    std::string update_build_, update_url_;  // the build the bridge offers
    std::string update_failed_;              // not tried again until the next boot
    std::mutex key_lock_;          // the key between the WebSocket, control and key tasks
    std::string key_to_save_;      // for key_task; empty erases
    std::string enroll_id_;        // its id, answered in `enrolled`
    std::string saved_key_, saved_id_;  // what key_task wrote last
    bool saved_ok_ = false;             // and whether NVS took it
    std::atomic<bool> key_task_running_{false};  // one key_task at a time
    bool own_key_ = false;         // headers_ carry this robot's own key (from NVS)
    bool reboot_pending_ = false;  // `reboot`: restart at the next pause
    int64_t status_at_ = 0;        // next `status`, microseconds; 0 = none
    std::atomic<bool> camera_ok_{false};
    std::atomic<bool> update_running_{false};
    std::atomic<bool> updating_{false};      // installing: taps are ignored
    std::atomic<State> state_{State::Offline};
    std::atomic<bool> mic_on_{false};
    std::atomic<bool> online_{false};       // the bridge said hello
    std::atomic<bool> voice_open_{false};   // between `speech start` and its end or a tap
    std::atomic<bool> decoding_{false};     // the voice task holds a packet
    bool played_pending_ = false;           // `speech end` came; say `played` once drained
    bool firmware_kept_ = false;            // marked valid after the first hello
    int64_t hello_deadline_ = 0;            // microseconds; 0 = none
    int64_t reconnect_at_ = 0;              // microseconds; 0 = none
    std::atomic<int64_t> heard_us_{0};      // last frame from the bridge, pongs included
    int backoff_ms_ = 1000;
    std::string text_buffer_;
    uint8_t frame_opcode_ = 0;
};

}  // namespace net
