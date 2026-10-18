// SPDX-License-Identifier: MIT
//
// The camera tool's upload: one photo and the question, posted as multipart
// to the bridge's vision endpoint (URL and token from the bridge's hello).
// The response body is the answer the voice model reads.
#pragma once

#include <cstddef>
#include <cstdint>
#include <string>

namespace net {

/// Blocks up to ~30 s. On failure `answer` says what went wrong.
bool ask_vision(const std::string& url, const std::string& token, const std::string& device_id,
                const std::string& question, const uint8_t* jpeg, size_t size, std::string& answer);

}  // namespace net
