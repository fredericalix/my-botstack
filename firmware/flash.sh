#!/bin/bash
# Dual boot on the StackChan: the StackChan firmware stays in ota_0, botstack
# goes to ota_1. Only the app partition and otadata are written; the
# bootloader, partition table, NVS (Wi-Fi, servo zeros) and assets are not.
# It first reads the robot's partition table and stops if it is not the
# StackChan firmware's (ota_1 at 0x510000).
#
#   ./flash.sh botstack    build (signed), write ota_1, boot it
#   ./flash.sh stackchan   boot ota_0 again: StackChan, until the first update
#                          over the air replaced it with botstack
#
# On the robot, a 3 s press on the screen also goes back to StackChan.
#
# The firmware is signed with signing-key.pem (gitignored, mode 600; created by
# scripts/signing-key.sh the first time). The robot installs only updates signed
# with the key of the firmware it runs (docs/firmware.md#updates).
#
# PORT=/dev/... picks the USB port; without it, the one USB serial device
# plugged in. IDF_ACTIVATE overrides the ESP-IDF 5.5.4 activation script.
set -eu

mode="${1:-}"
here=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
activate="${IDF_ACTIVATE:-$HOME/.espressif/tools/activate_idf_v5.5.4.sh}"

# The ESP-IDF 5.5.4 activation script only accepts being sourced from a shell
# whose name it recognises, so each command runs in its own `bash -c`.
idf() {
  IDF_ACTIVATE="$activate" bash -c 'source "$IDF_ACTIVATE" >/dev/null && "$@"' bash "$@"
}

case "$mode" in
  botstack|stackchan) ;;
  *)
    echo "usage: $0 botstack|stackchan" >&2
    exit 2
    ;;
esac

if [ ! -f "$activate" ]; then
  echo "no ESP-IDF activation script at $activate: install ESP-IDF 5.5.4 (QUICKSTART.md) or set IDF_ACTIVATE" >&2
  exit 1
fi

port=$("$here/../scripts/usb-port.sh")

# serial-log.py holds the port: flashing through it would corrupt the write.
logger_pid="$here/../out/serial/serial-log.pid"
if [ -f "$logger_pid" ] && kill -0 "$(cat "$logger_pid")" 2>/dev/null; then
  echo "the serial logger is running (pid $(cat "$logger_pid")): stop it first" >&2
  exit 1
fi

otatool="$(idf printenv IDF_PATH)/components/app_update/otatool.py"

# botstack expects the partition table of M5Stack's StackChan firmware (the
# same as partitions.csv). Read the robot's table (read-only) and write nothing
# if its app slots, otadata, NVS or assets are elsewhere.
table=$(mktemp)
trap 'rm -f "$table"' EXIT
idf python -m esptool --chip esp32s3 -p "$port" -b 460800 --before default_reset --after no_reset \
  read_flash 0x8000 0xc00 "$table" >/dev/null
python3 - "$table" <<'EOF'
import struct, sys
expected = {"nvs": (0x9000, 0x4000), "otadata": (0xd000, 0x2000), "ota_0": (0x20000, 0x4f0000),
            "ota_1": (0x510000, 0x4f0000), "assets": (0xa00000, 0x400000)}
data = open(sys.argv[1], "rb").read()
found = {}
for i in range(0, len(data) - 31, 32):
    entry = data[i:i + 32]
    if entry[:2] != b"\xaa\x50":
        break
    offset, size = struct.unpack_from("<II", entry, 4)
    found[entry[12:28].split(b"\0")[0].decode(errors="replace")] = (offset, size)
wrong = [name for name, place in expected.items() if found.get(name) != place]
if wrong:
    print("this robot's partition table is not the StackChan one botstack expects (%s differ); "
          "nothing was written (QUICKSTART.md, Flash a StackChan). Its table:" % ", ".join(wrong), file=sys.stderr)
    for name, (offset, size) in sorted(found.items(), key=lambda item: item[1]):
        print("  %-10s offset 0x%06x size 0x%06x" % (name, offset, size), file=sys.stderr)
    sys.exit(1)
EOF

case "$mode" in
  botstack)
    IDF_ACTIVATE="$activate" "$here/../scripts/signing-key.sh"
    cd "$here"
    idf idf.py build
    idf python -m esptool --chip esp32s3 -p "$port" -b 460800 --before default_reset --after no_reset \
      write_flash 0x510000 build/botstack-fw.bin
    idf python "$otatool" --port "$port" switch_ota_partition --slot 1
    ;;
  stackchan)
    idf python "$otatool" --port "$port" switch_ota_partition --slot 0
    ;;
esac
