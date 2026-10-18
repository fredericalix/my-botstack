#!/usr/bin/env bash
# Publish a firmware for an update over the air (docs/firmware.md#updates):
# build it, signed, and put it on the bridge's data volume of docker compose.
# The bridge offers it within 30 s; each device installs it between
# conversations, checks the signature first, and goes back to the version
# before if the new one does not reach the bridge.
#
#   scripts/publish-firmware.sh firmware      the StackChan (CoreS3)
#   scripts/publish-firmware.sh firmware-p4   the ESP32-P4 board
#
# The image goes to /data/firmware/<project>.bin: each device gets its own
# project's image. The bridge must be running (docker compose up -d).
# The first firmware that can update itself goes in by USB, which also
# creates the signing key: firmware/flash.sh botstack or firmware-p4/flash.sh.
# IDF_ACTIVATE overrides the ESP-IDF 5.5.4 activation script.
set -euo pipefail

root=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd)
case "${1:-}" in
  firmware | firmware-p4) firmware="$root/$1" ;;
  *) echo "usage: $0 firmware|firmware-p4" >&2; exit 2 ;;
esac
project=$(sed -n 's/^project(\([a-z0-9_-]*\)).*/\1/p' "$firmware/CMakeLists.txt" | head -n 1)
[[ -n "$project" ]] || { echo "no project() in $firmware/CMakeLists.txt" >&2; exit 1; }
image="$firmware/build/$project.bin"

if [[ ! -f "$root/firmware/signing-key.pem" ]]; then
  echo "no firmware/signing-key.pem: flash a device by USB first (firmware/flash.sh botstack or firmware-p4/flash.sh)" >&2
  exit 1
fi
activate="${IDF_ACTIVATE:-$HOME/.espressif/tools/activate_idf_v5.5.4.sh}"
[[ -f "$activate" ]] || { echo "no ESP-IDF activation script at $activate: install ESP-IDF 5.5.4 (QUICKSTART.md) or set IDF_ACTIVATE" >&2; exit 1; }

cd "$root"
if [[ -z "$(docker compose ps --status running -q bridge)" ]]; then
  echo "the bridge is not running: docker compose up -d" >&2
  exit 1
fi

# The activation script only accepts being sourced from a shell whose name it
# recognises, hence `bash -c`. The output is shown only when the build fails.
log=$(mktemp)
trap 'rm -f "$log"' EXIT
if ! (cd "$firmware" && IDF_ACTIVATE="$activate" bash -c 'source "$IDF_ACTIVATE" && idf.py build') >"$log" 2>&1; then
  cat "$log" >&2
  echo "the build of $1 failed (output above)" >&2
  exit 1
fi

# The build the robot compares with its own, and a check that the image is
# signed: an unsigned one would be downloaded and refused every time.
build=$(python3 - "$image" "$project" <<'EOF'
import re, sys
image = open(sys.argv[1], 'rb').read()
desc = image[32:208]
assert int.from_bytes(desc[0:4], 'little') == 0xABCD5432, 'not an ESP-IDF app image'
project = desc[48:80].split(b'\0')[0].decode()
assert re.fullmatch(r'[a-z0-9_-]{1,32}', project), 'not a botstack firmware project'
assert project == sys.argv[2], f'the image is {project}, not {sys.argv[2]}'
assert len(image) % 4096 == 0 and image[-4096] == 0xE7, 'the image is not signed'
print(desc[144:152].hex())
EOF
)
size=$(wc -c <"$image" | tr -d ' ')

# Written as the bridge's user, then renamed: the bridge never sees half an image.
docker compose exec -T bridge sh -c \
  'umask 022 && mkdir -p /data/firmware && cat >"/data/firmware/$1.bin.tmp" && mv "/data/firmware/$1.bin.tmp" "/data/firmware/$1.bin"' \
  sh "$project" <"$image"
stored=$(docker compose exec -T bridge sh -c 'wc -c <"/data/firmware/$1.bin"' sh "$project" | tr -d ' \r')
if [[ "$stored" != "$size" ]]; then
  echo "upload incomplete: $stored of $size bytes" >&2
  exit 1
fi
echo "published $project build $build ($size bytes); its devices install it at their next pause"
