// SPDX-License-Identifier: MIT
//
// The robot's ears and voice.
//
// Capture: microphone at 24 kHz -> 16 kHz -> ESP-SR AFE (echo cancellation,
// AGC, voice activity) -> a queue of clean 16 kHz mono PCM.
//
// The echo canceller's reference is what we play, not the ES7210 loopback
// channel: that channel carries the speaker about 40 dB under the microphone
// and the user's voice at a similar level, so the canceller removed the voice
// too. The played signal, resampled to 16 kHz, goes into a ring indexed by
// sample count; the capture task finds the echo delay by correlating the
// microphone's envelope with it and feeds the matching samples. Playback: a queue of 24 kHz mono PCM -> speaker,
// with silence when it is empty, so the AFE reference stays in step.
#pragma once

#include <atomic>
#include <mutex>
#include <cstddef>
#include <cstdint>

#include "audio/codec.h"
#include "esp_afe_sr_iface.h"
#include "freertos/FreeRTOS.h"
#include "freertos/stream_buffer.h"

namespace audio {

constexpr int kMicRate = 16000;
constexpr int kSpeakerRate = kCodecRate;

class Audio {
public:
    explicit Audio(Codec& codec) : codec_(codec) {}
    bool start(int volume);

    /// Clean microphone PCM (16 kHz mono). Returns the samples read.
    size_t read_mic(int16_t* dst, size_t max_samples, TickType_t wait);
    /// When off, captured audio is dropped instead of queued.
    void set_mic_enabled(bool enabled);

    /// Queue speaker PCM (24 kHz mono). Blocks while the queue is full, and
    /// gives up on what is left when stop_playback() is called meanwhile.
    void play(const int16_t* pcm, size_t samples);
    /// Drop everything queued for the speaker. Safe from any task.
    void stop_playback();
    /// Milliseconds of audio queued for the speaker.
    int queued_ms() const;

    /// Speaker level of the last 10 ms, 0..1, for the face.
    float speaker_level() const { return speaker_level_.load(); }
    /// The AFE hears speech.
    bool voice() const { return voice_.load(); }

    /// RMS (0..1) of the raw microphone and of the AFE output since the last call.
    void take_levels(float& raw, float& clean);
    /// The raw reference channel since the last call: RMS (0..1), peak, and
    /// samples at full scale.
    void take_reference(float& rms, int& peak, int& clipped);
    /// Something played in the last few hundred milliseconds.
    bool playing() const;
    /// Echo delay found by the capture task, ms (-1 until measured).
    int echo_delay_ms() const { return echo_delay_ms_.load(); }

private:
    static void capture_task(void* self);
    static void fetch_task(void* self);
    static void playback_task(void* self);

    Codec& codec_;
    const esp_afe_sr_iface_t* afe_ = nullptr;
    esp_afe_sr_data_t* afe_data_ = nullptr;
    StreamBufferHandle_t mic_ = nullptr;
    StreamBufferHandle_t speaker_ = nullptr;
    std::atomic<bool> mic_enabled_{true};
    std::atomic<float> speaker_level_{0};
    std::atomic<bool> voice_{false};

    std::atomic<uint64_t> raw_sq_{0}, raw_n_{0}, clean_sq_{0}, clean_n_{0}, ref_sq_{0}, ref_n_{0};
    std::atomic<int> ref_peak_{0}, ref_clipped_{0};
    std::atomic<int64_t> last_play_us_{0};  // last non-silent block written to the speaker
    // Bumped by stop_playback(): the playback task empties the speaker queue
    // and play() stops writing. A stream buffer reset alone fails while a
    // writer is blocked on it, and up to 4 s then played on.
    std::atomic<uint32_t> flush_epoch_{0};
    std::mutex play_lock_;  // play() from several tasks

    // The played signal at 16 kHz, by sample count (written by playback, read by capture).
    int16_t* played_ = nullptr;
    float* played_env_ = nullptr;  // per 10 ms block, by block count
    std::atomic<uint64_t> played_count_{0};
    std::atomic<int> echo_delay_ms_{-1};
    void estimate_echo_delay(const float* mic_env, uint64_t mic_blocks, int64_t& offset);
};

}  // namespace audio
