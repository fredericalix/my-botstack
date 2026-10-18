#!/bin/bash
# The P4 board by USB: bootloader, partition table, app, and the Wi-Fi networks
# of wifi.txt in the `wifi_nvs` partition. It never erases the flash and never
# writes `nvs`, which holds the board's own key. The app image holds no Wi-Fi
# password; later versions go over the air:
# scripts/publish-firmware.sh firmware-p4
#
# PORT=/dev/... picks the USB port; without it, the one USB serial device
# plugged in. IDF_ACTIVATE overrides the ESP-IDF 5.5.4 activation script. The
# image is signed with firmware/signing-key.pem, shared with the StackChan
# firmware and created by scripts/signing-key.sh the first time.
set -eu
here=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
activate="${IDF_ACTIVATE:-$HOME/.espressif/tools/activate_idf_v5.5.4.sh}"
# The ESP-IDF 5.5.4 activation script only accepts being sourced from a shell
# whose name it recognises, so each command runs in its own `bash -c`.
idf() { IDF_ACTIVATE="$activate" bash -c 'source "$IDF_ACTIVATE" >/dev/null && "$@"' bash "$@"; }
[ -f "$activate" ] || { echo "no ESP-IDF activation script at $activate: install ESP-IDF 5.5.4 (QUICKSTART.md) or set IDF_ACTIVATE" >&2; exit 1; }
port=$("$here/../scripts/usb-port.sh")
# serial-log.py holds the port: flashing through it would corrupt the write.
logger_pid="$here/../out/serial/serial-log.pid"
if [ -f "$logger_pid" ] && kill -0 "$(cat "$logger_pid")" 2>/dev/null; then
  echo "the serial logger is running (pid $(cat "$logger_pid")): stop it first" >&2
  exit 1
fi
[ -f "$here/wifi.txt" ] || { echo "no firmware-p4/wifi.txt: one line per network, ssid<TAB>password" >&2; exit 1; }
IDF_ACTIVATE="$activate" "$here/../scripts/signing-key.sh"
cd "$here"
idf idf.py build
# The networks image (passwords inside: mode 600, in the gitignored build/).
# The script prints the partition's offset and only counts and line numbers.
offset=$(idf python wifi_nvs.py wifi.txt partitions.csv build/wifi_nvs.bin)
# One esptool run: what `idf.py flash` writes (flash_args) plus the networks.
# --chip refuses a board that is not an ESP32-P4 (the StackChan is an S3).
cd build
idf python -m esptool --chip esp32p4 -p "$port" -b 460800 --before default_reset --after hard_reset \
    write_flash @flash_args "$offset" wifi_nvs.bin
