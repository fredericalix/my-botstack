// SPDX-License-Identifier: MIT
#include "net/vision.h"

#include <algorithm>
#include <cstring>

#include "esp_crt_bundle.h"
#include "esp_http_client.h"
#include "esp_log.h"

namespace net {
namespace {

const char* TAG = "vision";
constexpr const char* kBoundary = "botstack-photo-7f3a91";
constexpr int kMaxAnswer = 4096;

}  // namespace

bool ask_vision(const std::string& url, const std::string& token, const std::string& device_id,
                const std::string& question, const uint8_t* jpeg, size_t size, std::string& answer) {
    if (url.empty() || token.empty()) {
        answer = "The bridge gave no vision endpoint.";
        return false;
    }
    if (url.rfind("https://", 0) != 0) {  // the token and the photo never go in clear
        answer = "The vision endpoint is not HTTPS.";
        return false;
    }
    std::string head = std::string("--") + kBoundary +
                       "\r\nContent-Disposition: form-data; name=\"question\"\r\n\r\n" + question + "\r\n--" + kBoundary +
                       "\r\nContent-Disposition: form-data; name=\"file\"; filename=\"camera.jpg\"\r\n"
                       "Content-Type: image/jpeg\r\n\r\n";
    std::string tail = std::string("\r\n--") + kBoundary + "--\r\n";

    esp_http_client_config_t config = {};
    config.url = url.c_str();
    config.method = HTTP_METHOD_POST;
    config.timeout_ms = 30000;
    config.crt_bundle_attach = esp_crt_bundle_attach;
    config.buffer_size = 2048;
    config.buffer_size_tx = 2048;
    esp_http_client_handle_t client = esp_http_client_init(&config);
    if (client == nullptr) {
        answer = "The robot could not start the upload.";
        return false;
    }
    std::string bearer = "Bearer " + token;
    std::string type = std::string("multipart/form-data; boundary=") + kBoundary;
    esp_http_client_set_header(client, "Authorization", bearer.c_str());
    esp_http_client_set_header(client, "Device-Id", device_id.c_str());
    esp_http_client_set_header(client, "Content-Type", type.c_str());

    bool ok = false;
    int total = (int)(head.size() + size + tail.size());
    if (esp_http_client_open(client, total) != ESP_OK) {
        answer = "The robot could not reach the vision endpoint.";
    } else {
        bool sent = esp_http_client_write(client, head.data(), (int)head.size()) == (int)head.size();
        // Write the photo in pieces; the TLS layer takes what it can.
        for (size_t off = 0; sent && off < size;) {
            int n = esp_http_client_write(client, reinterpret_cast<const char*>(jpeg) + off, (int)std::min<size_t>(size - off, 4096));
            if (n <= 0) {
                sent = false;
                break;
            }
            off += n;
        }
        sent = sent && esp_http_client_write(client, tail.data(), (int)tail.size()) == (int)tail.size();
        if (!sent) {
            answer = "The photo upload was cut.";
        } else {
            esp_http_client_fetch_headers(client);
            int status = esp_http_client_get_status_code(client);
            char buf[512];
            answer.clear();
            int n;
            while ((n = esp_http_client_read(client, buf, sizeof(buf))) > 0 && (int)answer.size() < kMaxAnswer) {
                answer.append(buf, n);
            }
            ok = status == 200 && !answer.empty();
            if (!ok) {
                ESP_LOGW(TAG, "vision HTTP %d", status);
                answer = "The vision endpoint answered HTTP " + std::to_string(status) + ".";
            }
        }
    }
    esp_http_client_close(client);
    esp_http_client_cleanup(client);
    return ok;
}

}  // namespace net
