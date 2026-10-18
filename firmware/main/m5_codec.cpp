// SPDX-License-Identifier: MIT
#include "m5_codec.h"

#include "bsp/m5stack_core_s3.h"
#include "driver/i2s_std.h"
#include "driver/i2s_tdm.h"
#include "esp_codec_dev_defaults.h"
#include "esp_log.h"

namespace {

using audio::kCodecRate;

const char* TAG = "codec";
// ES7210 gain in dB on the microphone channel. The StackChan firmware asks for
// 60, which the driver clamps to the chip's maximum.
constexpr float kMicGain = 37.5f;

}  // namespace

bool M5Codec::init(int volume) {
    if (bsp_i2c_init() != ESP_OK) {
        ESP_LOGE(TAG, "I2C init failed");
        return false;
    }
    bsp_feature_enable(BSP_FEATURE_SPEAKER, true);

    i2s_chan_handle_t tx = nullptr, rx = nullptr;
    i2s_chan_config_t chan = I2S_CHANNEL_DEFAULT_CONFIG(I2S_NUM_0, I2S_ROLE_MASTER);
    chan.dma_desc_num = 6;
    chan.dma_frame_num = 240;
    chan.auto_clear_after_cb = true;
    if (i2s_new_channel(&chan, &tx, &rx) != ESP_OK) {
        ESP_LOGE(TAG, "I2S channel failed");
        return false;
    }

    i2s_std_config_t out = {};
    out.clk_cfg = I2S_STD_CLK_DEFAULT_CONFIG(kCodecRate);
    out.slot_cfg = I2S_STD_PHILIPS_SLOT_DEFAULT_CONFIG(I2S_DATA_BIT_WIDTH_16BIT, I2S_SLOT_MODE_STEREO);
    out.gpio_cfg = {BSP_I2S_MCLK, BSP_I2S_SCLK, BSP_I2S_LCLK, BSP_I2S_DOUT, I2S_GPIO_UNUSED, {}};

    i2s_tdm_config_t in = {};
    in.clk_cfg = I2S_TDM_CLK_DEFAULT_CONFIG(kCodecRate);
    in.clk_cfg.bclk_div = 8;
    in.slot_cfg = I2S_TDM_PHILIPS_SLOT_DEFAULT_CONFIG(
        I2S_DATA_BIT_WIDTH_16BIT, I2S_SLOT_MODE_STEREO,
        (i2s_tdm_slot_mask_t)(I2S_TDM_SLOT0 | I2S_TDM_SLOT1 | I2S_TDM_SLOT2 | I2S_TDM_SLOT3));
    in.gpio_cfg = {BSP_I2S_MCLK, BSP_I2S_SCLK, BSP_I2S_LCLK, I2S_GPIO_UNUSED, BSP_I2S_DSIN, {}};

    if (i2s_channel_init_std_mode(tx, &out) != ESP_OK || i2s_channel_init_tdm_mode(rx, &in) != ESP_OK ||
        i2s_channel_enable(tx) != ESP_OK || i2s_channel_enable(rx) != ESP_OK) {
        ESP_LOGE(TAG, "I2S setup failed");
        return false;
    }

    audio_codec_i2s_cfg_t i2s_cfg = {};
    i2s_cfg.port = I2S_NUM_0;
    i2s_cfg.rx_handle = rx;
    i2s_cfg.tx_handle = tx;
    const audio_codec_data_if_t* data_if = audio_codec_new_i2s_data(&i2s_cfg);

    audio_codec_i2c_cfg_t i2c_cfg = {.port = I2C_NUM_1, .addr = AW88298_CODEC_DEFAULT_ADDR, .bus_handle = bsp_i2c_get_handle()};
    const audio_codec_ctrl_if_t* out_ctrl = audio_codec_new_i2c_ctrl(&i2c_cfg);
    const audio_codec_gpio_if_t* gpio_if = audio_codec_new_gpio();
    aw88298_codec_cfg_t aw = {};
    aw.ctrl_if = out_ctrl;
    aw.gpio_if = gpio_if;
    aw.reset_pin = GPIO_NUM_NC;
    aw.hw_gain.pa_voltage = 5.0;
    aw.hw_gain.codec_dac_voltage = 3.3;
    aw.hw_gain.pa_gain = 1;
    esp_codec_dev_cfg_t out_dev = {.dev_type = ESP_CODEC_DEV_TYPE_OUT, .codec_if = aw88298_codec_new(&aw), .data_if = data_if};
    output_ = esp_codec_dev_new(&out_dev);

    i2c_cfg.addr = ES7210_CODEC_DEFAULT_ADDR;
    es7210_codec_cfg_t es = {};
    es.ctrl_if = audio_codec_new_i2c_ctrl(&i2c_cfg);
    es.mic_selected = ES7210_SEL_MIC1 | ES7210_SEL_MIC2 | ES7210_SEL_MIC3;
    esp_codec_dev_cfg_t in_dev = {.dev_type = ESP_CODEC_DEV_TYPE_IN, .codec_if = es7210_codec_new(&es), .data_if = data_if};
    input_ = esp_codec_dev_new(&in_dev);
    if (output_ == nullptr || input_ == nullptr) {
        ESP_LOGE(TAG, "codec devices failed");
        return false;
    }

    esp_codec_dev_sample_info_t speaker = {};
    speaker.bits_per_sample = 16;
    speaker.channel = 1;
    speaker.sample_rate = kCodecRate;
    esp_codec_dev_sample_info_t mic = {};
    mic.bits_per_sample = 16;
    mic.channel = 2;
    mic.channel_mask = ESP_CODEC_DEV_MAKE_CHANNEL_MASK(0) | ESP_CODEC_DEV_MAKE_CHANNEL_MASK(1);
    mic.sample_rate = kCodecRate;
    if (esp_codec_dev_open(output_, &speaker) != ESP_CODEC_DEV_OK || esp_codec_dev_open(input_, &mic) != ESP_CODEC_DEV_OK) {
        ESP_LOGE(TAG, "codec open failed");
        return false;
    }
    esp_codec_dev_set_in_channel_gain(input_, ESP_CODEC_DEV_MAKE_CHANNEL_MASK(0), kMicGain);
    set_volume(volume);
    ESP_LOGI(TAG, "duplex audio at %d Hz, microphone + speaker reference", kCodecRate);
    return true;
}

bool M5Codec::read(int16_t* interleaved, size_t frames) {
    return esp_codec_dev_read(input_, interleaved, frames * 2 * sizeof(int16_t)) == ESP_CODEC_DEV_OK;
}

bool M5Codec::write(const int16_t* mono, size_t frames) {
    return esp_codec_dev_write(output_, const_cast<int16_t*>(mono), frames * sizeof(int16_t)) == ESP_CODEC_DEV_OK;
}

void M5Codec::set_volume(int volume) { esp_codec_dev_set_out_vol(output_, volume); }
