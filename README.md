# botstack

botstack turns the M5Stack StackChan robot, and a round-screen ESP32-P4 board, into voice
assistants that talk with xAI's Grok realtime voice. It is a complete stack you host
yourself: an ESP-IDF firmware with an animated face (bloub), and a small Rust bridge that
runs on your server behind Caddy, keeps the devices' always-on link, shapes the voice for
their small speakers, runs their tools (face, head, camera), and lets an AI manage the
fleet over MCP. No cloud service is in the path but xAI's (and your own agent, if you
add one).

**Start here: [QUICKSTART.md](QUICKSTART.md)**, from an empty server to a talking robot.

![Who talks to whom](docs/img/architecture.svg)

## What works

- **Conversation in real time** with `grok-voice-3.0`: tap the screen (or touch the
  StackChan's head) and talk; tap or talk over it to interrupt (echo cancellation on the
  device, barge-in); say goodbye to end. English or French, on request or by default per
  device.
- **The bloub face**: 16 expressions, blinks and wander, listening / thinking / speaking /
  sleeping states and one-shot animations, chosen by Grok as it talks.
- **The StackChan's body**: head moves and gestures (nod, shake, look around) on its two
  servos, head touch, and the camera (“what do you see?”, answered by `grok-4.3` vision).
- **Two boards**: the StackChan (M5Stack CoreS3, ESP32-S3), dual-booting next to its stock
  firmware until the first update over the air replaces it, and the Waveshare ESP32-P4-WIFI6-Touch-LCD-4C (720×720 round screen).
- **A fleet**: each device introduces itself, waits for your approval, then gets its own
  key; per-device name, voice, language, notes and lock; a cap on simultaneous
  conversations; health reports.
- **Admin from an AI**: an MCP server (`/fleet/mcp`) to list, approve, rename, lock and
  reboot devices, rotate their keys and report their firmware, from Claude Code or any MCP
  client, or `scripts/fleet.sh` from a shell. Publishing a firmware is
  `scripts/publish-firmware.sh`.
- **Optional Grok Bot**: hand questions the voice cannot answer to an agent of yours
  (webhook out, MCP back); it can also make a device speak, take a photo, or lock it.
- **Signed updates over the air** with rollback, served by the bridge.
- **One-command server**: Docker Compose with Caddy (automatic HTTPS), a hardened bridge
  container, `scripts/setup.sh` for the configuration.

## Documentation

| Read | For |
| --- | --- |
| [QUICKSTART.md](QUICKSTART.md) | Install the server, flash a device, approve it, talk |
| [docs/](docs/README.md) | How it works, one page and one diagram per topic |
| [docs/protocol.md](docs/protocol.md) | botstack/v1, the link between a device and the bridge |
| [docs/deployment.md](docs/deployment.md) | Compose, Caddy, the data volume, every setting |
| [docs/security.md](docs/security.md) and [SECURITY.md](SECURITY.md) | Every door and limit; how to report a vulnerability |
| [AGENTS.md](AGENTS.md) | Build, test and the rules for contributors (human or AI) |

## Repository layout

| Path | What it is |
| --- | --- |
| `bridge/` | `botstack-bridge` (Rust): the device link, xAI realtime, the fleet and its admin MCP, Grok Bot MCP, vision; `examples/fake_robot.rs` tests it without hardware |
| `firmware/` | ESP-IDF 5.5.4 firmware for the StackChan's CoreS3: camera, head and head touch, codec, `flash.sh` (dual boot) |
| `firmware/components/botstack/` | The code both boards share: bloub face, audio (AFE, Opus), Wi-Fi, the botstack/v1 link, updates |
| `firmware-p4/` | ESP-IDF 5.5.4 firmware for the Waveshare ESP32-P4-WIFI6-Touch-LCD-4C (round screen, no head, no camera) |
| `scripts/` | `setup.sh`, `fleet.sh`, `publish-firmware.sh`, and the helpers the flash scripts use |
| `compose.yaml`, `Caddyfile`, `Dockerfile`, `.env.example` | The server: the bridge behind Caddy |
| `docs/` | The documentation and its diagrams (`docs/tools/` redraws them) |

## Status and limits

- **A lab project.** It is used every day with a few devices, but it is not a product: no multi-user accounts, one admin token, one bootstrap key in every image.
  Read [security](docs/security.md) before you expose it.
- **xAI costs money.** The realtime voice is billed per minute of conversation (about
  $0.08 per minute at the time of writing; check xAI's pricing), plus text to speech and
  vision. `MAX_CONVERSATIONS` caps simultaneous sessions; `/metrics` counts the seconds.
- **Espressif's esp-sr** (echo cancellation, voice detection) comes with its own licence,
  which restricts the use of the esp-sr library and its models to Espressif chips.
- The StackChan needs its stock firmware once, to join your Wi-Fi: botstack reads the
  networks it saved.

## Roadmap

- Tests of the link in CI (a `fake_robot` run against the bridge).
- Session resumption across short disconnects.
- Wi-Fi provisioning without the stock StackChan firmware.

## Credits

- **bloub** by Jérémy Perret, MIT: the face model, its poses, animation timings and
  wander/blink model (<https://github.com/jeremy-prt/bloub>, <https://bloub.vercel.app>;
  see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)).
- The **StackChan** project, the open-source robot this started from, and **M5Stack** for
  the hardware and the CoreS3 board support.
- **Waveshare** for the ESP32-P4 board support package, and **Espressif** for ESP-IDF,
  esp-sr and the components.
- **xAI** for the realtime voice, text to speech and vision models.

## Licence

MIT, see [LICENSE](LICENSE). The face derives from bloub (MIT, Jérémy Perret): its
notice is in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). Third-party components keep
their own licences (notably Espressif's esp-sr, above).
