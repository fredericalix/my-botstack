#!/bin/bash
# Creates firmware/signing-key.pem, the key that signs every firmware (StackChan
# and P4), when it is missing. It never replaces an existing key: a robot
# installs only updates signed with the key of the firmware it runs, so a new
# key means the next firmware goes in by USB (docs/firmware.md#updates).
#
# The flash scripts call it; IDF_ACTIVATE overrides the ESP-IDF 5.5.4
# activation script, like theirs.
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
key="$root/firmware/signing-key.pem"
[ -f "$key" ] && exit 0

activate="${IDF_ACTIVATE:-$HOME/.espressif/tools/activate_idf_v5.5.4.sh}"
if [ ! -f "$activate" ]; then
  echo "no ESP-IDF activation script at $activate: install ESP-IDF 5.5.4 (QUICKSTART.md) or set IDF_ACTIVATE" >&2
  exit 1
fi
# The activation script only accepts being sourced from a shell whose name it
# recognises, hence `bash -c`.
(umask 077 && IDF_ACTIVATE="$activate" bash -c \
  'source "$IDF_ACTIVATE" >/dev/null && python -m espsecure generate_signing_key --version 2 --scheme rsa3072 "$1" >/dev/null' \
  bash "$key")
echo "new firmware/signing-key.pem: keep a copy somewhere safe; firmware signed with another key needs the USB cable" >&2
