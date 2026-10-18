# Agents

Instructions for anyone, human or AI, who changes this repository.

botstack runs voice devices (the M5Stack StackChan robot, and the Waveshare
ESP32-P4-WIFI6-Touch-LCD-4C board) on its own stack: a Rust bridge (`bridge/`) between the
devices and xAI's realtime voice, and ESP-IDF firmwares (`firmware/`, `firmware-p4/`) with
the bloub face. Read [docs/](docs/README.md) for how it works and [QUICKSTART.md](QUICKSTART.md)
for how it is installed.

Code, comments and docs are in English. Change only what the task asks for; keep `docs/`
in step with behaviour changes.

## Layout

| Path | What it is |
| --- | --- |
| `bridge/` | `botstack-bridge` (Rust): device link (botstack/v1, `link.rs`), xAI realtime (`realtime.rs`), fleet (`fleet.rs`, `admin.rs`), Grok Bot MCP (`grokbot.rs`), vision, configuration (`main.rs`) |
| `bridge/examples/fake_robot.rs` | A fake device: the firmware side of botstack/v1, tools, camera upload. Test the bridge with it before touching hardware. |
| `bridge/examples/shape.rs` | Runs the downlink voice shaping on a WAV and prints levels |
| `firmware/` | ESP-IDF 5.5.4 app for the StackChan's CoreS3 (`botstack-fw`): `main/camera`, `main/head`, `main/m5_codec.*`, `main/main.cpp`, `flash.sh`, `serial-log.py` |
| `firmware/components/botstack/` | The code both boards share: `face`, `audio`, `net` (Wi-Fi, link, updates), `botstack_board.h`. A change here rebuilds both firmwares: build both, and the StackChan must not change by accident. |
| `firmware-p4/` | ESP-IDF 5.5.4 app for the P4 board (`botstack-p4`): `main/`, `partitions.csv`, `flash.sh`, `wifi_nvs.py` |
| `scripts/` | `setup.sh` (`.env` and the firmware's two files), `fleet.sh` (admin MCP), `publish-firmware.sh` (updates over the air), `signing-key.sh`, `usb-port.sh`, `dotenv.sh` |
| `compose.yaml`, `Caddyfile`, `Dockerfile`, `.env.example` | The server: the bridge behind Caddy |
| `docs/` | Illustrated docs; `docs/tools/` regenerates the diagrams |

## Build and test

Bridge:

```sh
cargo test -p botstack-bridge
cargo clippy -p botstack-bridge --all-targets -- -D warnings   # zero warnings
```

The bridge links libopus (`libopus-dev pkg-config cmake` on Debian/Ubuntu,
`brew install opus pkg-config cmake` on macOS). The code is not `cargo fmt`-formatted; do
not reformat whole files.

Run it locally with a fake device (no hardware; `out/` is gitignored):

```sh
export ROBOT_TOKEN="$(openssl rand -hex 32)"
BIND=127.0.0.1:8080 DATA_DIR=out/data FLEET_ADMIN_TOKEN=local GROK_BRIDGE_TOKEN=local \
  IDLE_CLOSE_SECONDS=8 cargo run -p botstack-bridge
cargo run -p botstack-bridge --example fake_robot -- --stay 5           # becomes pending
cargo run -p botstack-bridge --example fake_robot -- --talk --say "What time is it?"   # needs XAI_API_KEY
```

A fake device that should hold its own key needs `--enroll --device <another MAC> --out
<its own directory>`. Approve it through `/fleet/mcp` (`scripts/fleet.sh` with
`BOTSTACK_URL=http://127.0.0.1:8080` and a `.env` that holds the same `FLEET_ADMIN_TOKEN`).

Firmware, with ESP-IDF 5.5.4 exactly (the activation script path can be overridden with
`IDF_ACTIVATE`):

```sh
cd firmware        # or firmware-p4
bash -c 'source ~/.espressif/tools/activate_idf_v5.5.4.sh >/dev/null && idf.py build'
```

The build needs `firmware/robot-token` and `firmware/bridge-url` (`scripts/setup.sh`, or
throwaway values as in `.github/workflows/ci.yml`) and signs with
`firmware/signing-key.pem` (`scripts/signing-key.sh` creates it). The activation script
refuses to be sourced from a script whose name it does not know: wrap each command in
`bash -c`, as the scripts do.

Diagrams: SVG with a white background. From `docs/`, `python3 tools/diagrams.py img`
regenerates most of them; `architecture.svg` and `audio-path.svg` are edited by hand;
`tools/expressions.py` draws the expression sheet. Render a changed one with
`rsvg-convert` and look at it before committing.

## Rules

Secrets:

- Never print, log or commit `.env`, `firmware/robot-token`, `firmware/signing-key.pem`,
  `firmware-p4/wifi.txt`, a token, a key or a webhook URL. Report only whether a value is
  set, its length, and whether a URL is HTTPS.
- Never replace `firmware/signing-key.pem`: devices install only updates signed with the
  key of the firmware they run, so a new key means the next firmware goes in by USB.
- The bridge logs only whether each secret is set; a device key never appears in a log
  line or an MCP answer. The firmware never logs its headers (it silences ESP-IDF's
  `transport_ws` logger, whose error path prints the upgrade request).
- Do not keep recordings of anyone's voice. A recording made for a diagnosis is deleted
  afterwards.

Protocol: botstack/v1 changes go together in `docs/protocol.md`, `bridge/src/link.rs`
and `firmware/components/botstack/net/link.cpp` (and `fake_robot.rs`).

Firmware:

- A task whose stack is in PSRAM never writes the flash (NVS, otadata) and never
  `vTaskDelete`s itself; such writes run in a short task with an internal-RAM stack.
- The WebSocket event handler never sends and never takes `client_lock_`; it hands the
  work to the control task.
- StackChan dual boot: never erase NVS, never full-chip erase, never write the bootloader,
  the partition table or `assets`. NVS holds the Wi-Fi networks and the servo zeros, and
  M5Burner must still restore the factory image. `firmware/flash.sh` writes only `ota_1`
  and `otadata`.
- P4: `firmware-p4/flash.sh` never erases the flash and never writes `nvs` (the board's
  own key).
- Flash only with the board's USB cable plugged in; `scripts/usb-port.sh` finds the port
  (or `PORT=/dev/...`). Opening the serial port can reset the board: bounded captures,
  one at a time, and none while `firmware/serial-log.py` runs.
- The hard-won facts in [docs/firmware.md](docs/firmware.md#hard-won-facts) (internal RAM,
  echo cancellation, camera, head, P4 quirks) are measured: keep them.

Bridge facts to keep:

- The bridge owns the device's state; the device streams the microphone while `state`
  says `mic: true`, and plays voice only between `speech start` and `speech end` or
  `cancel`. One `played` per speech; a new `speech start` before it replaces the old one.
- Voice: one 60 ms packet per 60 ms after a 60 ms gap, at most 4 ahead (the device's
  queue holds 48).
- xAI Opus output cannot be regrouped losslessly: ask xAI for PCM 24 kHz and encode our
  own 60 ms packets (64 kbit/s CBR). Voice shaping: high-pass 300 Hz, +10 dB at 3 kHz,
  −6 dB, limiter −10 dBFS; higher peaks made the board stutter.
- Verbatim speech (Grok Bot replies, pushes, apologies) goes through xAI `/v1/tts`, not
  realtime `force_message`.
- xAI stalls: retry the realtime connect once (8 s), apologise after 25 s without audio.
- The WebSocket stays open between conversations; the bridge drops it after 75 s without
  a frame (the device pings every 5 s and restarts after 20 s without a frame from the
  bridge). A newer connection from the same device ends the older one, so a rebooted
  device does not leave an xAI session billing on a dead socket. Pushes not yet spoken
  when it drops go back to `grok_pending.json` (Grok Bot's words only, never the bridge's
  apology).
- Only approved devices may open a conversation or ask Grok Bot; replies and pushes for
  another `device_id` are refused. `MAX_CONVERSATIONS` caps simultaneous xAI sessions.
- Head moves: the bridge waits for the device's `tool_result` (3 s) before it answers
  Grok, for `move_head` and the first step of a `gesture`; a blind “Done.” once hid a dead
  head for a whole session.
