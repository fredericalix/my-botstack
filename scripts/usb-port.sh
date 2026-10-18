#!/bin/bash
# Prints the board's USB serial port for the flash scripts: $PORT when set,
# else the one USB serial device plugged in. Zero or several: an error that
# asks for PORT=...
set -eu

if [ -n "${PORT:-}" ]; then
  if [ ! -e "$PORT" ]; then
    echo "nothing on $PORT: plug the USB cable" >&2
    exit 1
  fi
  echo "$PORT"
  exit 0
fi
# USB CDC (the CoreS3, the P4 board), then USB-serial bridges (CP210x, CH340, FTDI).
ports=$(ls -d /dev/cu.usbmodem* /dev/cu.usbserial* /dev/cu.wchusbserial* /dev/cu.SLAB_USBtoUART* \
  /dev/ttyACM* /dev/ttyUSB* 2>/dev/null || true)
count=$(printf '%s' "$ports" | grep -c . || true)
if [ "$count" -eq 1 ]; then
  echo "$ports"
  exit 0
fi
if [ "$count" -eq 0 ]; then
  echo "no USB serial device: plug the board's USB cable (a charge-only cable or some hubs show none), or set PORT=/dev/..." >&2
else
  echo "several USB serial devices; pick one with PORT=/dev/...:" >&2
  printf '%s\n' "$ports" | sed 's/^/  /' >&2
fi
exit 1
