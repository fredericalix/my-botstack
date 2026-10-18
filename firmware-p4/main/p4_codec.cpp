// SPDX-License-Identifier: MIT
#include "p4_codec.h"

#include "bsp/esp-bsp.h"
#include "esp_log.h"

namespace {

using audio::kCodecRate;

const char* TAG = "codec";
// ES7210 gain in dB on the microphone. A first guess; tuned by ear on the board.
constexpr float kMicGainDb = 30.0f;

}  // namespace

bool P4Codec::init(int volume) {
    // The BSP sets up I2C, the duplex I2S bus and the codecs; the speaker
    // codec drives the power amplifier (GPIO 53).
    output_ = bsp_audio_codec_speaker_init();
    input_ = bsp_audio_codec_microphone_init();
    if (output_ == nullptr || input_ == nullptr) {
        ESP_LOGE(TAG, "codec devices failed");
        return false;
    }
    // Mono 16-bit at 24 kHz both ways: the BSP's I2S bus is standard mono, so
    // the ES7210 gives its first microphone.
    esp_codec_dev_sample_info_t fs = {};
    fs.bits_per_sample = 16;
    fs.channel = 1;
    fs.sample_rate = kCodecRate;
    if (esp_codec_dev_open(output_, &fs) != ESP_CODEC_DEV_OK || esp_codec_dev_open(input_, &fs) != ESP_CODEC_DEV_OK) {
        ESP_LOGE(TAG, "codec open failed");
        return false;
    }
    esp_codec_dev_set_in_gain(input_, kMicGainDb);
    set_volume(volume);
    ESP_LOGI(TAG, "duplex audio at %d Hz, microphone + silent reference", kCodecRate);
    return true;
}

bool P4Codec::read(int16_t* interleaved, size_t frames) {
    // The ES7210 gives one microphone channel; the reference channel stays
    // silent (audio.cpp uses the played signal).
    mono_.resize(frames);
    if (esp_codec_dev_read(input_, mono_.data(), frames * sizeof(int16_t)) != ESP_CODEC_DEV_OK) {
        return false;
    }
    for (size_t i = 0; i < frames; ++i) {
        interleaved[2 * i] = mono_[i];
        interleaved[2 * i + 1] = 0;
    }
    return true;
}

bool P4Codec::write(const int16_t* mono, size_t frames) {
    return esp_codec_dev_write(output_, const_cast<int16_t*>(mono), frames * sizeof(int16_t)) == ESP_CODEC_DEV_OK;
}

void P4Codec::set_volume(int volume) { esp_codec_dev_set_out_vol(output_, volume); }
