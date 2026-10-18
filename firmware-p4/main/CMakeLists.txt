# The Wi-Fi networks are not in the image: flash.sh writes them from wifi.txt
# into the `wifi_nvs` partition.
idf_component_register(
    SRCS "main.cpp" "p4_codec.cpp"
    INCLUDE_DIRS "."
    REQUIRES botstack nvs_flash app_update esp_timer esp_driver_gpio
)
