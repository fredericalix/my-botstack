# Board-independent botstack firmware: audio (AFE, Opus), the bloub face, the
# botstack/v1 link, Wi-Fi and vision. Each project provides its board: the
# codec (audio::Codec), optional head and camera (botstack_board.h) and Wi-Fi networks.
set(token "${CMAKE_CURRENT_LIST_DIR}/../../robot-token")
if(NOT EXISTS "${token}")
    message(FATAL_ERROR "firmware/robot-token is missing: run scripts/setup.sh")
endif()
# The bridge's WebSocket URL, one line: wss://<your domain>/botstack/v1.
set(bridge_url "${CMAKE_CURRENT_LIST_DIR}/../../bridge-url")
if(NOT EXISTS "${bridge_url}")
    message(FATAL_ERROR "firmware/bridge-url is missing: run scripts/setup.sh or write wss://<your domain>/botstack/v1 into it")
endif()

idf_component_register(
    SRCS "audio/audio.cpp" "audio/opus.cpp" "audio/resampler.cpp"
         "face/bloub.cpp"
         "net/wifi.cpp" "net/link.cpp" "net/vision.cpp"
    INCLUDE_DIRS "."
    EMBED_TXTFILES "${token}" "${bridge_url}"
    REQUIRES app_update esp_timer nvs_flash esp_wifi esp_netif json mbedtls esp_http_client esp_https_ota esp_app_format
)
