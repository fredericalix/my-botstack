// SPDX-License-Identifier: MIT
#include "audio/opus.h"

#include "decoder/impl/esp_opus_dec.h"
#include "encoder/impl/esp_opus_enc.h"
#include "esp_log.h"

namespace audio {
namespace {
const char* TAG = "opus";
}

bool OpusEncoder::open(int sample_rate, int bitrate) {
    esp_opus_enc_config_t cfg = ESP_OPUS_ENC_CONFIG_DEFAULT();
    cfg.sample_rate = sample_rate;
    cfg.channel = 1;
    cfg.bits_per_sample = 16;
    cfg.bitrate = bitrate;
    cfg.frame_duration = ESP_OPUS_ENC_FRAME_DURATION_60_MS;
    cfg.application_mode = ESP_OPUS_ENC_APPLICATION_VOIP;
    // Lowest complexity: the microphone stream only needs to be understood, and
    // the CPU is shared with echo cancellation.
    cfg.complexity = 0;
    cfg.enable_vbr = true;
    if (esp_opus_enc_open(&cfg, sizeof(cfg), &handle_) != ESP_AUDIO_ERR_OK) {
        ESP_LOGE(TAG, "encoder open failed");
        return false;
    }
    int in_size = 0;
    esp_opus_enc_get_frame_size(handle_, &in_size, &max_packet_);
    frame_samples_ = in_size / sizeof(int16_t);
    return true;
}

bool OpusEncoder::encode(const int16_t* pcm, std::vector<uint8_t>& packet) {
    packet.resize(max_packet_);
    esp_audio_enc_in_frame_t in = {(uint8_t*)pcm, (uint32_t)(frame_samples_ * sizeof(int16_t))};
    esp_audio_enc_out_frame_t out = {packet.data(), (uint32_t)packet.size(), 0, 0};
    if (esp_opus_enc_process(handle_, &in, &out) != ESP_AUDIO_ERR_OK) {
        return false;
    }
    packet.resize(out.encoded_bytes);
    return true;
}

bool OpusDecoder::open(int sample_rate) {
    esp_opus_dec_cfg_t cfg = ESP_OPUS_DEC_CONFIG_DEFAULT();
    cfg.sample_rate = sample_rate;
    cfg.channel = 1;
    cfg.frame_duration = ESP_OPUS_DEC_FRAME_DURATION_INVALID;  // any packet up to 120 ms
    cfg.self_delimited = false;
    if (esp_opus_dec_open(&cfg, sizeof(cfg), &handle_) != ESP_AUDIO_ERR_OK) {
        ESP_LOGE(TAG, "decoder open failed");
        return false;
    }
    buffer_.resize(sample_rate * 120 / 1000);
    return true;
}

bool OpusDecoder::decode(const uint8_t* packet, size_t size, std::vector<int16_t>& pcm) {
    esp_audio_dec_in_raw_t raw = {};
    raw.buffer = const_cast<uint8_t*>(packet);
    raw.len = size;
    esp_audio_dec_out_frame_t out = {(uint8_t*)buffer_.data(), (uint32_t)(buffer_.size() * sizeof(int16_t)), 0, 0};
    esp_audio_dec_info_t info = {};
    if (esp_opus_dec_decode(handle_, &raw, &out, &info) != ESP_AUDIO_ERR_OK) {
        return false;
    }
    pcm.insert(pcm.end(), buffer_.begin(), buffer_.begin() + out.decoded_size / sizeof(int16_t));
    return true;
}

}  // namespace audio
