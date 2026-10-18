# The StackChan board (CoreS3): codec, head and camera; the saved Wi-Fi
# networks and the rest are in the shared component (components/botstack).
idf_component_register(
    SRCS "main.cpp" "m5_codec.cpp"
         "camera/camera.cpp"
         "head/head.cpp" "head/touch.cpp"
    INCLUDE_DIRS "."
    REQUIRES botstack esp_driver_i2s esp_driver_gpio esp_driver_uart esp_driver_i2c nvs_flash app_update esp_timer
)
