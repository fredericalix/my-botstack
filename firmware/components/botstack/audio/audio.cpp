// SPDX-License-Identifier: MIT
#include "audio/audio.h"

#include <algorithm>
#include <climits>
#include <cmath>
#include <cstdlib>
#include <vector>

#include "audio/resampler.h"
#include "esp_afe_config.h"
#include "esp_afe_sr_iface.h"
#include "esp_afe_sr_models.h"
#include "esp_heap_caps.h"
#include "esp_log.h"
#include "esp_timer.h"

namespace audio {
namespace {

const char* TAG = "audio";
constexpr size_t kChunk = kCodecRate / 100;  // 10 ms at 24 kHz
constexpr size_t kMicQueueBytes = kMicRate * 2 * 2;          // 2 s
constexpr size_t kSpeakerQueueBytes = kSpeakerRate * 2 * 4;  // 4 s
// The reference stays on this long after the last sound written: the I2S DMA
// (6 x 10 ms) plus the room's echo tail.
constexpr int64_t kReferenceHoldUs = 300'000;
constexpr size_t kBlock = kMicRate / 100;    // 10 ms at 16 kHz, the envelope step
constexpr size_t kPlayedRing = 16384;        // ~1 s of played samples for the reference
constexpr size_t kEnvRing = 1024;            // ~10 s of 10 ms envelopes
constexpr size_t kEnvWindow = 300;           // correlate over the last 3 s
constexpr size_t kTxQueue = 6 * 240 * 2 / 3; // I2S DMA ahead of the speaker, at 16 kHz

StreamBufferHandle_t psram_stream(size_t bytes) {
    auto* storage = static_cast<uint8_t*>(heap_caps_malloc(bytes + 1, MALLOC_CAP_SPIRAM));
    auto* control = static_cast<StaticStreamBuffer_t*>(heap_caps_malloc(sizeof(StaticStreamBuffer_t), MALLOC_CAP_INTERNAL));
    if (storage == nullptr || control == nullptr) {
        return nullptr;
    }
    return xStreamBufferCreateStatic(bytes, 2, storage, control);
}

uint64_t sum_squares(const int16_t* pcm, size_t n) {
    uint64_t sum = 0;
    for (size_t i = 0; i < n; i++) {
        sum += (int32_t)pcm[i] * pcm[i];
    }
    return sum;
}

}  // namespace

bool Audio::start(int volume) {
    if (!codec_.init(volume)) {
        return false;
    }
    mic_ = psram_stream(kMicQueueBytes);
    speaker_ = psram_stream(kSpeakerQueueBytes);
    played_ = static_cast<int16_t*>(heap_caps_calloc(kPlayedRing, sizeof(int16_t), MALLOC_CAP_SPIRAM));
    played_env_ = static_cast<float*>(heap_caps_calloc(kEnvRing, sizeof(float), MALLOC_CAP_SPIRAM));
    if (mic_ == nullptr || speaker_ == nullptr || played_ == nullptr || played_env_ == nullptr) {
        ESP_LOGE(TAG, "no memory for audio queues");
        return false;
    }

    // Echo cancellation against the speaker reference, WebRTC voice activity and AGC.
    // No model partition on this board: no neural noise suppression.
    afe_config_t* config = afe_config_init("MR", nullptr, AFE_TYPE_VC, AFE_MODE_HIGH_PERF);
    // Reference: the played signal (see audio.h). VOIP_HIGH_PERF overloaded
    // core 1 (the AFE ring overflowed and IDLE1 starved).
    config->aec_init = true;
    config->aec_mode = AEC_MODE_VOIP_LOW_COST;
    config->aec_filter_length = 4;
    config->ns_init = false;
    config->vad_init = true;
    config->vad_mode = VAD_MODE_0;
    config->vad_min_noise_ms = 100;
    // The AFE output was too quiet for the server's voice detection (speech
    // around -50 dBFS). WebRTC AGC after echo cancellation brings it up.
    config->agc_init = true;
    config->agc_mode = AFE_AGC_MODE_WEBRTC;
    config->agc_compression_gain_db = 9;
    config->agc_target_level_dbfs = 3;
    config->afe_linear_gain = 1.0f;
    config->memory_alloc_mode = AFE_MEMORY_ALLOC_MORE_PSRAM;
    afe_ = esp_afe_handle_from_config(config);
    afe_data_ = afe_ != nullptr ? afe_->create_from_config(config) : nullptr;
    afe_config_free(config);
    if (afe_data_ == nullptr) {
        ESP_LOGE(TAG, "AFE create failed");
        return false;
    }
    ESP_LOGI(TAG, "AFE: feed %d samples x %d channels, fetch %d samples", afe_->get_feed_chunksize(afe_data_),
             afe_->get_feed_channel_num(afe_data_), afe_->get_fetch_chunksize(afe_data_));

    // Capture and AFE stacks in PSRAM (internal RAM is short); playback stays
    // internal, it is the one paced by the I2S DMA.
    xTaskCreatePinnedToCoreWithCaps(capture_task, "capture", 6144, this, 8, nullptr, 1, MALLOC_CAP_SPIRAM);
    xTaskCreatePinnedToCoreWithCaps(fetch_task, "afe_fetch", 6144, this, 7, nullptr, 1, MALLOC_CAP_SPIRAM);
    xTaskCreatePinnedToCore(playback_task, "playback", 4096, this, 9, nullptr, 0);
    return true;
}

void Audio::capture_task(void* arg) {
    auto* self = static_cast<Audio*>(arg);
    Resampler mic_rs(2, 3);
    std::vector<int16_t> raw(kChunk * 2), mic24(kChunk), mic16, feed;
    std::vector<float> mic_env(kEnvRing, 0.0f);
    const size_t chunk = self->afe_->get_feed_chunksize(self->afe_data_);
    uint64_t mic_count = 0, mic_blocks = 0, active_blocks = 0;
    float env = 0;
    int64_t offset = 0;  // reference index = microphone index + offset
    bool calibrated = false;
    while (true) {
        if (!self->codec_.read(raw.data(), kChunk)) {
            vTaskDelay(1);
            continue;
        }
        int peak = 0, clipped = 0;
        uint64_t ref_sq = 0;
        for (size_t i = 0; i < kChunk; i++) {
            mic24[i] = raw[2 * i];
            int16_t loop = raw[2 * i + 1];  // ES7210 loopback, measured only
            int a = std::abs((int)loop);
            peak = std::max(peak, a);
            clipped += a >= 32700;
            ref_sq += (int32_t)loop * loop;
        }
        self->raw_sq_ += sum_squares(mic24.data(), kChunk);
        self->raw_n_ += kChunk;
        self->ref_sq_ += ref_sq;
        self->ref_n_ += kChunk;
        self->ref_clipped_ += clipped;
        if (peak > self->ref_peak_.load()) {
            self->ref_peak_.store(peak);
        }

        mic16.clear();
        mic_rs.process(mic24.data(), kChunk, mic16);
        bool playing = self->playing();
        uint64_t played = self->played_count_.load();
        for (int16_t sample : mic16) {
            int16_t ref = 0;
            int64_t index = (int64_t)mic_count + offset;
            if (calibrated && index >= 0 && (uint64_t)index < played && played - index < kPlayedRing) {
                ref = self->played_[index % kPlayedRing];
            }
            // Until the delay is known the echo cannot be cancelled: send
            // silence while the robot speaks rather than its own voice.
            feed.push_back(!calibrated && playing ? 0 : sample);
            feed.push_back(ref);
            env += std::abs((int)sample);
            if (++mic_count % kBlock == 0) {
                mic_env[mic_blocks % kEnvRing] = env;
                env = 0;
                mic_blocks++;
                active_blocks = playing ? active_blocks + 1 : 0;
                // Every second of continuous playback, check the delay.
                if (active_blocks >= kEnvWindow / 2 && mic_blocks % 100 == 0) {
                    int64_t found = offset;
                    self->estimate_echo_delay(mic_env.data(), mic_blocks, found);
                    if (found != INT64_MIN) {
                        offset = found;
                        calibrated = true;
                    }
                }
            }
        }
        while (feed.size() >= chunk * 2) {
            self->afe_->feed(self->afe_data_, feed.data());
            feed.erase(feed.begin(), feed.begin() + chunk * 2);
        }
    }
}

// Normalised cross-correlation of the microphone and played envelopes over the
// last 3 s, for delays around the I2S queue. Sets `offset` (reference index =
// microphone index + offset), one block early so the reference leads the echo,
// or INT64_MIN when the match is weak.
void Audio::estimate_echo_delay(const float* mic_env, uint64_t mic_blocks, int64_t& offset) {
    uint64_t played_blocks = played_count_.load() / kBlock;
    // Played samples are written about kTxQueue ahead of the speaker.
    int64_t ahead = (int64_t)played_count_.load() - (int64_t)(mic_blocks * kBlock) - (int64_t)kTxQueue;
    int64_t guess = ahead / (int64_t)kBlock;
    float best = 0;
    int64_t best_k = 0;
    for (int64_t k = guess - 15; k <= guess + 10; k++) {
        double sx = 0, sy = 0, sxx = 0, syy = 0, sxy = 0;
        int n = 0;
        for (uint64_t b = mic_blocks - kEnvWindow; b < mic_blocks; b++) {
            int64_t pb = (int64_t)b + k;
            if (pb < 0 || (uint64_t)pb >= played_blocks || played_blocks - pb >= kEnvRing) {
                continue;
            }
            double x = mic_env[b % kEnvRing], y = played_env_[pb % kEnvRing];
            sx += x, sy += y, sxx += x * x, syy += y * y, sxy += x * y;
            n++;
        }
        if (n < (int)kEnvWindow / 2) {
            continue;
        }
        double cov = sxy - sx * sy / n, vx = sxx - sx * sx / n, vy = syy - sy * sy / n;
        float r = vx > 0 && vy > 0 ? (float)(cov / std::sqrt(vx * vy)) : 0;
        if (r > best) {
            best = r;
            best_k = k;
        }
    }
    // A first match calibrates; later ones must be clear to move it (the end of
    // a reply, mostly silence, correlates poorly).
    if (best < (echo_delay_ms_.load() < 0 ? 0.5f : 0.75f)) {
        offset = INT64_MIN;
        return;
    }
    int64_t found = (best_k + 1) * (int64_t)kBlock;
    if (found != offset || echo_delay_ms_.load() < 0) {
        ESP_LOGI(TAG, "echo delay: reference offset %lld samples (correlation %.2f, guess %lld blocks, best %lld)",
                 (long long)found, best, (long long)guess, (long long)best_k);
    }
    // The delay between writing a sample and hearing it back.
    echo_delay_ms_.store((int)((ahead - best_k * (int64_t)kBlock + (int64_t)kTxQueue) * 1000 / kMicRate));
    offset = found;
}

void Audio::fetch_task(void* arg) {
    auto* self = static_cast<Audio*>(arg);
    while (true) {
        afe_fetch_result_t* result = self->afe_->fetch(self->afe_data_);
        if (result == nullptr || result->ret_value == ESP_FAIL || result->data == nullptr) {
            continue;
        }
        size_t samples = result->data_size / sizeof(int16_t);
        self->clean_sq_ += sum_squares(result->data, samples);
        self->clean_n_ += samples;
        self->voice_.store(result->vad_state == VAD_SPEECH);
        if (self->mic_enabled_.load()) {
            // Never block the AFE: drop when the consumer is behind.
            xStreamBufferSend(self->mic_, result->data, result->data_size, 0);
        }
    }
}

void Audio::playback_task(void* arg) {
    auto* self = static_cast<Audio*>(arg);
    std::vector<int16_t> pcm(kChunk), pcm16;
    Resampler down(2, 3);
    uint64_t count = 0;
    float env = 0;
    uint32_t flushed = 0;
    while (true) {
        const uint32_t epoch = self->flush_epoch_.load();
        if (epoch != flushed) {
            flushed = epoch;
            while (xStreamBufferReceive(self->speaker_, pcm.data(), kChunk * sizeof(int16_t), 0) > 0) {
            }
        }
        size_t got = xStreamBufferReceive(self->speaker_, pcm.data(), kChunk * sizeof(int16_t), 0) / sizeof(int16_t);
        std::fill(pcm.begin() + got, pcm.end(), 0);
        float level = got ? std::sqrt(sum_squares(pcm.data(), kChunk) / (float)kChunk) / 32768.0f : 0.0f;
        if (got > 0) {
            self->last_play_us_.store(esp_timer_get_time());
        }
        self->speaker_level_.store(std::min(1.0f, level * 4.0f));
        // The echo canceller's reference: exactly what goes to the speaker.
        pcm16.clear();
        down.process(pcm.data(), kChunk, pcm16);
        for (int16_t sample : pcm16) {
            self->played_[count % kPlayedRing] = sample;
            env += std::abs((int)sample);
            if (++count % kBlock == 0) {
                self->played_env_[(count / kBlock - 1) % kEnvRing] = env;
                env = 0;
            }
        }
        self->played_count_.store(count);
        self->codec_.write(pcm.data(), kChunk);
    }
}

size_t Audio::read_mic(int16_t* dst, size_t max_samples, TickType_t wait) {
    return xStreamBufferReceive(mic_, dst, max_samples * sizeof(int16_t), wait) / sizeof(int16_t);
}

void Audio::set_mic_enabled(bool enabled) {
    mic_enabled_.store(enabled);
    if (!enabled) {
        xStreamBufferReset(mic_);
    }
}

void Audio::play(const int16_t* pcm, size_t samples) {
    // A stream buffer takes one writer: the voice and the camera's shutter both play.
    std::lock_guard<std::mutex> lock(play_lock_);
    const uint32_t epoch = flush_epoch_.load();
    const uint8_t* bytes = reinterpret_cast<const uint8_t*>(pcm);
    size_t left = samples * sizeof(int16_t);
    while (left > 0 && flush_epoch_.load() == epoch) {
        size_t sent = xStreamBufferSend(speaker_, bytes, left, pdMS_TO_TICKS(20));
        bytes += sent;
        left -= sent;
    }
}

void Audio::stop_playback() {
    flush_epoch_.fetch_add(1);
    xStreamBufferReset(speaker_);  // at once when no writer is blocked; else the playback task empties it
}

int Audio::queued_ms() const {
    return (int)(xStreamBufferBytesAvailable(speaker_) / sizeof(int16_t) * 1000 / kSpeakerRate);
}

void Audio::take_reference(float& rms, int& peak, int& clipped) {
    uint64_t n = ref_n_.exchange(0), sq = ref_sq_.exchange(0);
    rms = n ? std::sqrt((double)sq / n) / 32768.0 : 0;
    peak = ref_peak_.exchange(0);
    clipped = ref_clipped_.exchange(0);
}

bool Audio::playing() const { return esp_timer_get_time() - last_play_us_.load() < kReferenceHoldUs; }

void Audio::take_levels(float& raw, float& clean) {
    uint64_t rn = raw_n_.exchange(0), rs = raw_sq_.exchange(0);
    uint64_t cn = clean_n_.exchange(0), cs = clean_sq_.exchange(0);
    raw = rn ? std::sqrt((double)rs / rn) / 32768.0 : 0;
    clean = cn ? std::sqrt((double)cs / cn) / 32768.0 : 0;
}

}  // namespace audio
