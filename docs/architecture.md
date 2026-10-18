# Architecture

![Who talks to whom](img/architecture.svg)

Four parts; botstack is the first two:

| Part | Where | What it does |
| --- | --- | --- |
| **Devices** | on your desk: the StackChan robot (M5Stack CoreS3, ESP32-S3) or the Waveshare ESP32-P4 board | Shows the bloub face, listens, plays Grok's voice; the StackChan also moves its head and takes photos. Runs the botstack firmware. |
| **botstack-bridge** | one container on your server, behind Caddy (Docker Compose) | Speaks the devices' protocol on one side and xAI's realtime API on the other. Shapes the voice for the small speaker, runs the robot's tools, keeps the fleet of devices, receives Grok Bot's answers. |
| **xAI** | cloud | Realtime voice model, text to speech, and a vision model. |
| **Grok Bot** (recommended) | in the cloud | The usual peer: a routine receives the webhook and calls MCP. Any other agent that can do those two things can take its place. It can read, for example, your mail and calendar. |

## Which model answers you?

Three xAI models, each with one job:

1. **grok-voice-3.0** answers every conversation. It hears your audio and
   answers in audio directly (voice “eve”): no separate transcription or synthesis step.
   It also decides the face expressions and head moves.
2. **Grok Bot** is only asked when you need something it can see (`GROK_BOT_SCOPE`: your
   accounts, mail, calendar, files…) or when you name it. Its answer comes
   back later and is read out word for word by **xAI TTS** in the same voice.
3. **grok-4.3** describes the camera photo when you ask “what do you see?”.

No model from another vendor is involved, and nothing goes through M5Stack's or xiaozhi's
servers or protocols.

## Why a bridge at all?

- The robot keeps one simple, always-on protocol ([botstack/v1](protocol.md): JSON and
  Opus over WebSocket), and the bridge does the rest. It can make the robot speak at any
  time.
- The xAI key stays on the server.
- xAI's own Opus packets (10 ms, mixed modes) cannot be regrouped into the 60 ms packets
  the robot plays, so the bridge decodes, shapes and re-encodes the voice.
- Grok Bot needs a stable public endpoint to answer to.
- Several devices share one server, one xAI key and one bill, under a cap
  (`MAX_CONVERSATIONS`).

## Protocols

| Link | Transport | Format |
| --- | --- | --- |
| robot ⇄ bridge | `wss://bot.example.com/botstack/v1` | [botstack/v1](protocol.md): JSON messages + raw Opus frames, always on; `Device-Id` + Bearer device key |
| bridge ⇄ xAI | `wss://api.x.ai/v1/realtime` | xAI realtime events, PCM 16 kHz up / 24 kHz down |
| bridge → Grok Bot | `GROK_BOT_WEBHOOK_URL` | JSON webhook with the question ([Grok Bot](grok-bot.md)) |
| Grok Bot → bridge | `https://bot.example.com/grok/mcp` | MCP over HTTP, Bearer `GROK_BRIDGE_TOKEN` |
| admin AI → bridge | `https://bot.example.com/fleet/mcp` | MCP over HTTP, Bearer admin token ([fleet](fleet.md)) |
| robot → bridge | `https://bot.example.com/mcp/vision/explain` | multipart photo upload, Bearer vision token |

A device proves itself with its MAC and its own key, or the bootstrap key (`ROBOT_TOKEN`)
until the owner approves it; the fleet of devices, their keys and the admin MCP
(`/fleet/mcp`, `FLEET_ADMIN_TOKEN`) are in [Fleet](fleet.md). Every door and limit is on the
[security](security.md) page.
