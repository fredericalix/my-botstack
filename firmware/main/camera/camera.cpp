// SPDX-License-Identifier: MIT
#include "camera/camera.h"

#include <fcntl.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

#include "bsp/m5stack_core_s3.h"
#include "esp_heap_caps.h"
#include "esp_jpeg_enc.h"
#include "esp_log.h"
#include "esp_timer.h"
#include "esp_video_device.h"
#include "linux/videodev2.h"

namespace camera {
namespace {

const char* TAG = "camera";
constexpr int kSkipFrames = 15;  // ~0.75 s at 20 fps for auto exposure
constexpr int kQuality = 80;
constexpr size_t kJpegMax = 256 * 1024;
constexpr int kType = V4L2_BUF_TYPE_VIDEO_CAPTURE;

bool encode(const uint8_t* yuyv, size_t bytes, uint8_t** jpeg, size_t* size) {
    jpeg_enc_config_t config = DEFAULT_JPEG_ENC_CONFIG();
    config.width = kWidth;
    config.height = kHeight;
    config.src_type = JPEG_PIXEL_FORMAT_YCbYCr;
    config.subsampling = JPEG_SUBSAMPLE_422;
    config.quality = kQuality;
    jpeg_enc_handle_t encoder = nullptr;
    if (jpeg_enc_open(&config, &encoder) != JPEG_ERR_OK) {
        ESP_LOGE(TAG, "JPEG encoder unavailable");
        return false;
    }
    auto* out = static_cast<uint8_t*>(heap_caps_aligned_alloc(16, kJpegMax, MALLOC_CAP_SPIRAM));
    int written = 0;
    bool ok = out != nullptr &&
              jpeg_enc_process(encoder, yuyv, (int)bytes, out, (int)kJpegMax, &written) == JPEG_ERR_OK && written > 0;
    jpeg_enc_close(encoder);
    if (!ok) {
        free(out);
        ESP_LOGE(TAG, "JPEG encode failed");
        return false;
    }
    *jpeg = out;
    *size = (size_t)written;
    return true;
}

}  // namespace

bool Camera::start() {
    if (bsp_camera_start(nullptr) != ESP_OK) {
        ESP_LOGE(TAG, "camera start failed");
        return false;
    }
    fd_ = open(BSP_CAMERA_DEVICE, O_RDONLY);
    if (fd_ < 0) {
        ESP_LOGE(TAG, "cannot open %s", BSP_CAMERA_DEVICE);
        return false;
    }
    v4l2_format format = {};
    format.type = kType;
    format.fmt.pix.width = kWidth;
    format.fmt.pix.height = kHeight;
    format.fmt.pix.pixelformat = V4L2_PIX_FMT_YUYV;
    if (ioctl(fd_, VIDIOC_S_FMT, &format) != 0) {
        ESP_LOGE(TAG, "%dx%d YUYV not supported", kWidth, kHeight);
        return false;
    }
    v4l2_requestbuffers request = {};
    request.count = kBuffers;
    request.type = kType;
    request.memory = V4L2_MEMORY_MMAP;
    if (ioctl(fd_, VIDIOC_REQBUFS, &request) != 0) {
        ESP_LOGE(TAG, "no frame buffers");
        return false;
    }
    for (int i = 0; i < kBuffers; i++) {
        v4l2_buffer buf = {};
        buf.type = kType;
        buf.memory = V4L2_MEMORY_MMAP;
        buf.index = i;
        if (ioctl(fd_, VIDIOC_QUERYBUF, &buf) != 0) {
            return false;
        }
        buffers_[i] = static_cast<uint8_t*>(mmap(nullptr, buf.length, PROT_READ | PROT_WRITE, MAP_SHARED, fd_, buf.m.offset));
        if (buffers_[i] == nullptr) {
            return false;
        }
    }
    ready_ = true;
    ESP_LOGI(TAG, "camera ready, %dx%d YUYV", kWidth, kHeight);
    return true;
}

bool Camera::take(uint8_t** jpeg, size_t* size) {
    *jpeg = nullptr;
    *size = 0;
    if (!ready_) {
        return false;
    }
    int64_t started = esp_timer_get_time();
    for (int i = 0; i < kBuffers; i++) {
        v4l2_buffer buf = {};
        buf.type = kType;
        buf.memory = V4L2_MEMORY_MMAP;
        buf.index = i;
        if (ioctl(fd_, VIDIOC_QBUF, &buf) != 0) {
            return false;
        }
    }
    int type = kType;
    if (ioctl(fd_, VIDIOC_STREAMON, &type) != 0) {
        ESP_LOGE(TAG, "stream on failed; internal DMA RAM free %u, largest block %u",
                 (unsigned)heap_caps_get_free_size(MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL),
                 (unsigned)heap_caps_get_largest_free_block(MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL));
        return false;
    }

    // Skip the first frames (exposure, white balance), then encode the first good one.
    bool ok = false;
    for (int frame = 0; frame < kSkipFrames + 6 && !ok; frame++) {
        v4l2_buffer buf = {};
        buf.type = kType;
        buf.memory = V4L2_MEMORY_MMAP;
        if (ioctl(fd_, VIDIOC_DQBUF, &buf) != 0) {
            ESP_LOGE(TAG, "no frame");
            break;
        }
        bool good = (buf.flags & V4L2_BUF_FLAG_DONE) && !(buf.flags & V4L2_BUF_FLAG_ERROR);
        if (frame >= kSkipFrames && good) {
            ok = encode(buffers_[buf.index], buf.bytesused, jpeg, size);
            break;
        }
        if (ioctl(fd_, VIDIOC_QBUF, &buf) != 0) {
            break;
        }
    }
    ioctl(fd_, VIDIOC_STREAMOFF, &type);
    ESP_LOGI(TAG, "photo: %s, %u bytes in %lld ms", ok ? "ok" : "failed", (unsigned)*size,
             (long long)((esp_timer_get_time() - started) / 1000));
    return ok;
}

}  // namespace camera
